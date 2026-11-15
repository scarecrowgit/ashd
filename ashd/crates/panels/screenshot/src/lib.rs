//! Screenshot / recording overlay (det-08).
//!
//! `ashd screenshot [--region|--window|--output] [--record]` takes a still of the focused
//! output *first* (niri's `ScreenshotScreen` to a file under the runtime dir, pointer
//! hidden), then opens a modal overlay that shows that still as its backdrop — the overlay
//! never ends up in the result. In it the user picks what to capture (Region / Window /
//! Output), Shot or Record, and a delay of 0 / 3 / 5 s.
//!
//! A shot crops the still (or, with a delay, a fresh capture) to the selection, saves
//! `Screenshot from <time>.png`, puts the PNG on the clipboard (`wl-copy` through niri) and
//! toasts the result. A recording spawns `wf-recorder` (else `gpu-screen-recorder`) for the
//! selection from a supervisor thread; the REC chip, the countdown and the toasts live in
//! the passive overlay ([`ScreenshotHudPanel`]).
//!
//! In-panel keys: `r` region, `w` window, `o` output, `tab` Shot/Record, `d` delay, arrows
//! nudge the selection by 1 px (`shift` 10 px, `alt` resizes the right / bottom edge
//! instead), `enter` confirms, `esc` cancels; mouse: drag / move / resize the selection,
//! double-click confirms, click confirms in Window and Output mode, right-click cancels.
//!
//! Owner: screenshot. Pure logic (geometry, scale mapping, names, command lines):
//! `capture.rs`; disk and clipboard: `output.rs`; the recorder subprocess: `recorder.rs`;
//! the passive half: `hud.rs`; the overlay's layout: `view.rs`.

mod capture;
mod hud;
mod output;
mod recorder;
mod view;

use std::time::{Duration, Instant};

use chrono::Local;
use gpui::{
    AppContext as _, AsyncApp, Context, DisplayId, Entity, FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Task, Window,
};
use ashd_ipc::{CaptureMode, PanelOp, RecordOp, Request, Response};
use ashd_niri::{NiriHandle, niri_ipc::Action};
use ashd_shell_core::{ModalStyle, Panel, PanelContext, PanelId, Placement};

use capture::{
    Delay, Dim, Dir, Drag, Kind, OutputGeom, Pt, Rect, RecorderKind, WinRect, default_selection, nudge,
    nudge_step, rec_target, recording_file_name, unique_path, window_at, windows_in_view,
};
pub use hud::ScreenshotHudPanel;
use hud::{Countdown, Hud, HudEvent, Recording, Toast, ToastBody};
use output::Still;

/// How long niri gets to write the still.
const STILL_TIMEOUT: Duration = Duration::from_secs(5);
/// After hiding the countdown / overlay, wait this long before capturing so the compositor
/// has unmapped the surface.
const SETTLE: Duration = Duration::from_millis(180);
/// Result toasts stay up this long (errors a bit longer so they can be read).
const TOAST_HOLD: Duration = Duration::from_millis(2000);
const TOAST_HOLD_ERROR: Duration = Duration::from_millis(5000);

/// Mode, Shot / Record and delay survive from one use of the overlay to the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Prefs {
    pub mode: CaptureMode,
    pub kind: Kind,
    pub delay: Delay,
}

/// What the overlay needs while it is open: the frozen still and the selection on it.
pub(crate) struct Session {
    pub out: OutputGeom,
    pub bounds: Dim,
    pub still: Still,
    pub windows: Vec<WinRect>,
    /// Windows exist but niri did not report where they are (an upstream niri fills
    /// `tile_pos_in_workspace_view` with nothing): Window mode captures the focused window.
    pub windows_unavailable: bool,
    pub selection: Option<Rect>,
    pub drag: Option<Drag>,
    pub pointer: Option<Pt>,
}

impl Session {
    /// The window under the pointer.
    pub fn hover_window(&self) -> Option<&WinRect> {
        window_at(&self.windows, self.pointer?)
    }
}

