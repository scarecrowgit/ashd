//! GPUI elements that give the kit its small motions. They keep their own state in GPUI's
//! per-element state, so using them needs no field in a panel and no change in a signature:
//!
//! - [`hover_wash`]: an absolutely positioned layer that eases the hover / pressed fill of
//!   its parent in and out (80 ms), instead of the instant `.hover(..)` style.
//! - [`flip`]: wraps an element so that when *layout* moves it (a sibling above was added or
//!   removed, the list reflowed) it glides from the old position to the new one on the view
//!   spring instead of jumping.
//! - [`fade_slide`]: opacity + a small travel for things that appear and leave.
//!
//! - [`follow`]: an element built from a value that eases to its target when the target
//!   changes (meter / progress / slider fills, the toggle knob, an indicator): the first
//!   frame shows the target, a later change animates, a settled one costs nothing.
//!
//! All of them are time-based (the clock, not the frame count) and ask for frames only
//! while something moves, so a settled UI draws nothing.

use std::{cell::Cell, rc::Rc, time::Instant};

use gpui::{
    AnyElement, App, Bounds, Corners, Element, ElementId, GlobalElementId, Hitbox, HitboxBehavior, Hsla,
    InspectorElementId, IntoElement, LayoutId, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels,
    Point, StyleRefinement, Styled, Style, Window, fill, point, px,
};

use crate::{
    anim::{SPRING_VIEW, SpringValue, motion},
    presence::{Timing, Tween},
};

fn with_alpha(c: Hsla, f: f32) -> Hsla {
    Hsla { a: c.a * f.clamp(0.0, 1.0), ..c }
}

// ── hover wash ────────────────────────────────────────────────────────────────

/// The hover / pressed fill of a control, eased. Put it as the *first child* of the control
/// (so the content paints over it) and give the control `.relative()` if it is not already
/// positioned; remove the control's own `.hover(|s| s.bg(..))`.
///
/// `radius` is the control's inner corner radius. `id` must be unique among the control's
/// children (a constant string is fine).
pub fn hover_wash(hover: Hsla, press: Option<Hsla>, radius: Pixels) -> HoverWash {
    HoverWash {
        id: ElementId::Name("ashd-hover-wash".into()),
        hover,
        press,
        radius,
        ring: None,
        style: StyleRefinement::default(),
    }
    .absolute()
    .size_full()
    .top_0()
    .left_0()
}

/// Like [`hover_wash`] but eases a `width` px outline in `color` over the control's own
/// border (hover: the accent border of a card). `radius` is the control's outer radius.
pub fn hover_ring(color: Hsla, width: Pixels, radius: Pixels) -> HoverWash {
    HoverWash {
        id: ElementId::Name("ashd-hover-ring".into()),
        hover: color,
        press: None,
        radius,
        ring: Some(width),
        style: StyleRefinement::default(),
    }
    .absolute()
    .top(-width)
    .left(-width)
    .right(-width)
    .bottom(-width)
}

pub struct HoverWash {
    id: ElementId,
    hover: Hsla,
    press: Option<Hsla>,
    radius: Pixels,
    ring: Option<Pixels>,
    style: StyleRefinement,
}

