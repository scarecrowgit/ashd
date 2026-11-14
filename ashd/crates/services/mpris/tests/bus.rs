//! The mpris service against mock MPRIS players on a *private* `dbus-daemon` (never the
//! user's session bus). Skipped when `dbus-daemon` is not on PATH (outside the dev shell).
//!
//! The mock (`support/mock.rs`) logs every method call it receives, so the command tests
//! assert on what actually reached the player.

#[path = "support/mock.rs"]
mod mock;

use std::{
    io::{BufRead, BufReader},
    process::{Child, Command as Proc, Stdio},
    time::Duration,
};

use async_channel::Receiver;
use mock::{MockPlayer, MockTrack};
use ashd_svc_mpris::{Command, Handle, MprisState, PlaybackStatus, start_at};

struct Bus {
    child: Child,
    address: String,
}

impl Bus {
    fn start() -> Option<Bus> {
        let mut child = Proc::new("dbus-daemon").args(["--session", "--print-address", "--nofork"]).stdout(Stdio::piped()).spawn().ok()?;
        let mut line = String::new();
        BufReader::new(child.stdout.take()?).read_line(&mut line).ok()?;
        Some(Bus { child, address: line.trim().to_string() })
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Newest-state probe: remembers the last snapshot, so a state reached *before* `wait`
/// is called still counts.
struct Probe {
    rx: Receiver<MprisState>,
    last: MprisState,
}

impl Probe {
    fn wait(&mut self, what: &str, pred: impl Fn(&MprisState) -> bool) -> MprisState {
        while let Ok(s) = self.rx.try_recv() {
            self.last = s;
        }
        if pred(&self.last) {
            return self.last.clone();
        }
        let rx = self.rx.clone();
        let s = smol::block_on(smol::future::or(
            async {
                loop {
                    match rx.recv().await {
                        Ok(s) if pred(&s) => return s,
                        Ok(s) => self.last = s,
                        Err(_) => panic!("state channel closed waiting for {what}"),
                    }
                }
            },
            async {
                smol::Timer::after(Duration::from_secs(10)).await;
                panic!("timed out waiting for: {what}")
            },
        ));
        self.last = s.clone();
        s
    }
}

fn wait_log(player: &MockPlayer, want: &str) {
    for _ in 0..200 {
        if player.log().iter().any(|l| l == want) {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("{} never saw `{want}`; saw {:?}", player.bus_name, player.log());
}

fn album() -> Vec<MockTrack> {
    vec![
        MockTrack::new(1, "Intro", "Field Recorder", "Sessions", 90),
        MockTrack::new(2, "field-recording-03", "Field Recorder", "Sessions", 400),
        MockTrack::new(3, "Outro", "Field Recorder", "Sessions", 252),
    ]
}

struct Fixture {
    _bus: Bus,
    handle: Handle,
    probe: Probe,
    mpd: MockPlayer,
    firefox: MockPlayer,
}

/// Service + two players: `mpd` (track list, 3 tracks, starts on track 2) and `firefox`
/// (no track list, one track).
fn fixture() -> Option<Fixture> {
    let Some(bus) = Bus::start() else {
        eprintln!("dbus-daemon not found: skipping");
        return None;
    };
    let (handle, rx) = start_at(Some(bus.address.clone()));
    let mut probe = Probe { rx, last: MprisState::default() };
    probe.wait("bus connection", |s| s.available);

    let mpd = smol::block_on(MockPlayer::start(&bus.address, "mpd", "Music Player Daemon", album(), true)).unwrap();
    smol::block_on(mpd.set_current(1)).unwrap();
    probe.wait("mpd listed", |s| s.player(&mpd.bus_name).is_some_and(|p| p.track.title == "field-recording-03"));
    let ff_tracks = vec![MockTrack::new(7, "Some video", "", "", 600)];
    let firefox = smol::block_on(MockPlayer::start(&bus.address, "firefox.instance_1_2", "Mozilla Firefox", ff_tracks, false)).unwrap();
    probe.wait("both listed", |s| s.players.len() == 2);
    Some(Fixture { _bus: bus, handle, probe, mpd, firefox })
}

#[test]
fn unavailable_without_a_bus() {
    let (_handle, rx) = start_at(Some("unix:path=/nonexistent/ashd-mpris-test-bus".into()));
    let mut probe = Probe { rx, last: MprisState { available: true, ..Default::default() } };
    let s = probe.wait("initial unavailable state", |s| !s.available);
    assert!(s.players.is_empty() && s.active.is_none());
}

#[test]
fn discovery_identity_metadata_and_capabilities() {
    let Some(mut f) = fixture() else { return };
    let s = f.probe.wait("two players", |s| s.players.len() == 2);
    assert!(s.available);

    let mpd = s.player(&f.mpd.bus_name).unwrap();
    assert_eq!(mpd.short_name(), "mpd");
    assert_eq!(mpd.identity, "Music Player Daemon");
    assert_eq!(mpd.display_name(), "Music Player Daemon");
    assert_eq!(mpd.desktop_entry.as_deref(), Some("mpd"));
    assert_eq!(mpd.status, PlaybackStatus::Stopped);
    assert_eq!(mpd.track.title, "field-recording-03");
    assert_eq!(mpd.track.artists, ["Field Recorder"]);
    assert_eq!(mpd.track.album, "Sessions");
    assert_eq!(mpd.track.length, Some(Duration::from_secs(400)));
    assert_eq!(mpd.track.id.as_deref(), Some("/org/mock/track/2"));
    assert!(mpd.can_go_next && mpd.can_go_previous && mpd.can_play && mpd.can_pause && mpd.can_seek && mpd.can_control);
    assert_eq!(mpd.volume, Some(1.0));

    // queue from the TrackList
    assert!(mpd.has_track_list);
    assert_eq!(mpd.queue.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(), ["Intro", "field-recording-03", "Outro"]);
    assert_eq!(mpd.queue_index(), Some(1));
    assert_eq!(mpd.queue[2].length, Some(Duration::from_secs(252)));

    // no TrackList: empty queue
    let ff = s.player(&f.firefox.bus_name).unwrap();
    assert_eq!(ff.short_name(), "firefox.instance_1_2");
    assert!(!ff.has_track_list && ff.queue.is_empty());
    assert_eq!(ff.track.title, "Some video");
    assert!(!ff.can_go_next && !ff.can_go_previous, "single track");
}

#[test]
fn active_player_follows_playback_and_pinning() {
    let Some(mut f) = fixture() else { return };
    // both stopped: the most recently changed one (firefox joined last)
    let s = f.probe.wait("firefox active", |s| s.active.as_deref() == Some(&f.firefox.bus_name));
    assert_eq!(s.pinned, None);

    smol::block_on(f.mpd.set_status("Playing")).unwrap();
    f.probe.wait("mpd active while playing", |s| s.active.as_deref() == Some(&f.mpd.bus_name));
    smol::block_on(f.firefox.set_status("Playing")).unwrap();
    f.probe.wait("firefox started last", |s| s.active.as_deref() == Some(&f.firefox.bus_name));
    smol::block_on(f.firefox.set_status("Paused")).unwrap();
    let s = f.probe.wait("mpd still playing", |s| s.active.as_deref() == Some(&f.mpd.bus_name));
    assert!(s.active_player().unwrap().is_playing());

    // pinning wins over playback, unpinning gives it back
    f.handle.set_active(f.firefox.bus_name.clone());
    let s = f.probe.wait("pinned firefox", |s| s.active.as_deref() == Some(&f.firefox.bus_name));
    assert_eq!(s.pinned.as_deref(), Some(f.firefox.bus_name.as_str()));
    f.handle.unpin();
    f.probe.wait("unpinned", |s| s.pinned.is_none() && s.active.as_deref() == Some(&f.mpd.bus_name));

    // a pinned player that leaves the bus unpins itself
    f.handle.set_active(f.firefox.bus_name.clone());
    f.probe.wait("pinned again", |s| s.pinned.is_some());
    smol::block_on(f.firefox.conn.release_name(f.firefox.bus_name.as_str())).unwrap();
    let s = f.probe.wait("firefox gone", |s| s.players.len() == 1);
    assert_eq!(s.pinned, None);
    assert_eq!(s.active.as_deref(), Some(f.mpd.bus_name.as_str()));
}

#[test]
fn commands_reach_the_right_player() {
    let Some(mut f) = fixture() else { return };
    smol::block_on(f.mpd.set_status("Playing")).unwrap();
    f.probe.wait("mpd active", |s| s.active.as_deref() == Some(&f.mpd.bus_name));

    // untargeted → active player
    f.handle.play_pause();
    wait_log(&f.mpd, "PlayPause");
    f.probe.wait("mpd paused", |s| s.player(&f.mpd.bus_name).unwrap().status == PlaybackStatus::Paused);
    assert!(f.firefox.log().is_empty());

    // named → that player, whatever is active
    f.handle.send(Command::PlayPause(Some(f.firefox.bus_name.clone())));
    wait_log(&f.firefox, "PlayPause");
    f.probe.wait("firefox playing", |s| s.player(&f.firefox.bus_name).unwrap().status == PlaybackStatus::Playing);

    // next / previous / stop on mpd by name; track metadata follows
    let mpd = f.mpd.bus_name.clone();
    f.handle.send(Command::Next(Some(mpd.clone())));
    wait_log(&f.mpd, "Next");
    f.probe.wait("outro", |s| s.player(&mpd).unwrap().track.title == "Outro");
    let s = f.probe.wait("outro is last", |s| !s.player(&mpd).unwrap().can_go_next);
    assert_eq!(s.player(&mpd).unwrap().queue_index(), Some(2));
    f.handle.send(Command::Previous(Some(mpd.clone())));
    wait_log(&f.mpd, "Previous");
    f.probe.wait("back on 03", |s| s.player(&mpd).unwrap().track.title == "field-recording-03");
    f.handle.send(Command::Stop(Some(mpd.clone())));
    wait_log(&f.mpd, "Stop");
    f.probe.wait("mpd stopped", |s| s.player(&mpd).unwrap().status == PlaybackStatus::Stopped);

    // the queue row click
    f.handle.send(Command::GoTo { player: Some(mpd.clone()), track_id: "/org/mock/track/1".into() });
    wait_log(&f.mpd, "GoTo(/org/mock/track/1)");
    f.probe.wait("intro", |s| s.player(&mpd).unwrap().track.title == "Intro");

    // commands for a player that does not exist are dropped silently
    f.handle.send(Command::Next(Some("org.mpris.MediaPlayer2.ghost".into())));
}

#[test]
fn seek_uses_set_position_and_updates_the_position() {
    let Some(mut f) = fixture() else { return };
    let mpd = f.mpd.bus_name.clone();
    f.handle.set_active(mpd.clone());
    f.probe.wait("mpd pinned", |s| s.active.as_deref() == Some(&mpd));
    f.handle.seek_to(Duration::from_secs(30));
    wait_log(&f.mpd, "SetPosition(/org/mock/track/2,30000000)");
    let s = f.probe.wait("position 30 s", |s| s.player(&mpd).unwrap().position == Duration::from_secs(30));
    assert_eq!(s.player(&mpd).unwrap().progress(), 30.0 / 400.0);

    // beyond the end is clamped to the track length
    f.handle.seek_to(Duration::from_secs(10_000));
    wait_log(&f.mpd, "SetPosition(/org/mock/track/2,400000000)");

    // the player itself jumps: `Seeked`
    smol::block_on(f.mpd.jump_to(77_000_000)).unwrap();
    f.probe.wait("seeked to 77 s", |s| s.player(&mpd).unwrap().position == Duration::from_secs(77));

    // a player that cannot seek is left alone
    let ff = f.firefox.bus_name.clone();
    smol::block_on(f.firefox.set_can_seek(false)).unwrap();
    f.probe.wait("firefox cannot seek", |s| !s.player(&ff).unwrap().can_seek);
    f.handle.send(Command::SeekTo { player: Some(ff.clone()), position: Duration::from_secs(5) });
    f.handle.send(Command::SeekTo { player: Some(mpd.clone()), position: Duration::from_secs(1) });
    wait_log(&f.mpd, "SetPosition(/org/mock/track/2,1000000)");
    assert!(f.firefox.log().iter().all(|l| !l.starts_with("Se")), "{:?}", f.firefox.log());
}

#[test]
fn position_is_polled_while_playing_and_only_when_asked() {
    let Some(mut f) = fixture() else { return };
    let mpd = f.mpd.bus_name.clone();
    f.handle.set_active(mpd.clone());
    smol::block_on(f.mpd.set_status("Playing")).unwrap();
    f.probe.wait("playing", |s| s.player(&mpd).unwrap().status == PlaybackStatus::Playing);
    // the clock advances without any signal: only polling can see it
    let s = f.probe.wait("position advances", |s| s.player(&mpd).unwrap().position >= Duration::from_millis(1900));
    assert!(s.player(&mpd).unwrap().position < Duration::from_secs(5));

    // polling off: no more updates (a Seeked still comes through)
    f.handle.set_position_polling(false);
    std::thread::sleep(Duration::from_millis(300));
    while f.probe.rx.try_recv().is_ok() {}
    std::thread::sleep(Duration::from_millis(2300));
    assert!(f.probe.rx.try_recv().is_err(), "no polling while switched off");
    smol::block_on(f.mpd.jump_to(5_000_000)).unwrap();
    f.probe.wait("seeked while polling is off", |s| s.player(&mpd).unwrap().position == Duration::from_secs(5));

    // back on: caught up immediately
    f.handle.set_position_polling(true);
    f.probe.wait("polling resumes", |s| s.player(&mpd).unwrap().position > Duration::from_secs(5));
}

#[test]
fn queue_follows_the_track_list_signals() {
    let Some(mut f) = fixture() else { return };
    let mpd = f.mpd.bus_name.clone();
    smol::block_on(f.mpd.add_track(MockTrack::new(4, "Bonus", "Field Recorder", "Sessions", 61))).unwrap();
    let s = f.probe.wait("bonus in queue", |s| s.player(&mpd).unwrap().queue.len() == 4);
    assert_eq!(s.player(&mpd).unwrap().queue[3].title, "Bonus");
    assert_eq!(s.player(&mpd).unwrap().queue[3].length, Some(Duration::from_secs(61)));
}

#[test]
fn album_art_file_urls_become_paths_and_remote_ones_do_not() {
    let Some(mut f) = fixture() else { return };
    let mpd = f.mpd.bus_name.clone();
    let dir = std::env::temp_dir().join(format!("ashd-mpris-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let art = dir.join("cover art.png");
    std::fs::write(&art, b"not really a png").unwrap();
    let url = format!("file://{}", art.display().to_string().replace(' ', "%20"));

    smol::block_on(f.mpd.set_art(Some(url.clone()))).unwrap();
    let s = f.probe.wait("art path", |s| s.player(&mpd).unwrap().track.art_path.is_some());
    assert_eq!(s.player(&mpd).unwrap().track.art_path.as_deref(), Some(art.as_path()));
    assert_eq!(s.player(&mpd).unwrap().track.art_url.as_deref(), Some(url.as_str()));

    // a file that does not exist: no path
    smol::block_on(f.mpd.set_art(Some("file:///nonexistent/cover.png".into()))).unwrap();
    let s = f.probe.wait("missing art", |s| s.player(&mpd).unwrap().track.art_url.as_deref() == Some("file:///nonexistent/cover.png"));
    assert_eq!(s.player(&mpd).unwrap().track.art_path, None);

    // remote art is not fetched (no HTTP client): the URL is kept, the path is not
    smol::block_on(f.mpd.set_art(Some("https://example.org/cover.jpg".into()))).unwrap();
    let s = f.probe.wait("remote art", |s| s.player(&mpd).unwrap().track.art_url.as_deref() == Some("https://example.org/cover.jpg"));
    assert_eq!(s.player(&mpd).unwrap().track.art_path, None);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn playerctld_is_ignored_and_players_can_come_and_go() {
    let Some(mut f) = fixture() else { return };
    let address = f._bus.address.clone();
    let ctld = smol::block_on(MockPlayer::start(&address, "playerctld", "playerctl", album(), false)).unwrap();
    let late = smol::block_on(MockPlayer::start(&address, "late", "Late Player", album(), false)).unwrap();
    let s = f.probe.wait("late player listed", |s| s.player(&late.bus_name).is_some());
    assert!(s.player(&ctld.bus_name).is_none(), "playerctld duplicates the active player");
    assert_eq!(s.players.len(), 3);
    // sorted by bus name
    let names: Vec<_> = s.players.iter().map(|p| p.bus_name.clone()).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);

    smol::block_on(late.conn.release_name(late.bus_name.as_str())).unwrap();
    f.probe.wait("late player gone", |s| s.players.len() == 2);
}
