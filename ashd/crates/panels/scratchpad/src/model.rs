//! Pure panel logic: the three lists (hidden windows, images, clipboard), the instant
//! search, the item order used by the keyboard, and the scratch window ops against a niri
//! snapshot. No GPUI beyond the `Icon` enum.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use ashd_ipc::ScratchOp;
use ashd_niri::{
    NiriState,
    niri_ipc::{Action, Window},
};
use ashd_shell_core::apps::{app_for_app_id, app_glyph};
use ashd_svc_apps::App;
use ashd_svc_clipboard::{Entry, Kind};
use ashd_ui::Icon;

use crate::images::ImageFile;

/// Images shown (clipboard images and screenshots together).
pub const MAX_IMAGES: usize = 12;
/// Clipboard rows rendered (the search still looks through the whole history).
pub const MAX_CLIPS: usize = 40;

/// Shown when the compositor has no hidden workspace (stock niri, or the config lacks it).
pub const NO_HIDDEN_WORKSPACE: &str = "the compositor has no hidden workspace: add `workspace \"scratch\" { hidden }` \
     to the niri config (needs the ashd niri fork)";

// ── items ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ImageKey {
    /// A clipboard history entry.
    Clip(u64),
    /// A screenshot file.
    File(PathBuf),
}

/// One selectable thing in the panel.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ItemKey {
    Window(u64),
    Image(ImageKey),
    Clip(u64),
}

// ── hidden windows ────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct HiddenWindow {
    pub id: u64,
    pub title: String,
    pub app_id: String,
    pub app_name: Option<String>,
    pub icon_path: Option<PathBuf>,
    pub glyph: Icon,
    /// `02 code` — the workspace the window was hidden from, when it still exists.
    pub origin: Option<String>,
    /// Since when it has been hidden, if the panel saw it happen.
    pub age: Option<Duration>,
}

/// Ids of the hidden workspaces.
pub fn hidden_workspace_ids(niri: &NiriState) -> HashSet<u64> {
    niri.workspaces.values().filter(|w| w.is_hidden).map(|w| w.id).collect()
}

/// Windows on a hidden workspace.
pub fn hidden_window_ids(niri: &NiriState) -> HashSet<u64> {
    let hidden = hidden_workspace_ids(niri);
    niri.windows.values().filter(|w| w.workspace_id.is_some_and(|id| hidden.contains(&id))).map(|w| w.id).collect()
}

/// `02 code` / `02` for a workspace (the rail's naming).
pub fn workspace_label(idx: u8, name: Option<&str>) -> String {
    match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => format!("{idx:02} {n}"),
        None => format!("{idx:02}"),
    }
}

/// Every window on a hidden workspace, most recently hidden first (windows whose hiding
/// the panel did not see, e.g. at startup, follow, most recently focused first).
pub fn hidden_windows(niri: &NiriState, apps: &[App], since: &HashMap<u64, Instant>, now: Instant) -> Vec<HiddenWindow> {
    let ids = hidden_window_ids(niri);
    let mut windows: Vec<&Window> = niri.windows.values().filter(|w| ids.contains(&w.id)).collect();
    windows.sort_by(|a, b| {
        let (ta, tb) = (since.get(&a.id), since.get(&b.id));
        tb.cmp(&ta)
            .then_with(|| {
                let key = |w: &Window| w.focus_timestamp.as_ref().map(|t| (t.secs, t.nanos));
                key(b).cmp(&key(a))
            })
            .then(a.id.cmp(&b.id))
    });
    windows
        .into_iter()
        .map(|w| {
            let app_id = w.app_id.clone().unwrap_or_default();
            let app = app_for_app_id(apps, &app_id);
            let origin = w
                .hidden_from_workspace_id
                .and_then(|id| niri.desk(id).or_else(|| niri.workspaces.get(&id)))
                .map(|ws| {
                    let n = niri.visible_number(ws.id).map_or(ws.idx.max(1), |n| n.min(u8::MAX as usize) as u8);
                    workspace_label(n, ws.name.as_deref())
                });
            HiddenWindow {
                id: w.id,
                title: w.title.clone().filter(|t| !t.is_empty()).unwrap_or_else(|| app_id.clone()),
                app_name: app.map(|a| a.name.clone()),
                icon_path: app.and_then(|a| a.icon_path.clone()),
                glyph: app_glyph(app, &app_id),
                app_id,
                origin,
                age: since.get(&w.id).map(|t| now.saturating_duration_since(*t)),
            }
        })
        .collect()
}

