use std::collections::HashMap;

use niri_ipc::{
    Cast, CanvasViewState, Event, KeyboardLayouts, Output, StripViewState, Window, Workspace, WorkspaceMode,
    state::{
        CanvasState, CastsState, ConfigState, EventStreamStatePart, KeyboardLayoutsState, OverviewState,
        StripState, TouchpadState, WindowsState, WorkspacesState,
    },
};

use crate::{
    canvas::{self, CanvasWindow, PlaneRect},
    strip::{self, Strip},
};

/// Snapshot of the compositor as seen through the event stream. Cheap enough to clone
/// on every event (a few dozen windows).
#[derive(Clone, Debug, Default)]
pub struct NiriState {
    /// The event stream is connected. When false everything else is the last known state.
    pub connected: bool,
    /// By workspace id.
    pub workspaces: HashMap<u64, Workspace>,
    /// By window id.
    pub windows: HashMap<u64, Window>,
    pub keyboard_layouts: Option<KeyboardLayouts>,
    pub overview_open: bool,
    /// The last config (re)load failed.
    pub config_failed: bool,
    pub casts: HashMap<u64, Cast>,
    pub touchpad_on: bool,
    /// By output (connector) name, e.g. `DP-1`. Refreshed on connect and whenever
    /// workspaces change (outputs have no event of their own).
    pub outputs: HashMap<String, Output>,
    /// Path of the last screenshot niri saved (`Event::ScreenshotCaptured`).
    pub last_screenshot: Option<String>,
    /// View (pan/zoom, regions) of every canvas-mode workspace, by workspace id (ashd fork;
    /// empty on a stock niri).
    ///
    /// Kept current inside the ashd-niri thread, but the per-frame
    /// `CanvasViewChanged` events do **not** republish `NiriState` (the bar and rail would
    /// re-render every frame of a pan). In a published snapshot this is the state as of the
    /// last structural change; for live positions subscribe to
    /// [`NiriHandle::subscribe_views`](crate::NiriHandle::subscribe_views).
    pub canvas_views: HashMap<u64, CanvasViewState>,
    /// Scroll state of every strip-mode workspace, by workspace id (ashd fork; empty on a
    /// stock niri). Same publishing caveat as [`Self::canvas_views`].
    pub strip_views: HashMap<u64, StripViewState>,
}

impl NiriState {
    /// Apply one event using niri-ipc's reducers. Never panics: events that refer to
    /// workspaces / windows / casts we do not know (niri-ipc's own reducers `expect()` them)
    /// are ignored or trimmed — they happen when an event overtakes the one that announces
    /// its subject, see the niri-ipc docs.
    ///
    /// Returns `true` if the event may have changed the set of outputs (workspaces were
    /// replaced, or the config was (re)loaded): callers then refresh [`Self::outputs`],
    /// which have no event of their own.
    pub fn apply(&mut self, event: Event) -> bool {
        let Some(event) = self.sanitize(event) else { return false };
        let outputs_may_change = matches!(event, Event::WorkspacesChanged { .. } | Event::ConfigLoaded { .. });

        let mut ws = WorkspacesState { workspaces: std::mem::take(&mut self.workspaces) };
        let rest = ws.apply(event);
        self.workspaces = ws.workspaces;
        let Some(event) = rest else { return outputs_may_change };

        let mut win = WindowsState { windows: std::mem::take(&mut self.windows) };
        let rest = win.apply(event);
        self.windows = win.windows;
        let Some(event) = rest else { return outputs_may_change };

        let mut kb = KeyboardLayoutsState { keyboard_layouts: self.keyboard_layouts.take() };
        let rest = kb.apply(event);
        self.keyboard_layouts = kb.keyboard_layouts;
        let Some(event) = rest else { return outputs_may_change };

        let mut ov = OverviewState { is_open: self.overview_open };
        let rest = ov.apply(event);
        self.overview_open = ov.is_open;
        let Some(event) = rest else { return outputs_may_change };

        let mut cfg = ConfigState { failed: self.config_failed };
        let rest = cfg.apply(event);
        self.config_failed = cfg.failed;
        let Some(event) = rest else { return outputs_may_change };

        let mut casts = CastsState { casts: std::mem::take(&mut self.casts) };
        let rest = casts.apply(event);
        self.casts = casts.casts;
        let Some(event) = rest else { return outputs_may_change };

        let mut tp = TouchpadState { on: self.touchpad_on };
        let rest = tp.apply(event);
        self.touchpad_on = tp.on;
        let Some(event) = rest else { return outputs_may_change };

        let mut canvas = CanvasState { views: std::mem::take(&mut self.canvas_views) };
        let rest = canvas.apply(event);
        self.canvas_views = canvas.views;
        let Some(event) = rest else { return outputs_may_change };

        let mut strip = StripState { views: std::mem::take(&mut self.strip_views) };
        let rest = strip.apply(event);
        self.strip_views = strip.views;
        let Some(event) = rest else { return outputs_may_change };

        if let Event::ScreenshotCaptured { path } = event {
            self.last_screenshot = path;
        }
        outputs_may_change
    }

