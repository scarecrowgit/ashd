//! Applications: desktop entries (XDG data dirs), icon lookup, fuzzy search and launch
//! history. No GPUI.
//!
//! Contract (ARCHITECTURE.md "Services"): `start() -> (Handle, Receiver<AppsState>)`.
//! Search is a pure function ([`search`]) the launcher calls synchronously (nucleo is
//! fast enough for a few hundred entries per keystroke); launching is the shell's job:
//! `ctx.shell.spawn(app.launch_argv(Some(&terminal_command())), cx)` (niri
//! `Action::Spawn`), followed by `handle.record_launch(&app.id)`. Owner: launcher.
//!
//! Environment: `XDG_DATA_HOME` / `XDG_DATA_DIRS` (scan), `XDG_CURRENT_DESKTOP`
//! (`OnlyShowIn`/`NotShowIn`), `ASHD_ICON_THEME` (else GTK settings), `ASHD_STATE_DIR`
//! (launch history `apps.redb`), `ASHD_TERMINAL` / `TERMINAL` (terminal wrapper).

mod history;
mod icons;
mod scan;
mod search;
mod watch;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use async_channel::{Receiver, Sender};

pub use history::state_dir;
pub use icons::current_icon_theme;
pub use scan::{ScanEnv, parse_exec, scan, xdg_data_dirs};
pub use search::search;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct App {
    /// Desktop file id (`org.gnome.Nautilus`, `kitty`; subdirectories joined with `-`).
    pub id: String,
    pub name: String,
    pub generic_name: Option<String>,
    pub comment: Option<String>,
    /// `Exec` split into argv, field codes (%u %F …) removed.
    pub exec: Vec<String>,
    /// `Icon=` value (name or absolute path).
    pub icon_name: Option<String>,
    /// Resolved icon file (svg/png) for the current icon theme, if found.
    pub icon_path: Option<PathBuf>,
    pub terminal: bool,
    pub keywords: Vec<String>,
    pub categories: Vec<String>,
    /// Matches niri's `app_id` of the running windows (dock running indicators).
    pub startup_wm_class: Option<String>,
    pub desktop_file: PathBuf,
    /// Launches recorded through [`Handle::record_launch`] (frecency ranking).
    pub launch_count: u32,
    /// Unix seconds of the last recorded launch, 0 = never.
    pub last_launch: u64,
}

impl App {
    /// argv to spawn. `terminal`: the terminal command used for `Terminal=true` apps
    /// (e.g. `["kitty", "-e"]`, see [`terminal_command`]); `None` runs `exec` directly.
    pub fn launch_argv(&self, terminal: Option<&[String]>) -> Vec<String> {
        match (self.terminal, terminal) {
            (true, Some(term)) => term.iter().cloned().chain(self.exec.iter().cloned()).collect(),
            _ => self.exec.clone(),
        }
    }

    /// The niri `app_id` this app's windows are expected to have.
    pub fn window_app_id(&self) -> &str {
        self.startup_wm_class.as_deref().unwrap_or(&self.id)
    }

    /// Does a window with this niri `app_id` belong to this app? (Any of the rules of
    /// [`find_app_for_window`]; use that function to pick the best app.)
    pub fn matches_window(&self, app_id: &str) -> bool {
        window_match_rank(self, app_id).is_some()
    }
}

/// How well `app` matches a window `app_id`; lower is better.
fn window_match_rank(app: &App, app_id: &str) -> Option<u8> {
    if app_id.is_empty() {
        return None;
    }
    let lower = app_id.to_lowercase();
    let eq = |a: &str| a == app_id;
    let eq_ci = |a: &str| a.to_lowercase() == lower;
    let last = |s: &str| s.rsplit('.').next().unwrap_or(s).to_lowercase();
    if app.startup_wm_class.as_deref().is_some_and(eq) {
        return Some(0);
    }
    if eq(&app.id) {
        return Some(1);
    }
    if app.startup_wm_class.as_deref().is_some_and(eq_ci) || eq_ci(&app.id) {
        return Some(2);
    }
    // reverse-DNS ids: `org.mozilla.firefox` ↔ `firefox`
    if last(&app.id) == lower || last(app_id) == app.id.to_lowercase() || last(&app.id) == last(app_id) {
        return Some(3);
    }
    if app.exec.first().and_then(|e| e.rsplit('/').next()).is_some_and(|b| b.to_lowercase() == lower) {
        return Some(4);
    }
    if app.name.to_lowercase() == lower {
        return Some(5);
    }
    None
}

