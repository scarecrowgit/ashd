//! `ashd claude-rc`: a detached tmux session running `claude remote-control` for the
//! current directory, so it survives closing the terminal (was the bash script `claude-rc`
//! in modules/base/tmux.nix).

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context as _, bail};

pub const SPAWN_ENV: &str = "CLAUDE_RC_SPAWN";
pub const MODE_ENV: &str = "CLAUDE_RC_PERMISSION_MODE";
pub const DEFAULT_MODE: &str = "acceptEdits";

/// Last path component of `dir` (`root` for `/`).
pub fn base_name(dir: &Path) -> String {
    dir.file_name().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty()).unwrap_or_else(|| "root".into())
}

/// `claude-rc-<base>` with every character outside `[A-Za-z0-9_-]` replaced by `_`
/// (tmux forbids `.` and `:` in session names, and we use it unquoted in messages).
pub fn session_name(base: &str) -> String {
    let clean: String =
        base.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
    format!("claude-rc-{clean}")
}

/// `$VAR` unless unset or empty (bash `${VAR:-default}`).
fn env_or(var: &str, default: &str) -> String {
    std::env::var(var).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_owned())
}

/// Single-quote `s` for `sh` (`'` becomes `'\''`).
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Quote only when needed, so ordinary values appear verbatim.
fn sh_word(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':' | '/' | '=' | '+')) {
        s.to_owned()
    } else {
        sh_quote(s)
    }
}

/// The command line tmux runs in the new session. `$?` and `$SHELL` are left for the
/// session's own shell to expand after `claude` exits.
pub fn session_command(base: &str, spawn: &str, mode: &str) -> String {
    format!(
        "claude remote-control --name {} --spawn {} --permission-mode {}; echo; echo \"remote-control exited ($?)\"; exec $SHELL",
        sh_quote(base),
        sh_word(spawn),
        sh_word(mode),
    )
}

pub fn attach_hint(name: &str) -> String {
    format!("attach: tmux attach -t {name}    detach: Ctrl-b d    stop: tmux kill-session -t {name}")
}

fn tmux_ok(args: &[&str]) -> anyhow::Result<bool> {
    match Command::new("tmux").args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status() {
        Ok(s) => Ok(s.success()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!("tmux not found in PATH"),
        Err(e) => Err(e).context("running tmux"),
    }
}

fn in_git_work_tree(dir: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--is-inside-work-tree"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
}

/// Run `ashd claude-rc` in `cwd`; returns the lines to print.
pub fn run(cwd: &Path) -> anyhow::Result<Vec<String>> {
    let dir: PathBuf = cwd.canonicalize().with_context(|| format!("resolving {}", cwd.display()))?;
    let base = base_name(&dir);
    let name = session_name(&base);
    let spawn = match std::env::var(SPAWN_ENV).ok().filter(|v| !v.is_empty()) {
        Some(v) => v,
        None if in_git_work_tree(&dir) => "worktree".into(),
        None => "same-dir".into(),
    };
    let mode = env_or(MODE_ENV, DEFAULT_MODE);

    let mut lines = Vec::new();
    if tmux_ok(&["has-session", "-t", &format!("={name}")])? {
        lines.push(format!("already running: tmux session {name}"));
    } else {
        let command = session_command(&base, &spawn, &mode);
        let dir_arg = dir.to_string_lossy();
        let args = ["new-session", "-d", "-s", name.as_str(), "-c", dir_arg.as_ref(), command.as_str()];
        let out = Command::new("tmux").args(args).stdin(Stdio::null()).output().context("running tmux")?;
        if !out.status.success() {
            bail!("tmux new-session failed ({}): {}", out.status, String::from_utf8_lossy(&out.stderr).trim());
        }
        lines.push(format!("started tmux session {name} (spawn={spawn}, permission-mode={mode})"));
    }
    lines.push(attach_hint(&name));
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(base_name(Path::new("/home/u/projects/ashd")), "ashd");
        assert_eq!(base_name(Path::new("/")), "root");
        assert_eq!(session_name("ashd"), "claude-rc-ashd");
        assert_eq!(session_name("my project.v2:x"), "claude-rc-my_project_v2_x");
        assert_eq!(session_name("keep_this-and-1"), "claude-rc-keep_this-and-1");
        assert_eq!(session_name("héllo"), "claude-rc-h_llo");
        assert_eq!(session_name(""), "claude-rc-");
    }

    #[test]
    fn quoting() {
        assert_eq!(sh_quote("ashd"), "'ashd'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(sh_word("acceptEdits"), "acceptEdits");
        assert_eq!(sh_word("same-dir"), "same-dir");
        assert_eq!(sh_word("a b"), "'a b'");
        assert_eq!(sh_word("x;rm -rf ~"), "'x;rm -rf ~'");
        assert_eq!(sh_word(""), "''");
    }

    #[test]
    fn session_command_line() {
        assert_eq!(
            session_command("ashd", "worktree", "acceptEdits"),
            "claude remote-control --name 'ashd' --spawn worktree --permission-mode acceptEdits; echo; \
             echo \"remote-control exited ($?)\"; exec $SHELL"
        );
        assert_eq!(
            session_command("o'brien", "same-dir", "plan"),
            "claude remote-control --name 'o'\\''brien' --spawn same-dir --permission-mode plan; echo; \
             echo \"remote-control exited ($?)\"; exec $SHELL"
        );
    }

    #[test]
    fn hint() {
        assert_eq!(
            attach_hint("claude-rc-x"),
            "attach: tmux attach -t claude-rc-x    detach: Ctrl-b d    stop: tmux kill-session -t claude-rc-x"
        );
    }
}
