//! The workspace flyout (desk-05 `{{o0}}`..`{{o4}}`): a passive-overlay panel drawn next
//! to the hovered rail button. 268 wide, padding 12: title "02 · code" + "scroll · N cols",
//! column-proportion strip, window list (click focuses), Rename / Move / Canvas.
//! In: 120 ms, translateX −4 → 0 + fade; out: 100 ms (sys-06 "Rail submenu").

use gpui::{
    AnimationExt as _, App, Bounds, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled as _, Window, div, point, prelude::*, px, size,
};
use ashd_niri::niri_ipc::{Action, WorkspaceReferenceArg};
use ashd_shell_core::{Panel, PanelContext, PanelId, Placement, apps::app_for_app_id, apps::app_glyph, frame, hit_area};
use ashd_tokens::metrics;
use ashd_ui::{
    Icon,
    anim::{EASE_OUT_QUAD, EASE_STANDARD, timing, tween},
    hover_wash, icon, theme,
};

use crate::{geom, hover::RailHover, rename, workspace_title};

pub struct RailFlyoutPanel {
    pub ctx: PanelContext,
    hover: Entity<RailHover>,
}

struct Row {
    id: u64,
    title: String,
    glyph: Icon,
    active: bool,
}

impl RailFlyoutPanel {
    /// Card bounds in output-local px for the entry whose rail-local top is `top`, with
    /// `rows` window rows (`fullscreen`: the compact card of a fullscreen workspace).
    fn card_bounds(&self, top: f32, rows: usize, fullscreen: bool, cx: &App) -> Option<Bounds<gpui::Pixels>> {
        let t = theme(cx);
        let info = self.ctx.shell.display(self.ctx.display.as_ref()?.id, cx)?;
        let rail = info.rail_to_output(
            Bounds {
                origin: point(px(0.), px(top)),
                size: size(t.rail_w, px(geom::entry_h(t.rail, fullscreen))),
            },
            t,
        );
        let h = px(if fullscreen { geom::flyout_height_fullscreen() } else { geom::flyout_height(rows) });
        let x = rail.origin.x + t.rail_w + px(geom::flyout_offset_x(t));
        let mut y = rail.origin.y + px(geom::FLYOUT_OFFSET_Y);
        // Keep the card above the bar.
        let max_y = info.size().height - frame::inset(t) * 2. - t.bar_h - px(metrics::POPUP_GAP) - h;
        if y > max_y {
            y = max_y.max(px(0.));
        }
        Some(Bounds { origin: point(x, y), size: size(px(geom::FLYOUT_W), h) })
    }
}

impl Panel for RailFlyoutPanel {
    const ID: PanelId = PanelId("rail-flyout");
    const PLACEMENT: Placement = Placement::Passive;

    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self {
        let hover = crate::hover::for_display(ctx.display.as_ref().map(|d| d.id), &ctx.shell, cx);
        cx.observe(&hover, |_, _, cx| cx.notify()).detach();
        cx.observe(&ctx.niri, |this, _, cx| {
            if this.hover.read(cx).is_live() {
                cx.notify();
            }
        })
        .detach();
        cx.observe(&ctx.services.apps.state, |_, _, cx| cx.notify()).detach();
        Self { ctx, hover }
    }

    fn is_live(&self, cx: &App) -> bool {
        self.hover.read(cx).is_live()
    }
}