    /// Drop the parts of `event` that would make niri-ipc's reducers panic.
    fn sanitize(&self, event: Event) -> Option<Event> {
        let known = match &event {
            Event::WorkspaceActivated { id, .. } => self.workspaces.contains_key(id),
            Event::WorkspaceActiveWindowChanged { workspace_id, .. } => self.workspaces.contains_key(workspace_id),
            Event::WorkspaceModeChanged { id, .. } => self.workspaces.contains_key(id),
            Event::WindowClosed { id } => self.windows.contains_key(id),
            Event::KeyboardLayoutSwitched { .. } => self.keyboard_layouts.is_some(),
            Event::CastStopped { stream_id } => self.casts.contains_key(stream_id),
            Event::WindowLayoutsChanged { changes } => {
                let changes: Vec<_> =
                    changes.iter().filter(|(id, _)| self.windows.contains_key(id)).cloned().collect();
                return (!changes.is_empty()).then_some(Event::WindowLayoutsChanged { changes });
            }
            _ => true,
        };
        if !known {
            tracing::debug!("niri: ignoring event about an unknown subject: {event:?}");
        }
        known.then_some(event)
    }

    /// Visible workspaces of `output`, in on-screen order (`idx`). Hidden (scratchpad)
    /// workspaces are left out: they are never shown or navigated to; see
    /// [`Self::workspaces_on_including_hidden`].
    pub fn workspaces_on(&self, output: &str) -> Vec<&Workspace> {
        let mut v = self.workspaces_on_including_hidden(output);
        v.retain(|w| !w.is_hidden);
        v
    }

    /// Every workspace of `output`: the visible ones in `idx` order, then the hidden ones
    /// (the ashd fork gives hidden workspaces `idx` 0; older forks numbered them in line).
    pub fn workspaces_on_including_hidden(&self, output: &str) -> Vec<&Workspace> {
        let mut v: Vec<_> =
            self.workspaces.values().filter(|w| w.output.as_deref() == Some(output)).collect();
        v.sort_by_key(|w| (w.is_hidden, w.idx));
        v
    }

    /// All visible (non-hidden) workspaces sorted by (output, idx).
    pub fn workspaces_sorted(&self) -> Vec<&Workspace> {
        let mut v = self.workspaces_sorted_including_hidden();
        v.retain(|w| !w.is_hidden);
        v
    }

    /// All workspaces sorted by (output, hidden last, idx).
    pub fn workspaces_sorted_including_hidden(&self) -> Vec<&Workspace> {
        let mut v: Vec<_> = self.workspaces.values().collect();
        v.sort_by(|a, b| a.output.cmp(&b.output).then((a.is_hidden, a.idx).cmp(&(b.is_hidden, b.idx))));
        v
    }

    /// The number to show for workspace `id`: its 1-based position among the visible
    /// workspaces of its output — never 0. Computed from the sorted visible list so it is right
    /// with every niri: a stock one (no hidden workspaces), the current fork (`idx` is the
    /// visual index, 0 for hidden ones) and older forks that counted hidden ones in `idx`.
    /// `None` for hidden or unknown workspaces.
    pub fn visible_number(&self, id: u64) -> Option<usize> {
        let ws = self.workspaces.get(&id).filter(|w| !w.is_hidden)?;
        let mut v: Vec<_> =
            self.workspaces.values().filter(|w| w.output == ws.output && !w.is_hidden).map(|w| (w.idx, w.id)).collect();
        v.sort_unstable();
        v.iter().position(|(_, wid)| *wid == id).map(|p| p + 1)
    }

    /// Hidden (scratchpad) workspaces sorted by (output, idx).
    pub fn hidden_workspaces(&self) -> Vec<&Workspace> {
        let mut v = self.workspaces_sorted_including_hidden();
        v.retain(|w| w.is_hidden);
        v
    }

    /// Window arrangement mode of workspace `id` (`Strip` when unknown, and always on a
    /// stock niri).
    pub fn workspace_mode(&self, id: u64) -> WorkspaceMode {
        self.workspaces.get(&id).map(|w| w.mode).unwrap_or_default()
    }

    /// Workspace `id` is in canvas mode.
    pub fn is_canvas(&self, id: u64) -> bool {
        self.workspace_mode(id) == WorkspaceMode::Canvas
    }

    /// The view of canvas workspace `id` as of this snapshot (see the caveat on
    /// [`Self::canvas_views`]).
    pub fn canvas_view(&self, id: u64) -> Option<&CanvasViewState> {
        self.canvas_views.get(&id)
    }

    /// The scroll state of strip workspace `id` as of this snapshot.
    pub fn strip_view(&self, id: u64) -> Option<&StripViewState> {
        self.strip_views.get(&id)
    }

    /// Windows on the canvas plane of workspace `id` (`pos_in_canvas` set), sorted by id.
    /// Fullscreen/maximized windows of a canvas leave the plane and are not listed. On a
    /// workspace that is not focused the active window counts as focused.
    pub fn canvas_windows(&self, id: u64) -> Vec<CanvasWindow> {
        let active = self.workspaces.get(&id).and_then(|w| w.active_window_id);
        canvas::canvas_windows(self.windows.values().filter(|w| w.workspace_id == Some(id)), active)
    }

