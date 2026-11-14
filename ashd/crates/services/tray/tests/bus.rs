//! The tray service against mock tray apps on a *private* `dbus-daemon` (never the user's
//! session bus). Skipped when `dbus-daemon` is not on PATH (outside the dev shell).
//!
//! The mock (`support/mock.rs`) logs every method call it receives, so the command tests
//! assert on what actually reached the app.

#[path = "support/mock.rs"]
mod mock;

use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Child, Command as Proc, Stdio},
    time::Duration,
};

use async_channel::Receiver;
use mock::{Form, ItemBuilder, MItem, MockItem, solid};
use ashd_svc_tray::{Command, Config, Handle, MenuKind, Orientation, Status, Toggle, TrayItem, TrayState, start_with};
use tempfile::TempDir;

struct Bus {
    child: Child,
    address: String,
}

impl Bus {
    fn start() -> Option<Bus> {
        let mut child = Proc::new("dbus-daemon").args(["--session", "--print-address", "--nofork"]).stdout(Stdio::piped()).spawn().ok()?;
        let mut line = String::new();
        BufReader::new(child.stdout.take()?).read_line(&mut line).ok()?;
        Some(Bus { child, address: line.trim().to_string() })
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Newest-state probe: remembers the last snapshot, so a state reached *before* `wait`
/// is called still counts.
struct Probe {
    rx: Receiver<TrayState>,
    last: TrayState,
}

impl Probe {
    fn wait(&mut self, what: &str, pred: impl Fn(&TrayState) -> bool) -> TrayState {
        while let Ok(s) = self.rx.try_recv() {
            self.last = s;
        }
        if pred(&self.last) {
            return self.last.clone();
        }
        let rx = self.rx.clone();
        let s = smol::block_on(smol::future::or(
            async {
                loop {
                    match rx.recv().await {
                        Ok(s) if pred(&s) => return s,
                        Ok(s) => self.last = s,
                        Err(_) => panic!("state channel closed waiting for {what}"),
                    }
                }
            },
            async {
                smol::Timer::after(Duration::from_secs(10)).await;
                panic!("timed out waiting for: {what}")
            },
        ));
        self.last = s.clone();
        s
    }
}

fn wait_log(item: &MockItem, want: &str) {
    wait_log_for(item, want, Duration::from_secs(5));
}

fn wait_log_for(item: &MockItem, want: &str, limit: Duration) {
    for _ in 0..(limit.as_millis() / 25) {
        if item.log().iter().any(|l| l == want) {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("{} never saw `{want}`; saw {:?}", item.bus_name, item.log());
}

struct Fixture {
    bus: Bus,
    _cache: TempDir,
    handle: Handle,
    probe: Probe,
}

fn service(bus: &Bus) -> (Handle, Probe, TempDir) {
    let cache = tempfile::tempdir().unwrap();
    let (handle, rx) = start_with(Config { address: Some(bus.address.clone()), cache_dir: Some(cache.path().join("tray")) });
    (handle, Probe { rx, last: TrayState::default() }, cache)
}

/// Bus + service, owning the watcher.
fn fixture() -> Option<Fixture> {
    let Some(bus) = Bus::start() else {
        eprintln!("dbus-daemon not found: skipping");
        return None;
    };
    let (handle, mut probe, cache) = service(&bus);
    probe.wait("watcher", |s| s.available && s.is_watcher);
    Some(Fixture { bus, _cache: cache, handle, probe })
}

fn app(f: &Fixture, b: ItemBuilder) -> MockItem {
    smol::block_on(b.serve(&f.bus.address)).unwrap()
}

fn register(item: &MockItem, form: Form) {
    smol::block_on(item.register(form)).unwrap();
}

fn item_of<'a>(s: &'a TrayState, app_id: &str) -> &'a TrayItem {
    s.items.iter().find(|i| i.app_id == app_id).unwrap_or_else(|| panic!("no item `{app_id}` in {:#?}", s.items))
}

/// A client call on the watcher object (the way an app or another host sees it).
fn watcher_items(bus: &Bus) -> Vec<String> {
    let conn = smol::block_on(zbus::connection::Builder::address(bus.address.as_str()).unwrap().build()).unwrap();
    let reply = smol::block_on(conn.call_method(
        Some(mock::WATCHER_NAME),
        mock::WATCHER_PATH,
        Some("org.freedesktop.DBus.Properties"),
        "Get",
        &("org.kde.StatusNotifierWatcher", "RegisteredStatusNotifierItems"),
    ))
    .unwrap();
    let v: zbus::zvariant::OwnedValue = reply.body().deserialize().unwrap();
    v.try_into().unwrap()
}

fn watcher_prop<T: TryFrom<zbus::zvariant::OwnedValue>>(bus: &Bus, name: &str) -> T
where
    T::Error: std::fmt::Debug,
{
    let conn = smol::block_on(zbus::connection::Builder::address(bus.address.as_str()).unwrap().build()).unwrap();
    let reply = smol::block_on(conn.call_method(
        Some(mock::WATCHER_NAME),
        mock::WATCHER_PATH,
        Some("org.freedesktop.DBus.Properties"),
        "Get",
        &("org.kde.StatusNotifierWatcher", name),
    ))
    .unwrap();
    let v: zbus::zvariant::OwnedValue = reply.body().deserialize().unwrap();
    v.try_into().unwrap()
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[test]
fn unavailable_without_a_bus() {
    let (_handle, rx) = start_with(Config { address: Some("unix:path=/nonexistent/ashd-tray-test-bus".into()), cache_dir: None });
    let mut probe = Probe { rx, last: TrayState { available: true, ..Default::default() } };
    let s = probe.wait("initial unavailable state", |s| !s.available);
    assert!(s.items.is_empty() && !s.is_watcher && s.menu.is_none());
}

#[test]
fn owns_the_watcher_and_answers_its_properties() {
    let Some(f) = fixture() else { return };
    assert!(watcher_prop::<bool>(&f.bus, "IsStatusNotifierHostRegistered"));
    assert_eq!(watcher_prop::<i32>(&f.bus, "ProtocolVersion"), 0);
    assert!(watcher_items(&f.bus).is_empty());
}

#[test]
fn registers_a_bus_name_and_an_object_path() {
    let Some(mut f) = fixture() else { return };
    let a = app(&f, ItemBuilder::new("alpha").title("Alpha").icon_name("alpha-icon").tooltip("Alpha tip", "all quiet"));
    let b = app(&f, ItemBuilder::new("beta").title("Beta").path("/org/ayatana/NotificationItem/beta"));
    register(&a, Form::BusName);
    register(&b, Form::Path);
    let s = f.probe.wait("both items", |s| s.items.len() == 2);

    let alpha = item_of(&s, "alpha");
    assert_eq!(alpha.id, format!("{}/StatusNotifierItem", a.bus_name));
    assert_eq!(alpha.bus_name, a.bus_name);
    assert_eq!(alpha.object_path, "/StatusNotifierItem");
    assert_eq!(alpha.title, "Alpha");
    assert_eq!(alpha.icon_name, "alpha-icon");
    assert_eq!(alpha.tooltip.title, "Alpha tip");
    assert_eq!(alpha.tooltip.description, "all quiet");
    assert_eq!(alpha.label(), "Alpha tip");
    assert_eq!(alpha.status, Status::Active);

    let beta = item_of(&s, "beta");
    assert!(beta.id.starts_with(':'), "registered by path: the sender's unique name, {}", beta.id);
    assert!(beta.id.ends_with("/org/ayatana/NotificationItem/beta"));
    assert_eq!(beta.object_path, "/org/ayatana/NotificationItem/beta");

    // the watcher object lists both, in the `bus/path` spelling hosts parse
    let listed = watcher_items(&f.bus);
    assert_eq!(listed.len(), 2);
    assert!(listed.contains(&alpha.id) && listed.contains(&beta.id), "{listed:?}");

    // registering again changes nothing
    register(&a, Form::BusName);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(watcher_items(&f.bus).len(), 2);
    assert_eq!(f.probe.wait("still two", |s| s.items.len() == 2).items.len(), 2);
}

#[test]
fn pixmaps_become_png_files() {
    let Some(mut f) = fixture() else { return };
    // ARGB: A=255 R=10 G=20 B=30
    let pix = vec![solid(16, [255, 1, 1, 1]), solid(32, [255, 10, 20, 30]), solid(64, [255, 2, 2, 2])];
    let a = app(&f, ItemBuilder::new("pix").pixmaps(pix));
    register(&a, Form::BusName);
    let s = f.probe.wait("pixmap item", |s| s.items.len() == 1 && s.items[0].icon.is_some());
    let path: PathBuf = s.items[0].icon.clone().unwrap();
    assert!(path.starts_with(f._cache.path()), "cached under the configured dir: {}", path.display());
    let img = image::open(&path).unwrap().to_rgba8();
    assert_eq!(img.dimensions(), (32, 32), "the size nearest to 32");
    assert_eq!(img.get_pixel(3, 3).0, [10, 20, 30, 255], "ARGB → RGBA");

    // a new pixmap → a new file; the item refreshes on NewIcon
    smol::block_on(a.set_pixmaps(vec![solid(48, [128, 200, 100, 50])])).unwrap();
    let s = f.probe.wait("new icon", |s| s.items.first().and_then(|i| i.icon.clone()).is_some_and(|p| p != path));
    let img = image::open(s.items[0].icon.as_ref().unwrap()).unwrap().to_rgba8();
    assert_eq!(img.dimensions(), (48, 48));
    assert_eq!(img.get_pixel(0, 0).0, [200, 100, 50, 128]);
}

#[test]
fn icon_names_resolve_through_icon_theme_path() {
    let Some(mut f) = fixture() else { return };
    let themes = tempfile::tempdir().unwrap();
    let icon = themes.path().join("hicolor/48x48/status/tray-test-icon.png");
    std::fs::create_dir_all(icon.parent().unwrap()).unwrap();
    image::RgbaImage::new(8, 8).save(&icon).unwrap();
    let a = app(&f, ItemBuilder::new("themed").icon_name("tray-test-icon").icon_theme_path(themes.path().to_str().unwrap()));
    register(&a, Form::BusName);
    let s = f.probe.wait("themed icon", |s| s.items.first().is_some_and(|i| i.icon.is_some()));
    assert_eq!(s.items[0].icon.as_ref(), Some(&icon));
    // an unresolvable name and no pixmap: no icon (the bar draws its generic glyph)
    let b = app(&f, ItemBuilder::new("nameless").icon_name("definitely-not-an-icon-xyzzy"));
    register(&b, Form::BusName);
    let s = f.probe.wait("second item", |s| s.items.len() == 2);
    assert_eq!(item_of(&s, "nameless").icon, None);
}

#[test]
fn status_title_tooltip_and_attention_follow_signals() {
    let Some(mut f) = fixture() else { return };
    let a = app(&f, ItemBuilder::new("chat").category("Communications").title("Chat").status("Passive").pixmaps(vec![solid(16, [255, 9, 9, 9])]));
    register(&a, Form::BusName);
    let s = f.probe.wait("passive", |s| s.items.len() == 1);
    assert_eq!(s.items[0].status, Status::Passive);
    assert_eq!(s.items[0].category, ashd_svc_tray::Category::Communications);

    smol::block_on(a.set_status("Active")).unwrap();
    f.probe.wait("active", |s| s.items[0].status == Status::Active);
    smol::block_on(a.set_title("Chat (2)")).unwrap();
    f.probe.wait("title", |s| s.items[0].title == "Chat (2)");
    smol::block_on(a.set_tooltip("2 unread", "from ann")).unwrap();
    let s = f.probe.wait("tooltip", |s| s.items[0].tooltip.title == "2 unread");
    assert_eq!(s.items[0].tooltip.description, "from ann");

    // attention: the attention icon is what gets displayed
    let attention = f._cache.path().join("attention.png");
    image::RgbaImage::new(4, 4).save(&attention).unwrap();
    smol::block_on(a.set_attention_icon(attention.to_str().unwrap())).unwrap();
    smol::block_on(a.set_status("NeedsAttention")).unwrap();
    let s = f.probe.wait("attention", |s| s.items[0].status == Status::NeedsAttention && s.items[0].attention_icon.is_some());
    assert_eq!(s.items[0].display_icon(), Some(&attention));
    smol::block_on(a.set_status("Active")).unwrap();
    let s = f.probe.wait("back to normal", |s| s.items[0].status == Status::Active);
    assert_eq!(s.items[0].display_icon(), s.items[0].icon.as_ref());
}

#[test]
fn the_freedesktop_spelling_is_understood() {
    let Some(mut f) = fixture() else { return };
    let a = app(&f, ItemBuilder::new("fdo").title("Fdo app").fdo());
    register(&a, Form::BusName);
    let s = f.probe.wait("fdo item", |s| s.items.len() == 1);
    assert_eq!(s.items[0].title, "Fdo app");
    smol::block_on(a.set_title("Renamed")).unwrap();
    f.probe.wait("fdo signal", |s| s.items[0].title == "Renamed");
    f.handle.activate(&s.items[0].id, 1, 2);
    wait_log(&a, "Activate(1,2)");
}

#[test]
fn an_item_is_dropped_when_its_owner_leaves() {
    let Some(mut f) = fixture() else { return };
    let a = app(&f, ItemBuilder::new("stays").title("Stays"));
    let b = app(&f, ItemBuilder::new("goes").title("Goes"));
    register(&a, Form::BusName);
    register(&b, Form::Path);
    f.probe.wait("two", |s| s.items.len() == 2);
    smol::block_on(b.quit());
    let s = f.probe.wait("one", |s| s.items.len() == 1);
    assert_eq!(s.items[0].app_id, "stays");
    // the watcher's list follows
    for _ in 0..100 {
        if watcher_items(&f.bus).len() == 1 {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("watcher still lists {:?}", watcher_items(&f.bus));
}

#[test]
fn a_registration_without_the_interface_is_dropped() {
    let Some(mut f) = fixture() else { return };
    // a plain connection that registers but exports no StatusNotifierItem
    let conn = smol::block_on(zbus::connection::Builder::address(f.bus.address.as_str()).unwrap().build()).unwrap();
    smol::block_on(conn.call_method(
        Some(mock::WATCHER_NAME),
        mock::WATCHER_PATH,
        Some("org.kde.StatusNotifierWatcher"),
        "RegisterStatusNotifierItem",
        &("/nothing/here",),
    ))
    .unwrap();
    // an invalid argument is refused outright
    assert!(
        smol::block_on(conn.call_method(
            Some(mock::WATCHER_NAME),
            mock::WATCHER_PATH,
            Some("org.kde.StatusNotifierWatcher"),
            "RegisterStatusNotifierItem",
            &("not a name!",),
        ))
        .is_err()
    );
    // it never shows up, and a real item still works
    let a = app(&f, ItemBuilder::new("real"));
    register(&a, Form::BusName);
    let s = f.probe.wait("real item", |s| s.items.len() == 1);
    assert_eq!(s.items[0].app_id, "real");
}

#[test]
fn commands_reach_the_item() {
    let Some(mut f) = fixture() else { return };
    let a = app(&f, ItemBuilder::new("cmd"));
    register(&a, Form::BusName);
    let s = f.probe.wait("item", |s| s.items.len() == 1);
    let id = s.items[0].id.clone();
    f.handle.activate(&id, 10, 20);
    wait_log(&a, "Activate(10,20)");
    f.handle.secondary_activate(&id, 3, 4);
    wait_log(&a, "SecondaryActivate(3,4)");
    f.handle.context_menu(&id, 5, 6);
    wait_log(&a, "ContextMenu(5,6)");
    f.handle.scroll(&id, -120, Orientation::Vertical);
    wait_log(&a, "Scroll(-120,vertical)");
    f.handle.scroll(&id, 15, Orientation::Horizontal);
    wait_log(&a, "Scroll(15,horizontal)");
    // unknown ids are ignored
    f.handle.activate("nope", 0, 0);
}

fn demo_menu() -> Vec<MItem> {
    vec![
        MItem::new(1, "_Open").icon("folder").shortcut(&["Control", "O"]),
        MItem::separator(2),
        MItem::new(3, "Mute notifications").check(false),
        MItem::new(4, "Dark").radio(true),
        MItem::new(5, "Light").radio(false),
        MItem::new(6, "Unavailable").disabled(),
        MItem::new(7, "Secret").hidden(),
        MItem::new(8, "Status").sub(vec![MItem::new(9, "Online").radio(true), MItem::new(10, "Away").radio(false)]),
        MItem::new(11, "Lazy").lazy(vec![MItem::new(12, "Filled on demand")]),
        MItem::separator(13),
        MItem::new(14, "_Quit"),
    ]
}

#[test]
fn menu_layout_clicks_and_updates() {
    let Some(mut f) = fixture() else { return };
    let a = app(&f, ItemBuilder::new("menu").menu(demo_menu()));
    register(&a, Form::BusName);
    let s = f.probe.wait("item with menu", |s| s.items.len() == 1);
    assert_eq!(s.items[0].menu_path.as_deref(), Some("/MenuBar"));
    let id = s.items[0].id.clone();
    assert!(s.menu.is_none());

    f.handle.open_menu(&id);
    let s = f.probe.wait("menu", |s| s.menu.is_some());
    wait_log(&a, "AboutToShow(0)");
    let menu = s.menu.unwrap();
    assert_eq!(menu.item, id);
    let root = &menu.root;
    assert_eq!(root.id, 0);
    let labels: Vec<_> = root.children.iter().map(|c| (c.id, c.label.as_str())).collect();
    assert_eq!(labels[0], (1, "Open"), "mnemonic stripped");
    assert_eq!(root.children.last().unwrap().label, "Quit");
    assert_eq!(root.children[0].shortcut.as_deref(), Some("Ctrl+O"));
    assert_eq!(root.children[1].kind, MenuKind::Separator);
    assert_eq!(root.children[2].toggle, Toggle::Check(false));
    assert_eq!(root.children[3].toggle, Toggle::Radio(true));
    assert_eq!(root.children[4].toggle, Toggle::Radio(false));
    assert!(!root.children[5].enabled);
    assert!(!root.children[6].visible);
    assert_eq!(root.visible_children().count(), root.children.len() - 1);
    let status = root.find(8).unwrap();
    assert!(status.is_submenu());
    assert_eq!(status.children.iter().map(|c| c.label.as_str()).collect::<Vec<_>>(), ["Online", "Away"]);
    // a submenu delivered only after AboutToShow(11) was grafted in
    let lazy = root.find(11).unwrap();
    assert_eq!(lazy.children.len(), 1, "{lazy:?}");
    assert_eq!(lazy.children[0].label, "Filled on demand");
    assert!(a.log().iter().any(|l| l == "AboutToShow(11)"), "{:?}", a.log());

    // a click is delivered as the `clicked` event …
    f.handle.menu_click(&id, 1);
    wait_log(&a, "Event(1,clicked)");
    // … and a checkable entry flips and the open menu follows the app's update
    assert_eq!(a.toggle_state(3), Some(0));
    f.handle.menu_click(&id, 3);
    wait_log(&a, "Event(3,clicked)");
    let s = f.probe.wait("toggled", |s| s.menu.as_ref().and_then(|m| m.root.find(3)).is_some_and(|n| n.toggle == Toggle::Check(true)));
    assert_eq!(s.menu.unwrap().seq, menu.seq, "a refresh keeps the seq");

    // the app rebuilds its menu: LayoutUpdated
    smol::block_on(a.replace_menu(vec![MItem::new(20, "Brand new")])).unwrap();
    let s = f.probe.wait("replaced", |s| s.menu.as_ref().is_some_and(|m| m.root.children.len() == 1));
    assert_eq!(s.menu.unwrap().root.children[0].label, "Brand new");

    // closing forgets it (and tells the app)
    f.handle.close_menu();
    f.probe.wait("closed", |s| s.menu.is_none());
    wait_log(&a, "Event(0,closed)");

    // opening again is a new seq
    f.handle.open_menu(&id);
    let s = f.probe.wait("reopened", |s| s.menu.as_ref().is_some_and(|m| m.seq > menu.seq));
    assert_eq!(s.menu.unwrap().root.children[0].label, "Brand new");
}

#[test]
fn menu_icons_are_resolved_and_inline_png_is_cached() {
    let Some(mut f) = fixture() else { return };
    let themes = tempfile::tempdir().unwrap();
    let icon = themes.path().join("hicolor/32x32/apps/menu-icon-test.png");
    std::fs::create_dir_all(icon.parent().unwrap()).unwrap();
    image::RgbaImage::new(8, 8).save(&icon).unwrap();
    let a = app(&f, ItemBuilder::new("menu-icons").icon_theme_path(themes.path().to_str().unwrap()).menu(vec![MItem::new(1, "With icon").icon("menu-icon-test"), MItem::new(2, "No icon")]));
    register(&a, Form::BusName);
    let s = f.probe.wait("item", |s| s.items.len() == 1);
    f.handle.open_menu(&s.items[0].id);
    let s = f.probe.wait("menu", |s| s.menu.is_some());
    let root = s.menu.unwrap().root;
    assert_eq!(root.children[0].icon.as_ref(), Some(&icon));
    assert_eq!(root.children[1].icon, None);
}

#[test]
fn a_menu_only_item_falls_back_from_activate_to_its_menu() {
    let Some(mut f) = fixture() else { return };
    let a = app(&f, ItemBuilder::new("menuonly").item_is_menu(true).activate_fails().menu(vec![MItem::new(1, "Only entry")]));
    register(&a, Form::BusName);
    let s = f.probe.wait("item", |s| s.items.len() == 1);
    assert!(s.items[0].item_is_menu);
    f.handle.activate(&s.items[0].id, 0, 0);
    wait_log(&a, "Activate(0,0)");
    let s = f.probe.wait("menu from the failed Activate", |s| s.menu.is_some());
    assert_eq!(s.menu.unwrap().root.children[0].label, "Only entry");
}

#[test]
fn opening_a_menu_of_an_item_without_dbusmenu_asks_the_app_to_draw_it() {
    let Some(mut f) = fixture() else { return };
    let a = app(&f, ItemBuilder::new("own-menu"));
    register(&a, Form::BusName);
    let s = f.probe.wait("item", |s| s.items.len() == 1);
    f.handle.open_menu(&s.items[0].id);
    wait_log(&a, "ContextMenu(0,0)");
    assert!(f.probe.wait("no menu", |s| s.menu.is_none()).menu.is_none());
}

#[test]
fn a_hung_menu_does_not_stall_the_service() {
    let Some(mut f) = fixture() else { return };
    let a = app(&f, ItemBuilder::new("hung").menu(vec![MItem::new(1, "x")]).hang_menu());
    let other = app(&f, ItemBuilder::new("other").title("Other"));
    register(&a, Form::BusName);
    register(&other, Form::BusName);
    let s = f.probe.wait("both", |s| s.items.len() == 2);
    f.handle.open_menu(&item_of(&s, "hung").id);
    // while the menu is being fetched everything else keeps working
    smol::block_on(other.set_title("Still alive")).unwrap();
    f.probe.wait("other updates", |s| s.items.iter().any(|i| i.title == "Still alive"));
    assert!(f.probe.wait("no menu yet", |s| s.menu.is_none()).menu.is_none());
    // the deadline passes: no menu, and the app is asked to draw its own
    wait_log_for(&a, "ContextMenu(0,0)", Duration::from_secs(15));
    assert!(f.probe.wait("still no menu", |s| s.menu.is_none()).menu.is_none());
}

// ── watcher / host modes ──────────────────────────────────────────────────────

#[test]
fn another_watcher_is_never_replaced_and_is_consumed_as_a_host() {
    let Some(bus) = Bus::start() else {
        eprintln!("dbus-daemon not found: skipping");
        return;
    };
    // another shell owns the name first and already knows an item
    let (foreign, items, hosts) = smol::block_on(mock::ForeignWatcher::start(&bus.address)).unwrap();
    let early = smol::block_on(ItemBuilder::new("early").title("Early").serve(&bus.address)).unwrap();
    smol::block_on(early.register(Form::BusName)).unwrap();
    assert_eq!(items.lock().unwrap().len(), 1);

    let (handle, mut probe, _cache) = service(&bus);
    let s = probe.wait("host mode", |s| s.available && s.items.len() == 1);
    assert!(!s.is_watcher, "the existing owner keeps the name");
    assert_eq!(s.items[0].title, "Early", "the watcher's existing list is consumed");
    // the host registered itself with that watcher
    for _ in 0..100 {
        if !hosts.lock().unwrap().is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let h = hosts.lock().unwrap().clone();
    assert_eq!(h.len(), 1);
    assert!(h[0].starts_with("org.kde.StatusNotifierHost-"), "{h:?}");

    // items registering later arrive through the signal
    let late = smol::block_on(ItemBuilder::new("late").title("Late").serve(&bus.address)).unwrap();
    smol::block_on(late.register(Form::Path)).unwrap();
    let s = probe.wait("signal item", |s| s.items.len() == 2);
    assert_eq!(item_of(&s, "late").title, "Late");

    // commands work the same
    handle.activate(&item_of(&s, "late").id, 7, 8);
    wait_log(&late, "Activate(7,8)");

    // the foreign watcher goes away: the queued request is promoted, the service takes over
    drop(foreign);
    let s = probe.wait("takeover", |s| s.is_watcher);
    assert_eq!(s.items.len(), 2, "known items survive the takeover");
    assert_eq!(watcher_items(&bus).len(), 2, "…and are listed by the new watcher");
    // apps re-registering (they do when the name appears) do not duplicate
    smol::block_on(early.register(Form::BusName)).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(probe.wait("no dupes", |s| s.is_watcher).items.len(), 2);

    // an app that never registers (it did with the previous watcher) is found on the bus
    // when a service becomes the watcher
    drop(handle);
}

#[test]
fn takeover_discovers_unregistered_items_on_the_bus() {
    let Some(bus) = Bus::start() else {
        eprintln!("dbus-daemon not found: skipping");
        return;
    };
    let (foreign, _items, _hosts) = smol::block_on(mock::ForeignWatcher::start(&bus.address)).unwrap();
    // exports the item but never registered with anyone
    let orphan = smol::block_on(ItemBuilder::new("orphan").title("Orphan").serve(&bus.address)).unwrap();
    let (_handle, mut probe, _cache) = service(&bus);
    let s = probe.wait("host mode", |s| s.available);
    assert!(!s.is_watcher && s.items.is_empty());
    drop(foreign);
    let s = probe.wait("takeover finds it", |s| s.is_watcher && s.items.len() == 1);
    assert_eq!(s.items[0].app_id, "orphan");
    assert_eq!(s.items[0].id, format!("{}/StatusNotifierItem", orphan.bus_name));
    assert_eq!(watcher_items(&bus), vec![s.items[0].id.clone()]);
}

#[test]
fn commands_on_a_disconnected_handle_are_harmless() {
    let h = Handle::disconnected();
    h.activate("x", 0, 0);
    h.open_menu("x");
    h.send(Command::CloseMenu);
}
