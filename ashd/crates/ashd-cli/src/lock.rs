//! `ashd lock` without a running shell: exec `ashd-lock`.

use std::{
    ffi::OsStr,
    os::unix::{fs::PermissionsExt, process::CommandExt as _},
    path::{Path, PathBuf},
    process::Command,
};

/// Env var naming the lock binary (tests, unusual installs).
pub const LOCK_BIN_ENV: &str = "ASHD_LOCK_BIN";

fn is_executable(p: &Path) -> bool {
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// `name` in `exe_dir` (next to the `ashd` binary), else in one of the `path_var` dirs.
pub fn find_binary(name: &str, exe_dir: Option<&Path>, path_var: Option<&OsStr>) -> Option<PathBuf> {
    if let Some(dir) = exe_dir {
        let p = dir.join(name);
        if is_executable(&p) {
            return Some(p);
        }
    }
    std::env::split_paths(path_var?).filter(|d| !d.as_os_str().is_empty()).map(|d| d.join(name)).find(|p| is_executable(p))
}

/// Where `ashd-lock` is: `$ASHD_LOCK_BIN`, the directory of this executable, `$PATH`.
pub fn locate() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(LOCK_BIN_ENV).filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let exe = std::env::current_exe().ok();
    find_binary("ashd-lock", exe.as_deref().and_then(Path::parent), std::env::var_os("PATH").as_deref())
}

/// Replace this process with `ashd-lock`. Only returns on failure.
pub fn exec() -> anyhow::Error {
    let Some(bin) = locate() else {
        return anyhow::anyhow!("ashd-shell is not running and `ashd-lock` was not found (next to `ashd` or in PATH)");
    };
    let err = Command::new(&bin).exec();
    anyhow::Error::new(err).context(format!("running {}", bin.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exe(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn finds_next_to_the_binary_first_then_path() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let in_a = exe(a.path(), "ashd-lock");
        let in_b = exe(b.path(), "ashd-lock");
        let path = std::env::join_paths([b.path()]).unwrap();
        assert_eq!(find_binary("ashd-lock", Some(a.path()), Some(&path)), Some(in_a));
        assert_eq!(find_binary("ashd-lock", None, Some(&path)), Some(in_b.clone()));
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(find_binary("ashd-lock", Some(empty.path()), Some(&path)), Some(in_b));
        assert_eq!(find_binary("ashd-lock", Some(empty.path()), None), None);
        assert_eq!(find_binary("nothing-here", Some(a.path()), Some(&path)), None);
    }

    #[test]
    fn ignores_non_executables_and_dirs() {
        let a = tempfile::tempdir().unwrap();
        std::fs::write(a.path().join("ashd-lock"), "x").unwrap(); // mode 0644
        assert_eq!(find_binary("ashd-lock", Some(a.path()), None), None);
        let b = tempfile::tempdir().unwrap();
        std::fs::create_dir(b.path().join("ashd-lock")).unwrap();
        assert_eq!(find_binary("ashd-lock", Some(b.path()), None), None);
    }
}
