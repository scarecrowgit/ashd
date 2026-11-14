//! M0 GPUI spike: proves the layer-shell features ashd needs on niri.
//! Results: docs/GPUI-NOTES.md. Run ONLY inside the nested session
//! (`cargo xtask nested run -- …`), never on the live desktop.
//!
//! Driven by commands on stdin (one per line) so an external script can step through
//! the items and take screenshots in between:
//!
//!   modal-open | modal-close | passive-open | passive-close | popup | popup-close
//!   scratch-resize <w> | scratch-zone <px> | scratch-close | scratch-open
//!   fps | normal | displays | quit
//!
//! Everything the spike observes is printed as `SPIKE <item> …` lines on stdout.

use std::{
    io::BufRead,
    time::{Duration, Instant},
};

use gpui::{
    AnyWindowHandle, App, Bounds, Context, DisplayId, Entity, FocusHandle, Focusable, KeyBinding,
    QuitMode, SharedString, Size, Window, WindowBackgroundAppearance, WindowBounds, WindowHandle,
    WindowKind, WindowOptions, actions, div, layer_shell::*, point,
    popup::{PopupAnchor, PopupConstraintAdjustment, PopupGravity, PopupOptions},
    prelude::*, px, size,
};
use ashd_tokens::{ThemeName, metrics};
use ashd_ui::{ButtonKind, Icon, TextInput, TextInputEvent, button, icon, label, theme};

actions!(spike, [Dismiss]);

/// State shared by every surface (item 1).
struct Shared {
    clicks: u32,
    typed: SharedString,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Bar,
    Rail,
    Scratch,
    Modal,
    Passive,
    Popup,
    Normal,
}

struct Surface {
    kind: Kind,
    shared: Entity<Shared>,
    input: Option<Entity<TextInput>>,
    focus: FocusHandle,
    /// fps probe: frames counted since `fps_since` while `fps_left` > 0.
    fps_frames: u32,
    fps_since: Option<Instant>,
}

impl Surface {
    fn new(kind: Kind, shared: Entity<Shared>, cx: &mut Context<Self>) -> Self {
        cx.observe(&shared, |_, _, cx| cx.notify()).detach();
        let input = matches!(kind, Kind::Modal | Kind::Scratch).then(|| {
            let input = cx.new(|cx| TextInput::new(format!("{kind:?} input"), cx));
            cx.subscribe(&input, move |this, input, ev: &TextInputEvent, cx| {
                let text = input.read(cx).text().to_string();
                println!("SPIKE item3 {:?} event={ev:?} text={text:?}", this.kind);
                this.shared.update(cx, |s, cx| {
                    s.typed = text.into();
                    cx.notify();
                });
                if this.kind == Kind::Modal && *ev == TextInputEvent::Cancel {
                    // item 7: closing the last-but-not-only window must not quit.
                    cx.defer(|cx| {
                        if let Some(w) = cx.active_window() {
                            let _ = w.update(cx, |_, window, _| window.remove_window());
                        }
                    });
                    println!("SPIKE item7 modal closed on escape");
                }
            })
            .detach();
            input
        });
        Self { kind, shared, input, focus: cx.focus_handle(), fps_frames: 0, fps_since: None }
    }
}

impl Focusable for Surface {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Surface {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let shared = self.shared.read(cx);
        let status: SharedString = format!("clicks {} · typed {:?}", shared.clicks, shared.typed).into();
        let kind = self.kind;

        if let Some(since) = self.fps_since {
            self.fps_frames += 1;
            if since.elapsed() >= Duration::from_secs(2) {
                println!(
                    "SPIKE fps {kind:?} {:.1} frames/s (refresh_interval {:?})",
                    self.fps_frames as f32 / since.elapsed().as_secs_f32(),
                    window.refresh_interval(cx)
                );
                self.fps_since = None;
            } else {
                window.request_animation_frame();
            }
        }

        let click = cx.listener(move |this, _, _, cx| {
            this.shared.update(cx, |s, cx| {
                s.clicks += 1;
                cx.notify();
            });
            println!("SPIKE item1 click on {:?}", this.kind);
        });

