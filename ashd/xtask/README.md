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
  widths from the image (rail width, bar height, status width 560 with its right border,
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

## ci

`cargo xtask ci [--fast] [--niri-config F]`: `cargo check --workspace --all-targets`,
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, then
build + `shot-check --theme all` + `smoke` (`--fast` skips the nested steps;
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
