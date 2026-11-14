//! Pure panel logic: filters, counts, search, and the text of rows and meta lines.

use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};
use ashd_svc_clipboard::{Entry, Kind};

/// Rows the list shows at once (380 px body − 12 px padding at 36 px per row).
pub const MAX_VISIBLE: usize = 10;

/// The header chips.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    /// Everything that is not an image (text, links, paths, colours, code).
    Text,
    Images,
    Pinned,
}

impl Filter {
    pub const ALL: [Filter; 4] = [Filter::All, Filter::Text, Filter::Images, Filter::Pinned];

    pub fn label(self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::Text => "Text",
            Filter::Images => "Images",
            Filter::Pinned => "Pinned",
        }
    }

    pub fn next(self) -> Filter {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    pub fn prev(self) -> Filter {
        Self::ALL[(self.index() + Self::ALL.len() - 1) % Self::ALL.len()]
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|f| *f == self).unwrap_or(0)
    }

    pub fn accepts(self, e: &Entry) -> bool {
        match self {
            Filter::All => true,
            Filter::Text => e.kind != Kind::Image,
            Filter::Images => e.kind == Kind::Image,
            Filter::Pinned => e.pinned,
        }
    }
}

/// Chip counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub all: usize,
    pub text: usize,
    pub images: usize,
    pub pinned: usize,
}

impl Counts {
    pub fn of(&self, f: Filter) -> usize {
        match f {
            Filter::All => self.all,
            Filter::Text => self.text,
            Filter::Images => self.images,
            Filter::Pinned => self.pinned,
        }
    }
}

pub fn counts(entries: &[Entry]) -> Counts {
    let mut c = Counts { all: entries.len(), ..Counts::default() };
    for e in entries {
        if e.kind == Kind::Image {
            c.images += 1;
        } else {
            c.text += 1;
        }
        c.pinned += usize::from(e.pinned);
    }
    c
}

/// What the search matches against: the text preview (first 1 KB), or a descriptive line
/// for images (so "png" or "1920" find screenshots).
fn haystack(e: &Entry) -> String {
    let head: String = e.preview.chars().take(1024).collect();
    match e.image.as_ref() {
        Some(img) => format!("image {} {}x{}", e.mime, img.width, img.height),
        None => head,
    }
}

pub fn matcher() -> Matcher {
    Matcher::new(Config::DEFAULT)
}

/// Indices (into `entries`) of the rows to show: the filter first, then — when `query` is
/// not blank — a fuzzy match, best score first (recency breaks ties; no query keeps the
/// history order).
pub fn visible(entries: &[Entry], filter: Filter, query: &str, matcher: &mut Matcher) -> Vec<usize> {
    let candidates = entries.iter().enumerate().filter(|(_, e)| filter.accepts(e)).map(|(i, _)| i);
    let query = query.trim();
    if query.is_empty() {
        return candidates.collect();
    }
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut buf = Vec::new();
    let mut scored: Vec<(usize, u32)> = candidates
        .filter_map(|i| {
            let hay = haystack(&entries[i]);
            pattern.score(Utf32Str::new(&hay, &mut buf), matcher).map(|s| (i, s))
        })
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.into_iter().map(|(i, _)| i).collect()
}

/// `now` / `5m` / `3h` / `2d` / `6w` (the row's age column).
pub fn age_label(now_ms: u64, then_ms: u64) -> String {
    let s = now_ms.saturating_sub(then_ms) / 1000;
    match s {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h", s / 3600),
        86_400..=604_799 => format!("{}d", s / 86_400),
        _ => format!("{}w", s / 604_800),
    }
}

/// `412 KB`, `1.4 MB`, `38 B`.
pub fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{:.1} MB", b / (KB * KB))
    }
}

/// The meta line above the preview: `image/png · 1920×1080 · 412 KB`.
pub fn meta_line(e: &Entry) -> String {
    let mut parts = vec![e.mime.clone()];
    if let Some(img) = &e.image
        && img.width > 0
    {
        parts.push(format!("{}×{}", img.width, img.height));
    }
    parts.push(human_size(e.size));
    if e.kind != Kind::Image && e.lines > 1 {
        parts.push(format!("{} lines", e.lines));
    }
    parts.join(" · ")
}

/// The row text: the first line of text, or the meta line for images.
pub fn row_text(e: &Entry) -> String {
    if e.kind == Kind::Image {
        return meta_line(e);
    }
    let line = e.first_line();
    if line.is_empty() { "(blank)".into() } else { line }
}

/// 1–9 → row offset in the visible window; anything else `None`.
pub fn quick_pick(ch: &str) -> Option<usize> {
    let mut chars = ch.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    c.to_digit(10).filter(|d| (1..=9).contains(d)).map(|d| d as usize - 1)
}

/// Keep the selected row inside the window of [`MAX_VISIBLE`] rows starting at `scroll`.
pub fn scroll_into_view(scroll: usize, selected: usize, len: usize) -> usize {
    let mut s = scroll;
    if selected < s {
        s = selected;
    } else if selected >= s + MAX_VISIBLE {
        s = selected + 1 - MAX_VISIBLE;
    }
    s.min(len.saturating_sub(MAX_VISIBLE))
}

/// The largest size of an `iw`×`ih` image that fits `bw`×`bh` without upscaling.
pub fn fit(iw: u32, ih: u32, bw: f32, bh: f32) -> (f32, f32) {
    if iw == 0 || ih == 0 {
        return (bw, bh);
    }
    let scale = (bw / iw as f32).min(bh / ih as f32).min(1.0);
    ((iw as f32 * scale).max(1.0), (ih as f32 * scale).max(1.0))
}

#[cfg(test)]
mod tests {
    use ashd_svc_clipboard::{Colour, ImageInfo};