/// A confirmed capture on its way to the disk or the recorder.
struct Job {
    out: OutputGeom,
    rect: Rect,
    still: Still,
    kind: Kind,
    delay: Delay,
    /// Capture the focused window through niri's `ScreenshotWindow` instead of cropping.
    window_fallback: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Probe {
    Unknown,
    Missing,
    Found(RecorderKind),
}

/// The modal capture overlay and the controller of everything that follows it.
pub struct ScreenshotPanel {
    pub(crate) ctx: PanelContext,
    pub(crate) hud: Entity<Hud>,
    pub(crate) focus: FocusHandle,
    pub(crate) prefs: Prefs,
    pub(crate) session: Option<Session>,
    probe: Probe,
    /// The initial still is being taken (dropping the task cancels the wait).
    capturing: Option<Task<()>>,
    /// Countdown, then the fresh capture / the recorder start.
    delayed: Option<Task<()>>,
    recorder: Option<recorder::Recorder>,
    rec_tick: Option<Task<()>>,
    toast_task: Option<Task<()>>,
    toast_seq: u64,
}

impl Panel for ScreenshotPanel {
    const ID: PanelId = PanelId("screenshot");
    // The overlay draws its own dim (a hole around the selection), so no host scrim.
    const PLACEMENT: Placement = Placement::Modal(ModalStyle { scrim: false });
    /// The overlay is a picture of the screen: it must be there at once and gone at once
    /// (a fading dim would end up in the capture).
    const MOTION: ashd_shell_core::PanelMotion = ashd_shell_core::PanelMotion::Instant;

    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self {
        let hud = hud::shared(cx);
        cx.subscribe(&hud, |this: &mut Self, _, ev: &HudEvent, cx| match ev {
            HudEvent::StopRecording => this.stop_recording(cx),
            HudEvent::CancelCountdown => this.cancel_pending(cx),
        })
        .detach();
        // Which recorder is installed is looked up off the main thread (it stats PATH).
        let probe = cx.background_spawn(async { recorder::detect().map(|(k, _)| k) });
        cx.spawn(async move |this, cx| {
            let found = probe.await;
            this.update(cx, |this, _| this.probe = found.map_or(Probe::Missing, Probe::Found)).ok();
        })
        .detach();
        Self {
            ctx,
            hud,
            focus: cx.focus_handle(),
            prefs: Prefs { mode: CaptureMode::Region, kind: Kind::Shot, delay: Delay::Off },
            session: None,
            probe: Probe::Unknown,
            capturing: None,
            delayed: None,
            recorder: None,
            rec_tick: None,
            toast_task: None,
            toast_seq: 0,
        }
    }

    fn on_request(&mut self, request: &Request, cx: &mut Context<Self>) -> Option<Response> {
        match request {
            Request::Record(RecordOp::Stop) => {
                Some(if self.recorder.is_some() {
                    self.stop_recording(cx);
                    Response::Ok
                } else {
                    Response::error("not recording")
                })
            }
            Request::Record(RecordOp::Toggle) => {
                if self.recorder.is_some() {
                    self.stop_recording(cx);
                    return Some(Response::Ok);
                }
                Some(self.open(None, Some(Kind::Record), PanelOp::Open, cx))
            }
            Request::Screenshot(r) => {
                if r.record && r.op == PanelOp::Toggle && self.recorder.is_some() {
                    self.stop_recording(cx);
                    return Some(Response::Ok);
                }
                // `ashd screenshot` is a Shot unless `--record`: the keybinds must not inherit a
                // Record left over from an earlier use.
                let kind = if r.record { Kind::Record } else { Kind::Shot };
                Some(self.open(r.mode, Some(kind), r.op, cx))
            }
            _ => None,
        }
    }

    fn on_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        if let Some(s) = self.session.as_mut() {
            let p = window.mouse_position();
            s.pointer = Some(Pt::new(f32::from(p.x), f32::from(p.y)));
        }
        cx.notify();
    }

    fn on_close(&mut self, cx: &mut Context<Self>) {
        self.session = None;
        self.capturing = None;
        cx.notify();
    }
}

impl ScreenshotPanel {
    fn modal_open(&self, cx: &gpui::App) -> bool {
        self.ctx.shell.modal(cx).is_some_and(|m| m.panel == Self::ID)
    }

