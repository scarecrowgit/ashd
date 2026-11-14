//! det-07 drawing. All coordinates are design px of the 1920×1080 board; the painter's
//! scale maps them to physical px (`output scale × layout::fit`).

use std::{f64::consts::PI, time::Instant};

use ashd_gfx::{
    Painter, Text, TextLayout, TextStyle,
    icon::arc_path,
    text::{Generic, font_stack},
    vello,
};
use ashd_tokens::{Rgba, tokens};

use crate::{Button, LockUi, Phase, StatusIcon, layout as L};

pub(crate) struct Ctx<'a> {
    pub(crate) text: &'a mut Text,
    pub(crate) scale: f64,
    pub(crate) sans: String,
    pub(crate) mono: String,
}

impl Ctx<'_> {
    pub(crate) fn sans(&self, size: f32, color: Rgba) -> TextStyle {
        TextStyle::new(self.sans.clone(), size, color)
    }
    pub(crate) fn mono(&self, size: f32, color: Rgba) -> TextStyle {
        TextStyle::new(self.mono.clone(), size, color)
    }
    pub(crate) fn label(&self, color: Rgba) -> TextStyle {
        self.mono(L::LABEL_SIZE, color).tracking_em(L::LABEL_TRACKING_EM)
    }
    pub(crate) fn layout(&mut self, s: &str, style: &TextStyle) -> TextLayout {
        self.text.layout(s, style, self.scale)
    }
}

pub(crate) fn draw(ui: &mut LockUi, scene: &mut vello::Scene, text: &mut Text, size: (f64, f64), scale: f64, now: Instant) {
    let t = tokens(ui.theme);
    let c = t.colors;
    let radius = f64::from(t.radius.control);
    let k = L::fit(size);
    let (dw, dh) = (size.0 / k, size.1 / k);
    let s = scale * k;
    let mut p = Painter::new(scene, s);
    let mut cx = Ctx {
        text,
        scale: s,
        sans: font_stack(&t.fonts.sans, Generic::Sans),
        mono: font_stack(&t.fonts.mono, Generic::Mono),
    };

    // ── background: bg + 120 px grid of 1 px surface lines ────────────────────
    background(&mut p, dw, dh, &c);

    let alpha = ui.content_alpha(now);
    let mut hits = Vec::new();
    p.with_alpha(alpha, dw, dh, |p| {
        clock(ui, p, &mut cx, c.fg, c.fg_muted);
        auth(ui, p, &mut cx, dh, radius, now);
        hits = footer(ui, p, &mut cx, dw, dh, radius, now);
    });
    let hits = hits.into_iter().map(|(b, [x, y, w, h])| (b, [x * k, y * k, w * k, h * k])).collect();
    ui.set_hits(size, hits);
}

/// bg + 120 px grid of 1 px `surface` lines (shared with the greeter).
pub(crate) fn background(p: &mut Painter<'_>, dw: f64, dh: f64, c: &ashd_tokens::Colors) {
    p.fill_rect(0.0, 0.0, dw, dh, c.bg);
    let grid = c.surface.with_alpha(L::GRID_OPACITY);
    let mut x = 0.0;
    while x < dw {
        p.vline(x, 0.0, dh, 1.0, grid);
        x += L::GRID;
    }
    let mut y = 0.0;
    while y < dh {
        p.hline(0.0, y, dw, 1.0, grid);
        y += L::GRID;
    }
}

pub(crate) fn clock(ui: &LockUi, p: &mut Painter<'_>, cx: &mut Ctx<'_>, fg: Rgba, muted: Rgba) {
    let now = ui.wall_clock();
    let time = now.format("%H:%M").to_string();
    let date = now.format("%A, %-d %B").to_string();
    let style = cx
        .mono(L::CLOCK_SIZE, fg)
        .weight(L::CLOCK_WEIGHT)
        .line_height(L::CLOCK_LINE)
        .tracking_em(L::CLOCK_TRACKING_EM)
        .tabular();
    let time_l = cx.layout(&time, &style);
    p.text(&time_l, L::CLOCK_X, L::CLOCK_Y);
    let date_l = cx.layout(&date, &cx.sans(L::DATE_SIZE, muted));
    p.text(&date_l, L::CLOCK_X, L::CLOCK_Y + time_l.height() + L::CLOCK_GAP);
}