        let base = div().size_full().text_color(t.fg).font_family(t.font_sans.clone()).text_size(px(12.5));
        match kind {
            Kind::Bar => base
                .flex()
                .items_center()
                .gap(px(12.))
                .px(px(14.))
                .bg(t.surface)
                .border_t_1()
                .border_color(t.line)
                .child(label("bar · 46", cx))
                .child(icon(Icon::Wifi, 16.).text_color(t.fg))
                .child(icon(Icon::Volume, 16.).text_color(t.fg))
                .child(ashd_ui::numeric("14:51", cx))
                .child(button("bar-btn", "Click", ButtonKind::Primary, cx).on_click(click))
                .child(div().child(status)),
            Kind::Rail => base
                .flex()
                .flex_col()
                .items_center()
                .gap(px(6.))
                .pt(px(12.))
                .bg(t.surface)
                .border_r_1()
                .border_color(t.line)
                .child(label("rail", cx))
                .children((1u32..=4).map(|n| {
                    div()
                        .id(("ws", n))
                        .size(px(metrics::RAIL_BUTTON))
                        .flex()
                        .items_center()
                        .justify_center()
                        .border_1()
                        .border_color(if n == 2 { t.accent } else { t.line })
                        .font_family(t.font_mono.clone())
                        .child(format!("{n:02}"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            println!("SPIKE item4 rail button {n} clicked (through overlay?)");
                            this.shared.update(cx, |s, cx| {
                                s.clicks += 1;
                                cx.notify();
                            });
                        }))
                })),
            Kind::Scratch => base
                .flex()
                .flex_col()
                .gap(px(10.))
                .p(px(14.))
                .bg(t.surface)
                .border_l_1()
                .border_color(t.line)
                .child(ashd_ui::title("Scratchpad", cx))
                .child(label("on-demand keyboard", cx))
                .child(
                    div()
                        .id("scratch-input")
                        .on_mouse_down(gpui::MouseButton::Left, {
                            let input = self.input.clone();
                            move |_, window, cx| {
                                if let Some(input) = &input {
                                    input.focus_handle(cx).focus(window, cx);
                                    println!("SPIKE item3 scratch input focused by click");
                                }
                            }
                        })
                        .h(px(34.))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .border_1()
                        .border_color(t.line)
                        .bg(t.surface_2)
                        .children(self.input.clone()),
                )
                .child(div().child(status)),
            Kind::Modal => base
                .flex()
                .items_start()
                .justify_center()
                .pt(px(180.))
                .bg(t.scrim_power())
                .key_context("SpikeModal")
                .track_focus(&self.focus)
                .on_action(cx.listener(|_, _: &Dismiss, window, _| {
                    println!("SPIKE item7 modal dismissed via escape");
                    window.remove_window();
                }))
                .child(
                    ashd_ui::overlay_panel(cx)
                        .w(px(metrics::LAUNCHER_W))
                        .p(px(12.))
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .occlude()
                        .child(label("launcher · exclusive keyboard", cx))
                        .child(
                            div()
                                .h(px(34.))
                                .px(px(10.))
                                .flex()
                                .items_center()
                                .border_1()
                                .border_color(t.accent)
                                .children(self.input.clone()),
                        )
                        .child(div().child(status)),
                ),
            Kind::Passive => base.child(
                div()
                    .id("passive-rect")
                    .absolute()
                    .left(px(800.))
                    .top(px(300.))
                    .w(px(300.))
                    .h(px(200.))
                    .bg(ashd_ui::alpha(t.accent, 0.5))
                    .border_1()
                    .border_color(t.accent)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child("passive · input region")
                    .on_click(|_, _, _| println!("SPIKE item4 passive rect clicked (inside input region)")),
            ),
            Kind::Popup => base
                .bg(t.surface)
                .border_1()
                .border_color(t.frame)
                .p(px(12.))
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(label("anchored popup", cx))
                .child("parented to the bar")
                .child(button("popup-btn", "Popup click", ButtonKind::Secondary, cx).on_click(click)),
            Kind::Normal => base
                .bg(t.surface_2)
                .p(px(20.))
                .child(ashd_ui::title("normal xdg window", cx))
                .child(status),
        }
    }
}

struct Spike {
    shared: Entity<Shared>,
    display: Option<DisplayId>,
    bar: Option<WindowHandle<Surface>>,
    rail: Option<WindowHandle<Surface>>,
    scratch: Option<WindowHandle<Surface>>,
    modal: Option<WindowHandle<Surface>>,
    passive: Option<WindowHandle<Surface>>,
    popup: Option<WindowHandle<Surface>>,
    normal: Option<WindowHandle<Surface>>,
}