/// Keep `since` in step with the windows that are hidden now: new ones get `now` (unless
/// this is the first snapshot, when their age is unknown), gone ones are dropped.
pub fn track_hidden(since: &mut HashMap<u64, Instant>, hidden: &HashSet<u64>, first_snapshot: bool, now: Instant) {
    since.retain(|id, _| hidden.contains(id));
    if !first_snapshot {
        for id in hidden {
            since.entry(*id).or_insert(now);
        }
    }
}

// ── scratch window ops ────────────────────────────────────────────────────────

/// The niri action for `ashd scratch hide|show [--id N]`, or the reason it cannot be done.
/// `fallback_focus`: the window to hide when niri reports no focused one (the panel itself
/// has the keyboard).
pub fn plan_scratch_window(op: ScratchOp, niri: &NiriState, fallback_focus: Option<u64>) -> Result<Action, String> {
    if !niri.connected {
        return Err("niri is not connected".into());
    }
    if !niri.workspaces.values().any(|w| w.is_hidden) {
        return Err(NO_HIDDEN_WORKSPACE.into());
    }
    let hidden = hidden_window_ids(niri);
    match op {
        ScratchOp::Hide | ScratchOp::HideWindow(_) => {
            let id = match op {
                ScratchOp::HideWindow(id) => {
                    if !niri.windows.contains_key(&id) {
                        return Err(format!("no window with id {id}"));
                    }
                    id
                }
                _ => niri
                    .focused_window()
                    .map(|w| w.id)
                    .or_else(|| fallback_focus.filter(|id| niri.windows.contains_key(id)))
                    .ok_or("no focused window to hide")?,
            };
            if hidden.contains(&id) {
                return Err(format!("window {id} is already hidden"));
            }
            Ok(Action::HideWindow { id: Some(id) })
        }
        ScratchOp::Show => {
            if hidden.is_empty() {
                Err("no hidden windows".into())
            } else {
                // niri's default: the most recently hidden one.
                Ok(Action::UnhideWindow { id: None })
            }
        }
        ScratchOp::ShowWindow(id) => {
            if hidden.contains(&id) {
                Ok(Action::UnhideWindow { id: Some(id) })
            } else {
                Err(format!("window {id} is not hidden"))
            }
        }
        ScratchOp::Toggle | ScratchOp::Open | ScratchOp::Close => Err("not a window operation".into()),
    }
}

// ── images ────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct ImageItem {
    pub key: ImageKey,
    pub name: String,
    /// The original (preview, open).
    pub path: PathBuf,
    /// What the 96×60 grid cell shows.
    pub thumb: PathBuf,
    pub dims: Option<(u32, u32)>,
    pub bytes: u64,
    /// Unix milliseconds (copy time / file mtime).
    pub at_ms: u64,
}

