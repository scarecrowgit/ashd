# GPUI notes — pinned dependency, spike results, gotchas

Status: integration pass (all panels and services built). Owner: the integrator. Everything below was verified
on this machine (AMD RX 6650 XT, RADV/Vulkan) in the headless nested session described in
§7, never on the live desktop.

## 1. Pinned revision

| what | value |
|---|---|
| repo | `https://github.com/zed-industries/zed` |
| rev | **`69239eb54067293f1b646a7dd7f83fc4f62af728`** (`main`, 2026-11-13 15:12 UTC) |
| crates | `gpui` (`default-features = false, features = ["wayland"]`), `gpui_platform` (`default-features = false, features = ["wayland"]`) — both from the same rev, in `ashd/Cargo.toml` `[workspace.dependencies]` |
| toolchain | **nixpkgs rustc 1.97.1** (the flake's locked nixpkgs `56c02bc`, also what `<nixpkgs>` gives). Zed's own `rust-toolchain.toml` says 1.98.1, but nothing in gpui's dependency tree needs it: the whole tree builds warning-free with 1.97.1, so no rust-overlay/fenix pin is needed and the dev shell and the crane build use the same compiler. If a future bump fails with "feature X is unstable / requires rustc 1.98", pin the toolchain in `ashd/shell.nix` and `packages/ashd` together. |
| why this rev | the newest `main` at M0; it has everything the shell needs: `WindowKind::LayerShell` + `examples/layer_shell.rs`, `WindowKind::AnchoredPopup` (xdg_popup, also parented to layer surfaces), `Window::set_input_region`, `set_exclusive_zone`/`set_exclusive_edge`, `WindowOptions.display_id` honoured for layer surfaces (PR #50520), `QuitMode::Explicit`, per-window frame-interval knobs. crates.io gpui 0.2.2 has none of the layer-shell API. |

X11 is disabled (`gpui_linux` without `x11`): no libX11/xcb in the closure, and the shell is
Wayland-only anyway.

Layout of the gpui crates at this rev: `crates/gpui` (core, `gpui::layer_shell`,
`gpui::popup`), `crates/gpui_platform` (`gpui_platform::application()`), `crates/gpui_linux`
(Wayland client: `src/linux/wayland/{client,window,display}.rs`), `crates/gpui_wgpu`
(renderer: wgpu Vulkan/GL, cosmic-text + font-kit text). Read sources from
`~/.cargo/git/checkouts/zed-*/69239eb/crates/…` — never guess the API.

### `[patch.crates-io]` (replicated from Zed's root `Cargo.toml`)

Zed patches these for gpui's tree; `[patch]` does not propagate to consumers, so
`ashd/Cargo.toml` repeats them with the **same revs**:

| crate | source | why it matters for us |
|---|---|---|
| `async-task` | `zed-industries/async-task@015845a1` | gpui's scheduler |
| `async-process` | `zed-industries/async-process@0b6d6713` | smol/gpui process spawning |
| `calloop` | `zed-industries/calloop@3759371f` | gpui's Wayland event loop |
| `khronos-egl` | `zed-industries/khronos-egl@42214b36` | wgpu GL fallback (EGL_SUCCESS panic fix) |

Not replicated (not in gpui's tree): `cpal`, `tree-sitter-language`, `windows-capture`,
`livekit`, `libwebrtc`, `webrtc-sys`, `notify`, `notify-types`, `crash-context`, `scratch`.

**Bumping the rev**: change both `rev =` lines, re-copy the four patch revs from Zed's
`Cargo.toml` at the new rev, `cargo update -p gpui`, rebuild the spike
(`cargo build -p ashd-shell --example spike`) and re-run §2 and §6 in the session of §7. Only the integrator does this.

### Other dependency decisions

- **wgpu (two packages, deliberately).** gpui uses wgpu from git
  (`gfx-rs/wgpu@cddb2f5d`, version 30.0.0 with post-release API changes). vello 0.12 requires
  `wgpu ^30` but does **not** compile against that rev (`TextureViewDescriptor::swizzle` was
  added), so `[patch.crates-io] wgpu = <git>` is impossible. `ashd-gfx` (vello, for the lock
  screen/greeter) therefore uses crates.io wgpu 30.0.2. No binary links both: `ashd-shell`
  has gpui's wgpu, `ashd-lock`/`ashd-greeter` have vello's. Cost: one extra wgpu compile.
  Revisit when vello moves to a wgpu that matches gpui's rev.
- **zbus 5.** gpui's tree has zbus 5.19 (accesskit_unix, ashpd, oo7, atspi) and zbus 6.3
  (notify-rust only). Our services use zbus 5 to share the big copy.
- **niri-ipc** is vendored (`ashd/vendor/niri-ipc`, GPL-3.0-or-later) — see ARCHITECTURE.md.

## 2. Spike gate — `ashd/crates/ashd-shell/examples/spike.rs`

Driven over stdin (`modal-open`, `passive-open`, `popup`, `scratch-resize W H`,
`scratch-zone N`, `fps`, …), clicking with `wlrctl` and typing with `wtype` (at M0 from a
throwaway driver; today `cargo xtask nested run -- …`)
(niri implements virtual-pointer/virtual-keyboard). Screenshots: `ashd/target-notes/spike-*.png`,
log: `ashd/target-notes/spike-output.txt`.

| # | item | result | evidence |
|---|---|---|---|
| 1 | 3 layer surfaces in one process sharing an entity (bar L\|R\|B 46, rail T\|B\|L 64, scratchpad T\|B\|R 340) | **PASS** | clicks on rail, bar and popup bump one shared counter shown on every surface (`spike-09a-resize.png`: "clicks 4" everywhere) |
| 2 | exclusive zones respected by niri | **PASS** | normal xdg window tiled at x≥64+12, 740×1010 (= 1516×1034 work area, gaps 12) — `spike-02-normal.png`, `niri msg --json windows` |
| 3 | `KeyboardInteractivity::Exclusive` text input; OnDemand | **PASS** both | modal: `wtype "hello ashd"` → `TextInputEvent::Changed` per char, Escape → `Cancel`; scratchpad (OnDemand): niri focuses it on click, typing reaches the `TextInput` (`spike-03b-ondemand.png`) |
| 4 | `set_input_region` click-through overlay | **PASS** | full-output overlay with region 300×200: click inside → overlay handler; click on the rail outside the region → rail handler fires |
| 5 | output targeting via `display_id` | **PASS** (1 output) | `cx.displays()` lists `DisplayId(4)`, bounds 1920×1080, refresh 16.67 ms; windows opened with `display_id: Some(id)`. GPUI display uuid == `Uuid::new_v5(NAMESPACE_DNS, wl_output.name)` (verified for `"winit"`) — that is how shell-core maps a GPUI display to a niri output name. Multi-output not testable headless (single winit output). |
| 6 | transparent background (alpha) on a layer surface | **PASS** | `WindowBackgroundAppearance::Transparent`; 50 % accent rect and 60 % scrim blend over the windows below (`spike-04-passive.png`, `spike-03-modal.png`) |
| 7 | close + reopen layer windows without quitting | **PASS** | `Application::with_quit_mode(QuitMode::Explicit)`; modal/passive/scratchpad closed (`window.remove_window()`) and reopened repeatedly; process stays with 0..n windows |
| 8 | `AnchoredPopup` parented to a layer surface | **PASS** | popup above the bar's button, grows upward, clickable (`spike-08-popup.png`) |
| 9 | runtime changes of an existing layer surface | **partial** | size: **yes** (`window.resize(size(w, h))` → `set_size` → configure; pass the FULL size, see gotcha G1); exclusive zone: **yes** (`window.set_exclusive_zone(px(0.))` re-lays out windows 740→910 px); exclusive edge: yes (`set_exclusive_edge`); input region: yes. Keyboard interactivity, anchor, margin, layer, namespace: **no API** → close and recreate the window (~48 ms per layer window here; the first window of the process ~160–250 ms incl. GPU init). |

### Working idioms

```rust
// app: never quit on last window; embed assets
gpui_platform::application()
    .with_assets(ashd_ui::Assets)
    .with_quit_mode(QuitMode::Explicit)
    .run(|cx| { ashd_ui::init(ThemeName::Ash, cx); /* … */ });

// a layer surface (see ashd_shell_core::layer_options for the real ones)
cx.open_window(WindowOptions {
    titlebar: None,
    window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.), px(0.)),
        size: size(px(64.), px(0.)) })),          // 0 = stretch along the anchored axis
    window_background: WindowBackgroundAppearance::Transparent,
    display_id: Some(display.id),
    inactive_frame_interval: None,                 // see §4
    low_power_frame_interval: None,
    thermal_frame_interval: None,
    kind: WindowKind::LayerShell(LayerShellOptions {
        namespace: "ashd-rail".into(), layer: Layer::Top,
        anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT,
        exclusive_zone: Some(px(64.)), exclusive_edge: None,
        margin: Some((px(0.), px(0.), px(0.), px(0.))),
        keyboard_interactivity: KeyboardInteractivity::None,
    }),
    ..Default::default()
}, |_, cx| cx.new(|cx| Root::new(cx)))?;
// creation order = niri's exclusive-zone order: bar first, then rail/scratchpad.

// click-through overlay (ashd_shell_core::hit_area automates this per frame)
handle.update(cx, |_, window, _| window.set_input_region(Some(&[rect])))?;   // Some(&[]) = none, None = all

// runtime changes
window.set_exclusive_zone(px(0.));
window.resize(size(px(200.), px(1034.)));     // never 0 in either dimension

// close
handle.update(cx, |_, window, _| window.remove_window())?;

// popup parented to a layer surface
WindowKind::AnchoredPopup(PopupOptions { parent: bar.into(), anchor_rect, anchor: PopupAnchor::TopLeft,
    gravity: PopupGravity::TopRight, constraint_adjustment: PopupConstraintAdjustment::SLIDE_X
    | PopupConstraintAdjustment::FLIP_Y, offset: point(px(0.), px(-8.)), grab: false })
// grab: true only from a mouse-down handler (the grab needs the triggering press).

// focus a TextInput when a surface opens
input.focus_handle(cx).focus(window, cx);
```

### Fallbacks for the partial item

- Keyboard interactivity cannot be toggled → the modal overlay is **created on open and
  destroyed on close** (shell-core does this; measured ~50 ms). If that latency ever shows,
  fallback: patch `gpui_linux` (`WaylandWindow::set_keyboard_interactivity` is a 10-line
  addition calling `zwlr_layer_surface_v1.set_keyboard_interactivity` + commit) in a fork
  pinned the same way — integrator decision.
- Margin cannot change → the scratchpad slide animation (spring 1.0/700) animates content
  inside a full-width surface and toggles the exclusive zone, or recreates the surface; the
  scratchpad agent picks one.

## 3. API differences from gpui 0.2.2 (porting fm code)

| 0.2.2 (fm) | pinned rev |
|---|---|
| `impl IntoElement for MyElement` for custom `Element`s | blanket impl for every `Element`: delete the manual impl |
| `ShapedLine::paint(origin, line_height, window, cx)` | `paint(origin, line_height, TextAlign::Left, None, window, cx)` |
| `focus_handle.focus(window)` / `window.focus(&h)` | `focus_handle.focus(window, cx)` / `window.focus(&h, cx)` |
| `BoxShadow { color, offset, blur_radius, spread_radius }` | + `inset: bool` |
| `AsyncApp::update(..) -> Result<R>` | returns `R` directly (`WeakEntity::update` still returns `Result`) |
| `gpui::App::new()/Application::new()` | `gpui_platform::application()` |
| `.id(("x", n))` with an `i32` `n` | no `From<(&str, i32)>`: use `u32`/`u64` indices (`1u32..`) |
| — | new: `window.use_keyed_state`, `WindowKind::{LayerShell, AnchoredPopup}`, `QuitMode`, `cx.observe_displays`, frame-interval fields in `WindowOptions` |

`ashd_ui::{theme, ui, text_input, icons}` are fm's files ported with exactly these changes.

## 4. Fonts, icons, frame pacing

- **Fonts**: system fontconfig (gpui_wgpu: font-kit discovery + cosmic-text shaping). Nothing
  is embedded. `ashd_ui::Theme::resolved` maps token fonts to installed families
  (`JetBrains Mono` → `JetBrainsMono Nerd Font` …). The dev shell sets `FONTCONFIG_FILE` to
  Inter, JetBrains Mono, IBM Plex, Geist, DejaVu, Noto Color Emoji so screenshots are
  deterministic; **Manrope (Dusk) is not in nixpkgs** and falls back to Inter. The NixOS
  module must install the same families (`fonts.packages`). Embedding is possible if ever
  needed: `cx.text_system().add_fonts(vec![Cow::Borrowed(include_bytes!(…))])`.
- **SVG icons**: `ashd_ui::Assets` (rust-embed of `ashd/assets/**`) registered with
  `.with_assets(..)`; `icon(Icon::Wifi, 16.).text_color(t.fg)` = `svg().path("icons/wifi.svg")`,
  recoloured from the element's text colour (stroke `#000` in the files).
- **Frame pacing**: GPUI throttles windows that are not "active" (no keyboard focus — i.e.
  every layer surface) to `inactive_frame_interval` = 33 ms, plus `low_power_` (power-saver
  profile) and `thermal_` intervals. Shell surfaces set all three to `None`. Wayland present
  mode is Mailbox; frames are driven by `wl_surface.frame` callbacks. Measured in the nested
  session: **~32 fps for every window, normal xdg window included, on a 60 Hz nested
  output** — the nested chain (headless sway → niri winit) halves the callback rate, so this
  says nothing about 180 Hz. **Open risk**: re-measure on the real 180 Hz output (spike
  `fps` command) before tuning animations.

## 5. Gotchas (each cost an hour)

- **G1 — never size a layer window 0×N at runtime.** `window.resize(size(w, 0))` makes gpui
  call `wp_viewport.set_destination(…, 0)`: protocol error, niri drops the connection, and
  from then on **every** new window fails with *"Adapter … is not compatible with the
  display surface for this window"* (wgpu cannot make a surface on a dead connection). 0 is
  fine only in the initial `window_bounds` (means "stretch").
- **G2 — `window.request_animation_frame()` only while drawing** (render/prepaint/paint),
  else it panics; elsewhere `cx.notify()` and request frames from `render`.
- **G3 — re-entrancy.** Reading/updating an entity that is already being updated panics
  ("cannot read Shell while it is already being updated"). `cx.open_window` renders the
  first frame synchronously, so **never open a window (or build panels) inside an update of
  an entity those views read** — shell-core opens/closes surfaces outside `Shell` updates.
  Panels calling back into the shell (open/close modal, refresh overlays) go through
  `ShellHandle`, which defers to the end of the effect cycle.
- **G4 — Escape in a focused `TextInput`** is consumed by the input (`TextInputEvent::Cancel`);
  a modal panel with an input must close itself on `Cancel`.
- **G5 — OnDemand keyboard** needs a click on the surface (niri focuses on-demand layers on
  click) and the element must take GPUI focus on mouse-down.
- **G6 — nested session**: Unix socket paths ≤ 108 bytes (use `/tmp/ashd-nested.*`, not the
  scratchpad); turn off niri's hot corner or the test pointer parked at (0,0) opens the
  overview; `WAYLAND_DEBUG=1` on the client shows the exact protocol error.
- **G7 — key repeat survives a destroyed surface.** gpui_linux's `wl_keyboard` handler
  returns early while no window has keyboard focus, and `drop_window` clears the focused
  window without stopping the repeat timer. A modal destroyed between a key press and its
  release (Escape closes it) therefore loses the release, and the next surface that gets
  the keyboard receives the key every ~40 ms. Fixed once in shell-core's overlay host
  (`overlay.rs`, "key repeat across modal surfaces"): a modal closed while keys pressed in
  it are down **lingers** (empty, click-through, focused) until they are released or
  `LINGER_MAX` (1 s); a modal opened while keys are down **swallows** their repeats (a
  keystroke interceptor, which runs before key bindings) until release or another key.
  `KeyDownEvent::is_held` exists but reaches listeners only *after* key bindings ran.
- **G8 — sticky hover.** After `wl_pointer.leave` GPUI keeps the last pointer position and
  hit-tests hover against it: `.hover()` styles stick, `on_hover(false)` never fires (fast
  exits through a surface edge, or out of a passive overlay's input region). Fixed for every
  surface by `ashd_shell_core::pointer_exit_reset` (installed in each `SurfaceRoot`): on
  the leave it dispatches one synthetic mouse move far outside the window. Panels use plain
  GPUI hover APIs.

## 6. ashd-shell end-to-end (M0 definition of done)

`ashd-shell` (all 22 panels registered, services on) in the nested session:
`ashd ping` → `ashd-shell 0.1.0 (protocol 1, pid …)`; `ashd status` → bar 1920×46, rail
64×1034, scratchpad 340×1034 on output `winit`, `niri_connected: true`; a second
`ashd-shell` refuses ("…shell.sock is served by another ashd-shell"); `ashd launcher` opens
the modal (typing reaches its input, Escape closes it); `ashd quick-settings` twice
opens/closes; `ashd osd volume +5` creates the passive overlay, which is destroyed again
after 1.2 s; `ashd clipboard` → exit 1 "no panel handles `clipboard` yet"; `ashd scratch
toggle` removes/re-adds the scratchpad; `{"theme":{"set":"dusk"}}` crossfades all surfaces;
`ashd quit-shell` exits. Screenshots: `ashd/target-notes/shell-*.png`.

## 7. Headless nested session (how all of the above is run)

The Rust harness `cargo xtask nested …` (`ashd/xtask/README.md`):

```sh
nix-shell ashd/shell.nix                     # dev shell: GPUI libs, fonts, target dir
cd ashd && root=$(NIRI=<fork build> cargo xtask nested start --with-bus)
source "$root/env"                           # private XDG_RUNTIME_DIR, WAYLAND_DISPLAY, NIRI_SOCKET, D-Bus
$CARGO_TARGET_DIR/debug/ashd-shell &
ashd ping; ashd status; cargo xtask shot --root "$root"
cargo xtask nested stop "$root"
```

It runs sway with `WLR_BACKENDS=headless` (1 output), niri (winit; `NIRI=` picks the ashd
fork, needed for `workspace "scratch" { hidden }`) inside it with `xtask/assets/nested-niri.kdl`
(or `--niri-config`), optionally a private `dbus-daemon` and a throwaway `$HOME`; the live
`WAYLAND_DISPLAY`, `DISPLAY`, `NIRI_SOCKET` and session bus are scrubbed, and
`ASHD_NESTED=1`, `ASHD_FAKE_POWER=1`, `ASHD_NO_RELOAD=1` are set. Input: `wlrctl pointer
move/click`, `wtype`. GPU rendering works headless on this machine (RADV Vulkan through the
render node; no lavapipe needed). `shot-check` and `smoke` drive the same session.
