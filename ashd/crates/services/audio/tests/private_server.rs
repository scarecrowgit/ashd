//! End-to-end test against a throw-away PulseAudio daemon with null sinks on a private
//! socket — never the user's server. Opt-in: set `ASHD_TEST_PULSEAUDIO` to the
//! `pulseaudio` binary, e.g.
//!
//!     ASHD_TEST_PULSEAUDIO=$(nix-build '<nixpkgs>' -A pulseaudio --no-out-link)/bin/pulseaudio \
//!         cargo test -p ashd-svc-audio --test private_server
//!
//! Without the variable the test passes trivially (prints that it was skipped).

use std::{
    path::Path,
    process::{Child, Command as Proc, Stdio},
    time::Duration,
};

use async_channel::Receiver;
use ashd_svc_audio::{AudioState, Target, start_with_server};

fn spawn_daemon(bin: &str, dir: &Path) -> Child {
    let sock = dir.join("native");
    Proc::new(bin)
        .args(["-n", "--daemonize=no", "--use-pid-file=no", "--exit-idle-time=-1", "--log-target=stderr"])
        .arg("-L")
        .arg(format!("module-native-protocol-unix socket={} auth-anonymous=1", sock.display()))
        .args(["-L", "module-null-sink sink_name=null_a sink_properties=device.description=Alpha"])
        .args(["-L", "module-null-sink sink_name=null_b sink_properties=device.description=Beta"])
        .args(["-L", "module-null-source source_name=null_mic"])
        .env("PULSE_RUNTIME_PATH", dir)
        .env("PULSE_STATE_PATH", dir)
        .env("PULSE_CONFIG_PATH", dir)
        .env("XDG_RUNTIME_DIR", dir)
        .env("HOME", dir)
        .env_remove("PULSE_SERVER")
        // The daemon claims org.PulseAudio1 on the session bus (and exits when the real
        // pipewire-pulse owns it): point it at a bus that does not exist.
        .env("DBUS_SESSION_BUS_ADDRESS", format!("unix:path={}/no-bus", dir.display()))
        .env("DBUS_SYSTEM_BUS_ADDRESS", format!("unix:path={}/no-bus", dir.display()))
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn pulseaudio")
}

/// Receive states until `pred` holds (or panic after 10 s).
fn wait_for(rx: &Receiver<AudioState>, what: &str, pred: impl Fn(&AudioState) -> bool) -> AudioState {
    smol::block_on(async {
        let deadline = smol::Timer::after(Duration::from_secs(10));
        let found = smol::future::or(
            async {
                loop {
                    match rx.recv().await {
                        Ok(s) if pred(&s) => return Some(s),
                        Ok(_) => {}
                        Err(_) => return None,
                    }
                }
            },
            async {
                deadline.await;
                None
            },
        )
        .await;
        found.unwrap_or_else(|| panic!("timed out waiting for: {what}"))
    })
}

#[test]
fn commands_reach_a_private_server_and_the_service_reconnects() {
    let Ok(bin) = std::env::var("ASHD_TEST_PULSEAUDIO") else {
        eprintln!("skipped: ASHD_TEST_PULSEAUDIO not set");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let server = format!("unix:{}", dir.path().join("native").display());
    let mut daemon = spawn_daemon(&bin, dir.path());

    let (audio, rx) = start_with_server(Some(server));
    let s = wait_for(&rx, "all devices", |s| s.available && s.sinks.len() == 2 && s.sources.len() == 1);
    let names: Vec<_> = s.sinks.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"null_a") && names.contains(&"null_b"), "{names:?}");
    // monitors are not inputs
    assert_eq!(s.sources.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["null_mic"]);
    assert!(s.default_sink().is_some() && s.default_source().is_some());

    // volume: set, then step down; the snapshot follows the server
    audio.set_volume(Target::Sink("null_a".into()), 0.4);
    let s = wait_for(&rx, "volume 0.4", |s| s.sinks.iter().any(|d| d.name == "null_a" && (d.volume - 0.4).abs() < 0.01));
    assert!(s.sinks.iter().find(|d| d.name == "null_b").unwrap().volume > 0.9, "other sink untouched");
    audio.step_volume(Target::Sink("null_a".into()), -10);
    wait_for(&rx, "volume 0.3", |s| s.sinks.iter().any(|d| d.name == "null_a" && (d.volume - 0.3).abs() < 0.01));
    // boost above 100 % is allowed through set_volume
    audio.set_volume(Target::Sink("null_a".into()), 1.2);
    wait_for(&rx, "volume 1.2", |s| s.sinks.iter().any(|d| d.name == "null_a" && (d.volume - 1.2).abs() < 0.01));

    // mute
    audio.set_mute(Target::Sink("null_b".into()), true);
    wait_for(&rx, "muted", |s| s.sinks.iter().any(|d| d.name == "null_b" && d.muted));
    audio.toggle_mute(Target::Sink("null_b".into()));
    wait_for(&rx, "unmuted", |s| s.sinks.iter().any(|d| d.name == "null_b" && !d.muted));

    // default device
    audio.set_default(Target::Sink("null_b".into()));
    wait_for(&rx, "default null_b", |s| s.default_sink().is_some_and(|d| d.name == "null_b"));
    audio.set_mute(Target::DefaultSource, true);
    wait_for(&rx, "mic muted", |s| s.default_source().is_some_and(|d| d.muted));

    // server goes away → unavailable, comes back → available again
    daemon.kill().unwrap();
    daemon.wait().unwrap();
    wait_for(&rx, "unavailable", |s| !s.available && s.sinks.is_empty());
    let mut daemon = spawn_daemon(&bin, dir.path());
    wait_for(&rx, "available again", |s| s.available && s.sinks.len() == 2);
    daemon.kill().unwrap();
    let _ = daemon.wait();
}
