//! Clipboard history over `ext-data-control-v1` (fallback `zwlr-data-control-unstable-v1`).
//! No GPUI.
//!
//! Contract (ARCHITECTURE.md "Services"): `start() -> (Handle, Receiver<ClipboardState>)`.
//! The service thread (`ashd-svc-clipboard`) owns its own Wayland connection (to
//! `$WAYLAND_DISPLAY` — whichever compositor that is), watches the regular clipboard
//! (primary selection only when enabled), reads each new offer through a pipe with a size
//! cap, classifies it ([`Kind`]), and stores it in redb under the ashd state dir
//! (`<state>/clipboard.redb`, images as files in `<state>/clipboard/`).
//!
//! Ignored on purpose: offers that carry `x-kde-passwordManagerHint = secret`, our own
//! re-sets (every source we create also offers `application/x-ashd-clipboard`), empty and
//! whitespace-only text, entries over the caps ([`MAX_TEXT`], [`MAX_IMAGE`], reported in
//! [`ClipboardState::note`]).
//!
//! Environment: `ASHD_STATE_DIR` (history location), `ASHD_CLIPBOARD_PRIMARY=1` (record the
//! primary selection too), `WAYLAND_DISPLAY`.

mod kind;
mod proto;
mod store;

#[cfg(test)]
mod live_test;

use std::{
    os::fd::{AsFd, OwnedFd},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use async_channel::{Receiver, Sender, TryRecvError};

pub use kind::{Colour, Detected, Kind, detect, parse_hex_colour};
pub use proto::Protocol;
pub use store::{Captured, Entry, ImageInfo, MAX_UNPINNED, PREVIEW_MAX, THUMB_MAX, html_to_text, image_ext, uri_list_to_text};

use proto::{Event, Offer, OWN_MARKER, ReadResult, SENSITIVE_HINT, Served, Wayland};
use store::{Payload, Store, is_plain_text_mime};

/// Largest text (any representation) that is recorded.
pub const MAX_TEXT: usize = 1024 * 1024;
/// Largest image that is recorded.
pub const MAX_IMAGE: usize = 20 * 1024 * 1024;
/// Give up on a clipboard owner that sends nothing for this long.
const READ_IDLE: Duration = Duration::from_secs(3);

/// Outcome of the last save command (compare `seq` to see a new one).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SaveResult {
    pub seq: u64,
    pub id: u64,
    /// Where the file was written.
    pub path: Option<PathBuf>,
    pub error: Option<String>,
}

/// A one-line message for the user (an entry was skipped, …); compare `seq`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Note {
    pub seq: u64,
    pub text: String,
    /// Unix milliseconds when it was posted.
    pub at_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClipboardState {
    /// A data-control protocol is bound and the clipboard is being watched. `false`: the
    /// compositor has neither protocol (history is still readable, `activate` does nothing).
    pub available: bool,
    pub protocol: Option<Protocol>,
    /// History is stored on disk (`false`: in-memory fallback for this run).
    pub persistent: bool,
    /// Newest first (by last copy); pinned entries are in place, not hoisted.
    pub entries: Arc<Vec<Entry>>,
    /// The entry that is the clipboard right now (last captured or activated), if known.
    pub active: Option<u64>,
    /// Bumped whenever a new entry or a duplicate was captured.
    pub capture_seq: u64,
    pub watch_primary: bool,
    pub note: Option<Note>,
    pub last_save: Option<SaveResult>,
}

impl ClipboardState {
    pub fn entry(&self, id: u64) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    pub fn pinned_count(&self) -> usize {
        self.entries.iter().filter(|e| e.pinned).count()
    }
}

pub enum Command {
    /// Become the clipboard owner with this entry's data (all stored MIME types).
    Activate(u64),
    SetPinned(u64, bool),
    Remove(u64),
    ClearUnpinned,
    /// Write the entry's primary payload to this path (see [`ClipboardState::last_save`]).
    SaveTo(u64, PathBuf),
    /// Write into `~/Pictures` (images) or `~/Downloads` with a timestamped name.
    SaveDefault(u64),
    SetWatchPrimary(bool),
}

#[derive(Clone)]
pub struct Handle {
    tx: Sender<Command>,
    // Field order matters: `tx` drops first, then the last wake write end closes, which
    // hangs up the service thread's wake pipe and ends it.
    wake: Option<Arc<OwnedFd>>,
}

