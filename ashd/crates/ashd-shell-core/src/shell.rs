//! `Shell`: the root state entity, and `ShellHandle`, what panels use to talk to it.

use std::{collections::HashMap, path::PathBuf};

use gpui::{App, Context, DisplayId, Entity, WeakEntity};
use ashd_ipc::{Request, Response, ShellStatus, SurfaceStatus};
use ashd_niri::{NiriHandle, NiriState, niri_ipc};

use crate::{
    DisplayInfo, ModalAnchor, PanelId, PanelInstance, PanelRegistry, ShellConfig,
    surfaces::{DisplaySurfaces, ShapeKey},
};

/// A running service: its latest published state (observe it) + its command handle.
pub struct Service<S: 'static, H> {
    /// Replaced wholesale on every update from the service thread; `cx.notify()`d.
    pub state: Entity<S>,
    pub handle: H,
}

impl<S: 'static, H: Clone> Clone for Service<S, H> {
    fn clone(&self) -> Self {
        Self { state: self.state.clone(), handle: self.handle.clone() }
    }
}

/// Every M1 service. Later services are added here (integrator, additive).
#[derive(Clone)]
pub struct Services {
    pub niri: Service<NiriState, NiriHandle>,
    pub sysinfo: Service<ashd_svc_sysinfo::SysinfoState, ashd_svc_sysinfo::Handle>,
    pub audio: Service<ashd_svc_audio::AudioState, ashd_svc_audio::Handle>,
    pub apps: Service<ashd_svc_apps::AppsState, ashd_svc_apps::Handle>,
    pub notifications: Service<ashd_svc_notifications::NotificationsState, ashd_svc_notifications::Handle>,
    pub logind: Service<ashd_svc_logind::LogindState, ashd_svc_logind::Handle>,
    pub clipboard: Service<ashd_svc_clipboard::ClipboardState, ashd_svc_clipboard::Handle>,
    pub mpris: Service<ashd_svc_mpris::MprisState, ashd_svc_mpris::Handle>,
    pub idle: Service<ashd_svc_idle::IdleState, ashd_svc_idle::Handle>,
    pub nightlight: Service<ashd_svc_nightlight::NightlightState, ashd_svc_nightlight::Handle>,
    pub brightness: Service<ashd_svc_brightness::BrightnessState, ashd_svc_brightness::Handle>,
    pub polkit: Service<ashd_svc_polkit::PolkitState, ashd_svc_polkit::Handle>,
    pub tray: Service<ashd_svc_tray::TrayState, ashd_svc_tray::Handle>,
    pub network: Service<ashd_svc_network::NetworkState, ashd_svc_network::Handle>,
    pub bluetooth: Service<ashd_svc_bluetooth::BluetoothState, ashd_svc_bluetooth::Handle>,
}

/// The open modal: which panel, on which display, anchored where.
#[derive(Clone, Debug, PartialEq)]
pub struct ModalState {
    pub panel: PanelId,
    pub display: DisplayId,
    pub anchor: Option<ModalAnchor>,
}

/// Root state. One per process. Panels never hold it strongly (use [`ShellHandle`]).
pub struct Shell {
    pub services: Services,
    pub(crate) registry: PanelRegistry,
    /// Modal panels (singletons), in registration order.
    pub(crate) modal_panels: Vec<PanelInstance>,
    /// Per-display surfaces and their frame/passive panel instances.
    pub(crate) displays: HashMap<DisplayId, DisplaySurfaces>,
    pub(crate) modal: Option<ModalState>,
    /// Scratchpad shown (all displays). Toggled by `ashd scratch …`.
    pub scratch_visible: bool,
    /// Anchor of the request being dispatched by [`ShellHandle::request`] (a bar button):
    /// `open_modal(.., None)` falls back to it until the end of that effect cycle.
    pub(crate) request_anchor: Option<ModalAnchor>,
    /// Frame geometry the current frame surfaces were created with.
    pub(crate) shape: Option<ShapeKey>,
    /// IPC socket path (removed on shutdown).
    pub(crate) socket: Option<PathBuf>,
    /// Last seen `NiriState::connected` (logged on change).
    pub(crate) niri_connected: bool,
    pub(crate) shutting_down: bool,
}

impl Shell {
    pub fn handle(cx: &Context<Self>) -> ShellHandle {
        ShellHandle(cx.entity().downgrade())
    }

    /// Display info for every display that has frame surfaces, by display id.
    pub fn displays(&self) -> Vec<&DisplayInfo> {
        let mut v: Vec<_> = self.displays.values().map(|d| &d.info).collect();
        v.sort_by_key(|d| u64::from(d.id));
        v
    }

    /// The display the user is on: niri's focused output if it maps to a GPUI display,
    /// else the first display.
    pub fn focused_display(&self, cx: &App) -> Option<DisplayId> {
        let niri = self.services.niri.state.read(cx);
        let focused = niri.focused_output();
        self.displays
            .values()
            .find(|d| focused.is_some() && d.info.output.as_deref() == focused)
            .or_else(|| self.displays.values().min_by_key(|d| u64::from(d.info.id)))
            .map(|d| d.info.id)
    }