/// Clipboard images and screenshots, newest first, at most `cap`.
pub fn merge_images(clips: &[Entry], files: &[ImageFile], cap: usize) -> Vec<ImageItem> {
    let mut out: Vec<ImageItem> = clips
        .iter()
        .filter(|e| e.kind == Kind::Image)
        .filter_map(|e| {
            let info = e.image.as_ref()?;
            Some(ImageItem {
                key: ImageKey::Clip(e.id),
                name: format!("clip {}", clock_label(e.copied_at)),
                path: info.path.clone(),
                thumb: info.thumb.clone().unwrap_or_else(|| info.path.clone()),
                dims: (info.width > 0).then_some((info.width, info.height)),
                bytes: e.size,
                at_ms: e.copied_at,
            })
        })
        .chain(files.iter().map(|f| ImageItem {
            key: ImageKey::File(f.path.clone()),
            name: f.name.clone(),
            path: f.path.clone(),
            thumb: f.thumb.clone().unwrap_or_else(|| f.path.clone()),
            dims: f.dims,
            bytes: f.bytes,
            at_ms: f.modified_ms,
        }))
        .collect();
    out.sort_by(|a, b| b.at_ms.cmp(&a.at_ms).then_with(|| a.name.cmp(&b.name)));
    out.truncate(cap);
    out
}

/// `1920×1080 · 412 KB` (either part may be missing).
pub fn image_meta(item: &ImageItem) -> String {
    let dims = item.dims.map(|(w, h)| format!("{w}×{h}"));
    let size = (item.bytes > 0).then(|| human_size(item.bytes));
    [dims, size].into_iter().flatten().collect::<Vec<_>>().join(" · ")
}

/// The MIME type `wl-copy --type` gets for an image file.
pub fn image_mime(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        _ => "image/png",
    }
}

// ── clipboard ─────────────────────────────────────────────────────────────────

/// The row text: first line, or a description for images.
pub fn clip_text(e: &Entry) -> String {
    match (e.kind, e.image.as_ref()) {
        (Kind::Image, Some(i)) if i.width > 0 => format!("{} · {}×{} · {}", e.mime, i.width, i.height, human_size(e.size)),
        (Kind::Image, _) => format!("{} · {}", e.mime, human_size(e.size)),
        (Kind::Colour, _) => e.colour.map(|c| c.hex()).unwrap_or_else(|| e.first_line()),
        _ => e.first_line(),
    }
}

/// `text · 34 chars` — the selected card's header.
pub fn clip_meta(e: &Entry) -> String {
    match e.kind {
        Kind::Image => match e.image.as_ref() {
            Some(i) if i.width > 0 => format!("image · {}×{}", i.width, i.height),
            _ => "image".to_string(),
        },
        k => {
            let chars = if e.truncated { e.size as usize } else { e.preview.chars().count() };
            let lines = if e.lines > 1 { format!(" · {} lines", e.lines) } else { String::new() };
            format!("{} · {chars} chars{lines}", k.label())
        }
    }
}

/// Could this entry be a command line worth offering "Run" for? Conservative on purpose:
/// one short line of plain text whose first word looks like a program and which has
/// arguments (or a path-like first word).
pub fn looks_like_command(e: &Entry) -> bool {
    if !matches!(e.kind, Kind::Text | Kind::Code) || e.truncated || e.lines > 1 || e.size > 400 {
        return false;
    }
    let line = e.preview.trim();
    if line.is_empty() || line.contains('\n') || line.starts_with(['#', '-', '=', '"', '\'']) {
        return false;
    }
    let mut words = line.split_whitespace();
    let Some(first) = words.next() else { return false };
    let args: Vec<&str> = words.collect();
    let pathlike = first.starts_with("./") || first.starts_with("~/") || first.starts_with('/');
    if pathlike {
        // A bare path is not a command; a path followed by arguments is.
        return !args.is_empty();
    }
    let word = !first.is_empty()
        && first.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && first.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+'));
    if !word || args.is_empty() {
        return false;
    }
    const KNOWN: &[&str] = &[
        "niri", "ashd", "git", "sudo", "nix", "nix-shell", "nixos-rebuild", "cargo", "systemctl", "journalctl", "ls", "cd",
        "cat", "grep", "rg", "fd", "curl", "wget", "wl-copy", "wl-paste", "grim", "slurp", "ssh", "scp", "rsync", "make",
        "npm", "pnpm", "yarn", "python", "python3", "pip", "docker", "podman", "kubectl", "pacman", "apt", "dnf", "brew",
        "tar", "unzip", "mv", "cp", "rm", "mkdir", "touch", "chmod", "chown", "echo", "printf", "kill", "pkill", "killall",
        "xdg-open", "notify-send", "playerctl", "wpctl", "pactl", "brightnessctl", "nmcli", "bluetoothctl", "kitty",
        "foot", "nvim", "vim", "ffmpeg", "mpv", "jq", "sed", "awk", "find", "xargs", "tmux", "env", "export", "go", "node",
    ];
    let flaggy = args.iter().any(|a| a.starts_with('-'));
    let shelly = line.contains(" | ") || line.contains(" && ") || line.contains(" > ") || line.contains(" ; ");
    KNOWN.contains(&first) || flaggy || shelly
}