    /// `ashd screenshot …` / `ashd record toggle`: open, re-aim or close the overlay.
    fn open(&mut self, mode: Option<CaptureMode>, kind: Option<Kind>, op: PanelOp, cx: &mut Context<Self>) -> Response {
        let open = self.modal_open(cx) || self.capturing.is_some();
        match (op, open) {
            (PanelOp::Close, _) => {
                self.cancel_pending(cx);
                self.capturing = None;
                self.ctx.shell.close_modal(cx);
                Response::Ok
            }
            (PanelOp::Toggle, true) if mode.is_none_or(|m| m == self.prefs.mode) && kind.is_none_or(|k| k == self.prefs.kind) => {
                self.capturing = None;
                self.ctx.shell.close_modal(cx);
                Response::Ok
            }
            (_, true) => {
                self.apply_prefs(mode, kind, cx);
                Response::Ok
            }
            (_, false) => {
                self.prefs.delay = Delay::Off;
                if kind == Some(Kind::Record) && self.probe == Probe::Missing {
                    let output = self.focused_output_label(cx);
                    self.show_toast(output, failed("recording", NO_RECORDER), cx);
                    return Response::error(NO_RECORDER);
                }
                self.apply_prefs(mode, kind, cx);
                self.begin(cx)
            }
        }
    }

    fn apply_prefs(&mut self, mode: Option<CaptureMode>, kind: Option<Kind>, cx: &mut Context<Self>) {
        if let Some(m) = mode {
            self.set_mode(m, cx);
        }
        if let Some(k) = kind {
            self.prefs.kind = k;
        }
        cx.notify();
    }

    fn focused_output_label(&self, cx: &gpui::App) -> String {
        self.ctx.shell.modal_display(cx).map(|d| d.output_label()).unwrap_or_default()
    }

    // ── taking the still, opening the overlay ─────────────────────────────────

    fn begin(&mut self, cx: &mut Context<Self>) -> Response {
        let niri = self.ctx.niri.read(cx);
        if !niri.connected {
            return Response::error("niri is not connected");
        }
        let Some(display) = self.ctx.shell.modal_display(cx) else { return Response::error("no display to capture") };
        let label = display.output_label();
        let size = display.size();
        let bounds = Dim::new(f32::from(size.width), f32::from(size.height));
        let geom = niri
            .outputs
            .get(&label)
            .and_then(|o| o.logical.map(|l| (l.x, l.y, l.width, l.height, l.scale)))
            .map_or_else(
                || OutputGeom { name: label.clone(), x: 0, y: 0, w: bounds.w as u32, h: bounds.h as u32, scale: 1.0 },
                |(x, y, w, h, scale)| OutputGeom { name: label.clone(), x, y, w, h, scale },
            );
        let active_ws = niri.active_workspace(&label).map(|ws| ws.id);
        let windows = active_ws.map(|ws| windows_in_view(niri.windows.values(), ws, geom.bounds())).unwrap_or_default();
        let windows_unavailable =
            windows.is_empty() && active_ws.is_some_and(|ws| niri.windows.values().any(|w| w.workspace_id == Some(ws)));
        self.clear_toast(cx);
        let handle = self.ctx.niri_handle.clone();
        let id = display.id;
        self.capturing = Some(cx.spawn(async move |this, cx| {
            let res = take_still(&handle, cx).await;
            this.update(cx, |this, cx| this.still_ready(res, id, geom, windows, windows_unavailable, cx)).ok();
        }));
        Response::Ok
    }

    fn still_ready(
        &mut self,
        res: Result<Still, String>,
        display: DisplayId,
        out: OutputGeom,
        windows: Vec<WinRect>,
        windows_unavailable: bool,
        cx: &mut Context<Self>,
    ) {
        self.capturing = None;
        match res {
            Err(e) => self.show_toast(out.name, failed("screenshot failed", &e), cx),
            Ok(still) => {
                let bounds = out.bounds();
                self.session = Some(Session { out, bounds, still, windows, windows_unavailable, selection: None, drag: None, pointer: None });
                self.ctx.shell.open_modal_on(Self::ID, display, None, cx);
            }
        }
    }

