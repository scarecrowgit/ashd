# xtask — the ashd test harness

All tooling is Rust (project rule: no bash/python helper scripts). Run inside the dev shell:

    nix-shell ashd/shell.nix --run 'cd ashd && cargo xtask <task> …'

(`cargo xtask` is the alias from `ashd/.cargo/config.toml`; `xtask` builds with `-p xtask`
only, it does not wait for the rest of the workspace.)

## Nested session

Headless sway (wlroots headless backend) → niri (winit) with `assets/nested-niri.kdl`, in
a private `XDG_RUNTIME_DIR` (`/tmp/ashd-nested.XXXXXX`, mode 0700): the root holds the
compositor configs, logs, pid files, sockets and an `env` file to `source`.

| command | what |
|---|---|
| `nested start [--size 1920x1080] [--with-bus] [--theme ash] [--niri-config file]` | start, print the root (`root=$(cargo xtask nested start)`); `source "$root/env"` |
| `nested env <root>` | print the `export …` lines (`eval "$(cargo xtask nested env $root)"`) |
| `nested run [--keep] [--size …] [--with-bus] [--niri-config file] -- <cmd…>` | start, run `cmd` inside, tear down; exit code = the command's |
| `nested stop <root> [--keep]` | kill everything started in the session, remove the root |
| `nested list` | roots, alive / dead, pids, age |
| `nested gc` | remove roots of dead sessions (never a live or just-starting one) |

