//! Everything that touches the disk or the clipboard (no GPUI types except the decoded
//! image GPUI draws): where files go (`shell.json` can override), the temporary still niri
//! renders, cropping and saving the PNG, the thumbnail, the clipboard command. All of it
//! blocks, so callers run it off the main thread.

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime},
};

use chrono::NaiveDateTime;
use gpui::RenderImage;
use serde::Deserialize;

use crate::capture::{PixelRect, ScaleMap, Rect, screenshot_file_name, unique_path};

// ── destinations ──────────────────────────────────────────────────────────────

/// Where results go. Defaults: `~/Pictures/Screenshots` and `~/Videos/Recordings` (the XDG
/// pictures / videos directories when the user relocated them). `shell.json`
/// (`/etc/ashd/shell.json`, then `$XDG_CONFIG_HOME/ashd/shell.json`, the user file wins per
/// key) overrides them:
///
/// ```json
/// { "screenshot": { "dir": "~/Pictures/Shots", "recordingsDir": "/data/rec" } }
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dirs {
    pub screenshots: PathBuf,
    pub recordings: PathBuf,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawScreenshot {
    dir: Option<String>,
    recordings_dir: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawConfig {
    screenshot: Option<RawScreenshot>,
}

/// Resolve the destinations from the contents of the config files (later wins), `home`,
/// and the XDG pictures / videos directories.
pub fn resolve_dirs(configs: &[String], home: &Path, pictures: Option<PathBuf>, videos: Option<PathBuf>) -> Dirs {
    let mut dir = None;
    let mut rec = None;
    for text in configs {
        match serde_json::from_str::<RawConfig>(text) {
            Ok(RawConfig { screenshot: Some(s) }) => {
                dir = s.dir.filter(|d| !d.trim().is_empty()).or(dir);
                rec = s.recordings_dir.filter(|d| !d.trim().is_empty()).or(rec);
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("shell.json (screenshot): {e}"),
        }
    }
    let expand = |s: &str| match s.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if s == "~" => home.to_path_buf(),
        None => PathBuf::from(s),
    };
    Dirs {
        screenshots: dir.map_or_else(|| pictures.unwrap_or_else(|| home.join("Pictures")).join("Screenshots"), |d| expand(&d)),
        recordings: rec.map_or_else(|| videos.unwrap_or_else(|| home.join("Videos")).join("Recordings"), |d| expand(&d)),
    }
}

/// [`resolve_dirs`] for this user, reading the config files.
pub fn dirs() -> Dirs {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    let user = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".config"))
        .join("ashd/shell.json");
    let configs: Vec<String> =
        [PathBuf::from("/etc/ashd/shell.json"), user].iter().filter_map(|p| std::fs::read_to_string(p).ok()).collect();
    resolve_dirs(&configs, &home, dirs::picture_dir(), dirs::video_dir())
}

// ── the still ─────────────────────────────────────────────────────────────────

/// `$XDG_RUNTIME_DIR/ashd` (the shell's runtime dir), else `/tmp/ashd-$UID`.
pub fn runtime_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d).join("ashd"),
        None => PathBuf::from(format!("/tmp/ashd-{}", rustix::process::getuid().as_raw())),
    }
}

/// A fresh path for niri to write a still to (`<runtime>/ashd/screenshot-<pid>-<n>.png`).
pub fn still_path() -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    runtime_dir().join(format!("screenshot-{}-{}.png", std::process::id(), N.fetch_add(1, Ordering::Relaxed)))
}

/// Remove stills a cancelled capture left behind (older than a minute).
pub fn sweep_stale(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("screenshot-") && name.ends_with(".png")) {
            continue;
        }
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| SystemTime::now().duration_since(t).ok());
        if old.is_some_and(|age| age > Duration::from_secs(60)) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// A decoded still: the BGRA frame GPUI draws, and its size in pixels.
#[derive(Clone)]
pub struct Still {
    pub image: std::sync::Arc<RenderImage>,
    pub size: (u32, u32),
}

/// RGBA8 → the BGRA frame GPUI draws.
pub fn bgra_image(rgba: image::RgbaImage) -> std::sync::Arc<RenderImage> {
    let mut rgba = rgba;
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    std::sync::Arc::new(RenderImage::new(vec![image::Frame::new(rgba)]))
}