    // ── input ─────────────────────────────────────────────────────────────────

    pub(crate) fn set_mode(&mut self, mode: CaptureMode, cx: &mut Context<Self>) {
        self.prefs.mode = mode;
        if let Some(s) = self.session.as_mut() {
            s.drag = None;
        }
        cx.notify();
    }

    pub(crate) fn set_kind(&mut self, kind: Kind, cx: &mut Context<Self>) {
        self.prefs.kind = kind;
        cx.notify();
    }

    pub(crate) fn cycle_delay(&mut self, cx: &mut Context<Self>) {
        self.prefs.delay = self.prefs.delay.next();
        cx.notify();
    }

    pub(crate) fn recorder_missing(&self) -> bool {
        self.probe == Probe::Missing
    }

    pub(crate) fn on_mouse_down(&mut self, ev: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let p = Pt::new(f32::from(ev.position.x), f32::from(ev.position.y));
        if ev.button == MouseButton::Right {
            self.ctx.shell.close_modal(cx);
            return;
        }
        let mode = self.prefs.mode;
        let Some(s) = self.session.as_mut() else { return };
        s.pointer = Some(p);
        match mode {
            CaptureMode::Region => {
                if ev.click_count >= 2 && s.selection.is_some_and(|r| r.contains(p)) {
                    s.drag = None;
                    self.confirm(cx);
                    return;
                }
                let drag = Drag::start(s.selection.filter(Rect::is_usable), p);
                if matches!(drag, Drag::Create { .. }) {
                    s.selection = None;
                }
                s.drag = Some(drag);
            }
            CaptureMode::Window | CaptureMode::Output => self.confirm(cx),
        }
        cx.notify();
    }

