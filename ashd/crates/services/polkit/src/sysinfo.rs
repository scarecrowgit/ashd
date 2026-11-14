//! Local lookups for the dialog: user names (NSS via `getpwuid`) and the requesting
//! processes (`/proc/<pid>`). Run on the service thread, never on the UI thread.

use std::path::Path;

use crate::{Identity, ProcessInfo, ProcessRole};

/// `uid` → login name and display name (GECOS full name, else the login name).
pub fn identity(uid: u32) -> Identity {
    match nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid)) {
        Ok(Some(u)) => {
            let gecos = u.gecos.to_string_lossy();
            let full = gecos.split(',').next().unwrap_or("").trim().to_string();
            let display_name = if full.is_empty() { u.name.clone() } else { full };
            Identity { uid, name: u.name, display_name }
        }
        _ => Identity { uid, name: format!("uid {uid}"), display_name: format!("uid {uid}") },
    }
}

/// The uid of this process (the user the shell runs as).
pub fn current_uid() -> u32 {
    nix::unistd::getuid().as_raw()
}

/// Requesting processes named in polkit's details (`polkit.caller-pid`,
/// `polkit.subject-pid`), resolved under `proc_root` (`/proc`).
pub fn processes(details: &std::collections::BTreeMap<String, String>, proc_root: &Path) -> Vec<ProcessInfo> {
    let mut out = Vec::new();
    for (key, role) in [("polkit.subject-pid", ProcessRole::Subject), ("polkit.caller-pid", ProcessRole::Caller)] {
        let Some(pid) = details.get(key).and_then(|v| v.trim().parse::<u32>().ok()) else { continue };
        if pid == 0 {
            continue;
        }
        out.push(process(pid, role, proc_root));
    }
    out
}

fn process(pid: u32, role: ProcessRole, proc_root: &Path) -> ProcessInfo {
    let dir = proc_root.join(pid.to_string());
    let exe = std::fs::read_link(dir.join("exe")).ok().map(|p| p.to_string_lossy().into_owned());
    let comm = std::fs::read_to_string(dir.join("comm")).ok().map(|s| s.trim_end().to_string()).filter(|s| !s.is_empty());
    let cmdline = std::fs::read(dir.join("cmdline")).ok().and_then(|b| {
        let parts: Vec<String> = b
            .split(|&c| c == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect();
        let s = parts.join(" ");
        (!s.is_empty()).then(|| truncate(&s, 240))
    });
    ProcessInfo { role, pid, exe, comm, cmdline }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max - 1).collect();
    t.push('…');
    t
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn resolves_pids_from_a_proc_tree() {
        let root = tempfile::tempdir().unwrap();
        let p = root.path().join("4242");
        std::fs::create_dir(&p).unwrap();
        std::os::unix::fs::symlink("/run/wrappers/bin/pkexec", p.join("exe")).unwrap();
        std::fs::write(p.join("comm"), "pkexec\n").unwrap();
        std::fs::write(p.join("cmdline"), b"pkexec\0nixos-rebuild\0switch\0").unwrap();
        let details = BTreeMap::from([
            ("polkit.subject-pid".to_string(), "4242".to_string()),
            ("polkit.caller-pid".to_string(), "17".to_string()),
            ("polkit.message".to_string(), "x".to_string()),
        ]);
        let ps = processes(&details, root.path());
        assert_eq!(ps.len(), 2);
        assert_eq!(ps[0].role, ProcessRole::Subject);
        assert_eq!(ps[0].exe.as_deref(), Some("/run/wrappers/bin/pkexec"));
        assert_eq!(ps[0].comm.as_deref(), Some("pkexec"));
        assert_eq!(ps[0].cmdline.as_deref(), Some("pkexec nixos-rebuild switch"));
        assert_eq!(ps[0].program(), "/run/wrappers/bin/pkexec");
        // a vanished process keeps its pid
        assert_eq!((ps[1].pid, ps[1].exe.as_ref(), ps[1].program()), (17, None, "pid 17".to_string()));
    }

    #[test]
    fn this_user_resolves() {
        let me = identity(current_uid());
        assert!(!me.name.is_empty());
        assert_eq!(identity(u32::MAX - 7).name, format!("uid {}", u32::MAX - 7));
        assert_eq!(truncate("abcdef", 4), "abc…");
    }
}