/// `now` / `5m` / `3h` / `2d` / `6w`.
pub fn age_label(now_ms: u64, then_ms: u64) -> String {
    age_secs(now_ms.saturating_sub(then_ms) / 1000)
}

pub fn age_secs(s: u64) -> String {
    match s {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h", s / 3600),
        86_400..=604_799 => format!("{}d", s / 86_400),
        _ => format!("{}w", s / 604_800),
    }
}

/// `4 KB`-style sizes.
pub fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b >= KB * KB {
        format!("{:.1} MB", b / (KB * KB))
    } else if b >= KB {
        format!("{} KB", (b / KB).round() as u64)
    } else {
        format!("{bytes} B")
    }
}

/// `14:51` in local time.
pub fn clock_label(ms: u64) -> String {
    use chrono::{Local, TimeZone as _};
    Local.timestamp_millis_opt(ms as i64).single().map(|t| t.format("%H:%M").to_string()).unwrap_or_default()
}

// ── search ────────────────────────────────────────────────────────────────────

/// Every whitespace-separated term of `query` occurs in `haystack`, ignoring case. An
/// empty query matches everything. Instant: no scoring, no animation.
pub fn matches(query: &str, haystack: &str) -> bool {
    let hay = haystack.to_lowercase();
    query.split_whitespace().all(|term| hay.contains(&term.to_lowercase()))
}

fn window_haystack(w: &HiddenWindow) -> String {
    format!("{} {} {} {}", w.title, w.app_id, w.app_name.as_deref().unwrap_or(""), w.origin.as_deref().unwrap_or(""))
}

fn image_haystack(i: &ImageItem) -> String {
    format!("{} image {}", i.name, image_meta(i))
}

fn clip_haystack(e: &Entry) -> String {
    let head: String = e.preview.chars().take(1024).collect();
    format!("{} {} {}", head, e.kind.label(), e.mime)
}

/// The three lists after the search (and, for the clipboard, the ordering).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lists {
    pub windows: Vec<HiddenWindow>,
    pub images: Vec<ImageItem>,
    pub clips: Vec<Entry>,
    /// Totals before the search.
    pub total_windows: usize,
    pub total_images: usize,
    pub total_clips: usize,
}

/// Filter all three lists by `query`; with `pinned_first` the clipboard puts pinned entries
/// first (stable: newest first inside each group).
pub fn filter(
    query: &str,
    windows: Vec<HiddenWindow>,
    images: Vec<ImageItem>,
    clips: &[Entry],
    pinned_first: bool,
) -> Lists {
    let (total_windows, total_images, total_clips) = (windows.len(), images.len(), clips.len());
    let windows = windows.into_iter().filter(|w| matches(query, &window_haystack(w))).collect();
    let images = images.into_iter().filter(|i| matches(query, &image_haystack(i))).collect();
    let mut kept: Vec<&Entry> = clips.iter().filter(|e| matches(query, &clip_haystack(e))).collect();
    if pinned_first {
        kept.sort_by_key(|e| !e.pinned);
    }
    Lists {
        windows,
        images,
        clips: kept.into_iter().take(MAX_CLIPS).cloned().collect(),
        total_windows,
        total_images,
        total_clips,
    }
}

