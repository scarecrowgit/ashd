//! Wayland side: one fullscreen surface per output (wlr-layer-shell on the overlay layer
//! with exclusive keyboard focus when the compositor has it, else an xdg-toplevel set
//! fullscreen), keyboard (xkbcommon through sctk) and pointer input, frame-callback-driven
//! rendering with ashd-gfx. Same structure as `ashd-lock/src/app.rs`.

use std::{ffi::c_void, num::NonZeroU32, ptr::NonNull, sync::mpsc};

use ashd_gfx::{
    Gfx, SurfaceTarget, Text,
    wl::{FractionalScaleHandler, ScaleGlobals, SurfaceScale},
};
use ashd_lockui::{GreeterAction, GreeterUi, GreetdEvent, LockPointer};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData},
    output::{OutputHandler, OutputState},
    reexports::calloop::LoopHandle,
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keymap, Keysym, Modifiers, RawModifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler, cursor_shape::CursorShapeManager},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
        xdg::{
            XdgShell,
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
        },
    },
};
use wayland_client::{
    Connection, Proxy, QueueHandle,
    protocol::{wl_keyboard::WlKeyboard, wl_output::WlOutput, wl_pointer::WlPointer, wl_seat::WlSeat, wl_surface::WlSurface},
};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{Shape, WpCursorShapeDeviceV1};

use crate::{
    config::{SessionDef, State},
    greetd, keyboard,
    logind::Logind,
};

/// Size to use when the compositor does not say (never 0: GOTCHAS G1).
const FALLBACK_SIZE: (u32, u32) = (1280, 800);

enum Shell {
    Layer(LayerShell),
    Xdg(XdgShell),
}

/// The protocol object that gives a `wl_surface` its role.
enum Role {
    Layer(LayerSurface),
    Window(Window),
}

impl Role {
    fn wl_surface(&self) -> &WlSurface {
        match self {
            Role::Layer(l) => l.wl_surface(),
            Role::Window(w) => w.wl_surface(),
        }
    }
}

/// One output's surface. Field order = drop order: the wgpu surface goes before the
/// `wl_surface` it renders into.
struct Surf {
    target: Option<SurfaceTarget>,
    scale: SurfaceScale,
    role: Role,
    output: WlOutput,
    /// Logical size from the last configure.
    size: Option<(u32, u32)>,
    dirty: bool,
    frame_pending: bool,
}

/// Why the main loop ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// greetd accepted `start_session`: exit 0 and greetd starts the session.
    Started,
}

/// Messages from the worker threads, delivered through a calloop channel.
pub enum Msg {
    Greetd(greetd::Event),
    CanSuspend(bool),
}

pub struct App {
    // GPU state first: dropped before the Wayland objects.
    surfaces: Vec<Surf>,
    gfx: Option<Gfx>,
    text: Text,
    pub ui: GreeterUi,
    conn: Connection,
    qh: QueueHandle<App>,
    loop_handle: LoopHandle<'static, App>,
    registry: RegistryState,
    compositor: CompositorState,
    outputs: OutputState,
    seats: SeatState,
    shell: Shell,
    scales: ScaleGlobals,
    cursor_shapes: Option<CursorShapeManager>,
    keyboard: Option<WlKeyboard>,
    pointer: Option<(WlPointer, Option<WpCursorShapeDeviceV1>)>,
    mods: Modifiers,
    layouts: Vec<String>,
    layout_index: u32,
    /// `None` when there is no greetd to talk to (fatal screen).
    cmd_tx: Option<mpsc::Sender<greetd::Cmd>>,
    logind: Logind,
    sessions: Vec<SessionDef>,
    state_path: Option<std::path::PathBuf>,
    pub exit: Option<Exit>,
    presented: bool,
    /// Set when the UI went fatal; the main loop exits non-zero after a delay.
    pub fatal_since: Option<std::time::Instant>,
}