/// Wait for niri to finish writing the PNG at `path` (it writes from a thread after
/// answering the action, not atomically: a half-written file fails to decode and is retried),
/// decode it and delete it. Blocks up to `timeout`.
pub fn wait_for_still(path: &Path, timeout: Duration) -> Result<Still, String> {
    let deadline = Instant::now() + timeout;
    let mut last = String::from("niri did not write the screenshot");
    loop {
        if path.exists() {
            match image::open(path) {
                Ok(img) => {
                    let rgba = img.into_rgba8();
                    let size = rgba.dimensions();
                    let _ = std::fs::remove_file(path);
                    return Ok(Still { image: bgra_image(rgba), size });
                }
                Err(e) => last = format!("reading the screenshot: {e}"),
            }
        }
        if Instant::now() >= deadline {
            let _ = std::fs::remove_file(path);
            return Err(last);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

// ── result ────────────────────────────────────────────────────────────────────

/// The pixels of `r` out of a BGRA buffer of `iw`×`ih`, as RGBA.
pub fn crop_bgra(bgra: &[u8], iw: u32, ih: u32, r: PixelRect) -> Option<image::RgbaImage> {
    if bgra.len() < (iw as usize) * (ih as usize) * 4 || r.x + r.w > iw || r.y + r.h > ih {
        return None;
    }
    let mut out = Vec::with_capacity((r.w as usize) * (r.h as usize) * 4);
    for row in r.y..r.y + r.h {
        let start = ((row as usize) * (iw as usize) + r.x as usize) * 4;
        for px in bgra[start..start + (r.w as usize) * 4].chunks_exact(4) {
            out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
    }
    image::RgbaImage::from_raw(r.w, r.h, out)
}

fn encode_png(img: &image::RgbaImage) -> Result<Vec<u8>, String> {
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png).map_err(|e| format!("encoding the PNG: {e}"))?;
    Ok(buf.into_inner())
}

/// A saved screenshot.
pub struct Saved {
    pub path: PathBuf,
    pub size: (u32, u32),
    pub bytes: u64,
    pub thumb: Option<std::sync::Arc<RenderImage>>,
}

/// Longest side of the toast thumbnail, in px.
const THUMB: (u32, u32) = (120, 72);

/// Crop `sel` (logical, output-local) out of the still and save it as
/// `<dir>/Screenshot from <time>.png` (the directory is created).
pub fn save_shot(still: &Still, logical: crate::capture::Dim, sel: Rect, dir: &Path, now: NaiveDateTime) -> Result<Saved, String> {
    let map = ScaleMap { logical, image: still.size };
    let px = map.to_pixels(sel);
    let bgra = still.image.as_bytes(0).ok_or("the screenshot has no pixels")?;
    let cropped = crop_bgra(bgra, still.size.0, still.size.1, px).ok_or("the selection is outside the screenshot")?;
    let png = encode_png(&cropped)?;
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let path = unique_path(dir, &screenshot_file_name(now), |p| p.exists());
    std::fs::write(&path, &png).map_err(|e| format!("writing {}: {e}", path.display()))?;
    let (tw, th) = fit(cropped.width(), cropped.height(), THUMB.0, THUMB.1);
    let thumb = Some(bgra_image(image::imageops::thumbnail(&cropped, tw, th)));
    Ok(Saved { path, size: (cropped.width(), cropped.height()), bytes: png.len() as u64, thumb })
}

/// `w`×`h` scaled down to fit `max_w`×`max_h`, aspect kept, at least 1×1, never enlarged.
pub fn fit(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    let s = (f64::from(max_w) / f64::from(w.max(1))).min(f64::from(max_h) / f64::from(h.max(1))).min(1.0);
    (((f64::from(w) * s).round() as u32).max(1), ((f64::from(h) * s).round() as u32).max(1))
}

/// The shell command (run through niri's `SpawnSh`, so it inherits niri's environment) that
/// puts the PNG at `path` on the clipboard; `wl-copy` reads the file on stdin and keeps
/// serving the selection in the background.
pub fn copy_command(path: &Path) -> String {
    let quoted = shlex::try_quote(&path.to_string_lossy()).map(|q| q.into_owned()).unwrap_or_else(|_| "/dev/null".into());
    format!("wl-copy --type image/png < {quoted}")
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::capture::Dim;

    #[test]
    fn default_and_configured_dirs() {
        let home = Path::new("/home/u");
        let d = resolve_dirs(&[], home, None, None);
        assert_eq!(d.screenshots, PathBuf::from("/home/u/Pictures/Screenshots"));
        assert_eq!(d.recordings, PathBuf::from("/home/u/Videos/Recordings"));
        // relocated XDG directories
        let d = resolve_dirs(&[], home, Some("/data/pics".into()), Some("/data/vids".into()));
        assert_eq!(d.screenshots, PathBuf::from("/data/pics/Screenshots"));
        assert_eq!(d.recordings, PathBuf::from("/data/vids/Recordings"));
        // system then user: the user wins per key, `~` expands, junk files are skipped
        let sys = r#"{"screenshot":{"dir":"/srv/shots","recordingsDir":"~/rec"},"dock":["kitty"]}"#.to_string();
        let user = r#"{"screenshot":{"dir":"~/Pictures/Mine"}}"#.to_string();
        let d = resolve_dirs(&[sys, "{ nope".into(), user], home, None, None);
        assert_eq!(d.screenshots, PathBuf::from("/home/u/Pictures/Mine"));
        assert_eq!(d.recordings, PathBuf::from("/home/u/rec"));
        let d = resolve_dirs(&[r#"{"screenshot":{"dir":"  "}}"#.into()], home, None, None);
        assert_eq!(d.screenshots, PathBuf::from("/home/u/Pictures/Screenshots"), "blank is unset");
    }

    #[test]
    fn crop_swaps_channels_and_keeps_the_rows_straight() {
        // 4x3 BGRA image whose pixel (x, y) is B=x, G=y, R=10, A=255
        let (iw, ih) = (4u32, 3u32);
        let mut bgra = Vec::new();
        for y in 0..ih {
            for x in 0..iw {
                bgra.extend_from_slice(&[x as u8, y as u8, 10, 255]);
            }
        }
        let out = crop_bgra(&bgra, iw, ih, PixelRect { x: 1, y: 1, w: 2, h: 2 }).unwrap();
        assert_eq!(out.dimensions(), (2, 2));
        assert_eq!(out.get_pixel(0, 0).0, [10, 1, 1, 255]);
        assert_eq!(out.get_pixel(1, 1).0, [10, 2, 2, 255]);
        assert!(crop_bgra(&bgra, iw, ih, PixelRect { x: 3, y: 0, w: 2, h: 1 }).is_none());
        assert!(crop_bgra(&bgra[..8], iw, ih, PixelRect { x: 0, y: 0, w: 1, h: 1 }).is_none());
    }

    #[test]
    fn saving_a_crop_writes_a_png_of_the_selection() {
        let dir = tempfile::tempdir().unwrap();
        // 200x100 image: left half red, right half blue (BGRA)
        let mut img = image::RgbaImage::new(200, 100);
        for (x, _, p) in img.enumerate_pixels_mut() {
            *p = if x < 100 { image::Rgba([255, 0, 0, 255]) } else { image::Rgba([0, 0, 255, 255]) };
        }
        let still = Still { image: bgra_image(img), size: (200, 100) };
        let now = NaiveDate::from_ymd_opt(2026, 11, 14).unwrap().and_hms_opt(15, 4, 5).unwrap();
        // logical 100x50 at scale 2: the selection x 60..90 (logical) is pixels 120..180 = blue only
        let saved = save_shot(&still, Dim::new(100.0, 50.0), Rect::new(60.0, 10.0, 30.0, 20.0), &dir.path().join("new/sub"), now).unwrap();
        assert_eq!(saved.size, (60, 40));
        assert_eq!(saved.path.file_name().unwrap().to_str().unwrap(), "Screenshot from 2026-11-14 15-04-05.png");
        let back = image::open(&saved.path).unwrap().into_rgba8();
        assert_eq!(back.dimensions(), (60, 40));
        assert!(back.pixels().all(|p| p.0 == [0, 0, 255, 255]));
        assert!(saved.thumb.is_some());
        // a second shot in the same second does not overwrite
        let again = save_shot(&still, Dim::new(100.0, 50.0), Rect::new(0.0, 0.0, 10.0, 10.0), &dir.path().join("new/sub"), now).unwrap();
        assert_eq!(again.path.file_name().unwrap().to_str().unwrap(), "Screenshot from 2026-11-14 15-04-05 (2).png");
    }

    #[test]
    fn thumbnails_fit_without_enlarging() {
        assert_eq!(fit(1920, 1080, 120, 72), (120, 68));
        assert_eq!(fit(40, 30, 120, 72), (40, 30));
        assert_eq!(fit(1, 1000, 120, 72), (1, 72));
    }

    #[test]
    fn clipboard_command_quotes_the_path() {
        assert_eq!(
            copy_command(Path::new("/home/u/Pictures/Screenshots/Screenshot from 2026-11-14 15-04-05.png")),
            "wl-copy --type image/png < '/home/u/Pictures/Screenshots/Screenshot from 2026-11-14 15-04-05.png'"
        );
        assert_eq!(copy_command(Path::new("/tmp/it's.png")), r#"wl-copy --type image/png < "/tmp/it's.png""#);
    }

    #[test]
    fn waiting_for_a_still_decodes_and_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.png");
        image::RgbaImage::from_pixel(8, 4, image::Rgba([1, 2, 3, 255])).save(&path).unwrap();
        let s = wait_for_still(&path, Duration::from_secs(1)).unwrap();
        assert_eq!(s.size, (8, 4));
        assert_eq!(&s.image.as_bytes(0).unwrap()[..4], &[3, 2, 1, 255], "BGRA");
        assert!(!path.exists());
        let missing = wait_for_still(&dir.path().join("none.png"), Duration::from_millis(60));
        assert!(missing.is_err());
        // a truncated file keeps failing until the timeout and is cleaned up
        let bad = dir.path().join("bad.png");
        std::fs::write(&bad, b"\x89PNG\r\n\x1a\n").unwrap();
        assert!(wait_for_still(&bad, Duration::from_millis(60)).is_err());
    }
}
