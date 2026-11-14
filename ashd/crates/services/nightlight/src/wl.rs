//! The Wayland side: `zwlr_gamma_control_manager_v1` on every `wl_output`.
//!
//! One connection, one reader thread (`ashd-svc-nightlight-wl`) that dispatches events,
//! binds / drops outputs as they come and go, and records what the compositor says about
//! each gamma control (`gamma_size`, `failed`) in a shared table. The service thread
//! reads that table and sends `set_gamma` requests through the thread-safe proxies.
//! Everything a gamma control did is undone by the compositor when the control (or the
//! connection) goes away, so a crash never leaves the screen tinted.

use std::{
    collections::HashMap,
    io::{Seek as _, Write as _},
    os::fd::AsFd as _,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use async_channel::Sender;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use rustix::fs::{MemfdFlags, memfd_create};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy as _, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_output, wl_registry},
};
use wayland_protocols_wlr::gamma_control::v1::client::{
    zwlr_gamma_control_manager_v1::ZwlrGammaControlManagerV1,
    zwlr_gamma_control_v1::{self, ZwlrGammaControlV1},
};

use crate::{
    Gamma, Input, OutputStatus,
    ramp::{build_ramp, ramp_bytes},
};

/// One output as the compositor sees it.
struct Slot {
    output: wl_output::WlOutput,
    name: Option<String>,
    control: Option<ZwlrGammaControlV1>,
    /// Entries per channel, once the compositor told us.
    size: Option<u32>,
    /// The compositor refused or revoked the control ("already in use", unsupported).
    failed: bool,
}

impl Slot {
    fn ready(&self) -> bool {
        self.control.is_some() && self.size.is_some() && !self.failed
    }
}

type Table = Arc<Mutex<HashMap<u32, Slot>>>;

struct WlState {
    tx: Sender<Input>,
    manager: ZwlrGammaControlManagerV1,
    table: Table,
}

impl WlState {
    fn changed(&self) {
        let _ = self.tx.try_send(Input::OutputsChanged);
    }