pub struct AppInit {
    pub conn: Connection,
    pub globals: wayland_client::globals::GlobalList,
    pub qh: QueueHandle<App>,
    pub loop_handle: LoopHandle<'static, App>,
    pub ui: GreeterUi,
    pub cmd_tx: Option<mpsc::Sender<greetd::Cmd>>,
    pub logind: Logind,
    pub sessions: Vec<SessionDef>,
    pub state_path: Option<std::path::PathBuf>,
}

impl App {
    pub fn new(init: AppInit) -> anyhow::Result<Self> {
        let AppInit { conn, globals, qh, loop_handle, ui, cmd_tx, logind, sessions, state_path } = init;
        let compositor = CompositorState::bind(&globals, &qh).map_err(|e| anyhow::anyhow!("wl_compositor: {e}"))?;
        let shell = match LayerShell::bind(&globals, &qh) {
            Ok(l) => {
                tracing::info!("using wlr-layer-shell (overlay layer, exclusive keyboard)");
                Shell::Layer(l)
            }
            Err(e) => {
                tracing::info!("no wlr-layer-shell ({e}); falling back to fullscreen xdg-toplevels");
                Shell::Xdg(XdgShell::bind(&globals, &qh).map_err(|e| anyhow::anyhow!("the compositor has neither wlr-layer-shell nor xdg-shell ({e})"))?)
            }
        };
        let display = NonNull::new(conn.backend().display_ptr().cast::<c_void>()).ok_or_else(|| anyhow::anyhow!("no wl_display pointer"))?;
        // SAFETY: the display pointer belongs to `conn`, which `App` keeps alive and
        // which outlives `gfx` (field order).
        let gfx = unsafe { Gfx::with_wayland_display(display) }.map_err(|e| anyhow::anyhow!("GPU init failed: {e:#}"))?;
        Ok(Self {
            surfaces: Vec::new(),
            gfx: Some(gfx),
            text: Text::new(),
            ui,
            registry: RegistryState::new(&globals),
            outputs: OutputState::new(&globals, &qh),
            seats: SeatState::new(&globals, &qh),
            scales: ScaleGlobals::bind(&globals, &qh),
            cursor_shapes: CursorShapeManager::bind(&globals, &qh).ok(),
            compositor,
            shell,
            keyboard: None,
            pointer: None,
            mods: Modifiers::default(),
            layouts: Vec::new(),
            layout_index: 0,
            cmd_tx,
            logind,
            sessions,
            state_path,
            exit: None,
            presented: false,
            fatal_since: None,
            conn,
            qh,
            loop_handle,
        })
    }

    /// One surface per output (also for outputs that appear later).
    pub fn ensure_surfaces(&mut self) {
        for output in self.outputs.outputs() {
            if self.surfaces.iter().any(|s| s.output == output) {
                continue;
            }
            let name = self.outputs.info(&output).and_then(|i| i.name).unwrap_or_else(|| "?".into());
            tracing::info!("greeter surface for output {name}");
            let wl_surface = self.compositor.create_surface(&self.qh);
            let scale = self.scales.attach(&wl_surface, &self.qh);
            let role = match &self.shell {
                Shell::Layer(shell) => {
                    let layer = shell.create_layer_surface(&self.qh, wl_surface, Layer::Overlay, Some("ashd-greeter"), Some(&output));
                    layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
                    layer.set_exclusive_zone(-1);
                    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
                    layer.set_size(0, 0);
                    layer.commit();
                    Role::Layer(layer)
                }
                Shell::Xdg(shell) => {
                    let window = shell.create_window(wl_surface, WindowDecorations::None, &self.qh);
                    window.set_title("ashd-greeter");
                    window.set_app_id("ashd-greeter");
                    window.set_fullscreen(Some(&output));
                    window.commit();
                    Role::Window(window)
                }
            };
            self.surfaces.push(Surf { target: None, scale, role, output, size: None, dirty: true, frame_pending: false });
        }
    }

    fn surface_index(&self, surface: &WlSurface) -> Option<usize> {
        self.surfaces.iter().position(|s| s.role.wl_surface() == surface)
    }

