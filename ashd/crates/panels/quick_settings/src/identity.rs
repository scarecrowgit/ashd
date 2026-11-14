//! Who is logged in where, and for how long: the footer / session-menu header strings.

use std::time::Duration;

/// `user@host`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub user: String,
    pub host: String,
}

impl Identity {
    /// Environment + `uname(2)`: cheap and non-blocking (no file access).
    pub fn current() -> Self {
        let user = ["USER", "LOGNAME"]
            .iter()
            .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
            .unwrap_or_else(|| "user".into());
        let host = rustix::system::uname().nodename().to_string_lossy().into_owned();
        Self { user, host: if host.is_empty() { "localhost".into() } else { host } }
    }

    pub fn label(&self) -> String {
        format!("{}@{}", self.user, self.host)
    }

    /// The avatar letter.
    pub fn initial(&self) -> String {
        self.user.chars().next().map(|c| c.to_lowercase().collect()).unwrap_or_else(|| "u".into())
    }
}

/// `up 4d 02h` / `up 3h 05m` / `up 12m`.
pub fn format_uptime(up: Duration) -> String {
    let s = up.as_secs();
    let (d, h, m) = (s / 86_400, s / 3_600 % 24, s / 60 % 60);
    if d > 0 {
        format!("up {d}d {h:02}h")
    } else if h > 0 {
        format!("up {h}h {m:02}m")
    } else {
        format!("up {m}m")
    }
}

/// Seconds since boot from `/proc/uptime` (blocking read — call it off the main thread).
pub fn read_proc_uptime() -> Option<Duration> {
    let text = std::fs::read_to_string("/proc/uptime").ok()?;
    text.split_whitespace().next()?.parse::<f64>().ok().map(Duration::from_secs_f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_formats() {
        assert_eq!(format_uptime(Duration::from_secs(4 * 86_400 + 2 * 3_600 + 59)), "up 4d 02h");
        assert_eq!(format_uptime(Duration::from_secs(3 * 3_600 + 5 * 60)), "up 3h 05m");
        assert_eq!(format_uptime(Duration::from_secs(12 * 60 + 40)), "up 12m");
        assert_eq!(format_uptime(Duration::ZERO), "up 0m");
    }

    #[test]
    fn identity_label() {
        let id = Identity { user: "Ivan".into(), host: "forge".into() };
        assert_eq!(id.label(), "Ivan@forge");
        assert_eq!(id.initial(), "i");
    }
}
