//! ashd shell runtime (GPUI): the [`Shell`] root state, the [`Panel`] contract, the
//! surface registry (rail / bar / scratchpad / overlays per display), the overlay host,
//! IPC dispatch and the bridge from service threads into GPUI entities.
//!
//! `ashd-shell`'s `main` is wiring only:
//!
//! ```ignore
//! ashd_shell_core::run(ShellOptions::from_env(), |reg| {
//!     reg.register::<ashd_panel_bar::BarPanel>();
//!     reg.register::<ashd_panel_launcher::LauncherPanel>();
//! })
//! ```

pub mod anchors;
pub mod apps;
pub mod config;
pub mod frame;
mod ipc;
pub mod notices;
mod overlay;
mod panel;
mod power;
mod shell;
mod signals;
mod surfaces;

use std::{collections::HashMap, path::PathBuf};

use async_channel::Receiver;
use gpui::{App, AppContext as _, Entity, QuitMode};
pub use config::{AppsConfig, ShellConfig, config};
pub use ipc::dispatch;
pub use overlay::{LINGER_MAX, ModalDismiss, hit_area, pointer_exit_reset};
pub use panel::{ModalAnchor, ModalStyle, Panel, PanelContext, PanelId, PanelInstance, PanelRegistry, Placement, Slot};
use ashd_tokens::ThemeName;
pub use shell::{ModalState, Service, Services, Shell, ShellHandle};
pub use surfaces::{DisplayInfo, ShapeKey, SurfaceKind, SurfaceRoot, layer_options, output_uuid};

/// Startup options (`ashd-shell --help`).
#[derive(Clone, Debug)]
pub struct ShellOptions {
    /// Theme at startup; default: the persisted one (`ashd_tokens::state::current_theme`).
    pub theme: Option<ThemeName>,
    /// IPC socket path; default `ashd_ipc::socket_path()`. `None` + `ipc = false`: no IPC.
    pub socket: Option<PathBuf>,
    pub ipc: bool,
    /// Start the D-Bus / pulse / sysinfo service threads. Off: panels get default states
    /// and disconnected handles (for screenshots and tests).
    pub services: bool,
    /// Show the scratchpad at startup. Default `false` in M1 (the scratchpad panel arrives
    /// in M2; `ashd scratch toggle` still shows the placeholder surface).
    pub scratch_visible: bool,
}

impl Default for ShellOptions {
    fn default() -> Self {
        Self { theme: None, socket: None, ipc: true, services: true, scratch_visible: false }
    }
}

