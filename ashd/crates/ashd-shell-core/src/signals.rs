//! SIGTERM / SIGINT → clean shutdown, SIGHUP → reload `shell.json`.
//!
//! The signals are blocked in the calling (main) thread before any other thread exists,
//! so every thread inherits the mask and only the `ashd-signals` thread receives them
//! (`sigwait`). The shell never forks/execs children itself (apps go through niri), so
//! the inherited mask cannot leak into user processes.

use gpui::{App, AppContext as _, WeakEntity};
use nix::sys::signal::{SigSet, Signal};

use crate::{Shell, ShellConfig};

pub(crate) fn install() -> Option<async_channel::Receiver<Signal>> {
    let mut set = SigSet::empty();
    for s in [Signal::SIGTERM, Signal::SIGINT, Signal::SIGHUP] {
        set.add(s);
    }
    if let Err(e) = set.thread_block() {
        tracing::warn!("cannot block signals ({e}); default signal handling stays");
        return None;
    }
    let (tx, rx) = async_channel::unbounded();
    let spawned = std::thread::Builder::new().name("ashd-signals".into()).spawn(move || {
        while let Ok(sig) = set.wait() {
            if tx.send_blocking(sig).is_err() {
                break;
            }
        }
    });
    if let Err(e) = spawned {
        tracing::warn!("cannot spawn signal thread: {e}");
        let _ = set.thread_unblock();
        return None;
    }
    Some(rx)
}

pub(crate) fn listen(rx: async_channel::Receiver<Signal>, shell: WeakEntity<Shell>, cx: &mut App) {
    cx.spawn(async move |cx| {
        while let Ok(sig) = rx.recv().await {
            match sig {
                Signal::SIGHUP => {
                    let config = cx.background_spawn(async { ShellConfig::load() }).await;
                    tracing::info!("SIGHUP: reloaded config {:?}", config.sources);
                    cx.update(|cx| {
                        cx.set_global(config);
                        crate::config::apply_motion(cx);
                        cx.refresh_windows();
                    });
                }
                other => {
                    tracing::info!("{other}: shutting down");
                    let done = cx.update(|cx| match shell.upgrade() {
                        Some(s) => {
                            crate::shutdown(&s, cx);
                            false
                        }
                        None => {
                            cx.quit();
                            true
                        }
                    });
                    if done {
                        break;
                    }
                }
            }
        }
    })
    .detach();
}