    /// All live panel instances: modal singletons first, then per-display ones.
    pub fn panels(&self) -> impl Iterator<Item = &PanelInstance> {
        self.modal_panels.iter().chain(self.displays.values().flat_map(|d| d.panels.iter()))
    }

    pub fn modal(&self) -> Option<&ModalState> {
        self.modal.as_ref()
    }

    pub(crate) fn status(&self, cx: &App) -> ShellStatus {
        let mut surfaces = Vec::new();
        for d in self.displays() {
            let Some(d) = self.displays.get(&d.id) else { continue };
            for (kind, size) in d.surface_sizes(cx) {
                surfaces.push(SurfaceStatus {
                    kind: kind.name().into(),
                    output: d.info.output_label(),
                    width: size.width.into(),
                    height: size.height.into(),
                });
            }
        }
        ShellStatus {
            version: env!("CARGO_PKG_VERSION").into(),
            theme: ashd_ui::current_theme_name(cx),
            niri_connected: self.services.niri.state.read(cx).connected,
            modal: self.modal.as_ref().map(|m| m.panel.to_string()),
            surfaces,
        }
    }
}

/// What panels hold to reach the shell. All methods are no-ops once the shell is gone.
#[derive(Clone)]
pub struct ShellHandle(pub(crate) WeakEntity<Shell>);

impl ShellHandle {
    pub fn entity(&self) -> Option<Entity<Shell>> {
        self.0.upgrade()
    }

    /// Open `panel` (a `Placement::Modal` panel id) on the focused display, closing any
    /// other modal first. `anchor`: where a popup attaches (output-local px).
    ///
    /// Like every `ShellHandle` method that calls back into panels (`open_modal`,
    /// `close_modal`, `toggle_modal`, `refresh_overlays`, `set_scratch_visible`), it is
    /// deferred to the end of the current effect cycle, so it is safe to call from inside
    /// your own panel's update (listeners, `on_request`, subscriptions).
    pub fn open_modal(&self, panel: PanelId, anchor: Option<ModalAnchor>, cx: &mut App) {
        self.deferred(cx, move |shell, cx| crate::overlay::open_modal(shell, panel, None, anchor, cx));
    }

    /// Like [`Self::open_modal`], but on `display` (falls back to the focused display if
    /// it is gone): for modals that belong to a per-display panel, e.g. the rail's rename
    /// field opening next to the rail that was clicked. `anchor` is in that display's
    /// output-local px.
    pub fn open_modal_on(&self, panel: PanelId, display: DisplayId, anchor: Option<ModalAnchor>, cx: &mut App) {
        self.deferred(cx, move |shell, cx| crate::overlay::open_modal(shell, panel, Some(display), anchor, cx));
    }

    /// Whether the active window on `display` is fullscreen (see
    /// [`crate::frame::is_fullscreen`]); false while the output name is unknown.
    pub fn is_fullscreen(&self, display: DisplayId, cx: &App) -> bool {
        let Some(shell) = self.0.upgrade() else { return false };
        let s = shell.read(cx);
        let Some(output) = s.displays.get(&display).and_then(|d| d.info.output.as_deref()) else { return false };
        crate::frame::is_fullscreen(s.services.niri.state.read(cx), output)
    }

    pub fn close_modal(&self, cx: &mut App) {
        self.deferred(cx, crate::overlay::close_modal);
    }

    /// Open if closed (or another modal is open), close if `panel` is the open one.
    pub fn toggle_modal(&self, panel: PanelId, anchor: Option<ModalAnchor>, cx: &mut App) {
        let open = self.modal(cx).map(|m| m.panel) == Some(panel);
        if open { self.close_modal(cx) } else { self.open_modal(panel, anchor, cx) }
    }

    pub fn modal(&self, cx: &App) -> Option<ModalState> {
        self.0.upgrade().and_then(|s| s.read(cx).modal.clone())
    }

    /// The anchor the open modal was opened with, if `panel` is the open modal. While
    /// `panel` leaves (it was dismissed a moment ago and fades out) this is still the anchor
    /// it had, so a popup does not jump while it renders its exit.
    pub fn modal_anchor(&self, panel: PanelId, cx: &App) -> Option<ModalAnchor> {
        self.modal(cx)
            .filter(|m| m.panel == panel)
            .and_then(|m| m.anchor)
            .or_else(|| crate::overlay::closing_anchor(panel, cx))
    }

    /// The display the open modal is shown on (or the one a modal is leaving from, or the
    /// focused display).
    pub fn modal_display(&self, cx: &App) -> Option<DisplayInfo> {
        let shell = self.0.upgrade()?;
        let shell = shell.read(cx);
        let id = shell
            .modal
            .as_ref()
            .map(|m| m.display)
            .or_else(|| crate::overlay::closing_display(cx))
            .or_else(|| shell.focused_display(cx))?;
        shell.displays.get(&id).map(|d| d.info.clone())
    }