    pub fn mark_all_dirty(&mut self) {
        for s in &mut self.surfaces {
            s.dirty = true;
        }
    }

    fn send(&mut self, cmd: greetd::Cmd) {
        match &self.cmd_tx {
            Some(tx) if tx.send(cmd).is_ok() => {}
            _ => self.go_fatal("The login service is not reachable"),
        }
    }

    /// Show `message` and ignore the keyboard; the main loop exits non-zero after a delay.
    pub fn go_fatal(&mut self, message: &str) {
        tracing::error!("{message}");
        self.cmd_tx = None;
        self.ui.set_fatal(message);
        self.fatal_since.get_or_insert_with(std::time::Instant::now);
        self.mark_all_dirty();
    }

    fn apply(&mut self, action: GreeterAction) {
        match action {
            GreeterAction::None => {}
            GreeterAction::Redraw => self.mark_all_dirty(),
            GreeterAction::CreateSession(user) => {
                self.send(greetd::Cmd::Create(user));
                self.mark_all_dirty();
            }
            GreeterAction::Answer(pw) => {
                self.send(greetd::Cmd::Answer(pw));
                self.mark_all_dirty();
            }
            GreeterAction::Cancel => {
                self.send(greetd::Cmd::Cancel);
                self.mark_all_dirty();
            }
            GreeterAction::Power(a) => {
                self.logind.send(a);
                self.mark_all_dirty();
            }
        }
    }

    pub fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::CanSuspend(v) => {
                self.ui.lock.can_suspend = v;
                self.mark_all_dirty();
            }
            Msg::Greetd(greetd::Event::Greetd(ev)) => {
                if ev == GreetdEvent::Authenticated {
                    tracing::info!("authenticated as {}", self.ui.user().trim());
                    self.remember();
                } else if let GreetdEvent::Failed { auth, text } = &ev {
                    tracing::info!("login failed (wrong password: {auth}): {text}");
                }
                let a = self.ui.greetd_event(ev);
                self.apply(a);
                self.mark_all_dirty();
            }
            Msg::Greetd(greetd::Event::Started) => {
                tracing::info!("session started by greetd");
                self.exit = Some(Exit::Started);
            }
            Msg::Greetd(greetd::Event::Disconnected(why)) => {
                self.go_fatal(&format!("Lost the connection to greetd: {why}"));
            }
        }
    }

    /// Persist the user and session for the next boot (best effort).
    fn remember(&self) {
        let Some(path) = &self.state_path else { return };
        let state = State { user: Some(self.ui.user().trim().to_string()), session: Some(self.ui.session_name().to_string()) };
        if let Err(e) = state.save(path) {
            tracing::warn!("cannot write {}: {e}", path.display());
        }
    }

    /// Send `start_session` once the fade-out after a successful login is done.
    pub fn poll_start(&mut self, now: std::time::Instant) {
        if let Some(i) = self.ui.take_start(now) {
            let Some(session) = self.sessions.get(i) else { return };
            tracing::info!("starting session {} ({:?})", session.name, session.cmd);
            let cmd = greetd::Cmd::Start { cmd: session.cmd.clone(), env: session.env.clone() };
            self.send(cmd);
        }
    }

    /// Draw every dirty surface that is not waiting for a frame callback. Returns whether
    /// a skipped frame should be retried soon.
    pub fn render_dirty(&mut self) -> bool {
        let mut retry = false;
        let Some(gfx) = self.gfx.as_mut() else { return false };
        let display = NonNull::new(self.conn.backend().display_ptr().cast::<c_void>());
        for s in &mut self.surfaces {
            let Some((w, h)) = s.size else { continue };
            if !s.dirty || s.frame_pending {
                continue;
            }
            let scale = s.scale.scale();
            let wl_surface = s.role.wl_surface().clone();
            match &mut s.target {
                Some(t) => {
                    if t.logical != (f64::from(w), f64::from(h)) || t.scale != scale {
                        t.resize(gfx, w, h, scale);
                    }
                }
                None => {
                    let (Some(display), Some(surface_ptr)) = (display, NonNull::new(wl_surface.id().as_ptr().cast::<c_void>())) else {
                        continue;
                    };
                    // SAFETY: both pointers belong to live objects owned by `self`
                    // (`conn`, the surface role) that outlive the target (field order).
                    match unsafe { SurfaceTarget::from_wayland(gfx, display, surface_ptr, w, h, scale) } {
                        Ok(t) => s.target = Some(t),
                        Err(e) => {
                            tracing::error!("cannot render to the greeter surface: {e:#}");
                            retry = true;
                            continue;
                        }
                    }
                }
            }
            let Some(target) = s.target.as_mut() else { continue };
            s.scale.apply(&wl_surface, w, h);
            wl_surface.frame(&self.qh, FrameCallbackData(wl_surface.clone()));
            match gfx.render(target, &mut self.text, &mut self.ui) {
                Ok(true) => {
                    s.dirty = false;
                    s.frame_pending = true;
                    if !self.presented {
                        self.presented = true;
                        tracing::info!("first frame presented");
                    }
                }
                Ok(false) => retry = true,
                Err(e) => {
                    tracing::error!("render failed: {e:#}");
                    retry = true;
                }
            }
        }
        retry
    }

    fn key(&mut self, event: &KeyEvent) {
        if let Some(input) = keyboard::translate(event, &self.mods) {
            let a = self.ui.input(input);
            self.apply(a);
        } else if matches!(event.keysym, Keysym::Caps_Lock) {
            self.mark_all_dirty();
        }
    }

    fn update_layout_badge(&mut self) {
        self.ui.lock.layout = self.layouts.get(self.layout_index as usize).cloned();
    }

    fn configured(&mut self, surface: &WlSurface, size: (Option<NonZeroU32>, Option<NonZeroU32>)) {
        if let Some(i) = self.surface_index(surface) {
            let s = &mut self.surfaces[i];
            let prev = s.size.unwrap_or(FALLBACK_SIZE);
            s.size = Some((size.0.map_or(prev.0, NonZeroU32::get), size.1.map_or(prev.1, NonZeroU32::get)));
            s.dirty = true;
            // A configure must be answered by a commit with a buffer of the new size;
            // do not wait for an old frame callback.
            s.frame_pending = false;
        }
    }
}