impl Styled for HoverWash {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

struct WashState {
    hover: Tween,
    press: Tween,
    /// Set by the mouse-down / mouse-up listeners.
    pressed: Rc<Cell<bool>>,
}

impl WashState {
    fn new(now: Instant) -> Self {
        Self { hover: Tween::new(0.0, now), press: Tween::new(0.0, now), pressed: Rc::new(Cell::new(false)) }
    }
}

const WASH: Timing = Timing::new(motion::timing::HOVER, motion::EASE_STANDARD);

impl Element for HoverWash {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        use gpui::Refineable as _;
        let mut style = Style::default();
        style.refine(&self.style);
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        _: &mut App,
    ) -> Hitbox {
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        hitbox: &mut Hitbox,
        window: &mut Window,
        _: &mut App,
    ) {
        let Some(id) = id else { return };
        let now = Instant::now();
        let hovered = hitbox.is_hovered(window);
        let (hover_p, press_p, pressed_flag, animating) = window.with_element_state(id, |state: Option<WashState>, _| {
            let mut s = state.unwrap_or_else(|| WashState::new(now));
            let press_target = if hovered && s.pressed.get() { 1.0 } else { 0.0 };
            s.hover.go(if hovered { 1.0 } else { 0.0 }, WASH, 1.0, now);
            s.press.go(press_target, WASH, 1.0, now);
            let animating = s.hover.is_animating(now) || s.press.is_animating(now);
            ((s.hover.value(now), s.press.value(now), s.pressed.clone(), animating), s)
        });
        if animating {
            window.request_animation_frame();
        }

        let radii = Corners::all(self.radius);
        if hover_p > 0.0 {
            let color = with_alpha(self.hover, hover_p);
            match self.ring {
                Some(w) => window.paint_quad(
                    fill(bounds, gpui::transparent_black()).corner_radii(radii).border_widths(w).border_color(color),
                ),
                None => window.paint_quad(fill(bounds, color).corner_radii(radii)),
            }
        }
        if let (Some(press), true) = (self.press, press_p > 0.0) {
            window.paint_quad(fill(bounds, with_alpha(press, press_p)).corner_radii(radii));
        }

        // Hover / press changes must reach the next frame: the element itself is not a view.
        let was_hovered = hovered;
        let flag = pressed_flag.clone();
        let hb = hitbox.clone();
        window.on_mouse_event(move |_: &MouseMoveEvent, _, window, _| {
            if hb.is_hovered(window) != was_hovered {
                window.refresh();
            }
        });
        let (flag_down, hb_down) = (flag.clone(), hitbox.clone());
        window.on_mouse_event(move |ev: &MouseDownEvent, phase, window, _| {
            if phase == gpui::DispatchPhase::Capture && ev.button == gpui::MouseButton::Left && hb_down.is_hovered(window) {
                flag_down.set(true);
                window.refresh();
            }
        });
        window.on_mouse_event(move |ev: &MouseUpEvent, phase, window, _| {
            if phase == gpui::DispatchPhase::Capture && ev.button == gpui::MouseButton::Left && flag.get() {
                flag.set(false);
                window.refresh();
            }
        });
    }
}

// ── flip (layout reflow) ──────────────────────────────────────────────────────

/// Layout moves smaller than this (px) are not animated (rounding noise).
const FLIP_EPS: f32 = 0.5;

/// Wrap `child` so that a change of its *layout* position glides on the view spring
/// (damping 1, stiffness 800) instead of jumping. `id` must be unique among its siblings
/// (a key of the data, not the index). The first frame never animates; scrolling an
/// ancestor does not count as a move.
pub fn flip<E: IntoElement + 'static>(id: impl Into<ElementId>, child: E) -> Flip<E> {
    Flip { id: id.into(), child: Some(child) }
}

pub struct Flip<E> {
    id: ElementId,
    child: Option<E>,
}

struct FlipState {
    /// Layout origin last frame, minus the scroll offset of the ancestors.
    last: Option<Point<Pixels>>,
    x: SpringValue,
    y: SpringValue,
}

impl<E: IntoElement + 'static> Element for Flip<E> {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, AnyElement) {
        let mut el = self.child.take().expect("request_layout runs once").into_any_element();
        (el.request_layout(window, cx), el)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        el: &mut AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(id) = id else {
            el.prepaint(window, cx);
            return;
        };
        let now = Instant::now();
        let origin = bounds.origin - window.element_offset();
        let off = window.with_element_state(id, |state: Option<FlipState>, _| {
            let mut s = state.unwrap_or_else(|| FlipState {
                last: None,
                x: SpringValue::new(0.0, SPRING_VIEW),
                y: SpringValue::new(0.0, SPRING_VIEW),
            });
            if let Some(last) = s.last {
                let (dx, dy) = (f32::from(last.x - origin.x), f32::from(last.y - origin.y));
                if dx.abs() > FLIP_EPS || dy.abs() > FLIP_EPS {
                    s.x.shift_at(dx, now);
                    s.y.shift_at(dy, now);
                }
            }
            s.last = Some(origin);
            let moving = !s.x.is_settled_at(now) || !s.y.is_settled_at(now);
            // Settled = exactly in place (no leftover fraction of a pixel).
            let off = if moving { point(px(s.x.value_at(now)), px(s.y.value_at(now))) } else { point(px(0.), px(0.)) };
            ((off, moving), s)
        });
        let (off, moving) = off;
        if moving {
            window.request_animation_frame();
        }
        window.with_element_offset(off, |window| el.prepaint(window, cx));
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        el: &mut AnyElement,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        el.paint(window, cx);
    }
}

impl<E> ParentElement for Flip<E>
where
    E: ParentElement,
{
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        if let Some(c) = &mut self.child {
            c.extend(elements);
        }
    }
}

