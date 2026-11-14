//! Wire format of every `Request` / `Response` variant: serialise, check the JSON, parse
//! it back. The JSON strings are the contract with `ashd` scripts, keybinds and xtask.

use pretty_assertions::assert_eq;
use ashd_ipc::{
    CanvasOp, CaptureMode, LauncherMode, MediaControl, OsdRequest, PanelOp, RecordOp, Request, Response, ScratchOp,
    ScreenshotRequest, ShellStatus, SurfaceStatus, ThemeRequest, VolumeChange,
};
use ashd_tokens::ThemeName;

/// Every request variant (and every value of its sub-enums) with its exact wire form.
fn all_requests() -> Vec<(Request, String)> {
    let mut v: Vec<(Request, String)> = vec![
        (Request::Ping, r#""ping""#.into()),
        (Request::Status, r#""status""#.into()),
        (Request::Dismiss, r#""dismiss""#.into()),
        (Request::Quit, r#""quit""#.into()),
        (Request::Lock, r#""lock""#.into()),
        (Request::Osd(OsdRequest::Volume(VolumeChange::Step(5))), r#"{"osd":{"volume":{"step":5}}}"#.into()),
        (Request::Osd(OsdRequest::Volume(VolumeChange::Step(-10))), r#"{"osd":{"volume":{"step":-10}}}"#.into()),
        (Request::Osd(OsdRequest::Volume(VolumeChange::Mute)), r#"{"osd":{"volume":"mute"}}"#.into()),
        (Request::Osd(OsdRequest::MicMute), r#"{"osd":"mic-mute"}"#.into()),
        (Request::Osd(OsdRequest::Brightness(-5)), r#"{"osd":{"brightness":-5}}"#.into()),
        (Request::Launcher { op: PanelOp::Toggle, mode: None }, r#"{"launcher":{"op":"toggle"}}"#.into()),
    ];
    for (mode, name) in [
        (LauncherMode::Calc, "calc"),
        (LauncherMode::Files, "files"),
        (LauncherMode::Run, "run"),
        (LauncherMode::Emoji, "emoji"),
        (LauncherMode::Web, "web"),
    ] {
        v.push((
            Request::Launcher { op: PanelOp::Open, mode: Some(mode) },
            format!(r#"{{"launcher":{{"op":"open","mode":"{name}"}}}}"#),
        ));
    }
    for (op, name) in [(PanelOp::Toggle, "toggle"), (PanelOp::Open, "open"), (PanelOp::Close, "close")] {
        for (make, key) in [
            (Request::Clipboard { op }, "clipboard"),
            (Request::Notifications { op }, "notifications"),
            (Request::QuickSettings { op }, "quick-settings"),
            (Request::Calendar { op }, "calendar"),
            (Request::Session { op }, "session"),
            (Request::Cheatsheet { op }, "cheatsheet"),
            (Request::Media { op }, "media"),
            (Request::ThemeSwitcher { op }, "theme-switcher"),
        ] {
            v.push((make, format!(r#"{{"{key}":{{"op":"{name}"}}}}"#)));
        }
    }
    for (op, name) in [
        (ScratchOp::Hide, "hide"),
        (ScratchOp::Show, "show"),
        (ScratchOp::Toggle, "toggle"),
        (ScratchOp::Open, "open"),
        (ScratchOp::Close, "close"),
    ] {
        v.push((Request::Scratch(op), format!(r#"{{"scratch":"{name}"}}"#)));
    }
    v.push((Request::Scratch(ScratchOp::HideWindow(7)), r#"{"scratch":{"hide-window":7}}"#.into()));
    v.push((Request::Scratch(ScratchOp::ShowWindow(12)), r#"{"scratch":{"show-window":12}}"#.into()));
    for (op, name) in [
        (CanvasOp::Toggle, "toggle"),
        (CanvasOp::ZoomIn, "zoom-in"),
        (CanvasOp::ZoomOut, "zoom-out"),
        (CanvasOp::Fit, "fit"),
        (CanvasOp::Home, "home"),
    ] {
        v.push((Request::Canvas(op), format!(r#"{{"canvas":"{name}"}}"#)));
    }
    v.push((Request::Theme(ThemeRequest::Get), r#"{"theme":"get"}"#.into()));
    v.push((Request::Theme(ThemeRequest::Cycle), r#"{"theme":"cycle"}"#.into()));
    for (c, name) in [
        (MediaControl::PlayPause, "play-pause"),
        (MediaControl::Next, "next"),
        (MediaControl::Previous, "previous"),
        (MediaControl::Stop, "stop"),
    ] {
        v.push((Request::MediaControl(c), format!(r#"{{"media-control":"{name}"}}"#)));
    }
    for (mode, name) in [(CaptureMode::Region, "region"), (CaptureMode::Window, "window"), (CaptureMode::Output, "output")] {
        for (record, op, op_name) in [(false, PanelOp::Toggle, "toggle"), (true, PanelOp::Open, "open"), (false, PanelOp::Close, "close")] {
            v.push((
                Request::Screenshot(ScreenshotRequest { mode: Some(mode), record, op }),
                format!(r#"{{"screenshot":{{"mode":"{name}","record":{record},"op":"{op_name}"}}}}"#),
            ));
        }
    }
    v.push((Request::Screenshot(ScreenshotRequest::default()), r#"{"screenshot":{"record":false,"op":"toggle"}}"#.into()));
    v.push((Request::Record(RecordOp::Stop), r#"{"record":"stop"}"#.into()));
    v.push((Request::Record(RecordOp::Toggle), r#"{"record":"toggle"}"#.into()));
    for name in ThemeName::ALL {
        v.push((Request::Theme(ThemeRequest::Set(name)), format!(r#"{{"theme":{{"set":"{name}"}}}}"#)));
    }
    v
}

#[test]
fn every_request_roundtrips_with_its_exact_wire_form() {
    for (req, json) in all_requests() {
        assert_eq!(serde_json::to_string(&req).unwrap(), json, "serialising {req:?}");
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), req, "parsing {json}");
        // Re-serialising what we parsed is stable too.
        let again = serde_json::to_string(&serde_json::from_str::<Request>(&json).unwrap()).unwrap();
        assert_eq!(again, json);
    }
}

#[test]
fn request_names_are_the_wire_keys() {
    for (req, json) in all_requests() {
        let key = match serde_json::from_str::<serde_json::Value>(&json).unwrap() {
            serde_json::Value::String(s) => s,
            serde_json::Value::Object(o) => o.keys().next().unwrap().clone(),
            other => panic!("unexpected wire shape {other}"),
        };
        assert_eq!(req.name(), key, "Request::name for {json}");
    }
}

#[test]
fn launcher_prefixes() {
    assert_eq!(
        [LauncherMode::Calc, LauncherMode::Files, LauncherMode::Run, LauncherMode::Emoji, LauncherMode::Web]
            .map(LauncherMode::prefix),
        ['=', '/', '>', ':', '!']
    );
}

#[test]
fn request_defaults_and_leniency() {
    // `op` defaults to toggle, `mode` to none.
    for json in [r#"{"launcher":{}}"#, r#"{"launcher":{"op":"toggle"}}"#] {
        assert_eq!(
            serde_json::from_str::<Request>(json).unwrap(),
            Request::Launcher { op: PanelOp::Toggle, mode: None }
        );
    }
    assert_eq!(serde_json::from_str::<Request>(r#"{"clipboard":{}}"#).unwrap(), Request::Clipboard { op: PanelOp::Toggle });
    assert_eq!(serde_json::from_str::<Request>(r#"{"media":{}}"#).unwrap(), Request::Media { op: PanelOp::Toggle });
    // Whitespace is irrelevant.
    assert_eq!(serde_json::from_str::<Request>("  { \"scratch\" : \"hide\" }  ").unwrap(), Request::Scratch(ScratchOp::Hide));
}

#[test]
fn bad_requests_are_rejected() {
    for json in [
        "",
        "null",
        "42",
        r#""pong""#,
        r#""Ping""#,
        r#"{"ping":{}}"#,
        r#"{"osd":{"volume":{"step":"loud"}}}"#,
        r#"{"osd":{"volume":{"step":99999999999}}}"#,
        r#"{"theme":{"set":"neon"}}"#,
        r#"{"launcher":{"mode":"calculator"}}"#,
        r#"{"launcher":{"op":"flip"}}"#,
        r#"{"scratch":"explode"}"#,
        r#"{"canvas":1}"#,
        r#"["ping"]"#,
        r#"{"ping":1,"status":2}"#,
        "\"ping\" trailing",
    ] {
        assert!(serde_json::from_str::<Request>(json).is_err(), "should reject {json:?}");
    }
}

fn status() -> ShellStatus {
    ShellStatus {
        version: "0.1.0".into(),
        theme: ThemeName::Dusk,
        niri_connected: true,
        modal: Some("launcher".into()),
        surfaces: vec![
            SurfaceStatus { kind: "bar".into(), output: "DP-1".into(), width: 1920.0, height: 46.0 },
            SurfaceStatus { kind: "rail".into(), output: "DP-1".into(), width: 64.0, height: 1034.0 },
        ],
    }
}

#[test]
fn every_response_roundtrips() {
    let cases: Vec<(Response, &str)> = vec![
        (Response::Ok, r#""ok""#),
        (
            Response::Pong { version: "0.1.0".into(), protocol: 1, pid: 4242 },
            r#"{"pong":{"version":"0.1.0","protocol":1,"pid":4242}}"#,
        ),
        (Response::Theme(ThemeName::Midnight), r#"{"theme":"midnight"}"#),
        (Response::Error("no panel handles `clipboard` yet".into()), r#"{"error":"no panel handles `clipboard` yet"}"#),
        (
            Response::Status(ShellStatus { surfaces: vec![], modal: None, ..status() }),
            r#"{"status":{"version":"0.1.0","theme":"dusk","niri_connected":true,"modal":null,"surfaces":[]}}"#,
        ),
    ];
    for (resp, json) in cases {
        assert_eq!(serde_json::to_string(&resp).unwrap(), json);
        assert_eq!(serde_json::from_str::<Response>(json).unwrap(), resp);
    }
    // Full status with surfaces survives a round trip (field names are part of the contract).
    let full = Response::Status(status());
    let json = serde_json::to_string(&full).unwrap();
    assert!(json.contains(r#""surfaces":[{"kind":"bar","output":"DP-1","width":1920.0,"height":46.0}"#), "{json}");
    assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), full);
}

#[test]
fn error_messages_keep_special_characters_on_one_line() {
    let resp = Response::error("line1\nline2 \"quoted\" \\ é\t");
    let json = serde_json::to_string(&resp).unwrap();
    assert!(!json.contains('\n'), "NDJSON framing needs single-line values: {json}");
    assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), resp);
}

#[test]
fn bad_responses_are_rejected() {
    for json in ["", "ok", r#""fine""#, r#"{"pong":{"version":"x"}}"#, r#"{"theme":"neon"}"#, "[]"] {
        assert!(serde_json::from_str::<Response>(json).is_err(), "should reject {json:?}");
    }
}

#[test]
fn protocol_constants() {
    assert_eq!(ashd_ipc::PROTOCOL_VERSION, 1);
    assert_eq!(ashd_ipc::SOCKET_ENV, "ASHD_SOCKET");
}
