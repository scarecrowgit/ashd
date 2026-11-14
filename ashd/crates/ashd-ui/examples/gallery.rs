//! Every ashd-ui component and icon in an ordinary window. Run ONLY in the headless nested
//! session (docs/GPUI-NOTES §7), never on the live desktop.
//!
//! Keys: `t` next theme (300 ms crossfade of both windows) · `1` components (docs sys-04) ·
//! `2` more components · `3` icons · `s` scroll one screen down (wraps) · `m` reduced
//! motion · `q` quit.  Environment: `GALLERY_THEME=moss`, `GALLERY_PAGE=2`.

use std::path::PathBuf;

use gpui::{
    App, Bounds, Context, Div, FocusHandle, Focusable, FontWeight, IntoElement, KeyDownEvent, ParentElement as _,
    Render, ScrollHandle, Styled as _, Window, WindowBounds, WindowOptions, div, point, prelude::*, px, size,
};
use ashd_tokens::{ThemeName, metrics};
use ashd_ui::{
    BadgeKind, ButtonKind, CheckState, ControlState, FieldState, Icon, MenuItem, Segment, Tab, Toast, TooltipExt,
    anim, app_icon, badge, button, button_small, button_state, button_with_icon_state, checkbox, checkbox_box,
    empty_state, field, field_caret, field_error, hline, icon, icon_button, icon_button_kind, icon_button_state, keycap,
    label, list_row, menu, menu_separator, meter_colored, mono, numeric, overlay_panel, progress, radio, radio_dot,
    rich_row, rich_row_text, segmented, segmented_icons, set_theme, slider, spinner, status, tabs, theme, title,
    toggle, tooltip, window_frame, window_titlebar,
};

struct Gallery {
    focus: FocusHandle,
    scroll: ScrollHandle,
    page: u8,
    theme: ThemeName,
    toggles: [bool; 3],
    slider: f32,
    tab: usize,
    segment: usize,
    mode: usize,
    checks: [CheckState; 2],
    radio: usize,
    scroll_step: u32,
    png: Option<PathBuf>,
    svg: Option<PathBuf>,
}

fn find_icon(ext: &str) -> Option<PathBuf> {
    let base = PathBuf::from("/run/current-system/sw/share/icons/hicolor");
    for sub in ["scalable/apps", "48x48/apps", "64x64/apps", "128x128/apps"] {
        if let Ok(rd) = std::fs::read_dir(base.join(sub)) {
            let mut files: Vec<_> =
                rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == ext)).collect();
            files.sort();
            if let Some(f) = files.into_iter().next() {
                return Some(f);
            }
        }
    }
    None
}

impl Gallery {
    fn new(theme: ThemeName, cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            page: std::env::var("GALLERY_PAGE").ok().and_then(|v| v.parse().ok()).unwrap_or(1),
            theme,
            toggles: [true, false, true],
            slider: 0.62,
            tab: 0,
            segment: 0,
            mode: 0,
            checks: [CheckState::Checked, CheckState::Unchecked],
            radio: 0,
            scroll_step: 0,
            png: find_icon("png"),
            svg: find_icon("svg"),
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev.keystroke.key.as_str() {
            "t" => {
                self.theme = self.theme.next();
                set_theme(self.theme, true, cx);
            }
            k @ ("1" | "2" | "3") => {
                self.page = k.parse().unwrap_or(1);
                self.scroll.set_offset(point(px(0.), px(0.)));
                self.scroll_step = 0;
            }
            "s" => {
                self.scroll_step += 1;
                let max = self.scroll.max_offset().y;
                let y = px(self.scroll_step as f32 * 900.);
                if y - max > px(899.) {
                    self.scroll_step = 0;
                    self.scroll.set_offset(point(px(0.), px(0.)));
                } else {
                    self.scroll.set_offset(point(px(0.), -(y.min(max))));
                }
            }
            "m" => anim::set_reduced_motion(!anim::reduced_motion(), cx),
            "q" => cx.quit(),
            _ => {}
        }
        window.refresh();
        cx.notify();
    }
}