// ── shells ────────────────────────────────────────────────────────────────────

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        // The compositor took the surface away (output unplugged): forget it.
        self.surfaces.retain(|s| s.role.wl_surface() != layer.wl_surface());
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface, configure: LayerSurfaceConfigure, _serial: u32) {
        self.configured(layer.wl_surface(), (NonZeroU32::new(configure.new_size.0), NonZeroU32::new(configure.new_size.1)));
    }
}

impl WindowHandler for App {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        // A login screen does not close.
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, window: &Window, configure: WindowConfigure, _serial: u32) {
        self.configured(window.wl_surface(), configure.new_size);
    }
}

// ── compositor / output ───────────────────────────────────────────────────────

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &WlSurface, factor: i32) {
        if let Some(i) = self.surface_index(surface)
            && self.surfaces[i].scale.set_integer(factor)
        {
            self.surfaces[i].dirty = true;
            self.surfaces[i].frame_pending = false;
        }
    }

    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: wayland_client::protocol::wl_output::Transform) {}

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &WlSurface, _: u32) {
        let animating = ashd_gfx::Draw::animating(&self.ui);
        if let Some(i) = self.surface_index(surface) {
            let s = &mut self.surfaces[i];
            s.frame_pending = false;
            if animating {
                s.dirty = true;
            }
        }
    }

    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: &WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: &WlOutput) {}
}

