//! The history store: ordered entries in redb (`<state>/clipboard.redb`), image payloads
//! as files beside it (`<state>/clipboard/<id>.<ext>` + `<id>.thumb.png`).
//!
//! Pure logic (no Wayland): classification, dedupe (re-copying an existing entry moves it
//! to the top), the unpinned cap (pinned entries are never evicted) and persistence.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

use crate::kind::{Colour, Kind, detect};

/// Entries kept besides the pinned ones.
pub const MAX_UNPINNED: usize = 200;
/// Bytes of text kept in [`Entry::preview`].
pub const PREVIEW_MAX: usize = 4096;
/// Longest edge box of the stored thumbnail (2× the panel's 286×160 preview).
pub const THUMB_MAX: (u32, u32) = (572, 320);

const ENTRIES: TableDefinition<u64, &[u8]> = TableDefinition::new("entries");
const BLOBS: TableDefinition<(u64, u32), &[u8]> = TableDefinition::new("blobs");

/// One stored representation of an entry (index 0 is the primary one).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Rep {
    mime: String,
    size: u64,
    /// File name inside the image dir (image payloads); text lives in `BLOBS`.
    file: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct ImgMeta {
    width: u32,
    height: u32,
    thumb: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Meta {
    id: u64,
    kind: Kind,
    /// Unix milliseconds of the last (re-)copy; strictly increasing across entries.
    ts_ms: u64,
    pinned: bool,
    hash: u64,
    preview: String,
    truncated: bool,
    lines: u32,
    colour: Option<Colour>,
    reps: Vec<Rep>,
    image: Option<ImgMeta>,
}

/// An image payload on disk.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageInfo {
    pub width: u32,
    pub height: u32,
    /// The original bytes (png / jpeg / …).
    pub path: PathBuf,
    /// Downscaled PNG (≤ [`THUMB_MAX`]), when it could be decoded.
    pub thumb: Option<PathBuf>,
}

/// A history entry as published in the state: metadata and a small preview, never the
/// full payload.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: u64,
    pub kind: Kind,
    /// MIME type of the primary stored representation.
    pub mime: String,
    /// Other stored representations (e.g. `text/html` next to the text).
    pub alt_mimes: Vec<String>,
    /// Text preview (≤ [`PREVIEW_MAX`] bytes; empty for images).
    pub preview: String,
    /// The preview is shorter than the text.
    pub truncated: bool,
    /// Lines of the full text (1 for single-line text; 0 for images).
    pub lines: u32,
    /// Bytes of the primary representation.
    pub size: u64,
    /// Unix milliseconds of the last (re-)copy.
    pub copied_at: u64,
    pub pinned: bool,
    pub colour: Option<Colour>,
    pub image: Option<ImageInfo>,
}

impl Entry {
    /// First non-empty line, whitespace collapsed (the list row text).
    pub fn first_line(&self) -> String {
        self.preview
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
            .unwrap_or_default()
    }
}

/// What the Wayland side captured: representations (MIME, bytes), primary first.
#[derive(Clone, Debug, PartialEq)]
pub struct Captured {
    pub reps: Vec<(String, Vec<u8>)>,
}

/// Result of [`Store::insert`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Inserted {
    pub id: u64,
    /// An existing entry was moved to the top instead of adding a new one.
    pub duplicate: bool,
}

/// The payload to serve when an entry becomes the clipboard again.
#[derive(Clone, Debug, PartialEq)]
pub struct Payload {
    pub reps: Vec<(String, Vec<u8>)>,
}

