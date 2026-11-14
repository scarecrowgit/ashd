//! Greeter drawing: the det-07 board with the auth block of a login (user row with an
//! editable name, session caps line, prompt field). Background, clock and footer are the
//! lock screen's own (`draw.rs`); the auth block keeps the lock screen's geometry so both
//! screens put the field in the same place.

use std::{f64::consts::PI, time::Instant};

use ashd_gfx::{
    Painter, Text,
    icon::arc_path,
    text::{Generic, font_stack},
    vello,
};
use ashd_tokens::{Rgba, tokens};

use crate::{
    GreeterUi, Phase, PromptKind,
    draw::{Ctx, background, clock, footer},
    greeter::{Focus, UiHits},
    layout as L,
};

pub(crate) fn draw(g: &mut GreeterUi, scene: &mut vello::Scene, text: &mut Text, size: (f64, f64), scale: f64, now: Instant) {
    let t = tokens(g.lock.theme);
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

    background(&mut p, dw, dh, &c);

    let alpha = g.lock.content_alpha(now);
    let mut ui_hits = None;
    let mut footer_hits = Vec::new();
    p.with_alpha(alpha, dw, dh, |p| {
        clock(&g.lock, p, &mut cx, c.fg, c.fg_muted);
        ui_hits = Some(auth(g, p, &mut cx, dh, radius, now));
        footer_hits = footer(&g.lock, p, &mut cx, dw, dh, radius, now);
    });
    let scaled = |r: [f64; 4]| [r[0] * k, r[1] * k, r[2] * k, r[3] * k];
    g.lock.set_hits(size, footer_hits.into_iter().map(|(b, r)| (b, scaled(r))).collect());
    if let Some(h) = ui_hits {
        g.set_ui_hits(size, UiHits { name: scaled(h.name), field: scaled(h.field), session: h.session.map(scaled) });
    }
}

