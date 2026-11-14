//! Prints what the night-light service sees on `$WAYLAND_DISPLAY` for a few seconds.
//!
//! Only ever run this against a *nested* compositor (the verification session's headless
//! sway, `$ASHD_PARENT_WAYLAND_DISPLAY`): it asks for the gamma control of every output
//! and, with `--on`, tints them. It refuses to run unless `ASHD_NESTED=1` is set.
//!
//!     ASHD_NESTED=1 WAYLAND_DISPLAY=$ASHD_PARENT_WAYLAND_DISPLAY \
//!         cargo run -p ashd-svc-nightlight --example probe -- --on

use std::time::{Duration, Instant};

use ashd_svc_nightlight::{Mode, Settings, start_with_settings};

fn main() {
    if std::env::var_os("ASHD_NESTED").is_none_or(|v| v.is_empty() || v == "0") {
        eprintln!("refusing to touch gamma outside a nested session (ASHD_NESTED=1 not set)");
        std::process::exit(2);
    }
    let on = std::env::args().any(|a| a == "--on");
    let mut settings = Settings::default();
    settings.set_mode(if on { Mode::On } else { Mode::Off });
    settings.manual_transition = Duration::from_millis(300);
    let (_handle, rx) = start_with_settings(settings, None);
    let until = Instant::now() + Duration::from_secs(4);
    let mut last = None;
    while Instant::now() < until {
        while let Ok(state) = rx.try_recv() {
            if last.as_ref() != Some(&state) {
                println!("{state:#?}");
                last = Some(state);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
