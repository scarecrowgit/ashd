//! The live mini-desktop of the theme switcher (docs/design/pages/
//! det-13-session-buttons-and-theme-switcher-interactive.html, "Theme preview", 300 high):
//! rail, three windows with title strips, scratchpad, status bar and dock, drawn from one
//! [`Theme`] — colours, radii and the layout variants of its `Shape` (attached or floating
//! gaps, num / thumb / pill rail, bar / split / pill dock, stack / tabs scratchpad). The
//! caller passes a crossfaded theme (`Theme::lerp`) while a switch is in flight.

use gpui::prelude::FluentBuilder as _;
use gpui::{Div, FontWeight, ParentElement as _, Styled as _, div, px, relative};
use ashd_tokens::{DockVariant, LayoutVariant, RailVariant, ScratchpadVariant};
use ashd_ui::Theme;

pub const PREVIEW_H: f32 = 300.0;
/// Reference px (the Ash desktop, 1920×1080) → preview px.
const RAIL_SCALE: f32 = 0.47;
const BAR_SCALE: f32 = 0.8;
const GAP_SCALE: f32 = 0.6;

/// Pixel geometry of the preview for one theme (pure data, tested).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub floating: bool,
    /// Margin of every panel from the preview edge (0 when attached).
    pub inset: f32,
    /// Gap between panels (0 when attached).
    pub gap: f32,
    pub rail_w: f32,
    pub bar_h: f32,
    pub scratch_w: f32,
    pub status_w: f32,
    /// Gap between the windows of the strip.
    pub win_gap: f32,
}

impl Geometry {
    pub fn of(t: &Theme) -> Self {
        let floating = t.layout == LayoutVariant::Floating;
        let gap = if floating { f32::from(t.gap) * GAP_SCALE } else { 0.0 };
        Geometry {
            floating,
            inset: gap,
            gap,
            rail_w: f32::from(t.rail_w) * RAIL_SCALE,
            bar_h: f32::from(t.bar_h) * BAR_SCALE,
            scratch_w: 120.0,
            status_w: 210.0,
            win_gap: if floating { gap.max(4.0) } else { 6.0 },
        }
    }

    /// Height of the rail / scratchpad / window area: the preview minus the bottom bar.
    pub fn upper_bottom(&self) -> f32 {
        if self.floating { self.inset + self.bar_h + self.gap } else { self.bar_h }
    }
}

/// A panel box: `surface`, 1px `frame` border, panel radius. Attached panels only keep the
/// edge towards the content (`edge`).
fn panel_box(t: &Theme, g: &Geometry, edge: fn(Div) -> Div) -> Div {
    let b = div().bg(t.surface).border_color(t.frame);
    if g.floating { b.border_1().rounded(t.radius_panel) } else { edge(b) }
}

pub fn preview(t: &Theme) -> Div {
    let g = Geometry::of(t);
    let (inset, bottom) = (g.inset, g.upper_bottom());
    let main_left = if g.floating { inset + g.rail_w + g.gap } else { g.rail_w };
    let main_right = if g.floating { inset + g.scratch_w + g.gap } else { g.scratch_w };

    let rail = panel_box(t, &g, |d| d.border_r_1())
        .absolute()
        .left(px(inset))
        .top(px(inset))
        .bottom(px(bottom))
        .w(px(g.rail_w))
        .child(rail_items(t, &g));
    let scratch = panel_box(t, &g, |d| d.border_l_1())
        .absolute()
        .right(px(inset))
        .top(px(inset))
        .bottom(px(bottom))
        .w(px(g.scratch_w))
        .p(px(10.))
        .child(scratchpad(t));
    // The strip is scrolled: the first window sticks out to the left, the last to the right.
    let strip = div().absolute().left(px(main_left)).right(px(main_right)).top(px(inset)).bottom(px(bottom)).overflow_hidden().child(
        div()
            .absolute()
            .left(px(-44.))
            .right_0()
            .top(px(12.))
            .bottom(px(10.))
            .flex()
            .gap(px(g.win_gap))
            .child(window(t, false, 1).w(px(130.)).flex_none())
            .child(window(t, true, 1).flex_1().min_w_0())
            .child(window(t, false, 3).w(px(190.)).flex_none()),
    );

    div()
        .relative()
        .w_full()
        .h(px(PREVIEW_H))
        .overflow_hidden()
        .bg(t.bg)
        .border_1()
        .border_color(t.line)
        .font_family(t.font_mono.clone())
        .child(strip)
        .child(rail)
        .child(scratch)
        .child(bottom_bar(t, &g))
}