fn auth(g: &GreeterUi, p: &mut Painter<'_>, cx: &mut Ctx<'_>, dh: f64, radius: f64, now: Instant) -> UiHits {
    let ui = &g.lock;
    let c = tokens(ui.theme).colors;
    let hint_style = cx.sans(L::HINT_SIZE, c.fg_muted);
    let hint_h = cx.layout("Ag", &hint_style).height();
    let block_h = L::AVATAR + L::AUTH_GAP + L::FIELD_H + L::AUTH_GAP + L::BADGE_H + L::AUTH_GAP + hint_h;
    let top = dh - L::AUTH_BOTTOM - block_h;
    let x0 = L::AUTH_X;
    let checking = ui.phase == Phase::Checking;
    let caret_on = !checking && !g.is_fatal() && ui.caret_visible(now);

    // ── avatar + editable name + session line ─────────────────────────────────
    let user = g.user().trim();
    let initial = user.chars().next().map(|ch| ch.to_uppercase().to_string()).unwrap_or_else(|| "?".into());
    let avatar_color = if user.is_empty() { c.line } else { c.accent };
    p.border(x0, top, L::AVATAR, L::AVATAR, 1.0, radius, avatar_color);
    let ini = cx.layout(&initial, &cx.mono(L::AVATAR_FONT, if user.is_empty() { c.fg_muted } else { c.accent }).weight(500.0));
    p.text(&ini, x0 + (L::AVATAR - ini.width()) / 2.0, top + (L::AVATAR - ini.height()) / 2.0);

    let nx = x0 + L::AVATAR + L::NAME_GAP;
    let name_max_w = L::AUTH_W - L::AVATAR - L::NAME_GAP;
    let name_l = if g.user().is_empty() {
        cx.layout("Username", &cx.sans(L::NAME_SIZE, c.fg_muted))
    } else {
        let style = cx.sans(L::NAME_SIZE, if g.name_selected() { c.accent } else { c.fg }).weight(L::NAME_WEIGHT);
        cx.layout(g.user(), &style)
    };
    // session line: `HOST · SESSION`, the session in fg and a chevron when selectable
    let prefix = if ui.host.is_empty() { String::new() } else { format!("{} · ", ui.host.to_uppercase()) };
    let prefix_l = cx.layout(&prefix, &cx.label(c.fg_muted));
    let multi = g.multiple_sessions();
    let sess_text = format!("{}{}", g.session_name().to_uppercase(), if multi { " ↕" } else { "" });
    let sess_l = cx.layout(&sess_text, &cx.label(if multi { c.fg } else { c.fg_muted }));
    let name_h = name_l.height();
    let col_h = name_h + L::NAME_COL_GAP + sess_l.height();
    let ny = top + (L::AVATAR - col_h) / 2.0;
    p.clipped(nx, ny, name_max_w, name_h, |p| p.text(&name_l, nx, ny));
    let name_end = if g.user().is_empty() { nx } else { nx + name_l.width().min(name_max_w) };
    if g.focus() == Focus::Name && caret_on {
        p.vline(name_end + 1.0, ny + 1.0, name_h - 2.0, 1.0, c.accent);
    }
    let sy = ny + name_h + L::NAME_COL_GAP;
    p.text(&prefix_l, nx, sy);
    p.text(&sess_l, nx + prefix_l.width(), sy);
    let session_rect = multi.then_some([nx, sy - 2.0, prefix_l.width() + sess_l.width(), sess_l.height() + 4.0]);
    let name_rect = [nx, ny - 2.0, name_max_w, name_h + 2.0];

    // ── prompt field ──────────────────────────────────────────────────────────
    let fy = top + L::AVATAR + L::AUTH_GAP;
    let fx = x0 + ui.shake_offset(now);
    let focused = g.focus() == Focus::Prompt && !g.is_fatal();
    let border = if ui.error_border(now) {
        c.err
    } else if focused {
        c.accent
    } else {
        c.line
    };
    p.fill_round_rect(fx, fy, L::AUTH_W, L::FIELD_H, radius, c.surface);
    p.border(fx, fy, L::AUTH_W, L::FIELD_H, 1.0, radius, border);
    let cy = fy + L::FIELD_H / 2.0;
    let mut ix = fx + 1.0 + L::FIELD_PAD;
    p.icon(&ui.icons().field, ix, cy - L::FIELD_ICON / 2.0, L::FIELD_ICON, c.fg);
    ix += L::FIELD_ICON + L::FIELD_GAP;
    let right = fx + L::AUTH_W - 1.0 - L::FIELD_PAD;
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
    let avail = (right - right_w - L::FIELD_GAP) - ix - L::FIELD_GAP - 1.0;
    let n = g.shown_len();
    let mut caret_x = ix;
    if n == 0 {
        // placeholder = what greetd asks for ("PASSWORD"), after the caret
        let label = g.prompt_label().to_uppercase();
        let l = cx.layout(&label, &cx.label(c.fg_muted));
        p.clipped(ix, fy, avail.max(0.0) + L::FIELD_GAP, L::FIELD_H, |p| p.text(&l, ix + 10.0, cy - l.height() / 2.0));
    } else if g.prompt_kind() == PromptKind::Visible && !checking {
        let l = cx.layout(g.answer_text(), &cx.sans(14.0, if g.answer_selected() { c.accent } else { c.fg }));
        let w = l.width().min(avail.max(0.0));
        p.clipped(ix, fy, avail.max(0.0), L::FIELD_H, |p| p.text(&l, ix, cy - l.height() / 2.0));
        caret_x = ix + w + 2.0;
    } else {
        let max_dots = (avail / L::DOT_PITCH).floor().max(0.0) as usize;
        let shown = n.min(max_dots);
        let dot_color = if g.answer_selected() { c.accent } else { c.fg };
        for i in 0..shown {
            p.circle(ix + L::DOT_PITCH * i as f64 + L::DOT_PITCH / 2.0, cy, L::DOT_R, dot_color);
        }
        caret_x = ix + L::DOT_PITCH * shown as f64 + L::FIELD_GAP;
    }
    if focused && caret_on {
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
        let room = (x0 + 1.5 * L::AUTH_W - bx).max(0.0);
        p.clipped(bx, by, room, L::BADGE_H, |p| p.text(&l, bx, bcy - l.height() / 2.0));
    }

    // ── hint line (greetd info messages) ──────────────────────────────────────
    if let Some(info) = &ui.info {
        let hy = by + L::BADGE_H + L::AUTH_GAP;
        let l = cx.layout(info, &hint_style);
        p.clipped(x0, hy, 1.5 * L::AUTH_W, hint_h + 2.0, |p| p.text(&l, x0, hy));
    }

    UiHits { name: name_rect, field: [x0, fy, L::AUTH_W, L::FIELD_H], session: session_rect }
}
