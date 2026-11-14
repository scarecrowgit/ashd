//! End-to-end tests of the `ashd` binary against a fake ashd-shell (real ashd-ipc wire
//! protocol), a fake niri and fake helper programs (`wpctl`, `tmux`, `git`, `fm`,
//! `nixos-generate-config`, `ashd-lock`).
//!
//! Hygiene: every run gets a scrubbed environment (own HOME, `ASHD_STATE_DIR`,
//! `ASHD_SOCKET`, `PATH`; no `NIRI_SOCKET`, no `XDG_RUNTIME_DIR`), so nothing here can reach
//! the live shell, the live niri, real volume or the user's theme files. Theme tests
//! always pass `--no-reload`.

use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{Arc, Mutex},
    thread,
};

use ashd_ipc::{
    CanvasOp, LauncherMode, MediaControl, OsdRequest, PanelOp, Request, Response, ScratchOp, ShellStatus, SurfaceStatus, ThemeRequest,
    VolumeChange,
};
use ashd_tokens::ThemeName;

const ASHD: &str = env!("CARGO_BIN_EXE_ashd");

// ── harness ───────────────────────────────────────────────────────────────────

struct Env {
    dir: tempfile::TempDir,
}

struct Run {
    out: Output,
}

impl Run {
    fn code(&self) -> i32 {
        self.out.status.code().expect("ashd was killed by a signal")
    }
    fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.out.stdout).into_owned()
    }
    fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.out.stderr).into_owned()
    }
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for d in ["bin", "home", "state"] {
            fs::create_dir(dir.path().join(d)).unwrap();
        }
        Self { dir }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn socket(&self) -> PathBuf {
        self.path("shell.sock")
    }

    fn state_dir(&self) -> PathBuf {
        self.path("state")
    }

    /// Install an executable `#!/bin/sh` script `name` into the private PATH dir.
    fn tool(&self, name: &str, body: &str) -> PathBuf {
        self.script(&self.path("bin").join(name), body)
    }

    fn script(&self, path: &Path, body: &str) -> PathBuf {
        fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        path.to_path_buf()
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(ASHD);
        c.args(args)
            .env_clear()
            .env("HOME", self.path("home"))
            .env("ASHD_STATE_DIR", self.state_dir())
            .env("ASHD_SOCKET", self.socket())
            .env("PATH", self.path("bin"))
            .current_dir(self.path("home"));
        c
    }

    fn run(&self, args: &[&str]) -> Run {
        Run { out: output_retrying(&mut self.command(args)) }
    }

    fn run_with(&self, args: &[&str], f: impl FnOnce(&mut Command)) -> Run {
        let mut c = self.command(args);
        f(&mut c);
        Run { out: output_retrying(&mut c) }
    }
}

/// `c.output()`, retried while an executable involved is "busy" (ETXTBSY). Two sources,
/// neither a bug in `ashd`: a test thread's freshly written fake tool (or `ashd` copy) is
/// still open for writing in a child another test thread just forked (the fd closes on
/// that child's exec), and a concurrent `cargo build` in the shared target dir rewriting
/// the `ashd` binary itself. ETXTBSY can surface as a spawn error (exec of `ashd`) or as
/// `ashd`'s own exec of a helper failing ("Text file busy" on stderr).
fn output_retrying(c: &mut Command) -> Output {
    const BUSY: &str = "Text file busy";
    for _ in 0..50 {
        match c.output() {
            Ok(out) if String::from_utf8_lossy(&out.stderr).contains(BUSY) => {}
            Ok(out) => return out,
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {}
            Err(e) => panic!("spawn {:?}: {e}", c.get_program()),
        }
        thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("{:?} stayed busy (ETXTBSY) for 5 s", c.get_program());
}

/// A fake ashd-shell speaking the real protocol.
struct FakeShell {
    requests: Arc<Mutex<Vec<Request>>>,
}

impl FakeShell {
    fn start(env: &Env, answer: impl Fn(&Request) -> Option<Response> + Send + Sync + 'static) -> Self {
        let listener = UnixListener::bind(env.socket()).unwrap();
        let requests: Arc<Mutex<Vec<Request>>> = Arc::default();
        let log = requests.clone();
        let answer = Arc::new(answer);
        thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let (log, answer) = (log.clone(), answer.clone());
                thread::spawn(move || serve(conn, log, answer));
            }
        });
        Self { requests }
    }

    fn ok(env: &Env) -> Self {
        Self::start(env, |_| Some(Response::Ok))
    }

    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}

fn serve(conn: UnixStream, log: Arc<Mutex<Vec<Request>>>, answer: Arc<dyn Fn(&Request) -> Option<Response> + Send + Sync>) {
    let mut writer = conn.try_clone().unwrap();
    for line in BufReader::new(conn).lines() {
        let Ok(line) = line else { return };
        let req: Request = serde_json::from_str(&line).expect("ashd sent an invalid request");
        log.lock().unwrap().push(req.clone());
        match answer(&req) {
            Some(resp) => writeln!(writer, "{}", serde_json::to_string(&resp).unwrap()).unwrap(),
            None => return, // hang up without answering
        }
    }
}

fn status() -> ShellStatus {
    ShellStatus {
        version: "9.9.9".into(),
        theme: ThemeName::Ink,
        niri_connected: true,
        modal: Some("launcher".into()),
        surfaces: vec![SurfaceStatus { kind: "bar".into(), output: "winit".into(), width: 1920.0, height: 46.0 }],
    }
}

// ── shell commands ────────────────────────────────────────────────────────────

