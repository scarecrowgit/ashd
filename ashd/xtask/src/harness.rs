//! A nested session with `ashd-shell` running in it: build the binaries, start the shell,
//! talk to it through the `ashd` CLI (so the CLI is exercised too), take screenshots.

use std::{
    fs::{self, File},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use ashd_ipc::{Response, ShellStatus};

use crate::{
    nested::{Session, StartOpts},
    shot::{self, Via},
    util::{self, log_tail, run_captured, wait_until},
};

pub struct Bins {
    pub ashd: PathBuf,
    pub shell: PathBuf,
}

fn release_profile() -> bool {
    std::env::current_exe().ok().is_some_and(|p| p.components().any(|c| c.as_os_str() == "release"))
}

/// `cargo build -p ashd-shell -p ashd-cli` (same profile as xtask itself).
pub fn build_bins() -> Result<()> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.current_dir(util::workspace_root()).args(["build", "-p", "ashd-shell", "-p", "ashd-cli"]);
    if release_profile() {
        cmd.arg("--release");
    }
    eprintln!("xtask: building ashd-shell and ashd …");
    let st = cmd.status().context("running cargo build")?;
    if !st.success() {
        bail!("cargo build -p ashd-shell -p ashd-cli failed ({st})");
    }
    Ok(())
}

pub fn locate_bins() -> Result<Bins> {
    let dir = match std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        Some(d) if d.join("ashd-shell").exists() || d.join("ashd").exists() => d,
        _ => {
            let t = std::env::var_os("CARGO_TARGET_DIR").context("cannot locate the target dir")?;
            PathBuf::from(t).join(if release_profile() { "release" } else { "debug" })
        }
    };
    let (ashd, shell) = (dir.join("ashd"), dir.join("ashd-shell"));
    for p in [&ashd, &shell] {
        if !p.exists() {
            bail!("{} does not exist (build first)", p.display());
        }
    }
    Ok(Bins { ashd, shell })
}

pub struct AshdOut {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl AshdOut {
    /// The `--json` response, if stdout holds one.
    pub fn response(&self) -> Option<Response> {
        let text = self.stdout.trim();
        serde_json::from_str::<Response>(text)
            .ok()
            // `ashd --json status` prints the bare status object.
            .or_else(|| serde_json::from_str::<ShellStatus>(text).ok().map(Response::Status))
    }
}

pub struct Harness {
    pub session: Session,
    pub bins: Bins,
    pub via: Via,
    shell: Option<Child>,
    pub shell_log: PathBuf,
    shell_exit: Option<std::process::ExitStatus>,
}

impl Harness {
    /// Start a session (private runtime dir, optionally a private bus).
    pub fn new(opts: &StartOpts, via: Via) -> Result<Harness> {
        let bins = locate_bins()?;
        let session = Session::start(opts)?;
        let shell_log = session.root.join("ashd-shell.log");
        Ok(Harness { session, bins, via, shell: None, shell_log, shell_exit: None })
    }

    /// `ashd <args>` inside the session.
    pub fn ashd(&self, args: &[&str]) -> Result<AshdOut> {
        let mut cmd = self.session.command(&self.bins.ashd);
        cmd.arg("--json").args(args);
        let c = run_captured(cmd, Duration::from_secs(15))?;
        Ok(AshdOut { code: c.code, stdout: c.stdout, stderr: c.stderr, timed_out: c.timed_out })
    }

    pub fn start_shell(&mut self, extra: &[&str]) -> Result<()> {
        let log = File::create(&self.shell_log)?;
        let mut cmd = self.session.command(&self.bins.shell);
        cmd.args(extra)
            .env("RUST_BACKTRACE", "1")
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .process_group(0);
        // SAFETY: only async-signal-safe prctl between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong);
                Ok(())
            });
        }
        self.shell = Some(cmd.spawn().context("spawning ashd-shell")?);
        Ok(())
    }

    /// Is the shell process still running?
    pub fn shell_alive(&mut self) -> bool {
        let Some(c) = self.shell.as_mut() else { return false };
        match c.try_wait() {
            Ok(None) => true,
            Ok(Some(st)) => {
                self.shell_exit = Some(st);
                false
            }
            Err(_) => false,
        }
    }

    /// A variable of the running shell's environment (`/proc/<pid>/environ`).
    pub fn shell_env(&self, var: &str) -> Option<String> {
        let pid = self.shell.as_ref()?.id();
        let env = fs::read(format!("/proc/{pid}/environ")).ok()?;
        let prefix = format!("{var}=");
        env.split(|b| *b == 0)
            .find_map(|e| String::from_utf8_lossy(e).strip_prefix(&prefix).map(str::to_string))
    }

    pub fn shell_exit(&self) -> Option<std::process::ExitStatus> {
        self.shell_exit
    }

    /// Wait until `ashd ping` answers.
    pub fn wait_ping(&mut self, timeout: Duration) -> Result<()> {
        let end = std::time::Instant::now() + timeout;
        loop {
            if !self.shell_alive() {
                bail!(
                    "ashd-shell exited before answering ({:?})\n{}",
                    self.shell_exit,
                    log_tail(&self.shell_log, 25)
                );
            }
            if let Ok(o) = self.ashd(&["ping"])
                && o.code == Some(0)
            {
                return Ok(());
            }
            if std::time::Instant::now() >= end {
                bail!("ashd ping did not succeed within {timeout:?}\n{}", log_tail(&self.shell_log, 25));
            }
            util::sleep_ms(150);
        }
    }

    pub fn status(&self) -> Result<ShellStatus> {
        let o = self.ashd(&["status"])?;
        match o.response() {
            Some(Response::Status(s)) => Ok(s),
            other => bail!("ashd status: unexpected answer {other:?} (exit {:?}, stderr {})", o.code, o.stderr.trim()),
        }
    }

    /// Lines of the shell log that report a panic.
    pub fn panics(&self) -> Vec<String> {
        fs::read_to_string(&self.shell_log)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains("panicked at") || l.contains("PANIC"))
            .map(str::to_string)
            .collect()
    }

    pub fn screenshot(&self, out: &Path) -> Result<()> {
        shot::capture(&self.session, out, self.via)
    }

    pub fn screenshot_settled(&self, out: &Path) -> Result<image::RgbaImage> {
        shot::capture_settled(&self.session, out, self.via, Duration::from_secs(3))
    }

    /// Ask the shell to quit; kill it if it does not.
    pub fn stop_shell(&mut self) {
        if self.shell.is_none() {
            return;
        }
        let _ = self.ashd(&["quit-shell"]);
        let gone = wait_until(Duration::from_secs(5), Duration::from_millis(50), || (!self.shell_alive()).then_some(()));
        if gone.is_none()
            && let Some(c) = self.shell.as_mut()
        {
            let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(c.id() as i32), nix::sys::signal::Signal::SIGKILL);
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    /// Copy the shell log into target-notes (after a failure).
    pub fn save_log(&self, name: &str) {
        let _ = fs::create_dir_all(util::notes_dir());
        let _ = fs::copy(&self.shell_log, util::notes_dir().join(name));
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.stop_shell();
        // Session::drop kills everything else and removes the root.
    }
}