/// Run the shell until `Request::Quit` (or `cx.quit()`). Fails fast — before any window
/// opens — if another shell already serves the IPC socket.
pub fn run(options: ShellOptions, register: impl FnOnce(&mut PanelRegistry)) -> anyhow::Result<()> {
    // First, before any thread exists (async-io, GPUI, services): SIGTERM/SIGINT/SIGHUP
    // are blocked process-wide and received by one thread (clean shutdown / reload).
    let signals = signals::install();

    let mut registry = PanelRegistry::default();
    register(&mut registry);

    let socket = options.ipc.then(|| options.socket.clone().unwrap_or_else(ashd_ipc::socket_path));
    let server = match &socket {
        Some(path) => {
            Some(ashd_ipc::Server::bind(path).map_err(|e| anyhow::anyhow!("ipc socket {}: {e}", path.display()))?)
        }
        None => None,
    };
    let shell_config = ShellConfig::load();
    tracing::info!("config: {:?} (dock {:?}, apps {:?})", shell_config.sources, shell_config.dock, shell_config.apps);

    gpui_platform::application()
        .with_assets(ashd_ui::Assets)
        // A shell has no "last window": surfaces come and go (overlays, scratchpad,
        // display hotplug) and the process must stay.
        .with_quit_mode(QuitMode::Explicit)
        .run(move |cx: &mut App| {
            ashd_ui::init(options.theme.unwrap_or_else(ashd_tokens::state::current_theme), cx);
            cx.set_global(shell_config);
            overlay::bind_keys(cx);
            let services = start_services(options.services, cx);
            let shell = cx.new(|_| Shell {
                services: services.clone(),
                registry: registry.clone(),
                modal_panels: Vec::new(),
                displays: HashMap::new(),
                modal: None,
                scratch_visible: options.scratch_visible,
                request_anchor: None,
                shape: None,
                socket,
                niri_connected: false,
                shutting_down: false,
            });
            // Panels are built outside any Shell update: their `new` may read the shell.
            let ctx = PanelContext {
                shell: ShellHandle(shell.downgrade()),
                services: services.clone(),
                niri: services.niri.state.clone(),
                niri_handle: services.niri.handle.clone(),
                display: None,
            };
            let modal_panels = registry.build(|p| matches!(p, Placement::Modal(_)), &ctx, cx);
            shell.update(cx, |s, _| s.modal_panels = modal_panels);
            surfaces::sync_displays(&shell, cx);
            surfaces::sync_shape(&shell, cx);

            // Display hotplug: create / destroy per-output surfaces and panels.
            let weak = shell.downgrade();
            cx.observe_displays(move |_, cx| {
                if let Some(s) = weak.upgrade() {
                    surfaces::sync_displays(&s, cx);
                }
            })
            .detach();
            // Output names come from niri (display uuid = v5(output name)); after a niri
            // IPC reconnect the fresh state re-maps them.
            let weak = shell.downgrade();
            cx.observe(&services.niri.state, move |niri, cx| {
                let Some(s) = weak.upgrade() else { return };
                let connected = niri.read(cx).connected;
                if s.read(cx).niri_connected != connected {
                    if connected {
                        tracing::info!("niri ipc connected");
                    } else {
                        tracing::warn!("niri ipc disconnected (ashd-niri reconnects with backoff)");
                    }
                    s.update(cx, |s, cx| {
                        s.niri_connected = connected;
                        cx.notify();
                    });
                }
                surfaces::sync_displays(&s, cx);
            })
            .detach();
            // Theme switch: a different frame Shape (rail width, bar height, floating
            // gap) needs new frame surfaces. `set_theme` swaps the shape at the
            // crossfade midpoint; recreate outside the global update.
            let weak = shell.downgrade();
            cx.observe_global::<ashd_ui::Theme>(move |cx| {
                let Some(s) = weak.upgrade() else { return };
                if s.read(cx).shape != Some(ShapeKey::of(ashd_ui::theme(cx))) {
                    cx.defer(move |cx| surfaces::sync_shape(&s, cx));
                }
            })
            .detach();

            if options.services {
                power::wire(&services, shell.downgrade(), cx);
                notices::wire(&services, cx);
            }
            if let Some(server) = server {
                ipc::serve(server, shell.downgrade(), cx);
            }
            if let Some(rx) = signals {
                signals::listen(rx, shell.downgrade(), cx);
            }
            // The app owns the root entity for the whole run (windows/panels hold weak refs).
            cx.set_global(ShellGlobal(shell));
        });
    Ok(())
}

/// Clean shutdown: dismiss the modal (`on_close`), close every surface of every display
/// (overlays first, bar last), remove the IPC socket, quit the app.
pub(crate) fn shutdown(shell: &Entity<Shell>, cx: &mut App) {
    if shell.read(cx).shutting_down {
        return;
    }
    tracing::info!("shutting down");
    overlay::close_modal(shell, cx);
    overlay::end_all_lingering(cx);
    shell.update(cx, |s, _| s.shutting_down = true);
    let ids: Vec<_> = shell.read(cx).displays.keys().copied().collect();
    for id in ids {
        for kind in SurfaceKind::CLOSE_ORDER {
            surfaces::close_surface(shell, id, kind, cx);
        }
    }
    if let Some(path) = shell.read(cx).socket.clone() {
        let _ = std::fs::remove_file(&path);
    }
    // Let the surface removals and the last IPC response reach the wire first.
    cx.spawn(async move |cx| {
        cx.background_executor().timer(std::time::Duration::from_millis(50)).await;
        cx.update(|cx| cx.quit());
    })
    .detach();
}