impl FractionalScaleHandler for App {
    fn preferred_fractional_scale(&mut self, surface: &WlSurface, scale_120: u32) {
        if let Some(i) = self.surface_index(surface)
            && self.surfaces[i].scale.set_fractional(scale_120)
        {
            tracing::info!("fractional scale {}", f64::from(scale_120) / 120.0);
            self.surfaces[i].dirty = true;
            self.surfaces[i].frame_pending = false;
        }
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {
        self.ensure_surfaces();
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, output: WlOutput) {
        let before = self.surfaces.len();
        self.surfaces.retain(|s| s.output != output);
        if self.surfaces.len() != before {
            tracing::info!("output removed, greeter surface dropped");
        }
    }
}

// ── seat: keyboard + pointer ──────────────────────────────────────────────────

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}

    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: WlSeat, capability: Capability) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            let repeat = Box::new(|app: &mut App, _: &WlKeyboard, event: KeyEvent| app.key(&event));
            match self.seats.get_keyboard_with_repeat(qh, &seat, None, self.loop_handle.clone(), repeat) {
                Ok(k) => self.keyboard = Some(k),
                Err(e) => tracing::error!("keyboard: {e}"),
            }
        }
        if capability == Capability::Pointer && self.pointer.is_none() {
            match self.seats.get_pointer(qh, &seat) {
                Ok(p) => {
                    let shape = self.cursor_shapes.as_ref().map(|m| m.get_shape_device(&p, qh));
                    self.pointer = Some((p, shape));
                }
                Err(e) => tracing::error!("pointer: {e}"),
            }
        }
    }

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat, capability: Capability) {
        if capability == Capability::Keyboard
            && let Some(k) = self.keyboard.take()
        {
            k.release();
        }
        if capability == Capability::Pointer
            && let Some((p, shape)) = self.pointer.take()
        {
            if let Some(s) = shape {
                s.destroy();
            }
            p.release();
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
}

impl KeyboardHandler for App {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlKeyboard, _: &WlSurface, _: u32, _: &[u32], _: &[Keysym]) {}

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlKeyboard, _: &WlSurface, _: u32) {}

    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlKeyboard, _: u32, event: KeyEvent) {
        self.key(&event);
    }

    fn repeat_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlKeyboard, _: u32, event: KeyEvent) {
        self.key(&event);
    }

    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlKeyboard, _: u32, _: KeyEvent) {}

    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlKeyboard, _: u32, modifiers: Modifiers, _: RawModifiers, layout: u32) {
        let changed = self.mods.caps_lock != modifiers.caps_lock || self.layout_index != layout;
        self.mods = modifiers;
        self.layout_index = layout;
        if changed {
            self.ui.lock.caps_lock = modifiers.caps_lock;
            self.update_layout_badge();
            self.mark_all_dirty();
        }
    }

    fn update_keymap(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlKeyboard, keymap: Keymap<'_>) {
        let layouts = keyboard::layout_names(&keymap.as_string());
        // Virtual keyboards (wtype, remote input) upload anonymous keymaps for a moment;
        // keep showing the last named layouts then.
        if !layouts.is_empty() {
            tracing::info!("keyboard layouts: {layouts:?}");
            self.layouts = layouts;
        }
        self.update_layout_badge();
        self.mark_all_dirty();
    }
}

impl PointerHandler for App {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlPointer, events: &[PointerEvent]) {
        for ev in events {
            let Some(i) = self.surface_index(&ev.surface) else { continue };
            let Some((w, h)) = self.surfaces[i].size else { continue };
            let size = (f64::from(w), f64::from(h));
            let (x, y) = ev.position;
            let input = match ev.kind {
                PointerEventKind::Enter { serial } => {
                    if let Some((_, Some(shape))) = &self.pointer {
                        shape.set_shape(serial, Shape::Default);
                    }
                    LockPointer::Motion { x, y }
                }
                PointerEventKind::Leave { .. } => LockPointer::Leave,
                PointerEventKind::Motion { .. } => LockPointer::Motion { x, y },
                // BTN_LEFT
                PointerEventKind::Press { button: 0x110, .. } => LockPointer::Press { x, y },
                _ => continue,
            };
            let a = self.ui.pointer(input, size);
            self.apply(a);
        }
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_registry!(App);
smithay_client_toolkit::delegate_dispatch2!(App);
