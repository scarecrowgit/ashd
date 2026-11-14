//! The recorder subprocess: found on `PATH`, spawned and supervised from its own thread
//! (not through niri: the child handle is needed to stop it). The child inherits the
//! shell's environment — in particular the shell's own `WAYLAND_DISPLAY`, which in the
//! nested test session is the nested one — minus [`RECORDER_ENV_REMOVE`].
//!
//! Stopping: SIGINT, so the recorder finalizes the file (mp4 needs its trailer); SIGKILL if
//! it has not exited [`KILL_AFTER`] later. If the shell itself goes away the child gets
//! SIGINT too (`PR_SET_PDEATHSIG`; it is spawned from the supervisor thread, which lives
//! exactly as long as the child does).

use std::{
    io::Read as _,
    os::unix::process::CommandExt as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use async_channel::Receiver;
use rustix::process::{Pid, Signal, kill_process};

use crate::capture::{RECORDER_ENV_REMOVE, RecTarget, RecorderKind};

/// SIGKILL this long after SIGINT.
pub const KILL_AFTER: Duration = Duration::from_secs(6);
/// How often the supervisor looks at the child.
const POLL: Duration = Duration::from_millis(40);
/// How much of the recorder's stderr is kept for error messages.
const STDERR_KEEP: usize = 1200;

/// The first known recorder on `PATH` (stats a few directories: not for the main thread).
pub fn detect() -> Option<(RecorderKind, PathBuf)> {
    RecorderKind::PREFERRED.into_iter().find_map(|k| which::which(k.binary()).ok().map(|p| (k, p)))
}

/// The environment a recorder gets: `vars` without [`RECORDER_ENV_REMOVE`].
pub fn recorder_env(vars: impl IntoIterator<Item = (String, String)>) -> Vec<(String, String)> {
    vars.into_iter().filter(|(k, _)| !RECORDER_ENV_REMOVE.contains(&k.as_str())).collect()
}

/// What a finished recorder left behind.
#[derive(Clone, Debug)]
pub struct Exit {
    /// `stop()` was called (the exit was asked for).
    pub requested: bool,
    pub success: bool,
    pub code: Option<i32>,
    /// Last lines the recorder printed to stderr.
    pub stderr: String,
    /// Size of the output file (0 when it does not exist).
    pub bytes: u64,
    pub file: PathBuf,
}

impl Exit {
    /// The file is there and not empty.
    pub fn has_video(&self) -> bool {
        self.bytes > 0
    }
}

/// A running recorder.
pub struct Recorder {
    stop: Arc<AtomicBool>,
    pub kind: RecorderKind,
}

impl Recorder {
    /// Ask the recorder to finish (idempotent): SIGINT now, SIGKILL after [`KILL_AFTER`].
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Start `program` for `target` writing `file`. Returns the handle and a channel that
/// delivers the [`Exit`] once the process is gone. `Err`: the process could not be spawned.
pub fn spawn(
    kind: RecorderKind,
    program: &Path,
    target: &RecTarget,
    file: &Path,
    env: Vec<(String, String)>,
) -> Result<(Recorder, Receiver<Exit>), String> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    let args = kind.args(target, file);
    let (tx, rx) = async_channel::bounded(1);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let (program, file_owned) = (program.to_path_buf(), file.to_path_buf());
    std::thread::Builder::new()
        .name("ashd-screen-recorder".into())
        .spawn(move || {
            let mut cmd = Command::new(&program);
            cmd.args(&args).env_clear().envs(env).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
            cmd.process_group(0);
            // SAFETY: the closure only calls `prctl`, `sigemptyset`, `sigprocmask` and `signal`,
            // all async-signal-safe.
            unsafe {
                cmd.pre_exec(|| {
                    // The shell blocks SIGINT / SIGTERM / SIGHUP process-wide (one thread
                    // receives them) and Rust ignores SIGPIPE; the signal mask and the
                    // ignored set survive exec, so a recorder started as is would never see
                    // our SIGINT. Start it with a clean slate.
                    let mut set: libc::sigset_t = std::mem::zeroed();
                    libc::sigemptyset(&mut set);
                    libc::sigprocmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
                    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGPIPE] {
                        libc::signal(sig, libc::SIG_DFL);
                    }
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGINT);
                    Ok(())
                });
            }
            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("starting {}: {e}", program.display())));
                    return;
                }
            };
            let _ = ready_tx.send(Ok(()));
            let tail = Arc::new(Mutex::new(String::new()));
            let reader = child.stderr.take().map(|mut err| {
                let tail = tail.clone();
                std::thread::spawn(move || {
                    let mut buf = [0u8; 512];
                    while let Ok(n) = err.read(&mut buf) {
                        if n == 0 {
                            break;
                        }
                        let mut t = tail.lock().unwrap_or_else(|p| p.into_inner());
                        t.push_str(&String::from_utf8_lossy(&buf[..n]));
                        if t.len() > STDERR_KEEP {
                            let cut = t.len() - STDERR_KEEP;
                            let cut = (cut..t.len()).find(|i| t.is_char_boundary(*i)).unwrap_or(t.len());
                            t.drain(..cut);
                        }
                    }
                })
            });
            let pid = Pid::from_raw(child.id() as i32);
            let mut interrupted: Option<Instant> = None;
            let status = loop {
                match child.try_wait() {
                    Ok(Some(status)) => break Some(status),
                    Ok(None) => {}
                    Err(_) => break None,
                }
                if thread_stop.load(Ordering::SeqCst) {
                    match interrupted {
                        None => {
                            interrupted = Some(Instant::now());
                            if let Some(pid) = pid {
                                let _ = kill_process(pid, Signal::INT);
                            }
                        }
                        Some(at) if at.elapsed() > KILL_AFTER => {
                            let _ = child.kill();
                        }
                        Some(_) => {}
                    }
                }
                std::thread::sleep(POLL);
            };
            if let Some(r) = reader {
                let _ = r.join();
            }
            let bytes = std::fs::metadata(&file_owned).map(|m| m.len()).unwrap_or(0);
            let stderr = tail.lock().unwrap_or_else(|p| p.into_inner()).trim().to_string();
            let _ = tx.send_blocking(Exit {
                requested: thread_stop.load(Ordering::SeqCst),
                success: status.is_some_and(|s| s.success()),
                code: status.and_then(|s| s.code()),
                stderr,
                bytes,
                file: file_owned,
            });
        })
        .map_err(|e| format!("starting the recorder thread: {e}"))?;
    ready_rx.recv().map_err(|_| "the recorder thread died".to_string())??;
    Ok((Recorder { stop, kind }, rx))
}