    fn add_output(&self, registry: &wl_registry::WlRegistry, name: u32, version: u32, qh: &QueueHandle<Self>) {
        // The initial globals reach us twice if the registry replays them; bind once.
        if self.table.lock().is_ok_and(|t| t.contains_key(&name)) {
            return;
        }
        let output = registry.bind::<wl_output::WlOutput, _, _>(name, version.min(4), qh, name);
        let control = self.manager.get_gamma_control(&output, qh, name);
        if let Ok(mut t) = self.table.lock() {
            t.insert(name, Slot { output, name: None, control: Some(control), size: None, failed: false });
        }
        self.changed();
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for WlState {
    fn event(state: &mut Self, registry: &wl_registry::WlRegistry, event: wl_registry::Event, _: &GlobalListContents, _: &Connection, qh: &QueueHandle<Self>) {
        match event {
            wl_registry::Event::Global { name, interface, version } if interface == "wl_output" => {
                state.add_output(registry, name, version, qh);
            }
            wl_registry::Event::GlobalRemove { name } => {
                let removed = state.table.lock().ok().and_then(|mut t| t.remove(&name));
                if let Some(slot) = removed {
                    if let Some(c) = slot.control {
                        c.destroy();
                    }
                    if slot.output.version() >= 3 {
                        slot.output.release();
                    }
                    state.changed();
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, u32> for WlState {
    fn event(state: &mut Self, _: &wl_output::WlOutput, event: wl_output::Event, name: &u32, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_output::Event::Name { name: connector } = event {
            if let Some(slot) = state.table.lock().ok().as_mut().and_then(|t| t.get_mut(name)) {
                slot.name = Some(connector);
            }
            state.changed();
        }
    }
}

impl Dispatch<ZwlrGammaControlManagerV1, ()> for WlState {
    fn event(_: &mut Self, _: &ZwlrGammaControlManagerV1, _: <ZwlrGammaControlManagerV1 as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwlrGammaControlV1, u32> for WlState {
    fn event(state: &mut Self, _: &ZwlrGammaControlV1, event: zwlr_gamma_control_v1::Event, name: &u32, _: &Connection, _: &QueueHandle<Self>) {
        let mut guard = state.table.lock().ok();
        let Some(slot) = guard.as_mut().and_then(|t| t.get_mut(name)) else { return };
        match event {
            zwlr_gamma_control_v1::Event::GammaSize { size } => {
                slot.size = Some(size);
            }
            zwlr_gamma_control_v1::Event::Failed => {
                tracing::warn!("nightlight: gamma control of {} failed (in use by another client, or unsupported)", slot.name.as_deref().unwrap_or("an output"));
                slot.failed = true;
                if let Some(c) = slot.control.take() {
                    c.destroy();
                }
            }
            _ => {}
        }
        drop(guard);
        state.changed();
    }
}

pub(crate) struct WlGamma {
    conn: Connection,
    qh: QueueHandle<WlState>,
    manager: ZwlrGammaControlManagerV1,
    table: Table,
    stop: Arc<AtomicBool>,
}

impl WlGamma {
    pub(crate) fn connect(tx: Sender<Input>) -> Result<Self, String> {
        let conn = Connection::connect_to_env().map_err(|e| format!("wayland connect: {e}"))?;
        let (globals, mut queue) = registry_queue_init::<WlState>(&conn).map_err(|e| format!("wayland registry: {e}"))?;
        let qh = queue.handle();
        let manager = globals
            .bind::<ZwlrGammaControlManagerV1, _, _>(&qh, 1..=1, ())
            .map_err(|_| "the compositor does not offer zwlr_gamma_control_manager_v1".to_string())?;
        let table: Table = Arc::default();
        let mut state = WlState { tx, manager: manager.clone(), table: table.clone() };
        // Outputs that already exist: bind them like the registry events later bind new ones.
        let existing: Vec<(u32, u32)> = globals
            .contents()
            .with_list(|list| list.iter().filter(|g| g.interface == "wl_output").map(|g| (g.name, g.version)).collect());
        let registry = globals.registry().clone();
        for (name, version) in existing {
            state.add_output(&registry, name, version, &qh);
        }
        queue.roundtrip(&mut state).map_err(|e| format!("wayland roundtrip: {e}"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let (reader_conn, reader_stop) = (conn.clone(), stop.clone());
        std::thread::Builder::new()
            .name("ashd-svc-nightlight-wl".into())
            .spawn(move || {
                if let Err(e) = read_loop(&reader_conn, &mut queue, &mut state, &reader_stop) {
                    tracing::debug!("nightlight: wayland connection ended: {e}");
                    let _ = state.tx.try_send(Input::Lost);
                }
            })
            .map_err(|e| format!("spawn reader: {e}"))?;
        Ok(Self { conn, qh, manager, table, stop })
    }
}

/// A sealed-by-convention memfd holding the table, rewound for the compositor.
fn table_file(ramp: &[u16]) -> std::io::Result<std::fs::File> {
    let fd = memfd_create("ashd-nightlight-gamma", MemfdFlags::CLOEXEC).map_err(std::io::Error::from)?;
    let mut file = std::fs::File::from(fd);
    file.write_all(&ramp_bytes(ramp))?;
    file.rewind()?;
    Ok(file)
}

impl Gamma for WlGamma {
    fn apply(&mut self, rgb: [f32; 3]) {
        let Ok(table) = self.table.lock() else { return };
        for slot in table.values().filter(|s| s.ready()) {
            let (Some(control), Some(size)) = (&slot.control, slot.size) else { continue };
            match table_file(&build_ramp(size, rgb)) {
                Ok(file) => control.set_gamma(file.as_fd()),
                Err(e) => tracing::warn!("nightlight: cannot build the gamma table: {e}"),
            }
        }
        drop(table);
        if let Err(e) = self.conn.flush() {
            tracing::debug!("nightlight: flush: {e}");
        }
    }

    fn outputs(&self) -> Vec<OutputStatus> {
        let Ok(table) = self.table.lock() else { return Vec::new() };
        let mut v: Vec<_> = table
            .iter()
            .map(|(id, s)| (*id, OutputStatus { name: s.name.clone().unwrap_or_else(|| format!("output-{id}")), available: s.ready() }))
            .collect();
        v.sort_by_key(|(id, _)| *id);
        v.into_iter().map(|(_, s)| s).collect()
    }

    fn retry_failed(&mut self) -> bool {
        let Ok(mut table) = self.table.lock() else { return false };
        let mut any = false;
        for (name, slot) in table.iter_mut().filter(|(_, s)| s.failed) {
            slot.control = Some(self.manager.get_gamma_control(&slot.output, &self.qh, *name));
            slot.size = None;
            slot.failed = false;
            any = true;
        }
        drop(table);
        let _ = self.conn.flush();
        any
    }

    fn has_failed(&self) -> bool {
        self.table.lock().is_ok_and(|t| t.values().any(|s| s.failed))
    }
}

impl Drop for WlGamma {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Destroying a control restores the output's original gamma table.
        if let Ok(mut table) = self.table.lock() {
            for slot in table.values_mut() {
                if let Some(c) = slot.control.take() {
                    c.destroy();
                }
            }
        }
        self.manager.destroy();
        let _ = self.conn.flush();
    }
}

fn read_loop(conn: &Connection, queue: &mut EventQueue<WlState>, state: &mut WlState, stop: &AtomicBool) -> Result<(), String> {
    while !stop.load(Ordering::Acquire) {
        queue.dispatch_pending(state).map_err(|e| e.to_string())?;
        conn.flush().map_err(|e| e.to_string())?;
        let Some(guard) = queue.prepare_read() else { continue };
        let mut fds = [PollFd::new(guard.connection_fd(), PollFlags::POLLIN)];
        match poll(&mut fds, PollTimeout::from(250u8)) {
            Ok(n) if n > 0 => {
                guard.read().map_err(|e| e.to_string())?;
            }
            Ok(_) | Err(nix::errno::Errno::EINTR) => drop(guard),
            Err(e) => return Err(format!("poll: {e}")),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use super::*;
    use crate::ramp::kelvin_to_rgb;

    #[test]
    fn the_shared_memory_table_is_three_channels_rewound_to_the_start() {
        let ramp = build_ramp(256, kelvin_to_rgb(3400.0));
        let mut file = table_file(&ramp).unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes.len(), 256 * 3 * 2, "size x 3 channels x u16, exactly what the protocol validates");
        assert_eq!(bytes, ramp_bytes(&ramp));
        // blue is the last channel and the most attenuated
        let last = |ch: usize| u16::from_ne_bytes([bytes[(ch * 256 + 255) * 2], bytes[(ch * 256 + 255) * 2 + 1]]);
        assert_eq!(last(0), 65535);
        assert!(last(2) < last(1) && last(1) < last(0));
    }
}