#[test]
fn every_shell_command_sends_its_request() {
    let env = Env::new();
    let shell = FakeShell::ok(&env);
    let cases: Vec<(Vec<&str>, Request)> = vec![
        (vec!["ping"], Request::Ping),
        (vec!["dismiss"], Request::Dismiss),
        (vec!["quit-shell"], Request::Quit),
        (vec!["lock"], Request::Lock),
        (vec!["launcher"], Request::Launcher { op: PanelOp::Toggle, mode: None }),
        (vec!["launcher", "--mode", "web", "--close"], Request::Launcher { op: PanelOp::Close, mode: Some(LauncherMode::Web) }),
        (vec!["clipboard", "--open"], Request::Clipboard { op: PanelOp::Open }),
        (vec!["notifications"], Request::Notifications { op: PanelOp::Toggle }),
        (vec!["quick-settings"], Request::QuickSettings { op: PanelOp::Toggle }),
        (vec!["calendar", "--open"], Request::Calendar { op: PanelOp::Open }),
        (vec!["session", "--close"], Request::Session { op: PanelOp::Close }),
        (vec!["cheatsheet"], Request::Cheatsheet { op: PanelOp::Toggle }),
        (vec!["media"], Request::Media { op: PanelOp::Toggle }),
        (vec!["media", "--open"], Request::Media { op: PanelOp::Open }),
        (vec!["media", "play-pause"], Request::MediaControl(MediaControl::PlayPause)),
        (vec!["media", "next"], Request::MediaControl(MediaControl::Next)),
        (vec!["media", "previous"], Request::MediaControl(MediaControl::Previous)),
        (vec!["media", "stop"], Request::MediaControl(MediaControl::Stop)),
        (vec!["theme", "pick"], Request::ThemeSwitcher { op: PanelOp::Toggle }),
        (vec!["theme", "pick", "--open"], Request::ThemeSwitcher { op: PanelOp::Open }),
        (vec!["theme", "pick", "--close"], Request::ThemeSwitcher { op: PanelOp::Close }),
        (vec!["osd", "volume", "+5"], Request::Osd(OsdRequest::Volume(VolumeChange::Step(5)))),
        (vec!["osd", "volume", "-5"], Request::Osd(OsdRequest::Volume(VolumeChange::Step(-5)))),
        (vec!["osd", "volume", "mute"], Request::Osd(OsdRequest::Volume(VolumeChange::Mute))),
        (vec!["osd", "mic-mute"], Request::Osd(OsdRequest::MicMute)),
        (vec!["osd", "brightness", "-10"], Request::Osd(OsdRequest::Brightness(-10))),
        (vec!["scratch", "hide"], Request::Scratch(ScratchOp::Hide)),
        (vec!["scratch", "show"], Request::Scratch(ScratchOp::Show)),
        (vec!["scratch", "toggle"], Request::Scratch(ScratchOp::Toggle)),
        (vec!["scratch", "open"], Request::Scratch(ScratchOp::Open)),
        (vec!["scratch", "close"], Request::Scratch(ScratchOp::Close)),
        (vec!["scratch", "hide", "--id", "7"], Request::Scratch(ScratchOp::HideWindow(7))),
        (vec!["scratch", "show", "--id", "7"], Request::Scratch(ScratchOp::ShowWindow(7))),
        (vec!["canvas", "zoom-out"], Request::Canvas(CanvasOp::ZoomOut)),
    ];
    for (i, (args, want)) in cases.iter().enumerate() {
        let run = env.run(args);
        assert_eq!(run.code(), 0, "{args:?}: {}", run.stderr());
        assert_eq!(run.stdout(), "", "{args:?} is silent on success");
        assert_eq!(shell.requests().len(), i + 1);
        assert_eq!(&shell.requests()[i], want, "{args:?}");
    }
}

#[test]
fn exit_code_2_when_the_shell_is_not_running() {
    let env = Env::new();
    for args in [&["launcher"][..], &["status"], &["ping"], &["dismiss"], &["quit-shell"], &["osd", "brightness", "+5"]] {
        let run = env.run(args);
        assert_eq!(run.code(), 2, "{args:?}");
        assert!(run.stderr().contains("ashd-shell is not running"), "{}", run.stderr());
        assert!(run.stderr().contains("shell.sock"), "names the socket: {}", run.stderr());
    }
    // A stale socket file is "not running" too.
    drop(UnixListener::bind(env.socket()).unwrap());
    assert_eq!(env.run(&["ping"]).code(), 2);
}

