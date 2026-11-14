//! Offscreen renders of the lock screen for pixel checks against det-07, no Wayland:
//!
//!     cargo run -p ashd-lockui --example render -- ashd/target-notes
//!
//! Writes `lock-offscreen-*.png` (fixed clock 14:51, Friday 13 November).

use std::{path::PathBuf, time::Duration};

use ashd_gfx::{Gfx, Text};
use ashd_lockui::{LockInput, LockPointer, LockUi, StatusIcon, StatusItem};
use ashd_tokens::ThemeName;

fn ui(theme: ThemeName) -> LockUi {
    let mut ui = LockUi::new(theme, "user".into());
    ui.host = "forge".into();
    ui.layout = Some("EN".into());
    ui.set_clock(Some(chrono::NaiveDate::from_ymd_opt(2026, 11, 13).unwrap().and_hms_opt(14, 51, 0).unwrap()));
    ui
}

fn typed(ui: &mut LockUi, n: usize, at: std::time::Instant) {
    for _ in 0..n {
        ui.input_at(LockInput::Char('x'), at);
    }
}

fn main() -> anyhow::Result<()> {
    let out = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "target-notes".into()));
    std::fs::create_dir_all(&out)?;
    let mut gfx = Gfx::new()?;
    let mut text = Text::new();
    let t0 = std::time::Instant::now();
    let ms = |v| t0 + Duration::from_millis(v);

    let mut shots: Vec<(&str, LockUi, (f64, f64), f64, std::time::Instant)> = Vec::new();

    // det-07 as drawn: 14 dots, caps lock + layout badges, wrong password (attempt 2), err border.
    let mut mock = ui(ThemeName::Ash);
    for _ in 0..2 {
        typed(&mut mock, 3, t0);
        mock.input_at(LockInput::Submit, t0);
        mock.auth_result_at(false, None, t0);
    }
    // typing again clears the error text; the mock shows dots and the error together
    typed(&mut mock, 14, ms(600));
    mock.message = Some("Wrong password · attempt 2 of 5".into());
    mock.failed_at = Some(ms(200));
    mock.caps_lock = true;
    shots.push(("lock-offscreen-mock", mock, (1920.0, 1080.0), 1.0, ms(650)));

    let mut idle = ui(ThemeName::Ash);
    typed(&mut idle, 6, t0);
    idle.status = vec![StatusItem { icon: StatusIcon::Battery, text: "86%".into() }];
    idle.can_suspend = true;
    shots.push(("lock-offscreen-idle", idle, (1920.0, 1080.0), 1.0, ms(100)));

    let mut shake = ui(ThemeName::Ash);
    typed(&mut shake, 8, t0);
    shake.input_at(LockInput::Submit, t0);
    shake.auth_result_at(false, None, t0);
    shots.push(("lock-offscreen-shake", shake, (1920.0, 1080.0), 1.0, ms(42)));

    let mut checking = ui(ThemeName::Ash);
    typed(&mut checking, 9, t0);
    checking.input_at(LockInput::Submit, t0);
    shots.push(("lock-offscreen-checking", checking, (1920.0, 1080.0), 1.0, ms(300)));

    let mut armed = ui(ThemeName::Ash);
    armed.can_suspend = true;
    shots.push(("lock-offscreen-armed", armed, (1920.0, 1080.0), 1.0, ms(10)));

    shots.push(("lock-offscreen-1366x768", ui(ThemeName::Ash), (1366.0, 768.0), 1.0, ms(10)));
    shots.push(("lock-offscreen-hidpi-1.5", ui(ThemeName::Ash), (1280.0, 720.0), 1.5, ms(10)));
    let mut dusk = ui(ThemeName::Dusk);
    typed(&mut dusk, 5, t0);
    dusk.caps_lock = true;
    shots.push(("lock-offscreen-dusk", dusk, (1920.0, 1080.0), 1.0, ms(10)));

    for (name, mut ui, size, scale, at) in shots {
        if name == "lock-offscreen-armed" {
            // draw once to learn the button rects, then press "Reboot"
            gfx.render_offscreen(&mut text, &mut ui, size, scale, at)?;
            ui.pointer_at(LockPointer::Press { x: size.0 - 200.0, y: size.1 - 63.0 }, size, at);
        }
        let img = gfx.render_offscreen(&mut text, &mut ui, size, scale, at)?;
        let path = out.join(format!("{name}.png"));
        image::RgbaImage::from_raw(img.width, img.height, img.data)
            .ok_or_else(|| anyhow::anyhow!("bad image buffer"))?
            .save(&path)?;
        println!("{}", path.display());
    }
    Ok(())
}