    use super::*;

    fn entry(id: u64, kind: Kind, preview: &str) -> Entry {
        Entry {
            id,
            kind,
            mime: if kind == Kind::Image { "image/png".into() } else { "text/plain".into() },
            alt_mimes: vec![],
            preview: preview.into(),
            truncated: false,
            lines: preview.lines().count().max(1) as u32,
            size: preview.len() as u64,
            copied_at: 0,
            pinned: false,
            colour: (kind == Kind::Colour).then_some(Colour { r: 1, g: 2, b: 3, a: 255 }),
            image: (kind == Kind::Image).then(|| ImageInfo { width: 1920, height: 1080, path: "/x.png".into(), thumb: None }),
        }
    }

    fn sample() -> Vec<Entry> {
        let mut pinned = entry(2, Kind::Text, "preset-column-widths");
        pinned.pinned = true;
        let mut img = entry(4, Kind::Image, "");
        img.size = 412 * 1024;
        vec![entry(1, Kind::Text, "niri msg action focus-column-right"), pinned, entry(3, Kind::Link, "https://github.com/YaLTeR/niri"), img]
    }

    #[test]
    fn filters_and_counts() {
        let e = sample();
        assert_eq!(counts(&e), Counts { all: 4, text: 3, images: 1, pinned: 1 });
        let mut m = matcher();
        assert_eq!(visible(&e, Filter::All, "", &mut m), [0, 1, 2, 3]);
        assert_eq!(visible(&e, Filter::Text, "", &mut m), [0, 1, 2]);
        assert_eq!(visible(&e, Filter::Images, "", &mut m), [3]);
        assert_eq!(visible(&e, Filter::Pinned, "", &mut m), [1]);
        assert_eq!(Filter::All.next(), Filter::Text);
        assert_eq!(Filter::All.prev(), Filter::Pinned);
        assert_eq!(Filter::Pinned.next(), Filter::All);
        assert_eq!(Counts { all: 9, text: 3, images: 4, pinned: 2 }.of(Filter::Images), 4);
    }

    #[test]
    fn search_is_fuzzy_ranked_and_filtered() {
        let e = sample();
        let mut m = matcher();
        assert_eq!(visible(&e, Filter::All, "niri", &mut m).len(), 2);
        assert_eq!(visible(&e, Filter::All, "  ", &mut m), [0, 1, 2, 3]);
        assert_eq!(visible(&e, Filter::All, "pcw", &mut m), [1]);
        assert_eq!(visible(&e, Filter::All, "zzzz", &mut m), Vec::<usize>::new());
        // kind / mime words match images
        assert_eq!(visible(&e, Filter::All, "png", &mut m), [3]);
        // the filter applies before the query
        assert_eq!(visible(&e, Filter::Images, "niri", &mut m), Vec::<usize>::new());
        // exact-ish beats scattered
        let e = vec![entry(1, Kind::Text, "a long thing with n i r i spread"), entry(2, Kind::Text, "niri")];
        assert_eq!(visible(&e, Filter::All, "niri", &mut m)[0], 1);
    }

    #[test]
    fn labels() {
        assert_eq!(age_label(1_000_000, 1_000_000 - 30_000), "now");
        assert_eq!(age_label(1_000_000, 1_000_000 - 120_000), "2m");
        assert_eq!(age_label(10_000_000, 10_000_000 - 3_600_000 * 2), "2h");
        assert_eq!(age_label(1_000_000_000, 1_000_000_000 - 86_400_000 * 3), "3d");
        assert_eq!(age_label(10_000_000_000, 10_000_000_000 - 604_800_000 * 2), "2w");
        assert_eq!(age_label(5, 10), "now");
        assert_eq!(human_size(38), "38 B");
        assert_eq!(human_size(412 * 1024), "412 KB");
        assert_eq!(human_size(1_468_006), "1.4 MB");
    }

    #[test]
    fn meta_and_rows() {
        let e = sample();
        assert_eq!(meta_line(&e[3]), "image/png · 1920×1080 · 412 KB");
        assert_eq!(row_text(&e[3]), "image/png · 1920×1080 · 412 KB");
        assert_eq!(row_text(&e[0]), "niri msg action focus-column-right");
        assert_eq!(meta_line(&entry(9, Kind::Code, "a\nb\nc")), "text/plain · 5 B · 3 lines");
        assert_eq!(row_text(&entry(9, Kind::Text, "  \n  ")), "(blank)");
        assert_eq!(row_text(&entry(9, Kind::Text, "\n  two   words \nnext")), "two words");
    }

    #[test]
    fn quick_picks_and_scrolling() {
        assert_eq!(quick_pick("1"), Some(0));
        assert_eq!(quick_pick("9"), Some(8));
        assert_eq!(quick_pick("0"), None);
        assert_eq!(quick_pick("12"), None);
        assert_eq!(quick_pick("a"), None);
        assert_eq!(quick_pick(""), None);
        assert_eq!(scroll_into_view(0, 3, 50), 0);
        assert_eq!(scroll_into_view(0, 10, 50), 1);
        assert_eq!(scroll_into_view(20, 5, 50), 5);
        assert_eq!(scroll_into_view(40, 49, 50), 40);
        assert_eq!(scroll_into_view(9, 0, 5), 0);
    }

    #[test]
    fn fitting() {
        assert_eq!(fit(1920, 1080, 286.0, 160.0), (284.44446, 160.0));
        assert_eq!(fit(64, 32, 286.0, 160.0), (64.0, 32.0));
        assert_eq!(fit(0, 0, 286.0, 160.0), (286.0, 160.0));
        let (w, h) = fit(100, 4000, 286.0, 160.0);
        assert!((h - 160.0).abs() < 0.01 && w < 5.0);
    }
}
