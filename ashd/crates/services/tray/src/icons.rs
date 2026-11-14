//! Icons: names → files (the item's `IconThemePath`, then the session's icon theme) and
//! pixmaps → PNG files under the ashd runtime directory. Everything here blocks on the
//! file system: the service calls it through `smol::unblock`.

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash as _, Hasher as _},
    path::{Path, PathBuf},
};

use walkdir::WalkDir;

use crate::model::Pixmap;

/// Formats GPUI can draw.
const EXTS: [&str; 5] = ["svg", "png", "webp", "jpg", "jpeg"];
/// Edge the theme lookup asks for (see [`crate::PIXMAP_TARGET`]).
const LOOKUP_SIZE: u16 = 32;
/// A (hostile or huge) `IconThemePath` is searched at most this deep / this long.
const SEARCH_DEPTH: usize = 5;
const SEARCH_LIMIT: usize = 20_000;

fn supported(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// `$XDG_RUNTIME_DIR/ashd/tray` (the runtime dir is private to the user, 0700), else a
/// per-user directory under the system temp dir.
pub fn cache_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        Some(run) => PathBuf::from(run).join("ashd").join("tray"),
        None => std::env::temp_dir().join(format!("ashd-tray-{}", rustix::process::getuid().as_raw())),
    }
}

/// Resolves icon names for one service session.
#[derive(Clone)]
pub(crate) struct Resolver {
    theme: String,
    cache: PathBuf,
}

impl Resolver {
    pub fn new(cache: PathBuf) -> Self {
        Self { theme: ashd_svc_apps::current_icon_theme(), cache }
    }

    /// `name` may be an absolute path, a theme name, or a name inside `theme_path`.
    pub fn resolve(&self, name: &str, theme_path: &str) -> Option<PathBuf> {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        let as_path = Path::new(name);
        if as_path.is_absolute() {
            return (as_path.is_file() && supported(as_path)).then(|| as_path.to_path_buf());
        }
        let stem = EXTS.iter().find_map(|e| name.strip_suffix(&format!(".{e}"))).unwrap_or(name);
        if !theme_path.is_empty()
            && let Some(p) = find_in_dir(Path::new(theme_path), stem)
        {
            return Some(p);
        }
        let themed = |theme: &str| {
            freedesktop_icons::lookup(stem)
                .with_size(LOOKUP_SIZE)
                .with_theme(theme)
                .with_cache()
                .find()
                .filter(|p| supported(p))
        };
        themed(&self.theme)
            .or_else(|| if self.theme != "hicolor" { themed("hicolor") } else { None })
            .or_else(|| pixmaps(stem))
    }

    /// Write `pixmap` as a PNG named after its content hash (`None` if the file system
    /// refuses).
    pub fn store_pixmap(&self, pixmap: &Pixmap) -> Option<PathBuf> {
        let mut h = DefaultHasher::new();
        (pixmap.width, pixmap.height).hash(&mut h);
        pixmap.rgba.hash(&mut h);
        self.store_png(h.finish(), pixmap)
    }

    fn store_png(&self, hash: u64, pixmap: &Pixmap) -> Option<PathBuf> {
        let path = self.cache.join(format!("{hash:016x}.png"));
        if path.is_file() {
            return Some(path);
        }
        std::fs::create_dir_all(&self.cache).ok()?;
        let img = image::RgbaImage::from_raw(pixmap.width, pixmap.height, pixmap.rgba.clone())?;
        // Write beside, then rename: a UI never sees half a PNG.
        let tmp = self.cache.join(format!("{hash:016x}.tmp"));
        img.save_with_format(&tmp, image::ImageFormat::Png).ok()?;
        std::fs::rename(&tmp, &path).ok()?;
        Some(path)
    }

    /// PNG bytes (dbusmenu `icon-data`) → a file in the cache, content-addressed too.
    pub fn store_png_bytes(&self, bytes: &[u8]) -> Option<PathBuf> {
        // Through the decoder: only real images reach the UI.
        let img = image::load_from_memory(bytes).ok()?.to_rgba8();
        let (width, height) = img.dimensions();
        self.store_pixmap(&Pixmap { width, height, rgba: img.into_raw() })
    }
}