impl Focusable for Gallery {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

// ── layout helpers ────────────────────────────────────────────────────────────

fn card(heading: &str, hint: Option<&str>, cx: &App) -> Div {
    let t = theme(cx);
    let mut head = div()
        .flex()
        .items_baseline()
        .gap(px(12.))
        .pb(px(10.))
        .border_b_1()
        .border_color(t.line)
        .child(title(heading.to_string(), cx));
    if let Some(h) = hint {
        head = head.child(label(h.to_string(), cx));
    }
    div()
        .flex()
        .flex_col()
        .gap(px(14.))
        .p(px(20.))
        .bg(t.surface)
        .border_1()
        .border_color(t.line)
        .rounded(t.radius_panel)
        .child(head)
}

fn hrow() -> Div {
    div().flex().items_center().gap(px(12.))
}

fn column() -> Div {
    div().flex_1().min_w(px(0.)).flex().flex_col().gap(px(16.))
}

fn page_header(heading: &str, text: &str, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .text_size(px(28.))
                .line_height(px(32.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.fg)
                .child(heading.to_string()),
        )
        .child(div().max_w(px(720.)).text_size(px(13.)).line_height(px(20.)).text_color(t.fg_muted).child(text.to_string()))
}

impl Gallery {
    // ── page 1: docs/design/pages/sys-04-components-ash.html ──────────────────

    fn buttons(&self, cx: &App) -> Div {
        let states = [
            ControlState::Default,
            ControlState::Hover,
            ControlState::Pressed,
            ControlState::Focus,
            ControlState::Disabled,
        ];
        let mut c = card("Buttons", None, cx);
        c = c.child(
            hrow().child(div().w(px(90.))).children(
                ["default", "hover", "pressed", "focus", "disabled"].map(|s| div().flex_1().child(label(s, cx))),
            ),
        );
        for (name, kind, text) in [
            ("primary", ButtonKind::Primary, "Paste"),
            ("secondary", ButtonKind::Secondary, "Rename"),
            ("ghost", ButtonKind::Ghost, "Cancel"),
            ("danger", ButtonKind::Danger, "Delete"),
        ] {
            c = c.child(hrow().child(div().w(px(90.)).child(label(name, cx))).children(states.map(|st| {
                let b = if kind == ButtonKind::Danger {
                    button_with_icon_state((name, st as u32), Icon::Trash, text, kind, st, cx)
                } else {
                    button_state((name, st as u32), text, kind, st, false, cx)
                };
                div().flex_1().child(div().flex().child(b))
            })));
        }
        c.child(
            hrow()
                .gap(px(10.))
                .child(div().w(px(90.)).child(label("icon", cx)))
                .child(icon_button_state("ib1", Icon::Plus, ButtonKind::Secondary, ControlState::Default, cx))
                .child(icon_button_kind("ib2", Icon::Search, ButtonKind::Ghost, cx))
                .child(icon_button_kind("ib3", Icon::Fit, ButtonKind::Secondary, cx))
                .child(button_small("small", "Small", ButtonKind::Secondary, cx)),
        )
    }

    fn inputs(&self, cx: &App) -> Div {
        let t = theme(cx);
        let labelled = |name: &str, el: Div| div().flex_1().flex().flex_col().gap(px(6.)).child(label(name.to_string(), cx)).child(el);
        let text = |s: &str| div().flex_1().child(s.to_string());
        let placeholder = |s: &str| div().flex_1().text_color(t.fg_muted).child(s.to_string());
        card("Inputs", None, cx)
            .child(
                hrow()
                    .items_start()
                    .gap(px(16.))
                    .child(labelled("default", field(FieldState::Default, cx).child(placeholder("search clipboard, windows, images"))))
                    .child(labelled("focus", field(FieldState::Focus, cx).child(text("~/dots/niri")).child(field_caret(cx)))),
            )
            .child(
                hrow()
                    .items_start()
                    .gap(px(16.))
                    .child(labelled(
                        "error",
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(field(FieldState::Error, cx).child(text("config.kdl")))
                            .child(field_error("A file with this name already exists", cx)),
                    ))
                    .child(labelled("filled", field(FieldState::Default, cx).child(text("wallpapers")))),
            )
    }

