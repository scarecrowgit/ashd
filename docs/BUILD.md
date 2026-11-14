# Building ashd

Three pieces, three build paths:

| Piece | Where | Built with |
| --- | --- | --- |
| Compositor: niri fork | [scarecrowgit/niri](https://github.com/scarecrowgit/niri), branch `ashd` | its own flake (`nix build .#niri` here builds the pinned revision) |
| Shell: `ashd`, `ashd-shell`, `ashd-lock`, `ashd-greeter` | `ashd/` (Cargo workspace) | crane (`packages/ashd`), or cargo in the dev shell |
| System: NixOS configuration | `flake.nix`, `hosts/`, `modules/` | `nixos-rebuild` |

Everything is x86_64-linux and assumes Nix with flakes. There is no global Rust toolchain
and no helper script anywhere: the dev shell provides cargo, and every helper is a
subcommand of `ashd` or `cargo xtask`.

## Requirements

- Nix with `nix-command` and `flakes` enabled.
- About 20 GB of free disk for a first full build: the niri and ashd release builds each
  need several GB of temporary space, the cargo dev build another ~10 GB.
- A GPU with a real Vulkan and EGL driver for the nested test session. niri refuses
  software EGL, so nothing can be shown in a VM without GPU acceleration.
- KVM for the VM test.

## Packages

```sh
nix build .#niri      # the compositor fork at the revision pinned in flake.lock
nix build .#ashd      # bin/{ashd,ashd-shell,ashd-lock,ashd-greeter} + shell completions
nix build .#fm        # the file manager (github:scarecrowgit/fm)
```

`.#ashd` is two derivations: the dependencies (GPUI from the Zed repository at a pinned
revision, wgpu, vello, zbus, …; slow, cached) and the workspace itself. The first
evaluation fetches the Zed repository (about 1.3 GB). The binaries carry their library
search path in RUNPATH, not in a wrapper, so `LD_LIBRARY_PATH` never leaks into the
applications the shell launches.

Only files matched by the source filter in `packages/ashd/default.nix` reach the build:
Cargo sources plus `tokens/`, `assets/`, `vendor/`, `fixtures/` and `*.lua`. A file pulled
in with `include_str!` from anywhere else builds with cargo and fails in Nix.

## The system

```sh
nix flake check --no-build                        # evaluate everything, incl. keybind assertions
sudo nixos-rebuild build  --flake .#Ivant-main    # build, activate nothing
sudo nixos-rebuild switch --flake .#Ivant-main    # activate now
sudo nixos-rebuild boot   --flake .#Ivant-main    # activate on the next boot
sudo nixos-rebuild switch --rollback              # previous generation (also in the boot menu)
```

`switch` is safe from inside the session: greetd is not restarted, niri reloads its config,
and the shell is a user service that restarts on its own. Use `boot` when the login manager
itself changes (the first migration replaced SDDM, and `switch` would have killed the
session).

For another machine, add `hosts/<name>/` (copy `Ivant-main`, replace
`hardware-configuration.nix` with the output of `ashd sys generate-hardware-config <name>`
or `nixos-generate-config --show-hardware-config`, edit `vars.nix`). Hosts are discovered
from the directory names. The desktop is switched on by `ashd.enable` and
`ashd.shell.enable` (`hosts/Ivant-main/desktop.nix` shows every option in use).

Check the generated niri config without starting anything:

```sh
nix eval --raw '.#nixosConfigurations.Ivant-main.config.environment.etc."niri/config.kdl".text' > /tmp/config.kdl
$(nix build .#niri --no-link --print-out-paths)/bin/niri validate -c /tmp/config.kdl
```

## Developing the shell

```sh
nix develop                       # or: nix-shell ashd/shell.nix
cd ashd
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build -p ashd-shell -p ashd-cli
```

Build output goes to `ashd/target` (ignored by git); set `CARGO_TARGET_DIR` to keep it on
another disk.

**Never start a debug `ashd-shell`, `ashd-lock` or `ashd-greeter` from a terminal of your
real session.** They would draw on your desktop and talk to the real logind. Debug builds
refuse to start unless `ASHD_NESTED=1` is set (or `--live` is passed on purpose). Run them
through the harness, which starts a private headless compositor with a nested niri inside
and scrubs every variable that points at the real session:

```sh
nix build .#niri                                   # once, in the repository root
cd ashd
export NIRI=$PWD/../result/bin/niri                # the fork; a stock niri lacks canvas and scratchpad
SCRATCH=crates/panels/scratchpad/tests/assets/nested-niri-scratch.kdl

cargo xtask nested run --niri-config $SCRATCH -- ashd-shell     # one command inside a session
root=$(cargo xtask nested start --with-bus --niri-config $SCRATCH)   # or a session to poke at
cargo xtask nested env "$root"                     # prints the exports to use it from a shell
cargo xtask shot --root "$root" --out /tmp/shot.png
cargo xtask nested stop "$root"

cargo xtask shot-check --theme all --niri-config $SCRATCH   # frame geometry and token colours, 5 themes
cargo xtask smoke --niri-config $SCRATCH                    # every CLI request against a live shell
cargo xtask ci --niri-config $SCRATCH                       # check, clippy, tests, shot-check, smoke
```

Inside the nested session power actions, theme reloads and audio are faked
(`ASHD_FAKE_POWER=1`, `ASHD_NO_RELOAD=1`, a dead `PULSE_SERVER`), and D-Bus is a private
bus when `--with-bus` is given. `ashd/xtask/README.md` documents every subcommand,
`ashd/ARCHITECTURE.md` the crates and their contracts, `docs/GPUI-NOTES.md` the GPUI
pitfalls.

## Developing the compositor

Clone the fork next to this repository; its `ASHD-FORK.md` lists every file it touches and
how to build and test it (`nix develop`, `cargo test`, the slow randomized layout tests).
To try a local checkout with this flake without committing or pushing:

```sh
nix build .#niri --override-input niri git+file:///path/to/niri
```

When the fork's IPC (`niri-ipc/src`) changes, the copy vendored in the shell must follow:

```sh
rsync -a --delete /path/to/niri/niri-ipc/src/ ashd/vendor/niri-ipc/src/
touch ashd/vendor/niri-ipc/src/*.rs      # rsync keeps mtimes; cargo would reuse stale artefacts
```

then bump the commit in the header of `ashd/vendor/niri-ipc/Cargo.toml`. After pushing to
the fork: `nix flake update niri` and commit `flake.lock`.

## The VM test

```sh
nix build .#checks.x86_64-linux.vm -L
```

boots the configuration in QEMU, logs in through the text greeter, and waits for
`niri.service`, `ashd-shell.service`, `ashd ping` and the generated theme files. It cannot
show the desktop: niri has no output without GPU acceleration, so rendering is covered by
`cargo xtask ci` instead. `.#checks.x86_64-linux.vm-greeter` is the same test with the
graphical greeter and cannot pass in a VM for the same reason.

## First boot checklist

Things only real hardware can show:

- `ashd lock`, then a wrong and a right password; `kill -9` of `ashd-lock` from another
  console must leave the session locked, and starting `ashd-lock` again must recover it.
- The graphical greeter (`ashd.greeter.kind = "ashd"`): if it does not appear, log in on
  another console (Ctrl+Alt+F2), read `journalctl -t ashd-greeter`, and set the option back
  to `"tuigreet"`.
- `ashd.polkitAgent = "ashd"`: `pkexec true` must show the built-in dialog; the default
  stays on the third-party agent until that works.
- Night light, DDC brightness (`ashd.brightness.ddc.enable`), Bluetooth pairing, tray icons
  of the applications you use.

Monitor modes: leave the refresh rate out (`mode = "1920x1080"`) to get the highest one. A
rate must match a mode from `niri msg outputs` to three decimals; anything else silently
falls back to the preferred mode.