    /// Union of the plane rects of workspace `id`'s canvas windows and its regions (from
    /// this snapshot's view). `None` when there is nothing on the plane.
    pub fn canvas_bounds(&self, id: u64) -> Option<PlaneRect> {
        self.canvas_bounds_in(id, self.canvas_views.get(&id))
    }

    /// [`Self::canvas_bounds`] with the regions of an explicit (live) `view`.
    pub fn canvas_bounds_in(&self, id: u64, view: Option<&CanvasViewState>) -> Option<PlaneRect> {
        canvas::bounds(&self.canvas_windows(id), view.map(|v| v.regions.as_slice()).unwrap_or_default())
    }

    /// The globally focused workspace.
    pub fn focused_workspace(&self) -> Option<&Workspace> {
        self.workspaces.values().filter(|w| w.is_focused).min_by_key(|w| w.id)
    }

    /// The visible workspace of `output`.
    pub fn active_workspace(&self, output: &str) -> Option<&Workspace> {
        self.workspaces.values().filter(|w| w.is_active && w.output.as_deref() == Some(output)).min_by_key(|w| w.id)
    }

    /// Name of the focused output (output of the focused workspace).
    pub fn focused_output(&self) -> Option<&str> {
        self.focused_workspace().and_then(|w| w.output.as_deref())
    }

    pub fn focused_window(&self) -> Option<&Window> {
        self.windows.values().filter(|w| w.is_focused).min_by_key(|w| w.id)
    }

    /// Windows on workspace `id`, tiled first in strip order, then floating by id.
    pub fn windows_on(&self, workspace_id: u64) -> Vec<&Window> {
        let mut v: Vec<_> =
            self.windows.values().filter(|w| w.workspace_id == Some(workspace_id)).collect();
        v.sort_by_key(|w| (w.layout.pos_in_scrolling_layout.unwrap_or((usize::MAX, usize::MAX)), w.id));
        v
    }

    /// Windows whose app id is `app_id` (any workspace), sorted by id.
    pub fn windows_of_app(&self, app_id: &str) -> Vec<&Window> {
        let mut v: Vec<_> = self.windows.values().filter(|w| w.app_id.as_deref() == Some(app_id)).collect();
        v.sort_by_key(|w| w.id);
        v
    }

    /// Scrolling-strip columns of workspace `id`.
    ///
    /// On a workspace that is not focused no window is focused; the column of the
    /// workspace's active window is then reported as focused instead.
    pub fn strip(&self, workspace_id: u64) -> Strip {
        let active = self.workspaces.get(&workspace_id).and_then(|w| w.active_window_id);
        strip::strip_with_active(self.windows.values().filter(|w| w.workspace_id == Some(workspace_id)), active)
    }