/// Files in the first matching `pixmaps` directory of the XDG data dirs.
fn pixmaps(name: &str) -> Option<PathBuf> {
    let dirs = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    for dir in dirs.split(':').filter(|d| !d.is_empty()) {
        for ext in EXTS {
            let p = Path::new(dir).join("pixmaps").join(format!("{name}.{ext}"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// The best file called `<name>.<ext>` below `root`: `root/<name>.x` itself, else a
/// theme-shaped tree (`hicolor/48x48/apps/<name>.png`, `scalable/…svg`), scalable first,
/// then the size nearest to [`LOOKUP_SIZE`].
pub(crate) fn find_in_dir(root: &Path, name: &str) -> Option<PathBuf> {
    for ext in EXTS {
        let p = root.join(format!("{name}.{ext}"));
        if p.is_file() {
            return Some(p);
        }
    }
    let mut best: Option<(i64, PathBuf)> = None;
    for entry in WalkDir::new(root).max_depth(SEARCH_DEPTH).follow_links(true).into_iter().filter_map(Result::ok).take(SEARCH_LIMIT) {
        let p = entry.path();
        if !entry.file_type().is_file() || !supported(p) || p.file_stem().and_then(|s| s.to_str()) != Some(name) {
            continue;
        }
        let score = score(p);
        if best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((score, p.to_path_buf()));
        }
    }
    best.map(|(_, p)| p)
}

/// Higher is better: svg, then closeness of a `NxN` / `N` directory to the wanted size.
fn score(p: &Path) -> i64 {
    if p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("svg")) {
        return 10_000;
    }
    let size = p
        .ancestors()
        .skip(1)
        .filter_map(|a| a.file_name()?.to_str())
        .find_map(|n| n.split('x').next()?.parse::<i64>().ok())
        .unwrap_or(0);
    // Bigger than wanted is fine (downscaled); smaller is blurry.
    let want = i64::from(LOOKUP_SIZE);
    if size >= want { 5_000 - (size - want) } else { size }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(root: &Path, rel: &str) -> PathBuf {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"x").unwrap();
        p
    }

    #[test]
    fn theme_path_flat_and_nested() {
        let d = tempfile::tempdir().unwrap();
        let flat = touch(d.path(), "flat.png");
        assert_eq!(find_in_dir(d.path(), "flat"), Some(flat));
        let small = touch(d.path(), "hicolor/16x16/status/app.png");
        let big = touch(d.path(), "hicolor/48x48/status/app.png");
        let _huge = touch(d.path(), "hicolor/512x512/status/app.png");
        assert_eq!(find_in_dir(d.path(), "app"), Some(big.clone()), "nearest size at least 32 wins");
        std::fs::remove_file(&big).unwrap();
        std::fs::remove_file(d.path().join("hicolor/512x512/status/app.png")).unwrap();
        assert_eq!(find_in_dir(d.path(), "app"), Some(small));
        let svg = touch(d.path(), "hicolor/scalable/status/app.svg");
        assert_eq!(find_in_dir(d.path(), "app"), Some(svg), "svg beats raster");
        assert_eq!(find_in_dir(d.path(), "missing"), None);
    }

    #[test]
    fn absolute_names_and_theme_path() {
        let d = tempfile::tempdir().unwrap();
        let png = touch(d.path(), "a.png");
        let r = Resolver { theme: "hicolor".into(), cache: d.path().join("cache") };
        assert_eq!(r.resolve(png.to_str().unwrap(), ""), Some(png.clone()));
        assert_eq!(r.resolve("a", d.path().to_str().unwrap()), Some(png));
        assert_eq!(r.resolve("", ""), None);
        assert_eq!(r.resolve("/nonexistent/x.png", ""), None);
    }

    #[test]
    fn pixmaps_are_content_addressed() {
        let d = tempfile::tempdir().unwrap();
        let r = Resolver { theme: "hicolor".into(), cache: d.path().join("c") };
        let px = Pixmap { width: 2, height: 2, rgba: vec![255; 16] };
        let a = r.store_pixmap(&px).unwrap();
        assert_eq!(r.store_pixmap(&px), Some(a.clone()));
        let other = Pixmap { width: 2, height: 2, rgba: vec![0; 16] };
        assert_ne!(r.store_pixmap(&other), Some(a.clone()));
        let img = image::open(&a).unwrap().to_rgba8();
        assert_eq!(img.dimensions(), (2, 2));
        // not a PNG: refused
        assert_eq!(r.store_png_bytes(b"nope"), None);
    }
}