/// The app a window with niri `app_id` belongs to: `StartupWMClass`, desktop id, then
/// lower-cased / reverse-DNS-suffix / executable-name / display-name heuristics, best first.
pub fn find_app_for_window<'a>(apps: &'a [App], app_id: &str) -> Option<&'a App> {
    apps.iter()
        .filter_map(|a| window_match_rank(a, app_id).map(|r| (r, a)))
        .min_by_key(|(r, _)| *r)
        .map(|(_, a)| a)
}

/// The prefix that makes a configured terminal (`"kitty"`, `"foot --server"`) run a
/// program: the command split into argv plus the terminal's "execute" flag, if it needs one.
pub fn terminal_for(command: &str) -> Option<Vec<String>> {
    let mut a = shlex::split(command).filter(|a| !a.is_empty())?;
    let flag = exec_flag(&a[0]);
    a.extend(flag.iter().map(|s| s.to_string()));
    Some(a)
}

/// The command prefix that runs a program in a terminal (for `Terminal=true` apps and
/// `>` commands): `$ASHD_TERMINAL` (a full prefix, e.g. `foot -e`), else `$TERMINAL`
/// with the right "execute" flag, else the first known terminal on `$PATH`.
pub fn terminal_command() -> Vec<String> {
    let split = |v: String| shlex::split(&v).filter(|a| !a.is_empty());
    if let Ok(v) = std::env::var("ASHD_TERMINAL")
        && let Some(a) = split(v)
    {
        return a;
    }
    if let Ok(v) = std::env::var("TERMINAL")
        && let Some(mut a) = split(v)
    {
        let flag = exec_flag(&a[0]);
        a.extend(flag.iter().map(|s| s.to_string()));
        return a;
    }
    let path: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    for cand in ["kitty", "foot", "alacritty", "wezterm", "ghostty", "konsole", "gnome-terminal", "xterm"] {
        if path.iter().any(|d| d.join(cand).is_file()) {
            let mut v = vec![cand.to_string()];
            v.extend(exec_flag(cand).iter().map(|s| s.to_string()));
            return v;
        }
    }
    vec!["xterm".into(), "-e".into()]
}