`--niri-config <file>` replaces the built-in niri config (the Ash colours in it are still swapped
for `--theme` / `shot-check`'s theme). `NIRI=<binary>` picks the compositor; the ashd fork needs it for
`workspace "scratch" { hidden }`, which stock niri rejects.

Environment inside a session (`env`, `run`, and every process xtask starts):
`XDG_RUNTIME_DIR=$root`, `WAYLAND_DISPLAY` (niri's socket), `NIRI_SOCKET`,
`ASHD_STATE_DIR=$root/ashd-state`, `HOME`/`XDG_{CONFIG,STATE,DATA}_HOME` under
`$root/home`, `ASHD_NESTED=1`, `ASHD_FAKE_POWER=1` (logind / power panels log instead of
rebooting the real machine), `ASHD_NO_RELOAD=1` (theme switching never signals the real
kitty/niri), `PULSE_SERVER=unix:$root/no-pulse` (no path to real audio), `ASHD_PARENT_WAYLAND_DISPLAY` (sway's
socket, for `grim`), and with `--with-bus` `DBUS_SESSION_BUS_ADDRESS` of a private
`dbus-daemon --session` (`$root/bus`, no bus-activation directories). **Scrubbed** (removed
from every child, and `unset` in `env`): `WAYLAND_DISPLAY` of the live desktop, `DISPLAY`,
`WAYLAND_SOCKET`, `NIRI_SOCKET`, `SWAYSOCK`, `DBUS_SESSION_BUS_ADDRESS`, `ASHD_SOCKET`,
other `PULSE_*`, `PIPEWIRE_*`, `XAUTHORITY`, activation tokens. Without `--with-bus`
`DBUS_SESSION_BUS_ADDRESS` is unset (services report `available: false`).

Teardown: sway/niri/dbus are process-group leaders; teardown SIGTERMs, then SIGKILLs the
pid-file groups **and every process whose environment carries the root as
`XDG_RUNTIME_DIR`** (so nothing a test spawned survives), then removes the root. It runs
on normal exit, on error/panic (drop guard), and on SIGINT/SIGTERM/SIGHUP. Children of
`run`/`shot-check`/`smoke` also get `PR_SET_PDEATHSIG(SIGKILL)`, so they die even if xtask
is SIGKILLed; `nested gc` cleans the leftover roots.

## Screenshots

`cargo xtask shot [--out png] [--root dir] [--via grim|niri]` — screenshot of the session
(`--root` defaults to `$XDG_RUNTIME_DIR` when it is a session, else the only live one).
`grim` captures sway's headless output (the whole 1920x1080); `niri` uses niri's
`screenshot-screen --path` action. Output is decoded with the `image` crate. Default
`ashd/target-notes/shot.png`.

## shot-check

`cargo xtask shot-check [--theme ash|ink|midnight|moss|dusk|all] [--no-build] [--niri-config F] [--keep]`

Builds `ashd-shell` and `ashd`, starts a session and the shell, waits for `ashd ping`, and
asserts, per theme (`all` switches live with `ashd theme set <t> --no-reload`, also
rewriting the nested niri background):

- `ashd status`: theme, `niri_connected`, no modal, surfaces rail / bar / scratchpad with
  the sizes from the theme's `Shape` (Ash: rail 64x1034, bar 1920x46, scratchpad 340x1034;
  floating themes inset by `gap`);
- pixels (exact token colours): main area = `bg`, rail / bar / status / scratchpad fill =
  `surface`, 1px borders = `line` (or `frame`), accent pixels in the rail, plus *measured*
  widths from the image (rail width, bar height, status width — content width, at most 560 — with its right border,
  scratchpad 340), with the scratchpad shown (`ashd scratch open`) and hidden (`ashd scratch close`);
- the shell is still running and logged no panic.

Prints PASS/FAIL per probe, exits non-zero on any failure. Saves
`target-notes/check-<theme>.png`, `check-<theme>-annotated.png` (probe points: green pass,
red fail) and `check-<theme>-noscratch.png`.

## smoke

`cargo xtask smoke [--no-build] [--skip <step>] [--include-lock] [--niri-config F] [--keep]`

Nested session **with private bus** and a plain client window (`$SMOKE_CLIENT`, else
`foot` from `PATH`); every `ashd` command that talks to the shell (`ping status
launcher[--mode calc] quick-settings session notifications clipboard cheatsheet calendar
media media play-pause theme pick screenshot --region record toggle dismiss osd volume
±N|mute, mic-mute, brightness scratch toggle|open|close|hide|show canvas … theme
get|cycle|set app quit`, finally `quit-shell`; `lock` only with `--include-lock` because it
locks the nested session; `record toggle` only opens the overlay in Record — nothing records;
`fm` is not driven). `scratch hide|show` need `NIRI=` the ashd fork and `--niri-config` with
`workspace "scratch" { hidden }` (e.g. `crates/panels/scratchpad/tests/assets/nested-niri-scratch.kdl`);
without a client window they are skipped. Each answer is classified `ok`, `not-impl`
(documented "no panel handles `x` yet", "… unavailable", "no active media player") or
`FAIL`; after each step the shell must be alive with no new panic line in its log (and
`smoke` refuses to start unless `ASHD_FAKE_POWER=1` and `ASHD_NO_RELOAD=1` are in the shell's
own environment), and shell-core-guaranteed state is verified (modal open/closed,
scratchpad surface, theme). Screenshot per step: `target-notes/smoke-<step>.png`. Prints a
table; exit 1 on any FAIL, dead shell or panic.

## motion

`cargo xtask motion [--scale 10] [--frames 14] [--only NAME]… [--env KEY=VALUE]… [--reduce-motion] [--no-build] [--niri-config F]`

Does every surface really move? A nested session with a private bus; the shell runs with
`ASHD_MOTION_SCALE` (every shell tween and spring takes N× as long), `ASHD_FRAME_LOG=1` and,
and, for the second batch only (the shell restarts for it), `ASHD_BAR_FIXTURE=calm` (frozen,
narrow bar readouts: a region of the bar only changes when a scenario changes it) and
`ASHD_NET_MOCK=vpn`, unless `--env` says otherwise. It builds the two
mocks `mock_tray` / `mock_player` (examples of `ashd-svc-tray` / `ashd-svc-mpris`) too.

First batch, per scenario (launcher, quick-settings, calendar, session, notifications,
clipboard, cheatsheet, theme-pick, scratchpad, canvas (the Scroll/Canvas toggle); `--only
switch` (a modal leaves while another opens), `--only dock` (a `kitty --class` window comes and
goes), `--only rail` (the active marker slides between workspaces), `--only hover` moves the
pointer over a dock button, the bar's Canvas segment and power button, a rail workspace and a
quick-settings tile through `wlrctl`; `--only toasts` posts and closes notifications through
`notify-send` / `gdbus`):

1. screenshot burst while it opens: at least two frames must differ from both the settled
   *before* and *after* pictures (the frames in between exist); the picture is judged *settled*
   when the shell's own frame log has been quiet (nothing but the bar drew) for two samples;
2. the same while it closes, then *close restores the before state*;
3. open → close → open back to back (a reversal in flight) must settle exactly like a plain
   open, close → open → close exactly like the start (popups anchored to bar icons are compared
   after realigning ±12 px: the status block's live width moves them);
4. idle: seven seconds with nothing happening — rail, scratchpad and overlays must log zero
   frames, the bar (fed by the 1 Hz system sample and the 160 ms meter ease) must not hold a
   frame loop.

Second batch (shell crates; each judges a *region* of the screenshot, same in-between rule):

| `--only` | what |
|---|---|
| `bar` | tray items arrive / leave (`mock_tray`), now-playing fades in / out (`mock_player`), the unread dot pops in and shrinks away (`notify-send`, `CloseNotification`; fixture `calm` has no forced dot) |
| `launcher-ui` | the selection plate glides to the hovered row and back, the mode chip swaps (click on it), an inline `>` command's output enters (`wlrctl keyboard type`), closing the card while `sleep` runs still animates and leaves the shell alive and the process cleaned up |
| `osd` | fade in, fade out, the volume to brightness cross-fade, two requests 300 ms apart settle to nothing |
| `mode` | with a `kitty` window: Scroll to Canvas (strip header leaves, minimap card shows around the switch and is hidden when idle), a settled Canvas desktop draws no frames, the minimap card collapses into its chip and expands again, back to Scroll |
| `shape` | `theme set dusk` / `ash` (another frame shape: the frame surfaces are recreated and fade in) |

The live status block of the bar (clock, cpu, mem, network) is excluded from the first
batch's comparisons. Frames are `target-notes/motion-<scenario>-<phase>-<n>.png`, the shell log
`target-notes/motion-shell.log`. Set `GRIM=…/bin/grim` and `WLRCTL=…/bin/wlrctl` (otherwise
each screenshot runs `nix-build '<nixpkgs>' -A grim` first and takes seconds). `--reduce-motion` starts the shell with `ASHD_REDUCE_MOTION=1`: bursts must then
show no more than 3 in-between frames (the 80 ms fade); the second batch is skipped there.

## fullscreen

`cargo xtask fullscreen [--no-build] [--niri-config F]` (needs `NIRI=` the ashd fork with
`fullscreen-workspaces` and `edge-reveal`): a nested session with
`assets/nested-niri-fullscreen.kdl` (`fullscreen-workspaces`, `create-workspaces-by-index`,
`edge-reveal` for `^ashd-(rail|bar)$`, hidden scratch workspace), the shell, and a `kitty`
window. A window goes fullscreen (`niri msg action fullscreen-window`):

- niri lists one workspace with `idx` 0 and `fullscreen_origin_id` / `fullscreen_window_id` set;
- the rail and the bar are covered (by the compositor: the shell blanks nothing), the strip header
  (overlay layer, hidden by the shell) is off; the OSD and a toast still show over the window;
- the pointer rests at the left edge (`wlrctl`, relative moves): the rail slides in with its real
  content, the desk and the compact app entry with its lit marker; at the bottom edge: the bar,
  its status block pixel-identical to the windowed bar; away from both: covered again;
- leaving fullscreen: the workspace is gone and rail and bar look as before.

Screenshots: `target-notes/fullscreen-NN-<step>.png`. `ci` runs it after `smoke`.

## ci

`cargo xtask ci [--fast] [--niri-config F]`: `cargo check --workspace --all-targets`,
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, then
build + `shot-check --theme all` + `smoke` + `fullscreen` (`--fast` skips the nested steps;
`--niri-config` goes to both). Summary table, exit 1 if anything failed (all steps always
run). The full gate:

    NIRI=$PWD/../result/bin/niri cargo xtask ci \          # after `nix build .#niri` in the repo root
        --niri-config crates/panels/scratchpad/tests/assets/nested-niri-scratch.kdl

## Notes for agents

- Run anything with a window only through these commands or `nested run` — never on the
  live desktop.
- `cargo xtask nested run -- true` is the quickest "is the nested stack healthy" check.
- Extra tools are looked up as `$SWAY`, `$NIRI`, `$DBUS_DAEMON`, `$GRIM`, then `PATH`, then
  `nix-build '<nixpkgs>' -A <pkg>`.
- Failure artefacts: `target-notes/{check,smoke}-shell.log` (the shell's stderr).
- Never run `cargo` in a shell that sourced `$root/env`: it sets `HOME` to the session's
  throwaway home, so cargo starts a fresh registry there and rebuilds every dependency in the
  shared target dir. Source the env only for the binaries you start in the session.