// ── follow (a value that eases to its target) ─────────────────────────────────

/// Linear mix of two colours in sRGB (`t` in 0..=1): the hover / state colour tweens.
pub fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let (mut a, mut b): (gpui::Rgba, gpui::Rgba) = (a.into(), b.into());
    // A fully transparent end has no colour of its own: fade to / from the other one.
    if a.a == 0.0 {
        a = gpui::Rgba { a: 0.0, ..b };
    } else if b.a == 0.0 {
        b = gpui::Rgba { a: 0.0, ..a };
    }
    let t = t.clamp(0.0, 1.0);
    gpui::Rgba { r: a.r + (b.r - a.r) * t, g: a.g + (b.g - a.g) * t, b: a.b + (b.b - a.b) * t, a: a.a + (b.a - a.a) * t }.into()
}

/// Build an element from `target`, eased: when `target` changes between frames the value
/// handed to `build` glides to it with `timing` (a leg shorter than `range` takes
/// proportionally less time). The first frame uses `target` as is. `id` must be unique among
/// its siblings. Under reduced motion position / size values jump; [`Follow::fade`] marks
/// opacity / colour values, which keep their (80 ms) fade.
pub fn follow<E: IntoElement + 'static>(
    id: impl Into<ElementId>,
    target: f32,
    timing: Timing,
    build: impl FnOnce(f32) -> E + 'static,
) -> Follow<E> {
    Follow { id: id.into(), target, range: 1.0, timing, fade: false, immediate: false, build: Some(Box::new(build)) }
}

pub struct Follow<E> {
    id: ElementId,
    target: f32,
    range: f32,
    timing: Timing,
    fade: bool,
    immediate: bool,
    build: Option<Box<dyn FnOnce(f32) -> E>>,
}

impl<E> Follow<E> {
    /// Take the target at once while `yes` (a slider being dragged: no lag behind the
    /// pointer).
    pub fn immediate(mut self, yes: bool) -> Self {
        self.immediate = yes;
        self
    }

    /// The value is an opacity / colour mix (keeps the 80 ms fade under reduced motion).
    pub fn fade(mut self) -> Self {
        self.fade = true;
        self
    }

    /// The distance a full-length leg covers (default 1: values in 0..=1).
    pub fn range(mut self, range: f32) -> Self {
        self.range = range;
        self
    }
}

impl<E: IntoElement + 'static> Element for Follow<E> {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, AnyElement) {
        let now = Instant::now();
        let (value, animating) = match id {
            Some(id) => window.with_element_state(id, |state: Option<Tween>, _| {
                let mut t = state.unwrap_or_else(|| Tween::new(self.target, now));
                if self.immediate || (!self.fade && crate::anim::reduced_motion()) {
                    t.snap(self.target, now);
                } else {
                    t.go(self.target, self.timing, self.range, now);
                }
                ((t.value(now), t.is_animating(now)), t)
            }),
            None => (self.target, false),
        };
        if animating {
            window.request_animation_frame();
        }
        let mut el = (self.build.take().expect("request_layout runs once"))(value).into_any_element();
        (el.request_layout(window, cx), el)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        el: &mut AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) {
        el.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        el: &mut AnyElement,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        el.paint(window, cx);
    }
}

/// Colours of a control in one state, for [`state_layer`].
#[derive(Clone, Copy, Debug)]
pub struct Look {
    pub fill: Hsla,
    pub border: Hsla,
}

impl Look {
    pub fn new(fill: Hsla, border: Hsla) -> Self {
        Self { fill, border }
    }
}

/// The fill and 1px border of a control that has two states (a tile on / off, the selected
/// radio row): a layer under the content, covering the control's border box, whose colours
/// cross-fade (120 ms) when `on` flips. Give the control a transparent 1px border (so the
/// content keeps its place) and put this layer first among its children.
pub fn state_layer(id: impl Into<ElementId>, on: bool, off: Look, active: Look, radius: Pixels) -> Follow<gpui::Div> {
    follow(
        id,
        if on { 1.0 } else { 0.0 },
        Timing::new(motion::timing::SWAP, motion::EASE_STANDARD),
        move |p| {
            gpui::div()
                .absolute()
                .top(px(-1.))
                .left(px(-1.))
                .right(px(-1.))
                .bottom(px(-1.))
                .border_1()
                .border_color(mix(off.border, active.border, p))
                .bg(mix(off.fill, active.fill, p))
                .rounded(radius)
        },
    )
    .fade()
}

