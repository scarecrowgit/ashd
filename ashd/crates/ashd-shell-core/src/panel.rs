//! The `Panel` contract: how a panel crate plugs into the shell.
//!
//! A panel is a GPUI view (`Render`) plus this trait. `ashd-shell` registers every panel
//! type once (`reg.register::<RailPanel>()`); shell-core then creates instances:
//!
//! | placement | instances | window |
//! |---|---|---|
//! | `Frame(slot)` | one per display | that display's rail / bar / scratchpad surface |
//! | `Passive` | one per display | that display's passive overlay (created while any passive panel `is_live`) |
//! | `Modal(_)` | one, app-wide | the modal overlay of the focused display, only while it is the open modal |
//!
//! Requests from the CLI are offered to modal panels and to the instances on the focused
//! display, in registration order; the first `Some(response)` wins.

use std::{any::Any, fmt, rc::Rc};

use gpui::{AnyView, App, AppContext as _, Context, Entity, Render, Window};
use ashd_ipc::{Request, Response};

use crate::{DisplayInfo, ShellHandle, shell::Services};

/// Stable panel id (`"launcher"`, `"quick-settings"`). Also what `ashd status` reports
/// as the open modal.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct PanelId(pub &'static str);

impl fmt::Display for PanelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// Region of a frame surface. Panels in the same slot are laid out in registration order.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Slot {
    /// Left rail surface (RAIL_W, anchored top|bottom|left).
    Rail,
    /// Bottom bar, left block (status bar, STATUS_W).
    BarStart,
    /// Bottom bar, middle (flex-1; the dock centres itself in it).
    BarCenter,
    /// Bottom bar, right block (workspace label + Scroll|Canvas).
    BarEnd,
    /// Right scratchpad surface (SCRATCH_W, anchored top|bottom|right).
    Scratchpad,
}

/// How the modal overlay presents a modal panel.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ModalStyle {
    /// Dim everything behind (60 % `scrim_power`): launcher, session, polkit, dialogs.
    /// Popups growing out of the bar (quick settings, calendar, …) use `false`.
    pub scrim: bool,
}

/// How the overlay host moves a modal panel in and out (ARCHITECTURE.md "Overlay host
/// motion"). The host does it for every modal; a panel only says when the default is
/// wrong, by setting [`Panel::MOTION`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PanelMotion {
    /// Dialogs (with a scrim) and the launcher class fade in 140 ms (out-expo) and out
    /// 100 ms (out-quad) together with their scrim; popups without a scrim (they grow
    /// out of the bar) do the same in 120 / 100 ms and travel 4 px.
    #[default]
    Auto,
    /// Fade only, never travel.
    Fade,
    /// Appears and disappears at once: panels that are a *picture of the screen* (the
    /// screenshot overlay must be gone before the capture).
    Instant,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Placement {
    Frame(Slot),
    Passive,
    Modal(ModalStyle),
}

/// Where a popup should attach, in output-local logical px (e.g. the bounds of the bar
/// button that opened it, converted with `DisplayInfo::bar_to_output`). Modal panels read
/// it via `ShellHandle::modal_anchor`.
pub type ModalAnchor = gpui::Bounds<gpui::Pixels>;

/// What a panel gets at construction. Cheap to clone; keep it in your struct.
#[derive(Clone)]
pub struct PanelContext {
    /// Shell-level operations (open/close modals, spawn apps, niri actions …).
    pub shell: ShellHandle,
    /// Service states (entities, observe them) and handles (send commands).
    pub services: Services,
    /// niri state (observe it) — also reachable as `services.niri`.
    pub niri: Entity<ashd_niri::NiriState>,
    pub niri_handle: ashd_niri::NiriHandle,
    /// The display this instance lives on. `None` for modal panels (they move to the
    /// focused display; use `shell.modal_display(cx)`).
    pub display: Option<DisplayInfo>,
}

pub trait Panel: Render + Sized + 'static {
    const ID: PanelId;
    const PLACEMENT: Placement;

    /// Modal popups: named anchors (`crate::anchors::names`) this panel grows from when it
    /// is opened without an explicit anchor (keybind, `ashd …`); the first one a frame
    /// panel published on the target display wins. Panels keep honouring an explicit anchor.
    const ANCHORS: &'static [&'static str] = &[];

    /// Modal panels: how the overlay host moves the panel in and out. The default fits
    /// every popup and dialog; see [`PanelMotion`].
    const MOTION: PanelMotion = PanelMotion::Auto;

    /// Build the panel. No `Window` here: modal panels are created at startup before any
    /// overlay window exists. Subscribe to what you render:
    /// `cx.observe(&ctx.services.sysinfo.state, |_, _, cx| cx.notify()).detach();`
    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self;

    /// Claim an IPC request. Return `None` to let the next panel see it.
    fn on_request(&mut self, _request: &Request, _cx: &mut Context<Self>) -> Option<Response> {
        None
    }

    /// Passive panels: whether anything is on screen right now. The passive overlay of a
    /// display exists only while at least one of its passive panels is live; call
    /// `ctx.shell.refresh_overlays(cx)` after a change that flips this.
    fn is_live(&self, _cx: &App) -> bool {
        false
    }

    /// Modal panels: the modal overlay just started showing this panel. Focus your
    /// `TextInput` here.
    fn on_open(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    /// Modal panels: dismissed (outside click, Escape, `ashd … --close`, another modal
    /// opened, `Request::Dismiss`). Reset transient state here.
    ///
    /// The surface fades out first (≤ 100 ms) and stays drawn from the panel's state as it
    /// was, so this runs when that exit is over, not at the moment of dismissal (at once
    /// for [`PanelMotion::Instant`]). If the same panel is opened again while it leaves,
    /// the exit is reversed and neither `on_close` nor a second construction happens:
    /// only `on_open` runs again.
    fn on_close(&mut self, _cx: &mut Context<Self>) {}
}

