//! IPC server wiring and request dispatch.
//!
//! Order: shell-level requests (ping, status, quit, dismiss, theme, lock) are answered
//! here; everything else is offered to panels — modal singletons first, then the
//! instances on the focused display — and the first `Some(response)` wins. A few
//! requests have a shell-level fallback when no panel claims them (`scratch`).

use gpui::{App, AppContext as _, WeakEntity};
use ashd_ipc::{Incoming, Request, Response, ScratchOp, Server, ThemeRequest};

use crate::Shell;

pub(crate) fn serve(server: Server, shell: WeakEntity<Shell>, cx: &mut App) {
    tracing::info!("ipc listening on {}", server.path().display());
    let (tx, rx) = async_channel::unbounded::<Incoming>();
    cx.background_spawn(server.run(tx)).detach();
    cx.spawn(async move |cx| {
        while let Ok(incoming) = rx.recv().await {
            let response = cx.update(|cx| dispatch(&shell, &incoming.request, cx));
            incoming.respond(response);
        }
    })
    .detach();
}

/// Answer one request on the main thread. Public for tests and for panels that want to
/// re-dispatch (e.g. a bar button that behaves like `ashd launcher`).
pub fn dispatch(shell: &WeakEntity<Shell>, request: &Request, cx: &mut App) -> Response {
    let Some(shell) = shell.upgrade() else { return Response::error("shell is shutting down") };
    tracing::debug!("ipc request {}", request.name());
    let handle = crate::ShellHandle(shell.downgrade());
    let response = match request {
        Request::Ping => Response::Pong {
            version: env!("CARGO_PKG_VERSION").into(),
            protocol: ashd_ipc::PROTOCOL_VERSION,
            pid: std::process::id(),
        },
        Request::Status => handle.status(cx),
        Request::Quit => {
            handle.shutdown(cx);
            Response::Ok
        }
        Request::Dismiss => {
            handle.close_modal(cx);
            Response::Ok
        }
        Request::Theme(ThemeRequest::Get) => Response::Theme(ashd_ui::current_theme_name(cx)),
        Request::Theme(ThemeRequest::Set(name)) => {
            ashd_ui::set_theme(*name, true, cx);
            Response::Theme(*name)
        }
        Request::Theme(ThemeRequest::Cycle) => {
            let next = ashd_ui::current_theme_name(cx).next();
            ashd_ui::set_theme(next, true, cx);
            Response::Theme(next)
        }
        Request::Lock => {
            handle.spawn(["ashd-lock"], cx);
            Response::Ok
        }
        other => offer_to_panels(&shell, other, cx).unwrap_or_else(|| fallback(&handle, other, cx)),
    };
    handle.refresh_overlays(cx);
    response
}

fn offer_to_panels(shell: &gpui::Entity<Shell>, request: &Request, cx: &mut App) -> Option<Response> {
    let (modal, local) = {
        let s = shell.read(cx);
        let focused = s.focused_display(cx);
        let local: Vec<_> =
            focused.and_then(|id| s.displays.get(&id)).map(|d| d.panels.clone()).unwrap_or_default();
        (s.modal_panels.clone(), local)
    };
    modal.iter().chain(local.iter()).find_map(|p| p.on_request(request, cx))
}

fn fallback(handle: &crate::ShellHandle, request: &Request, cx: &mut App) -> Response {
    match request {
        Request::Scratch(op) => {
            let visible = handle.entity().map(|s| s.read(cx).scratch_visible).unwrap_or(false);
            // The scratchpad panel answers every op itself; this is the panel-less
            // fallback: only the surface can be shown or hidden, windows cannot.
            let want = match op {
                ScratchOp::Open => true,
                ScratchOp::Close => false,
                ScratchOp::Toggle => !visible,
                ScratchOp::Hide | ScratchOp::Show | ScratchOp::HideWindow(_) | ScratchOp::ShowWindow(_) => {
                    return Response::error("no scratchpad panel is registered; cannot hide or show windows");
                }
            };
            handle.set_scratch_visible(want, cx);
            Response::Ok
        }
        other => Response::error(format!("no panel handles `{}` yet", other.name())),
    }
}