    fn toggles_meters(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let t = theme(cx).clone();
        let mut tg = Vec::new();
        for ix in 0..2 {
            tg.push(toggle(
                ("tg", ix as u32),
                self.toggles[ix],
                cx.listener(move |this, on: &bool, _, cx| {
                    this.toggles[ix] = *on;
                    cx.notify();
                }),
                cx,
            ));
        }
        let vol = |value: f32, cx: &App| {
            hrow()
                .child(icon(Icon::Volume, 14.).text_color(t.fg))
                .child(
                    div()
                        .flex_1()
                        .h(px(metrics::SLIDER_TRACK_H))
                        .relative()
                        .bg(t.line)
                        .child(div().absolute().top_0().bottom_0().left_0().w(gpui::relative(value)).bg(t.accent))
                        .child(
                            div()
                                .absolute()
                                .left(gpui::relative(value))
                                .ml(px(-1.))
                                .top(px(-5.))
                                .w(px(2.))
                                .h(px(14.))
                                .bg(t.fg),
                        ),
                )
                .child(mono(format!("{}", (value * 100.0).round()), cx).text_size(px(11.5)))
        };
        let live_slider = {
            let v = self.slider;
            slider(
                "vol-live",
                v,
                cx.listener(|this, v: &f32, _, cx| {
                    this.slider = *v;
                    cx.notify();
                }),
                window,
                cx,
            )
        };
        card("Toggles and meters", None, cx)
            .child(
                hrow()
                    .gap(px(28.))
                    .children(tg)
                    .child(
                        hrow()
                            .gap(px(10.))
                            .child(checkbox_box(CheckState::Checked, cx))
                            .child(checkbox_box(CheckState::Mixed, cx))
                            .child(checkbox_box(CheckState::Unchecked, cx)),
                    )
                    .child(hrow().gap(px(6.)).child(radio_dot(true, cx)).child(radio_dot(false, cx))),
            )
            .child(vol(0.62, cx))
            .child(vol(1.0, cx))
            .child(
                hrow().gap(px(20.)).children(
                    [("cpu", 0.34, t.accent, "34%"), ("disk", 0.86, t.warn, "86%"), ("bat", 0.08, t.err, "8%")]
                        .map(|(n, v, c, s)| {
                            hrow()
                                .gap(px(6.))
                                .child(label(n, cx))
                                .child(meter_colored(v, metrics::METER_W, c, cx))
                                .child(mono(s, cx).text_size(px(11.)))
                        }),
                ),
            )
            .child(hrow().child(label("live slider", cx)).child(div().flex_1().child(live_slider)))
    }

    fn segments_tabs(&mut self, cx: &mut Context<Self>) -> Div {
        card("Segments and tabs", None, cx)
            .child(div().flex().child(segmented_icons(
                "seg",
                [Segment::new("Scroll").icon(Icon::Columns), Segment::new("Canvas").icon(Icon::Canvas)],
                self.segment,
                cx.listener(|this, ix: &usize, _, cx| {
                    this.segment = *ix;
                    cx.notify();
                }),
                cx,
            )))
            .child(tabs(
                "tabs",
                [Tab::new("Clipboard").count(24), Tab::new("Images").count(6), Tab::new("Windows").count(4)],
                self.tab,
                cx.listener(|this, ix: &usize, _, cx| {
                    this.tab = *ix;
                    cx.notify();
                }),
                cx,
            ))
            .child(div().flex().child(segmented(
                "mode",
                ["Scroll", "Canvas"],
                self.mode,
                cx.listener(|this, ix: &usize, _, cx| {
                    this.mode = *ix;
                    cx.notify();
                }),
                cx,
            )))
    }