/// The highlight of a selected row (keyboard selection, hovered-selects menus): a `fill`
/// plate and, optionally, a 2 px `bar` on the left edge, both fading (120 ms) as `selected`
/// flips. The layer covers the row's padding box; put it first among the row's children and
/// give the row a transparent `border_l_2` if the bar is used.
pub fn select_plate(id: impl Into<ElementId>, selected: bool, fill: Hsla, bar: Option<Hsla>) -> Follow<gpui::Div> {
    follow(
        id,
        if selected { 1.0 } else { 0.0 },
        Timing::new(motion::timing::SWAP, motion::EASE_STANDARD),
        move |p| {
            let clear = gpui::transparent_black();
            let mut d = gpui::div().absolute().top_0().left_0().size_full().bg(mix(clear, fill, p));
            if let Some(bar) = bar {
                d = d.child(
                    gpui::div().absolute().top(px(0.)).bottom(px(0.)).left(px(-2.)).w(px(2.)).bg(mix(clear, bar, p)),
                );
            }
            d
        },
    )
    .fade()
}

// ── swap (a value that changes cross-fades) ───────────────────────────────────

/// An element built from `p` that dips and comes back (from [`SWAP_FROM`] to 1, 120 ms) each
/// time `key` changes between frames: a clock minute, a workspace name, a speed readout.
/// The first frame (and a new element) shows the final look. Use `p` for opacity.
pub fn swap<E: IntoElement + 'static>(
    id: impl Into<ElementId>,
    key: impl std::hash::Hash,
    build: impl FnOnce(f32) -> E + 'static,
) -> Swap<E> {
    use std::hash::Hasher as _;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut h);
    Swap { id: id.into(), key: h.finish(), build: Some(Box::new(build)) }
}

/// Opacity a swapped value starts from.
pub const SWAP_FROM: f32 = 0.3;

pub struct Swap<E> {
    id: ElementId,
    key: u64,
    build: Option<Box<dyn FnOnce(f32) -> E>>,
}

struct SwapState {
    key: u64,
    tween: Tween,
}

impl<E: IntoElement + 'static> Element for Swap<E> {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, AnyElement) {
        let now = Instant::now();
        let (p, animating) = match id {
            Some(id) => window.with_element_state(id, |state: Option<SwapState>, _| {
                let mut s = state.unwrap_or_else(|| SwapState { key: self.key, tween: Tween::new(1.0, now) });
                if s.key != self.key {
                    s.key = self.key;
                    s.tween.snap(SWAP_FROM, now);
                    s.tween.go(1.0, Timing::new(motion::timing::SWAP, motion::EASE_STANDARD), 1.0, now);
                }
                ((s.tween.value(now), s.tween.is_animating(now)), s)
            }),
            None => (1.0, false),
        };
        if animating {
            window.request_animation_frame();
        }
        let mut el = (self.build.take().expect("request_layout runs once"))(p).into_any_element();
        (el.request_layout(window, cx), el)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        el: &mut AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) {
        el.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        el: &mut AnyElement,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        el.paint(window, cx);
    }
}

// ── fill bar (meters, progress) ───────────────────────────────────────────────

/// The fill of a meter / progress bar: it fills `value` (0..=1) of the width of the box it
/// is placed in (`size_full`) and eases there (160 ms standard) when the value changes. The
/// bar paints its own quad, so it needs no id of its own: each instance keeps its state under
/// its position (two meters side by side, with no element id between them and the nearest
/// ancestor that has one, cannot share a state), and a bar that appears or moves shows its
/// value at once.
pub fn fill_bar(value: f32, color: Hsla, radius: Pixels) -> FillBar {
    FillBar { value: value.clamp(0.0, 1.0), color, radius, style: StyleRefinement::default() }.size_full()
}

pub struct FillBar {
    value: f32,
    color: Hsla,
    radius: Pixels,
    style: StyleRefinement,
}