impl Handle {
    fn send(&self, c: Command) {
        if self.tx.try_send(c).is_ok()
            && let Some(w) = &self.wake
        {
            let _ = nix::unistd::write(w.as_fd(), &[1u8]);
        }
    }

    /// Become the clipboard owner with this entry (the next paste yields it).
    pub fn activate(&self, id: u64) {
        self.send(Command::Activate(id));
    }
    pub fn set_pinned(&self, id: u64, pinned: bool) {
        self.send(Command::SetPinned(id, pinned));
    }
    pub fn remove(&self, id: u64) {
        self.send(Command::Remove(id));
    }
    pub fn clear_unpinned(&self) {
        self.send(Command::ClearUnpinned);
    }
    pub fn save_to(&self, id: u64, path: impl Into<PathBuf>) {
        self.send(Command::SaveTo(id, path.into()));
    }
    /// Save into `~/Pictures` / `~/Downloads`; the path comes back in `last_save`.
    pub fn save_default(&self, id: u64) {
        self.send(Command::SaveDefault(id));
    }
    pub fn set_watch_primary(&self, on: bool) {
        self.send(Command::SetWatchPrimary(on));
    }
    pub fn send_command(&self, c: Command) {
        self.send(c);
    }
    pub fn disconnected() -> Self {
        Self { tx: async_channel::bounded(1).0, wake: None }
    }
}

/// `$ASHD_STATE_DIR` or `$XDG_STATE_HOME/ashd` or `~/.local/state/ashd` (same rule as
/// `ashd_tokens::state::state_dir`).
pub fn state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("ASHD_STATE_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    dirs::state_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("ashd")
}

/// Spawn the `ashd-svc-clipboard` thread. Connects to `$WAYLAND_DISPLAY`.
pub fn start() -> (Handle, Receiver<ClipboardState>) {
    start_in(state_dir())
}

/// [`start`] with an explicit state directory.
pub fn start_in(dir: PathBuf) -> (Handle, Receiver<ClipboardState>) {
    spawn(dir, true)
}

/// `connect = false`: history and commands only, never a Wayland connection (tests).
fn spawn(dir: PathBuf, connect: bool) -> (Handle, Receiver<ClipboardState>) {
    let (tx, rx) = async_channel::unbounded();
    let (state_tx, state_rx) = async_channel::bounded(1);
    let (wake_rd, wake_wr) = nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC | nix::fcntl::OFlag::O_NONBLOCK)
        .expect("clipboard wake pipe");
    std::thread::Builder::new()
        .name("ashd-svc-clipboard".into())
        .spawn(move || run(rx, wake_rd, state_tx, dir, connect))
        .expect("spawn ashd-svc-clipboard");
    (Handle { tx, wake: Some(Arc::new(wake_wr)) }, state_rx)
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Which representations of an offer to read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub primary: String,
    pub alts: Vec<String>,
}

/// Choose what to read from the offered MIME types: an image (png first), else plain text
/// (`text/plain;charset=utf-8` first), else a URI list, else HTML; `text/html` and
/// `text/uri-list` ride along as alternatives of text. `None`: nothing we can store.
pub fn plan_mimes(mimes: &[String]) -> Option<Plan> {
    let find = |want: &str| mimes.iter().find(|m| m.eq_ignore_ascii_case(want)).cloned();
    if let Some(png) = find("image/png") {
        return Some(Plan { primary: png, alts: Vec::new() });
    }
    if let Some(img) = mimes.iter().find(|m| m.to_ascii_lowercase().starts_with("image/") && !m.contains('*')) {
        return Some(Plan { primary: img.clone(), alts: Vec::new() });
    }
    let plain = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING", "STRING", "TEXT"].into_iter().find_map(find);
    let html = find("text/html");
    let uris = find("text/uri-list");
    match plain {
        Some(primary) => Some(Plan { primary, alts: [html, uris].into_iter().flatten().collect() }),
        None => match (uris, html) {
            (Some(u), h) => Some(Plan { primary: u, alts: h.into_iter().collect() }),
            (None, Some(h)) => Some(Plan { primary: h, alts: Vec::new() }),
            (None, None) => None,
        },
    }
}

