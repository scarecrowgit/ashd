//! Wayland side: ext-session-lock-v1 via smithay-client-toolkit, one lock surface per
//! output (created/destroyed on hotplug), keyboard (xkbcommon through sctk) and pointer
//! input, frame-callback-driven rendering with ashd-gfx.

use std::{ffi::c_void, ptr::NonNull, sync::mpsc};

use ashd_gfx::{
    Gfx, SurfaceTarget, Text,
    wl::{FractionalScaleHandler, ScaleGlobals, SurfaceScale},
};
use ashd_lockui::{LockAction, LockPointer, LockUi};
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
    session_lock::{SessionLock, SessionLockHandler, SessionLockState, SessionLockSurface, SessionLockSurfaceConfigure},
};
use wayland_client::{
    Connection, Proxy, QueueHandle,
    protocol::{wl_keyboard::WlKeyboard, wl_output::WlOutput, wl_pointer::WlPointer, wl_seat::WlSeat, wl_surface::WlSurface},
};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{Shape, WpCursorShapeDeviceV1};
use zeroize::Zeroizing;

use crate::{
    keyboard,
    logind::{self, Logind},
};

/// One lock surface. Field order = drop order: the wgpu surface goes before the
/// `wl_surface` it renders into.
struct LockSurf {
    target: Option<SurfaceTarget>,
    scale: SurfaceScale,
    lock: SessionLockSurface,
    output: WlOutput,
    /// Logical size from the last configure.
    size: Option<(u32, u32)>,
    dirty: bool,
    frame_pending: bool,
}

/// Exit status requested by protocol events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// Unlocked after a successful authentication.
    Unlocked,
    /// The compositor refused the lock (another locker is active) — never locked.
    Refused,
    /// The compositor ended an established lock by itself.
    FinishedByCompositor,
}

pub struct App {
    // GPU state first: dropped before the Wayland objects.
    surfaces: Vec<LockSurf>,
    gfx: Option<Gfx>,
    text: Text,
    pub ui: LockUi,
    conn: Connection,
    qh: QueueHandle<App>,
    loop_handle: LoopHandle<'static, App>,
    registry: RegistryState,
    compositor: CompositorState,
    outputs: OutputState,
    seats: SeatState,
    lock_state: SessionLockState,
    scales: ScaleGlobals,
    cursor_shapes: Option<CursorShapeManager>,
    lock: Option<SessionLock>,
    locked: bool,
    keyboard: Option<WlKeyboard>,
    pointer: Option<(WlPointer, Option<WpCursorShapeDeviceV1>)>,
    mods: Modifiers,
    layouts: Vec<String>,
    layout_index: u32,
    auth_tx: mpsc::Sender<Zeroizing<String>>,
    pub logind: Logind,
    pub exit: Option<Exit>,
    gfx_failed: bool,
}

pub struct AppInit {
    pub conn: Connection,
    pub globals: wayland_client::globals::GlobalList,
    pub qh: QueueHandle<App>,
    pub loop_handle: LoopHandle<'static, App>,
    pub ui: LockUi,
    pub auth_tx: mpsc::Sender<Zeroizing<String>>,
    pub logind: Logind,
}

impl App {
    pub fn new(init: AppInit) -> anyhow::Result<Self> {
        let AppInit { conn, globals, qh, loop_handle, ui, auth_tx, logind } = init;
        let compositor = CompositorState::bind(&globals, &qh).map_err(|e| anyhow::anyhow!("wl_compositor: {e}"))?;
        let lock_state = SessionLockState::new(&globals, &qh);
        // GPU before locking; without it the lock still works, just undrawn (typing blind
        // still unlocks) — refusing to lock would be the worse failure.
        let display = NonNull::new(conn.backend().display_ptr().cast::<c_void>());
        let gfx = match display {
            // SAFETY: the display pointer belongs to `conn`, which `App` keeps alive and
            // which outlives `gfx` (field order).
            Some(d) => match unsafe { Gfx::with_wayland_display(d) } {
                Ok(g) => Some(g),
                Err(e) => {
                    tracing::error!("GPU init failed, the lock screen will not be drawn: {e:#}");
                    None
                }
            },
            None => None,
        };
        Ok(Self {
            surfaces: Vec::new(),
            gfx,
            text: Text::new(),
            ui,
            registry: RegistryState::new(&globals),
            outputs: OutputState::new(&globals, &qh),
            seats: SeatState::new(&globals, &qh),
            scales: ScaleGlobals::bind(&globals, &qh),
            cursor_shapes: CursorShapeManager::bind(&globals, &qh).ok(),
            compositor,
            lock_state,
            lock: None,
            locked: false,
            keyboard: None,
            pointer: None,
            mods: Modifiers::default(),
            layouts: Vec::new(),
            layout_index: 0,
            auth_tx,
            logind,
            exit: None,
            gfx_failed: false,
            conn,
            qh,
            loop_handle,
        })
    }