// ── keyboard order ────────────────────────────────────────────────────────────

/// What the Tabs variant shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Clipboard,
    Images,
    Windows,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Clipboard, Tab::Images, Tab::Windows];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Clipboard => "Clipboard",
            Tab::Images => "Images",
            Tab::Windows => "Windows",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    pub fn next(self) -> Tab {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }
}

/// The items Up / Down walk through, in display order: Stack shows all three sections,
/// Tabs only the selected tab.
pub fn items(lists: &Lists, tab: Option<Tab>) -> Vec<ItemKey> {
    let mut out = Vec::new();
    if tab.is_none_or(|t| t == Tab::Windows) {
        out.extend(lists.windows.iter().map(|w| ItemKey::Window(w.id)));
    }
    if tab.is_none_or(|t| t == Tab::Images) {
        out.extend(lists.images.iter().map(|i| ItemKey::Image(i.key.clone())));
    }
    if tab.is_none_or(|t| t == Tab::Clipboard) {
        out.extend(lists.clips.iter().map(|e| ItemKey::Clip(e.id)));
    }
    out
}

/// Index of the selection in `items`: the chosen item if it is still there; otherwise the
/// first item when `default_first` (a search is running, or the Tabs card needs one).
pub fn selection(items: &[ItemKey], chosen: Option<&ItemKey>, default_first: bool) -> Option<usize> {
    chosen.and_then(|k| items.iter().position(|i| i == k)).or_else(|| (default_first && !items.is_empty()).then_some(0))
}

/// Move the selection by `delta` (wrapping); from no selection Down goes to the first item,
/// Up to the last.
pub fn step(len: usize, current: Option<usize>, delta: isize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match current {
        Some(c) => (c as isize + delta).rem_euclid(len as isize) as usize,
        None if delta >= 0 => 0,
        None => len - 1,
    })
}

#[cfg(test)]
mod tests {
    use ashd_niri::niri_ipc::Workspace;
    use ashd_svc_clipboard::ImageInfo;

    use super::*;

    fn ws(id: u64, idx: u8, name: Option<&str>, hidden: bool) -> Workspace {
        serde_json::from_value(serde_json::json!({
            "id": id, "idx": idx, "name": name, "output": "X", "is_urgent": false,
            "is_active": !hidden, "is_focused": id == 1, "active_window_id": null, "is_hidden": hidden
        }))
        .unwrap()
    }

    fn win(id: u64, workspace: u64, focused: bool, from: Option<u64>, title: &str, app: &str) -> Window {
        serde_json::from_value(serde_json::json!({
            "id": id, "title": title, "app_id": app, "pid": null, "workspace_id": workspace,
            "is_focused": focused, "is_floating": false, "is_urgent": false,
            "layout": {"pos_in_scrolling_layout": null, "tile_size": [1.0, 1.0], "window_size": [1, 1],
                       "tile_pos_in_workspace_view": null, "window_offset_in_tile": [0.0, 0.0]},
            "focus_timestamp": null, "hidden_from_workspace_id": from
        }))
        .unwrap()
    }

    fn niri(workspaces: Vec<Workspace>, windows: Vec<Window>) -> NiriState {
        NiriState {
            connected: true,
            workspaces: workspaces.into_iter().map(|w| (w.id, w)).collect(),
            windows: windows.into_iter().map(|w| (w.id, w)).collect(),
            ..NiriState::default()
        }
    }