    pub(crate) fn on_mouse_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let p = Pt::new(f32::from(ev.position.x), f32::from(ev.position.y));
        let Some(s) = self.session.as_mut() else { return };
        s.pointer = Some(p);
        if let Some(drag) = s.drag {
            if ev.pressed_button == Some(MouseButton::Left) {
                s.selection = Some(drag.apply(p, s.bounds));
            } else {
                // The release happened over something that swallowed it (the toolbar).
                Self::end_drag(s);
            }
        }
        cx.notify();
    }

    pub(crate) fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(s) = self.session.as_mut() {
            Self::end_drag(s);
        }
        cx.notify();
    }

    fn end_drag(s: &mut Session) {
        if s.drag.take().is_some() && !s.selection.is_some_and(|r| r.is_usable()) {
            s.selection = None;
        }
    }

    pub(crate) fn on_key_down(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        let m = k.modifiers;
        let plain = !(m.control || m.alt || m.platform || m.function);
        match k.key.as_str() {
            "enter" => self.confirm(cx),
            "r" if plain => self.set_mode(CaptureMode::Region, cx),
            "w" if plain => self.set_mode(CaptureMode::Window, cx),
            "o" if plain => self.set_mode(CaptureMode::Output, cx),
            "d" if plain => self.cycle_delay(cx),
            "tab" => {
                let kind = if self.prefs.kind == Kind::Shot { Kind::Record } else { Kind::Shot };
                self.set_kind(kind, cx);
            }
            key @ ("left" | "right" | "up" | "down") => {
                let dir = match key {
                    "left" => Dir::Left,
                    "right" => Dir::Right,
                    "up" => Dir::Up,
                    _ => Dir::Down,
                };
                self.nudge_selection(dir, nudge_step(m.shift), m.alt, cx);
            }
            _ => {}
        }
    }

    fn nudge_selection(&mut self, dir: Dir, step: f32, resize: bool, cx: &mut Context<Self>) {
        if self.prefs.mode != CaptureMode::Region {
            return;
        }
        let Some(s) = self.session.as_mut() else { return };
        s.selection = Some(match s.selection.filter(Rect::is_usable) {
            Some(sel) => nudge(sel, dir, step, resize, s.bounds),
            None => default_selection(s.bounds),
        });
        cx.notify();
    }

    // ── confirm ───────────────────────────────────────────────────────────────

    /// Enter / click / double-click: capture what the overlay shows.
    pub(crate) fn confirm(&mut self, cx: &mut Context<Self>) {
        let Some(s) = self.session.as_ref() else { return };
        let rect = match self.prefs.mode {
            CaptureMode::Region => s.selection.filter(Rect::is_usable).unwrap_or(Rect::full(s.bounds)),
            CaptureMode::Output => Rect::full(s.bounds),
            CaptureMode::Window if s.windows_unavailable => Rect::full(s.bounds),
            CaptureMode::Window => match s.hover_window().or_else(|| s.windows.iter().find(|w| w.focused)) {
                Some(w) => w.rect,
                None => return,
            },
        };
        let window_fallback = self.prefs.mode == CaptureMode::Window && s.windows_unavailable;
        let Some(s) = self.session.take() else { return };
        let job = Job { out: s.out, rect, still: s.still, kind: self.prefs.kind, delay: self.prefs.delay, window_fallback };
        self.ctx.shell.close_modal(cx);
        if window_fallback && job.kind == Kind::Record {
            let msg = "this niri does not report window positions; record a region or the output";
            self.show_toast(job.out.name.clone(), failed("recording", msg), cx);
            return;
        }
        self.run(job, cx);
        cx.notify();
    }

    /// Countdown (if any), then the capture or the recorder start.
    fn run(&mut self, job: Job, cx: &mut Context<Self>) {
        let secs = job.delay.secs();
        let record = job.kind == Kind::Record;
        let handle = self.ctx.niri_handle.clone();
        self.delayed = Some(cx.spawn(async move |this, cx| {
            if secs > 0 {
                for left in (1..=secs).rev() {
                    let shown = Countdown { output: job.out.name.clone(), left, record };
                    if this.update(cx, |this, cx| this.set_countdown(Some(shown), cx)).is_err() {
                        return;
                    }
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                }
                if this.update(cx, |this, cx| this.set_countdown(None, cx)).is_err() {
                    return;
                }
            }
            if secs > 0 || record || job.window_fallback {
                cx.background_executor().timer(SETTLE).await;
            }
            let fresh = if job.window_fallback {
                Some(take_window_still(&handle, cx).await)
            } else if !record && secs > 0 {
                Some(take_still(&handle, cx).await)
            } else {
                None
            };
            this.update(cx, |this, cx| {
                this.delayed = None;
                this.fire(job, fresh, cx);
            })
            .ok();
        }));
    }

    fn fire(&mut self, job: Job, fresh: Option<Result<Still, String>>, cx: &mut Context<Self>) {
        match job.kind {
            Kind::Shot => match fresh.unwrap_or(Ok(job.still.clone())) {
                Ok(still) if job.window_fallback => {
                    // the window's own picture: its pixels are the whole selection
                    let size = Dim::new(still.size.0 as f32, still.size.1 as f32);
                    self.save_shot(still, size, Rect::full(size), job.out.name.clone(), cx)
                }
                Ok(still) => self.save_shot(still, job.out.bounds(), job.rect, job.out.name.clone(), cx),
                Err(e) => self.show_toast(job.out.name.clone(), failed("screenshot failed", &e), cx),
            },
            Kind::Record => self.start_recording(&job, cx),
        }
    }

    fn set_countdown(&mut self, c: Option<Countdown>, cx: &mut Context<Self>) {
        self.hud.update(cx, |h, cx| {
            h.countdown = c;
            cx.notify();
        });
        self.ctx.shell.refresh_overlays(cx);
    }

    /// Abort a pending countdown / capture (HUD click, `ashd screenshot --close`).
    fn cancel_pending(&mut self, cx: &mut Context<Self>) {
        if self.delayed.take().is_some() {
            self.set_countdown(None, cx);
        }
    }

    // ── shot ──────────────────────────────────────────────────────────────────

    fn save_shot(&mut self, still: Still, logical: Dim, rect: Rect, name: String, cx: &mut Context<Self>) {
        let task = cx.background_spawn(async move {
            let dir = output::dirs().screenshots;
            output::save_shot(&still, logical, rect, &dir, Local::now().naive_local())
        });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            this.update(cx, |this, cx| match res {
                Ok(saved) => {
                    // wl-copy reads the file we just wrote; niri spawns it (its environment).
                    this.ctx.shell.niri_action(ashd_niri::spawn_sh_action(output::copy_command(&saved.path)), cx);
                    let detail = format!("{} × {} · {}", saved.size.0, saved.size.1, human_size(saved.bytes));
                    this.show_toast(name, ToastBody::Saved { path: saved.path, thumb: saved.thumb, video: false, detail }, cx);
                }
                Err(e) => this.show_toast(name, failed("screenshot failed", &e), cx),
            })
            .ok();
        })
        .detach();
    }

    // ── recording ─────────────────────────────────────────────────────────────

    fn start_recording(&mut self, job: &Job, cx: &mut Context<Self>) {
        let name = job.out.name.clone();
        if self.recorder.is_some() {
            self.show_toast(name, failed("recording", "already recording"), cx);
            return;
        }
        let target = rec_target(&job.out, job.rect);
        let task = cx.background_spawn(async move {
            let (kind, program) = recorder::detect().ok_or_else(|| NO_RECORDER.to_string())?;
            let file = unique_path(&output::dirs().recordings, &recording_file_name(Local::now().naive_local()), |p| p.exists());
            // The shell's own environment (so the recorder talks to the shell's display).
            let env = recorder::recorder_env(std::env::vars());
            let (rec, rx) = recorder::spawn(kind, &program, &target, &file, env)?;
            Ok::<_, String>((rec, rx))
        });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            this.update(cx, |this, cx| this.recording_started(res, name, cx)).ok();
        })
        .detach();
    }

    fn recording_started(
        &mut self,
        res: Result<(recorder::Recorder, async_channel::Receiver<recorder::Exit>), String>,
        output: String,
        cx: &mut Context<Self>,
    ) {
        let (rec, rx) = match res {
            Ok(r) => r,
            Err(e) => {
                if e == NO_RECORDER {
                    self.probe = Probe::Missing;
                }
                self.show_toast(output, failed("recording", &e), cx);
                return;
            }
        };
        self.probe = Probe::Found(rec.kind);
        tracing::info!("recording started on {output} ({:?})", rec.kind);
        self.recorder = Some(rec);
        let started = Instant::now();
        self.hud.update(cx, |h, cx| {
            h.recording = Some(Recording { output: output.clone(), started, stopping: false });
            cx.notify();
        });
        self.ctx.shell.refresh_overlays(cx);
        let hud = self.hud.clone();
        self.rec_tick = Some(cx.spawn(async move |this, cx| {
            loop {
                let wait = hud.update(cx, |h, cx| {
                    cx.notify();
                    let ms = h.recording.as_ref().map_or(0, |r| r.started.elapsed().as_millis() as u64);
                    // next blink step (every 600 ms) or next clock tick (every second)
                    (600 - ms % 600).min(1000 - ms % 1000).max(15)
                });
                if this.update(cx, |_, _| ()).is_err() {
                    return;
                }
                cx.background_executor().timer(Duration::from_millis(wait)).await;
            }
        }));
        cx.spawn(async move |this, cx| {
            if let Ok(exit) = rx.recv().await {
                this.update(cx, |this, cx| this.recording_exited(exit, output, started, cx)).ok();
            }
        })
        .detach();
    }

    fn stop_recording(&mut self, cx: &mut Context<Self>) {
        let Some(rec) = &self.recorder else { return };
        rec.stop();
        self.hud.update(cx, |h, cx| {
            if let Some(r) = h.recording.as_mut() {
                r.stopping = true;
            }
            cx.notify();
        });
    }

    fn recording_exited(&mut self, exit: recorder::Exit, output: String, started: Instant, cx: &mut Context<Self>) {
        tracing::info!(
            "recorder exited: requested={} success={} code={:?} bytes={} file={} stderr={:?}",
            exit.requested,
            exit.success,
            exit.code,
            exit.bytes,
            exit.file.display(),
            exit.stderr
        );
        self.recorder = None;
        self.rec_tick = None;
        self.hud.update(cx, |h, cx| {
            h.recording = None;
            cx.notify();
        });
        if exit.has_video() && (exit.requested || exit.success) {
            let detail = format!("{} · {}", capture::clock(started.elapsed().as_secs()), human_size(exit.bytes));
            self.show_toast(output, ToastBody::Saved { path: exit.file, thumb: None, video: true, detail }, cx);
        } else {
            let why = recorder::error_line(&exit.stderr, exit.code);
            self.show_toast(output, failed("recording failed", &why), cx);
        }
        self.ctx.shell.refresh_overlays(cx);
    }

    // ── toasts ────────────────────────────────────────────────────────────────

    fn show_toast(&mut self, output: String, body: ToastBody, cx: &mut Context<Self>) {
        self.toast_seq += 1;
        let id = self.toast_seq;
        let hold = if matches!(body, ToastBody::Failed { .. }) { TOAST_HOLD_ERROR } else { TOAST_HOLD };
        self.hud.update(cx, |h, cx| {
            h.toast = Some(Toast { id, output, body, hovered: false });
            cx.notify();
        });
        self.ctx.shell.refresh_overlays(cx);
        let hud = self.hud.clone();
        // Replacing the task cancels the previous toast's timer.
        self.toast_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(hold).await;
            // The pointer on the toast keeps it up.
            while hud.read_with(cx, |h, _| h.toast.as_ref().is_some_and(|t| t.id == id && t.hovered)) {
                cx.background_executor().timer(Duration::from_millis(400)).await;
            }
            this.update(cx, |this, cx| {
                if this.hud.read(cx).toast.as_ref().is_some_and(|t| t.id == id) {
                    this.clear_toast(cx);
                }
            })
            .ok();
        }));
    }

    fn clear_toast(&mut self, cx: &mut Context<Self>) {
        self.toast_task = None;
        if self.hud.read(cx).toast.is_some() {
            self.hud.update(cx, |h, cx| {
                h.toast = None;
                cx.notify();
            });
            self.ctx.shell.refresh_overlays(cx);
        }
    }
}