    /// Current keyboard layout name (`English (US)`), if known.
    pub fn current_layout(&self) -> Option<&str> {
        let kb = self.keyboard_layouts.as_ref()?;
        kb.names.get(kb.current_idx as usize).map(String::as_str)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use niri_ipc::{CastKind, CastTarget, Timestamp, Window, WindowLayout, Workspace};

    use super::*;

    pub fn ws(id: u64, idx: u8, output: &str, active: bool, focused: bool) -> Workspace {
        Workspace {
            id,
            idx,
            name: None,
            output: Some(output.into()),
            is_urgent: false,
            is_active: active,
            is_focused: focused,
            active_window_id: None,
            mode: Default::default(),
            is_hidden: false,
        }
    }

    pub fn win(id: u64, ws: u64, pos: Option<(usize, usize)>, w: f64, focused: bool) -> Window {
        Window {
            id,
            title: Some(format!("w{id}")),
            app_id: Some(if id.is_multiple_of(2) { "kitty".into() } else { "zen".into() }),
            pid: None,
            workspace_id: Some(ws),
            is_focused: focused,
            is_floating: pos.is_none(),
            is_urgent: false,
            layout: WindowLayout {
                pos_in_scrolling_layout: pos,
                tile_size: (w, 1000.0),
                window_size: (w as i32, 1000),
                tile_pos_in_workspace_view: None,
                window_offset_in_tile: (0.0, 0.0),
                pos_in_canvas: None,
                is_pinned: false,
            },
            focus_timestamp: None,
            hidden_from_workspace_id: None,
        }
    }

    fn cast(stream_id: u64) -> Cast {
        Cast {
            stream_id,
            session_id: 1,
            kind: CastKind::PipeWire,
            target: CastTarget::Nothing {},
            is_dynamic_target: false,
            is_active: true,
            pid: None,
            pw_node_id: None,
        }
    }

    /// Two outputs: DP-1 with workspaces 1 (active, focused) and 2, HDMI-A-1 with 3 (active).
    fn session() -> NiriState {
        let mut s = NiriState::default();
        s.apply(Event::WorkspacesChanged {
            workspaces: vec![
                ws(1, 1, "DP-1", true, true),
                ws(2, 2, "DP-1", false, false),
                ws(3, 1, "HDMI-A-1", true, false),
            ],
        });
        s.apply(Event::WindowsChanged {
            windows: vec![
                win(10, 1, Some((1, 1)), 600.0, true),
                win(11, 1, Some((2, 1)), 400.0, false),
                win(12, 2, Some((1, 1)), 500.0, false),
                win(13, 3, None, 300.0, false),
            ],
        });
        s
    }

    #[test]
    fn reducer() {
        let mut s = NiriState::default();
        assert!(s.apply(Event::WorkspacesChanged {
            workspaces: vec![ws(1, 1, "DP-1", true, true), ws(2, 2, "DP-1", false, false), ws(3, 1, "HDMI-A-1", true, false)],
        }));
        s.apply(Event::WindowsChanged { windows: vec![win(10, 1, Some((1, 1)), 600.0, true), win(11, 2, Some((1, 1)), 400.0, false)] });
        assert_eq!(s.focused_window().map(|w| w.id), Some(10));
        assert_eq!(s.workspaces_on("DP-1").iter().map(|w| w.id).collect::<Vec<_>>(), vec![1, 2]);

        s.apply(Event::WorkspaceActivated { id: 2, focused: true });
        assert_eq!(s.focused_workspace().map(|w| w.id), Some(2));
        assert_eq!(s.active_workspace("DP-1").map(|w| w.id), Some(2));
        assert_eq!(s.active_workspace("HDMI-A-1").map(|w| w.id), Some(3));

        s.apply(Event::WindowFocusChanged { id: Some(11) });
        assert_eq!(s.focused_window().map(|w| w.id), Some(11));
        s.apply(Event::WindowClosed { id: 11 });
        assert!(s.focused_window().is_none());

        s.apply(Event::KeyboardLayoutsChanged {
            keyboard_layouts: KeyboardLayouts { names: vec!["English (US)".into(), "Russian".into()], current_idx: 0 },
        });
        s.apply(Event::KeyboardLayoutSwitched { idx: 1 });
        assert_eq!(s.current_layout(), Some("Russian"));
        s.apply(Event::OverviewOpenedOrClosed { is_open: true });
        assert!(s.overview_open);
        s.apply(Event::ScreenshotCaptured { path: Some("/tmp/a.png".into()) });
        assert_eq!(s.last_screenshot.as_deref(), Some("/tmp/a.png"));
    }

    #[test]
    fn workspace_events() {
        let mut s = session();
        assert_eq!(s.focused_output(), Some("DP-1"));
        assert_eq!(s.workspaces_sorted().iter().map(|w| w.id).collect::<Vec<_>>(), vec![1, 2, 3]);

        // Urgency.
        s.apply(Event::WorkspaceUrgencyChanged { id: 2, urgent: true });
        assert!(s.workspaces[&2].is_urgent && !s.workspaces[&1].is_urgent);
        s.apply(Event::WorkspaceUrgencyChanged { id: 2, urgent: false });
        assert!(!s.workspaces[&2].is_urgent);

        // Activating without focus only changes the output's active workspace.
        s.apply(Event::WorkspaceActivated { id: 2, focused: false });
        assert_eq!(s.active_workspace("DP-1").map(|w| w.id), Some(2));
        assert_eq!(s.focused_workspace().map(|w| w.id), Some(1), "focus stays");
        assert!(!s.workspaces[&1].is_active && s.workspaces[&3].is_active, "other output untouched");

        // Focusing a workspace on the other output moves the focused output.
        s.apply(Event::WorkspaceActivated { id: 3, focused: true });
        assert_eq!(s.focused_output(), Some("HDMI-A-1"));
        assert_eq!(s.workspaces.values().filter(|w| w.is_focused).count(), 1);

        // Active window.
        s.apply(Event::WorkspaceActiveWindowChanged { workspace_id: 1, active_window_id: Some(11) });
        assert_eq!(s.workspaces[&1].active_window_id, Some(11));
        s.apply(Event::WorkspaceActiveWindowChanged { workspace_id: 1, active_window_id: None });
        assert_eq!(s.workspaces[&1].active_window_id, None);

        // A workspace disappearing (output unplugged) is a full replacement.
        assert!(s.apply(Event::WorkspacesChanged { workspaces: vec![ws(1, 1, "DP-1", true, true)] }));
        assert_eq!(s.workspaces.len(), 1);
        assert!(s.workspaces_on("HDMI-A-1").is_empty());
        assert!(s.active_workspace("HDMI-A-1").is_none());
    }

    #[test]
    fn window_events() {
        let mut s = session();
        assert_eq!(s.windows_on(1).iter().map(|w| w.id).collect::<Vec<_>>(), vec![10, 11]);
        assert_eq!(s.windows_on(3).iter().map(|w| w.id).collect::<Vec<_>>(), vec![13]);
        assert_eq!(s.windows_of_app("kitty").iter().map(|w| w.id).collect::<Vec<_>>(), vec![10, 12]);
        assert_eq!(s.windows_of_app("zen").iter().map(|w| w.id).collect::<Vec<_>>(), vec![11, 13]);
        assert!(s.windows_of_app("nothing").is_empty());

        // A new focused window takes focus from the old one.
        s.apply(Event::WindowOpenedOrChanged { window: win(14, 1, Some((3, 1)), 300.0, true) });
        assert_eq!(s.windows.len(), 5);
        assert_eq!(s.focused_window().map(|w| w.id), Some(14));
        assert!(!s.windows[&10].is_focused);

        // An update of an existing window replaces it (title change).
        let mut changed = win(11, 1, Some((2, 1)), 400.0, false);
        changed.title = Some("renamed".into());
        s.apply(Event::WindowOpenedOrChanged { window: changed });
        assert_eq!(s.windows[&11].title.as_deref(), Some("renamed"));
        assert_eq!(s.focused_window().map(|w| w.id), Some(14), "unfocused update keeps focus");

        // Focus to nothing.
        s.apply(Event::WindowFocusChanged { id: None });
        assert!(s.focused_window().is_none());
        s.apply(Event::WindowFocusChanged { id: Some(12) });
        assert_eq!(s.focused_window().map(|w| w.id), Some(12));

        s.apply(Event::WindowFocusTimestampChanged { id: 12, focus_timestamp: Some(Timestamp { secs: 7, nanos: 9 }) });
        assert_eq!(s.windows[&12].focus_timestamp.map(|t| (t.secs, t.nanos)), Some((7, 9)));
        s.apply(Event::WindowUrgencyChanged { id: 13, urgent: true });
        assert!(s.windows[&13].is_urgent);

        // Layout changes: columns move.
        let mut layout = s.windows[&10].layout.clone();
        layout.pos_in_scrolling_layout = Some((2, 1));
        layout.tile_size = (700.0, 1000.0);
        let mut other = s.windows[&11].layout.clone();
        other.pos_in_scrolling_layout = Some((1, 1));
        s.apply(Event::WindowLayoutsChanged { changes: vec![(10, layout), (11, other)] });
        assert_eq!(s.windows_on(1).iter().map(|w| w.id).collect::<Vec<_>>(), vec![11, 10, 14]);
        assert_eq!(s.strip(1).columns[1].width, 700.0);

        // Closing.
        s.apply(Event::WindowClosed { id: 12 });
        assert!(!s.windows.contains_key(&12) && s.focused_window().is_none());
        s.apply(Event::WindowsChanged { windows: vec![] });
        assert!(s.windows.is_empty());
    }

    #[test]
    fn keyboard_overview_config_touchpad_casts_screenshot() {
        let mut s = NiriState::default();
        assert_eq!(s.current_layout(), None);
        s.apply(Event::KeyboardLayoutsChanged {
            keyboard_layouts: KeyboardLayouts { names: vec!["us".into(), "ru".into(), "de".into()], current_idx: 0 },
        });
        assert_eq!(s.current_layout(), Some("us"));
        s.apply(Event::KeyboardLayoutSwitched { idx: 2 });
        assert_eq!(s.current_layout(), Some("de"));
        s.apply(Event::KeyboardLayoutSwitched { idx: 9 });
        assert_eq!(s.current_layout(), None, "out-of-range index is not a panic");

        assert!(!s.overview_open);
        s.apply(Event::OverviewOpenedOrClosed { is_open: true });
        assert!(s.overview_open);
        s.apply(Event::OverviewOpenedOrClosed { is_open: false });
        assert!(!s.overview_open);

        assert!(s.apply(Event::ConfigLoaded { failed: true }), "a config load may change outputs");
        assert!(s.config_failed);
        s.apply(Event::ConfigLoaded { failed: false });
        assert!(!s.config_failed);

        assert!(!s.touchpad_on);
        s.apply(Event::TouchpadToggled { on: true });
        assert!(s.touchpad_on);

        s.apply(Event::CastsChanged { casts: vec![cast(1), cast(2)] });
        assert_eq!(s.casts.len(), 2);
        let mut c3 = cast(3);
        s.apply(Event::CastStartedOrChanged { cast: c3.clone() });
        c3.is_active = false;
        s.apply(Event::CastStartedOrChanged { cast: c3 });
        assert_eq!(s.casts.len(), 3);
        assert!(!s.casts[&3].is_active);
        s.apply(Event::CastStopped { stream_id: 1 });
        assert_eq!(s.casts.keys().copied().collect::<std::collections::BTreeSet<_>>(), [2, 3].into());

        s.apply(Event::ScreenshotCaptured { path: None });
        assert_eq!(s.last_screenshot, None);
        s.apply(Event::ScreenshotCaptured { path: Some("/x.png".into()) });
        s.apply(Event::ScreenshotCaptured { path: None });
        assert_eq!(s.last_screenshot, None, "clipboard-only screenshot clears the path");
    }

    #[test]
    fn only_workspace_and_config_events_ask_for_an_outputs_refresh() {
        let mut s = session();
        assert!(s.apply(Event::WorkspacesChanged { workspaces: vec![ws(1, 1, "DP-1", true, true)] }));
        for ev in [
            Event::WorkspaceActivated { id: 1, focused: true },
            Event::WindowFocusChanged { id: None },
            Event::OverviewOpenedOrClosed { is_open: true },
            Event::TouchpadToggled { on: true },
            Event::ScreenshotCaptured { path: None },
        ] {
            assert!(!s.apply(ev));
        }
    }

    /// niri-ipc's own reducers `expect()` their subjects; our `apply` must shrug instead.
    #[test]
    fn events_about_unknown_subjects_never_panic() {
        let mut empty = NiriState::default();
        let before = session();
        let mut s = before.clone();
        for state in [&mut empty, &mut s] {
            state.apply(Event::WorkspaceActivated { id: 999, focused: true });
            state.apply(Event::WorkspaceActiveWindowChanged { workspace_id: 999, active_window_id: Some(1) });
            state.apply(Event::WindowClosed { id: 999 });
            state.apply(Event::WindowLayoutsChanged { changes: vec![(999, win(1, 1, None, 1.0, false).layout)] });
            state.apply(Event::CastStopped { stream_id: 999 });
            state.apply(Event::WorkspaceUrgencyChanged { id: 999, urgent: true });
            state.apply(Event::WindowUrgencyChanged { id: 999, urgent: true });
            state.apply(Event::WindowFocusTimestampChanged { id: 999, focus_timestamp: None });
        }
        empty.apply(Event::KeyboardLayoutSwitched { idx: 1 });
        assert!(empty.keyboard_layouts.is_none());
        // A known layout change next to an unknown one is still applied.
        let mut layout = s.windows[&10].layout.clone();
        layout.tile_size = (123.0, 456.0);
        s.apply(Event::WindowLayoutsChanged {
            changes: vec![(999, layout.clone()), (10, layout), (998, win(1, 1, None, 1.0, false).layout)],
        });
        assert_eq!(s.windows[&10].layout.tile_size, (123.0, 456.0));
        // Nothing else moved.
        assert_eq!(s.workspaces, before.workspaces);
        assert_eq!(s.windows.len(), before.windows.len());
    }

    /// Replay of a recorded event stream in niri's actual JSON encoding. Guards both the
    /// vendored wire format and the reducer on a realistic sequence.
    #[test]
    fn recorded_stream_replay() {
        let lines = [
            r#"{"WorkspacesChanged":{"workspaces":[{"id":1,"idx":1,"name":null,"output":"winit","is_urgent":false,"is_active":true,"is_focused":true,"active_window_id":null},{"id":2,"idx":2,"name":"web","output":"winit","is_urgent":false,"is_active":false,"is_focused":false,"active_window_id":null}]}}"#,
            r#"{"WindowsChanged":{"windows":[]}}"#,
            r#"{"KeyboardLayoutsChanged":{"keyboard_layouts":{"names":["English (US)","Russian"],"current_idx":0}}}"#,
            r#"{"OverviewOpenedOrClosed":{"is_open":false}}"#,
            r#"{"ConfigLoaded":{"failed":false}}"#,
            r#"{"CastsChanged":{"casts":[]}}"#,
            r#"{"TouchpadToggled":{"on":true}}"#,
            r#"{"WindowOpenedOrChanged":{"window":{"id":1,"title":null,"app_id":null,"pid":4242,"workspace_id":1,"is_focused":false,"is_floating":false,"is_urgent":false,"layout":{"pos_in_scrolling_layout":[1,1],"tile_size":[960.0,1080.0],"window_size":[956,1076],"tile_pos_in_workspace_view":null,"window_offset_in_tile":[2.0,2.0]},"focus_timestamp":null}}}"#,
            r#"{"WorkspaceActiveWindowChanged":{"workspace_id":1,"active_window_id":1}}"#,
            r#"{"WindowOpenedOrChanged":{"window":{"id":1,"title":"~","app_id":"kitty","pid":4242,"workspace_id":1,"is_focused":true,"is_floating":false,"is_urgent":false,"layout":{"pos_in_scrolling_layout":[1,1],"tile_size":[960.0,1080.0],"window_size":[956,1076],"tile_pos_in_workspace_view":[0.0,0.0],"window_offset_in_tile":[2.0,2.0]},"focus_timestamp":{"secs":12,"nanos":500}}}}"#,
            r#"{"WindowFocusChanged":{"id":1}}"#,
            r#"{"WindowOpenedOrChanged":{"window":{"id":2,"title":"nvim","app_id":"kitty","pid":4243,"workspace_id":1,"is_focused":true,"is_floating":false,"is_urgent":false,"layout":{"pos_in_scrolling_layout":[2,1],"tile_size":[640.0,1080.0],"window_size":[636,1076],"tile_pos_in_workspace_view":[972.0,0.0],"window_offset_in_tile":[2.0,2.0]},"focus_timestamp":null}}}"#,
            r#"{"WindowLayoutsChanged":{"changes":[[1,{"pos_in_scrolling_layout":[1,1],"tile_size":[900.0,1080.0],"window_size":[896,1076],"tile_pos_in_workspace_view":[0.0,0.0],"window_offset_in_tile":[2.0,2.0]}]]}}"#,
            r#"{"SomethingNewFromANewerNiri":{"x":1}}"#,
            r#"{"WorkspaceActivated":{"id":2,"focused":true}}"#,
            r#"{"WindowClosed":{"id":2}}"#,
            r#"{"ScreenshotCaptured":{"path":"/home/u/Pictures/Screenshots/a.png"}}"#,
        ];
        let mut s = NiriState::default();
        let mut skipped = 0;
        for line in lines {
            match serde_json::from_str::<Event>(line) {
                Ok(ev) => {
                    s.apply(ev);
                }
                Err(_) => skipped += 1,
            }
        }
        assert_eq!(skipped, 1, "only the unknown event is skipped");
        assert_eq!(s.workspaces.len(), 2);
        assert_eq!(s.focused_workspace().and_then(|w| w.name.as_deref()), Some("web"));
        assert_eq!(s.windows.len(), 1);
        assert_eq!(s.windows[&1].app_id.as_deref(), Some("kitty"));
        assert_eq!(s.windows[&1].layout.tile_size.0, 900.0);
        assert!(s.focused_window().is_none(), "window 2 held the focus and was closed");
        assert_eq!(s.current_layout(), Some("English (US)"));
        assert!(s.touchpad_on && !s.config_failed && !s.overview_open);
        assert_eq!(s.last_screenshot.as_deref(), Some("/home/u/Pictures/Screenshots/a.png"));
        let strip = s.strip(1);
        assert_eq!(strip.columns.len(), 1);
        assert_eq!(strip.columns[0].width, 900.0);
        assert_eq!(strip.columns[0].view_x, Some(0.0));
        assert!(strip.columns[0].focused, "workspace 1's active window is 1");
    }

    #[test]
    fn strip_follows_state() {
        let mut s = session();
        let strip = s.strip(1);
        assert_eq!(strip.columns.iter().map(|c| (c.index, c.width, c.x)).collect::<Vec<_>>(), vec![(1, 600.0, 0.0), (2, 400.0, 600.0)]);
        assert_eq!(strip.focused_column().map(|c| c.index), Some(1));
        // Workspace 2 has no focused window; its active window decides.
        assert!(s.strip(2).focused_column().is_none());
        s.apply(Event::WorkspaceActiveWindowChanged { workspace_id: 2, active_window_id: Some(12) });
        assert_eq!(s.strip(2).focused_column().map(|c| c.windows.clone()), Some(vec![12]));
        // Floating-only workspace.
        let floating = s.strip(3);
        assert!(floating.columns.is_empty() && floating.floating == vec![13]);
        assert!(s.strip(404).is_empty());
    }

    fn view(ws: u64, x: f64) -> CanvasViewState {
        CanvasViewState {
            workspace_id: ws,
            offset: (x, 0.0),
            zoom: 0.5,
            target_offset: (x, 0.0),
            target_zoom: 0.5,
            view_size: (1920.0, 1080.0),
            animating: false,
            regions: vec![niri_ipc::CanvasRegion { name: "r".into(), x: -500.0, y: -500.0, w: 100.0, h: 100.0 }],
        }
    }

    #[test]
    fn workspace_modes_and_views() {
        let mut s = session();
        assert_eq!(s.workspace_mode(1), WorkspaceMode::Strip);
        assert_eq!(s.workspace_mode(404), WorkspaceMode::Strip);
        s.apply(Event::WorkspaceModeChanged { id: 1, mode: WorkspaceMode::Canvas });
        assert!(s.is_canvas(1) && !s.is_canvas(2));
        // An unknown workspace is ignored (niri-ipc's reducer would panic).
        s.apply(Event::WorkspaceModeChanged { id: 999, mode: WorkspaceMode::Canvas });
        assert_eq!(s.workspaces.len(), 3);

        s.apply(Event::CanvasViewsChanged { views: vec![view(1, 0.0)] });
        s.apply(Event::StripViewsChanged {
            views: vec![StripViewState {
                workspace_id: 2,
                view_offset: 0.0,
                target_view_offset: 0.0,
                working_area_width: 1500.0,
                column_xs: vec![0.0],
            }],
        });
        assert!(s.canvas_view(1).is_some() && s.strip_view(2).is_some());
        s.apply(Event::CanvasViewChanged { view: view(1, 42.0) });
        assert_eq!(s.canvas_view(1).map(|v| v.offset.0), Some(42.0));
        s.apply(Event::StripViewChanged {
            workspace_id: 2,
            view_offset: 7.0,
            target_view_offset: 9.0,
            working_area_width: 1500.0,
            column_xs: vec![0.0, 512.0],
        });
        assert_eq!(s.strip_view(2).map(|v| (v.view_offset, v.column_xs.len())), Some((7.0, 2)));
        // Full replacements drop workspaces that left a mode.
        s.apply(Event::CanvasViewsChanged { views: vec![] });
        assert!(s.canvas_view(1).is_none());
        assert!(!s.apply(Event::CanvasViewChanged { view: view(1, 0.0) }), "views never ask for an outputs refresh");
    }

    #[test]
    fn canvas_windows_and_bounds() {
        let mut s = session();
        s.apply(Event::WorkspaceModeChanged { id: 1, mode: WorkspaceMode::Canvas });
        let mut a = s.windows[&10].layout.clone();
        a.pos_in_scrolling_layout = None;
        a.pos_in_canvas = Some((100.0, 200.0));
        a.tile_size = (800.0, 600.0);
        let mut b = s.windows[&11].layout.clone();
        b.pos_in_scrolling_layout = None;
        b.pos_in_canvas = Some((-300.0, 0.0));
        b.tile_size = (200.0, 100.0);
        b.is_pinned = true;
        s.apply(Event::WindowLayoutsChanged { changes: vec![(10, a), (11, b)] });
        let w = s.canvas_windows(1);
        assert_eq!(w.iter().map(|w| (w.id, w.focused, w.pinned)).collect::<Vec<_>>(), vec![(10, true, false), (11, false, true)]);
        assert_eq!(s.canvas_bounds(1), Some(PlaneRect::new(-300.0, 0.0, 1200.0, 800.0)));
        // Regions of the view widen the bounds.
        s.apply(Event::CanvasViewsChanged { views: vec![view(1, 0.0)] });
        assert_eq!(s.canvas_bounds(1), Some(PlaneRect::new(-500.0, -500.0, 1400.0, 1300.0)));
        assert_eq!(s.canvas_bounds_in(1, None), Some(PlaneRect::new(-300.0, 0.0, 1200.0, 800.0)));
        assert!(s.canvas_windows(2).is_empty() && s.canvas_bounds(2).is_none());
    }

    #[test]
    fn hidden_workspaces_are_left_out_of_the_default_lists() {
        let mut s = session();
        let mut scratch = ws(4, 3, "DP-1", false, false);
        scratch.name = Some("scratch".into());
        scratch.is_hidden = true;
        let mut all: Vec<_> = s.workspaces.values().cloned().collect();
        all.push(scratch);
        s.apply(Event::WorkspacesChanged { workspaces: all });
        assert_eq!(s.workspaces_on("DP-1").iter().map(|w| w.id).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(s.workspaces_on_including_hidden("DP-1").iter().map(|w| w.id).collect::<Vec<_>>(), vec![1, 2, 4]);
        assert_eq!(s.workspaces_sorted().iter().map(|w| w.id).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!(s.workspaces_sorted_including_hidden().len(), 4);
        assert_eq!(s.hidden_workspaces().iter().map(|w| w.id).collect::<Vec<_>>(), vec![4]);

        // A hidden workspace in the middle shifts the visible numbers, not niri's idx.
        let mut all: Vec<_> = s.workspaces.values().cloned().collect();
        for w in &mut all {
            match w.id {
                4 => w.idx = 1,
                1 => w.idx = 2,
                2 => w.idx = 3,
                _ => {}
            }
        }
        s.apply(Event::WorkspacesChanged { workspaces: all });
        assert_eq!(s.visible_number(1), Some(1));
        assert_eq!(s.visible_number(2), Some(2));
        assert_eq!(s.visible_number(3), Some(1), "numbered per output");
        assert_eq!(s.visible_number(4), None);
        assert_eq!(s.visible_number(404), None);

        // The current fork: visible workspaces carry their visual index, hidden ones idx 0.
        let mut all: Vec<_> = s.workspaces.values().cloned().collect();
        for w in &mut all {
            match w.id {
                4 => w.idx = 0,
                1 => w.idx = 1,
                2 => w.idx = 2,
                _ => {}
            }
        }
        s.apply(Event::WorkspacesChanged { workspaces: all });
        assert_eq!((s.visible_number(1), s.visible_number(2), s.visible_number(4)), (Some(1), Some(2), None));
        assert_eq!(
            s.workspaces_on_including_hidden("DP-1").iter().map(|w| w.id).collect::<Vec<_>>(),
            vec![1, 2, 4],
            "hidden (idx 0) last, not first"
        );
        assert_eq!(s.workspaces_sorted_including_hidden().iter().map(|w| w.id).collect::<Vec<_>>(), vec![1, 2, 4, 3]);
    }

    /// A newer compositor's workspace/window JSON with the fork fields, and an old one
    /// without them, both parse; the old one is plain Strip.
    #[test]
    fn fork_fields_parse_with_and_without() {
        let new = r#"{"WorkspacesChanged":{"workspaces":[{"id":3,"idx":0,"name":"scratch","output":"winit","is_urgent":false,"is_active":false,"is_focused":false,"active_window_id":null,"mode":"Canvas","is_hidden":true}]}}"#;
        let old = r#"{"WorkspacesChanged":{"workspaces":[{"id":3,"idx":1,"name":null,"output":"winit","is_urgent":false,"is_active":true,"is_focused":true,"active_window_id":null}]}}"#;
        let mut s = NiriState::default();
        s.apply(serde_json::from_str(new).unwrap());
        assert!(s.is_canvas(3) && s.workspaces[&3].is_hidden);
        assert!(s.workspaces_on("winit").is_empty());
        s.apply(serde_json::from_str(old).unwrap());
        assert!(!s.is_canvas(3) && !s.workspaces[&3].is_hidden);
        let win = r#"{"WindowOpenedOrChanged":{"window":{"id":7,"title":"foot","app_id":"foot","pid":4242,"workspace_id":3,"is_focused":true,"is_floating":true,"is_urgent":false,"layout":{"pos_in_scrolling_layout":null,"tile_size":[802.0,602.0],"window_size":[800,600],"tile_pos_in_workspace_view":[611.0,224.0],"window_offset_in_tile":[1.0,1.0],"pos_in_canvas":[559.0,239.0],"is_pinned":false},"focus_timestamp":null,"hidden_from_workspace_id":null}}}"#;
        s.apply(serde_json::from_str(win).unwrap());
        assert_eq!(s.canvas_windows(3).first().map(|w| w.rect), Some(PlaneRect::new(559.0, 239.0, 802.0, 602.0)));
    }

    #[test]
    fn current_layout_edge_cases() {
        let s = NiriState {
            keyboard_layouts: Some(KeyboardLayouts { names: vec![], current_idx: 0 }),
            ..NiriState::default()
        };
        assert_eq!(s.current_layout(), None);
    }
}
