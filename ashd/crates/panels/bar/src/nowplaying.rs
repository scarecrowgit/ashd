//! Now-playing control (status area, at the head of the tray area): shown only while the
//! MPRIS service has an active player — music glyph + truncated title, accent while
//! playing. Click toggles the media popup (anchored here; the bar publishes the `media`
//! named anchor), middle click play/pause, wheel next / previous. It shares the tray's
//! width budget ([`crate::tray::plan_row`]): the title shrinks, then disappears, before
//! any tray icon moves behind the chevron.

use gpui::{
    ClickEvent, Context, Div, InteractiveElement as _, ParentElement as _, ScrollDelta, ScrollEvent,
    Stateful, StatefulInteractiveElement as _, Styled as _, div, prelude::*, px,
};
use ashd_ipc::{PanelOp, Request};
use ashd_svc_mpris::MprisState;
use ashd_ui::{Icon, Theme, icon};

use crate::{BarPanel, caps, geom, hover_layer, ids, is_open, open_layer, tray::MediaFit};

/// What the control shows: the track title, else the player's name.
pub fn now_playing_label(state: &MprisState) -> Option<(String, bool)> {
    let p = state.active_player()?;
    let title = if p.track.title.trim().is_empty() { p.display_name().to_string() } else { p.track.title.trim().to_string() };
    Some((title, p.is_playing()))
}

impl BarPanel {
    pub(crate) fn has_now_playing(&self, cx: &gpui::App) -> bool {
        self.ctx.services.mpris.state.read(cx).active_player().is_some()
    }

    /// The control for `fit` (`None` when there is no active player).
    pub(crate) fn now_playing(&mut self, fit: MediaFit, t: &Theme, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let Some((title, playing)) = now_playing_label(self.ctx.services.mpris.state.read(cx)) else {
            self.bounds.forget("media", cx);
            return None;
        };
        if fit == MediaFit::None {
            self.bounds.forget("media", cx);
            return None;
        }
        let open = is_open(ids::MEDIA, &self.ctx, cx);
        let color = if playing { t.accent } else { t.fg_muted };
        let el = div()
            .id("bar-now-playing")
            .relative()
            .flex_none()
            .h(px(geom::TRAY_BOX))
            .min_w(px(geom::NOW_PLAYING_GLYPH_W))
            .px(px((geom::NOW_PLAYING_GLYPH_W - geom::NOW_PLAYING_ICON) / 2.))
            .flex()
            .items_center()
            .gap(px(geom::NOW_PLAYING_TITLE_GAP))
            .rounded(t.radius_control)
            .cursor_pointer()
            .child(open_layer(open, t))
            .when(!open, |d| d.child(hover_layer(t)))
            .child(icon(Icon::Music, geom::NOW_PLAYING_ICON).text_color(color))
            .when_some(if let MediaFit::Title(w) = fit { Some(w) } else { None }, |d, w| {
                d.child(
                    caps(title, geom::VALUE_SIZE, color, t)
                        .max_w(px(w))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis(),
                )
            })
            .child(self.bounds.track("media"))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                let anchor = this.bounds.anchor("media", &this.ctx, cx);
                this.ctx.shell.request(Request::Media { op: PanelOp::Toggle }, anchor, cx);
            }))
            .on_aux_click(cx.listener(|this, ev: &ClickEvent, _, cx| {
                if ev.is_middle_click() {
                    this.ctx.services.mpris.handle.play_pause();
                    cx.notify();
                }
            }))
            .on_scroll(cx.listener(|this, ev: &ScrollEvent, _, _| this.media_scroll(ev)));
        Some(el)
    }

    /// The control as it was, for the moment it fades out after the player went (no
    /// handlers: it is already gone).
    pub(crate) fn now_playing_ghost(title: String, playing: bool, fit: MediaFit, t: &Theme) -> Div {
        let color = if playing { t.accent } else { t.fg_muted };
        div()
            .relative()
            .flex_none()
            .h(px(geom::TRAY_BOX))
            .min_w(px(geom::NOW_PLAYING_GLYPH_W))
            .px(px((geom::NOW_PLAYING_GLYPH_W - geom::NOW_PLAYING_ICON) / 2.))
            .flex()
            .items_center()
            .gap(px(geom::NOW_PLAYING_TITLE_GAP))
            .child(icon(Icon::Music, geom::NOW_PLAYING_ICON).text_color(color))
            .when_some(if let MediaFit::Title(w) = fit { Some(w) } else { None }, |d, w| {
                d.child(caps(title, geom::VALUE_SIZE, color, t).max_w(px(w)).overflow_hidden().whitespace_nowrap().text_ellipsis())
            })
    }

    /// Wheel: one notch (or `TRAY_SCROLL_PIXELS` of touchpad travel) = one track; up =
    /// previous, down = next.
    fn media_scroll(&mut self, ev: &ScrollEvent) {
        let notches = match ev.delta {
            ScrollDelta::Lines(l) => l.y,
            ScrollDelta::Pixels(p) => f32::from(p.y) / geom::TRAY_SCROLL_PIXELS,
        };
        self.state.media_scroll += notches;
        let whole = self.state.media_scroll.trunc();
        if whole == 0.0 {
            return;
        }
        self.state.media_scroll -= whole;
        let mpris = &self.ctx.services.mpris.handle;
        if whole > 0.0 { mpris.previous() } else { mpris.next() }
    }
}

#[cfg(test)]
mod tests {
    use ashd_svc_mpris::{PlaybackStatus, Player};

    use super::*;

    #[test]
    fn label_is_the_title_else_the_player() {
        let mut p = Player { bus_name: "org.mpris.MediaPlayer2.mpd".into(), ..Default::default() };
        let mut s = MprisState { available: true, players: vec![p.clone()], active: None, pinned: None };
        assert_eq!(now_playing_label(&s), None, "no active player, no control");
        s.active = Some(p.bus_name.clone());
        assert_eq!(now_playing_label(&s), Some(("mpd".into(), false)));
        p.track.title = "Song".into();
        p.status = PlaybackStatus::Playing;
        s.players = vec![p];
        assert_eq!(now_playing_label(&s), Some(("Song".into(), true)));
    }
}