pub struct Store {
    db: Database,
    persistent: bool,
    image_dir: PathBuf,
    /// Newest first.
    metas: Vec<Meta>,
    next_id: u64,
    last_ts: u64,
    cap: usize,
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn truncate_at_boundary(s: &str, max: usize) -> (&str, bool) {
    if s.len() <= max {
        return (s, false);
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    (&s[..end], true)
}

/// File extension for an image MIME type.
pub fn image_ext(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        "image/svg+xml" => "svg",
        _ => "bin",
    }
}

/// Is `mime` a plain-text representation (so the aliases `UTF8_STRING`, … apply)?
pub fn is_plain_text_mime(mime: &str) -> bool {
    mime.starts_with("text/plain") || matches!(mime, "UTF8_STRING" | "STRING" | "TEXT")
}

/// `text/html` → readable text (tags dropped, common entities decoded, whitespace collapsed).
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    let mut skip_until: Option<&'static str> = None;
    let lower = html.to_ascii_lowercase();
    let bytes = html.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if let Some(end) = skip_until {
            match lower[i..].find(end) {
                Some(p) => {
                    i += p + end.len();
                    skip_until = None;
                    continue;
                }
                None => break,
            }
        }
        let c = bytes[i];
        if in_tag {
            if c == b'>' {
                in_tag = false;
            }
            i += 1;
        } else if c == b'<' {
            if lower[i..].starts_with("<script") {
                skip_until = Some("</script>");
            } else if lower[i..].starts_with("<style") {
                skip_until = Some("</style>");
            } else if lower[i..].starts_with("<br") || lower[i..].starts_with("</p") || lower[i..].starts_with("</div") {
                out.push('\n');
            }
            in_tag = true;
            i += 1;
        } else {
            let ch = html[i..].chars().next().unwrap_or(' ');
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    decoded.lines().map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n")
}

/// `text/uri-list` → one decoded entry per line (`file://` URIs become paths).
pub fn uri_list_to_text(list: &str) -> String {
    list.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| match l.strip_prefix("file://") {
            Some(rest) => percent_decode(rest.strip_prefix("localhost").unwrap_or(rest)),
            None => l.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = ((b[i + 1] as char).to_digit(16), (b[i + 2] as char).to_digit(16))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The text a primary representation reads as, by MIME type.
fn text_of(mime: &str, bytes: &[u8]) -> String {
    let raw = String::from_utf8_lossy(bytes);
    match mime {
        "text/html" => html_to_text(&raw),
        "text/uri-list" => uri_list_to_text(&raw),
        _ => raw.into_owned(),
    }
}

impl Store {
    /// Open (or create) the store. Never fails: if the database cannot be opened (another
    /// process holds it, read-only state dir) history lives in memory for this run.
    pub fn open(db_path: &Path, image_dir: &Path) -> Store {
        Self::open_with(db_path, image_dir, MAX_UNPINNED)
    }

    pub fn open_with(db_path: &Path, image_dir: &Path, cap: usize) -> Store {
        if let Some(parent) = db_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::create_dir_all(image_dir);
        let (db, persistent) = match Database::create(db_path) {
            Ok(db) => (db, true),
            Err(e) => {
                tracing::warn!("clipboard history not persisted ({}): {e}", db_path.display());
                let mem = Database::builder()
                    .create_with_backend(redb::backends::InMemoryBackend::new())
                    .expect("in-memory redb");
                (mem, false)
            }
        };
        let mut store =
            Store { db, persistent, image_dir: image_dir.to_path_buf(), metas: Vec::new(), next_id: 1, last_ts: 0, cap };
        store.load();
        store
    }

    fn load(&mut self) {
        let run = || -> Result<Vec<Meta>, redb::Error> {
            let tx = self.db.begin_read()?;
            let table = match tx.open_table(ENTRIES) {
                Ok(t) => t,
                Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
                Err(e) => return Err(e.into()),
            };
            let mut out = Vec::new();
            for row in table.iter()? {
                let (_, v) = row?;
                match serde_json::from_slice::<Meta>(v.value()) {
                    Ok(m) => out.push(m),
                    Err(e) => tracing::warn!("skipping unreadable clipboard entry: {e}"),
                }
            }
            Ok(out)
        };
        let mut metas = run().unwrap_or_else(|e| {
            tracing::warn!("reading clipboard history: {e}");
            Vec::new()
        });
        metas.sort_by(|a, b| b.ts_ms.cmp(&a.ts_ms).then(b.id.cmp(&a.id)));
        self.next_id = metas.iter().map(|m| m.id).max().map_or(1, |m| m + 1);
        self.last_ts = metas.iter().map(|m| m.ts_ms).max().unwrap_or(0);
        self.metas = metas;
        // Enforce the cap on whatever an older run (or a different cap) left behind.
        let evicted = self.evict();
        if !evicted.is_empty() {
            self.persist(&[], &evicted, &[]);
        }
    }

    /// History is stored on disk (false: in-memory fallback).
    pub fn persistent(&self) -> bool {
        self.persistent
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.metas.len()
    }

    /// Entries, newest first.
    pub fn entries(&self) -> Vec<Entry> {
        self.metas.iter().map(|m| self.entry(m)).collect()
    }

    pub fn entry_by_id(&self, id: u64) -> Option<Entry> {
        self.metas.iter().find(|m| m.id == id).map(|m| self.entry(m))
    }

    fn entry(&self, m: &Meta) -> Entry {
        let primary = &m.reps[0];
        Entry {
            id: m.id,
            kind: m.kind,
            mime: primary.mime.clone(),
            alt_mimes: m.reps.iter().skip(1).map(|r| r.mime.clone()).collect(),
            preview: m.preview.clone(),
            truncated: m.truncated,
            lines: m.lines,
            size: primary.size,
            copied_at: m.ts_ms,
            pinned: m.pinned,
            colour: m.colour,
            image: m.image.as_ref().zip(primary.file.as_ref()).map(|(img, file)| ImageInfo {
                width: img.width,
                height: img.height,
                path: self.image_dir.join(file),
                thumb: img.thumb.as_ref().map(|t| self.image_dir.join(t)),
            }),
        }
    }

    fn next_ts(&mut self, now_ms: u64) -> u64 {
        self.last_ts = now_ms.max(self.last_ts + 1);
        self.last_ts
    }

    /// Store a capture (primary representation first). An existing entry with the same
    /// primary bytes is moved to the top instead (keeping its pin).
    pub fn insert(&mut self, cap: Captured, now_ms: u64) -> Option<Inserted> {
        let (mime, bytes) = cap.reps.first()?;
        if bytes.is_empty() {
            return None;
        }
        let hash = fnv1a(bytes);
        let size = bytes.len() as u64;
        if let Some(pos) =
            self.metas.iter().position(|m| m.hash == hash && m.reps[0].size == size && m.kind_is_image() == mime.starts_with("image/"))
        {
            let id = self.touch_at(pos, now_ms);
            return Some(Inserted { id, duplicate: true });
        }

        let id = self.next_id;
        self.next_id += 1;
        let ts = self.next_ts(now_ms);
        let mut meta = Meta {
            id,
            kind: Kind::Text,
            ts_ms: ts,
            pinned: false,
            hash,
            preview: String::new(),
            truncated: false,
            lines: 0,
            colour: None,
            reps: Vec::with_capacity(cap.reps.len()),
            image: None,
        };
        let mut blobs: Vec<(u32, Vec<u8>)> = Vec::new();
        let mut files: Vec<PathBuf> = Vec::new();

        if mime.starts_with("image/") {
            meta.kind = Kind::Image;
            let file = format!("{id}.{}", image_ext(mime));
            let path = self.image_dir.join(&file);
            if let Err(e) = std::fs::write(&path, bytes) {
                tracing::warn!("saving clipboard image {}: {e}", path.display());
                return None;
            }
            files.push(path);
            meta.reps.push(Rep { mime: mime.clone(), size, file: Some(file) });
            if let Some((w, h, thumb)) = make_thumbnail(bytes) {
                let thumb_name = format!("{id}.thumb.png");
                let thumb_path = self.image_dir.join(&thumb_name);
                let thumb_ok = std::fs::write(&thumb_path, thumb).is_ok();
                if thumb_ok {
                    files.push(thumb_path);
                }
                meta.image = Some(ImgMeta { width: w, height: h, thumb: thumb_ok.then_some(thumb_name) });
            } else {
                meta.image = Some(ImgMeta { width: 0, height: 0, thumb: None });
            }
        } else {
            let text = text_of(mime, bytes);
            let detected = detect(&text, |p| p.exists());
            meta.kind = detected.kind;
            meta.colour = detected.colour;
            meta.lines = text.lines().count().max(1) as u32;
            let (prev, truncated) = truncate_at_boundary(&text, PREVIEW_MAX);
            meta.preview = prev.to_string();
            meta.truncated = truncated;
            meta.reps.push(Rep { mime: mime.clone(), size, file: None });
            blobs.push((0, bytes.clone()));
            let mut seen: HashSet<&str> = HashSet::from([mime.as_str()]);
            for (alt_mime, alt_bytes) in cap.reps.iter().skip(1) {
                if seen.insert(alt_mime.as_str()) && !alt_bytes.is_empty() {
                    blobs.push((meta.reps.len() as u32, alt_bytes.clone()));
                    meta.reps.push(Rep { mime: alt_mime.clone(), size: alt_bytes.len() as u64, file: None });
                }
            }
        }

        self.metas.insert(0, meta.clone());
        let evicted = self.evict();
        self.persist(&[(meta, blobs)], &evicted, &[]);
        Some(Inserted { id, duplicate: false })
    }

    fn touch_at(&mut self, pos: usize, now_ms: u64) -> u64 {
        let ts = self.next_ts(now_ms);
        let mut meta = self.metas.remove(pos);
        meta.ts_ms = ts;
        let id = meta.id;
        self.metas.insert(0, meta.clone());
        self.persist(&[], &[], &[meta]);
        id
    }

    /// Move an entry to the top (it became the clipboard again).
    pub fn touch(&mut self, id: u64, now_ms: u64) -> bool {
        match self.metas.iter().position(|m| m.id == id) {
            Some(pos) => {
                self.touch_at(pos, now_ms);
                true
            }
            None => false,
        }
    }

    pub fn set_pinned(&mut self, id: u64, pinned: bool) -> bool {
        let Some(m) = self.metas.iter_mut().find(|m| m.id == id) else { return false };
        if m.pinned == pinned {
            return true;
        }
        m.pinned = pinned;
        let meta = m.clone();
        self.persist(&[], &[], &[meta]);
        if !pinned {
            // un-pinning can push the unpinned count over the cap
            let evicted = self.evict();
            self.persist(&[], &evicted, &[]);
        }
        true
    }

    pub fn remove(&mut self, id: u64) -> bool {
        let Some(pos) = self.metas.iter().position(|m| m.id == id) else { return false };
        let meta = self.metas.remove(pos);
        self.persist(&[], &[meta], &[]);
        true
    }

    /// Remove every unpinned entry; returns how many.
    pub fn clear_unpinned(&mut self) -> usize {
        let (keep, drop): (Vec<_>, Vec<_>) = std::mem::take(&mut self.metas).into_iter().partition(|m| m.pinned);
        self.metas = keep;
        let n = drop.len();
        if n > 0 {
            self.persist(&[], &drop, &[]);
        }
        n
    }

    /// Drop the oldest unpinned entries beyond the cap; returns them (for deletion).
    fn evict(&mut self) -> Vec<Meta> {
        let unpinned = self.metas.iter().filter(|m| !m.pinned).count();
        if unpinned <= self.cap {
            return Vec::new();
        }
        let mut excess = unpinned - self.cap;
        let mut evicted = Vec::new();
        // metas are newest first: walk from the back
        let mut i = self.metas.len();
        while i > 0 && excess > 0 {
            i -= 1;
            if !self.metas[i].pinned {
                evicted.push(self.metas.remove(i));
                excess -= 1;
            }
        }
        evicted
    }

    /// Representations of an entry with their bytes, primary first.
    pub fn payload(&self, id: u64) -> Option<Payload> {
        let meta = self.metas.iter().find(|m| m.id == id)?;
        let mut reps = Vec::with_capacity(meta.reps.len());
        let tx = self.db.begin_read().ok();
        let blobs = tx.as_ref().and_then(|t| t.open_table(BLOBS).ok());
        for (i, rep) in meta.reps.iter().enumerate() {
            let bytes = match &rep.file {
                Some(file) => std::fs::read(self.image_dir.join(file)).ok()?,
                None => blobs.as_ref()?.get((id, i as u32)).ok().flatten()?.value().to_vec(),
            };
            reps.push((rep.mime.clone(), bytes));
        }
        Some(Payload { reps })
    }

    /// Path of the primary payload when it is a file (images).
    pub fn payload_file(&self, id: u64) -> Option<PathBuf> {
        let meta = self.metas.iter().find(|m| m.id == id)?;
        meta.reps[0].file.as_ref().map(|f| self.image_dir.join(f))
    }

    /// Write inserts / updates / deletions in one transaction and delete the image files of
    /// removed entries.
    fn persist(&self, inserts: &[(Meta, Vec<(u32, Vec<u8>)>)], removed: &[Meta], updated: &[Meta]) {
        let run = || -> Result<(), redb::Error> {
            let tx = self.db.begin_write()?;
            {
                let mut entries = tx.open_table(ENTRIES)?;
                let mut blobs = tx.open_table(BLOBS)?;
                for (meta, bl) in inserts {
                    entries.insert(meta.id, serde_json::to_vec(meta).unwrap_or_default().as_slice())?;
                    for (idx, bytes) in bl {
                        blobs.insert((meta.id, *idx), bytes.as_slice())?;
                    }
                }
                for meta in updated {
                    entries.insert(meta.id, serde_json::to_vec(meta).unwrap_or_default().as_slice())?;
                }
                for meta in removed {
                    entries.remove(meta.id)?;
                    for i in 0..meta.reps.len() {
                        blobs.remove((meta.id, i as u32))?;
                    }
                }
            }
            tx.commit()?;
            Ok(())
        };
        if let Err(e) = run() {
            tracing::warn!("writing clipboard history: {e}");
        }
        for meta in removed {
            for rep in &meta.reps {
                if let Some(f) = &rep.file {
                    let _ = std::fs::remove_file(self.image_dir.join(f));
                }
            }
            if let Some(t) = meta.image.as_ref().and_then(|i| i.thumb.as_ref()) {
                let _ = std::fs::remove_file(self.image_dir.join(t));
            }
        }
    }
}

impl Meta {
    fn kind_is_image(&self) -> bool {
        self.kind == Kind::Image
    }
}

/// Decode `bytes`, return `(width, height, thumbnail PNG)`.
fn make_thumbnail(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::load_from_memory(bytes).ok()?;
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return None;
    }
    let thumb = if w > THUMB_MAX.0 || h > THUMB_MAX.1 { img.thumbnail(THUMB_MAX.0, THUMB_MAX.1) } else { img };
    let mut out = std::io::Cursor::new(Vec::new());
    thumb.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some((w, h, out.into_inner()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Captured {
        Captured { reps: vec![("text/plain;charset=utf-8".into(), s.as_bytes().to_vec())] }
    }

    fn png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba(rgba));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img).write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    fn open(dir: &Path, cap: usize) -> Store {
        Store::open_with(&dir.join("clipboard.redb"), &dir.join("clipboard"), cap)
    }

    #[test]
    fn insert_orders_newest_first_and_classifies() {
        let d = tempfile::tempdir().unwrap();
        let mut s = open(d.path(), 10);
        s.insert(text("hello world"), 1000).unwrap();
        s.insert(text("https://example.com/a"), 2000).unwrap();
        s.insert(text("#E0A458"), 3000).unwrap();
        let e = s.entries();
        assert_eq!(e.iter().map(|e| e.kind).collect::<Vec<_>>(), [Kind::Colour, Kind::Link, Kind::Text]);
        assert_eq!(e[0].colour.map(|c| c.hex()).as_deref(), Some("#E0A458"));
        assert_eq!(e[2].first_line(), "hello world");
        assert!(e[0].copied_at > e[1].copied_at && e[1].copied_at > e[2].copied_at);
    }

    #[test]
    fn dedupe_moves_to_top_and_keeps_pin() {
        let d = tempfile::tempdir().unwrap();
        let mut s = open(d.path(), 10);
        let a = s.insert(text("a"), 10).unwrap();
        s.insert(text("b"), 20).unwrap();
        assert!(s.set_pinned(a.id, true));
        let again = s.insert(text("a"), 5).unwrap();
        assert_eq!(again, Inserted { id: a.id, duplicate: true });
        let e = s.entries();
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].id, a.id);
        assert!(e[0].pinned);
        // monotonic even with a clock that went backwards
        assert!(e[0].copied_at > e[1].copied_at);
    }