// ── type-erased registry ──────────────────────────────────────────────────────

/// A live panel instance, type-erased.
#[derive(Clone)]
pub struct PanelInstance {
    pub id: PanelId,
    pub placement: Placement,
    /// [`Panel::ANCHORS`].
    pub anchors: &'static [&'static str],
    /// [`Panel::MOTION`].
    pub motion: PanelMotion,
    pub view: AnyView,
    pub(crate) vtable: Rc<dyn PanelVTable>,
    pub(crate) entity: Rc<dyn Any>,
}

pub(crate) trait PanelVTable {
    fn on_request(&self, entity: &dyn Any, req: &Request, cx: &mut App) -> Option<Response>;
    fn is_live(&self, entity: &dyn Any, cx: &App) -> bool;
    fn on_open(&self, entity: &dyn Any, window: &mut Window, cx: &mut App);
    fn on_close(&self, entity: &dyn Any, cx: &mut App);
}

struct VTable<P>(std::marker::PhantomData<P>);

impl<P: Panel> PanelVTable for VTable<P> {
    fn on_request(&self, entity: &dyn Any, req: &Request, cx: &mut App) -> Option<Response> {
        let e = entity.downcast_ref::<Entity<P>>()?;
        e.update(cx, |p, cx| p.on_request(req, cx))
    }
    fn is_live(&self, entity: &dyn Any, cx: &App) -> bool {
        entity.downcast_ref::<Entity<P>>().is_some_and(|e| e.read(cx).is_live(cx))
    }
    fn on_open(&self, entity: &dyn Any, window: &mut Window, cx: &mut App) {
        if let Some(e) = entity.downcast_ref::<Entity<P>>() {
            e.update(cx, |p, cx| p.on_open(window, cx));
        }
    }
    fn on_close(&self, entity: &dyn Any, cx: &mut App) {
        if let Some(e) = entity.downcast_ref::<Entity<P>>() {
            e.update(cx, |p, cx| p.on_close(cx));
        }
    }
}

impl PanelInstance {
    pub fn on_request(&self, req: &Request, cx: &mut App) -> Option<Response> {
        self.vtable.on_request(self.entity.as_ref(), req, cx)
    }
    pub fn is_live(&self, cx: &App) -> bool {
        self.vtable.is_live(self.entity.as_ref(), cx)
    }
    pub fn on_open(&self, window: &mut Window, cx: &mut App) {
        self.vtable.on_open(self.entity.as_ref(), window, cx)
    }
    pub fn on_close(&self, cx: &mut App) {
        self.vtable.on_close(self.entity.as_ref(), cx)
    }
    /// The concrete entity, if this instance is a `P`.
    pub fn downcast<P: Panel>(&self) -> Option<Entity<P>> {
        self.entity.downcast_ref::<Entity<P>>().cloned()
    }
}

type Factory = Rc<dyn Fn(PanelContext, &mut App) -> PanelInstance>;

/// Filled by `ashd-shell`'s `main` (the only place that knows every panel crate).
#[derive(Clone, Default)]
pub struct PanelRegistry {
    pub(crate) entries: Vec<(PanelId, Placement, Factory)>,
}

impl PanelRegistry {
    pub fn register<P: Panel>(&mut self) -> &mut Self {
        assert!(
            !self.entries.iter().any(|(id, _, _)| *id == P::ID),
            "panel id `{}` registered twice",
            P::ID
        );
        let factory: Factory = Rc::new(|ctx, cx: &mut App| {
            let entity: Entity<P> = cx.new(|cx| P::new(ctx, cx));
            PanelInstance {
                id: P::ID,
                placement: P::PLACEMENT,
                anchors: P::ANCHORS,
                motion: P::MOTION,
                view: entity.clone().into(),
                vtable: Rc::new(VTable::<P>(std::marker::PhantomData)),
                entity: Rc::new(entity),
            }
        });
        self.entries.push((P::ID, P::PLACEMENT, factory));
        self
    }

    pub fn ids(&self) -> impl Iterator<Item = (PanelId, Placement)> + '_ {
        self.entries.iter().map(|(id, p, _)| (*id, *p))
    }

    pub(crate) fn build(&self, filter: impl Fn(Placement) -> bool, ctx: &PanelContext, cx: &mut App) -> Vec<PanelInstance> {
        self.entries.iter().filter(|(_, p, _)| filter(*p)).map(|(_, _, f)| f(ctx.clone(), cx)).collect()
    }
}
