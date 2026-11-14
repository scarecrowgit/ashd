# NixOS VM test of the ashd session: greetd -> greeter -> login by typing ->
# niri.service -> ashd-shell.service -> `ashd ping`.
#
#   checks.<system>.vm          greeterKind = "tuigreet" (the default login screen)
#   checks.<system>.vm-greeter  greeterKind = "ashd": the graphical ashd-greeter inside the
#                               greeter niri. CANNOT PASS in this VM (see below).
#
# What this test can and cannot show: niri's TTY backend rejects software EGL renderers
# ("software EGL renderers are skipped", src/backend/tty.rs), and a QEMU VM inside the
# Nix sandbox has no GPU acceleration, so niri runs here WITHOUT ANY OUTPUT. The test
# therefore proves boot, login, units, IPC and the generated theme files, and nothing
# about what is drawn: no layer surface can be mapped, and the graphical greeter never
# gets a surface to draw its first frame on. Rendering is covered by `cargo xtask ci`
# (nested sessions on the real GPU).
#
# Screenshots end up in the build result (`result/*.png`).
#
#   nix build .#checks.x86_64-linux.vm --cores 6 -j 1   # needs KVM, ~2 GB of closure
#
# `pkgs` must already carry overlays/ashd.nix (the flake's pkgs does): the test framework
# makes nixpkgs.overlays read-only inside the nodes.
#
# The shell draws with wgpu on Vulkan; in the VM that is lavapipe (mesa's software
# ICD, installed through hardware.graphics) on virtio-gpu.
{ pkgs, inputs, greeterKind ? "tuigreet" }:

pkgs.testers.runNixOSTest {
  name = "ashd-vm-${greeterKind}";

  node.specialArgs = { inherit inputs; };

  nodes.machine = { lib, ... }: {
    imports = [
      inputs.home-manager.nixosModules.home-manager
      ../modules/desktop
    ];

    ashd = {
      enable = true;
      shell.enable = true;
      fm.enable = false; # the fm package is not part of this test
      greeter.kind = greeterKind;
      apps = {
        terminal = "kitty";
        browser = "kitty";
        fileManager = "kitty";
      };
      outputs."Virtual-1".mode = "1280x800";
    };

    hardware.graphics.enable = true;

    users.users.alice = {
      isNormalUser = true;
      uid = 1000;
      password = "alice";
      extraGroups = [ "video" "input" ];
    };
    home-manager = {
      useGlobalPkgs = true;
      useUserPackages = true;
      users.alice.home.stateVersion = "25.05";
    };
    system.stateVersion = "25.05";

    virtualisation = {
      memorySize = 3072;
      cores = 4;
      resolution = {
        x = 1280;
        y = 800;
      };
qemu.options = [ "-vga none" "-device virtio-gpu-pci" ];
    };
  };

  testScript = ''
    machine.start()
    machine.wait_for_unit("greetd.service")

    if "${greeterKind}" == "tuigreet":
        # tuigreet on tty1: user name, then password.
        machine.wait_until_tty_matches("1", "Username")
        machine.send_chars("alice\n")
        machine.wait_until_tty_matches("1", "Password")
        machine.send_chars("alice\n")
    else:
        # The graphical greeter: greetd runs niri (user greeter) with the greeter config, and
        # that niri starts ashd-greeter.
        machine.wait_until_succeeds("pgrep -u greeter -x niri", timeout=120)
        machine.wait_until_succeeds("pgrep -u greeter -x ashd-greeter", timeout=120)
        machine.succeed("test -s /etc/ashd/greeter.json && test -s /etc/ashd/greeter-niri.kdl")
        # The greeter logs "first frame presented" once the GPU path (vulkan -> layer
        # surface) works; only then are key presses certain to reach it.
        try:
            machine.wait_until_succeeds("journalctl -b --no-pager | grep -q 'first frame presented'", timeout=90)
        except Exception:
            # Say why: what the greeter, its compositor and the GPU stack logged.
            for cmd in [
                "ps -eo pid,user,stat,etimes,args | grep -E 'niri|greet' | grep -v grep",
                "loginctl list-sessions --no-legend; loginctl show-session 1 -p Seat -p VTNr -p Type -p Class -p Active -p State",
                "ls -la /run/user/998/ /dev/dri/ 2>&1",
                "for c in /sys/class/drm/*/status; do echo $c $(cat $c); done",
                "journalctl -b --no-pager -t ashd-greeter -n 60 2>&1",
                "journalctl -b --no-pager -u greetd -n 60 2>&1",
                "journalctl -b --no-pager _UID=998 -n 80 2>&1",
            ]:
                print(machine.execute(cmd + " || true")[1])
            machine.screenshot("greeter-failed")
            raise
        machine.sleep(2)
        machine.screenshot("greeter")

        # First try: wrong password. The name field is focused on a fresh machine; Tab moves to
        # the password field, and what is typed there before greetd asks is held back until it
        # does. Wrong password -> greetd error -> shake + error line, the name stays.
        machine.send_chars("alice")
        machine.send_key("tab")
        machine.send_chars("wrong\n")
        machine.wait_until_succeeds("journalctl -b --no-pager | grep -q 'login failed (wrong password: true)'", timeout=60)
        machine.sleep(1)
        machine.screenshot("greeter-wrong")

        # Second try: the password field is focused again with the user kept.
        machine.send_chars("alice\n")
        machine.wait_until_succeeds("journalctl -b --no-pager | grep -q 'authenticated as alice'", timeout=60)
        machine.screenshot("greeter-authenticated")
        # The greeter exits 0 after start_session, its niri quits, greetd starts the session.
        machine.wait_until_fails("pgrep -u greeter -x ashd-greeter", timeout=60)
        machine.wait_until_succeeds("journalctl -b --no-pager | grep -q 'session started by greetd'", timeout=60)
        # Next boot's greeter starts with this user.
        machine.succeed("grep -q alice /var/cache/ashd-greeter/state.json")


    # The user manager comes up after the PAM session opens; poll it through its bus.
    machine.wait_until_succeeds("systemctl is-active user@1000.service", timeout=120)
    user = "su - alice -c 'XDG_RUNTIME_DIR=/run/user/1000 {}'"
    machine.wait_until_succeeds(user.format("systemctl --user is-active niri.service"), timeout=120)
    machine.wait_until_succeeds(user.format("systemctl --user is-active ashd-shell.service"), timeout=120)

    # The CLI finds the socket through XDG_RUNTIME_DIR.
    machine.wait_until_succeeds(user.format("ashd ping"), timeout=60)
    machine.succeed(user.format("ashd status"))

    # Activation ran `ashd theme apply --quiet`.
    machine.succeed("test -s /home/alice/.local/state/ashd/generated/niri.kdl")
    machine.succeed("test -s /home/alice/.local/state/ashd/generated/kitty.conf")

    # niri's own view of its outputs and layer surfaces (both empty in this VM, see the
    # header), and QEMU's view of the scanout (ashd-session.png).
    machine.sleep(5)
    niri = "su - alice -c 'XDG_RUNTIME_DIR=/run/user/1000 NIRI_SOCKET=$(ls /run/user/1000/niri.*.sock | head -n1) niri msg {}'"
    print(machine.succeed(niri.format("outputs")))
    layers = machine.succeed(niri.format("layers"))
    print(layers)
    # Informational only: with no output (see the header) nothing can be mapped here.
    print("shell frame surfaces mapped:", "ashd-bar" in layers and "ashd-rail" in layers)
    machine.screenshot("ashd-session")
  '';
}