    #[test]
    fn eviction_drops_oldest_unpinned_only() {
        let d = tempfile::tempdir().unwrap();
        let mut s = open(d.path(), 3);
        let first = s.insert(text("pinned"), 1).unwrap();
        s.set_pinned(first.id, true);
        for i in 0..6 {
            s.insert(text(&format!("item {i}")), 10 + i).unwrap();
        }
        let e = s.entries();
        // 3 unpinned + 1 pinned
        assert_eq!(e.len(), 4);
        assert!(e.iter().any(|e| e.id == first.id && e.pinned));
        let texts: Vec<_> = e.iter().filter(|e| !e.pinned).map(|e| e.preview.clone()).collect();
        assert_eq!(texts, ["item 5", "item 4", "item 3"]);
        // un-pinning re-applies the cap
        s.set_pinned(first.id, false);
        assert_eq!(s.len(), 3);
        assert!(s.entry_by_id(first.id).is_none());
    }

    #[test]
    fn pin_remove_clear() {
        let d = tempfile::tempdir().unwrap();
        let mut s = open(d.path(), 10);
        let a = s.insert(text("a"), 1).unwrap().id;
        let b = s.insert(text("b"), 2).unwrap().id;
        let c = s.insert(text("c"), 3).unwrap().id;
        s.set_pinned(b, true);
        assert!(s.remove(c));
        assert!(!s.remove(c));
        assert_eq!(s.len(), 2);
        assert_eq!(s.clear_unpinned(), 1);
        assert_eq!(s.entries().iter().map(|e| e.id).collect::<Vec<_>>(), [b]);
        assert!(s.payload(a).is_none());
        assert!(s.set_pinned(b, true));
        assert!(!s.set_pinned(a, true));
    }