    /// Request the lock. Errors only if the compositor lacks ext-session-lock-v1.
    pub fn lock(&mut self) -> anyhow::Result<()> {
        let lock = self
            .lock_state
            .lock(&self.qh)
            .map_err(|e| anyhow::anyhow!("the compositor does not support ext-session-lock-v1 ({e})"))?;
        self.lock = Some(lock);
        self.ensure_surfaces();
        Ok(())
    }

    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// One lock surface per output (also for outputs that appear later).
    fn ensure_surfaces(&mut self) {
        let Some(lock) = &self.lock else { return };
        for output in self.outputs.outputs() {
            if self.surfaces.iter().any(|s| s.output == output) {
                continue;
            }
            let name = self.outputs.info(&output).and_then(|i| i.name).unwrap_or_else(|| "?".into());
            tracing::info!("lock surface for output {name}");
            let wl_surface = self.compositor.create_surface(&self.qh);
            let scale = self.scales.attach(&wl_surface, &self.qh);
            let lock_surface = lock.create_lock_surface(wl_surface, &output, &self.qh);
            self.surfaces.push(LockSurf {
                target: None,
                scale,
                lock: lock_surface,
                output,
                size: None,
                dirty: true,
                frame_pending: false,
            });
        }
    }

    fn surface_index(&self, surface: &WlSurface) -> Option<usize> {
        self.surfaces.iter().position(|s| s.lock.wl_surface() == surface)
    }

    pub fn mark_all_dirty(&mut self) {
        for s in &mut self.surfaces {
            s.dirty = true;
        }
    }

    fn apply(&mut self, action: LockAction) {
        match action {
            LockAction::None => {}
            LockAction::Redraw => self.mark_all_dirty(),
            LockAction::Authenticate(pw) => {
                if self.auth_tx.send(pw).is_err() {
                    tracing::error!("auth worker is gone");
                    self.ui.auth_result(false, Some("Authentication unavailable".into()));
                }
                self.mark_all_dirty();
            }
            LockAction::Power(a) => {
                self.logind.send(logind::Command::Power(a));
                self.mark_all_dirty();
            }
        }
    }

    pub fn on_auth(&mut self, event: crate::pam::AuthEvent) {
        use crate::pam::{AuthEvent, AuthOutcome};
        match event {
            AuthEvent::Info(text) => self.ui.set_info(Some(text)),
            AuthEvent::Done(outcome) => {
                match outcome {
                    AuthOutcome::Ok => {
                        tracing::info!("authenticated");
                        self.ui.auth_result(true, None);
                    }
                    AuthOutcome::Denied => {
                        tracing::info!("authentication failed");
                        self.ui.auth_result(false, None);
                    }
                    AuthOutcome::Error(m) => {
                        tracing::warn!("authentication error: {m}");
                        self.ui.auth_result(false, Some(m));
                    }
                }
            }
        }
        self.mark_all_dirty();
    }