fn window(t: &Theme, focused: bool, accent_line: usize) -> Div {
    let mut w = div()
        .bg(t.surface_2)
        .border_1()
        .border_color(if focused { t.accent } else { t.frame })
        .rounded(t.radius_window)
        .overflow_hidden()
        .flex()
        .flex_col()
        .child(div().h(px(14.)).flex_none().border_b_1().border_color(t.line));
    for (ix, width) in [0.60, 0.75, 0.50, 0.66, 0.40].into_iter().enumerate() {
        let hot = focused && ix == accent_line;
        w = w.child(div().h(px(4.)).flex_none().mx(px(10.)).mt(px(8.)).w(relative(width)).bg(if hot { t.accent } else { t.line }));
    }
    w
}

fn rail_items(t: &Theme, g: &Geometry) -> Div {
    let col = div().size_full().pt(px(12.)).flex().flex_col().items_center().gap(px(10.));
    match t.rail {
        RailVariant::Num => col.children((1..=4).map(|n| {
            let active = n == 2;
            div()
                .size(px(18.))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .rounded(t.radius_control)
                .font_family(t.font_mono.clone())
                .text_size(px(9.))
                .font_weight(FontWeight::MEDIUM)
                .when(active, |d| d.bg(t.accent).text_color(t.on_accent))
                .when(!active, |d| d.text_color(t.fg_muted))
                .child(n.to_string())
        })),
        RailVariant::Thumb => col.children((1..=4).map(|n| {
            let active = n == 2;
            div()
                .w(px((g.rail_w - 10.0).max(12.0)))
                .h(px(20.))
                .flex_none()
                .bg(t.surface_2)
                .border_1()
                .border_color(if active { t.accent } else { t.line })
                .rounded(t.radius_control)
        })),
        RailVariant::Pill => col.children((1..=4).map(|n| {
            let active = n == 2;
            div()
                .w(px(6.))
                .h(px(if active { 14. } else { 6. }))
                .flex_none()
                .rounded(px(3.))
                .bg(if active { t.accent } else { t.fg_muted })
        })),
    }
}

fn scratchpad(t: &Theme) -> Div {
    let row = |accent: bool| {
        div().h(px(16.)).flex_none().mb(px(6.)).bg(t.surface_2).border_1().border_color(if accent { t.accent } else { t.line })
    };
    match t.scratchpad {
        ScratchpadVariant::Stack => div().flex().flex_col().children((0..6).map(|ix| row(ix == 0))),
        ScratchpadVariant::Tabs => div()
            .flex()
            .flex_col()
            .child(div().flex().gap(px(4.)).mb(px(8.)).children((0..3).map(|ix| {
                div().h(px(14.)).w(px(28.)).flex_none().border_b_2().border_color(if ix == 0 { t.accent } else { t.line })
            })))
            .children((0..4).map(|ix| row(ix == 0))),
    }
}

fn status_block(t: &Theme) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(10.))
        .child(div().font_family(t.font_mono.clone()).text_size(px(14.)).font_weight(FontWeight::MEDIUM).text_color(t.fg).child("14:51"))
        .child(div().w(px(1.)).h(px(16.)).flex_none().bg(t.line))
        .child(div().relative().w(px(30.)).h(px(4.)).flex_none().bg(t.line).child(div().absolute().left_0().top_0().bottom_0().w(relative(0.4)).bg(t.accent)))
        .child(div().flex_1())
        .child(
            div()
                .h(px(18.))
                .px(px(5.))
                .flex()
                .items_center()
                .border_1()
                .border_color(t.accent)
                .text_color(t.accent)
                .font_family(t.font_mono.clone())
                .text_size(px(9.))
                .font_weight(FontWeight::MEDIUM)
                .child("EN"),
        )
}