fn auth(ui: &LockUi, p: &mut Painter<'_>, cx: &mut Ctx<'_>, dh: f64, radius: f64, now: Instant) {
    let c = tokens(ui.theme).colors;
    let hint_style = cx.sans(L::HINT_SIZE, c.fg_muted);
    let hint_h = cx.layout("Ag", &hint_style).height();
    let block_h = L::AVATAR + L::AUTH_GAP + L::FIELD_H + L::AUTH_GAP + L::BADGE_H + L::AUTH_GAP + hint_h;
    let top = dh - L::AUTH_BOTTOM - block_h;
    let x0 = L::AUTH_X;

    // ── avatar + name ─────────────────────────────────────────────────────────
    p.border(x0, top, L::AVATAR, L::AVATAR, 1.0, radius, c.accent);
    let initial: String = ui.display_name.chars().next().map(String::from).unwrap_or_default();
    let ini = cx.layout(&initial, &cx.mono(L::AVATAR_FONT, c.accent).weight(500.0));
    p.text(&ini, x0 + (L::AVATAR - ini.width()) / 2.0, top + (L::AVATAR - ini.height()) / 2.0);
    let name = cx.layout(&ui.display_name, &cx.sans(L::NAME_SIZE, c.fg).weight(L::NAME_WEIGHT));
    let host_line = if ui.host.is_empty() { ui.compositor.clone() } else { format!("{} · {}", ui.host, ui.compositor) };
    let host = cx.layout(&host_line.to_uppercase(), &cx.label(c.fg_muted));
    let col_h = name.height() + L::NAME_COL_GAP + host.height();
    let nx = x0 + L::AVATAR + L::NAME_GAP;
    let ny = top + (L::AVATAR - col_h) / 2.0;
    p.text(&name, nx, ny);
    p.text(&host, nx, ny + name.height() + L::NAME_COL_GAP);

    // ── password field ────────────────────────────────────────────────────────
    let fy = top + L::AVATAR + L::AUTH_GAP;
    let fx = x0 + ui.shake_offset(now);
    let border = if ui.error_border(now) { c.err } else { c.line };
    p.fill_round_rect(fx, fy, L::AUTH_W, L::FIELD_H, radius, c.surface);
    p.border(fx, fy, L::AUTH_W, L::FIELD_H, 1.0, radius, border);
    let cy = fy + L::FIELD_H / 2.0;
    let mut ix = fx + 1.0 + L::FIELD_PAD;
    p.icon(&ui.icons().field, ix, cy - L::FIELD_ICON / 2.0, L::FIELD_ICON, c.fg);
    ix += L::FIELD_ICON + L::FIELD_GAP;
    let right = fx + L::AUTH_W - 1.0 - L::FIELD_PAD;
    let checking = ui.phase == Phase::Checking;
    // right end: ↵ hint, or the spinner while PAM runs
    let enter = cx.layout("↵", &cx.label(c.fg_muted));
    let right_w = if checking { L::SPINNER } else { enter.width() };
    if checking {
        let started = ui.checking_since().unwrap_or(now);
        let t = now.saturating_duration_since(started).as_secs_f64();
        let r = L::SPINNER / 2.0 - 1.0;
        let center = (right - L::SPINNER / 2.0, cy);
        let ring = arc_path(center.into(), r, 0.0, 2.0 * PI);
        p.stroke_path(&ring, 1.5, c.line);
        let arc = arc_path(center.into(), r, t * 2.0 * PI * 1.25, PI * 0.6);
        p.stroke_path(&arc, 1.5, c.accent);
    } else {
        p.text(&enter, right - enter.width(), cy - enter.height() / 2.0);
    }
    // dots (8 px pitch) + caret, clipped before the right-hand item
    let avail = (right - right_w - L::FIELD_GAP) - ix - L::FIELD_GAP - 1.0;
    let max_dots = (avail / L::DOT_PITCH).floor().max(0.0) as usize;
    let n = ui.shown_len().min(max_dots);
    let dot_color = if ui.selected_all() { c.accent } else { c.fg };
    for i in 0..n {
        p.circle(ix + L::DOT_PITCH * i as f64 + L::DOT_PITCH / 2.0, cy, L::DOT_R, dot_color);
    }
    if !checking && ui.caret_visible(now) {
        let caret_x = ix + L::DOT_PITCH * n as f64 + L::FIELD_GAP;
        p.vline(caret_x, cy - L::CARET_H / 2.0, L::CARET_H, 1.0, c.accent);
    }

    // ── badges + error text ───────────────────────────────────────────────────
    let by = fy + L::FIELD_H + L::AUTH_GAP;
    let bcy = by + L::BADGE_H / 2.0;
    let mut bx = x0;
    let mut badges: Vec<(String, Rgba)> = Vec::new();
    if ui.caps_lock {
        badges.push(("CAPS LOCK".into(), c.warn));
    }
    if let Some(l) = &ui.layout {
        badges.push((l.to_uppercase(), c.accent));
    }
    for (label, color) in badges {
        let l = cx.layout(&label, &cx.mono(L::LABEL_SIZE, color).weight(500.0));
        let w = l.width() + 2.0 * L::BADGE_PAD + 2.0;
        p.border(bx, by, w, L::BADGE_H, 1.0, radius, color);
        p.text(&l, bx + 1.0 + L::BADGE_PAD, bcy - l.height() / 2.0);
        bx += w + L::BADGE_GAP;
    }
    if let Some(err) = ui.error_text(now) {
        let l = cx.layout(&err, &cx.sans(L::ERROR_SIZE, c.err));
        p.text(&l, bx, bcy - l.height() / 2.0);
    }

    // ── hint line (PAM info) ──────────────────────────────────────────────────
    if let Some(info) = &ui.info {
        let hy = by + L::BADGE_H + L::AUTH_GAP;
        let l = cx.layout(info, &hint_style);
        p.text(&l, x0, hy);
    }

    // Layout hook (det-07 right column: media card + notification summary), not drawn
    // in this milestone: right L::SIDE_RIGHT, bottom L::SIDE_BOTTOM, width L::SIDE_W.
}