fn layer(
    namespace: &str,
    layer: Layer,
    anchor: Anchor,
    size: Size<gpui::Pixels>,
    zone: Option<f32>,
    keyboard: KeyboardInteractivity,
    display: Option<DisplayId>,
) -> WindowOptions {
    WindowOptions {
        titlebar: None,
        window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.), px(0.)), size })),
        app_id: Some("ashd-spike".into()),
        window_background: WindowBackgroundAppearance::Transparent,
        display_id: display,
        // Layer surfaces never count as "active": without this GPUI throttles them to
        // 30 fps (inactive_frame_interval defaults to 33 ms).
        inactive_frame_interval: None,
        low_power_frame_interval: None,
        thermal_frame_interval: None,
        kind: WindowKind::LayerShell(LayerShellOptions {
            namespace: namespace.into(),
            layer,
            anchor,
            exclusive_zone: zone.map(px),
            exclusive_edge: None,
            margin: None,
            keyboard_interactivity: keyboard,
        }),
        ..Default::default()
    }
}

impl Spike {
    fn open(&self, kind: Kind, cx: &mut App) -> anyhow::Result<WindowHandle<Surface>> {
        let d = self.display;
        let full = Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT;
        let opts = match kind {
            Kind::Bar => layer(
                "ashd-bar",
                Layer::Top,
                Anchor::LEFT | Anchor::RIGHT | Anchor::BOTTOM,
                size(px(0.), px(metrics::BAR_H)),
                Some(metrics::BAR_H),
                KeyboardInteractivity::None,
                d,
            ),
            Kind::Rail => layer(
                "ashd-rail",
                Layer::Top,
                Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT,
                size(px(metrics::RAIL_W), px(0.)),
                Some(metrics::RAIL_W),
                KeyboardInteractivity::None,
                d,
            ),
            Kind::Scratch => layer(
                "ashd-scratchpad",
                Layer::Top,
                Anchor::TOP | Anchor::BOTTOM | Anchor::RIGHT,
                size(px(metrics::SCRATCH_W), px(0.)),
                Some(metrics::SCRATCH_W),
                KeyboardInteractivity::OnDemand,
                d,
            ),
            Kind::Modal => {
                layer("ashd-overlay-modal", Layer::Overlay, full, size(px(0.), px(0.)), Some(-1.), KeyboardInteractivity::Exclusive, d)
            }
            Kind::Passive => {
                layer("ashd-overlay-passive", Layer::Overlay, full, size(px(0.), px(0.)), Some(-1.), KeyboardInteractivity::None, d)
            }
            Kind::Popup => {
                let parent: AnyWindowHandle = self.bar.ok_or_else(|| anyhow::anyhow!("no bar"))?.into();
                WindowOptions {
                    titlebar: None,
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: point(px(0.), px(0.)),
                        size: size(px(300.), px(160.)),
                    })),
                    window_background: WindowBackgroundAppearance::Opaque,
                    inactive_frame_interval: None,
                    kind: WindowKind::AnchoredPopup(PopupOptions {
                        parent,
                        // the "Click" button region of the bar, bar-local coords
                        anchor_rect: Bounds { origin: point(px(200.), px(0.)), size: size(px(60.), px(46.)) },
                        anchor: PopupAnchor::TopLeft,
                        gravity: PopupGravity::TopRight,
                        constraint_adjustment: PopupConstraintAdjustment::SLIDE_X
                            | PopupConstraintAdjustment::FLIP_Y,
                        offset: point(px(0.), px(-8.)),
                        grab: false,
                    }),
                    ..Default::default()
                }
            }
            Kind::Normal => WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.), px(0.)),
                    size: size(px(800.), px(600.)),
                })),
                app_id: Some("ashd-spike-normal".into()),
                display_id: d,
                ..Default::default()
            },
        };
        let shared = self.shared.clone();
        let started = Instant::now();
        let handle = cx.open_window(opts, move |window, cx| {
            let view = cx.new(|cx| Surface::new(kind, shared, cx));
            if matches!(kind, Kind::Modal) {
                let input = view.read(cx).input.clone();
                if let Some(input) = input {
                    input.focus_handle(cx).focus(window, cx);
                }
            }
            view
        })?;
        println!("SPIKE open {kind:?} in {:?}", started.elapsed());
        if matches!(kind, Kind::Passive) {
            // item 4: only this rectangle receives pointer input.
            handle.update(cx, |_, window, _| {
                window.set_input_region(Some(&[Bounds {
                    origin: point(px(800.), px(300.)),
                    size: size(px(300.), px(200.)),
                }]))
            })?;
        }
        Ok(handle)
    }

    fn close(handle: &mut Option<WindowHandle<Surface>>, cx: &mut App) {
        if let Some(h) = handle.take() {
            let _ = h.update(cx, |_, window, _| window.remove_window());
        }
    }
}

