//! Notification images, resolved off the service thread (`smol::unblock`): the
//! `image-data` hint becomes a (down-scaled) RGBA8 buffer, `image-path` / `app_icon`
//! become files, icon names are looked up in the icon theme.
//!
//! Priority (spec 1.2): `image-data` > `image-path` > `app_icon` > deprecated `icon_data`;
//! when nothing was sent, the `desktop-entry` / app name is tried as an icon name.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use crate::NotificationImage;

/// Largest side kept for `image-data`; bigger images are scaled down (a toast shows 32 px).
pub const MAX_SIDE: u32 = 128;
/// Refuse absurd `image-data` payloads (bytes).
const MAX_BYTES: usize = 32 * 1024 * 1024;

/// The raw `(iiibiiay)` struct of the `image-data` hint.
#[derive(Clone, Debug, PartialEq)]
pub struct RawImage {
    pub width: i32,
    pub height: i32,
    pub rowstride: i32,
    pub has_alpha: bool,
    pub bits_per_sample: i32,
    pub channels: i32,
    pub data: Vec<u8>,
}

/// Everything the Notify call said about the picture.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImageSpec {
    pub data: Option<RawImage>,
    pub path: Option<String>,
    pub app_icon: String,
    /// Deprecated `icon_data` hint.
    pub legacy_data: Option<RawImage>,
    pub desktop_entry: Option<String>,
    pub app_name: String,
}

/// Resolve the image. Blocking (decoding, file system): call through `smol::unblock`.
pub fn resolve(spec: ImageSpec) -> Option<NotificationImage> {
    if let Some(img) = spec.data.as_ref().and_then(decode_raw) {
        return Some(img);
    }
    if let Some(p) = spec.path.as_deref().filter(|p| !p.is_empty()).and_then(existing_file) {
        return Some(NotificationImage::Path(p));
    }
    if !spec.app_icon.is_empty() {
        if let Some(p) = existing_file(&spec.app_icon) {
            return Some(NotificationImage::Path(p));
        }
        if !looks_like_path(&spec.app_icon) {
            return Some(lookup_icon(&spec.app_icon).map_or_else(|| NotificationImage::Name(spec.app_icon.clone()), NotificationImage::Path));
        }
    }
    if let Some(img) = spec.legacy_data.as_ref().and_then(decode_raw) {
        return Some(img);
    }
    let guesses = spec
        .desktop_entry
        .iter()
        .map(|d| d.trim_end_matches(".desktop").to_string())
        .chain((!spec.app_name.is_empty()).then(|| spec.app_name.to_lowercase().replace(' ', "-")));
    for g in guesses {
        if let Some(p) = lookup_icon(&g) {
            return Some(NotificationImage::Path(p));
        }
    }
    None
}

fn looks_like_path(s: &str) -> bool {
    s.starts_with('/') || s.starts_with("file://")
}

fn existing_file(s: &str) -> Option<PathBuf> {
    let p = Path::new(s.strip_prefix("file://").unwrap_or(s));
    (p.is_absolute() && p.is_file()).then(|| p.to_path_buf())
}

/// Icon-theme lookup by name, 48 px (the toast box is 32 px at up to 2x).
fn lookup_icon(name: &str) -> Option<PathBuf> {
    static THEME: OnceLock<String> = OnceLock::new();
    let theme = THEME.get_or_init(|| freedesktop_icons::default_theme_gtk().unwrap_or_else(|| "hicolor".into()));
    freedesktop_icons::lookup(name)
        .with_size(48)
        .with_theme(theme)
        .with_cache()
        .find()
        .or_else(|| freedesktop_icons::lookup(name).with_size(48).with_theme("hicolor").with_cache().find())
}

