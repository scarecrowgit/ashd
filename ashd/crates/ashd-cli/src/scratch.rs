//! `ashd scratch hide|show` without a running shell: straight to niri.
//!
//! Semantics (shared with the scratchpad panel, which answers the same requests while the
//! shell runs): the PANEL is the right-hand surface (`toggle|open|close`, shell only);
//! HIDING A WINDOW sends it to the compositor's hidden scratch workspace (`hide`, optionally
//! `--id N`) and `show` brings back the most recently hidden one (or window `--id N`).
//! Both need the ashd niri fork with a `workspace "scratch" { hidden }` entry in its config;
//! on stock niri or without that entry they fail with a clear error and change nothing.

use anyhow::{Context as _, bail};
use ashd_ipc::ScratchOp;
use ashd_niri::{
    NiriError, blocking,
    niri_ipc::{Action, Request, Response, Window, Workspace},
};

/// The message when the compositor has no hidden workspace.
pub const NO_HIDDEN_WORKSPACE: &str = "the compositor has no hidden workspace: add `workspace \"scratch\" { hidden }` \
     to the niri config (needs the ashd niri fork)";

/// Windows that sit on a hidden workspace.
pub fn hidden_windows<'a>(workspaces: &[Workspace], windows: &'a [Window]) -> Vec<&'a Window> {
    windows
        .iter()
        .filter(|w| w.workspace_id.is_some_and(|id| workspaces.iter().any(|ws| ws.id == id && ws.is_hidden)))
        .collect()
}

/// The niri action for a scratch window op, or why it cannot be done. Pure.
pub fn plan(op: ScratchOp, workspaces: &[Workspace], windows: &[Window]) -> Result<Action, String> {
    let hidden = hidden_windows(workspaces, windows);
    match op {
        ScratchOp::Hide | ScratchOp::HideWindow(_) => {
            if !workspaces.iter().any(|ws| ws.is_hidden) {
                return Err(NO_HIDDEN_WORKSPACE.into());
            }
            let id = match op {
                ScratchOp::HideWindow(id) => {
                    let w = windows.iter().find(|w| w.id == id).ok_or_else(|| format!("no window with id {id}"))?;
                    if hidden.iter().any(|h| h.id == w.id) {
                        return Err(format!("window {id} is already hidden"));
                    }
                    id
                }
                _ => windows.iter().find(|w| w.is_focused).map(|w| w.id).ok_or("no focused window to hide")?,
            };
            Ok(Action::HideWindow { id: Some(id) })
        }
        ScratchOp::Show | ScratchOp::ShowWindow(_) => {
            if !workspaces.iter().any(|ws| ws.is_hidden) {
                return Err(NO_HIDDEN_WORKSPACE.into());
            }
            match op {
                ScratchOp::ShowWindow(id) => {
                    if !hidden.iter().any(|w| w.id == id) {
                        return Err(format!("window {id} is not hidden"));
                    }
                    Ok(Action::UnhideWindow { id: Some(id) })
                }
                _ if hidden.is_empty() => Err("no hidden windows".into()),
                // niri's default: the most recently hidden one.
                _ => Ok(Action::UnhideWindow { id: None }),
            }
        }
        ScratchOp::Toggle | ScratchOp::Open | ScratchOp::Close => Err("this needs the running ashd-shell".into()),
    }
}

/// Whether `op` can be done without the shell.
pub fn is_local(op: ScratchOp) -> bool {
    matches!(op, ScratchOp::Hide | ScratchOp::Show | ScratchOp::HideWindow(_) | ScratchOp::ShowWindow(_))
}

