//! Change detection for the applications dirs: inotify (via rustix) for immediacy, plus
//! a cheap signature (canonical path + mtime) the service polls, which also catches
//! NixOS profile switches (a symlink swap above the watched inode).

use std::{
    mem::MaybeUninit,
    path::{Path, PathBuf},
    time::SystemTime,
};

use async_channel::Sender;
use rustix::{fs::inotify, io::Errno};

use crate::Command;

/// `<data dir>/applications` and its immediate subdirectories.
fn watch_roots(data_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for d in data_dirs {
        let apps = d.join("applications");
        if !apps.is_dir() {
            continue;
        }
        if let Ok(rd) = std::fs::read_dir(&apps) {
            for e in rd.flatten() {
                if e.path().is_dir() {
                    out.push(e.path());
                }
            }
        }
        out.push(apps);
    }
    out
}

pub type Signature = Vec<(PathBuf, Option<SystemTime>)>;

pub fn signature(data_dirs: &[PathBuf]) -> Signature {
    watch_roots(data_dirs)
        .into_iter()
        .map(|p| {
            let canon = p.canonicalize().unwrap_or_else(|_| p.clone());
            let mtime = canon.metadata().and_then(|m| m.modified()).ok();
            (canon, mtime)
        })
        .collect()
}

/// Spawn the inotify thread; every burst of events sends [`Command::Changed`].
/// The watches are registered before this returns, so call it before the first scan.
pub fn spawn(data_dirs: &[PathBuf], tx: Sender<Command>) {
    let roots = watch_roots(data_dirs);
    let fd = match inotify::init(inotify::CreateFlags::CLOEXEC) {
        Ok(fd) => fd,
        Err(e) => {
            tracing::warn!("inotify unavailable, polling only: {e}");
            return;
        }
    };
    let flags = inotify::WatchFlags::CREATE
        | inotify::WatchFlags::DELETE
        | inotify::WatchFlags::MOVED_FROM
        | inotify::WatchFlags::MOVED_TO
        | inotify::WatchFlags::CLOSE_WRITE
        | inotify::WatchFlags::DELETE_SELF
        | inotify::WatchFlags::MOVE_SELF;
    let mut any = false;
    for r in &roots {
        any |= add(&fd, r, flags);
    }
    if !any {
        return;
    }
    let result = std::thread::Builder::new().name("ashd-svc-apps-inotify".into()).spawn(move || {
        if let Err(e) = watch(fd, &tx) {
            tracing::warn!("inotify watcher stopped: {e}");
        }
    });
    if let Err(e) = result {
        tracing::warn!("cannot spawn inotify watcher: {e}");
    }
}

fn watch(fd: rustix::fd::OwnedFd, tx: &Sender<Command>) -> Result<(), Errno> {
    let mut buf = [MaybeUninit::<u8>::uninit(); 4096];
    let mut reader = inotify::Reader::new(&fd, &mut buf);
    loop {
        match reader.next() {
            Ok(_) => {
                if tx.try_send(Command::Changed).is_err() && tx.is_closed() {
                    return Ok(());
                }
            }
            Err(Errno::INTR) => {}
            Err(e) => return Err(e),
        }
    }
}

fn add(fd: &impl rustix::fd::AsFd, path: &Path, flags: inotify::WatchFlags) -> bool {
    inotify::add_watch(fd, path, flags).is_ok()
}