/// `image-data` -> RGBA8 (rowstride honoured, RGB gets opaque alpha), max side [`MAX_SIDE`].
pub fn decode_raw(raw: &RawImage) -> Option<NotificationImage> {
    let (w, h) = (u32::try_from(raw.width).ok().filter(|v| *v > 0)?, u32::try_from(raw.height).ok().filter(|v| *v > 0)?);
    if raw.bits_per_sample != 8 || !(raw.channels == 3 || raw.channels == 4) {
        return None;
    }
    let channels = raw.channels as usize;
    let rowstride = usize::try_from(raw.rowstride).ok()?;
    let row_bytes = (w as usize).checked_mul(channels)?;
    if rowstride < row_bytes || (w as usize).checked_mul(h as usize)?.checked_mul(4)? > MAX_BYTES {
        return None;
    }
    let needed = rowstride.checked_mul(h as usize - 1)?.checked_add(row_bytes)?;
    if raw.data.len() < needed {
        return None;
    }
    let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
    for y in 0..h as usize {
        let row = &raw.data[y * rowstride..y * rowstride + row_bytes];
        if channels == 4 {
            rgba.extend_from_slice(row);
        } else {
            for px in row.chunks_exact(3) {
                rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
        }
    }
    let (mut w, mut h) = (w, h);
    if w.max(h) > MAX_SIDE {
        let buf = image::RgbaImage::from_raw(w, h, rgba)?;
        let scale = MAX_SIDE as f32 / w.max(h) as f32;
        let (nw, nh) = (((w as f32 * scale).round() as u32).max(1), ((h as f32 * scale).round() as u32).max(1));
        rgba = image::imageops::resize(&buf, nw, nh, image::imageops::FilterType::Triangle).into_raw();
        (w, h) = (nw, nh);
    }
    Some(NotificationImage::Rgba { width: w, height: h, data: Arc::new(rgba) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(w: i32, h: i32, channels: i32, rowstride: i32) -> RawImage {
        RawImage {
            width: w,
            height: h,
            rowstride,
            has_alpha: channels == 4,
            bits_per_sample: 8,
            channels,
            data: (0..rowstride * h).map(|i| (i % 251) as u8).collect(),
        }
    }

    #[test]
    fn rgb_gets_opaque_alpha_and_rowstride_padding_is_skipped() {
        // 2x2 RGB with 2 bytes of padding per row (stride 8)
        let r = RawImage {
            width: 2,
            height: 2,
            rowstride: 8,
            has_alpha: false,
            bits_per_sample: 8,
            channels: 3,
            data: vec![1, 2, 3, 4, 5, 6, 99, 99, 7, 8, 9, 10, 11, 12, 99, 99],
        };
        let NotificationImage::Rgba { width, height, data } = decode_raw(&r).unwrap() else { panic!() };
        assert_eq!((width, height), (2, 2));
        assert_eq!(*data, vec![1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255]);
    }

    #[test]
    fn rgba_passes_through() {
        let r = raw(3, 2, 4, 12);
        let NotificationImage::Rgba { data, .. } = decode_raw(&r).unwrap() else { panic!() };
        assert_eq!(data.len(), 3 * 2 * 4);
        assert_eq!(&data[..12], &r.data[..12]);
    }

    #[test]
    fn big_images_are_downscaled() {
        let r = raw(512, 256, 4, 512 * 4);
        let NotificationImage::Rgba { width, height, data } = decode_raw(&r).unwrap() else { panic!() };
        assert_eq!((width, height), (128, 64));
        assert_eq!(data.len(), 128 * 64 * 4);
    }

    #[test]
    fn malformed_data_is_rejected() {
        assert!(decode_raw(&RawImage { bits_per_sample: 16, ..raw(2, 2, 4, 8) }).is_none());
        assert!(decode_raw(&RawImage { channels: 2, ..raw(2, 2, 4, 8) }).is_none());
        assert!(decode_raw(&RawImage { width: 0, ..raw(2, 2, 4, 8) }).is_none());
        assert!(decode_raw(&RawImage { width: -1, ..raw(2, 2, 4, 8) }).is_none());
        assert!(decode_raw(&RawImage { rowstride: 4, ..raw(2, 2, 4, 8) }).is_none());
        let mut short = raw(4, 4, 4, 16);
        short.data.truncate(10);
        assert!(decode_raw(&short).is_none());
        assert!(decode_raw(&RawImage { width: i32::MAX, height: i32::MAX, rowstride: i32::MAX, data: vec![0; 16], ..raw(1, 1, 4, 4) }).is_none());
    }

    #[test]
    fn resolve_prefers_image_data_then_path() {
        let spec = ImageSpec { data: Some(raw(2, 2, 4, 8)), path: Some("/nonexistent".into()), ..Default::default() };
        assert!(matches!(resolve(spec), Some(NotificationImage::Rgba { .. })));
        let spec = ImageSpec { path: Some("/etc/hostname".into()), ..Default::default() };
        assert_eq!(resolve(spec), Some(NotificationImage::Path("/etc/hostname".into())));
        let spec = ImageSpec { app_icon: "file:///etc/hostname".into(), ..Default::default() };
        assert_eq!(resolve(spec), Some(NotificationImage::Path("/etc/hostname".into())));
        // missing file path -> nothing (we do not invent a name out of a path)
        let spec = ImageSpec { app_icon: "/nonexistent.png".into(), ..Default::default() };
        assert_eq!(resolve(spec), None);
    }
}
