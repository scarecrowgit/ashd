//! Offscreen renders of the greeter for pixel checks against det-07, no Wayland:
//!
//!     cargo run -p ashd-lockui --example render_greeter -- ashd/target-notes
//!
//! Writes `greeter-*.png` (fixed clock 14:51, Friday 13 November).

use std::{path::PathBuf, time::Duration};

use ashd_gfx::{Gfx, Text};
use ashd_lockui::{GreeterAction, GreeterInput, GreeterUi, GreetdEvent, PromptKind, StatusIcon, StatusItem};
use ashd_tokens::ThemeName;

type Shot = (&'static str, GreeterUi, (f64, f64), f64, std::time::Instant);

fn greeter(theme: ThemeName, sessions: &[&str], user: Option<&str>) -> GreeterUi {
    let mut ui = GreeterUi::new(theme, sessions.iter().map(|s| s.to_string()).collect(), user.map(String::from));
    ui.lock.host = "forge".into();
    ui.lock.layout = Some("EN".into());
    ui.set_clock(Some(chrono::NaiveDate::from_ymd_opt(2026, 11, 13).unwrap().and_hms_opt(14, 51, 0).unwrap()));
    ui
}

fn typed(ui: &mut GreeterUi, s: &str, at: std::time::Instant) {
    for c in s.chars() {
        ui.input_at(GreeterInput::Char(c), at);
    }
}

fn main() -> anyhow::Result<()> {
    let out = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "target-notes".into()));
    std::fs::create_dir_all(&out)?;
    let mut gfx = Gfx::new()?;
    let mut text = Text::new();
    let t0 = std::time::Instant::now();
    let ms = |v| t0 + Duration::from_millis(v);
    let full = (1920.0, 1080.0);
    let mut shots: Vec<Shot> = Vec::new();

    // First boot: empty, focused name field.
    shots.push(("greeter-empty", greeter(ThemeName::Ash, &["niri"], None), full, 1.0, ms(10)));

    // Name typed, password field focused (remembered user), 9 dots, caps lock.
    let mut idle = greeter(ThemeName::Ash, &["niri"], Some("alice"));
    typed(&mut idle, "hunter2xy", t0);
    idle.lock.caps_lock = true;
    idle.lock.status = vec![StatusItem { icon: StatusIcon::Battery, text: "86%".into() }];
    idle.lock.can_suspend = true;
    shots.push(("greeter-user", idle, full, 1.0, ms(100)));

    // Name being typed.
    let mut name = greeter(ThemeName::Ash, &["niri"], None);
    typed(&mut name, "ali", t0);
    shots.push(("greeter-typing-name", name, full, 1.0, ms(100)));

    // Waiting for greetd.
    let mut checking = greeter(ThemeName::Ash, &["niri"], Some("alice"));
    typed(&mut checking, "hunter2", t0);
    assert!(matches!(checking.input_at(GreeterInput::Submit, t0), GreeterAction::CreateSession(_)));
    shots.push(("greeter-checking", checking, full, 1.0, ms(300)));

    // Wrong password: shake + error line.
    let mut wrong = greeter(ThemeName::Ash, &["niri"], Some("alice"));
    typed(&mut wrong, "nope", t0);
    wrong.input_at(GreeterInput::Submit, t0);
    wrong.greetd_event_at(GreetdEvent::Prompt { kind: PromptKind::Secret, text: "Password:".into() }, t0);
    wrong.greetd_event_at(GreetdEvent::Failed { auth: true, text: String::new() }, t0);
    shots.push(("greeter-wrong", wrong, full, 1.0, ms(42)));

    // Two sessions, selectable.
    let mut sessions = greeter(ThemeName::Ash, &["niri", "sway"], Some("alice"));
    sessions.input_at(GreeterInput::Submit, t0);
    sessions.greetd_event_at(GreetdEvent::Prompt { kind: PromptKind::Secret, text: "Password:".into() }, t0);
    typed(&mut sessions, "abc", t0);
    shots.push(("greeter-sessions", sessions, full, 1.0, ms(100)));

    // Visible prompt (second factor) + info message.
    let mut otp = greeter(ThemeName::Ash, &["niri"], Some("alice"));
    otp.input_at(GreeterInput::Submit, t0);
    otp.greetd_event_at(GreetdEvent::Message { error: false, text: "Your password expires in 3 days".into() }, t0);
    otp.greetd_event_at(GreetdEvent::Prompt { kind: PromptKind::Visible, text: "Verification code:".into() }, t0);
    typed(&mut otp, "482913", t0);
    shots.push(("greeter-visible-prompt", otp, full, 1.0, ms(100)));

    // Fatal: no greetd.
    let mut fatal = greeter(ThemeName::Ash, &["niri"], None);
    fatal.set_fatal("greetd is not running (GREETD_SOCK is not set) · restarting in 20 s");
    shots.push(("greeter-fatal", fatal, full, 1.0, ms(100)));

    // Other sizes and a theme.
    let mut small = greeter(ThemeName::Ash, &["niri"], Some("alice"));
    typed(&mut small, "hunter2", t0);
    shots.push(("greeter-1366x768", small, (1366.0, 768.0), 1.0, ms(100)));
    shots.push(("greeter-hidpi-1.5", greeter(ThemeName::Ash, &["niri"], Some("alice")), (1280.0, 720.0), 1.5, ms(10)));
    let mut dusk = greeter(ThemeName::Dusk, &["niri"], Some("alice"));
    typed(&mut dusk, "hunter2", t0);
    shots.push(("greeter-dusk", dusk, full, 1.0, ms(100)));

    for (name, mut ui, size, scale, at) in shots {
        // draw once to learn the hit rects like a live run would
        gfx.render_offscreen(&mut text, &mut ui, size, scale, at)?;
        let img = gfx.render_offscreen(&mut text, &mut ui, size, scale, at)?;
        let path = out.join(format!("{name}.png"));
        image::RgbaImage::from_raw(img.width, img.height, img.data)
            .ok_or_else(|| anyhow::anyhow!("bad image buffer"))?
            .save(&path)?;
        println!("{}", path.display());
    }
    Ok(())
}