#[test]
fn ping_status_and_json() {
    let env = Env::new();
    let _shell = FakeShell::start(&env, |req| {
        Some(match req {
            Request::Ping => Response::Pong { version: "9.9.9".into(), protocol: 1, pid: 77 },
            Request::Status => Response::Status(status()),
            Request::Theme(ThemeRequest::Get) => Response::Theme(ThemeName::Moss),
            _ => Response::Ok,
        })
    });
    let run = env.run(&["ping"]);
    assert_eq!((run.code(), run.stdout()), (0, "ashd-shell 9.9.9 (protocol 1, pid 77)\n".into()));
    assert_eq!(env.run(&["--json", "ping"]).stdout().trim(), r#"{"pong":{"version":"9.9.9","protocol":1,"pid":77}}"#);

    let run = env.run(&["status"]);
    assert_eq!(run.code(), 0);
    let text = run.stdout();
    assert!(text.contains("ashd-shell 9.9.9") && text.contains("theme:  ink") && text.contains("niri:   connected"), "{text}");
    assert!(text.contains("modal:  launcher") && text.contains("bar") && text.contains("1920 x 46"), "{text}");

    let run = env.run(&["status", "--json"]);
    assert_eq!(run.code(), 0);
    let parsed: ShellStatus = serde_json::from_str(&run.stdout()).expect("status --json is a ShellStatus");
    assert_eq!(parsed, status());
}

#[test]
fn status_json_when_not_running() {
    let env = Env::new();
    let run = env.run(&["status", "--json"]);
    assert_eq!(run.code(), 2);
    assert_eq!(run.stdout().trim(), r#"{"running":false}"#);
}

#[test]
fn shell_errors_exit_1() {
    let env = Env::new();
    let _shell = FakeShell::start(&env, |_| Some(Response::error("no panel handles `clipboard` yet")));
    let run = env.run(&["clipboard"]);
    assert_eq!(run.code(), 1);
    assert_eq!(run.stderr().trim(), "ashd: no panel handles `clipboard` yet");
    assert_eq!(run.stdout(), "");
    let run = env.run(&["--json", "clipboard"]);
    assert_eq!(run.code(), 1);
    assert_eq!(run.stdout().trim(), r#"{"error":"no panel handles `clipboard` yet"}"#);
}

#[test]
fn a_shell_that_hangs_up_is_an_error_except_for_quit() {
    let env = Env::new();
    let _shell = FakeShell::start(&env, |_| None);
    let run = env.run(&["launcher"]);
    assert_eq!(run.code(), 1);
    assert!(run.stderr().contains("without answering"), "{}", run.stderr());
    assert_eq!(env.run(&["quit-shell"]).code(), 0, "the shell may exit before it answers");
}

#[test]
fn a_hung_shell_times_out_with_a_friendly_message() {
    let env = Env::new();
    let listener = UnixListener::bind(env.socket()).unwrap();
    let held = thread::spawn(move || {
        let conns: Vec<_> = listener.incoming().take(1).flatten().collect();
        thread::sleep(std::time::Duration::from_secs(7));
        drop(conns);
    });
    let started = std::time::Instant::now();
    let run = env.run(&["ping"]);
    assert_eq!(run.code(), 1);
    assert!(run.stderr().contains("did not answer within 5 s"), "{}", run.stderr());
    assert!(started.elapsed().as_secs() < 7);
    drop(held);
}

// ── usage errors, help, completions ───────────────────────────────────────────

#[test]
fn usage_errors_exit_1_not_2() {
    let env = Env::new();
    for args in [&[][..], &["bogus"], &["scratch", "explode"], &["osd", "volume", "loud"], &["theme", "set", "neon"]] {
        let run = env.run(args);
        assert_eq!(run.code(), 1, "{args:?}");
        assert!(!run.stderr().is_empty());
    }
    let run = env.run(&["theme", "set", "neon"]);
    assert!(run.stderr().contains("ash") && run.stderr().contains("dusk"), "lists the themes: {}", run.stderr());
}

#[test]
fn help_and_version() {
    let env = Env::new();
    let run = env.run(&["--version"]);
    assert_eq!((run.code(), run.stdout()), (0, format!("ashd {}\n", env!("CARGO_PKG_VERSION"))));
    let run = env.run(&["--help"]);
    assert_eq!(run.code(), 0);
    for word in ["launcher", "theme", "osd", "sys", "claude-rc", "completions", "lock"] {
        assert!(run.stdout().contains(word), "help mentions {word}");
    }
    assert_eq!(env.run(&["sys", "--help"]).code(), 0);
}

#[test]
fn completions() {
    let env = Env::new();
    for (shell, marker) in [("zsh", "#compdef ashd"), ("bash", "_ashd"), ("fish", "complete -c ashd")] {
        let run = env.run(&["completions", shell]);
        assert_eq!(run.code(), 0, "{shell}");
        assert!(run.stdout().contains(marker), "{shell}: {}", &run.stdout()[..run.stdout().len().min(200)]);
        assert!(run.stdout().contains("claude-rc"), "{shell} knows the subcommands");
    }
    assert_eq!(env.run(&["completions", "klingon"]).code(), 1);
}

// ── osd fallback ──────────────────────────────────────────────────────────────

fn fake_wpctl(env: &Env, exit: i32) -> PathBuf {
    let log = env.path("wpctl.log");
    env.tool("wpctl", &format!("echo \"$*\" >> '{}'\nexit {exit}", log.display()))
}

fn wpctl_calls(env: &Env) -> Vec<String> {
    fs::read_to_string(env.path("wpctl.log")).map(|s| s.lines().map(str::to_owned).collect()).unwrap_or_default()
}

#[test]
fn osd_falls_back_to_wpctl_without_a_shell() {
    let env = Env::new();
    let wpctl = fake_wpctl(&env, 0);
    for (args, want) in [
        (&["osd", "volume", "+5"][..], "set-volume -l 1.5 @DEFAULT_AUDIO_SINK@ 5%+"),
        (&["osd", "volume", "-10"], "set-volume -l 1.5 @DEFAULT_AUDIO_SINK@ 10%-"),
        (&["osd", "volume", "mute"], "set-mute @DEFAULT_AUDIO_SINK@ toggle"),
        (&["osd", "mic-mute"], "set-mute @DEFAULT_AUDIO_SOURCE@ toggle"),
    ] {
        let run = env.run_with(args, |c| {
            c.env("ASHD_WPCTL", &wpctl);
        });
        assert_eq!(run.code(), 0, "{args:?}: {}", run.stderr());
        assert_eq!(wpctl_calls(&env).last().map(String::as_str), Some(want), "{args:?}");
    }
    assert_eq!(wpctl_calls(&env).len(), 4);
    // `osd volume +0` has nothing to do.
    assert_eq!(env.run_with(&["osd", "volume", "+0"], |c| { c.env("ASHD_WPCTL", &wpctl); }).code(), 0);
    assert_eq!(wpctl_calls(&env).len(), 4);
}

#[test]
fn wpctl_is_found_in_path_and_not_used_when_the_shell_runs() {
    let env = Env::new();
    fake_wpctl(&env, 0); // on PATH, no ASHD_WPCTL
    assert_eq!(env.run(&["osd", "volume", "+3"]).code(), 0);
    assert_eq!(wpctl_calls(&env), vec!["set-volume -l 1.5 @DEFAULT_AUDIO_SINK@ 3%+"]);

    let shell = FakeShell::ok(&env);
    assert_eq!(env.run(&["osd", "volume", "+3"]).code(), 0);
    assert_eq!(wpctl_calls(&env).len(), 1, "the shell handles it, wpctl untouched");
    assert_eq!(shell.requests().len(), 1);
}

#[test]
fn osd_fallback_failures_are_reported() {
    let env = Env::new();
    // No wpctl at all.
    let run = env.run(&["osd", "volume", "+5"]);
    assert_eq!(run.code(), 1);
    assert!(run.stderr().contains("wpctl"), "{}", run.stderr());
    // wpctl fails.
    fake_wpctl(&env, 3);
    let run = env.run(&["osd", "mic-mute"]);
    assert_eq!(run.code(), 1);
    assert!(run.stderr().contains("wpctl") && run.stderr().contains("failed"), "{}", run.stderr());
}

// ── theme ─────────────────────────────────────────────────────────────────────

fn theme_files(env: &Env) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(env.state_dir().join("generated"))
        .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// File names `theme apply` writes: one per render target (kit may add targets).
fn generated() -> Vec<String> {
    let mut v: Vec<String> = ashd_tokens::render::Target::ALL.iter().map(|t| t.file_name().to_owned()).collect();
    v.sort();
    v
}

#[test]
fn theme_set_persists_and_writes_files_without_a_shell() {
    let env = Env::new();
    let run = env.run(&["theme", "set", "moss", "--no-reload"]);
    assert_eq!((run.code(), run.stdout().trim()), (0, "moss"), "{}", run.stderr());
    assert_eq!(fs::read_to_string(env.state_dir().join("theme")).unwrap(), "moss\n");
    assert_eq!(theme_files(&env), generated());
    let niri = fs::read_to_string(env.state_dir().join("generated/niri.kdl")).unwrap();
    assert!(niri.contains("#A6D1A4"), "Moss accent in niri.kdl");

    // `get` falls back to the persisted theme; --json wraps it like the shell would.
    assert_eq!(env.run(&["theme", "get"]).stdout().trim(), "moss");
    assert_eq!(env.run(&["--json", "theme", "set", "dusk", "--no-reload"]).stdout().trim(), r#"{"theme":"dusk"}"#);
    assert_eq!(env.run(&["theme", "get"]).stdout().trim(), "dusk");
}

#[test]
fn theme_set_tells_the_shell() {
    let env = Env::new();
    let shell = FakeShell::start(&env, |_| Some(Response::Theme(ThemeName::Midnight)));
    let run = env.run(&["theme", "set", "midnight", "--no-reload"]);
    assert_eq!((run.code(), run.stdout().trim()), (0, "midnight"), "{}", run.stderr());
    assert_eq!(shell.requests(), vec![Request::Theme(ThemeRequest::Set(ThemeName::Midnight))]);
}

#[test]
fn theme_set_keeps_the_files_when_the_shell_complains() {
    let env = Env::new();
    let _shell = FakeShell::start(&env, |_| Some(Response::error("busy")));
    let run = env.run(&["theme", "set", "ink", "--no-reload"]);
    assert_eq!(run.code(), 1);
    assert!(run.stderr().contains("theme saved, but ashd-shell could not switch: busy"), "{}", run.stderr());
    assert_eq!(fs::read_to_string(env.state_dir().join("theme")).unwrap(), "ink\n");
    assert_eq!(theme_files(&env), generated());
}

#[test]
fn theme_get_prefers_the_running_shell() {
    let env = Env::new();
    env.run(&["theme", "set", "ash", "--no-reload"]);
    let _shell = FakeShell::start(&env, |_| Some(Response::Theme(ThemeName::Moss)));
    assert_eq!(env.run(&["theme", "get"]).stdout().trim(), "moss");
}

#[test]
fn theme_cycle_walks_the_list() {
    let env = Env::new();
    let order: Vec<String> = ThemeName::ALL.iter().map(|t| t.label().to_owned()).collect();
    // No state yet: the default theme is the first, so cycling starts at the second.
    let mut seen = Vec::new();
    for _ in 0..ThemeName::ALL.len() {
        let run = env.run(&["theme", "cycle", "--no-reload"]);
        assert_eq!(run.code(), 0, "{}", run.stderr());
        seen.push(run.stdout().trim().to_owned());
    }
    let mut expected: Vec<String> = order[1..].to_vec();
    expected.push(order[0].clone());
    assert_eq!(seen, expected, "wraps around");
    assert_eq!(fs::read_to_string(env.state_dir().join("theme")).unwrap().trim(), order[0]);
}

#[test]
fn theme_cycle_starts_from_the_shells_theme() {
    let env = Env::new();
    let shell = FakeShell::start(&env, |req| {
        Some(match req {
            Request::Theme(ThemeRequest::Get) => Response::Theme(ThemeName::Moss),
            _ => Response::Ok,
        })
    });
    let run = env.run(&["theme", "cycle", "--no-reload"]);
    assert_eq!((run.code(), run.stdout().trim()), (0, ThemeName::Moss.next().label()));
    assert_eq!(shell.requests().last(), Some(&Request::Theme(ThemeRequest::Set(ThemeName::Moss.next()))));
}

#[test]
fn theme_apply_rewrites_the_files_of_the_persisted_theme() {
    let env = Env::new();
    env.run(&["theme", "set", "moss", "--no-reload"]);
    fs::remove_dir_all(env.state_dir().join("generated")).unwrap();
    let run = env.run(&["theme", "apply", "--no-reload"]);
    assert_eq!(run.code(), 0, "{}", run.stderr());
    let listed: Vec<String> = run.stdout().lines().map(|l| l.rsplit('/').next().unwrap().to_owned()).collect();
    assert_eq!(listed.len(), ashd_tokens::render::Target::ALL.len());
    assert!(run.stdout().lines().all(|l| l.starts_with(env.state_dir().to_str().unwrap())));
    assert_eq!(theme_files(&env), generated());

    fs::remove_dir_all(env.state_dir().join("generated")).unwrap();
    let run = env.run(&["theme", "apply", "--quiet", "--no-reload"]);
    assert_eq!((run.code(), run.stdout()), (0, String::new()));
    assert_eq!(theme_files(&env), generated());
}

// ── app quit (fake niri) ──────────────────────────────────────────────────────

fn fake_niri(env: &Env, windows: serde_json::Value) -> (PathBuf, Arc<Mutex<Vec<String>>>) {
    let path = env.path("niri.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let actions: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = actions.clone();
    thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let (log, windows) = (log.clone(), windows.clone());
            thread::spawn(move || {
                let mut w = conn.try_clone().unwrap();
                for line in BufReader::new(conn).lines().map_while(Result::ok) {
                    let req: serde_json::Value = serde_json::from_str(&line).unwrap();
                    let focused = windows.as_array().unwrap().iter().find(|x| x["is_focused"] == true).cloned();
                    let reply = if req == "FocusedWindow" {
                        serde_json::json!({"Ok": {"FocusedWindow": focused}})
                    } else if req == "Windows" {
                        serde_json::json!({"Ok": {"Windows": windows}})
                    } else if let Some(a) = req.get("Action") {
                        log.lock().unwrap().push(a.to_string());
                        serde_json::json!({"Ok": "Handled"})
                    } else {
                        serde_json::json!({"Err": format!("unexpected {req}")})
                    };
                    writeln!(w, "{reply}").unwrap();
                }
            });
        }
    });
    (path, actions)
}

fn niri_window(id: u64, app: &str, focused: bool) -> serde_json::Value {
    serde_json::json!({
        "id": id, "title": format!("w{id}"), "app_id": app, "pid": null, "workspace_id": 1,
        "is_focused": focused, "is_floating": false, "is_urgent": false,
        "layout": {"pos_in_scrolling_layout": [id, 1], "tile_size": [500.0, 900.0], "window_size": [496, 896],
                   "tile_pos_in_workspace_view": null, "window_offset_in_tile": [0.0, 0.0]},
        "focus_timestamp": null
    })
}

#[test]
fn app_quit_closes_all_windows_of_the_focused_app() {
    let env = Env::new();
    let (sock, actions) = fake_niri(
        &env,
        serde_json::json!([
            niri_window(1, "kitty", false),
            niri_window(2, "kitty", true),
            niri_window(3, "zen", false),
            niri_window(4, "kitty", false)
        ]),
    );
    let run = env.run_with(&["app", "quit"], |c| {
        c.env("NIRI_SOCKET", &sock);
    });
    assert_eq!((run.code(), run.stdout()), (0, String::new()), "{}", run.stderr());
    let closed: Vec<String> = actions.lock().unwrap().clone();
    assert_eq!(
        closed,
        vec![r#"{"CloseWindow":{"id":1}}"#, r#"{"CloseWindow":{"id":4}}"#, r#"{"CloseWindow":{"id":2}}"#]
    );
    let run = env.run_with(&["--json", "app", "quit"], |c| {
        c.env("NIRI_SOCKET", &sock);
    });
    assert_eq!(run.stdout().trim(), r#"{"closed":3}"#);
}

#[test]
fn app_quit_without_niri_is_a_friendly_error() {
    let env = Env::new();
    let run = env.run(&["app", "quit"]);
    assert_eq!(run.code(), 1);
    assert!(run.stderr().contains("NIRI_SOCKET"), "{}", run.stderr());
}

// ── lock ──────────────────────────────────────────────────────────────────────

#[test]
fn lock_goes_through_the_shell_when_it_runs() {
    let env = Env::new();
    let shell = FakeShell::ok(&env);
    let marker = env.path("lock-ran");
    let lock = env.script(&env.path("my-ashd-lock"), &format!(": > '{}'", marker.display()));
    let run = env.run_with(&["lock"], |c| {
        c.env("ASHD_LOCK_BIN", &lock);
    });
    assert_eq!(run.code(), 0);
    assert_eq!(shell.requests(), vec![Request::Lock]);
    assert!(!marker.exists(), "ashd-lock not spawned by the CLI when the shell does it");
}

#[test]
fn lock_execs_ashd_lock_without_a_shell() {
    let env = Env::new();
    // Explicit override; exit status and stdout come from ashd-lock itself (exec).
    let lock = env.script(&env.path("my-ashd-lock"), "echo locked-by-override\nexit 0");
    let run = env.run_with(&["lock"], |c| {
        c.env("ASHD_LOCK_BIN", &lock);
    });
    assert_eq!((run.code(), run.stdout().trim()), (0, "locked-by-override"));
    let failing = env.script(&env.path("failing-lock"), "exit 7");
    let run = env.run_with(&["lock"], |c| {
        c.env("ASHD_LOCK_BIN", &failing);
    });
    assert_eq!(run.code(), 7, "exec: ashd-lock's own status");
}

#[test]
fn lock_finds_ashd_lock_next_to_the_binary_or_in_path() {
    let env = Env::new();
    // A private copy of `ashd` so "next to the binary" is a directory we control.
    let bindir = env.path("install");
    fs::create_dir(&bindir).unwrap();
    let ashd = bindir.join("ashd");
    // Copy under a temporary name and rename: `ashd` never exists half-written.
    let partial = bindir.join(".ashd.partial");
    fs::copy(ASHD, &partial).unwrap();
    fs::rename(&partial, &ashd).unwrap();

    let run_copy = |env: &Env| {
        let mut c = Command::new(&ashd);
        c.arg("lock")
            .env_clear()
            .env("ASHD_SOCKET", env.socket())
            .env("PATH", env.path("bin"))
            .env("HOME", env.path("home"));
        // The fresh copy (and the fake `ashd-lock`s) may still be open for writing in a
        // concurrently forked test process: see `output_retrying`.
        Run { out: output_retrying(&mut c) }
    };

    let run = run_copy(&env);
    assert_eq!(run.code(), 1, "nothing to run");
    assert!(run.stderr().contains("ashd-lock"), "{}", run.stderr());

    env.tool("ashd-lock", "echo from-path");
    assert_eq!(run_copy(&env).stdout().trim(), "from-path");

    env.script(&bindir.join("ashd-lock"), "echo from-sibling");
    assert_eq!(run_copy(&env).stdout().trim(), "from-sibling", "the sibling wins over PATH");
}

// ── sys ───────────────────────────────────────────────────────────────────────

#[test]
fn generate_hardware_config_writes_the_file() {
    let env = Env::new();
    let gen_ok = env.script(
        &env.path("gen-ok"),
        "[ \"$1\" = --show-hardware-config ] || { echo \"bad args: $*\" >&2; exit 9; }\necho '# hardware'\necho '{ }'",
    );
    // Default flake dir = cwd, printed relative like the old script did.
    let flake = env.path("flake");
    fs::create_dir(&flake).unwrap();
    let run = env.run_with(&["sys", "generate-hardware-config", "box"], |c| {
        c.env("ASHD_NIXOS_GENERATE_CONFIG", &gen_ok).current_dir(&flake);
    });
    assert_eq!((run.code(), run.stdout().trim()), (0, "Wrote hosts/box/hardware-configuration.nix"), "{}", run.stderr());
    assert_eq!(fs::read_to_string(flake.join("hosts/box/hardware-configuration.nix")).unwrap(), "# hardware\n{ }\n");

    // --flake-dir; existing host dir and contents are replaced atomically.
    let other = env.path("other");
    fs::create_dir_all(other.join("hosts/box")).unwrap();
    fs::write(other.join("hosts/box/hardware-configuration.nix"), "old").unwrap();
    fs::write(other.join("hosts/box/default.nix"), "keep").unwrap();
    let run = env.run_with(&["sys", "generate-hardware-config", "box", "--flake-dir", other.to_str().unwrap()], |c| {
        c.env("ASHD_NIXOS_GENERATE_CONFIG", &gen_ok);
    });
    assert_eq!(run.code(), 0, "{}", run.stderr());
    assert_eq!(run.stdout().trim(), format!("Wrote {}/hosts/box/hardware-configuration.nix", other.display()));
    assert_eq!(fs::read_to_string(other.join("hosts/box/hardware-configuration.nix")).unwrap(), "# hardware\n{ }\n");
    assert_eq!(fs::read_to_string(other.join("hosts/box/default.nix")).unwrap(), "keep");
}

#[test]
fn generate_hardware_config_failures_write_nothing() {
    let env = Env::new();
    let flake = env.path("flake");
    fs::create_dir(&flake).unwrap();
    let failing = env.script(&env.path("gen-fail"), "echo 'no hardware here' >&2\nexit 4");
    let run = env.run_with(&["sys", "generate-hardware-config", "box", "--flake-dir", flake.to_str().unwrap()], |c| {
        c.env("ASHD_NIXOS_GENERATE_CONFIG", &failing);
    });
    assert_eq!(run.code(), 1);
    assert!(run.stderr().contains("no hardware here"), "{}", run.stderr());
    assert!(!flake.join("hosts/box/hardware-configuration.nix").exists());

    // Not installed.
    let run = env.run_with(&["sys", "generate-hardware-config", "box", "--flake-dir", flake.to_str().unwrap()], |_| {});
    assert_eq!(run.code(), 1);
    assert!(run.stderr().contains("nixos-generate-config"), "{}", run.stderr());

    // Hostile host names never touch the filesystem.
    let ok = env.script(&env.path("gen-ok"), "echo x");
    for host in ["../evil", "a/b", ".hidden", ""] {
        let run = env.run_with(&["sys", "generate-hardware-config", host, "--flake-dir", flake.to_str().unwrap()], |c| {
            c.env("ASHD_NIXOS_GENERATE_CONFIG", &ok);
        });
        assert_eq!(run.code(), 1, "{host:?}");
        assert!(run.stderr().contains("invalid host name"), "{host:?}: {}", run.stderr());
    }
    assert!(!env.path("evil").exists() && !flake.join("hosts").exists());
}

#[test]
fn flclashx_sync_is_idempotent_and_installs_setuid_core() {
    let env = Env::new();
    let src = env.path("store/opt/FlClashX");
    fs::create_dir_all(src.join("lib")).unwrap();
    fs::create_dir_all(src.join("data")).unwrap();
    fs::write(src.join("FlClashX"), "bin").unwrap();
    fs::write(src.join("FlClashCore"), "core").unwrap();
    let dst = env.path("var/lib/flclashx");
    let args = ["sys", "flclashx-sync", "--src", src.to_str().unwrap(), "--dst", dst.to_str().unwrap()];

    let run = env.run(&args);
    assert_eq!(run.code(), 0, "{}", run.stderr());
    assert!(run.stdout().contains("installed"), "{}", run.stdout());
    assert_eq!(fs::metadata(dst.join("FlClashCore")).unwrap().permissions().mode() & 0o7777, 0o4755);
    assert_eq!(fs::read_link(dst.join("lib")).unwrap(), src.join("lib"));
    assert_eq!(fs::read_to_string(dst.join(".source")).unwrap().trim(), src.to_str().unwrap());

    fs::write(dst.join("sentinel"), "x").unwrap();
    let run = env.run(&args);
    assert_eq!((run.code(), run.stdout(), run.stderr()), (0, String::new(), String::new()), "second run is a silent no-op");
    assert!(dst.join("sentinel").exists());

    // Missing FlClashCore in a new package: error, old install untouched.
    let broken = env.path("store2/opt/FlClashX");
    fs::create_dir_all(&broken).unwrap();
    fs::write(broken.join("FlClashX"), "x").unwrap();
    let run = env.run(&["sys", "flclashx-sync", "--src", broken.to_str().unwrap(), "--dst", dst.to_str().unwrap()]);
    assert_eq!(run.code(), 1);
    assert!(dst.join("sentinel").exists() && dst.join("FlClashCore").exists());
}

#[test]
fn fm_pick_execs_fm() {
    let env = Env::new();
    env.tool("fm", "echo \"fm $*\"\nexit 3");
    // fm follows the theme (the persisted one; Ash in a fresh state dir), like `ashd fm`.
    let cases: [(&[&str], &str); 4] = [
        (&["0", "0", "0", "/data", "/tmp/out"], "fm --theme ash --pick --out /tmp/out /data"),
        (&["1", "0", "0", "/data", "/tmp/out"], "fm --theme ash --pick --multiple --out /tmp/out /data"),
        (&["1", "1", "0", "/data", "/tmp/out"], "fm --theme ash --pick --directory --out /tmp/out /data"),
        (
            &["0", "0", "1", "", "/tmp/out"],
            &format!("fm --theme ash --pick --save --out /tmp/out {}", env.path("home").display()),
        ),
    ];
    for (args, want) in cases {
        let mut full = vec!["sys", "fm-pick"];
        full.extend(args);
        let run = env.run(&full);
        assert_eq!(run.stdout().trim(), want, "{args:?}");
        assert_eq!(run.code(), 3, "fm's exit status is ours (exec)");
        assert_eq!(run.stderr(), "", "no debug output unless asked");
    }
    let run = env.run(&["sys", "fm-pick", "0", "0", "1", "/a b", "/tmp/o", "1"]);
    assert_eq!(run.stderr().trim(), "fm --theme ash --pick --save --out /tmp/o '/a b'", "debug=1 prints the command");
    let run = env.run(&["sys", "fm-pick", "0", "0", "0", "/x", "/tmp/o", "0"]);
    assert_eq!(run.stderr(), "");
}

#[test]
fn fm_follows_the_theme_unless_unchecked() {
    let env = Env::new();
    let fm = env.script(&env.path("my-fm"), "echo \"fm $*\"\nexit 4");
    fs::write(env.state_dir().join("theme"), "moss\n").unwrap();
    let run = env.run_with(&["fm", "--", "--pick", "/x y"], |c| {
        c.env("ASHD_FM_BIN", &fm);
    });
    assert_eq!((run.code(), run.stdout().trim()), (4, "fm --theme moss --pick /x y"), "{}", run.stderr());
    // plain `ashd fm`, fm from PATH
    env.tool("fm", "echo \"path-fm $*\"");
    assert_eq!(env.run(&["fm"]).stdout().trim(), "path-fm --theme moss");
    // "apply to" without fm: no theme
    fs::write(env.state_dir().join("apply-to.json"), r#"{"fm": false}"#).unwrap();
    assert_eq!(env.run(&["fm", "--", "/tmp"]).stdout().trim(), "path-fm /tmp");
}

#[test]
fn fm_without_fm_is_an_error() {
    let env = Env::new();
    let run = env.run(&["fm"]);
    assert_eq!(run.code(), 1);
    assert!(run.stderr().contains("ASHD_FM_BIN"), "{}", run.stderr());
}

#[test]
fn fm_pick_without_fm_is_an_error() {
    let env = Env::new();
    let run = env.run(&["sys", "fm-pick", "0", "0", "0", "/x", "/tmp/o"]);
    assert_eq!(run.code(), 1);
    assert!(run.stderr().contains("fm"), "{}", run.stderr());
}

// ── claude-rc ─────────────────────────────────────────────────────────────────

/// Fake tmux: logs `--` + one arg per line; `has-session` succeeds if `$TMUX_HAS` exists.
fn fake_tmux(env: &Env) {
    env.tool(
        "tmux",
        &format!(
            "{{ echo --; for a in \"$@\"; do echo \"$a\"; done; }} >> '{}'\n\
             if [ \"$1\" = has-session ]; then [ -e '{}' ]; exit $?; fi\nexit 0",
            env.path("tmux.log").display(),
            env.path("tmux-has").display()
        ),
    );
}

fn tmux_log(env: &Env) -> Vec<Vec<String>> {
    let text = fs::read_to_string(env.path("tmux.log")).unwrap_or_default();
    text.split("--\n").filter(|s| !s.is_empty()).map(|s| s.lines().map(str::to_owned).collect()).collect()
}

#[test]
fn claude_rc_starts_a_session() {
    let env = Env::new();
    fake_tmux(&env);
    let project = env.path("home/my proj.v1");
    fs::create_dir(&project).unwrap();
    let canonical = project.canonicalize().unwrap();
    let run = env.run_with(&["claude-rc"], |c| {
        c.current_dir(&project);
    });
    assert_eq!(run.code(), 0, "{}", run.stderr());
    let lines: Vec<String> = run.stdout().lines().map(str::to_owned).collect();
    assert_eq!(
        lines,
        vec![
            "started tmux session claude-rc-my_proj_v1 (spawn=same-dir, permission-mode=acceptEdits)",
            "attach: tmux attach -t claude-rc-my_proj_v1    detach: Ctrl-b d    stop: tmux kill-session -t claude-rc-my_proj_v1",
        ]
    );
    let log = tmux_log(&env);
    assert_eq!(log.len(), 2);
    assert_eq!(log[0], ["has-session", "-t", "=claude-rc-my_proj_v1"]);
    assert_eq!(
        log[1],
        [
            "new-session",
            "-d",
            "-s",
            "claude-rc-my_proj_v1",
            "-c",
            canonical.to_str().unwrap(),
            "claude remote-control --name 'my proj.v1' --spawn same-dir --permission-mode acceptEdits; echo; \
             echo \"remote-control exited ($?)\"; exec $SHELL"
        ]
    );
}

#[test]
fn claude_rc_reuses_a_running_session() {
    let env = Env::new();
    fake_tmux(&env);
    fs::write(env.path("tmux-has"), "").unwrap();
    let project = env.path("home/ashd");
    fs::create_dir(&project).unwrap();
    let run = env.run_with(&["claude-rc"], |c| {
        c.current_dir(&project);
    });
    assert_eq!(run.code(), 0, "{}", run.stderr());
    assert_eq!(
        run.stdout(),
        "already running: tmux session claude-rc-ashd\n\
         attach: tmux attach -t claude-rc-ashd    detach: Ctrl-b d    stop: tmux kill-session -t claude-rc-ashd\n"
    );
    assert_eq!(tmux_log(&env).len(), 1, "only has-session ran");
}

#[test]
fn claude_rc_spawn_and_mode_come_from_the_environment() {
    let env = Env::new();
    fake_tmux(&env);
    let project = env.path("home/ashd");
    fs::create_dir(&project).unwrap();

    // Inside a git work tree the default spawn mode is `worktree`.
    env.tool("git", "echo true");
    let run = env.run_with(&["claude-rc"], |c| {
        c.current_dir(&project);
    });
    assert!(run.stdout().contains("(spawn=worktree, permission-mode=acceptEdits)"), "{}", run.stdout());

    // Not a work tree (git fails) → same-dir.
    env.tool("git", "exit 128");
    let run = env.run_with(&["claude-rc"], |c| {
        c.current_dir(&project);
    });
    assert!(run.stdout().contains("(spawn=same-dir,"), "{}", run.stdout());

    // Overrides; empty variables count as unset.
    let run = env.run_with(&["claude-rc"], |c| {
        c.current_dir(&project).env("CLAUDE_RC_SPAWN", "session").env("CLAUDE_RC_PERMISSION_MODE", "plan");
    });
    assert!(run.stdout().contains("(spawn=session, permission-mode=plan)"), "{}", run.stdout());
    let run = env.run_with(&["claude-rc"], |c| {
        c.current_dir(&project).env("CLAUDE_RC_SPAWN", "").env("CLAUDE_RC_PERMISSION_MODE", "");
    });
    assert!(run.stdout().contains("(spawn=same-dir, permission-mode=acceptEdits)"), "{}", run.stdout());
    let last = tmux_log(&env).pop().unwrap();
    assert!(last.last().unwrap().contains("--spawn same-dir --permission-mode acceptEdits;"));
}

#[test]
fn claude_rc_errors() {
    let env = Env::new();
    let run = env.run(&["claude-rc"]);
    assert_eq!(run.code(), 1);
    assert!(run.stderr().contains("tmux"), "{}", run.stderr());

    env.tool("tmux", "if [ \"$1\" = has-session ]; then exit 1; fi\necho 'server exited unexpectedly' >&2\nexit 1");
    let run = env.run(&["claude-rc"]);
    assert_eq!(run.code(), 1);
    assert!(run.stderr().contains("server exited unexpectedly"), "{}", run.stderr());
    assert!(!run.stdout().contains("started"), "{}", run.stdout());
}

// ── scratch hide / show (fake niri, no shell) ─────────────────────────────────

fn niri_workspace(id: u64, hidden: bool) -> serde_json::Value {
    serde_json::json!({
        // the ashd fork: hidden workspaces have idx 0
        "id": id, "idx": if hidden { 0 } else { id }, "name": if hidden { serde_json::json!("scratch") } else { serde_json::Value::Null },
        "output": "X", "is_urgent": false, "is_active": !hidden, "is_focused": id == 1,
        "active_window_id": null, "is_hidden": hidden
    })
}

fn niri_window_on(id: u64, workspace: u64, focused: bool) -> serde_json::Value {
    let mut w = niri_window(id, "kitty", focused);
    w["workspace_id"] = workspace.into();
    w
}

/// Like [`fake_niri`], and it answers `Workspaces`.
fn fake_niri_scratch(
    env: &Env,
    workspaces: serde_json::Value,
    windows: serde_json::Value,
) -> (PathBuf, Arc<Mutex<Vec<String>>>) {
    let path = env.path("niri.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let actions: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = actions.clone();
    thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let (log, windows, workspaces) = (log.clone(), windows.clone(), workspaces.clone());
            thread::spawn(move || {
                let mut w = conn.try_clone().unwrap();
                for line in BufReader::new(conn).lines().map_while(Result::ok) {
                    let req: serde_json::Value = serde_json::from_str(&line).unwrap();
                    let reply = if req == "Windows" {
                        serde_json::json!({"Ok": {"Windows": windows}})
                    } else if req == "Workspaces" {
                        serde_json::json!({"Ok": {"Workspaces": workspaces}})
                    } else if let Some(a) = req.get("Action") {
                        log.lock().unwrap().push(a.to_string());
                        serde_json::json!({"Ok": "Handled"})
                    } else {
                        serde_json::json!({"Err": format!("unexpected {req}")})
                    };
                    writeln!(w, "{reply}").unwrap();
                }
            });
        }
    });
    (path, actions)
}

#[test]
fn scratch_hide_and_show_work_without_the_shell() {
    let env = Env::new();
    let (sock, actions) = fake_niri_scratch(
        &env,
        serde_json::json!([niri_workspace(1, false), niri_workspace(9, true)]),
        serde_json::json!([niri_window_on(1, 1, true), niri_window_on(2, 1, false), niri_window_on(3, 9, false)]),
    );
    for (args, want) in [
        (&["scratch", "hide"][..], r#"{"HideWindow":{"id":1}}"#),
        (&["scratch", "hide", "--id", "2"], r#"{"HideWindow":{"id":2}}"#),
        (&["scratch", "show"], r#"{"UnhideWindow":{"id":null}}"#),
        (&["scratch", "show", "--id", "3"], r#"{"UnhideWindow":{"id":3}}"#),
    ] {
        let run = env.run_with(args, |c| {
            c.env("NIRI_SOCKET", &sock);
        });
        assert_eq!((run.code(), run.stdout()), (0, String::new()), "{args:?}: {}", run.stderr());
        assert_eq!(actions.lock().unwrap().last().map(String::as_str), Some(want), "{args:?}");
    }
    assert_eq!(actions.lock().unwrap().len(), 4);
    // Window 3 is already hidden, window 2 is not.
    for (args, why) in [(&["scratch", "hide", "--id", "3"][..], "already hidden"), (&["scratch", "show", "--id", "2"], "not hidden")] {
        let run = env.run_with(args, |c| {
            c.env("NIRI_SOCKET", &sock);
        });
        assert_eq!(run.code(), 1, "{args:?}");
        assert!(run.stderr().contains(why), "{}", run.stderr());
    }
    assert_eq!(actions.lock().unwrap().len(), 4, "failed requests send no action");
}

#[test]
fn scratch_without_a_hidden_workspace_is_a_clear_error_and_does_nothing() {
    let env = Env::new();
    let (sock, actions) =
        fake_niri_scratch(&env, serde_json::json!([niri_workspace(1, false)]), serde_json::json!([niri_window_on(1, 1, true)]));
    for op in ["hide", "show"] {
        let run = env.run_with(&["scratch", op], |c| {
            c.env("NIRI_SOCKET", &sock);
        });
        assert_eq!(run.code(), 1, "{op}");
        assert!(run.stderr().contains("no hidden workspace") && run.stderr().contains("hidden }"), "{}", run.stderr());
    }
    assert!(actions.lock().unwrap().is_empty());
}

#[test]
fn scratch_outside_niri_and_panel_ops_without_the_shell() {
    let env = Env::new();
    let run = env.run(&["scratch", "hide"]);
    assert_eq!(run.code(), 1);
    assert!(run.stderr().contains("NIRI_SOCKET"), "{}", run.stderr());
    for op in ["toggle", "open", "close"] {
        let run = env.run(&["scratch", op]);
        assert_eq!(run.code(), 2, "{op} needs the shell");
        assert!(run.stderr().contains("ashd-shell is not running"), "{}", run.stderr());
    }
}