fn command(spike: &Entity<Spike>, line: &str, cx: &mut App) {
    let mut parts = line.split_whitespace();
    let cmd = parts.next().unwrap_or("");
    let arg: f32 = parts.next().and_then(|a| a.parse().ok()).unwrap_or(0.0);
    let arg2: f32 = parts.next().and_then(|a| a.parse().ok()).unwrap_or(0.0);
    println!("SPIKE cmd {line}");
    spike.update(cx, |s, cx| {
        let res: anyhow::Result<()> = (|| {
            match cmd {
                "modal-open" => {
                    Spike::close(&mut s.modal, cx);
                    s.modal = Some(s.open(Kind::Modal, cx)?)
                }
                "modal-close" => Spike::close(&mut s.modal, cx),
                "passive-open" => s.passive = Some(s.open(Kind::Passive, cx)?),
                "passive-close" => Spike::close(&mut s.passive, cx),
                "popup" => s.popup = Some(s.open(Kind::Popup, cx)?),
                "popup-close" => Spike::close(&mut s.popup, cx),
                "normal" => s.normal = Some(s.open(Kind::Normal, cx)?),
                "scratch-close" => Spike::close(&mut s.scratch, cx),
                "scratch-open" => s.scratch = Some(s.open(Kind::Scratch, cx)?),
                "scratch-resize" => {
                    if let Some(h) = s.scratch {
                        h.update(cx, |_, window, _| window.resize(size(px(arg), px(arg2))))?;
                    }
                }
                "scratch-zone" => {
                    if let Some(h) = s.scratch {
                        h.update(cx, |_, window, _| window.set_exclusive_zone(px(arg)))?;
                    }
                }
                "fps" => {
                    for h in [s.bar, s.rail, s.scratch, s.normal].into_iter().flatten() {
                        // (request_animation_frame may only be called while drawing:
                        // notify, and the render keeps requesting frames.)
                        h.update(cx, |view, _, cx| {
                            view.fps_frames = 0;
                            view.fps_since = Some(Instant::now());
                            cx.notify();
                        })?;
                    }
                }
                "displays" => print_displays(cx),
                "quit" => cx.quit(),
                "" => {}
                other => println!("SPIKE unknown command {other}"),
            }
            Ok(())
        })();
        if let Err(e) = res {
            println!("SPIKE error {cmd}: {e:#}");
        }
        println!("SPIKE windows open: {}", cx.windows().len());
    });
}

fn print_displays(cx: &App) {
    for d in cx.displays() {
        println!(
            "SPIKE item5 display id={:?} uuid={:?} bounds={:?} refresh={:?}",
            d.id(),
            d.uuid().ok(),
            d.bounds(),
            d.refresh_interval()
        );
    }
    // GPUI's Wayland display uuid is `Uuid::new_v5(NAMESPACE_DNS, wl_output.name)`, which
    // is how a GPUI display is matched to a niri output name.
    let expected = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, b"winit");
    println!("SPIKE item5 uuid_v5(\"winit\")={expected}");
}

fn main() {
    gpui_platform::application()
        .with_assets(ashd_ui::Assets)
        // item 7: never quit when the last window closes.
        .with_quit_mode(QuitMode::Explicit)
        .run(|cx: &mut App| {
            ashd_ui::init(ThemeName::Ash, cx);
            cx.bind_keys([KeyBinding::new("escape", Dismiss, Some("SpikeModal"))]);
            println!("SPIKE fonts: sans={} mono={}", theme(cx).font_sans, theme(cx).font_mono);
            print_displays(cx);
            let display = cx.displays().first().map(|d| d.id());
            let shared = cx.new(|_| Shared { clicks: 0, typed: "".into() });
            let spike = cx.new(|_| Spike {
                shared,
                display,
                bar: None,
                rail: None,
                scratch: None,
                modal: None,
                passive: None,
                popup: None,
                normal: None,
            });
            // Order matters: niri arranges exclusive zones in creation order, so the bar
            // (full width) comes first and the rail/scratchpad stop above it.
            spike.update(cx, |s, cx| {
                s.bar = s.open(Kind::Bar, cx).map_err(|e| println!("SPIKE error bar: {e:#}")).ok();
                s.rail = s.open(Kind::Rail, cx).map_err(|e| println!("SPIKE error rail: {e:#}")).ok();
                s.scratch = s.open(Kind::Scratch, cx).map_err(|e| println!("SPIKE error scratch: {e:#}")).ok();
            });

            let (tx, rx) = async_channel::unbounded::<String>();
            std::thread::spawn(move || {
                for line in std::io::stdin().lock().lines().map_while(Result::ok) {
                    if tx.send_blocking(line).is_err() {
                        break;
                    }
                }
            });
            cx.spawn(async move |cx| {
                while let Ok(line) = rx.recv().await {
                    cx.update(|cx| command(&spike, line.trim(), cx));
                }
            })
            .detach();
        });
}