impl Styled for FillBar {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

type FillStates = std::collections::HashMap<(i32, i32), Tween>;

const FILL: Timing = Timing::new(motion::timing::VALUE, motion::EASE_STANDARD);

impl Element for FillBar {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(ElementId::Name("ashd-fill-bar".into()))
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        use gpui::Refineable as _;
        let mut style = Style::default();
        style.refine(&self.style);
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut Window,
        _: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        _: &mut App,
    ) {
        let now = Instant::now();
        let key = (f32::from(bounds.origin.x).round() as i32, f32::from(bounds.origin.y).round() as i32);
        let (v, animating) = match id {
            Some(id) => window.with_element_state(id, |state: Option<FillStates>, _| {
                let mut map = state.unwrap_or_default();
                if map.len() > 16 {
                    map.clear();
                }
                let t = map.entry(key).or_insert_with(|| Tween::new(self.value, now));
                if crate::anim::reduced_motion() {
                    t.snap(self.value, now);
                } else {
                    t.go(self.value, FILL, 1.0, now);
                }
                ((t.value(now), t.is_animating(now)), map)
            }),
            None => (self.value, false),
        };
        if animating {
            window.request_animation_frame();
        }
        if v > 0.0 {
            let w = bounds.size.width * v;
            let filled = Bounds { origin: bounds.origin, size: gpui::size(w, bounds.size.height) };
            window.paint_quad(fill(filled, self.color).corner_radii(Corners::all(self.radius)));
        }
    }
}

// ── reveal (height) ───────────────────────────────────────────────────────────

/// An expanding / collapsing block: its height is `p` of the content's natural height
/// (clipped), `p` coming from the caller's [`Presence`](crate::presence::Presence) (so the
/// caller also knows when it is gone and stops building it). `build` gets `p` as well, for
/// the content's own fade (`.opacity(p)`). The natural height is measured while the content
/// is laid out; the first frame of an opening is collapsed (height 0) until it is known.
pub fn reveal<E: IntoElement + 'static>(
    id: impl Into<ElementId>,
    p: f32,
    build: impl FnOnce(f32) -> E + 'static,
) -> Reveal<E> {
    Reveal { id: id.into(), p: p.clamp(0.0, 1.0), build: Some(Box::new(build)) }
}

pub struct Reveal<E> {
    id: ElementId,
    p: f32,
    build: Option<Box<dyn FnOnce(f32) -> E>>,
}

pub struct RevealLayout {
    child: AnyElement,
    child_layout: LayoutId,
    clipped: bool,
}

impl<E: IntoElement + 'static> Element for Reveal<E> {
    type RequestLayoutState = RevealLayout;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, RevealLayout) {
        let p = if crate::anim::reduced_motion() { self.p.round() } else { self.p };
        let known = id.and_then(|id| window.with_element_state(id, |s: Option<Pixels>, _| (s, s.unwrap_or(px(0.)))));
        let mut child = (self.build.take().expect("request_layout runs once"))(p).into_any_element();
        let child_layout = child.request_layout(window, cx);
        let mut style = Style::default();
        let clipped = p < 1.0;
        if clipped {
            style.overflow = Point { x: gpui::Overflow::Clip, y: gpui::Overflow::Clip };
            style.flex_shrink = 0.0;
            // Unknown natural height (first frame): collapsed, then measured in prepaint.
            let h = known.map_or(0.0, |h| f32::from(h) * p);
            style.size.height = gpui::Length::Definite(px(h).into());
        }
        let layout = window.request_layout(style, [child_layout], cx);
        if clipped && known.is_none() {
            window.request_animation_frame();
        }
        (layout, RevealLayout { child, child_layout, clipped })
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut RevealLayout,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(id) = id {
            // Natural height = what the content laid out to, clipped or not.
            let natural = window.layout_bounds(state.child_layout).size.height;
            if natural > px(0.) {
                window.with_element_state(id, |_: Option<Pixels>, _| ((), natural));
            }
        }
        if state.clipped {
            window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| state.child.prepaint(window, cx));
        } else {
            state.child.prepaint(window, cx);
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut RevealLayout,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        if state.clipped {
            window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| state.child.paint(window, cx));
        } else {
            state.child.paint(window, cx);
        }
    }
}

// ── fade + slide ──────────────────────────────────────────────────────────────

/// Opacity `v` (0..=1, the fade) and a vertical travel of `dy * (1 - m)` px, `m` the
/// movement progress ([`crate::presence::Presence::movement`]): the standard way a popup,
/// row or badge appears and leaves. The element must be positioned (GPUI's default is
/// relative; an absolute one keeps its own `left`/`bottom` and gets `top` replaced, so give
/// absolute ones no `top`).
pub fn fade_slide<E: Styled>(el: E, v: f32, m: f32, dy: f32) -> E {
    el.opacity(v.clamp(0.0, 1.0)).top(px(dy * (1.0 - m)))
}
