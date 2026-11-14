# ashd — architecture (as built)

ashd is a Linux desktop shell in Rust on [GPUI](https://github.com/zed-industries/zed)
(git, pinned), drawn as **wlr-layer-shell surfaces** on the ashd fork of niri (branch
`ashd`), deployed by the NixOS flake (`../flake.nix`, `../packages/ashd`). Visual reference:
the design mockups (`docs/design`, not part of the public repository; their values live in
`tokens/tokens.json` and `ashd-tokens`' `metrics`/`motion`; Ash is the reference theme). GPUI specifics, the pinned rev and the gotchas that cost time (G1–G8):
`../docs/GPUI-NOTES.md` — read it before writing GPUI code.

This file describes the system as it is. Signatures quoted here are the public API other
crates build on; changing one is an integrator decision.

## 1. Build

NixOS, no global Rust: everything runs through the dev shell (nixpkgs from the repo's
`flake.lock` → rustc 1.97.1, the same as the flake build).

    nix-shell ashd/shell.nix --run 'cd ashd && cargo check -p <crate>'
    nix-shell ashd/shell.nix --run 'cd ashd && cargo xtask ci'        # the whole gate, §13

- Build output goes to `ashd/target` (gitignored), or to `$CARGO_TARGET_DIR` if you set it
  (the artefacts reach ~20 GB). Concurrent cargo runs serialise on the target-dir lock.
- The workspace is **warning-free**: `cargo clippy --workspace --all-targets -- -D warnings`.
- Binaries: `ashd` (crates/ashd-cli), `ashd-shell`, `ashd-lock`, `ashd-greeter` — exactly the
  `default-members` and what `packages/ashd` builds (`--locked -p ashd-shell -p ashd-cli -p
  ashd-lock -p ashd-greeter`). `Cargo.lock` is committed and must satisfy `--locked`.
- Packaging: crane; the source filter keeps Cargo sources plus `tokens/`, `assets/`,
  `vendor/`, `fixtures/` and `*.lua`. **Every file embedded with `include_str!` /
  `include_bytes!` / rust-embed must live under one of those** (today: `tokens/tokens.json`,
  `assets/**` (rust-embed in ashd-ui, icons in ashd-lockui), `ashd-tokens/src/nvim_apply.lua`,
  `xtask/assets/nested-niri.kdl`, test fixtures under `*/fixtures/` and `*/tests/assets/`).
  dlopen'ed libs (wayland, xkbcommon, vulkan-loader, fontconfig, libGL) go into RUNPATH, never
  an `LD_LIBRARY_PATH` wrapper.
- Anything with a window runs **only** in the headless nested session (§13), never with the
  inherited `WAYLAND_DISPLAY`/`DISPLAY`.

## 2. Crate graph

```
ashd-shell (bin: registers panels) ──► panels/* ──► ashd-shell-core ──► ashd-ui ──► ashd-tokens
                                          │              ├──► ashd-ipc ──► ashd-tokens
                                          │              ├──► ashd-niri ──► vendor/niri-ipc
                                          │              └──► services/* (all of them, §10)
                                          └──► services/*, ashd-niri, ashd-tokens, ashd-ipc (types)
                                          └──► ashd-theme (quick_settings, theme_switcher)
ashd-cli (bin `ashd`) ──► ashd-ipc, ashd-theme, ashd-niri, ashd-tokens            (no GPUI)
ashd-theme ──► ashd-tokens, ashd-niri                                             (no GPUI)
ashd-lock / ashd-greeter ──► ashd-lockui ──► ashd-gfx (sctk + wgpu + vello + parley) ──► ashd-tokens
xtask ──► ashd-ipc, ashd-tokens
```

Rules: **services and ashd-theme never depend on GPUI** (plain data + channels; testable
without a display). Panels never depend on each other — they name each other by `PanelId`
and talk through `ShellHandle::request` / `open_modal`. Only `ashd-shell` knows every panel
crate. Only shell-core opens windows. ashd-lock/greeter use crates.io wgpu 30 for vello;
gpui uses its own git wgpu — no binary links both (GPUI-NOTES §1).

## 3. Module map

| path | package | what |
|---|---|---|
| `crates/ashd-tokens` | ashd-tokens | tokens.json → `Tokens`; `metrics`, `motion`, `render` (generated theme files), `state` (theme persistence, `apply_selected`) |
| `crates/ashd-ui` | ashd-ui | GPUI kit: `Theme` global + crossfade, components, `TextInput`, `icons`, `anim`, `Assets` |
| `crates/ashd-ipc` | ashd-ipc | CLI ↔ shell protocol (§7), `Client`, `Server` |
| `crates/ashd-niri` | ashd-niri | niri event-stream reducer `NiriState`, `NiriHandle`, strip / canvas helpers, live views, blocking helpers (§9) |
| `crates/ashd-theme` | ashd-theme | the one theme-apply sequence + "apply to" consumers + `ashd fm` argv (§8) |
| `crates/ashd-shell-core` | ashd-shell-core | `Shell`, `Panel`, surfaces, overlay host, named anchors, IPC dispatch, service bridges, notices, power bridge (§11) |
| `crates/ashd-shell` | ashd-shell | `main.rs` registers 28 panels; `examples/spike.rs` (M0 gate) |
| `crates/ashd-cli` | ashd-cli (bin `ashd`) | clap CLI (§7) |
| `crates/services/<x>` | ashd-svc-<x> | §10 |
| `crates/panels/<x>` | ashd-panel-<x> | rail, bar (+ tray, now-playing), dock, launcher, toasts, osd, quick_settings, session, scratchpad, strip_header, canvas_hud, minimap, calendar, media, clipboard, cheatsheet, notification_center, screenshot, polkit_dialog, battery, theme_switcher |
| `crates/ashd-gfx`, `ashd-lockui`, `ashd-lock`, `ashd-greeter` | | lock screen and greetd greeter (no GPUI: ext-session-lock surfaces cannot be GPUI windows) |
| `vendor/niri-ipc` | niri-ipc | vendored copy of the fork's IPC crate (§9) |
| `xtask` | xtask | the test harness (§13) |
| `tokens/tokens.json`, `assets/icons/*.svg` | | design tokens, icons (both embedded) |
| `target-notes/` | | screenshots / logs of verification runs |

## 4. Rules

1. Edit what you own; shared crates get additive, minimal edits. Never rename or re-sign a
   public item without updating every user in the same change.
2. **Never block the main thread** (disk, D-Bus, subprocess, socket, sleep):
   `let task = cx.background_spawn(async move { … }); cx.spawn(async move |this, cx| { let r = task.await; this.update(cx, |this, cx| { …; cx.notify() }).ok(); }).detach();`
   Services run on their own threads and talk through channels.
3. **Never hard-code a colour, font or radius**: `let t = ashd_ui::theme(cx).clone();` then
   `t.accent`, `t.font_mono.clone()`, `t.radius_control`; sizes from `ashd_tokens::metrics`
   (no panel-local copy of a metric) / `t.rail_w` / `t.bar_h`; durations from `motion`.
4. **Never launch on the live desktop**, never touch the real session bus, clipboard, audio,
   polkit, network, Bluetooth or power from tests: nested session only (§13).
5. No git index/history commands; the integrator commits. No `cargo update`.
6. **Apps are launched through niri** (`ctx.shell.spawn(argv, cx)` → `Action::Spawn`), never
   `std::process::Command` from the shell. Binaries never rely on `LD_LIBRARY_PATH`.
7. One module per region: a panel's state lives in its struct, its view in `render`, its
   reactions in `on_request` / listeners. Don't reach into another panel's state.
8. No bash/python scripts in the repo: tooling is Rust (`xtask`).
9. Workspace numbers shown to the user are `NiriState::visible_number(id)` (niri's `idx`
   counts the hidden scratch workspace); the mode word is from `NiriState::is_canvas(id)`.

## 5. ashd-tokens (no GPUI)

```rust
pub enum ThemeName { Ash, Ink, Midnight, Moss, Dusk }   // serde/FromStr/Display "ash" …; ALL, label(), title(), next(), from_label()
pub struct Rgba { r, g, b, a: u8 }                      // hex(), alpha(), with_alpha(), contrast() …
pub struct Colors { bg, surface, surface_2, line, frame, fg, fg_muted, accent, on_accent, ok, warn, err, info,
                    syn_keyword, syn_string, syn_number, syn_comment: Rgba }
pub struct Tokens { name, colors, fonts: Fonts { sans, mono }, radius: Radii { panel, window, control }, shape: Shape }
pub struct Shape { layout: LayoutVariant /* Attached|Floating */, gap, rail: RailVariant /* Num|Thumb|Pill */, rail_width,
                   dock: DockVariant /* Bar|Split|Pill */, dock_height, scratchpad: ScratchpadVariant /* Stack|Tabs */,
                   default_mode: WindowMode /* Scroll|Canvas */ }
pub fn tokens(ThemeName) -> &'static Tokens;  pub fn shared() -> &'static Shared;  pub const TOKENS_JSON: &str;
```

- `metrics` (Ash reference px): `RAIL_W 64`, `RAIL_HEADER_H 52`, `RAIL_BUTTON 56`,
  `RAIL_FLYOUT_W 268`, `BAR_H 46`, `STATUS_W 560`, `DOCK_CENTER_X 822`, `DOCK_BUTTON 36`,
  `SCRATCH_W 340`, `STRIP_GAP 12`, `POPUP_GAP 8`, `QUICK_SETTINGS_W 400`,
  `NOTIFICATION_CENTER_W 400`, `CALENDAR_W 340`, `MEDIA_W 340` (det-11), `LAUNCHER_W 620`,
  `TOAST_W 380`, `TOAST_COMPACT_W 340` (det-12 network toasts), `OSD_W/H 260/48`,
  `CLIPBOARD_W/H 860/466` (det-10 panel, not the 980×620 board), `SESSION_BUTTON 40`,
  `type_scale::*`, `icon_size::*`, …
- `motion`: durations, `timing::*`, Bézier curves, springs (TOKENS.md §9).
- `render::Target { Kitty, Starship, StarshipConfig, Nvim, Gtk, Niri }` with `ALL`,
  `file_name()`, `render(&Tokens)`.
- `state`: `state_dir()` (`$ASHD_STATE_DIR`, else `$XDG_STATE_HOME/ashd`), `theme_file()`,
  `generated_dir()`, `etc_dir()` (`$ASHD_ETC_DIR`, else `/etc/ashd`), `current_theme()`
  (state file → `/etc/ashd/theme` → Ash), `set_current_theme`, `apply`, `apply_selected(name,
  &[Target])`, `apply_to_in`, `write_atomic`.

## 6. ashd-ui (GPUI kit)

```rust
pub fn init(ThemeName, &mut App);  pub fn theme(&App) -> &Theme;  pub fn set_theme(ThemeName, animate: bool, &mut App);
pub fn current_theme_name(&App) -> ThemeName;  pub fn color(Rgba) -> Hsla;  pub struct Assets;  // rust-embed ashd/assets/**
pub struct Theme { name, bg … syn_comment: Hsla, font_sans, font_mono, radius_panel/window/control: Pixels,
                   layout, gap, rail, rail_w, dock, bar_h, scratchpad, default_mode, label_caps, … }
// components: label mono numeric title keycap kbd icon_button(_sized) button(_small/_with_icon) segmented tabs toggle
// checkbox radio slider meter progress list_row rich_row panel overlay_panel menu tooltip toast_card badge status_dot
// field empty_state window_frame hline vline alpha empty
pub struct TextInput; pub enum TextInputEvent { Changed, Submit, Cancel, Tab, Up, Down, Blur }
pub enum Icon { … }  pub fn icon(Icon, size) -> Svg;  pub fn app_icon(..);  pub fn spinner(..);
pub mod anim { tween, ease_*, SpringValue, duration_ms /* ASHD_REDUCE_MOTION */ }
```

## 7. ashd-ipc — CLI ↔ shell protocol

Socket `$ASHD_SOCKET`, else `$XDG_RUNTIME_DIR/ashd/shell.sock` (dir 0700), else
`/tmp/ashd-$UID/shell.sock`. Newline-delimited JSON, one `Response` per `Request`, in order.
Serde externally tagged, kebab-case. `PROTOCOL_VERSION = 1`.

```rust
pub enum Request {
    Ping, Status, Dismiss, Quit, Lock,
    Launcher { op: PanelOp, mode: Option<LauncherMode /* Calc|Files|Run|Emoji|Web */> },
    Clipboard { op }, Notifications { op }, QuickSettings { op }, Calendar { op }, Session { op },
    Cheatsheet { op }, ThemeSwitcher { op }, Media { op },
    MediaControl(MediaControl /* PlayPause|Next|Previous|Stop */),
    Osd(OsdRequest /* Volume(Step(i32)|Mute) | MicMute | Brightness(i32) */),
    Scratch(ScratchOp /* Hide|Show|Toggle|Open|Close|HideWindow(u64)|ShowWindow(u64) */),
    Canvas(CanvasOp /* Toggle|ZoomIn|ZoomOut|Fit|Home */),
    Theme(ThemeRequest /* Get|Set(ThemeName)|Cycle */),
    Screenshot(ScreenshotRequest { mode: Option<CaptureMode /* Region|Window|Output */>, record: bool, op }),
    Record(RecordOp /* Stop|Toggle */),
}
pub enum PanelOp { Toggle /* default */, Open, Close }
pub enum Response { Ok, Pong { version, protocol, pid }, Theme(ThemeName), Status(ShellStatus), Error(String) }
pub struct ShellStatus { version, theme, niri_connected, modal: Option<String>, surfaces: Vec<SurfaceStatus { kind, output, width, height }> }
pub fn socket_path() -> PathBuf;  pub fn request(&Request) -> Result<Response, IpcError>;  Client, Server, Incoming
```

### CLI (`ashd`; exit 0 ok / 1 error / 2 shell not running)

| command | does |
|---|---|
| `ashd launcher [--mode calc\|files\|run\|emoji\|web] [--open\|--close]` | `Launcher` |
| `ashd clipboard \| notifications \| quick-settings \| calendar \| session \| cheatsheet [--open\|--close]` | the panel's toggle |
| `ashd media [--open\|--close]` · `ashd media play-pause\|next\|previous\|stop` | `Media` · `MediaControl` |
| `ashd screenshot [--region\|--window\|--output] [--record]` · `ashd record stop\|toggle` | `Screenshot` · `Record` |
| `ashd osd volume <+N\|-N\|mute>` · `osd mic-mute` · `osd brightness <±N>` | `Osd` (wpctl fallback without a shell) |
| `ashd scratch hide\|show [--id N]` · `scratch toggle\|open\|close` | `Scratch` (hide/show go straight to niri without a shell) |
| `ashd canvas toggle\|zoom-in\|zoom-out\|fit\|home` | `Canvas` (straight to niri without a shell) |
| `ashd lock` | `Lock` (shell spawns `ashd-lock` via niri); without a shell execs `ashd-lock` |
| `ashd theme get` / `set <name>` / `cycle` / `apply [--quiet]` [`--no-reload`] | `ashd_theme::set` / `reapply_current` (§8), then `Theme(Set)` to the shell |
| `ashd theme pick [--open\|--close]` | `ThemeSwitcher` |
| `ashd fm [-- ARGS…]` | exec `fm --theme <current> ARGS…` (`$ASHD_FM_BIN` or `fm` from PATH; no `--theme` when "fm" is unchecked in the switcher) |
| `ashd app quit` | niri: close every window of the focused window's app (local) |
| `ashd status` / `ping` / `dismiss` / `quit-shell` / `completions <shell>` | introspection & control |
| `ashd sys generate-hardware-config <host>` · `sys flclashx-sync` · `sys fm-pick …` · `ashd claude-rc` | NixOS helpers (fm-pick = xdg-desktop-portal-termfilechooser backend, themed like `ashd fm`) |

## 8. Theme files (contract with the NixOS config) — `ashd-theme`

One sequence, used by `ashd theme set|cycle|apply`, the quick-settings theme row and the
theme switcher: `ashd_theme::apply(name, &ApplyTo, reload)` =

1. persist the name (`<state>/theme`; `ashd-shell` reads it at startup);
2. write the generated files of the selected consumers (`ApplyTo`, persisted by the theme
   switcher's "apply to" checkboxes in `<state>/apply-to.json`, all on by default);
   unchecked consumers keep their previous files;
3. reload niri (`LoadConfigFile`) and kitty (`SIGUSR1` to the user's kitty processes, only if
   kitty is selected) — **skipped when `ASHD_NO_RELOAD` or `ASHD_FAKE_POWER` is set**.

`ashd_theme::set(name, reload)` uses the persisted selection; `reapply_current(reload)` is
`ashd theme apply`. The shell part (crossfade) is the caller's: `ashd_ui::set_theme` in the
shell, `Request::Theme(Set)` from the CLI. `<state>` = `$ASHD_STATE_DIR`, else
`$XDG_STATE_HOME/ashd` (`~/.local/state/ashd`):

| file | consumer | the NixOS side must |
|---|---|---|
| `theme` | ashd-shell | — (`/etc/ashd/theme` is the default before the first switch) |
| `generated/kitty.conf` | kitty | `include ~/.local/state/ashd/generated/kitty.conf` |
| `generated/starship-palette.toml` | starship | palette table only (if you assemble the config yourself) |
| `generated/starship.toml` | starship | `STARSHIP_CONFIG=~/.local/state/ashd/generated/starship.toml` (= `/etc/ashd/starship-base.toml` + `palette = "ashd"` + the table) |
| `generated/nvim.lua` | nvim | `dofile(vim.fn.expand("~/.local/state/ashd/generated/nvim.lua")).apply()` |
| `generated/gtk.css` | GTK 3/4 | `@import url("file://…/generated/gtk.css");` in `gtk-3.0/gtk.css`, `gtk-4.0/gtk.css` |
| `generated/niri.kdl` | niri | `include "~/.local/state/ashd/generated/niri.kdl"` (focus ring, gaps, radius, canvas look) |
| `apply-to.json` | ashd-theme | — |
| (fm) | fm | use **`ashd fm`** as the file-manager command (it passes `--theme`) |

Fonts the themes name (Inter, JetBrains Mono, IBM Plex Sans/Mono, Inter Tight, Geist, Geist
Mono, Manrope) must be in `fonts.packages`; missing ones fall back (GPUI-NOTES §4).

## 9. ashd-niri (no GPUI)

```rust
pub use niri_ipc;                                                   // the vendored crate
pub fn start() -> (NiriHandle, Receiver<NiriState>);               // thread `ashd-niri`; reconnects with backoff
impl NiriHandle { send_action(Action); async action(Action); async request(Request);
                  subscribe_views() -> Receiver<Arc<LiveViews>>;   // per-frame canvas/strip view state, ≤ 1 per VIEWS_MIN_INTERVAL
                  disconnected() }
pub struct NiriState { connected, workspaces, windows, keyboard_layouts, overview_open, config_failed, casts,
                       touchpad_on, outputs, last_screenshot, canvas_views, strip_views }
impl NiriState {
    apply(Event) -> bool;
    workspaces_on(output) /* visible only */; workspaces_on_including_hidden; workspaces_sorted(_including_hidden);
    visible_number(ws) -> Option<usize>;      // what the user sees: 1-based among the output's non-hidden workspaces
    hidden_workspaces(); workspace_mode(ws); is_canvas(ws); canvas_view(ws); strip_view(ws);
    canvas_windows(ws); canvas_bounds(ws); canvas_bounds_in(ws, view);
    focused_workspace(); active_workspace(output); focused_output(); focused_window();
    windows_on(ws); windows_of_app(app_id); strip(ws) -> Strip; current_layout()
}
pub mod canvas { PlaneRect, CanvasWindow, canvas_windows, bounds, visible_plane_rect, target_plane_rect, offset_centering, LiveViews }
pub mod strip { Strip { columns, floating }, StripColumn { index, windows, width, x, view_x, focused } }
pub fn spawn_action(argv) -> Action;  pub fn spawn_sh_action(cmd) -> Action;  pub const SOCKET_ENV;
pub mod blocking { request, action, close_focused_app_windows, Connection }
```

Fork additions read through niri-ipc: `Workspace::{mode, is_hidden}`,
`Window::hidden_from_workspace_id`, `WindowLayout::{pos_in_canvas, is_pinned}`,
`Action::{SetWorkspaceMode, ToggleWorkspaceMode, hide/unhide-window, canvas zoom/pan …}`,
`Screenshot*{ copy_to_clipboard, notify }` (the screenshot panel's internal stills pass
`false, false`: no clipboard, no niri notification). Since fork 6f311d7b `Workspace::idx` is
the visual index among the output's visible workspaces and **0 for hidden ones**
(`focus-workspace N` counts visible ones); ashd-niri sorts hidden workspaces last and every
number shown comes from `visible_number` (never 0), which is right for a stock niri, the
current fork and older forks alike.
**Vendored niri-ipc** (`vendor/niri-ipc`, GPL-3.0-or-later): `src/` is a byte copy of the
fork's `niri-ipc/src`; only the manifest differs. Re-sync whenever the fork's IPC changes
(integrator); unknown events are skipped. The workspace licence is GPL-3.0-or-later.

## 10. Services (no GPUI)

Shape of every service: `XxxState` (plain data, full snapshot), `Handle` (fire-and-forget
command methods, `disconnected()`), `start() -> (Handle, Receiver<XxxState>)` spawning a
named thread; state channel `bounded(1)` + `force_send` (newest wins); one-shot signals are
counters in the state (`lock_seq`, `last_event.seq`). Unavailability is state
(`available: false`), never an error. D-Bus via zbus 5.

| crate | state (key fields) | handle |
|---|---|---|
| sysinfo | cpu, per-core, mem/swap, temp, load, uptime, battery, network summary | `set_interval`, `refresh_now` |
| audio (pulse) | `available, sinks, sources` | `set_volume`, `step_volume`, `set_mute`, `toggle_mute`, `set_default` |
| apps | `loaded, apps` (+ launch counts in redb) | `reload`, `record_launch`; free fn `search` |
| notifications (`org.freedesktop.Notifications`) | `available, notifications (history), dnd, last_seq` | `dismiss`, `expire`, `dismiss_all`, `invoke_action`, `reply`, `set_dnd`, **`post(LocalNotification)`** |
| logind | session, locked, idle hint, `lock_seq`, can_* | `lock_session`, `suspend`, `reboot`, `power_off`, `logout` (`ASHD_FAKE_POWER` logs instead) |
| mpris | `players, active, pinned` | `play_pause`, `next`, `previous`, `stop`, `seek_to`, `go_to`, `set_active` … |
| network (NM / sysfs) | `primary, interfaces, vpns, wifi, profiles, throughput, last_event { seq, kind, ifname, detail }` | connect/disconnect, profiles, static apply + safe revert, Wi-Fi |
| bluetooth (BlueZ) | adapter, devices, pairing | power, discovery, connect, pair, trust, remove |
| clipboard (wlr data-control) | history entries, pinned | `activate`, `set_pinned`, `remove`, `clear_unpinned`, `save_to` |
| idle (ext-idle-notify) | stages, `lock_seq`, inhibited | `set_config`, `set_inhibited` |
| nightlight | enabled, temperature, mode | `toggle`, `set_temperature`, `set_mode` |
| brightness (backlight / DDC) | displays | `set`, `step` |
| polkit (built-in agent) | `pending` request (phase, identities), queue | `respond_to`, `cancel_request`, `select_identity_for`, `retry` |
| tray (StatusNotifierWatcher + dbusmenu) | `items`, open `menu` | activate, scroll, menus |
| upower | doc-only stub (battery comes from sysinfo) | |

`LocalNotification { app_name, summary, body, icon /* theme name, path or "ashd:<glyph>" */,
urgency, timeout, actions, category, tag }`: shell-originated notifications take the same
store, toast path and history as D-Bus ones, without a bus (works with none at all); a
`tag` replaces the previous local one with that tag.

## 11. ashd-shell-core (GPUI)

### Surfaces (per display; created by shell-core only)

| kind | namespace | layer | anchor | exclusive zone | margin (floating) | keyboard | lifetime |
|---|---|---|---|---|---|---|---|
| bar | `ashd-bar` | Top | L\|R\|B, h = `t.bar_h` | `bar_h + gap` | 0 / gap / gap / gap (t r b l) | None | always, created **first** |
| rail | `ashd-rail` | Top | T\|B\|L, w = `t.rail_w` | `rail_w + gap` | gap top/left, 0 bottom | None | always |
| scratchpad (attached) | `ashd-scratchpad` | Top | T\|B\|R, w = `SCRATCH_W` | `SCRATCH_W` | — | OnDemand | while `scratch_visible` |
| scratchpad (floating) | `ashd-scratchpad` | Top | T\|B\|R | **-1** (full height, ignores the bar) | gap t/r/b | OnDemand | while `scratch_visible` |
| scratchpad-zone (floating) | `ashd-scratchpad-zone` | Top | T\|B\|R, 1 px, empty input region | `SCRATCH_W + 2·gap` | — | None | with the floating scratchpad |
| overlay-passive | `ashd-overlay-passive` | Overlay | all | -1 | — | None | while a passive panel `is_live` |
| overlay-modal | `ashd-overlay-modal` | Overlay | all | -1 | — | Exclusive | while a modal is open (+ linger, below) |

Zones as niri applies them: the reserved strip is `exclusive_zone + margin` on the anchored
edge; surfaces created later lay out inside what is left, hence the creation order. Attached
themes (Ash) have no margins. A theme switch that changes the frame `ShapeKey` (layout, gap,
rail width, bar height) recreates bar/rail/scratchpad; everything else is a re-render
(300 ms crossfade). The bar draws per `DockVariant`: `Bar` (Ash: one bar, status 560 with a
border, dock centred on `DOCK_CENTER_X`), `Split` (Ink, Moss: status pill + three pills),
`Pill` (Midnight, Dusk: status pill + one centred pill); in floating layouts the row ends at
the main area's right edge while the scratchpad is shown. Every surface root also carries
`pointer_exit_reset()` (sticky-hover fix, GPUI-NOTES G8). `layer_options(kind, &DisplayInfo,
&Theme)` is public. Displays map to niri outputs by `output_uuid(name)` (GPUI's v5 uuid).

### Panel contract

```rust
pub struct PanelId(pub &'static str);
pub enum Slot { Rail, BarStart, BarCenter, BarEnd, Scratchpad }
pub enum Placement { Frame(Slot), Passive, Modal(ModalStyle { scrim: bool }) }
pub type ModalAnchor = Bounds<Pixels>;                                   // output-local px
pub trait Panel: Render + Sized + 'static {
    const ID: PanelId;  const PLACEMENT: Placement;
    const ANCHORS: &'static [&'static str] = &[];                        // named anchors a popup grows from
    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self;
    fn on_request(&mut self, &Request, &mut Context<Self>) -> Option<Response> { None }
    fn is_live(&self, &App) -> bool { false }                            // passive panels
    fn on_open(&mut self, &mut Window, &mut Context<Self>) {}            // modal: focus inputs
    fn on_close(&mut self, &mut Context<Self>) {}
}
pub struct PanelContext { shell: ShellHandle, services: Services, niri: Entity<NiriState>, niri_handle, display: Option<DisplayInfo> }
```

Frame and passive panels: one instance per display; modal panels: one per app, shown on the
focused display. Registration (`crates/ashd-shell/src/main.rs`) order = order within a slot
and request-offer order. Requests: shell-core answers `Ping`, `Status`, `Quit`, `Dismiss`,
`Theme(..)`, `Lock`; everything else is offered to the modal panels, then the focused
display's panels; first `Some` wins; nothing → `Error("no panel handles `x` yet")`
(`Scratch` falls back to showing/hiding the scratchpad surface).

### Shell / ShellHandle / Services

```rust
pub struct Services { niri, sysinfo, audio, apps, notifications, logind, clipboard, mpris, idle, nightlight,
                      brightness, polkit, tray, network, bluetooth: Service<State, Handle> }   // Service { state: Entity<S>, handle: H }
impl ShellHandle {
    // deferred to the end of the effect cycle (safe inside your own update, GPUI-NOTES G3):
    open_modal(PanelId, Option<ModalAnchor>, cx); open_modal_on(PanelId, DisplayId, Option<ModalAnchor>, cx);
    close_modal(cx); toggle_modal(..); refresh_overlays(cx); set_scratch_visible(bool, cx); shutdown(cx);
    request(Request, Option<ModalAnchor>, cx);     // dispatch like `ashd …`, the anchor goes to the opened popup
    // immediate:
    modal(cx); modal_anchor(PanelId, cx); modal_display(cx); display(DisplayId, cx); is_fullscreen(DisplayId, cx);
    request_anchor(cx); niri_action(Action, cx); spawn(argv, cx); launch(id_or_command, cx); config(cx); status(cx); entity();
}
pub fn hit_area(element) -> Div;            // passive overlay input region
pub fn pointer_exit_reset() -> impl IntoElement;
pub mod anchors { names::{CLOCK, NETWORK, VOLUME, MEDIA, BELL, SESSION}, publish, forget, track, resolve, resolve_first }
pub mod notices;                             // shell-originated notifications
pub fn run(ShellOptions, register) -> anyhow::Result<()>;  dispatch; bridge; start_service; ShellConfig (shell.json)
```

Also wired in `run`: the power bridge (`idle.lock_seq` / `logind.lock_seq` / PrepareForSleep →
`Lock`; a playing MPRIS player inhibits idle), notices (network link events → toast),
signals (SIGTERM/SIGINT → clean shutdown, SIGHUP → reload `shell.json`).

### Overlay host behaviour

- **Passive** (toasts, OSD, rail flyout, strip header, canvas HUD, minimap, screenshot HUD,
  tray tooltip): each passive panel draws over the whole output; pointer input only inside
  `hit_area(..)`s (union applied every frame with `set_input_region`), click-through
  elsewhere. The surface exists while any passive panel of the display is live; call
  `refresh_overlays` when that flips outside an IPC request. Both overlays are on the Overlay
  layer, stacked in creation order, so a passive surface created before a modal sits under
  it: toasts stand aside while the notification centre is open (they are in its list) and
  come back when it closes; network/link notices are compact 340 px toasts (`network.*`).
- **Modal** (launcher, quick settings, session, popups, dialogs): one at a time, on the
  focused display, keyboard Exclusive; opening another closes the current one (`on_close`).
  Dismissed by mouse-down outside the panel's `.occlude()`d content, Escape (`ModalDismiss`
  in key context `AshdModal`; a focused `TextInput` turns Escape into `Cancel`: close
  yourself), `Request::Dismiss`, `PanelOp::Close`. The surface is created on open and
  destroyed on close (~50 ms; keyboard interactivity is immutable).
- **Key release across modals** (GPUI-NOTES G7): a modal closed while keys pressed in it are
  still down **lingers** — out of the registry (a new modal gets a fresh surface), empty,
  click-through, focused — until those keys are released or `LINGER_MAX` (1 s), so the
  release reaches a GPUI window and the platform key repeat stops. A modal opened while keys
  are down **swallows their repeats** (keystroke interceptor, before key bindings) until
  release or another key. Holding Escape over a queue of polkit requests cancels exactly one;
  launcher → Escape → reopen stays open. Panels need no local debounce.
- **Named anchors**: frame panels publish where their popup controls are while they paint
  (`anchors::track` / `publish`; the bar: `clock`, `network`, `volume`, `media`, `bell`,
  `session`, in every dock variant). `open_modal(id, None, …)` resolves, in order: the
  explicit anchor, the anchor of the `ShellHandle::request` being dispatched (a click), then
  the panel's `ANCHORS` on the target display (keybind / CLI). Popups position themselves at
  `bottom: t.bar_h (+ gap) + POPUP_GAP` from the anchor; modal dim = `t.scrim_power()` (60 %).

### Status area (bar)

clock │ cpu, mem │ network + volume (opens quick settings; VPN name capped at
`VPN_LABEL_MAX`) · battery │ layout chip · [tray area] · bell. The tray area (`flex-1`,
measured) holds the now-playing control (only while MPRIS has an active player: music glyph +
title, accent while playing; click → `Request::Media` anchored there, middle click
play/pause, wheel next/previous) followed by the tray icons; one width budget
(`tray::plan_row`): the title shrinks, then disappears, before tray icons move behind the
chevron.

## 12. Lock stack (no GPUI)

`ashd-gfx`: wgpu + vello renderer for sctk surfaces, `Text` (parley), icon parser for
`assets/icons/*.svg`. `ashd-lockui`: `LockUi` and the greeter UI (state machines + `Draw`),
glyphs from the icon set (lock, moon, reboot, power); offscreen renders via
`cargo run -p ashd-lockui --example render|render_greeter -- target-notes`. `ashd-lock`:
ext-session-lock-v1, one surface per output, PAM service `ashd-lock`, passwords only in
`Zeroizing` (debug builds refuse to run without `ASHD_NESTED=1`; `ASHD_LOCK_TEST_PASSWORD`
for the nested session). `ashd-greeter`: greetd client. The shell spawns `ashd-lock` through
niri on `Request::Lock` and on the power bridge's lock events.

## 13. Test harness and environment switches

`cargo xtask …` (`xtask/README.md`), Rust only:

- `nested start|env|run|stop|list|gc [--with-bus] [--niri-config F]`: headless sway → niri
  (`NIRI=<binary>`; the fork build for hidden workspaces / canvas) in `/tmp/ashd-nested.*`,
  environment scrubbed (no live Wayland/X11/niri/bus/pulse), `source "$root/env"`.
- `shot [--root]`, `shot-check --theme ash|…|all [--niri-config F]` (status geometry + exact
  token pixels per theme), `smoke [--niri-config F] [--include-lock]` (every CLI request incl.
  calendar, media, theme pick, screenshot, record toggle, scratch hide/show on a client
  window; asserts no panic in the shell log), `ci [--fast] [--niri-config F]` (check, clippy
  -D warnings, test, build, shot-check all themes, smoke; summary table).
- CI recipe (build the fork first: `nix build .#niri`): `NIRI=$PWD/../result/bin/niri cargo xtask ci
  --niri-config crates/panels/scratchpad/tests/assets/nested-niri-scratch.kdl`.

| variable | effect |
|---|---|
| `ASHD_NESTED=1` | nested session: debug `ashd-lock` may run, brightness/nightlight fakes allowed, polkit mock allowed |
| `ASHD_FAKE_POWER=1` | logind / power panels log instead of suspend/reboot/power-off/logout; theme reload off |
| `ASHD_NO_RELOAD=1` | theme changes never signal kitty / niri |
| `ASHD_STATE_DIR`, `ASHD_ETC_DIR`, `ASHD_SOCKET` | state dir, `/etc/ashd` replacement, IPC socket |
| `ASHD_NET_MOCK=<scenario>` | network service serves a canned state (`connected`, `nm`, `wifi`, `limited`, …); Disconnect/Connect raise link events |
| `ASHD_BT_MOCK=<scenario>` | canned Bluetooth state |
| `ASHD_FAKE_BRIGHTNESS=n`, `ASHD_FAKE_NIGHTLIGHT=1` | fake displays / gamma (with `ASHD_NESTED=1`) |
| `ASHD_POLKIT_AGENT=ashd`, `ASHD_POLKIT_BUS`, `ASHD_POLKIT_HELPER` | polkit agent on / mock bus + helper (`polkit_mock` example) |
| `ASHD_FM_BIN`, `ASHD_LOCK_BIN`, `ASHD_TERMINAL`, `ASHD_KEYBINDS`, `ASHD_ICON_THEME`, `ASHD_SEARCH_URL`, `ASHD_CLIPBOARD_PRIMARY`, `ASHD_REDUCE_MOTION` | binaries / config overrides |