impl Render for RailFlyoutPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div().size_full().relative();
        let h = self.hover.read(cx);
        let (Some((ws_id, top)), closing, generation) = (h.shown, h.closing, h.generation) else {
            return root;
        };
        let t = theme(cx).clone();
        let niri = self.ctx.niri.read(cx);
        let Some(ws) = niri.workspaces.get(&ws_id).cloned() else { return root };
        let strip = niri.strip(ws_id);
        let canvas = niri.is_canvas(ws_id);
        let canvas_windows = niri.canvas_windows(ws_id);
        let fullscreen = ws.fullscreen_origin_id.is_some();
        let ws_number = niri.desk_number(ws_id).map_or(ws.idx.max(1), |n| n.min(u8::MAX as usize) as u8);
        let origin_name = niri.desk(ws_id).and_then(|d| d.name.clone()).filter(|n| !n.is_empty());
        let leaving_window = niri.fullscreen_window(ws_id).or_else(|| niri.windows_on(ws_id).into_iter().next()).map(|w| w.id);
        let ws_focused = ws.is_focused;
        let apps = self.ctx.services.apps.state.read(cx).apps.clone();
        let rows: Vec<Row> = niri
            .windows_on(ws_id)
            .into_iter()
            .take(geom::MAX_ROWS)
            .map(|w| {
                let app_id = w.app_id.clone().unwrap_or_default();
                let app = app_for_app_id(&apps, &app_id);
                let name = app.map(|a| a.name.clone()).unwrap_or_else(|| app_id.clone());
                Row {
                    id: w.id,
                    title: w.title.clone().filter(|s| !s.is_empty()).unwrap_or(name),
                    glyph: app_glyph(app, &app_id),
                    active: ws.active_window_id == Some(w.id),
                }
            })
            .collect();
        let Some(card) = self.card_bounds(top, rows.len(), fullscreen, cx) else { return root };
        let active_window = ws.active_window_id;
        let total: f64 = strip.columns.iter().map(|c| c.width.max(1.0)).sum();
        let (s2, muted, fg, line) = (t.surface_2, t.fg_muted, t.fg, t.line);

        let meta = div()
            .font_family(t.font_mono.clone())
            .text_size(px(geom::FLYOUT_META_SIZE))
            .text_color(muted)
            .child({
                if fullscreen {
                    let text = match &origin_name {
                        Some(n) => format!("fullscreen · {ws_number:02} {n}"),
                        None if ws_number > 0 => format!("fullscreen · {ws_number:02}"),
                        None => "fullscreen".to_string(),
                    };
                    t.label_text(&text)
                } else if canvas {
                    let n = canvas_windows.len();
                    t.label_text(&format!("canvas · {n} {}", if n == 1 { "window" } else { "windows" }))
                } else {
                    let n = strip.columns.len();
                    t.label_text(&format!("scroll · {n} {}", if n == 1 { "col" } else { "cols" }))
                }
            });
        let header = div()
            .flex()
            .items_baseline()
            .justify_between()
            .child(
                div()
                    .font_family(t.font_sans.clone())
                    .text_size(px(metrics::type_scale::TITLE.0))
                    .line_height(px(metrics::type_scale::TITLE.1))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(fg)
                    .child(if fullscreen {
                        rows.first().map(|r| r.title.clone()).unwrap_or_else(|| "fullscreen".into())
                    } else {
                        workspace_title(ws_number, ws.name.as_deref())
                    }),
            )
            .child(meta);

        let strip_el = div().flex().gap(px(geom::STRIP_GAP)).h(px(geom::STRIP_H)).children(
            if canvas && !canvas_windows.is_empty() {
                // Canvas: a tiny map of the window rects, full card width.
                let w = geom::FLYOUT_W - 2.0 * geom::FLYOUT_PAD - 2.0;
                vec![crate::canvas_map(&canvas_windows, w, geom::STRIP_H, true, &t)]
            } else if strip.columns.is_empty() {
                vec![div().flex_1().h_full().border_1().border_dashed().border_color(line).rounded(t.radius_control)]
            } else {
                strip
                    .columns
                    .iter()
                    .map(|c| {
                        let on = active_window.is_some_and(|w| c.windows.contains(&w));
                        let mut seg = div()
                            .h_full()
                            .min_w(px(2.))
                            .flex_basis(px(0.))
                            .border_1()
                            .border_color(if on { t.accent } else { line })
                            .rounded(t.radius_control);
                        seg.style().flex_grow = Some((c.width.max(1.0) / total * 100.0) as f32);
                        seg
                    })
                    .collect()
            },
        );

        let list = div().flex().flex_col().gap(px(geom::ROW_GAP)).children(if rows.is_empty() {
            vec![
                div()
                    .h(px(metrics::ROW_H))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .font_family(t.font_mono.clone())
                    .text_size(px(geom::FLYOUT_META_SIZE))
                    .text_color(muted)
                    .child(t.label_text("empty"))
                    .into_any_element(),
            ]
        } else {
            rows.into_iter()
                .enumerate()
                .map(|(i, r)| {
                    let id = r.id;
                    div()
                        .id(("rail-flyout-win", id))
                        .h(px(metrics::ROW_H))
                        .w_full()
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .rounded(t.radius_control)
                        .font_family(t.font_sans.clone())
                        .text_size(px(metrics::type_scale::UI.0))
                        .cursor_pointer()
                        .when(r.active, |d| d.bg(s2).text_color(fg))
                        .when(!r.active, |d| {
                            d.text_color(muted).hover(move |s| s.text_color(fg)).child(hover_wash(s2, None, t.radius_control))
                        })
                        .child(icon(r.glyph, geom::ROW_ICON).text_color(if r.active { fg } else { muted }))
                        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(r.title))
                        .child(
                            div()
                                .font_family(t.font_mono.clone())
                                .text_size(px(geom::FLYOUT_META_SIZE))
                                .text_color(muted)
                                .child(format!("{}", i + 1)),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.ctx.shell.niri_action(Action::FocusWindow { id }, cx);
                            this.hover.update(cx, |h, cx| h.dismiss(cx));
                        }))
                        .into_any_element()
                })
                .collect()
        });

        let action = |id: &'static str, text: &'static str, enabled: bool| {
            div()
                .id(id)
                .flex_1()
                .h(px(geom::ACTION_H))
                .flex()
                .items_center()
                .justify_center()
                .border_1()
                .border_color(line)
                .rounded(t.radius_control)
                .font_family(t.font_sans.clone())
                .text_size(px(geom::ACTION_TEXT))
                .text_color(if enabled { fg } else { muted })
                .when(enabled, |d| d.cursor_pointer().child(hover_wash(s2, None, t.radius_control)))
                .child(text)
        };
        let ws_ref = WorkspaceReferenceArg::Id(ws_id);
        let move_ref = ws_ref.clone();
        let current_name = ws.name.clone().unwrap_or_default();
        let ws_idx = ws_number;
        let display = self.ctx.display.as_ref().map(|d| d.id);
        let actions = div()
            .flex()
            .gap(px(geom::ACTION_GAP))
            .pt(px(geom::FLYOUT_GAP))
            .border_t_1()
            .border_color(line)
            .child(action("rail-flyout-rename", "Rename", true).on_click(cx.listener(move |this, _, _, cx| {
                rename::begin(
                    rename::RenameTarget { workspace: ws_id, idx: ws_idx, name: current_name.clone(), display },
                    Some(card),
                    &this.ctx.shell,
                    cx,
                );
                this.hover.update(cx, |h, cx| h.dismiss(cx));
            })))
            .child(action("rail-flyout-move", "Move", true).on_click(cx.listener(move |this, _, _, cx| {
                this.ctx.shell.niri_action(
                    Action::MoveWindowToWorkspace { window_id: None, reference: move_ref.clone(), focus: false },
                    cx,
                );
                this.hover.update(cx, |h, cx| h.dismiss(cx));
            })))
            // Switch this workspace's mode (`set-workspace-mode` acts on the focused
            // workspace: focus it first). On a canvas the button offers going back.
            .child(action("rail-flyout-canvas", if canvas { "Scroll" } else { "Canvas" }, true).on_click(cx.listener(
                move |this, _, _, cx| {
                    use ashd_niri::niri_ipc::WorkspaceMode;
                    let mode = if canvas { WorkspaceMode::Strip } else { WorkspaceMode::Canvas };
                    let handle = this.ctx.niri_handle.clone();
                    cx.spawn(async move |_, _| {
                        if !ws_focused
                            && handle.action(Action::FocusWorkspace { reference: WorkspaceReferenceArg::Id(ws_id) }).await.is_err()
                        {
                            return;
                        }
                        handle.send_action(Action::SetWorkspaceMode { mode });
                    })
                    .detach();
                    this.hover.update(cx, |h, cx| h.dismiss(cx));
                },
            )));

        // A fullscreen workspace: one action, back to its desk (the compositor moves the window back).
        let leave = div()
            .flex()
            .pt(px(geom::FLYOUT_GAP))
            .border_t_1()
            .border_color(line)
            .child(action("rail-flyout-leave", "Leave fullscreen", leaving_window.is_some()).on_click(cx.listener(move |this, _, _, cx| {
                if let Some(id) = leaving_window {
                    this.ctx.shell.niri_action(Action::FullscreenWindow { id: Some(id) }, cx);
                }
                this.hover.update(cx, |h, cx| h.dismiss(cx));
            })));
        let card_el = div()
            .w(px(geom::FLYOUT_W))
            .p(px(geom::FLYOUT_PAD))
            .flex()
            .flex_col()
            .gap(px(geom::FLYOUT_GAP))
            .bg(t.surface)
            .border_1()
            .border_color(t.frame)
            .rounded(t.radius_panel)
            .shadow(t.shadow_flyout())
            .child(header)
            .when(!fullscreen, |d| d.child(strip_el))
            .child(list)
            .child(if fullscreen { leave.into_any_element() } else { actions.into_any_element() });

        // The hit area includes the 16px bridge to the rail, so the pointer can travel
        // from the button to the card without leaving both.
        let bridge = px(geom::flyout_offset_x(&t));
        let x = card.origin.x - bridge;
        let hit = hit_area(div().pl(bridge).child(card_el))
            .id("rail-flyout")
            .on_hover(cx.listener(|this, inside: &bool, _, cx| {
                let inside = *inside;
                this.hover.update(cx, |h, cx| h.set_in_flyout(inside, cx));
            }));
        let positioned = div().absolute().top(card.origin.y).left(x).child(hit);
        let el = if closing {
            positioned
                .with_animation(("rail-flyout-out", generation), tween(timing::SUBMENU_OUT, EASE_OUT_QUAD), move |el, p| {
                    el.opacity(1.0 - p).left(x - px(4.0 * p))
                })
                .into_any_element()
        } else {
            positioned
                .with_animation(("rail-flyout-in", generation), tween(timing::SUBMENU_IN, EASE_STANDARD), move |el, p| {
                    el.opacity(p).left(x - px(4.0 * (1.0 - p)))
                })
                .into_any_element()
        };
        root.child(el)
    }
}