    fn badges(&self, cx: &App) -> Div {
        let t = theme(cx);
        card("Badges and status", None, cx)
            .child(
                hrow()
                    .flex_wrap()
                    .gap(px(8.))
                    .child(badge("EN", BadgeKind::Plain, cx))
                    .child(badge("24", BadgeKind::Accent, cx))
                    .child(badge("mod+s", BadgeKind::Plain, cx))
                    .child(badge("rust", BadgeKind::Plain, cx))
                    .child(badge("dots", BadgeKind::Plain, cx))
                    .child(badge("S-1", BadgeKind::Plain, cx))
                    .child(badge("ok", BadgeKind::Ok, cx))
                    .child(badge("warn", BadgeKind::Warn, cx))
                    .child(badge("err", BadgeKind::Err, cx))
                    .child(badge("info", BadgeKind::Info, cx)),
            )
            .child(
                hrow()
                    .gap(px(18.))
                    .child(status(t.ok, "connected", cx))
                    .child(status(t.warn, "12% free", cx))
                    .child(status(t.err, "failed", cx))
                    .child(status(t.info, "syncing", cx)),
            )
    }

    fn menus(&self, cx: &App) -> Div {
        let m = menu(cx)
            .child(MenuItem::new("m1", "Open").icon(Icon::Folder).shortcut("↵").build(cx))
            .child(MenuItem::new("m2", "Open with").icon(Icon::Folder).submenu().selected(true).build(cx))
            .child(MenuItem::new("m3", "Rename").icon(Icon::Text).shortcut("F2").build(cx))
            .child(MenuItem::new("m4", "Copy path").icon(Icon::Clip).shortcut("ctrl+⇧+c").build(cx))
            .child(MenuItem::new("m4b", "Duplicate").icon(Icon::Copy).disabled(true).build(cx))
            .child(menu_separator(cx))
            .child(MenuItem::new("m5", "Pin to sidebar").icon(Icon::Pin).build(cx))
            .child(MenuItem::new("m6", "Move to trash").icon(Icon::Trash).shortcut("del").destructive().build(cx));
        card("Menu and tooltip", None, cx).child(
            hrow().items_start().gap(px(24.)).child(m).child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .items_start()
                    .child(tooltip("Fit all windows", Some("mod+0".into()), cx))
                    .child(label("tooltip · inverted", cx)),
            ),
        )
    }