pub(crate) fn footer(
    ui: &LockUi,
    p: &mut Painter<'_>,
    cx: &mut Ctx<'_>,
    dw: f64,
    dh: f64,
    radius: f64,
    now: Instant,
) -> Vec<(Button, [f64; 4])> {
    let c = tokens(ui.theme).colors;
    let row_h = L::BUTTON_H;
    let rule_y = dh - L::FOOTER_BOTTOM - row_h - L::FOOTER_PAD_TOP - 1.0;
    let (x0, x1) = (L::FOOTER_X, dw - L::FOOTER_X);
    p.hline(x0, rule_y, x1 - x0, 1.0, c.line);
    let cy = dh - L::FOOTER_BOTTOM - row_h / 2.0;

    // status items (left)
    let mut x = x0;
    for item in &ui.status {
        let icon = match item.icon {
            StatusIcon::Battery => &ui.icons().battery,
            StatusIcon::Wifi => &ui.icons().wifi,
            StatusIcon::Volume => &ui.icons().volume,
        };
        p.icon(icon, x, cy - L::STATUS_ICON / 2.0, L::STATUS_ICON, c.fg);
        x += L::STATUS_ICON + L::STATUS_ICON_GAP;
        let l = cx.layout(&item.text.to_uppercase(), &cx.label(c.fg));
        p.text(&l, x, cy - l.height() / 2.0);
        x += l.width() + L::FOOTER_GAP;
    }

    // buttons (right-aligned)
    let armed = ui.armed(now);
    let buttons = ui.buttons();
    let mut laid = Vec::with_capacity(buttons.len());
    for b in &buttons {
        let is_armed = armed == Some(*b);
        let hovered = ui.hovered() == Some(*b);
        let fg = if is_armed {
            c.err
        } else if hovered {
            c.fg
        } else {
            c.fg_muted
        };
        let label = if is_armed { b.confirm_label() } else { b.label() };
        let l = cx.layout(label, &cx.sans(L::BUTTON_FONT, fg));
        let w = 1.0 + L::BUTTON_PAD + L::BUTTON_ICON + L::BUTTON_ICON_GAP + l.width() + L::BUTTON_PAD + 1.0;
        laid.push((*b, l, w, fg, is_armed, hovered));
    }
    let total: f64 = laid.iter().map(|(_, _, w, ..)| *w).sum::<f64>() + L::FOOTER_GAP * laid.len().saturating_sub(1) as f64;
    let mut bx = (x1 - total).max(x);
    let by = cy - row_h / 2.0;
    let mut hits = Vec::with_capacity(laid.len());
    for (b, l, w, fg, is_armed, hovered) in laid {
        if hovered {
            p.fill_round_rect(bx, by, w, row_h, radius, c.surface_2);
        }
        p.border(bx, by, w, row_h, 1.0, radius, if is_armed { c.err } else { c.line });
        let icon = match b {
            Button::Suspend => &ui.icons().suspend,
            Button::Reboot => &ui.icons().reboot,
            Button::PowerOff => &ui.icons().power_off,
        };
        let ix = bx + 1.0 + L::BUTTON_PAD;
        p.icon(icon, ix, cy - L::BUTTON_ICON / 2.0, L::BUTTON_ICON, fg);
        p.text(&l, ix + L::BUTTON_ICON + L::BUTTON_ICON_GAP, cy - l.height() / 2.0);
        hits.push((b, [bx, by, w, row_h]));
        bx += w + L::FOOTER_GAP;
    }
    hits
}
