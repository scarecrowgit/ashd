//! A scriptable mock tray app for the nested session (never the user's bus).
//!
//!     cargo run -p ashd-svc-tray --example mock_tray -- [--bus ADDRESS] [--items N]
//!
//! Serves two tray items on the session bus (`$DBUS_SESSION_BUS_ADDRESS`, i.e. the nested
//! session's private one) and registers them with whoever owns
//! `org.kde.StatusNotifierWatcher` (retrying until one does):
//!
//! * `pixmap` — an `IconPixmap` only (a generated disc), tooltip, a plain `Activate`;
//! * `named` — an `IconName` resolved through its own `IconThemePath`, a dbusmenu with a
//!   check item, radio items, a separator, a disabled row, a submenu, a lazy submenu and
//!   Quit.
//!
//! Commands on stdin (one per line); calls the shell sends are printed as `call <item> …`:
//!
//!     status <pixmap|named> <Passive|Active|NeedsAttention>
//!     title <item> <text…>
//!     tooltip <item> <title…>
//!     color <item> <r> <g> <b>        repaint the pixmap / icon file (pixmap item only)
//!     extra <n>                       add n more passive-capable items (crowding the bar)
//!     quit <item>                     that item leaves the bus
//!     quit                            exit

#[path = "../tests/support/mock.rs"]
mod mock;

use std::{io::BufRead, path::PathBuf, time::Duration};

use mock::{Form, ItemBuilder, MItem, MockItem, Pixmaps};

/// An `edge × edge` ARGB disc with a soft edge.
fn disc(edge: i32, rgb: [u8; 3]) -> Pixmaps {
    let mut data = Vec::with_capacity((edge * edge * 4) as usize);
    let c = (edge as f32 - 1.0) / 2.0;
    for y in 0..edge {
        for x in 0..edge {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            let a = ((c + 0.5 - d) * 2.0).clamp(0.0, 1.0);
            data.extend_from_slice(&[(a * 255.0) as u8, rgb[0], rgb[1], rgb[2]]);
        }
    }
    vec![(edge, edge, data)]
}

/// A ring PNG for the `named` item, in a private icon theme directory.
fn write_ring(dir: &std::path::Path) -> std::io::Result<()> {
    let target = dir.join("hicolor/48x48/status");
    std::fs::create_dir_all(&target)?;
    let img = image::RgbaImage::from_fn(48, 48, |x, y| {
        let d = ((x as f32 - 23.5).powi(2) + (y as f32 - 23.5).powi(2)).sqrt();
        if (14.0..21.0).contains(&d) { image::Rgba([232, 232, 224, 255]) } else { image::Rgba([0, 0, 0, 0]) }
    });
    img.save(target.join("mock-tray-named.png")).map_err(std::io::Error::other)
}

fn menu() -> Vec<MItem> {
    vec![
        MItem::new(1, "_Open window").icon("mock-tray-named").shortcut(&["Control", "O"]),
        MItem::new(2, "Mute notifications").check(false),
        MItem::separator(3),
        MItem::new(4, "Dark theme").radio(true),
        MItem::new(5, "Light theme").radio(false),
        MItem::new(6, "Unavailable action").disabled(),
        MItem::separator(7),
        MItem::new(8, "Status").sub(vec![MItem::new(9, "Online").radio(true), MItem::new(10, "Away").radio(false), MItem::new(11, "Do not disturb").radio(false)]),
        MItem::new(12, "More").lazy(vec![MItem::new(13, "Filled on demand"), MItem::new(14, "Second entry")]),
        MItem::separator(15),
        MItem::new(16, "_Quit").shortcut(&["Control", "Q"]),
    ]
}