fn dock_items(t: &Theme, count: usize, accent: Option<usize>) -> Vec<Div> {
    (0..count)
        .map(|ix| div().size(px(18.)).flex_none().rounded(t.radius_control).bg(if accent == Some(ix) { t.accent } else { t.line }))
        .collect()
}

fn dock(t: &Theme, g: &Geometry) -> Div {
    let region = div().flex_1().min_w_0().h_full().flex().items_center().justify_center().gap(px(g.gap.max(6.0)));
    let boxed = |items: Vec<Div>, radius: gpui::Pixels| {
        div()
            .h(px(g.bar_h))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .px(px(10.))
            .bg(t.surface)
            .border_1()
            .border_color(t.frame)
            .rounded(radius)
            .children(items)
    };
    match t.dock {
        DockVariant::Bar => region.child(div().flex().items_center().gap(px(8.)).children(dock_items(t, 5, Some(2)))),
        DockVariant::Split => region
            .child(boxed(dock_items(t, 3, Some(2)), t.radius_panel))
            .child(boxed(dock_items(t, 2, None), t.radius_panel)),
        // The mock: dock radius = twice the panel radius.
        DockVariant::Pill => region.child(boxed(dock_items(t, 5, Some(2)), t.radius_panel * 2.0)),
    }
}

fn bottom_bar(t: &Theme, g: &Geometry) -> Div {
    if g.floating {
        div()
            .absolute()
            .left(px(g.inset))
            .right(px(g.inset))
            .bottom(px(g.inset))
            .h(px(g.bar_h))
            .flex()
            .gap(px(g.gap))
            .child(panel_box(t, g, |d| d).w(px(g.status_w)).h_full().flex_none().child(status_block(t).h_full()))
            .child(dock(t, g))
    } else {
        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom_0()
            .h(px(g.bar_h))
            .flex()
            .bg(t.surface)
            .border_t_1()
            .border_color(t.frame)
            .child(div().w(px(g.status_w)).h_full().flex_none().border_r_1().border_color(t.frame).child(status_block(t).h_full()))
            .child(dock(t, g))
    }
}

#[cfg(test)]
mod tests {
    use ashd_tokens::ThemeName;

    use super::*;

    fn geo(name: ThemeName) -> Geometry {
        Geometry::of(&Theme::get(name))
    }

    #[test]
    fn attached_has_no_gaps_floating_has() {
        let ash = geo(ThemeName::Ash);
        assert!(!ash.floating);
        assert_eq!((ash.inset, ash.gap), (0.0, 0.0));
        for name in [ThemeName::Ink, ThemeName::Midnight, ThemeName::Moss, ThemeName::Dusk] {
            let g = geo(name);
            assert!(g.floating && g.inset > 0.0 && g.gap > 0.0, "{name}");
        }
        // gap order follows the tokens: Dusk 12 > Ink 10 = Midnight 10 > Moss 8
        assert!(geo(ThemeName::Dusk).gap > geo(ThemeName::Ink).gap);
        assert!(geo(ThemeName::Ink).gap > geo(ThemeName::Moss).gap);
    }

    #[test]
    fn rail_and_bar_follow_the_shape() {
        // thumb rails (Ink, Dusk) are wider than num (Ash, Moss) and pill (Midnight)
        assert!(geo(ThemeName::Ink).rail_w > geo(ThemeName::Ash).rail_w);
        assert_eq!(geo(ThemeName::Ink).rail_w, geo(ThemeName::Dusk).rail_w);
        assert!(geo(ThemeName::Midnight).rail_w < geo(ThemeName::Ash).rail_w);
        // split / pill docks are taller than the bar dock
        assert!(geo(ThemeName::Moss).bar_h > geo(ThemeName::Ash).bar_h);
        assert!(geo(ThemeName::Dusk).bar_h > geo(ThemeName::Moss).bar_h);
    }

    #[test]
    fn upper_area_stops_above_the_bar() {
        let ash = geo(ThemeName::Ash);
        assert_eq!(ash.upper_bottom(), ash.bar_h);
        let ink = geo(ThemeName::Ink);
        assert_eq!(ink.upper_bottom(), ink.inset + ink.bar_h + ink.gap);
        assert!(ink.upper_bottom() < PREVIEW_H / 2.0);
    }
}