const NO_RECORDER: &str = "no screen recorder installed";

fn failed(title: &str, message: &str) -> ToastBody {
    ToastBody::Failed { title: title.into(), message: message.into() }
}

fn human_size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / f64::from(1 << 20)),
        b if b >= 1 << 10 => format!("{:.0} kB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

/// Ask niri for a still of the focused output (no pointer) and wait for the file.
async fn take_still(handle: &NiriHandle, cx: &AsyncApp) -> Result<Still, String> {
    // Internal stills: never on the clipboard, no niri notification (ashd fork fields).
    grab(handle, cx, |path| Action::ScreenshotScreen {
        write_to_disk: true,
        show_pointer: false,
        path: Some(path),
        copy_to_clipboard: false,
        notify: false,
    })
    .await
}

/// The focused window on its own (niri renders just that window).
async fn take_window_still(handle: &NiriHandle, cx: &AsyncApp) -> Result<Still, String> {
    grab(handle, cx, |path| Action::ScreenshotWindow {
        id: None,
        write_to_disk: true,
        show_pointer: false,
        path: Some(path),
        copy_to_clipboard: false,
        notify: false,
    })
        .await
}

/// Run a niri screenshot action that writes to a fresh file under the runtime dir, and wait
/// for the decoded result.
async fn grab(handle: &NiriHandle, cx: &AsyncApp, action: impl FnOnce(String) -> Action) -> Result<Still, String> {
    let path = output::still_path();
    let dir = output::runtime_dir();
    cx.background_executor()
        .spawn(async move {
            let _ = std::fs::create_dir_all(&dir);
            output::sweep_stale(&dir);
        })
        .await;
    handle.action(action(path.to_string_lossy().into_owned())).await.map_err(|e| e.to_string())?;
    cx.background_executor().spawn(async move { output::wait_for_still(&path, STILL_TIMEOUT) }).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::mode_from_index;

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(20 * 1024), "20 kB");
        assert_eq!(human_size(3 * 1024 * 1024 + 512 * 1024), "3.5 MB");
    }

    #[test]
    fn mode_indices_match_the_segments() {
        assert_eq!(mode_from_index(0), CaptureMode::Region);
    }
}