/// The directory a default save goes to: images prefer Pictures then Downloads, everything
/// else Downloads then Pictures; the first that exists, else the first candidate.
pub fn default_save_dir(image: bool, home: &Path, pictures: Option<PathBuf>, downloads: Option<PathBuf>) -> PathBuf {
    let pictures = pictures.unwrap_or_else(|| home.join("Pictures"));
    let downloads = downloads.unwrap_or_else(|| home.join("Downloads"));
    let order = if image { [pictures, downloads] } else { [downloads, pictures] };
    order.iter().find(|d| d.is_dir()).cloned().unwrap_or_else(|| order[0].clone())
}

fn save_extension(entry: &Entry) -> &'static str {
    match entry.mime.as_str() {
        m if m.starts_with("image/") => image_ext(m),
        "text/html" => "html",
        _ => "txt",
    }
}

/// `clip-20261113-142200.png`, de-duplicated against existing files in `dir`.
fn timestamped_path(dir: &Path, ext: &str) -> PathBuf {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let mut path = dir.join(format!("clip-{stamp}.{ext}"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("clip-{stamp}-{n}.{ext}"));
        n += 1;
    }
    path
}

fn human_bytes(n: usize) -> String {
    const MB: usize = 1024 * 1024;
    if n >= MB { format!("{} MB", n / MB) } else { format!("{} KB", n / 1024) }
}

/// The service's mutable core: store + Wayland connection + the state it publishes.
struct Svc {
    store: Store,
    wl: Option<Wayland>,
    tx: Sender<ClipboardState>,
    state: ClipboardState,
    seq: u64,
}

impl Svc {
    fn new(dir: &Path, tx: Sender<ClipboardState>, connect: bool) -> Svc {
        let store = Store::open(&dir.join("clipboard.redb"), &dir.join("clipboard"));
        let mut state = ClipboardState {
            persistent: store.persistent(),
            entries: Arc::new(store.entries()),
            watch_primary: std::env::var("ASHD_CLIPBOARD_PRIMARY").is_ok_and(|v| v == "1"),
            ..ClipboardState::default()
        };
        let wl = if connect {
            match Wayland::connect() {
                Ok(w) => {
                    state.available = true;
                    state.protocol = Some(w.protocol());
                    tracing::info!("clipboard: watching via {}", w.protocol().name());
                    Some(w)
                }
                Err(e) => {
                    tracing::warn!("clipboard history unavailable: {e}");
                    None
                }
            }
        } else {
            None
        };
        Svc { store, wl, tx, state, seq: 0 }
    }

    fn publish(&self) {
        let _ = self.tx.force_send(self.state.clone());
    }

    fn refresh_entries(&mut self) {
        self.state.entries = Arc::new(self.store.entries());
    }

    fn note(&mut self, text: impl Into<String>) {
        self.seq += 1;
        self.state.note = Some(Note { seq: self.seq, text: text.into(), at_ms: now_ms() });
    }

    /// The connection broke: keep the history, stop watching.
    fn lost(&mut self, why: &str) {
        tracing::warn!("clipboard: {why}");
        self.wl = None;
        self.state.available = false;
        self.state.protocol = None;
        self.publish();
    }

    fn handle_events(&mut self) {
        let Some(wl) = self.wl.as_mut() else { return };
        let events = wl.take_events();
        if events.is_empty() {
            return;
        }
        // Only the newest selection of each kind matters; superseded offers are destroyed.
        let (mut selection, mut primary) = (None, None);
        let mut finished = false;
        for ev in events {
            match ev {
                Event::Selection(o) => {
                    if let Some(old) = selection.replace(o).flatten() {
                        old.destroy();
                    }
                }
                Event::Primary(o) => {
                    if let Some(old) = primary.replace(o).flatten() {
                        old.destroy();
                    }
                }
                Event::Finished => finished = true,
            }
        }
        if finished {
            self.lost("the data device was withdrawn by the compositor");
            return;
        }
        match selection {
            Some(Some(offer)) => self.capture(offer),
            Some(None) if self.state.active.take().is_some() => self.publish(),
            _ => {}
        }
        match primary {
            Some(Some(offer)) if self.state.watch_primary => self.capture(offer),
            Some(Some(offer)) => offer.destroy(),
            _ => {}
        }
    }