    fn windows(&self, cx: &App) -> Div {
        let t = theme(cx);
        let code = |focused: bool, lines: &[(&str, bool)]| {
            let mut body = div().flex().flex_col().p(px(8.)).font_family(t.font_mono.clone()).text_size(px(12.)).line_height(px(19.));
            for (ix, (text, hot)) in lines.iter().enumerate() {
                body = body.child(
                    div()
                        .flex()
                        .gap(px(14.))
                        .when(*hot, |d| d.bg(t.line))
                        .child(div().w(px(22.)).flex_none().text_right().text_color(if *hot { t.fg } else { t.syn_comment }).child(format!("{}", ix + 1)))
                        .child(div().text_color(if text.starts_with("//") { t.syn_comment } else { t.fg }).child(text.to_string())),
                );
            }
            window_frame(focused, cx)
                .h(px(170.))
                .child(window_titlebar(if focused { "nvim" } else { "kitty" }, "config.kdl  [+]", cx))
                .child(body)
        };
        card("Window", None, cx).child(
            hrow()
                .items_start()
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(code(true, &[("// ~/.config/niri/config.kdl", false), ("layout {", false), ("    gaps 12", false), ("    proportion 0.5", true), ("}", false)]))
                        .child(label("focused · accent frame", cx)),
                )
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(code(false, &[("M niri/config.kdl", false), ("M ags/scratchpad.ts", false), ("~/dots ❯", false)]))
                        .child(label("unfocused · frame border", cx)),
                ),
        )
    }

    fn list_rows(&self, cx: &App) -> Div {
        let t = theme(cx);
        let simple = |ix: u32, selected: bool, ic: Icon, text: &str, n: &str| {
            list_row(("lr", ix), selected, cx)
                .child(icon(ic, 14.).text_color(if selected { t.fg } else { t.fg_muted }))
                .child(div().flex_1().child(text.to_string()))
                .child(label(n.to_string(), cx))
        };
        let rich = |ix: u32, selected: bool, ic: Icon, text: &str, pinned: bool, age: &str| {
            rich_row(("rr", ix), selected, cx)
                .child(icon(ic, 14.).text_color(t.fg_muted))
                .child(rich_row_text(text.to_string(), cx))
                .when(pinned, |d| d.child(icon(Icon::Pin, 12.).text_color(t.fg)))
                .child(label(age.to_string(), cx))
        };
        card("List rows", None, cx)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(simple(0, false, Icon::Term, "kitty — ~/dots", "2"))
                    .child(simple(1, true, Icon::Term, "nvim — config.kdl", "4"))
                    .child(simple(2, false, Icon::Globe, "Firefox — wiki", "5")),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(rich(0, true, Icon::Text, "niri msg action focus-column-right", true, "2m"))
                    .child(rich(1, false, Icon::Folder, "~/dots/niri/config.kdl", false, "14m"))
                    .child(rich(2, false, Icon::Text, "preset-column-widths { proportion 0.5 }", true, "2h")),
            )
    }

    fn toasts(&self, cx: &App) -> Div {
        card("Toast", Some("lives above the status bar"), cx).child(
            Toast::new(Icon::Chat, "pushed the minimap")
                .source("matrix · #ashd")
                .body("off-screen windows now render as outlines")
                .action(button_small("t-reply", "Reply", ButtonKind::Secondary, cx))
                .action(button_small("t-open", "Open", ButtonKind::Ghost, cx))
                .timer(0.64)
                .build(cx),
        )
    }

    fn page_components(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let (c1, c2) = (self.buttons(cx), self.inputs(cx));
        let (c3, c4, c5, c6) = (self.toggles_meters(window, cx), self.segments_tabs(cx), self.badges(cx), self.menus(cx));
        let (c7, c8, c9) = (self.windows(cx), self.list_rows(cx), self.toasts(cx));
        div()
            .flex()
            .flex_col()
            .gap(px(32.))
            .child(page_header(
                "Components",
                "Ash. Every state in one place: default, hover, pressed, focus, disabled. Keyboard focus is a 1px outline with a 2px offset.",
                cx,
            ))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(16.))
                    .child(column().child(c1).child(c2))
                    .child(column().child(c3).child(c4).child(c5).child(c6))
                    .child(column().child(c7).child(c8).child(c9)),
            )
    }

    // ── page 2: the rest of the kit ───────────────────────────────────────────

    fn page_more(&mut self, cx: &mut Context<Self>) -> Div {
        let t = theme(cx).clone();
        let checks = (0..2).map(|ix| {
            hrow().gap(px(8.)).child(checkbox(
                ("chk", ix as u32),
                self.checks[ix],
                cx.listener(move |this, on: &bool, _, cx| {
                    this.checks[ix] = (*on).into();
                    cx.notify();
                }),
                cx,
            )).child(div().text_size(px(12.5)).child(if ix == 0 { "Show hidden files" } else { "Follow symlinks" }))
        });
        let radios = (0..3).map(|ix| {
            hrow().gap(px(8.)).child(radio(
                ("rad", ix as u32),
                self.radio == ix,
                cx.listener(move |this, _, _, cx| {
                    this.radio = ix;
                    cx.notify();
                }),
                cx,
            )).child(div().text_size(px(12.5)).child(["Name", "Size", "Modified"][ix]))
        });
        let spinners = card("Spinner and progress", None, cx)
            .child(hrow().gap(px(20.))
                .child(spinner("sp14", 14., t.fg))
                .child(spinner("sp16", 16., t.accent))
                .child(spinner("sp22", 22., t.fg_muted)))
            .child(div().flex().flex_col().gap(px(10.))
                .child(hrow().child(label("2", cx)).child(div().flex_1().child(progress(0.64, metrics::PROGRESS_THIN, cx))))
                .child(hrow().child(label("3", cx)).child(div().flex_1().child(progress(0.4, metrics::PROGRESS_MID, cx))))
                .child(hrow().child(label("4", cx)).child(div().flex_1().child(progress(0.85, metrics::PROGRESS_THICK, cx)))));
        let controls = card("Checkbox, radio, kbd", None, cx)
            .child(hrow().items_start().gap(px(32.))
                .child(div().flex().flex_col().gap(px(10.)).children(checks))
                .child(div().flex().flex_col().gap(px(10.)).children(radios)))
            .child(hrow().gap(px(6.))
                .child(keycap("mod", cx)).child(keycap("shift", cx)).child(keycap("q", cx))
                .child(ashd_ui::kbd_combo(["ctrl", "alt", "del"], cx)))
            .child(hrow().gap(px(12.))
                .child(icon_button("old-ib", Icon::Settings, cx).ashd_tooltip("Settings (hover)", Some("mod+,")))
                .child(button("tt-btn", "Hover for tooltip", ButtonKind::Secondary, cx).ashd_tooltip("Inverted tooltip", None)));
        let png = self.png.clone();
        let svg = self.svg.clone();
        let apps = card("Application icons", Some("png · svg · fallback"), cx).child(
            hrow().gap(px(24.))
                .child(div().flex().flex_col().items_center().gap(px(6.)).child(app_icon(png.as_deref(), 32., cx)).child(label("png", cx)))
                .child(div().flex().flex_col().items_center().gap(px(6.)).child(app_icon(svg.as_deref(), 32., cx)).child(label("svg", cx)))
                .child(div().flex().flex_col().items_center().gap(px(6.)).child(app_icon(Some(std::path::Path::new("/nonexistent/icon.png")), 32., cx)).child(label("missing", cx)))
                .child(div().flex().flex_col().items_center().gap(px(6.)).child(app_icon(None, 32., cx)).child(label("none", cx))),
        );
        let empty = card("Empty state", None, cx).child(
            div().h(px(220.)).border_1().border_color(t.line).child(empty_state(
                Icon::Folder,
                "Nothing here",
                "Drop files here or create a new one.",
                vec![
                    button_small("e1", "New file", ButtonKind::Secondary, cx).into_any_element(),
                    button_small("e2", "New folder", ButtonKind::Ghost, cx).into_any_element(),
                ],
                cx,
            )),
        );
        let popover = card("Popover panel", Some("overlay shadow"), cx).child(
            div().h(px(200.)).p(px(24.)).bg(t.bg).child(
                overlay_panel(cx).p(px(14.)).w(px(260.)).flex().flex_col().gap(px(8.))
                    .child(title("Quick settings", cx))
                    .child(hline(cx))
                    .child(div().text_size(px(12.5)).text_color(t.fg_muted).child("Menus use the flyout shadow, this panel the overlay one.")),
            ),
        );
        let big = card("Numbers", None, cx)
            .child(hrow().gap(px(24.)).child(numeric("14:51", cx)).child(numeric("86%", cx)).child(numeric("9.4G", cx)))
            .child(hrow().gap(px(24.)).child(label("scroll · col 3/5 · 62%", cx)).child(mono("center-focused-column \"always\"", cx)));
        div()
            .flex()
            .flex_col()
            .gap(px(32.))
            .child(page_header("More components", "Spinner, progress, empty state, application icons, popover panel, checkbox and radio, tooltips, keyboard chips.", cx))
            .child(div().flex().items_start().gap(px(16.))
                .child(column().child(spinners).child(controls).child(big))
                .child(column().child(apps).child(empty))
                .child(column().child(popover)))
    }

    // ── page 3: icons ─────────────────────────────────────────────────────────

    fn page_icons(&self, cx: &App) -> Div {
        let t = theme(cx);
        let ramp = hrow().gap(px(24.)).children([13., 14., 16., 18., 22.].map(|s| {
            div().flex().flex_col().items_center().gap(px(8.)).child(icon(Icon::Folder, s).text_color(t.fg)).child(label(format!("{s}px"), cx))
        }));
        let cell = |ic: Icon| {
            let name = ic.path().trim_start_matches("icons/").trim_end_matches(".svg").to_string();
            div()
                .w(px(160.))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(10.))
                .py(px(18.))
                .px(px(8.))
                .border_1()
                .border_color(t.line)
                .bg(t.surface)
                .child(icon(ic, 22.).text_color(t.fg))
                .child(
                    hrow().gap(px(10.)).child(icon(ic, 14.).text_color(t.fg_muted)).child(icon(ic, 16.).text_color(t.accent)),
                )
                .child(label(name, cx))
        };
        div()
            .flex()
            .flex_col()
            .gap(px(24.))
            .child(page_header("Icons", &format!("{} icons. 24 grid, 1.6 stroke, round caps and joins, currentColor. 13–14 in dense rows, 16 in status and menus, 18 in the dock, 22 in empty states.", Icon::ALL.len()), cx))
            .child(ramp)
            .child(div().flex().flex_wrap().gap(px(8.)).children(Icon::ALL.iter().map(|ic| cell(*ic))))
    }
}