    fn entry(id: u64, kind: Kind, preview: &str) -> Entry {
        Entry {
            id,
            kind,
            mime: "text/plain".into(),
            alt_mimes: Vec::new(),
            preview: preview.into(),
            truncated: false,
            lines: preview.lines().count().max(1) as u32,
            size: preview.len() as u64,
            copied_at: 1_000 * id,
            pinned: false,
            colour: None,
            image: None,
        }
    }

    fn p(op: ScratchOp, n: &NiriState, fb: Option<u64>) -> Result<String, String> {
        plan_scratch_window(op, n, fb).map(|a| serde_json::to_string(&a).unwrap())
    }

    #[test]
    fn hidden_windows_come_with_origin_and_age() {
        let n = niri(
            // the fork: hidden scratch idx 0, visible ones by visual index
            vec![ws(9, 0, Some("scratch"), true), ws(5, 1, Some("web"), false), ws(1, 2, Some("code"), false)],
            vec![
                win(1, 1, true, None, "main", "kitty"),
                win(2, 9, false, Some(1), "btop", "btop"),
                win(3, 9, false, None, "", "foot"),
                win(4, 9, false, Some(77), "gone", "x"),
            ],
        );
        let now = Instant::now();
        let mut since = HashMap::new();
        since.insert(2, now - Duration::from_secs(120));
        since.insert(4, now - Duration::from_secs(5));
        let list = hidden_windows(&n, &[], &since, now);
        // 4 was hidden last, then 2, then the unknown 3.
        assert_eq!(list.iter().map(|w| w.id).collect::<Vec<_>>(), vec![4, 2, 3]);
        assert_eq!(list[1].origin.as_deref(), Some("02 code"));
        assert_eq!(list[1].age, Some(Duration::from_secs(120)));
        assert_eq!(list[0].origin, None, "the origin workspace is gone");
        assert_eq!(list[2].title, "foot", "no title falls back to the app id");
        assert_eq!(list[2].age, None);
    }

    #[test]
    fn tracking_when_windows_were_hidden() {
        let now = Instant::now();
        let mut since = HashMap::new();
        track_hidden(&mut since, &HashSet::from([1, 2]), true, now);
        assert!(since.is_empty(), "windows already hidden at the first snapshot have an unknown age");
        track_hidden(&mut since, &HashSet::from([1, 2, 3]), false, now);
        assert_eq!(since.len(), 3);
        track_hidden(&mut since, &HashSet::from([3]), false, now + Duration::from_secs(1));
        assert_eq!(since.keys().copied().collect::<Vec<_>>(), vec![3]);
    }