    #[test]
    fn db_round_trip_with_alternatives() {
        let d = tempfile::tempdir().unwrap();
        let (id_t, id_i) = {
            let mut s = open(d.path(), 10);
            assert!(s.persistent());
            let t = s
                .insert(
                    Captured {
                        reps: vec![
                            ("text/plain;charset=utf-8".into(), b"plain text".to_vec()),
                            ("text/html".into(), b"<b>plain</b> text".to_vec()),
                        ],
                    },
                    100,
                )
                .unwrap()
                .id;
            s.set_pinned(t, true);
            let i = s.insert(Captured { reps: vec![("image/png".into(), png(40, 20, [255, 0, 0, 128]))] }, 200).unwrap().id;
            (t, i)
        };
        let s = open(d.path(), 10);
        let e = s.entries();
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].id, id_i);
        assert_eq!(e[0].kind, Kind::Image);
        let img = e[0].image.as_ref().unwrap();
        assert_eq!((img.width, img.height), (40, 20));
        assert!(img.path.exists());
        assert!(img.thumb.as_ref().unwrap().exists());
        assert_eq!(e[1].id, id_t);
        assert!(e[1].pinned);
        assert_eq!(e[1].alt_mimes, ["text/html"]);
        let p = s.payload(id_t).unwrap();
        assert_eq!(p.reps[0], ("text/plain;charset=utf-8".to_string(), b"plain text".to_vec()));
        assert_eq!(p.reps[1].0, "text/html");
        let p = s.payload(id_i).unwrap();
        assert_eq!(p.reps.len(), 1);
        assert_eq!(p.reps[0].0, "image/png");
        // a new entry after reload does not reuse ids
        let mut s = s;
        let n = s.insert(text("new"), 300).unwrap().id;
        assert!(n > id_i);
    }

    #[test]
    fn image_removal_deletes_files() {
        let d = tempfile::tempdir().unwrap();
        let mut s = open(d.path(), 10);
        let id = s.insert(Captured { reps: vec![("image/png".into(), png(900, 500, [0, 255, 0, 255]))] }, 1).unwrap().id;
        let e = s.entry_by_id(id).unwrap();
        let img = e.image.unwrap();
        // 900x500 → thumbnail fits 572x320
        let thumb = image::open(img.thumb.as_ref().unwrap()).unwrap();
        assert!(thumb.width() <= THUMB_MAX.0 && thumb.height() <= THUMB_MAX.1);
        assert_eq!((img.width, img.height), (900, 500));
        assert!(s.remove(id));
        assert!(!img.path.exists());
        assert!(!img.thumb.unwrap().exists());
    }

    #[test]
    fn undecodable_image_is_kept_without_dimensions() {
        let d = tempfile::tempdir().unwrap();
        let mut s = open(d.path(), 10);
        let id = s.insert(Captured { reps: vec![("image/bmp".into(), vec![1, 2, 3, 4])] }, 1).unwrap().id;
        let img = s.entry_by_id(id).unwrap().image.unwrap();
        assert_eq!((img.width, img.height), (0, 0));
        assert!(img.thumb.is_none());
        assert_eq!(s.payload(id).unwrap().reps[0].1, vec![1, 2, 3, 4]);
    }

    #[test]
    fn empty_payload_is_ignored_and_preview_truncates() {
        let d = tempfile::tempdir().unwrap();
        let mut s = open(d.path(), 10);
        assert!(s.insert(text(""), 1).is_none());
        let big = "é".repeat(PREVIEW_MAX);
        let id = s.insert(text(&big), 2).unwrap().id;
        let e = s.entry_by_id(id).unwrap();
        assert!(e.truncated);
        assert!(e.preview.len() <= PREVIEW_MAX);
        assert_eq!(e.size, big.len() as u64);
        assert_eq!(s.payload(id).unwrap().reps[0].1.len(), big.len());
    }

    #[test]
    fn html_and_uri_list_texts() {
        assert_eq!(html_to_text("<p>Hello <b>big</b> &amp; world</p><script>x()</script><div>next</div>"), "Hello big & world\nnext");
        assert_eq!(uri_list_to_text("# c\nfile:///home/me/a%20b.txt\nhttps://x.org/"), "/home/me/a b.txt\nhttps://x.org/");
        let d = tempfile::tempdir().unwrap();
        let mut s = open(d.path(), 10);
        let id = s.insert(Captured { reps: vec![("text/uri-list".into(), b"file:///etc/hosts\r\n".to_vec())] }, 1).unwrap().id;
        let e = s.entry_by_id(id).unwrap();
        assert_eq!(e.kind, Kind::Path);
        assert_eq!(e.first_line(), "/etc/hosts");
        let id = s.insert(Captured { reps: vec![("text/html".into(), b"<a href=x>link text</a>".to_vec())] }, 2).unwrap().id;
        assert_eq!(s.entry_by_id(id).unwrap().first_line(), "link text");
    }
}