/// Do `op` through niri's IPC.
pub fn run(op: ScratchOp) -> anyhow::Result<()> {
    let mut conn = blocking::Connection::connect().map_err(|e| match e {
        NiriError::NoSocket => anyhow::anyhow!("not running under niri ($NIRI_SOCKET is not set)"),
        e => anyhow::Error::new(e).context("connecting to niri"),
    })?;
    let Response::Workspaces(workspaces) = conn.request(&Request::Workspaces).context("asking niri for workspaces")?
    else {
        bail!("unexpected reply to Workspaces");
    };
    let Response::Windows(windows) = conn.request(&Request::Windows).context("asking niri for windows")? else {
        bail!("unexpected reply to Windows");
    };
    let action = plan(op, &workspaces, &windows).map_err(|e| anyhow::anyhow!(e))?;
    conn.action(action).context("sending the action to niri")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(id: u64, hidden: bool) -> Workspace {
        serde_json::from_value(serde_json::json!({
            // the ashd fork: hidden workspaces have idx 0
            "id": id, "idx": if hidden { 0 } else { id }, "name": null, "output": "X", "is_urgent": false,
            "is_active": !hidden, "is_focused": id == 1, "active_window_id": null, "is_hidden": hidden
        }))
        .unwrap()
    }

    fn win(id: u64, workspace: u64, focused: bool) -> Window {
        serde_json::from_value(serde_json::json!({
            "id": id, "title": "t", "app_id": "a", "pid": null, "workspace_id": workspace,
            "is_focused": focused, "is_floating": false, "is_urgent": false,
            "layout": {"pos_in_scrolling_layout": null, "tile_size": [1.0, 1.0], "window_size": [1, 1],
                       "tile_pos_in_workspace_view": null, "window_offset_in_tile": [0.0, 0.0]},
            "focus_timestamp": null
        }))
        .unwrap()
    }

    fn p(op: ScratchOp, w: &[Workspace], win: &[Window]) -> Result<String, String> {
        plan(op, w, win).map(|a| serde_json::to_string(&a).unwrap())
    }

    #[test]
    fn hide_and_show_plans() {
        let w = [ws(1, false), ws(9, true)];
        let win = [win(1, 1, true), win(2, 1, false), win(3, 9, false)];
        assert_eq!(p(ScratchOp::Hide, &w, &win), Ok(r#"{"HideWindow":{"id":1}}"#.to_string()));
        assert_eq!(p(ScratchOp::HideWindow(2), &w, &win), Ok(r#"{"HideWindow":{"id":2}}"#.to_string()));
        assert_eq!(p(ScratchOp::Show, &w, &win), Ok(r#"{"UnhideWindow":{"id":null}}"#.to_string()));
        assert_eq!(p(ScratchOp::ShowWindow(3), &w, &win), Ok(r#"{"UnhideWindow":{"id":3}}"#.to_string()));
        assert!(p(ScratchOp::HideWindow(3), &w, &win).unwrap_err().contains("already hidden"));
        assert!(p(ScratchOp::HideWindow(77), &w, &win).unwrap_err().contains("no window"));
        assert!(p(ScratchOp::ShowWindow(2), &w, &win).unwrap_err().contains("not hidden"));
    }

    #[test]
    fn no_hidden_workspace_is_a_clear_error() {
        let w = [ws(1, false)];
        let win = [win(1, 1, true)];
        for op in [ScratchOp::Hide, ScratchOp::Show, ScratchOp::HideWindow(1), ScratchOp::ShowWindow(1)] {
            assert_eq!(p(op, &w, &win), Err(NO_HIDDEN_WORKSPACE.to_string()), "{op:?}");
        }
    }

    #[test]
    fn nothing_to_hide_or_show() {
        let w = [ws(1, false), ws(9, true)];
        assert_eq!(p(ScratchOp::Hide, &w, &[win(1, 1, false)]), Err("no focused window to hide".into()));
        assert_eq!(p(ScratchOp::Show, &w, &[win(1, 1, true)]), Err("no hidden windows".into()));
    }

    #[test]
    fn panel_ops_need_the_shell() {
        for op in [ScratchOp::Toggle, ScratchOp::Open, ScratchOp::Close] {
            assert!(!is_local(op));
        }
        assert!(is_local(ScratchOp::Hide) && is_local(ScratchOp::ShowWindow(1)));
    }
}