    fn capture(&mut self, offer: Offer) {
        let Some(wl) = self.wl.as_ref() else { return };
        let mimes = offer.mimes();
        if mimes.iter().any(|m| m == OWN_MARKER) {
            offer.destroy();
            return;
        }
        if mimes.iter().any(|m| m == SENSITIVE_HINT) {
            let hint = wl.read_offer(&offer, SENSITIVE_HINT, 64, READ_IDLE);
            // unreadable hint: assume the worst
            if !matches!(&hint, ReadResult::Data(d) if String::from_utf8_lossy(d).trim() != "secret") {
                offer.destroy();
                return;
            }
        }
        let Some(plan) = plan_mimes(&mimes) else {
            tracing::debug!("clipboard: nothing storable in {mimes:?}");
            offer.destroy();
            return;
        };
        let is_image = plan.primary.to_ascii_lowercase().starts_with("image/");
        let cap = if is_image { MAX_IMAGE } else { MAX_TEXT };
        let mut reps: Vec<(String, Vec<u8>)> = Vec::new();
        let mut skipped = None;
        match wl.read_offer(&offer, &plan.primary, cap, READ_IDLE) {
            ReadResult::Data(d) => reps.push((plan.primary.clone(), d)),
            ReadResult::TooBig => {
                skipped = Some(format!("Skipped a {} clipboard entry: larger than {}.", plan.primary, human_bytes(cap)));
            }
            ReadResult::Failed(e) => tracing::debug!("clipboard: reading {}: {e}", plan.primary),
        }
        if !reps.is_empty() {
            for alt in &plan.alts {
                if let ReadResult::Data(d) = wl.read_offer(&offer, alt, MAX_TEXT, READ_IDLE) {
                    reps.push((alt.clone(), d));
                }
            }
        }
        offer.destroy();
        let _ = wl.flush();
        if let Some(text) = skipped {
            self.note(text);
            self.publish();
            return;
        }
        if reps.first().is_none_or(|(m, d)| !m.to_ascii_lowercase().starts_with("image/") && String::from_utf8_lossy(d).trim().is_empty()) {
            return;
        }
        if let Some(ins) = self.store.insert(Captured { reps }, now_ms()) {
            self.state.active = Some(ins.id);
            self.state.capture_seq += 1;
            self.refresh_entries();
            self.publish();
        }
    }

    fn command(&mut self, c: Command) {
        match c {
            Command::Activate(id) => self.activate(id),
            Command::SetPinned(id, pinned) => {
                if self.store.set_pinned(id, pinned) {
                    self.refresh_entries();
                }
            }
            Command::Remove(id) => {
                if self.store.remove(id) {
                    if self.state.active == Some(id) {
                        self.state.active = None;
                    }
                    self.refresh_entries();
                }
            }
            Command::ClearUnpinned => {
                self.store.clear_unpinned();
                if self.state.active.is_some_and(|id| self.store.entry_by_id(id).is_none()) {
                    self.state.active = None;
                }
                self.refresh_entries();
            }
            Command::SaveTo(id, path) => self.save(id, path),
            Command::SaveDefault(id) => {
                let Some(entry) = self.store.entry_by_id(id) else { return };
                let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
                let dir = default_save_dir(entry.kind == Kind::Image, &home, dirs::picture_dir(), dirs::download_dir());
                let path = timestamped_path(&dir, save_extension(&entry));
                self.save(id, path);
            }
            Command::SetWatchPrimary(on) => self.state.watch_primary = on,
        }
        self.publish();
    }

    fn activate(&mut self, id: u64) {
        let Some(Payload { reps }) = self.store.payload(id) else {
            self.note("That entry is gone.");
            return;
        };
        let Some(wl) = self.wl.as_mut() else {
            self.note("The clipboard is not available (no data-control protocol).");
            return;
        };
        let mut served: Vec<(String, Arc<Vec<u8>>)> = Vec::new();
        for (i, (mime, bytes)) in reps.into_iter().enumerate() {
            let bytes = Arc::new(bytes);
            let aliases: &[&str] = if i == 0 && is_plain_text_mime(&mime) {
                &["text/plain;charset=utf-8", "text/plain", "UTF8_STRING", "STRING", "TEXT"]
            } else {
                &[]
            };
            if !served.iter().any(|(m, _)| *m == mime) {
                served.push((mime, bytes.clone()));
            }
            for alias in aliases {
                if !served.iter().any(|(m, _)| m == alias) {
                    served.push(((*alias).to_string(), bytes.clone()));
                }
            }
        }
        wl.set_selection(Served { reps: served }, false, true);
        self.store.touch(id, now_ms());
        self.state.active = Some(id);
        self.refresh_entries();
    }