impl Render for Gallery {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let page = match self.page {
            2 => self.page_more(cx),
            3 => self.page_icons(cx),
            _ => self.page_components(window, cx),
        };
        div()
            .id("gallery")
            .key_context("Gallery")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .bg(t.bg)
            .text_color(t.fg)
            .font_family(t.font_sans.clone())
            .text_size(px(13.))
            .child(
                div()
                    .id("scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .p(px(56.))
                    .child(page)
                    .child(
                        div().mt(px(24.)).child(label(
                            format!("theme {} · t theme · 1/2/3 pages · s scroll · m reduced motion ({})", t.name, anim::reduced_motion()),
                            cx,
                        )),
                    ),
            )
    }
}

/// The second window: proves a crossfade re-renders every open window.
struct Mirror;

impl Render for Mirror {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        div()
            .size_full()
            .bg(t.bg)
            .text_color(t.fg)
            .font_family(t.font_sans.clone())
            .p(px(16.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(title(format!("second window · {}", t.name), cx))
            .child(
                div().flex().gap(px(6.)).children(
                    [t.surface, t.surface_2, t.line, t.accent, t.ok, t.warn, t.err, t.info]
                        .map(|c| div().size(px(24.)).bg(c).border_1().border_color(t.line)),
                ),
            )
            .child(mono(format!("{} / {}", t.font_sans, t.font_mono), cx))
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(button("mirror-p", "Primary", ButtonKind::Primary, cx))
                    .child(button("mirror-s", "Secondary", ButtonKind::Secondary, cx)),
            )
    }
}

fn main() {
    let theme_name = std::env::var("GALLERY_THEME").ok().and_then(|v| v.parse().ok()).unwrap_or(ThemeName::Ash);
    gpui_platform::application().with_assets(ashd_ui::Assets).run(move |cx: &mut App| {
        ashd_ui::init(theme_name, cx);
        let bounds = Bounds::centered(None, size(px(1840.), px(1040.)), cx);
        cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
            |window, cx| {
                let gallery = cx.new(|cx| Gallery::new(theme_name, cx));
                window.focus(&gallery.focus_handle(cx), cx);
                gallery
            },
        )
        .expect("open gallery window");
        let small = Bounds { origin: point(px(40.), px(40.)), size: size(px(420.), px(190.)) };
        cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(small)), ..Default::default() },
            |_, cx| cx.new(|_| Mirror),
        )
        .expect("open mirror window");
        cx.activate(true);
    });
}
