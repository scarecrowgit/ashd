//! Small shared helpers: paths, tool resolution, polling, process liveness.

use std::{
    collections::BTreeMap,
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};

/// `ashd/` (the cargo workspace root).
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask lives in ashd/xtask").to_path_buf()
}

/// `ashd/target-notes/` — screenshots and logs of verification runs.
pub fn notes_dir() -> PathBuf {
    workspace_root().join("target-notes")
}

fn is_executable(p: &Path) -> bool {
    fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

pub fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path).map(|d| d.join(name)).find(|p| is_executable(p))
}

/// Resolve an external tool: `$ENV_VAR`, then `$PATH`, then `nix-build '<nixpkgs>' -A attr`
/// (how the nested session finds sway).
pub fn resolve_tool(env_var: &str, bin: &str, nix_attr: &str) -> Result<PathBuf> {
    if let Some(v) = env::var_os(env_var) {
        let p = PathBuf::from(v);
        if is_executable(&p) {
            return Ok(p);
        }
        bail!("${env_var}={} is not an executable", p.display());
    }
    if let Some(p) = find_in_path(bin) {
        return Ok(p);
    }
    let out = Command::new("nix-build")
        .args(["<nixpkgs>", "-A", nix_attr, "--no-out-link"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .with_context(|| format!("`{bin}` is not in PATH and nix-build could not be run"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let store = stdout.lines().last().unwrap_or("").trim();
    let p = Path::new(store).join("bin").join(bin);
    if out.status.success() && is_executable(&p) {
        return Ok(p);
    }
    bail!("cannot find `{bin}` (not in PATH, `nix-build '<nixpkgs>' -A {nix_attr}` failed); set ${env_var}")
}

/// Poll `f` every `interval` until it returns `Some` or `timeout` passes.
pub fn wait_until<T>(timeout: Duration, interval: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let end = Instant::now() + timeout;
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if Instant::now() >= end {
            return None;
        }
        thread::sleep(interval);
    }
}

pub fn sleep_ms(ms: u64) {
    thread::sleep(Duration::from_millis(ms));
}

/// The process exists and is not a zombie.
pub fn pid_alive(pid: i32) -> bool {
    let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else { return false };
    stat.rsplit_once(')')
        .and_then(|(_, rest)| rest.trim_start().chars().next())
        .is_some_and(|state| state != 'Z' && state != 'X')
}

/// `/proc/<pid>/cmdline` (NULs as spaces).
pub fn cmdline(pid: i32) -> String {
    fs::read(format!("/proc/{pid}/cmdline"))
        .map(|b| String::from_utf8_lossy(&b).replace('\0', " "))
        .unwrap_or_default()
}

pub fn pgid_of(pid: i32) -> Option<i32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.rsplit_once(')')?.1;
    // fields after comm: state ppid pgrp ...
    rest.split_whitespace().nth(2)?.parse().ok()
}

/// Every live process of ours whose environment has `XDG_RUNTIME_DIR=/tmp/ashd-nested.*`,
/// grouped by that directory. This finds *everything* started inside a session, whatever
/// process group it ended up in.
pub fn scan_session_pids() -> BTreeMap<PathBuf, Vec<i32>> {
    const NEEDLE: &[u8] = b"XDG_RUNTIME_DIR=/tmp/ashd-nested.";
    let mut out: BTreeMap<PathBuf, Vec<i32>> = BTreeMap::new();
    let me = std::process::id() as i32;
    let Ok(rd) = fs::read_dir("/proc") else { return out };
    for e in rd.flatten() {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<i32>() else { continue };
        if pid == me {
            continue;
        }
        let Ok(env) = fs::read(e.path().join("environ")) else { continue };
        for var in env.split(|b| *b == 0) {
            if var.starts_with(NEEDLE) {
                let root = PathBuf::from(String::from_utf8_lossy(&var[b"XDG_RUNTIME_DIR=".len()..]).into_owned());
                if pid_alive(pid) {
                    out.entry(root).or_default().push(pid);
                }
                break;
            }
        }
    }
    out
}

/// Last `n` lines of a log file (for error messages).
pub fn log_tail(path: &Path, n: usize) -> String {
    let text = fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// Output of a finished command.
pub struct Captured {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// Run `cmd` with captured output and a timeout (the child is killed on timeout).
pub fn run_captured(mut cmd: Command, timeout: Duration) -> Result<Captured> {
    use std::{io::Read, os::unix::process::CommandExt, process::Child};
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0);
    let mut child: Child = cmd.spawn().with_context(|| format!("spawning {:?}", cmd.get_program()))?;
    let mut so = child.stdout.take().expect("piped");
    let mut se = child.stderr.take().expect("piped");
    let t_out = thread::spawn(move || {
        let mut s = String::new();
        let _ = so.read_to_string(&mut s);
        s
    });
    let t_err = thread::spawn(move || {
        let mut s = String::new();
        let _ = se.read_to_string(&mut s);
        s
    });
    let end = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        if Instant::now() >= end {
            timed_out = true;
            let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(child.id() as i32), nix::sys::signal::Signal::SIGKILL);
            let _ = child.kill();
            break child.wait()?;
        }
        thread::sleep(Duration::from_millis(10));
    };
    Ok(Captured {
        code: status.code(),
        stdout: t_out.join().unwrap_or_default(),
        stderr: t_err.join().unwrap_or_default(),
        timed_out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_helpers() {
        let me = std::process::id() as i32;
        assert!(pid_alive(me));
        assert!(!pid_alive(i32::MAX));
        assert!(pgid_of(me).is_some());
        assert!(!cmdline(me).is_empty());
    }

    #[test]
    fn captured_timeout_and_output() {
        let mut c = Command::new("sh");
        c.args(["-c", "echo hi; echo err >&2"]);
        let r = run_captured(c, Duration::from_secs(5)).unwrap();
        assert_eq!(r.code, Some(0));
        assert_eq!(r.stdout.trim(), "hi");
        assert_eq!(r.stderr.trim(), "err");
        let mut c = Command::new("sleep");
        c.arg("30");
        let r = run_captured(c, Duration::from_millis(100)).unwrap();
        assert!(r.timed_out);
    }

    #[test]
    fn wait_until_works() {
        let mut n = 0;
        let r = wait_until(Duration::from_secs(1), Duration::from_millis(1), || {
            n += 1;
            (n == 3).then_some(n)
        });
        assert_eq!(r, Some(3));
        assert_eq!(wait_until(Duration::from_millis(5), Duration::from_millis(1), || None::<()>), None);
    }
}