/// The last meaningful line of the recorder's stderr, for a one-line error.
pub fn error_line(stderr: &str, code: Option<i32>) -> String {
    match stderr.lines().map(str::trim).rfind(|l| !l.is_empty()) {
        Some(l) => l.chars().take(160).collect(),
        None => match code {
            Some(c) => format!("the recorder exited with status {c}"),
            None => "the recorder was killed".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn the_environment_keeps_the_shells_display_and_drops_the_rest() {
        let vars = [
            ("WAYLAND_DISPLAY", "wayland-nested"),
            ("XDG_RUNTIME_DIR", "/tmp/ashd-nested.x"),
            ("PATH", "/bin"),
            ("LD_LIBRARY_PATH", "/dev/shell"),
            ("DISPLAY", ":0"),
            ("WAYLAND_SOCKET", "5"),
            ("LD_PRELOAD", "x.so"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()));
        let kept = recorder_env(vars);
        let keys: Vec<_> = kept.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["WAYLAND_DISPLAY", "XDG_RUNTIME_DIR", "PATH"]);
        assert_eq!(kept[0].1, "wayland-nested");
    }

    #[test]
    fn error_lines() {
        assert_eq!(error_line("a\n\nfailed to bind wlr-screencopy\n", Some(1)), "failed to bind wlr-screencopy");
        assert_eq!(error_line("", Some(3)), "the recorder exited with status 3");
        assert_eq!(error_line("  ", None), "the recorder was killed");
    }

    /// A stand-in recorder: `sh` that traps SIGINT like `wf-recorder`, writes the file it was
    /// told about and exits 0.
    #[test]
    fn stopping_sends_sigint_and_waits_for_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-recorder");
        std::fs::write(
            &script,
            "#!/bin/sh\ntrap 'echo done > \"$5\"; exit 0' INT\ntouch \"$5.ready\"\nwhile :; do sleep 0.05; done\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let file = dir.path().join("out/rec.mp4");
        // The shell blocks SIGINT process-wide; the recorder must not inherit that.
        // SAFETY: plain libc signal-mask calls on this test's own thread.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGINT);
            libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        }
        // `wf-recorder -D -r 60 -o <output> -f <file>`: "$5" is the output name here, so point the
        // fake at the file through the target name.
        let target = RecTarget::Output(file.to_string_lossy().into_owned());
        let (rec, rx) = spawn(
            RecorderKind::WfRecorder,
            &script,
            &target,
            &file,
            vec![("PATH".into(), std::env::var("PATH").unwrap_or_default())],
        )
        .unwrap();
        // wait until the script has installed its trap (a loaded machine can be slow)
        let ready = dir.path().join("out/rec.mp4.ready");
        for _ in 0..200 {
            if ready.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(ready.exists(), "the stand-in recorder never started");
        assert!(rx.try_recv().is_err(), "still running");
        rec.stop();
        let exit = rx.recv_blocking().unwrap();
        assert!(exit.requested && exit.success, "{exit:?}");
        assert!(exit.has_video());
        assert_eq!(std::fs::read_to_string(&file).unwrap().trim(), "done");
    }

    #[test]
    fn a_recorder_that_dies_on_its_own_reports_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("failing");
        std::fs::write(&script, "#!/bin/sh\necho 'compositor does not support wlr-screencopy' >&2\nexit 1\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let file = dir.path().join("rec.mp4");
        let (_rec, rx) = spawn(RecorderKind::WfRecorder, &script, &RecTarget::Output("X".into()), &file, Vec::new()).unwrap();
        let exit = rx.recv_blocking().unwrap();
        assert!(!exit.requested && !exit.success && !exit.has_video());
        assert_eq!(error_line(&exit.stderr, exit.code), "compositor does not support wlr-screencopy");
    }

    #[test]
    fn spawning_a_missing_binary_is_an_error() {
        let r = spawn(
            RecorderKind::WfRecorder,
            Path::new("/nonexistent/wf-recorder"),
            &RecTarget::Output("X".into()),
            Path::new("/tmp/never.mp4"),
            Vec::new(),
        );
        assert!(r.is_err());
    }
}
