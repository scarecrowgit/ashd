//! `ashd-shell`: registers every panel crate and runs the shell. Wiring only — no
//! logic belongs here (ARCHITECTURE.md). Adding a panel = one `register` line.

use clap::Parser;
use ashd_shell_core::ShellOptions;
use ashd_tokens::ThemeName;

#[derive(Parser)]
#[command(name = "ashd-shell", version, about = "ashd desktop shell (layer-shell surfaces on niri)")]
struct Args {
    /// Theme at startup (default: the persisted one, see `ashd theme get`).
    #[arg(long)]
    theme: Option<ThemeName>,
    /// IPC socket (default: $XDG_RUNTIME_DIR/ashd/shell.sock, or $ASHD_SOCKET).
    #[arg(long, env = "ASHD_SOCKET")]
    socket: Option<std::path::PathBuf>,
    /// Do not start D-Bus / pulse / sysinfo services (screenshots, tests).
    #[arg(long)]
    no_services: bool,
    /// Show the scratchpad at startup (hidden by default in M1; `ashd scratch toggle`).
    #[arg(long, conflicts_with = "no_scratchpad")]
    scratchpad: bool,
    /// Start with the scratchpad hidden (the default; kept for scripts).
    #[arg(long, hide = true)]
    no_scratchpad: bool,
    /// Debug builds only: allow running outside the nested test session.
    #[arg(long, hide = true)]
    live: bool,
}

/// A debug build is a development build. Started by mistake from a shell that
/// still carries the real session's `WAYLAND_DISPLAY`, it would put a second
/// rail and bar on the user's desktop and talk to the real logind — that has
/// happened. So debug builds only run inside the test harness (`ASHD_NESTED=1`)
/// unless `--live` says the real session is intended. Release builds, which
/// are what the NixOS package ships, have no such gate.
fn refuse_live_debug_run(live: bool) -> anyhow::Result<()> {
    if cfg!(debug_assertions) && !live && std::env::var_os("ASHD_NESTED").is_none() {
        anyhow::bail!(
            "debug build of ashd-shell refuses to start outside the nested test session \
             (ASHD_NESTED is not set); use `cargo xtask nested run -- ashd-shell`, or pass \
             --live to run it on the current session on purpose"
        );
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,naga=warn,wgpu=warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let args = Args::parse();
    refuse_live_debug_run(args.live)?;
    let options = ShellOptions {
        theme: args.theme,
        socket: args.socket,
        ipc: true,
        services: !args.no_services,
        scratch_visible: args.scratchpad && !args.no_scratchpad,
    };
    ashd_shell_core::run(options, |reg| {
        // frame (per display)
        reg.register::<ashd_panel_rail::RailPanel>()
            .register::<ashd_panel_bar::BarPanel>()
            .register::<ashd_panel_dock::DockPanel>()
            .register::<ashd_panel_bar::BarEndPanel>()
            .register::<ashd_panel_scratchpad::ScratchpadPanel>()
            // passive overlay (per display)
            .register::<ashd_panel_rail::RailFlyoutPanel>()
            .register::<ashd_panel_toasts::ToastsPanel>()
            .register::<ashd_panel_osd::OsdPanel>()
            .register::<ashd_panel_strip_header::StripHeaderPanel>()
            .register::<ashd_panel_canvas_hud::CanvasHudPanel>()
            .register::<ashd_panel_minimap::MinimapPanel>()
            .register::<ashd_panel_screenshot::ScreenshotHudPanel>()
            // modal overlay (singletons)
            .register::<ashd_panel_launcher::LauncherPanel>()
            .register::<ashd_panel_quick_settings::QuickSettingsPanel>()
            .register::<ashd_panel_session::SessionPanel>()
            .register::<ashd_panel_calendar::CalendarPanel>()
            .register::<ashd_panel_media::MediaPanel>()
            .register::<ashd_panel_clipboard::ClipboardPanel>()
            .register::<ashd_panel_cheatsheet::CheatsheetPanel>()
            .register::<ashd_panel_notification_center::NotificationCenterPanel>()
            .register::<ashd_panel_screenshot::ScreenshotPanel>()
            .register::<ashd_panel_polkit_dialog::PolkitDialogPanel>()
            .register::<ashd_panel_battery::BatteryPanel>()
            .register::<ashd_panel_theme_switcher::ThemeSwitcherPanel>()
            .register::<ashd_panel_rail::RailRenamePanel>()
            .register::<ashd_panel_bar::TrayMenuPanel>()
            .register::<ashd_panel_bar::TrayOverflowPanel>()
            .register::<ashd_panel_bar::TrayTooltipPanel>();
    })
}