fn exec_flag(bin: &str) -> &'static [&'static str] {
    match Path::new(bin).file_name().and_then(|n| n.to_str()).unwrap_or(bin) {
        "kitty" | "foot" => &[],
        "wezterm" => &["start", "--"],
        "gnome-terminal" => &["--"],
        _ => &["-e"],
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AppsState {
    /// Desktop entries have been scanned at least once.
    pub loaded: bool,
    /// Visible apps (NoDisplay/Hidden filtered out), sorted by name.
    pub apps: Arc<Vec<App>>,
}

/// One search hit.
#[derive(Clone, Debug, PartialEq)]
pub struct Match {
    /// Index into `AppsState::apps`.
    pub index: usize,
    pub score: u32,
    /// Matched char indices in `App::name` (for highlighting).
    pub name_indices: Vec<u32>,
}

pub enum Command {
    /// Rescan desktop entries now.
    Reload,
    /// Count a launch of the app with this desktop id.
    RecordLaunch(String),
    /// Something changed in an applications dir (debounced, then rescanned).
    Changed,
}

#[derive(Clone)]
pub struct Handle {
    tx: Sender<Command>,
}

impl Handle {
    pub fn reload(&self) {
        let _ = self.tx.try_send(Command::Reload);
    }
    pub fn record_launch(&self, id: &str) {
        let _ = self.tx.try_send(Command::RecordLaunch(id.to_string()));
    }
    pub fn disconnected() -> Self {
        Self { tx: async_channel::bounded(1).0 }
    }
}

/// Spawn the `ashd-svc-apps` thread.
pub fn start() -> (Handle, Receiver<AppsState>) {
    let (tx, rx) = async_channel::unbounded();
    let (state_tx, state_rx) = async_channel::bounded(1);
    let tx2 = tx.clone();
    std::thread::Builder::new()
        .name("ashd-svc-apps".into())
        .spawn(move || smol::block_on(run(rx, tx2, state_tx, ScanEnv::from_env(), state_dir().join("apps.redb"))))
        .expect("spawn ashd-svc-apps");
    (Handle { tx }, state_rx)
}

const POLL: Duration = Duration::from_secs(10);
const DEBOUNCE: Duration = Duration::from_millis(300);

fn now_secs() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

enum Wake {
    Cmd(Option<Command>),
    Tick,
}

async fn run(commands: Receiver<Command>, self_tx: Sender<Command>, state: Sender<AppsState>, env: ScanEnv, db_path: PathBuf) {
    let history = history::History::open(&db_path);
    let mut launches: HashMap<String, (u32, u64)> = history.load();
    watch::spawn(&env.data_dirs, self_tx);
    let mut sig = watch::signature(&env.data_dirs);
    let mut apps = Arc::new(scan(&env, &launches));
    let publish = |apps: &Arc<Vec<App>>| {
        let _ = state.force_send(AppsState { loaded: true, apps: apps.clone() });
    };
    publish(&apps);

    loop {
        let wake = smol::future::or(async { Wake::Cmd(commands.recv().await.ok()) }, async {
            smol::Timer::after(POLL).await;
            Wake::Tick
        })
        .await;
        let mut rescan = false;
        let mut pending = Vec::new();
        match wake {
            Wake::Cmd(None) => return,
            Wake::Cmd(Some(c)) => pending.push(c),
            Wake::Tick => {
                let now = watch::signature(&env.data_dirs);
                if now != sig {
                    rescan = true;
                }
            }
        }
        // Debounce bursts of changes (package installs write many files).
        if pending.iter().any(|c| matches!(c, Command::Reload | Command::Changed)) {
            smol::Timer::after(DEBOUNCE).await;
        }
        while let Ok(c) = commands.try_recv() {
            pending.push(c);
        }
        let mut republish = false;
        for c in pending {
            match c {
                Command::Reload | Command::Changed => rescan = true,
                Command::RecordLaunch(id) => {
                    let entry = launches.entry(id.clone()).or_insert((0, 0));
                    entry.0 = entry.0.saturating_add(1);
                    entry.1 = now_secs();
                    let value = *entry;
                    history.store(&id, value);
                    let mut list = (*apps).clone();
                    if let Some(a) = list.iter_mut().find(|a| a.id == id) {
                        a.launch_count = value.0;
                        a.last_launch = value.1;
                    }
                    apps = Arc::new(list);
                    republish = true;
                }
            }
        }
        if rescan {
            sig = watch::signature(&env.data_dirs);
            let fresh = scan(&env, &launches);
            if fresh != *apps {
                apps = Arc::new(fresh);
                republish = true;
            }
        }
        if republish {
            publish(&apps);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    fn entry(name: &str, extra: &str) -> String {
        format!("[Desktop Entry]\nType=Application\nName={name}\nExec={} %U\n{extra}", name.to_lowercase())
    }

    fn env_for(dirs: &[&Path]) -> ScanEnv {
        ScanEnv {
            data_dirs: dirs.iter().map(|d| d.to_path_buf()).collect(),
            locales: vec!["en".into()],
            desktops: vec!["niri".into()],
            icon_theme: "hicolor".into(),
            path: vec![],
        }
    }

    #[test]
    fn terminal_apps_get_the_terminal_prefix() {
        let app = App { exec: vec!["htop".into()], terminal: true, ..Default::default() };
        let term = vec!["kitty".to_string(), "-e".to_string()];
        assert_eq!(app.launch_argv(Some(&term)), vec!["kitty", "-e", "htop"]);
        assert_eq!(app.launch_argv(None), vec!["htop"]);
    }

    #[test]
    fn scan_filters_and_dedups() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let (a, b) = (a.path(), b.path());
        write(a, "applications/firefox.desktop", &entry("Firefox", "Icon=firefox\nStartupWMClass=firefox\nKeywords=web;browser;\n"));
        write(a, "applications/hidden.desktop", &entry("Hidden", "NoDisplay=true\n"));
        write(a, "applications/deleted.desktop", &entry("Deleted", "Hidden=true\n"));
        write(a, "applications/gnome.desktop", &entry("GnomeOnly", "OnlyShowIn=GNOME;\n"));
        write(a, "applications/niri.desktop", &entry("NiriOnly", "OnlyShowIn=GNOME;niri;\n"));
        write(a, "applications/notniri.desktop", &entry("NotNiri", "NotShowIn=niri;\n"));
        write(a, "applications/gone.desktop", &entry("Gone", "TryExec=/definitely/not/here\n"));
        write(a, "applications/link.desktop", "[Desktop Entry]\nType=Link\nName=Link\nURL=https://x\n");
        write(a, "applications/sub/nested.desktop", &entry("Nested", ""));
        write(a, "applications/htop.desktop", &entry("Htop", "Terminal=true\n"));
        // lower priority: duplicate id is ignored, masked id stays masked
        write(b, "applications/firefox.desktop", &entry("Firefox Old", ""));
        write(b, "applications/deleted.desktop", &entry("Deleted", ""));
        write(b, "applications/other.desktop", &entry("Other", ""));

        let apps = scan(&env_for(&[a, b]), &HashMap::new());
        let names: Vec<_> = apps.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, ["Firefox", "Htop", "Nested", "NiriOnly", "Other"]);
        let ff = &apps[0];
        assert_eq!(ff.id, "firefox");
        assert_eq!(ff.exec, ["firefox"]);
        assert_eq!(ff.keywords, ["web", "browser"]);
        assert_eq!(ff.startup_wm_class.as_deref(), Some("firefox"));
        assert!(apps[1].terminal);
        assert_eq!(apps[2].id, "sub-nested");
    }

    #[test]
    fn icons_resolve_with_fallbacks() {
        let a = tempfile::tempdir().unwrap();
        let a = a.path();
        write(a, "icons/hicolor/48x48/apps/themed.png", "x");
        write(a, "icons/hicolor/scalable/apps/vector.svg", "<svg/>");
        write(a, "pixmaps/pix.png", "x");
        write(a, "pixmaps/old.xpm", "x");
        write(a, "abs/direct.svg", "<svg/>");
        for (n, icon) in [("t", "themed"), ("v", "vector"), ("p", "pix"), ("x", "old"), ("n", "missing")] {
            write(a, &format!("applications/{n}.desktop"), &entry(n, &format!("Icon={icon}\n")));
        }
        let abs = a.join("abs/direct.svg");
        write(a, "applications/d.desktop", &entry("d", &format!("Icon={}\n", abs.display())));
        let apps = scan(&env_for(&[a]), &HashMap::new());
        let icon = |id: &str| apps.iter().find(|x| x.id == id).unwrap().icon_path.clone();
        assert_eq!(icon("t"), Some(a.join("icons/hicolor/48x48/apps/themed.png")));
        assert_eq!(icon("v"), Some(a.join("icons/hicolor/scalable/apps/vector.svg")));
        assert_eq!(icon("p"), Some(a.join("pixmaps/pix.png")));
        assert_eq!(icon("x"), None, "xpm is not drawable");
        assert_eq!(icon("n"), None);
        assert_eq!(icon("d"), Some(abs));
    }

    fn fixture_apps() -> Vec<App> {
        fn mk(id: &str, name: &str, extra: impl FnOnce(&mut App)) -> App {
            let mut a = App { id: id.into(), name: name.into(), exec: vec![id.into()], ..Default::default() };
            extra(&mut a);
            a
        }
        vec![
            mk("firefox", "Firefox", |a| a.keywords = vec!["browser".into(), "web".into()]),
            mk("kitty", "kitty", |a| a.generic_name = Some("Terminal".into())),
            mk("org.gnome.Nautilus", "Files", |a| a.comment = Some("Access and organize files".into())),
            mk("firewall-config", "Firewall", |_| {}),
            mk("fm", "fm", |_| {}),
        ]
    }

    #[test]
    fn search_ranks_and_highlights() {
        let apps = fixture_apps();
        let hits = search(&apps, "fir", 10);
        let names: Vec<_> = hits.iter().map(|h| apps[h.index].name.as_str()).collect();
        assert_eq!(names, ["Firefox", "Firewall"]);
        assert_eq!(hits[0].name_indices, [0, 1, 2]);
        // generic name / keyword / id matches, no name indices
        let hits = search(&apps, "terminal", 10);
        assert_eq!(apps[hits[0].index].name, "kitty");
        assert!(hits[0].name_indices.is_empty());
        let hits = search(&apps, "browser", 10);
        assert_eq!(apps[hits[0].index].name, "Firefox");
        assert!(search(&apps, "qzxqzx", 10).is_empty());
        assert_eq!(search(&apps, "", 3).len(), 3);
        assert_eq!(search(&apps, "f", 2).len(), 2);
    }

    #[test]
    fn frecency_orders_empty_query_and_breaks_ties() {
        let mut apps = fixture_apps();
        apps[1].launch_count = 5;
        apps[1].last_launch = now_secs();
        apps[4].launch_count = 9;
        apps[4].last_launch = now_secs();
        let hits = search(&apps, "", 5);
        assert_eq!(apps[hits[0].index].name, "fm");
        assert_eq!(apps[hits[1].index].name, "kitty");
        // a heavily used app beats an otherwise equal match
        let mut apps = vec![
            App { id: "a".into(), name: "Term One".into(), exec: vec!["a".into()], ..Default::default() },
            App { id: "b".into(), name: "Term Two".into(), exec: vec!["b".into()], launch_count: 40, last_launch: now_secs(), ..Default::default() },
        ];
        let hits = search(&apps, "term", 5);
        assert_eq!(apps[hits[0].index].id, "b");
        apps[1].launch_count = 0;
        let hits = search(&apps, "term", 5);
        assert_eq!(apps[hits[0].index].id, "a");
    }

    #[test]
    fn windows_map_to_entries() {
        let mk = |id: &str, wm: Option<&str>, name: &str, exec: &str| App {
            id: id.into(),
            name: name.into(),
            startup_wm_class: wm.map(str::to_string),
            exec: vec![exec.into()],
            ..Default::default()
        };
        let apps = vec![
            mk("org.mozilla.firefox", Some("firefox"), "Firefox", "firefox"),
            mk("org.gnome.Nautilus", None, "Files", "nautilus"),
            mk("kitty", None, "kitty", "kitty"),
            mk("code", Some("Code"), "Visual Studio Code", "code"),
            mk("signal-desktop", None, "Signal", "/opt/Signal/signal-desktop"),
        ];
        let id = |app_id: &str| find_app_for_window(&apps, app_id).map(|a| a.id.as_str());
        assert_eq!(id("firefox"), Some("org.mozilla.firefox"));
        assert_eq!(id("org.gnome.Nautilus"), Some("org.gnome.Nautilus"));
        assert_eq!(id("nautilus"), Some("org.gnome.Nautilus"));
        assert_eq!(id("kitty"), Some("kitty"));
        assert_eq!(id("code"), Some("code"));
        assert_eq!(id("Code"), Some("code"));
        assert_eq!(id("Signal"), Some("signal-desktop"));
        assert_eq!(id("signal-desktop"), Some("signal-desktop"));
        assert_eq!(id("unknown-thing"), None);
        assert_eq!(id(""), None);
    }

    #[test]
    fn service_scans_records_launches_and_notices_changes() {
        let data = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        write(data.path(), "applications/a.desktop", &entry("Alpha", ""));
        let env = env_for(&[data.path()]);
        let db = state_dir.path().join("apps.redb");

        let (tx, rx) = async_channel::unbounded();
        let (state_tx, state_rx) = async_channel::bounded(1);
        let (env2, db2) = (env.clone(), db.clone());
        std::thread::spawn(move || smol::block_on(run(rx, tx.clone(), state_tx, env2, db2)));
        let wait = |pred: &dyn Fn(&AppsState) -> bool| -> AppsState {
            smol::block_on(async {
                loop {
                    let s = smol::future::or(async { state_rx.recv().await.ok() }, async {
                        smol::Timer::after(Duration::from_secs(8)).await;
                        None
                    })
                    .await
                    .expect("timed out waiting for apps state");
                    if pred(&s) {
                        return s;
                    }
                }
            })
        };
        let first = wait(&|s| s.loaded);
        assert_eq!(first.apps.len(), 1);

        // a new desktop file shows up (inotify or poll)
        write(data.path(), "applications/b.desktop", &entry("Beta", ""));
        let s = wait(&|s| s.apps.len() == 2);
        assert_eq!(s.apps[1].name, "Beta");
    }

    #[test]
    fn record_launch_is_counted_and_persisted() {
        let data = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        write(data.path(), "applications/a.desktop", &entry("Alpha", ""));
        let env = env_for(&[data.path()]);
        let db = state.path().join("apps.redb");
        let (tx, rx) = async_channel::unbounded();
        let (state_tx, state_rx) = async_channel::bounded(1);
        let (db2, tx2) = (db.clone(), tx.clone());
        std::thread::spawn(move || smol::block_on(run(rx, tx2, state_tx, env, db2)));
        let s = smol::block_on(state_rx.recv()).unwrap();
        assert_eq!(s.apps[0].launch_count, 0);
        tx.try_send(Command::RecordLaunch("a".into())).unwrap();
        let s = smol::block_on(async {
            loop {
                let s = state_rx.recv().await.unwrap();
                if s.apps[0].launch_count == 1 {
                    return s;
                }
            }
        });
        assert!(s.apps[0].last_launch > 0);
    }

    #[test]
    fn history_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.redb");
        {
            let h = history::History::open(&path);
            h.store("a", (3, 99));
            h.store("b", (1, 5));
        }
        let h = history::History::open(&path);
        let m = h.load();
        assert_eq!(m.get("a"), Some(&(3, 99)));
        assert_eq!(m.len(), 2);
    }
}
