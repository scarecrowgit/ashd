//! Night light tile (+ expandable settings) and the brightness rows of the quick-settings
//! panel (det-06).
//!
//! * The tile is the sibling of "Do not disturb": click toggles the night light, the
//!   chevron at its right edge expands a section with the temperature slider and the mode
//!   selector. Sub-label: `until 07:00` (tinted by a schedule), `from 21:00` (scheduled,
//!   not tinted yet), `manual`, `off`. Hidden while the service has no outputs at all.
//! * One brightness row per display (icon, 4 px bar, mono value); hidden when the service
//!   is unavailable. With several displays each row gets a small caps caption.
//!
//! Everything that decides *what* to show is a plain function below (unit-tested); the
//! `impl QuickSettingsPanel` at the end only lays it out.

use std::time::Instant;

use gpui::{
    ClickEvent, Context, Div, ElementId, InteractiveElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use ashd_svc_nightlight::{Mode, NightlightState};
use ashd_tokens::metrics;
use ashd_ui::{Icon, icon, label, theme};

use crate::{QuickSettingsPanel, effective, percent, tile};

/// Coolest / warmest night temperatures the slider offers.
pub const TEMP_MIN: u32 = 2000;
pub const TEMP_MAX: u32 = 6000;
const TEMP_STEP: u32 = 50;

/// Slider position (0..=1, left = warm) -> Kelvin in `TEMP_STEP`s.
pub fn temp_from_slider(v: f32) -> u32 {
    let k = TEMP_MIN as f32 + v.clamp(0.0, 1.0) * (TEMP_MAX - TEMP_MIN) as f32;
    ((k / TEMP_STEP as f32).round() as u32 * TEMP_STEP).clamp(TEMP_MIN, TEMP_MAX)
}

pub fn slider_from_temp(k: u32) -> f32 {
    (k.clamp(TEMP_MIN, TEMP_MAX) - TEMP_MIN) as f32 / (TEMP_MAX - TEMP_MIN) as f32
}

/// The tile's second line.
pub fn night_light_label(s: &NightlightState) -> String {
    if !s.available {
        return "unavailable".into();
    }
    match (s.mode, s.until) {
        (Mode::Off, _) => "off".into(),
        (Mode::On, _) => "manual".into(),
        (_, Some(t)) if s.active => format!("until {t}"),
        (_, Some(t)) => format!("from {t}"),
        (_, None) if s.active => "on".into(),
        (_, None) => "scheduled".into(),
    }
}

/// Show the tile at all? Not when the compositor has no outputs to tint (service down,
/// no gamma protocol).
pub fn night_light_visible(s: &NightlightState) -> bool {
    s.available || !s.outputs.is_empty()
}

/// Segment order of the mode selector.
pub const MODES: [Mode; 4] = [Mode::Off, Mode::On, Mode::Fixed, Mode::Sun];
pub const MODE_LABELS: [&str; 4] = ["off", "manual", "times", "sun"];

pub fn mode_index(m: Mode) -> usize {
    MODES.iter().position(|x| *x == m).unwrap_or(0)
}

/// A line explaining the selected mode.
pub fn mode_hint(s: &NightlightState) -> String {
    match s.mode {
        Mode::Off => "never tinted".into(),
        Mode::On => "stays on".into(),
        Mode::Fixed => format!("{} to {}", s.start, s.end),
        Mode::Sun if s.has_location => "sunset to sunrise".into(),
        Mode::Sun => format!("no location in shell.json · using {} to {}", s.start, s.end),
    }
}

/// Brightness icon (the design uses the sliders glyph for brightness).
pub const BRIGHTNESS_ICON: Icon = Icon::Sliders;

impl QuickSettingsPanel {
    /// The tile; `None` while hidden.
    pub(crate) fn night_light_tile(&mut self, cx: &mut Context<Self>) -> Option<gpui::Stateful<Div>> {
        let s = self.ctx.services.nightlight.state.read(cx).clone();
        if !night_light_visible(&s) {
            return None;
        }
        let t = theme(cx).clone();
        let on = s.enabled && s.available;
        let fg = if on { t.on_accent } else { t.fg_muted };
        let open = self.state.night_open;
        let chevron = div()
            .id("qs-night-expand")
            .flex_none()
            .size(px(22.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(t.radius_control)
            .cursor_pointer()
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                this.state.night_open = !this.state.night_open;
                cx.notify();
            }))
            .child(icon(if open { Icon::ChevronDown } else { Icon::ChevronRight }, 12.).text_color(fg));
        let handle = self.ctx.services.nightlight.handle.clone();
        Some(
            tile("tile-night", Icon::Moon, "Night light", &night_light_label(&s), on, cx)
                .on_click(move |_, _, _| handle.toggle())
                .child(chevron),
        )
    }

    /// The expanded settings under the tile grid.
    pub(crate) fn night_light_section(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
        if !self.state.night_open {
            return None;
        }
        let s = self.ctx.services.nightlight.state.read(cx).clone();
        if !night_light_visible(&s) {
            return None;
        }
        let t = theme(cx).clone();
        let slider_value = match self.state.pending_temp {
            Some((v, at)) if at.elapsed() < std::time::Duration::from_millis(crate::PENDING_MS) => v,
            _ => slider_from_temp(s.temperature),
        };
        let kelvin = temp_from_slider(slider_value);
        let slider = ashd_ui::slider(
            "qs-night-temp",
            slider_value,
            cx.listener(|this, v: &f32, _, cx| {
                this.state.pending_temp = Some((*v, Instant::now()));
                this.ctx.services.nightlight.handle.set_temperature(temp_from_slider(*v));
                this.pending_clear(cx);
                cx.notify();
            }),
            window,
            cx,
        );
        let handle = self.ctx.services.nightlight.handle.clone();
        let modes = ashd_ui::segmented(
            "qs-night-mode",
            MODE_LABELS,
            mode_index(s.mode),
            move |ix, _, _| handle.set_mode(MODES[(*ix).min(MODES.len() - 1)]),
            cx,
        );
        Some(
            div()
                .p(px(12.))
                .flex()
                .flex_col()
                .gap(px(10.))
                .border_1()
                .border_color(t.line)
                .rounded(t.radius_control)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .child(icon(Icon::Sun, 15.).text_color(t.fg_muted))
                        .child(div().flex_1().child(slider))
                        .child(
                            div()
                                .w(px(50.))
                                .flex_none()
                                .flex()
                                .justify_end()
                                .font_family(t.font_mono.clone())
                                .text_size(px(11.5))
                                .text_color(t.fg)
                                .child(format!("{kelvin}K")),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(10.))
                        .child(div().min_w_0().flex_1().truncate().child(label(mode_hint(&s), cx)))
                        .child(modes),
                ),
        )
    }

    /// One row per display; empty when the service is unavailable.
    pub(crate) fn brightness_rows(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<Div> {
        let b = self.ctx.services.brightness.state.read(cx).clone();
        if !b.available {
            return Vec::new();
        }
        let t = theme(cx).clone();
        let several = b.displays.len() > 1;
        let mut rows = Vec::new();
        for (ix, d) in b.displays.iter().enumerate() {
            let pending = self.state.pending_brightness.get(&d.id).copied();
            let v = effective(d.value, pending);
            let id = d.id.clone();
            let slider = ashd_ui::slider(
                ElementId::from(("qs-brightness", ix as u32)),
                v,
                cx.listener(move |this, v: &f32, _, cx| {
                    this.state.pending_brightness.insert(id.clone(), (*v, Instant::now()));
                    this.ctx.services.brightness.handle.set(ashd_svc_brightness::Target::Display(id.clone()), *v);
                    this.pending_clear(cx);
                    cx.notify();
                }),
                window,
                cx,
            );
            let row = div()
                .flex()
                .items_center()
                .gap(px(12.))
                .child(icon(BRIGHTNESS_ICON, 15.).text_color(t.fg))
                .child(div().flex_1().child(slider))
                .child(
                    div()
                        .w(px(30.))
                        .flex_none()
                        .flex()
                        .justify_end()
                        .font_family(t.font_mono.clone())
                        .text_size(px(11.5))
                        .text_color(t.fg)
                        .child(percent(v)),
                );
            rows.push(if several {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(label(SharedString::from(d.name.clone()), cx).text_size(px(metrics::type_scale::LABEL.0 - 0.5)))
                    .child(row)
            } else {
                row
            });
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use ashd_svc_nightlight::{OutputStatus, TimeOfDay};

    use super::*;

    fn state(mode: Mode, active: bool, until: Option<TimeOfDay>) -> NightlightState {
        NightlightState {
            available: true,
            enabled: mode != Mode::Off,
            active,
            mode,
            until,
            outputs: vec![OutputStatus { name: "DP-1".into(), available: true }],
            ..NightlightState::default()
        }
    }

    #[test]
    fn labels_follow_the_design() {
        let seven = Some(TimeOfDay::new(7, 0));
        assert_eq!(night_light_label(&state(Mode::Fixed, true, seven)), "until 07:00");
        assert_eq!(night_light_label(&state(Mode::Fixed, false, Some(TimeOfDay::new(21, 0)))), "from 21:00");
        assert_eq!(night_light_label(&state(Mode::On, true, None)), "manual");
        assert_eq!(night_light_label(&state(Mode::Off, false, None)), "off");
        assert_eq!(night_light_label(&state(Mode::Sun, true, None)), "on");
        assert_eq!(night_light_label(&state(Mode::Sun, false, None)), "scheduled");
        let mut s = state(Mode::Fixed, true, seven);
        s.available = false;
        assert_eq!(night_light_label(&s), "unavailable");
    }

    #[test]
    fn tile_is_hidden_without_any_output() {
        assert!(!night_light_visible(&NightlightState::default()));
        assert!(night_light_visible(&state(Mode::Off, false, None)));
        let mut s = NightlightState::default();
        s.outputs.push(OutputStatus { name: "DP-1".into(), available: false });
        assert!(night_light_visible(&s), "an output in use by another client still shows the tile (as unavailable)");
    }

    #[test]
    fn temperature_slider_maps_both_ways_in_50k_steps() {
        assert_eq!(temp_from_slider(0.0), TEMP_MIN);
        assert_eq!(temp_from_slider(1.0), TEMP_MAX);
        assert_eq!(temp_from_slider(-3.0), TEMP_MIN);
        assert_eq!(temp_from_slider(0.35), 3400);
        assert_eq!(temp_from_slider(0.3517) % 50, 0);
        assert!((slider_from_temp(3400) - 0.35).abs() < 1e-6);
        assert_eq!(slider_from_temp(100), 0.0);
        for k in (TEMP_MIN..=TEMP_MAX).step_by(50) {
            assert_eq!(temp_from_slider(slider_from_temp(k)), k);
        }
    }

    #[test]
    fn mode_selector_order_matches_the_labels() {
        for (i, m) in MODES.iter().enumerate() {
            assert_eq!(mode_index(*m), i);
        }
        assert_eq!(MODE_LABELS.len(), MODES.len());
    }

    #[test]
    fn hints_explain_the_mode() {
        let mut s = state(Mode::Fixed, true, None);
        assert_eq!(mode_hint(&s), "21:00 to 07:00");
        s.mode = Mode::Sun;
        assert!(mode_hint(&s).starts_with("no location"));
        s.has_location = true;
        assert_eq!(mode_hint(&s), "sunset to sunrise");
    }
}