    #[test]
    fn scratch_window_plans() {
        let n = niri(
            vec![ws(1, 1, None, false), ws(9, 0, Some("scratch"), true)],
            vec![win(1, 1, true, None, "a", "a"), win(2, 1, false, None, "b", "b"), win(3, 9, false, Some(1), "c", "c")],
        );
        assert_eq!(p(ScratchOp::Hide, &n, None), Ok(r#"{"HideWindow":{"id":1}}"#.to_string()));
        assert_eq!(p(ScratchOp::HideWindow(2), &n, None), Ok(r#"{"HideWindow":{"id":2}}"#.to_string()));
        assert_eq!(p(ScratchOp::Show, &n, None), Ok(r#"{"UnhideWindow":{"id":null}}"#.to_string()));
        assert_eq!(p(ScratchOp::ShowWindow(3), &n, None), Ok(r#"{"UnhideWindow":{"id":3}}"#.to_string()));
        assert!(p(ScratchOp::HideWindow(3), &n, None).unwrap_err().contains("already hidden"));
        assert!(p(ScratchOp::HideWindow(8), &n, None).unwrap_err().contains("no window"));
        assert!(p(ScratchOp::ShowWindow(2), &n, None).unwrap_err().contains("not hidden"));
    }

    #[test]
    fn scratch_window_errors() {
        // No hidden workspace: stock niri / missing config.
        let n = niri(vec![ws(1, 1, None, false)], vec![win(1, 1, true, None, "a", "a")]);
        for op in [ScratchOp::Hide, ScratchOp::Show, ScratchOp::HideWindow(1), ScratchOp::ShowWindow(1)] {
            assert_eq!(p(op, &n, None), Err(NO_HIDDEN_WORKSPACE.to_string()), "{op:?}");
        }
        // Nothing focused: the panel's remembered window is used if it still exists.
        let n = niri(vec![ws(1, 1, None, false), ws(9, 0, None, true)], vec![win(1, 1, false, None, "a", "a")]);
        assert_eq!(p(ScratchOp::Hide, &n, None), Err("no focused window to hide".into()));
        assert_eq!(p(ScratchOp::Hide, &n, Some(1)), Ok(r#"{"HideWindow":{"id":1}}"#.to_string()));
        assert_eq!(p(ScratchOp::Hide, &n, Some(55)), Err("no focused window to hide".into()));
        assert_eq!(p(ScratchOp::Show, &n, None), Err("no hidden windows".into()));
        let mut off = n.clone();
        off.connected = false;
        assert_eq!(p(ScratchOp::Show, &off, None), Err("niri is not connected".into()));
    }

    #[test]
    fn images_merge_newest_first_and_capped() {
        let mut clip = entry(5, Kind::Image, "");
        clip.mime = "image/png".into();
        clip.size = 412 * 1024;
        clip.image = Some(ImageInfo { width: 1920, height: 1080, path: "/c/5.png".into(), thumb: Some("/c/5.thumb.png".into()) });
        let files = vec![
            ImageFile { path: "/p/a.png".into(), name: "a.png".into(), modified_ms: 9_000, bytes: 10, thumb: None, dims: None },
            ImageFile { path: "/p/b.png".into(), name: "b.png".into(), modified_ms: 1_000, bytes: 10, thumb: Some("/t/b.png".into()), dims: Some((4, 3)) },
        ];
        let items = merge_images(&[clip.clone(), entry(6, Kind::Text, "x")], &files, 12);
        assert_eq!(items.iter().map(|i| i.name.as_str()).collect::<Vec<_>>().len(), 3);
        assert_eq!(items[0].name, "a.png");
        assert_eq!(items[1].key, ImageKey::Clip(5));
        assert_eq!(items[1].thumb, PathBuf::from("/c/5.thumb.png"));
        assert_eq!(items[0].thumb, PathBuf::from("/p/a.png"), "no thumbnail yet: the file itself");
        assert_eq!(image_meta(&items[1]), "1920×1080 · 412 KB");
        assert_eq!(merge_images(&[clip], &files, 1).len(), 1);
    }

    #[test]
    fn mime_by_extension() {
        assert_eq!(image_mime(Path::new("/a/b.PNG")), "image/png");
        assert_eq!(image_mime(Path::new("/a/b.jpeg")), "image/jpeg");
        assert_eq!(image_mime(Path::new("/a/b.webp")), "image/webp");
        assert_eq!(image_mime(Path::new("/a/b")), "image/png");
    }

    #[test]
    fn command_lines() {
        let yes = ["niri msg action focus-column-right", "ls -la ~/Pictures", "foo --bar baz", "cat a | grep b", "/usr/bin/x --y", "./configure --prefix=/usr"];
        let no = ["hello", "hello world", "https://example.com/a b", "#E0A458", "/home/me/file.txt", "~/dots/niri/config.kdl", "", "Niri msg action x"];
        for s in yes {
            assert!(looks_like_command(&entry(1, Kind::Text, s)), "{s:?} should be a command");
        }
        for s in no {
            assert!(!looks_like_command(&entry(1, Kind::Text, s)), "{s:?} should not be a command");
        }
        assert!(!looks_like_command(&entry(1, Kind::Link, "git status")));
        assert!(!looks_like_command(&entry(1, Kind::Text, "ls -l\nrm -rf x")), "multi-line text is never offered");
        let mut long = entry(1, Kind::Text, "ls -la");
        long.truncated = true;
        assert!(!looks_like_command(&long), "a truncated preview is not the whole command");
    }

    #[test]
    fn search_is_instant_and_forgiving() {
        assert!(matches("", "anything"));
        assert!(matches("BTOP", "x btop y"));
        assert!(matches("code btop", "from 02 code · btop"));
        assert!(!matches("code zzz", "from 02 code · btop"));
    }

    #[test]
    fn filter_orders_and_counts() {
        let mut pinned = entry(1, Kind::Text, "pinned note");
        pinned.pinned = true;
        let clips = vec![entry(3, Kind::Text, "newest"), entry(2, Kind::Link, "https://a.b"), pinned];
        let all = filter("", Vec::new(), Vec::new(), &clips, false);
        assert_eq!(all.clips.iter().map(|e| e.id).collect::<Vec<_>>(), vec![3, 2, 1]);
        let pf = filter("", Vec::new(), Vec::new(), &clips, true);
        assert_eq!(pf.clips.iter().map(|e| e.id).collect::<Vec<_>>(), vec![1, 3, 2]);
        let q = filter("link", Vec::new(), Vec::new(), &clips, false);
        assert_eq!(q.clips.iter().map(|e| e.id).collect::<Vec<_>>(), vec![2], "the kind is searchable");
        assert_eq!((q.total_clips, q.clips.len()), (3, 1));
    }

    #[test]
    fn keyboard_order_and_selection() {
        let lists = Lists {
            windows: vec![HiddenWindow {
                id: 4,
                title: "t".into(),
                app_id: "a".into(),
                app_name: None,
                icon_path: None,
                glyph: Icon::Grid,
                origin: None,
                age: None,
            }],
            images: vec![ImageItem {
                key: ImageKey::Clip(7),
                name: "n".into(),
                path: "/p".into(),
                thumb: "/p".into(),
                dims: None,
                bytes: 0,
                at_ms: 0,
            }],
            clips: vec![entry(1, Kind::Text, "x"), entry(2, Kind::Text, "y")],
            ..Lists::default()
        };
        let stack = items(&lists, None);
        assert_eq!(
            stack,
            vec![ItemKey::Window(4), ItemKey::Image(ImageKey::Clip(7)), ItemKey::Clip(1), ItemKey::Clip(2)]
        );
        assert_eq!(items(&lists, Some(Tab::Clipboard)), vec![ItemKey::Clip(1), ItemKey::Clip(2)]);
        assert_eq!(items(&lists, Some(Tab::Windows)), vec![ItemKey::Window(4)]);
        assert_eq!(selection(&stack, Some(&ItemKey::Clip(1)), false), Some(2));
        assert_eq!(selection(&stack, Some(&ItemKey::Clip(99)), false), None);
        assert_eq!(selection(&stack, None, true), Some(0));
        assert_eq!(selection(&[], None, true), None);
        assert_eq!(step(4, None, 1), Some(0));
        assert_eq!(step(4, None, -1), Some(3));
        assert_eq!(step(4, Some(3), 1), Some(0));
        assert_eq!(step(4, Some(0), -1), Some(3));
        assert_eq!(step(0, None, 1), None);
    }

    #[test]
    fn labels() {
        assert_eq!(workspace_label(2, Some("code")), "02 code");
        assert_eq!(workspace_label(11, None), "11");
        assert_eq!(workspace_label(1, Some("  ")), "01");
        assert_eq!(age_secs(30), "now");
        assert_eq!(age_secs(120), "2m");
        assert_eq!(age_secs(7200), "2h");
        assert_eq!(age_secs(200_000), "2d");
        assert_eq!(human_size(412 * 1024), "412 KB");
        assert_eq!(human_size(10), "10 B");
        assert_eq!(Tab::Windows.next(), Tab::Clipboard);
    }
}