    fn save(&mut self, id: u64, path: PathBuf) {
        self.seq += 1;
        let seq = self.seq;
        let result = match self.write_payload(id, &path) {
            Ok(()) => SaveResult { seq, id, path: Some(path), error: None },
            Err(e) => SaveResult { seq, id, path: Some(path), error: Some(e) },
        };
        self.state.last_save = Some(result);
    }

    fn write_payload(&self, id: u64, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        if let Some(src) = self.store.payload_file(id) {
            std::fs::copy(src, path).map(|_| ()).map_err(|e| format!("{}: {e}", path.display()))
        } else {
            let payload = self.store.payload(id).ok_or("That entry is gone.")?;
            let (_, bytes) = payload.reps.first().ok_or("That entry is empty.")?;
            std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
        }
    }
}

fn run(commands: Receiver<Command>, wake: OwnedFd, tx: Sender<ClipboardState>, dir: PathBuf, connect: bool) {
    let mut svc = Svc::new(&dir, tx, connect);
    svc.publish();
    loop {
        if let Some(wl) = svc.wl.as_mut()
            && let Err(e) = wl.dispatch_pending()
        {
            svc.lost(&e);
        }
        svc.handle_events();
        if let Some(wl) = &svc.wl
            && let Err(e) = wl.flush()
        {
            svc.lost(&e);
        }
        loop {
            match commands.try_recv() {
                Ok(c) => svc.command(c),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Closed) => return,
            }
        }
        let woke = match svc.wl.as_mut() {
            Some(wl) => match wl.wait(Some(wake.as_fd()), nix::poll::PollTimeout::NONE) {
                Ok(w) => w,
                Err(e) => {
                    svc.lost(&e);
                    false
                }
            },
            None => wait_wake(&wake),
        };
        if woke && drain_wake(&wake) {
            return;
        }
    }
}

/// Block until the wake pipe is readable or hung up.
fn wait_wake(wake: &OwnedFd) -> bool {
    use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
    let mut fds = [PollFd::new(wake.as_fd(), PollFlags::POLLIN)];
    let _ = poll(&mut fds, PollTimeout::NONE);
    true
}