/// Strong handle to the root [`Shell`] entity, owned by the app.
pub struct ShellGlobal(pub Entity<Shell>);

impl gpui::Global for ShellGlobal {}

/// Copy every value from a service thread into `entity` and notify observers.
pub fn bridge<S: 'static>(rx: Receiver<S>, entity: Entity<S>, cx: &mut App) {
    cx.spawn(async move |cx| {
        while let Ok(state) = rx.recv().await {
            cx.update(|cx| {
                entity.update(cx, |s, cx| {
                    *s = state;
                    cx.notify();
                })
            });
        }
    })
    .detach();
}

/// `start()` a service and bridge its state into a new entity.
pub fn start_service<S: Default + 'static, H>(start: impl FnOnce() -> (H, Receiver<S>), cx: &mut App) -> Service<S, H> {
    let (handle, rx) = start();
    let state = cx.new(|_| S::default());
    bridge(rx, state.clone(), cx);
    Service { state, handle }
}

fn idle<S: Default + 'static, H>(handle: H, cx: &mut App) -> Service<S, H> {
    Service { state: cx.new(|_| S::default()), handle }
}

fn start_services(enabled: bool, cx: &mut App) -> Services {
    // niri always: the frame is useless without it (and it degrades to `connected: false`).
    let niri = start_service(ashd_niri::start, cx);
    if !enabled {
        return Services {
            niri,
            sysinfo: idle(ashd_svc_sysinfo::Handle::disconnected(), cx),
            audio: idle(ashd_svc_audio::Handle::disconnected(), cx),
            apps: idle(ashd_svc_apps::Handle::disconnected(), cx),
            notifications: idle(ashd_svc_notifications::Handle::disconnected(), cx),
            logind: idle(ashd_svc_logind::Handle::disconnected(), cx),
            clipboard: idle(ashd_svc_clipboard::Handle::disconnected(), cx),
            mpris: idle(ashd_svc_mpris::Handle::disconnected(), cx),
            idle: idle(ashd_svc_idle::Handle::disconnected(), cx),
            nightlight: idle(ashd_svc_nightlight::Handle::disconnected(), cx),
            brightness: idle(ashd_svc_brightness::Handle::disconnected(), cx),
            polkit: idle(ashd_svc_polkit::Handle::disconnected(), cx),
            tray: idle(ashd_svc_tray::Handle::disconnected(), cx),
            network: idle(ashd_svc_network::Handle::disconnected(), cx),
            bluetooth: idle(ashd_svc_bluetooth::Handle::disconnected(), cx),
        };
    }
    Services {
        niri,
        sysinfo: start_service(ashd_svc_sysinfo::start, cx),
        audio: start_service(ashd_svc_audio::start, cx),
        apps: start_service(ashd_svc_apps::start, cx),
        notifications: start_service(ashd_svc_notifications::start, cx),
        logind: start_service(ashd_svc_logind::start, cx),
        clipboard: start_service(ashd_svc_clipboard::start, cx),
        mpris: start_service(ashd_svc_mpris::start, cx),
        idle: start_service(ashd_svc_idle::start, cx),
        nightlight: start_service(ashd_svc_nightlight::start, cx),
        brightness: start_service(ashd_svc_brightness::start, cx),
        polkit: start_service(ashd_svc_polkit::start, cx),
        tray: start_service(ashd_svc_tray::start, cx),
        network: start_service(ashd_svc_network::start, cx),
        bluetooth: start_service(ashd_svc_bluetooth::start, cx),
    }
}
