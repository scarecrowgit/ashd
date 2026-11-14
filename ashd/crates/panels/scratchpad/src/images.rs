//! Screenshots on disk: find the newest images of `~/Pictures/Screenshots` and keep small
//! thumbnails of them in a cache under the ashd runtime directory. Blocking (disk, image
//! decoding): the panel runs [`scan`] on a background task, on every opening.

use std::{
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

/// The thumbnail box: twice the 96×60 grid cell.
pub const THUMB_BOX: (u32, u32) = (192, 120);
/// Files above this are listed but not decoded (a thumbnail would take too long).
const MAX_DECODE_BYTES: u64 = 64 * 1024 * 1024;
const EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "webp", "gif"];

/// A screenshot (or any image) in the watched folder.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageFile {
    pub path: PathBuf,
    pub name: String,
    pub modified_ms: u64,
    pub bytes: u64,
    /// The cached thumbnail, when it could be made.
    pub thumb: Option<PathBuf>,
    pub dims: Option<(u32, u32)>,
}

/// `~/Pictures/Screenshots`.
pub fn screenshots_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")).join("Pictures").join("Screenshots")
}

/// `<runtime dir>/ashd/scratch-thumbs` (next to the shell socket; tmpfs, gone at logout).
pub fn cache_dir() -> PathBuf {
    ashd_ipc::socket_path().parent().map(Path::to_path_buf).unwrap_or_else(std::env::temp_dir).join("scratch-thumbs")
}

fn is_image(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// The `cap` newest image files of `dir` as `(path, mtime ms, bytes)`, newest first.
pub fn list_newest(dir: &Path, cap: usize) -> Vec<(PathBuf, u64, u64)> {
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<(PathBuf, u64, u64)> = rd
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let meta = e.metadata().ok().filter(|m| m.is_file())?;
            is_image(&path).then(|| {
                let ms = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                (path, ms, meta.len())
            })
        })
        .collect();
    files.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    files.truncate(cap);
    files
}

/// FNV-1a: a stable, dependency-free cache key.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3))
}

/// Cache file for a source: changes when the file's path, mtime or size change.
pub fn thumb_path(cache: &Path, src: &Path, modified_ms: u64, bytes: u64) -> PathBuf {
    let key = format!("{}\0{modified_ms}\0{bytes}", src.display());
    cache.join(format!("{:016x}.png", fnv(key.as_bytes())))
}

/// Decode `src`, write a [`THUMB_BOX`] PNG to `dst`; the original's size. `None` when the
/// file cannot be decoded.
pub fn make_thumb(src: &Path, dst: &Path) -> Option<(u32, u32)> {
    let img = image::ImageReader::open(src).ok()?.with_guessed_format().ok()?.decode().ok()?;
    let dims = (img.width(), img.height());
    let thumb = img.resize(THUMB_BOX.0, THUMB_BOX.1, image::imageops::FilterType::Lanczos3);
    let tmp = dst.with_extension("tmp");
    thumb.save_with_format(&tmp, image::ImageFormat::Png).ok()?;
    fs::rename(&tmp, dst).ok()?;
    Some(dims)
}

/// The newest `cap` images of `dir` with thumbnails (made on demand, reused when present).
pub fn scan(dir: &Path, cap: usize, cache: &Path) -> Vec<ImageFile> {
    let _ = fs::create_dir_all(cache);
    list_newest(dir, cap)
        .into_iter()
        .map(|(path, modified_ms, bytes)| {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let dst = thumb_path(cache, &path, modified_ms, bytes);
            let (thumb, dims) = if bytes > MAX_DECODE_BYTES {
                (None, None)
            } else if dst.is_file() {
                (Some(dst), image::image_dimensions(&path).ok())
            } else {
                match make_thumb(&path, &dst) {
                    Some(dims) => (Some(dst), Some(dims)),
                    None => (None, None),
                }
            };
            ImageFile { path, name, modified_ms, bytes, thumb, dims }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_png(path: &Path, w: u32, h: u32) {
        image::RgbaImage::from_pixel(w, h, image::Rgba([200, 120, 40, 255])).save(path).unwrap();
    }

    #[test]
    fn lists_only_images_newest_first_and_capped() {
        let dir = tempfile::tempdir().unwrap();
        for (name, secs) in [("a.png", 100), ("b.jpg", 300), ("c.txt", 400), ("d.PNG", 200), ("e.webp", 50)] {
            let p = dir.path().join(name);
            fs::write(&p, b"x").unwrap();
            let t = UNIX_EPOCH + std::time::Duration::from_secs(secs);
            fs::File::options().write(true).open(&p).unwrap().set_modified(t).unwrap();
        }
        fs::create_dir(dir.path().join("sub.png")).unwrap();
        let names = |cap| {
            list_newest(dir.path(), cap)
                .into_iter()
                .map(|(p, _, _)| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(10), ["b.jpg", "d.PNG", "a.png", "e.webp"]);
        assert_eq!(names(2), ["b.jpg", "d.PNG"]);
        assert!(list_newest(&dir.path().join("missing"), 5).is_empty());
    }

    #[test]
    fn thumbnails_are_made_cached_and_keyed_by_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache");
        let shots = dir.path().join("shots");
        fs::create_dir(&shots).unwrap();
        write_png(&shots.join("big.png"), 1920, 1080);
        fs::write(shots.join("broken.png"), b"not a png").unwrap();
        let files = scan(&shots, 12, &cache);
        assert_eq!(files.len(), 2);
        let big = files.iter().find(|f| f.name == "big.png").unwrap();
        assert_eq!(big.dims, Some((1920, 1080)));
        let thumb = big.thumb.clone().unwrap();
        assert!(thumb.starts_with(&cache));
        assert_eq!(image::image_dimensions(&thumb).unwrap(), (192, 108), "fitted into 192×120, aspect kept");
        let broken = files.iter().find(|f| f.name == "broken.png").unwrap();
        assert_eq!((broken.thumb.clone(), broken.dims), (None, None));
        // Second scan reuses the cached thumbnail.
        let again = scan(&shots, 12, &cache);
        assert_eq!(again.iter().find(|f| f.name == "big.png").unwrap().thumb, Some(thumb));
        assert_ne!(thumb_path(&cache, Path::new("/a.png"), 1, 2), thumb_path(&cache, Path::new("/a.png"), 2, 2));
        assert_ne!(thumb_path(&cache, Path::new("/a.png"), 1, 2), thumb_path(&cache, Path::new("/b.png"), 1, 2));
    }
}