/// Empty the wake pipe; `true` when every [`Handle`] is gone (write end closed).
fn drain_wake(wake: &OwnedFd) -> bool {
    let mut buf = [0u8; 64];
    loop {
        match nix::unistd::read(wake, &mut buf) {
            Ok(0) => return true,
            Ok(_) => {}
            Err(nix::errno::Errno::EINTR) => {}
            Err(_) => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mimes(m: &[&str]) -> Vec<String> {
        m.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn plan_prefers_png_then_text_with_alternatives() {
        let p = plan_mimes(&mimes(&["text/plain", "image/jpeg", "image/png"])).unwrap();
        assert_eq!(p, Plan { primary: "image/png".into(), alts: vec![] });
        let p = plan_mimes(&mimes(&["image/webp", "TARGETS"])).unwrap();
        assert_eq!(p.primary, "image/webp");
        let p = plan_mimes(&mimes(&["UTF8_STRING", "text/html", "text/plain;charset=utf-8", "text/plain"])).unwrap();
        assert_eq!(p, Plan { primary: "text/plain;charset=utf-8".into(), alts: vec!["text/html".into()] });
        let p = plan_mimes(&mimes(&["text/uri-list", "text/plain", "x-special/gnome-copied-files"])).unwrap();
        assert_eq!(p, Plan { primary: "text/plain".into(), alts: vec!["text/uri-list".into()] });
        let p = plan_mimes(&mimes(&["text/uri-list", "text/html"])).unwrap();
        assert_eq!(p, Plan { primary: "text/uri-list".into(), alts: vec!["text/html".into()] });
        let p = plan_mimes(&mimes(&["text/html"])).unwrap();
        assert_eq!(p.primary, "text/html");
        assert!(plan_mimes(&mimes(&["application/octet-stream", "TARGETS", OWN_MARKER])).is_none());
        let p = plan_mimes(&mimes(&["text/plain;charset=UTF-8"])).unwrap();
        assert_eq!(p.primary, "text/plain;charset=UTF-8");
    }

    #[test]
    fn save_dir_choice() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path();
        // none exists: first candidate
        assert_eq!(default_save_dir(true, home, None, None), home.join("Pictures"));
        assert_eq!(default_save_dir(false, home, None, None), home.join("Downloads"));
        std::fs::create_dir(home.join("Downloads")).unwrap();
        // images fall back to Downloads when there is no Pictures
        assert_eq!(default_save_dir(true, home, None, None), home.join("Downloads"));
        std::fs::create_dir(home.join("Pictures")).unwrap();
        assert_eq!(default_save_dir(true, home, None, None), home.join("Pictures"));
        assert_eq!(default_save_dir(false, home, None, None), home.join("Downloads"));
        // XDG overrides
        let custom = home.join("pics");
        std::fs::create_dir(&custom).unwrap();
        assert_eq!(default_save_dir(true, home, Some(custom.clone()), None), custom);
    }

    fn svc(dir: &Path) -> (Svc, Receiver<ClipboardState>) {
        let (tx, rx) = async_channel::bounded(1);
        (Svc::new(dir, tx, false), rx)
    }

    #[test]
    fn commands_update_state_and_save() {
        let d = tempfile::tempdir().unwrap();
        let (mut s, rx) = svc(d.path());
        assert!(!s.state.available);
        let a = s.store.insert(Captured { reps: vec![("text/plain".into(), b"alpha".to_vec())] }, 1).unwrap().id;
        let b = s.store.insert(Captured { reps: vec![("text/plain".into(), b"beta".to_vec())] }, 2).unwrap().id;
        s.refresh_entries();
        s.command(Command::SetPinned(a, true));
        assert_eq!(rx.try_recv().unwrap().pinned_count(), 1);
        // activate without a data-control protocol: a note, no panic, nothing reordered
        s.command(Command::Activate(a));
        let st = rx.try_recv().unwrap();
        assert!(st.note.as_ref().unwrap().text.contains("not available"));
        assert_eq!(st.entries[0].id, b);

        let out = d.path().join("out/a.txt");
        s.command(Command::SaveTo(a, out.clone()));
        let st = rx.try_recv().unwrap();
        let saved = st.last_save.unwrap();
        assert_eq!(saved.path.as_deref(), Some(out.as_path()));
        assert!(saved.error.is_none());
        assert_eq!(std::fs::read(&out).unwrap(), b"alpha");

        s.command(Command::SaveTo(999, d.path().join("x.txt")));
        assert!(rx.try_recv().unwrap().last_save.unwrap().error.is_some());

        s.command(Command::Remove(b));
        s.command(Command::ClearUnpinned);
        let st = rx.try_recv().unwrap();
        assert_eq!(st.entries.len(), 1);
        assert_eq!(st.entries[0].id, a);
        assert_eq!(st.entry(a).map(|e| e.first_line()), Some("alpha".to_string()));
    }

    #[test]
    fn image_save_copies_the_file() {
        let d = tempfile::tempdir().unwrap();
        let (mut s, rx) = svc(d.path());
        let img = image::RgbaImage::from_pixel(8, 8, image::Rgba([1, 2, 3, 255]));
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img).write_to(&mut png, image::ImageFormat::Png).unwrap();
        let png = png.into_inner();
        let id = s.store.insert(Captured { reps: vec![("image/png".into(), png.clone())] }, 1).unwrap().id;
        s.refresh_entries();
        let out = d.path().join("saved.png");
        s.command(Command::SaveTo(id, out.clone()));
        assert!(rx.try_recv().unwrap().last_save.unwrap().error.is_none());
        assert_eq!(std::fs::read(out).unwrap(), png);
    }

    #[test]
    fn timestamped_names_do_not_collide() {
        let d = tempfile::tempdir().unwrap();
        let a = timestamped_path(d.path(), "png");
        std::fs::write(&a, b"x").unwrap();
        let b = timestamped_path(d.path(), "png");
        assert_ne!(a, b);
        assert!(a.file_name().unwrap().to_str().unwrap().starts_with("clip-"));
    }

    #[test]
    fn handle_drop_stops_the_thread() {
        // No Wayland (never the live compositor in tests): history and commands only.
        let d = tempfile::tempdir().unwrap();
        let (h, rx) = spawn(d.path().to_path_buf(), false);
        let st = smol_free_recv(&rx);
        assert!(st.entries.is_empty());
        h.set_watch_primary(true);
        drop(h);
    }

    fn smol_free_recv(rx: &Receiver<ClipboardState>) -> ClipboardState {
        for _ in 0..200 {
            if let Ok(s) = rx.try_recv() {
                return s;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("service did not publish");
    }
}
