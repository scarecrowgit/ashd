//! Text for the numbers the UI shows (mono, as on the mockups).

/// `4d 02h`, `2h 07m`, `12m`, `41s`.
pub fn uptime(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3_600, secs % 3_600 / 60);
    if d > 0 {
        format!("{d}d {h:02}h")
    } else if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{secs}s")
    }
}

fn scaled(v: f64, units: &[&str]) -> String {
    let mut v = v;
    let mut i = 0;
    while v >= 999.95 && i + 1 < units.len() {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 { format!("{} {}", v.round() as u64, units[0]) } else { format!("{v:.1} {}", units[i]) }
}

/// Throughput: `84.2 MB/s`, `3.1 kB/s`, `0 B/s` (decimal prefixes, bytes).
pub fn rate(bytes_per_sec: f32) -> String {
    scaled(bytes_per_sec.max(0.0) as f64, &["B/s", "kB/s", "MB/s", "GB/s"])
}

/// Volume moved: `41.6 G`, `812 M`, `3.1 K`, `0 B`.
pub fn bytes(n: u64) -> String {
    scaled(n as f64, &["B", "K", "M", "G", "T"])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_text() {
        assert_eq!(uptime(4 * 86_400 + 2 * 3_600 + 5), "4d 02h");
        assert_eq!(uptime(2 * 3_600 + 7 * 60), "2h 07m");
        assert_eq!(uptime(12 * 60 + 3), "12m");
        assert_eq!(uptime(41), "41s");
    }

    #[test]
    fn rate_and_bytes_text() {
        assert_eq!(rate(84_200_000.0), "84.2 MB/s");
        assert_eq!(rate(3_100.0), "3.1 kB/s");
        assert_eq!(rate(0.0), "0 B/s");
        assert_eq!(rate(999.0), "999 B/s");
        assert_eq!(bytes(41_600_000_000), "41.6 G");
        assert_eq!(bytes(812_000_000), "812.0 M");
        assert_eq!(bytes(0), "0 B");
    }
}
