//! The theme row of the quick-settings panel (det-13): "Theme", `{NAME} · {sans} + {mono}`,
//! five 26×26 swatches and a "more" button that opens the theme switcher.
//!
//! Picking a swatch crossfades the shell now and runs the one shared theme sequence
//! (`ashd_theme::set`: persist → write the files of the consumers checked in the theme
//! switcher's "apply to" → reload unless `ASHD_NO_RELOAD` / `ASHD_FAKE_POWER`) in the
//! background — exactly what `ashd theme set` does.

use gpui::{
    App, ClickEvent, Div, ElementId, InteractiveElement as _, ParentElement as _, StatefulInteractiveElement as _,
    Styled as _, div, px,
};
use ashd_ipc::{PanelOp, Request};
use ashd_shell_core::ShellHandle;
use ashd_tokens::ThemeName;
use ashd_ui::{Icon, color, icon, icon_button_sized, theme};

/// Apply `name`: crossfade the shell now; persist + write files + reload consumers in
/// the background (`ashd_theme::set`, the persisted "apply to" selection).
pub fn select_theme(name: ThemeName, cx: &mut App) {
    ashd_ui::set_theme(name, true, cx);
    let task = cx.background_executor().spawn(async move { ashd_theme::set(name, true) });
    cx.spawn(async move |_| match task.await {
        Ok(applied) => {
            for w in applied.reload.iter().flat_map(|r| r.warnings()) {
                tracing::warn!("theme {name}: {w}");
            }
        }
        Err(e) => tracing::warn!("cannot persist theme {name}: {e}"),
    })
    .detach();
}

/// "More": the full theme switcher (cards, preview, "apply to").
pub fn open_switcher(shell: &ShellHandle, cx: &mut App) {
    shell.request(Request::ThemeSwitcher { op: PanelOp::Open }, None, cx);
}

/// `{NAME} · {sans} + {mono}` for the row's subtitle.
pub fn theme_caption(name: ThemeName) -> String {
    let fonts = &ashd_tokens::tokens(name).fonts;
    format!("{} · {} + {}", name.title(), fonts.sans, fonts.mono)
}

/// The row. `on_pick` receives the clicked theme, `on_more` the "more" click.
pub fn theme_row(
    current: ThemeName,
    on_pick: impl Fn(ThemeName, &mut App) + Clone + 'static,
    on_more: impl Fn(&mut App) + 'static,
    cx: &App,
) -> Div {
    let t = theme(cx).clone();
    let swatches = ThemeName::ALL.into_iter().enumerate().map(|(ix, name)| {
        let tokens = ashd_tokens::tokens(name);
        let selected = name == current;
        let accent = color(tokens.colors.accent);
        let on_pick = on_pick.clone();
        div()
            .id(ElementId::from(("theme-swatch", ix as u32)))
            .relative()
            .size(px(26.))
            .flex_none()
            .border_1()
            .border_color(if selected { accent } else { color(tokens.colors.line) })
            .bg(color(tokens.colors.bg))
            .rounded(t.radius_control)
            .cursor_pointer()
            .child(div().absolute().inset(px(4.)).bg(accent))
            .on_click(move |_: &ClickEvent, _, cx| on_pick(name, cx))
    });
    let more = icon_button_sized("theme-more", Icon::Dots, 26., 14., cx).on_click(move |_: &ClickEvent, _, cx| on_more(cx));
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .px(px(10.))
        .py(px(8.))
        .border_1()
        .border_color(t.line)
        .rounded(t.radius_control)
        .child(icon(Icon::Contrast, 15.).text_color(t.fg))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .font_family(t.font_sans.clone())
                        .text_size(px(ashd_tokens::metrics::type_scale::UI.0))
                        .text_color(t.fg)
                        .child("Theme"),
                )
                .child(
                    ashd_ui::label(theme_caption(current), cx)
                        .text_size(px(10.))
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis(),
                ),
        )
        .child(div().flex().gap(px(4.)).children(swatches))
        .child(more)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caption_names_the_theme_and_fonts() {
        let c = theme_caption(ThemeName::Ash);
        assert!(c.starts_with("Ash · "), "{c}");
        assert!(c.contains(" + "));
    }
}