fn register_when_possible(item: &MockItem, form: Form) {
    for _ in 0..600 {
        if smol::block_on(item.register(form)).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    eprintln!("mock_tray: no StatusNotifierWatcher on the bus");
}

/// Print every method call the shell sends, as `call <name> <call>`.
fn follow(name: &'static str, item: &MockItem) {
    let shared = item.shared.clone();
    std::thread::spawn(move || {
        let mut seen = 0;
        loop {
            let log = shared.lock().unwrap_or_else(|e| e.into_inner()).log.clone();
            for l in &log[seen.min(log.len())..] {
                println!("call {name} {l}");
            }
            seen = log.len();
            std::thread::sleep(Duration::from_millis(100));
        }
    });
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mut bus = std::env::var("DBUS_SESSION_BUS_ADDRESS").ok();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--bus" => bus = args.next(),
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    let bus = bus.ok_or("no bus: set DBUS_SESSION_BUS_ADDRESS (the nested session's private bus) or pass --bus")?;
    let themes: PathBuf = std::env::temp_dir().join(format!("ashd-mock-tray-{}", std::process::id()));
    write_ring(&themes)?;

    let pixmap = smol::block_on(
        ItemBuilder::new("mock-pixmap")
            .title("Mock pixmap")
            .tooltip("Mock pixmap app", "an IconPixmap-only item")
            .pixmaps(disc(48, [224, 164, 88]))
            .serve(&bus),
    )?;
    let named = smol::block_on(
        ItemBuilder::new("mock-named")
            .title("Mock named")
            .category("Communications")
            .tooltip("Mock named app", "an IconName item with a menu")
            .icon_name("mock-tray-named")
            .icon_theme_path(themes.to_str().unwrap_or_default())
            .attention_icon_name("mock-tray-named")
            .menu(menu())
            .serve(&bus),
    )?;
    follow("pixmap", &pixmap);
    follow("named", &named);
    register_when_possible(&pixmap, Form::BusName);
    register_when_possible(&named, Form::Path);
    println!("ready");

    let mut extra: Vec<MockItem> = Vec::new();
    let mut pixmap = Some(pixmap);
    let mut named = Some(named);
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        let words: Vec<&str> = line.split_whitespace().collect();
        let pick = |name: &str| -> Option<&MockItem> {
            match name {
                "pixmap" => pixmap.as_ref(),
                "named" => named.as_ref(),
                _ => None,
            }
        };
        let result = match words.as_slice() {
            ["status", item, status] => pick(item).map(|i| smol::block_on(i.set_status(status))),
            ["title", item, rest @ ..] => pick(item).map(|i| smol::block_on(i.set_title(&rest.join(" ")))),
            ["tooltip", item, rest @ ..] => pick(item).map(|i| smol::block_on(i.set_tooltip(&rest.join(" "), ""))),
            ["color", "pixmap", r, g, b] => pixmap.as_ref().map(|i| {
                let c = |s: &str| s.parse::<u8>().unwrap_or(0);
                smol::block_on(i.set_pixmaps(disc(48, [c(r), c(g), c(b)])))
            }),
            ["extra", n] => {
                for k in 0..n.parse::<usize>().unwrap_or(0) {
                    let hue = (k * 47 % 200) as u8 + 40;
                    let item = smol::block_on(
                        ItemBuilder::new(&format!("extra-{k}"))
                            .title(&format!("Extra {k}"))
                            .status(if k % 2 == 0 { "Passive" } else { "Active" })
                            .pixmaps(disc(32, [hue, 255 - hue, 120]))
                            .serve(&bus),
                    )?;
                    register_when_possible(&item, Form::BusName);
                    extra.push(item);
                }
                Some(Ok(()))
            }
            ["quit", "pixmap"] => {
                pixmap = None;
                Some(Ok(()))
            }
            ["quit", "named"] => {
                named = None;
                Some(Ok(()))
            }
            ["quit"] => return Ok(()),
            [] => continue,
            _ => {
                eprintln!("unknown command: {line}");
                continue;
            }
        };
        if let Some(Err(e)) = result {
            eprintln!("{line}: {e}");
        }
    }
    Ok(())
}