    /// Draw every dirty surface that is not waiting for a frame callback. Returns whether
    /// a skipped frame should be retried soon.
    pub fn render_dirty(&mut self) -> bool {
        let mut retry = false;
        let Some(gfx) = self.gfx.as_mut() else { return false };
        if self.gfx_failed {
            return false;
        }
        let display = NonNull::new(self.conn.backend().display_ptr().cast::<c_void>());
        for s in &mut self.surfaces {
            let Some((w, h)) = s.size else { continue };
            if !s.dirty || s.frame_pending {
                continue;
            }
            let scale = s.scale.scale();
            let wl_surface = s.lock.wl_surface().clone();
            match &mut s.target {
                Some(t) => {
                    if t.logical != (f64::from(w), f64::from(h)) || t.scale != scale {
                        t.resize(gfx, w, h, scale);
                    }
                }
                None => {
                    let (Some(display), Some(surface_ptr)) = (display, NonNull::new(wl_surface.id().as_ptr().cast::<c_void>()))
                    else {
                        continue;
                    };
                    // SAFETY: both pointers belong to live objects owned by `self`
                    // (`conn`, the lock surface) that outlive the target (field order).
                    match unsafe { SurfaceTarget::from_wayland(gfx, display, surface_ptr, w, h, scale) } {
                        Ok(t) => s.target = Some(t),
                        Err(e) => {
                            tracing::error!("cannot render to the lock surface: {e:#}");
                            self.gfx_failed = true;
                            return false;
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

    /// `unlock_and_destroy`, then make sure the compositor has it before we go away.
    pub fn unlock(&mut self) {
        if let Some(lock) = self.lock.take() {
            lock.unlock();
            let _ = self.conn.roundtrip();
        }
        self.surfaces.clear();
        let _ = self.conn.flush();
        self.exit = Some(Exit::Unlocked);
    }

    pub fn exit_requested(&self) -> Option<Exit> {
        self.exit
    }

    pub fn has_lock(&self) -> bool {
        self.lock.is_some()
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
        self.ui.layout = self.layouts.get(self.layout_index as usize).cloned();
    }
}

// ── session lock ──────────────────────────────────────────────────────────────

impl SessionLockHandler for App {
    fn locked(&mut self, _: &Connection, _: &QueueHandle<Self>, _: SessionLock) {
        tracing::info!("session locked");
        self.locked = true;
        self.logind.send(logind::Command::SetLockedHint(true));
    }

    fn finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: SessionLock) {
        if self.locked {
            tracing::warn!("the compositor ended the lock");
            self.exit = Some(Exit::FinishedByCompositor);
        } else {
            self.exit = Some(Exit::Refused);
        }
        // `finished` without `locked`: the lock object must just be destroyed.
        self.surfaces.clear();
        self.lock = None;
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: SessionLockSurface,
        configure: SessionLockSurfaceConfigure,
        _serial: u32,
    ) {
        if let Some(i) = self.surface_index(surface.wl_surface()) {
            let s = &mut self.surfaces[i];
            s.size = Some((configure.new_size.0.max(1), configure.new_size.1.max(1)));
            s.dirty = true;
            // A configure must be answered by a commit with a buffer of the new size;
            // do not wait for an old frame callback.
            s.frame_pending = false;
        }
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

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: wayland_client::protocol::wl_output::Transform,
    ) {
    }

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
        // Hotplug while locked: the new output needs its lock surface too.
        self.ensure_surfaces();
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, output: WlOutput) {
        let before = self.surfaces.len();
        self.surfaces.retain(|s| s.output != output);
        if self.surfaces.len() != before {
            tracing::info!("output removed, lock surface dropped");
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

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        modifiers: Modifiers,
        _: RawModifiers,
        layout: u32,
    ) {
        let changed = self.mods.caps_lock != modifiers.caps_lock || self.layout_index != layout;
        self.mods = modifiers;
        self.layout_index = layout;
        if changed {
            self.ui.caps_lock = modifiers.caps_lock;
            self.update_layout_badge();
            self.mark_all_dirty();
        }
    }

    fn update_keymap(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlKeyboard, keymap: Keymap<'_>) {
        let text = keymap.as_string();
        if tracing::enabled!(tracing::Level::DEBUG) {
            for l in text.lines().filter(|l| l.contains("xkb_symbols") || l.trim_start().starts_with("name[")) {
                tracing::debug!("keymap: {}", l.trim());
            }
        }
        let layouts = keyboard::layout_names(&text);
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