    /// Live info for `display` (the output name arrives from niri after startup, so
    /// prefer this over the `PanelContext::display` snapshot).
    pub fn display(&self, display: DisplayId, cx: &App) -> Option<DisplayInfo> {
        let shell = self.0.upgrade()?;
        shell.read(cx).displays.get(&display).map(|d| d.info.clone())
    }

    /// Re-evaluate `Panel::is_live` of passive panels: create / destroy passive overlays.
    pub fn refresh_overlays(&self, cx: &mut App) {
        self.deferred(cx, crate::overlay::refresh_passive);
    }

    pub fn set_scratch_visible(&self, visible: bool, cx: &mut App) {
        self.deferred(cx, move |shell, cx| crate::surfaces::set_scratch_visible(shell, visible, cx));
    }

    /// Run a niri action (fire-and-forget).
    pub fn niri_action(&self, action: niri_ipc::Action, cx: &App) {
        if let Some(shell) = self.0.upgrade() {
            shell.read(cx).services.niri.handle.send_action(action);
        }
    }

    /// Launch a program through niri (`Action::Spawn`): it inherits niri's environment,
    /// not the shell's.
    pub fn spawn(&self, argv: impl IntoIterator<Item = impl Into<String>>, cx: &App) {
        self.niri_action(ashd_niri::spawn_action(argv), cx);
    }

    /// Answer for `Request::Status`.
    pub fn status(&self, cx: &App) -> Response {
        match self.0.upgrade() {
            Some(s) => Response::Status(s.read(cx).status(cx)),
            None => Response::error("shell is gone"),
        }
    }

    /// Dispatch `request` exactly like `ashd …` would (offered to the panels, shell-level
    /// fallbacks), with `anchor` as the popup anchor: a panel that answers with
    /// `open_modal(id, None, cx)` gets this anchor. This is how bar buttons open popups
    /// owned by other panels (`Request::QuickSettings { op: Toggle }` + the button bounds
    /// from `DisplayInfo::bar_to_output`). Deferred to the end of the effect cycle (safe
    /// from inside your own update); errors are logged.
    pub fn request(&self, request: Request, anchor: Option<ModalAnchor>, cx: &mut App) {
        self.deferred(cx, move |shell, cx| {
            shell.update(cx, |s, _| s.request_anchor = anchor);
            let response = crate::dispatch(&shell.downgrade(), &request, cx);
            if let Response::Error(e) = &response {
                tracing::warn!("{}: {e}", request.name());
            }
            // Runs after the deferred `open_modal` the panel queued while answering.
            let weak = shell.downgrade();
            cx.defer(move |cx| {
                if let Some(s) = weak.upgrade() {
                    s.update(cx, |s, _| s.request_anchor = None);
                }
            });
        });
    }

    /// The anchor of the request currently being dispatched through [`Self::request`]
    /// (for panels that want to read it in `on_request`).
    pub fn request_anchor(&self, cx: &App) -> Option<ModalAnchor> {
        self.0.upgrade().and_then(|s| s.read(cx).request_anchor)
    }

    /// The merged `shell.json` configuration (dock pins, well-known apps).
    pub fn config(&self, cx: &App) -> ShellConfig {
        crate::config(cx).clone()
    }

    /// Launch an app by desktop id (or a command line if no installed app has that id),
    /// through niri; `Terminal=true` apps run in `config.apps.terminal`. Records the
    /// launch in the apps service. Returns false if nothing could be resolved.
    pub fn launch(&self, id_or_command: &str, cx: &App) -> bool {
        let Some(shell) = self.0.upgrade() else { return false };
        let apps = shell.read(cx).services.apps.clone();
        let state = apps.state.read(cx);
        let terminal = crate::config(cx).apps.terminal.clone();
        let Some(argv) = crate::config::resolve_argv(id_or_command, &state.apps, terminal.as_deref()) else {
            tracing::warn!("launch `{id_or_command}`: nothing to run");
            return false;
        };
        let id = crate::config::normalize_id(id_or_command);
        if state.apps.iter().any(|a| a.id == id) {
            apps.handle.record_launch(&id);
        }
        tracing::info!("launch {id_or_command}: {argv:?}");
        self.spawn(argv, cx);
        true
    }

    /// Close every surface, remove the IPC socket and quit (also on SIGTERM/SIGINT).
    pub fn shutdown(&self, cx: &mut App) {
        self.deferred(cx, crate::shutdown);
    }

    fn deferred(&self, cx: &mut App, f: impl FnOnce(&Entity<Shell>, &mut App) + 'static) {
        let weak = self.0.clone();
        cx.defer(move |cx| {
            if let Some(shell) = weak.upgrade() {
                f(&shell, cx);
            }
        });
    }
}

