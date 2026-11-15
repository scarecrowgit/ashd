//! `ASHD_FRAME_LOG=1`: once a second, log how many frames each kind of surface drew
//! (`frames/s bar=0 rail=0 …`). A settled desktop must read all zeros; the evidence that
//! nothing keeps asking for frames when nothing moves. Counting is one relaxed atomic add
//! per drawn frame; the reporter thread only exists with the variable set.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use crate::surfaces::SurfaceKind;

const KINDS: [SurfaceKind; 6] = [
    SurfaceKind::Bar,
    SurfaceKind::Rail,
    SurfaceKind::Scratchpad,
    SurfaceKind::ScratchpadZone,
    SurfaceKind::OverlayPassive,
    SurfaceKind::OverlayModal,
];

static COUNTS: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];

fn slot(kind: SurfaceKind) -> usize {
    KINDS.iter().position(|k| *k == kind).unwrap_or(0)
}

/// A surface root was drawn.
pub(crate) fn frame(kind: SurfaceKind) {
    COUNTS[slot(kind)].fetch_add(1, Ordering::Relaxed);
}

/// Start the reporter when `ASHD_FRAME_LOG` is set to something truthy.
pub(crate) fn start_from_env() {
    let on = std::env::var("ASHD_FRAME_LOG").is_ok_and(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"));
    if !on {
        return;
    }
    std::thread::Builder::new()
        .name("ashd-framelog".into())
        .spawn(|| {
            let mut last = [0u64; 6];
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let mut line = String::from("frames/s");
                for (i, k) in KINDS.iter().enumerate() {
                    let now = COUNTS[i].load(Ordering::Relaxed);
                    line.push_str(&format!(" {}={}", k.name(), now - last[i]));
                    last[i] = now;
                }
                tracing::info!("{line}");
            }
        })
        .ok();
}
