//! Grouping of the notification history by app, plus the small text helpers of the list.
//! No GPUI.

use chrono::{DateTime, Local};
use ashd_svc_notifications::Notification;
use ashd_ui::Icon;

/// The notifications of one app, newest first.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    /// The app name as the first (newest) notification spelled it.
    pub app: String,
    pub items: Vec<Notification>,
}

impl Group {
    pub fn newest_seq(&self) -> u64 {
        self.items.first().map_or(0, |n| n.seq)
    }

    /// Header text: the app name, or `unknown` when the app sent none.
    pub fn title(&self) -> &str {
        if self.app.trim().is_empty() { "unknown" } else { self.app.trim() }
    }
}

/// Group by app name (case-insensitive), groups ordered by their newest notification,
/// items newest first.
pub fn group_by_app(notifications: &[Notification]) -> Vec<Group> {
    let mut sorted: Vec<&Notification> = notifications.iter().collect();
    sorted.sort_by_key(|n| std::cmp::Reverse(n.seq));
    let mut groups: Vec<Group> = Vec::new();
    for n in sorted {
        let key = n.app_name.trim().to_lowercase();
        match groups.iter_mut().find(|g| g.app.trim().to_lowercase() == key) {
            Some(g) => g.items.push(n.clone()),
            None => groups.push(Group { app: n.app_name.clone(), items: vec![n.clone()] }),
        }
    }
    groups
}

/// `now`, `5m`, `3h`, `2d` (shown upper-case in the list).
pub fn age_label(received: DateTime<Local>, now: DateTime<Local>) -> String {
    let secs = (now - received).num_seconds().max(0);
    match secs {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// The 13px glyph of a group header, guessed from the app name / category / desktop entry.
pub fn group_icon(n: &Notification) -> Icon {
    let hay = format!(
        "{} {} {}",
        n.app_name.to_lowercase(),
        n.category.as_deref().unwrap_or("").to_lowercase(),
        n.desktop_entry.as_deref().unwrap_or("").to_lowercase()
    );
    let has = |words: &[&str]| words.iter().any(|w| hay.contains(w));
    if has(&["battery", "upower"]) {
        Icon::Battery
    } else if has(&["transfer", "download"]) {
        Icon::Download
    } else if has(&["network", "wifi", "networkmanager"]) {
        Icon::Wifi
    } else if has(&["volume", "audio", "sound"]) {
        Icon::Volume
    } else if has(&["screenshot", "screencast"]) {
        Icon::Camera
    } else if has(&["system", "update", "upgrade", "settings", "power", "nix", "pacman", "apt"]) {
        Icon::Sliders
    } else if n.app_name.eq_ignore_ascii_case("fm") || has(&["file", "nautilus", "thunar", "dolphin", "folder", "device", "storage", "udisks"]) {
        Icon::Folder
    } else if has(&["mail", "calendar", "reminder"]) {
        Icon::Clock
    } else {
        Icon::Chat
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use ashd_svc_notifications::Urgency;

    fn n(id: u32, seq: u64, app: &str) -> Notification {
        Notification {
            id,
            seq,
            app_name: app.into(),
            summary: format!("s{id}"),
            body: String::new(),
            actions: vec![],
            urgency: Urgency::Normal,
            image: None,
            category: None,
            desktop_entry: None,
            expire_timeout_ms: None,
            resident: false,
            transient: false,
            progress: None,
            reply_placeholder: None,
            expired: false,
            sync_tag: None,
            received: Local::now(),
        }
    }

    #[test]
    fn groups_by_app_newest_first() {
        // Oldest first, as the service delivers them.
        let list = vec![n(1, 1, "matrix"), n(2, 2, "fm"), n(3, 3, "Matrix"), n(4, 4, "system"), n(5, 5, "fm")];
        let groups = group_by_app(&list);
        let names: Vec<_> = groups.iter().map(|g| g.app.as_str()).collect();
        assert_eq!(names, ["fm", "system", "Matrix"], "groups ordered by their newest item");
        assert_eq!(groups[0].items.iter().map(|i| i.id).collect::<Vec<_>>(), [5, 2]);
        assert_eq!(groups[2].items.iter().map(|i| i.id).collect::<Vec<_>>(), [3, 1]);
        assert_eq!(groups[2].app, "Matrix", "spelled like the newest item");
        assert_eq!(groups[0].newest_seq(), 5);
    }

    #[test]
    fn replaced_notifications_float_to_the_top() {
        // A replace bumps `seq`: the entry id 1 is now the newest.
        let list = vec![n(2, 2, "a"), n(1, 3, "a")];
        let groups = group_by_app(&list);
        assert_eq!(groups[0].items.iter().map(|i| i.id).collect::<Vec<_>>(), [1, 2]);
    }

    #[test]
    fn empty_app_name_is_titled_unknown() {
        let groups = group_by_app(&[n(1, 1, "  ")]);
        assert_eq!(groups[0].title(), "unknown");
        assert!(group_by_app(&[]).is_empty());
    }

    #[test]
    fn ages() {
        let now = Local::now();
        assert_eq!(age_label(now, now), "now");
        assert_eq!(age_label(now - Duration::seconds(59), now), "now");
        assert_eq!(age_label(now - Duration::seconds(60), now), "1m");
        assert_eq!(age_label(now - Duration::minutes(18), now), "18m");
        assert_eq!(age_label(now - Duration::minutes(59), now), "59m");
        assert_eq!(age_label(now - Duration::hours(1), now), "1h");
        assert_eq!(age_label(now - Duration::hours(23), now), "23h");
        assert_eq!(age_label(now - Duration::hours(50), now), "2d");
        assert_eq!(age_label(now + Duration::minutes(5), now), "now", "clock skew never goes negative");
    }

    #[test]
    fn icons_follow_the_app() {
        assert_eq!(group_icon(&n(1, 1, "matrix")), Icon::Chat);
        assert_eq!(group_icon(&n(1, 1, "fm")), Icon::Folder);
        assert_eq!(group_icon(&n(1, 1, "system")), Icon::Sliders);
        assert_eq!(group_icon(&n(1, 1, "upower")), Icon::Battery);
        let mut x = n(1, 1, "x");
        x.category = Some("transfer.complete".into());
        assert_eq!(group_icon(&x), Icon::Download);
    }
}
