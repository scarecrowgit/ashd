//! Protocol behaviour against a real compositor. Ignored by default and refuses to run
//! outside the nested session (`ASHD_NESTED=1`, set by `cargo xtask nested`): it connects
//! to `$WAYLAND_DISPLAY` and owns the clipboard there — never the live desktop's.
//!
//!     cargo xtask nested run -- cargo test -p ashd-svc-clipboard -- --ignored --nocapture

use std::time::{Duration, Instant};

use async_channel::Receiver;

use crate::{
    ClipboardState, Kind,
    proto::{Event, ReadResult, SENSITIVE_HINT, Served, Wayland},
    start_in,
};

fn require_nested() {
    assert_eq!(
        std::env::var("ASHD_NESTED").as_deref(),
        Ok("1"),
        "refusing to touch a clipboard outside the nested session (ASHD_NESTED=1 missing)"
    );
    assert!(std::env::var_os("WAYLAND_DISPLAY").is_some(), "no WAYLAND_DISPLAY");
}

fn served(reps: &[(&str, &[u8])]) -> Served {
    Served { reps: reps.iter().map(|(m, d)| (m.to_string(), std::sync::Arc::new(d.to_vec()))).collect() }
}

fn png() -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(64, 32, image::Rgba([200, 80, 40, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img).write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

/// Pump `owner` (so it serves `send`) until `pred` holds for a published state.
fn wait_for(owner: &mut Wayland, rx: &Receiver<ClipboardState>, last: &mut ClipboardState, what: &str, pred: impl Fn(&ClipboardState) -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        owner.pump(50).unwrap();
        while let Ok(s) = rx.try_recv() {
            *last = s;
        }
        if pred(last) {
            return;
        }
    }
    panic!("timed out waiting for: {what}; entries: {:?}", last.entries.iter().map(|e| e.first_line()).collect::<Vec<_>>());
}

#[test]
#[ignore = "needs the nested session: cargo xtask nested run -- cargo test -p ashd-svc-clipboard -- --ignored"]
fn watch_capture_activate_roundtrip() {
    require_nested();
    let dir = tempfile::tempdir().unwrap();
    let (handle, rx) = start_in(dir.path().to_path_buf());
    let mut last = ClipboardState::default();
    let mut owner = Wayland::connect().expect("data-control in the nested compositor");

    let end = Instant::now() + Duration::from_secs(5);
    while !last.available && Instant::now() < end {
        if let Ok(s) = rx.recv_blocking() {
            last = s;
        }
    }
    assert!(last.available, "service did not bind a data-control protocol");

    // text, with an HTML alternative
    owner.set_selection(
        served(&[("text/plain;charset=utf-8", b"hello nested"), ("text/html", b"<b>hello nested</b>")]),
        false,
        false,
    );
    wait_for(&mut owner, &rx, &mut last, "text entry", |s| s.entries.first().is_some_and(|e| e.preview == "hello nested"));
    assert_eq!(last.entries[0].kind, Kind::Text);
    assert_eq!(last.entries[0].alt_mimes, ["text/html"]);
    let text_id = last.entries[0].id;

    // colour, link, image
    owner.set_selection(served(&[("text/plain", b"#E0A458")]), false, false);
    wait_for(&mut owner, &rx, &mut last, "colour entry", |s| s.entries.first().is_some_and(|e| e.kind == Kind::Colour));
    owner.set_selection(served(&[("text/plain", b"https://github.com/YaLTeR/niri/wiki")]), false, false);
    wait_for(&mut owner, &rx, &mut last, "link entry", |s| s.entries.first().is_some_and(|e| e.kind == Kind::Link));
    owner.set_selection(served(&[("image/png", &png())]), false, false);
    wait_for(&mut owner, &rx, &mut last, "image entry", |s| s.entries.first().is_some_and(|e| e.kind == Kind::Image));
    let img = last.entries[0].image.clone().unwrap();
    assert_eq!((img.width, img.height), (64, 32));
    assert!(img.thumb.unwrap().exists());

    // a secret is never recorded
    owner.set_selection(served(&[("text/plain", b"hunter2"), (SENSITIVE_HINT, b"secret")]), false, false);
    owner.pump(500).unwrap();
    owner.set_selection(served(&[("text/plain", b"after the secret")]), false, false);
    wait_for(&mut owner, &rx, &mut last, "entry after the secret", |s| s.entries.first().is_some_and(|e| e.preview == "after the secret"));
    assert!(last.entries.iter().all(|e| e.preview != "hunter2"), "a secret was recorded");

    // re-copy dedupes to the top
    let count = last.entries.len();
    owner.set_selection(served(&[("text/plain;charset=utf-8", b"hello nested")]), false, false);
    wait_for(&mut owner, &rx, &mut last, "dedupe", |s| s.entries.first().is_some_and(|e| e.id == text_id));
    assert_eq!(last.entries.len(), count);

    // activate an older entry: another client now reads it, and we do not record our own set
    let image_id = last.entries.iter().find(|e| e.kind == Kind::Image).unwrap().id;
    handle.activate(image_id);
    wait_for(&mut owner, &rx, &mut last, "activation", |s| s.active == Some(image_id));
    let mut reader = Wayland::connect().unwrap();
    reader.pump(200).unwrap();
    let offer = reader
        .take_events()
        .into_iter()
        .rev()
        .find_map(|e| match e {
            Event::Selection(Some(o)) => Some(o),
            _ => None,
        })
        .expect("a selection offer for the reader");
    assert!(offer.mimes().iter().any(|m| m == "image/png"));
    match reader.read_offer(&offer, "image/png", 1 << 20, Duration::from_secs(3)) {
        ReadResult::Data(d) => assert_eq!(d, png()),
        other => panic!("reading the activated entry: {other:?}"),
    }
    owner.pump(500).unwrap();
    while let Ok(s) = rx.try_recv() {
        last = s;
    }
    assert_eq!(last.entries.len(), count, "our own re-set was recorded again");

    // an activated text entry is served under every text alias
    handle.activate(text_id);
    wait_for(&mut owner, &rx, &mut last, "text activation", |s| s.active == Some(text_id));
    let mut reader = Wayland::connect().unwrap();
    reader.pump(200).unwrap();
    let offer = reader
        .take_events()
        .into_iter()
        .rev()
        .find_map(|e| match e {
            Event::Selection(Some(o)) => Some(o),
            _ => None,
        })
        .unwrap();
    let mimes = offer.mimes();
    for want in ["text/plain;charset=utf-8", "UTF8_STRING", "text/html"] {
        assert!(mimes.iter().any(|m| m == want), "{want} missing in {mimes:?}");
    }
    assert_eq!(reader.read_offer(&offer, "UTF8_STRING", 1024, Duration::from_secs(3)), ReadResult::Data(b"hello nested".to_vec()));
}
