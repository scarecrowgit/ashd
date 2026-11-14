//! Pure text/geometry helpers of the media card (unit-tested; no GPUI).

use std::time::Duration;

use ashd_svc_mpris::{Player, Track};

/// `m:ss`, or `h:mm:ss` from an hour on.
pub fn clock(d: Duration) -> String {
    let s = d.as_secs();
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// Track title, else the file name of its URL, else a placeholder.
pub fn title(track: &Track) -> String {
    if !track.title.is_empty() {
        return track.title.clone();
    }
    track.url.as_deref().and_then(file_name).unwrap_or_else(|| "Unknown track".to_string())
}

/// Last path segment of a URL, percent-decoded (`file:///m/a%20b.flac` → `a b.flac`).
fn file_name(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next()?;
    let last = path.trim_end_matches('/').rsplit('/').next().filter(|s| !s.is_empty())?;
    let bytes = last.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if let Some(byte) = last.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok()) => {
                out.push(byte);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok().filter(|s| !s.is_empty())
}

/// `artist · album`; whichever exists; else the player's name (the "source").
pub fn subtitle(p: &Player) -> String {
    let parts: Vec<String> = [p.track.artist_line(), p.track.album.clone()].into_iter().filter(|s| !s.is_empty()).collect();
    if parts.is_empty() { p.display_name().to_string() } else { parts.join(" · ") }
}

/// Short, lower-case labels for the player chips: the desktop entry (`firefox`), else the
/// bus name without the MPRIS prefix and any `.instance…` suffix. Duplicates get ` 2`, ` 3`…
pub fn chip_labels(players: &[Player]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    players
        .iter()
        .map(|p| {
            let base = p
                .desktop_entry
                .clone()
                .unwrap_or_else(|| p.short_name().split(".instance").next().unwrap_or_default().to_string())
                .to_lowercase();
            let base = if base.is_empty() { "player".to_string() } else { base };
            let n = seen.iter().filter(|s| **s == base).count() + 1;
            seen.push(base.clone());
            if n == 1 { base } else { format!("{base} {n}") }
        })
        .collect()
}

/// The rows of the queue to show: up to `max`, starting at the current track (the mockup's
/// "up next" list); from the top when the current track is not in the queue. Returns
/// `(first index, count)`.
pub fn queue_window(queue_len: usize, current: Option<usize>, max: usize) -> (usize, usize) {
    let start = current.unwrap_or(0).min(queue_len.saturating_sub(1));
    // Near the end of a long queue: pull the window back so it stays full.
    let start = if queue_len > max { start.min(queue_len - max) } else { 0 };
    (start, queue_len.min(max))
}

/// Pointer x over a bar → 0..=1.
pub fn fraction_at(x: f32, left: f32, width: f32) -> f32 {
    if width <= 0.0 { 0.0 } else { ((x - left) / width).clamp(0.0, 1.0) }
}

/// `01`…`99`, then plain numbers.
pub fn index_label(i: usize) -> String {
    format!("{:02}", i + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(entry: Option<&str>, bus: &str) -> Player {
        Player {
            bus_name: format!("org.mpris.MediaPlayer2.{bus}"),
            desktop_entry: entry.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn clock_formats() {
        assert_eq!(clock(Duration::from_secs(0)), "0:00");
        assert_eq!(clock(Duration::from_secs(134)), "2:14");
        assert_eq!(clock(Duration::from_secs(400)), "6:40");
        assert_eq!(clock(Duration::from_secs(3599)), "59:59");
        assert_eq!(clock(Duration::from_secs(3600)), "1:00:00");
        assert_eq!(clock(Duration::from_secs(2710)), "45:10");
        assert_eq!(clock(Duration::from_millis(1999)), "0:01");
    }

    #[test]
    fn titles_fall_back_to_the_file_name() {
        let mut t = Track { title: "Song".into(), ..Default::default() };
        assert_eq!(title(&t), "Song");
        t.title.clear();
        t.url = Some("file:///music/a%20b.flac?x=1".into());
        assert_eq!(title(&t), "a b.flac");
        t.url = None;
        assert_eq!(title(&t), "Unknown track");
    }

    #[test]
    fn subtitles() {
        let mut p = player(None, "mpd");
        p.identity = "Music Player Daemon".into();
        assert_eq!(subtitle(&p), "Music Player Daemon");
        p.track.artists = vec!["A".into(), "B".into()];
        assert_eq!(subtitle(&p), "A, B");
        p.track.album = "Album".into();
        assert_eq!(subtitle(&p), "A, B · Album");
        p.track.artists.clear();
        assert_eq!(subtitle(&p), "Album");
    }

    #[test]
    fn chips_are_short_unique_and_lowercase() {
        let ps = [
            player(Some("Firefox"), "firefox.instance_1_2"),
            player(None, "mpd"),
            player(None, "vlc.instance77"),
            player(Some("firefox"), "firefox.instance_9_9"),
            player(None, "vlc.instance88"),
        ];
        assert_eq!(chip_labels(&ps), ["firefox", "mpd", "vlc", "firefox 2", "vlc 2"]);
    }

    #[test]
    fn queue_windows() {
        assert_eq!(queue_window(0, None, 5), (0, 0));
        assert_eq!(queue_window(3, Some(1), 5), (0, 3));
        assert_eq!(queue_window(6, Some(2), 5), (1, 5), "pulled back so the window stays full");
        assert_eq!(queue_window(20, Some(4), 5), (4, 5));
        assert_eq!(queue_window(20, Some(19), 5), (15, 5));
        assert_eq!(queue_window(20, None, 5), (0, 5));
        assert_eq!(queue_window(20, Some(99), 5), (15, 5));
    }

    #[test]
    fn bar_fraction() {
        assert_eq!(fraction_at(50.0, 0.0, 100.0), 0.5);
        assert_eq!(fraction_at(-5.0, 0.0, 100.0), 0.0);
        assert_eq!(fraction_at(500.0, 100.0, 100.0), 1.0);
        assert_eq!(fraction_at(10.0, 0.0, 0.0), 0.0);
        assert_eq!(index_label(2), "03");
        assert_eq!(index_label(120), "121");
    }
}
