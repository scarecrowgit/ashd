//! Argument definitions (clap) and the pure mapping from commands to shell requests.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::{
    Args, Parser, Subcommand, ValueEnum,
    builder::{PossibleValuesParser, TypedValueParser as _},
};
use ashd_ipc::{
    CanvasOp, CaptureMode, LauncherMode, MediaControl, OsdRequest, PanelOp, RecordOp, Request, ScratchOp,
    ScreenshotRequest, VolumeChange,
};
use ashd_tokens::ThemeName;

#[derive(Parser, Debug)]
#[command(
    name = "ashd",
    version,
    about = "Control the ashd desktop shell",
    long_about = "Control the ashd desktop shell.\n\nExit status: 0 ok, 1 error, 2 ashd-shell is not running.",
    propagate_version = true,
    arg_required_else_help = true
)]
pub struct Cli {
    /// Machine-readable output (`status`, and the raw response for other shell commands).
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Mode {
    Calc,
    Files,
    Run,
    Emoji,
    Web,
}

impl From<Mode> for LauncherMode {
    fn from(m: Mode) -> Self {
        match m {
            Mode::Calc => Self::Calc,
            Mode::Files => Self::Files,
            Mode::Run => Self::Run,
            Mode::Emoji => Self::Emoji,
            Mode::Web => Self::Web,
        }
    }
}

#[derive(Args, Clone, Copy, Debug)]
pub struct PanelArgs {
    /// Open instead of toggling.
    #[arg(long, conflicts_with = "close")]
    pub open: bool,
    /// Close instead of toggling.
    #[arg(long)]
    pub close: bool,
}

impl PanelArgs {
    pub fn op(self) -> PanelOp {
        if self.open {
            PanelOp::Open
        } else if self.close {
            PanelOp::Close
        } else {
            PanelOp::Toggle
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Toggle the launcher.
    Launcher {
        /// Start in a mode: `=` calc, `/` files, `>` run, `:` emoji, `!` web.
        #[arg(long, value_enum)]
        mode: Option<Mode>,
        #[command(flatten)]
        panel: PanelArgs,
    },
    /// Toggle the clipboard picker.
    Clipboard(PanelArgs),
    /// Toggle the notification centre.
    Notifications(PanelArgs),
    /// Toggle quick settings.
    QuickSettings(PanelArgs),
    /// Toggle the calendar popup.
    Calendar(PanelArgs),
    /// Toggle the session / power menu.
    Session(PanelArgs),
    /// Toggle the keybinding cheatsheet.
    Cheatsheet(PanelArgs),
    /// Toggle the media popup, or send a media key to the active player.
    #[command(args_conflicts_with_subcommands = true)]
    Media {
        #[command(subcommand)]
        key: Option<MediaCmd>,
        #[command(flatten)]
        panel: PanelArgs,
    },
    /// Open the screenshot / recording overlay (region, window or output).
    Screenshot {
        #[command(flatten)]
        what: ScreenshotArgs,
        #[command(flatten)]
        panel: PanelArgs,
    },
    /// Control the running screen recording.
    Record {
        #[arg(value_enum)]
        op: RecordArg,
    },
    /// Change a value and show the OSD.
    Osd {
        #[command(subcommand)]
        what: OsdCmd,
    },
    /// Scratchpad: hide / show windows, open / close the panel.
    #[command(long_about = "Scratchpad.\n\n\
        hide     send the focused window (or --id N) to the hidden scratch workspace\n\
        show     bring back the most recently hidden window (or --id N)\n\
        toggle   show / hide the scratchpad panel on the right\n\
        open     show the panel\n\
        close    hide the panel\n\n\
        hide and show also work without ashd-shell (straight to niri); they need the ashd niri \
        fork with `workspace \"scratch\" { hidden }` in its config.")]
    Scratch {
        #[arg(value_enum)]
        op: ScratchArg,
        /// The niri window id to hide / show (default: the focused / most recently hidden one).
        #[arg(long)]
        id: Option<u64>,
    },
    /// Application helpers.
    App {
        #[command(subcommand)]
        cmd: AppCmd,
    },
    /// Canvas mode.
    Canvas {
        #[arg(value_enum)]
        op: CanvasArg,
    },
    /// Lock the session.
    Lock,
    /// Themes (ash ink midnight moss dusk).
    Theme {
        #[command(subcommand)]
        cmd: ThemeCmd,
    },
    /// Launch fm, the file manager, in the current theme: `fm --theme <theme> ARGS…` (binary
    /// from $ASHD_FM_BIN or PATH; no `--theme` when fm is unchecked in `ashd theme pick`).
    Fm {
        /// Passed to fm unchanged (after `--`).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// NixOS helpers.
    Sys {
        #[command(subcommand)]
        cmd: SysCmd,
    },
    /// Start (or reuse) a tmux session running `claude remote-control` for the cwd.
    ClaudeRc,
    /// Shell status (theme, niri connection, modal, surfaces).
    Status,
    /// Check that ashd-shell answers.
    Ping,
    /// Close the open modal / popup.
    Dismiss,
    /// Quit ashd-shell.
    QuitShell,
    /// Print shell completions.
    Completions { shell: clap_complete::Shell },
}

/// `ashd screenshot` mode flags (at most one) and `--record`.
#[derive(Args, Clone, Copy, Debug)]
pub struct ScreenshotArgs {
    /// Start in region mode (drag a rectangle).
    #[arg(long, conflicts_with_all = ["window", "output"])]
    pub region: bool,
    /// Start in window mode (click the window).
    #[arg(long, conflicts_with = "output")]
    pub window: bool,
    /// Start in output mode (the whole screen).
    #[arg(long)]
    pub output: bool,
    /// Record a video instead of taking a picture.
    #[arg(long)]
    pub record: bool,
}

impl ScreenshotArgs {
    fn mode(self) -> Option<CaptureMode> {
        if self.region {
            Some(CaptureMode::Region)
        } else if self.window {
            Some(CaptureMode::Window)
        } else if self.output {
            Some(CaptureMode::Output)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum RecordArg {
    /// Stop the running recording.
    Stop,
    /// Stop the running recording, else open the overlay in "Record".
    Toggle,
}

/// Media keys (bind XF86AudioPlay / Next / Prev / Stop to these).
#[derive(Subcommand, Clone, Copy, Debug)]
pub enum MediaCmd {
    /// Play or pause the active player.
    PlayPause,
    /// Next track.
    Next,
    /// Previous track.
    Previous,
    /// Stop playback.
    Stop,
}

#[derive(Subcommand, Debug)]
pub enum OsdCmd {
    /// `+N` / `-N` percent points, or `mute` (toggle).
    Volume {
        #[arg(allow_hyphen_values = true, value_name = "+N|-N|mute")]
        change: String,
    },
    /// Toggle microphone mute.
    MicMute,
    /// `+N` / `-N` percent points.
    Brightness {
        #[arg(allow_hyphen_values = true, value_name = "+N|-N")]
        change: String,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ScratchArg {
    /// Hide the focused window to the scratch workspace.
    Hide,
    /// Bring back the most recently hidden window.
    Show,
    /// Show / hide the scratchpad panel.
    Toggle,
    /// Show the scratchpad panel.
    Open,
    /// Hide the scratchpad panel.
    Close,
}

#[derive(Subcommand, Debug)]
pub enum AppCmd {
    /// Close every window of the focused window's app id (niri).
    Quit,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CanvasArg {
    Toggle,
    ZoomIn,
    ZoomOut,
    Fit,
    Home,
}

fn theme_parser() -> impl clap::builder::TypedValueParser<Value = ThemeName> {
    PossibleValuesParser::new(ThemeName::ALL.map(ThemeName::label))
        .map(|s: String| ThemeName::from_label(&s).expect("possible values are theme labels"))
}

#[derive(Subcommand, Debug)]
pub enum ThemeCmd {
    /// Print the current theme.
    Get,
    /// Persist, write the theme files, reload consumers, switch the shell (crossfade).
    Set {
        #[arg(value_parser = theme_parser())]
        name: ThemeName,
        /// Do not signal kitty / niri (tests: the nested session must not touch live apps).
        #[arg(long)]
        no_reload: bool,
    },
    /// `set` the next theme.
    Cycle {
        /// Do not signal kitty / niri.
        #[arg(long)]
        no_reload: bool,
    },
    /// Toggle the theme switcher (five theme cards, live preview, "apply to").
    Pick(PanelArgs),
    /// (Re)write the theme files of the persisted theme and reload consumers.
    Apply {
        /// Do not list the written files.
        #[arg(long)]
        quiet: bool,
        /// Do not signal kitty / niri.
        #[arg(long)]
        no_reload: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum SysCmd {
    /// Write hosts/<host>/hardware-configuration.nix from nixos-generate-config.
    GenerateHardwareConfig {
        host: String,
        /// Flake checkout containing `hosts/` (default: the current directory).
        #[arg(long, value_name = "DIR")]
        flake_dir: Option<PathBuf>,
    },
    /// Install the FlClashX TUN copy (setuid FlClashCore) from the store; idempotent.
    FlclashxSync {
        /// Package directory (…/opt/FlClashX).
        #[arg(long)]
        src: PathBuf,
        /// Install directory (e.g. /var/lib/flclashx).
        #[arg(long)]
        dst: PathBuf,
    },
    /// File picker backend for xdg-desktop-portal-termfilechooser (runs `fm --pick`).
    FmPick {
        /// 1 = multiple selection.
        multiple: String,
        /// 1 = pick a directory.
        directory: String,
        /// 1 = save dialog.
        save: String,
        /// Starting path (empty = $HOME).
        #[arg(allow_hyphen_values = true)]
        path: String,
        /// File the chosen paths are written to.
        #[arg(allow_hyphen_values = true)]
        outfile: String,
        /// 1 = print the command line to stderr first.
        debug: Option<String>,
    },
}

/// `+5` / `-5` / `5` / `5%` → percent points.
pub fn signed(s: &str) -> anyhow::Result<i32> {
    let t = s.trim().trim_end_matches('%');
    // `i32::from_str` accepts a leading `+` or `-` (but not both).
    t.parse::<i32>()
        .with_context(|| format!("expected +N or -N, got `{s}`"))
}

/// The shell request a command maps to; `None` for commands that run locally.
pub fn to_request(cmd: &Cmd) -> anyhow::Result<Option<Request>> {
    Ok(Some(match cmd {
        Cmd::Launcher { mode, panel } => Request::Launcher { op: panel.op(), mode: mode.map(Into::into) },
        Cmd::Clipboard(p) => Request::Clipboard { op: p.op() },
        Cmd::Notifications(p) => Request::Notifications { op: p.op() },
        Cmd::QuickSettings(p) => Request::QuickSettings { op: p.op() },
        Cmd::Calendar(p) => Request::Calendar { op: p.op() },
        Cmd::Session(p) => Request::Session { op: p.op() },
        Cmd::Cheatsheet(p) => Request::Cheatsheet { op: p.op() },
        Cmd::Media { key: None, panel } => Request::Media { op: panel.op() },
        Cmd::Media { key: Some(key), .. } => Request::MediaControl(match key {
            MediaCmd::PlayPause => MediaControl::PlayPause,
            MediaCmd::Next => MediaControl::Next,
            MediaCmd::Previous => MediaControl::Previous,
            MediaCmd::Stop => MediaControl::Stop,
        }),
        Cmd::Theme { cmd: ThemeCmd::Pick(p) } => Request::ThemeSwitcher { op: p.op() },
        Cmd::Screenshot { what, panel } => {
            Request::Screenshot(ScreenshotRequest { mode: what.mode(), record: what.record, op: panel.op() })
        }
        Cmd::Record { op } => Request::Record(match op {
            RecordArg::Stop => RecordOp::Stop,
            RecordArg::Toggle => RecordOp::Toggle,
        }),
        Cmd::Osd { what } => Request::Osd(match what {
            OsdCmd::Volume { change } if change.trim().eq_ignore_ascii_case("mute") => {
                OsdRequest::Volume(VolumeChange::Mute)
            }
            OsdCmd::Volume { change } => OsdRequest::Volume(VolumeChange::Step(signed(change)?)),
            OsdCmd::MicMute => OsdRequest::MicMute,
            OsdCmd::Brightness { change } => OsdRequest::Brightness(signed(change)?),
        }),
        Cmd::Scratch { op, id } => Request::Scratch(match (op, id) {
            (ScratchArg::Hide, None) => ScratchOp::Hide,
            (ScratchArg::Hide, Some(id)) => ScratchOp::HideWindow(*id),
            (ScratchArg::Show, None) => ScratchOp::Show,
            (ScratchArg::Show, Some(id)) => ScratchOp::ShowWindow(*id),
            (_, Some(_)) => anyhow::bail!("--id only applies to `scratch hide` and `scratch show`"),
            (ScratchArg::Toggle, None) => ScratchOp::Toggle,
            (ScratchArg::Open, None) => ScratchOp::Open,
            (ScratchArg::Close, None) => ScratchOp::Close,
        }),
        Cmd::Canvas { op } => Request::Canvas(match op {
            CanvasArg::Toggle => CanvasOp::Toggle,
            CanvasArg::ZoomIn => CanvasOp::ZoomIn,
            CanvasArg::ZoomOut => CanvasOp::ZoomOut,
            CanvasArg::Fit => CanvasOp::Fit,
            CanvasArg::Home => CanvasOp::Home,
        }),
        Cmd::Lock => Request::Lock,
        Cmd::Status => Request::Status,
        Cmd::Ping => Request::Ping,
        Cmd::Dismiss => Request::Dismiss,
        Cmd::QuitShell => Request::Quit,
        Cmd::App { .. } | Cmd::Theme { .. } | Cmd::Fm { .. } | Cmd::Sys { .. } | Cmd::ClaudeRc | Cmd::Completions { .. } => {
            return Ok(None);
        }
    }))
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};
    use pretty_assertions::assert_eq;

    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("ashd").chain(args.iter().copied()))
    }

    fn req(args: &[&str]) -> Request {
        to_request(&parse(args).unwrap().cmd).unwrap().unwrap()
    }

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn signed_numbers() {
        assert_eq!(signed("+5").unwrap(), 5);
        assert_eq!(signed("-5").unwrap(), -5);
        assert_eq!(signed("5").unwrap(), 5);
        assert_eq!(signed(" 10% ").unwrap(), 10);
        assert_eq!(signed("-10%").unwrap(), -10);
        assert_eq!(signed("0").unwrap(), 0);
        for bad in ["", "+", "abc", "5.5", "--5", "+-5", "99999999999"] {
            assert!(signed(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn shell_commands_map_to_requests() {
        assert_eq!(req(&["osd", "volume", "-5"]), Request::Osd(OsdRequest::Volume(VolumeChange::Step(-5))));
        assert_eq!(req(&["osd", "volume", "+5"]), Request::Osd(OsdRequest::Volume(VolumeChange::Step(5))));
        assert_eq!(req(&["osd", "volume", "mute"]), Request::Osd(OsdRequest::Volume(VolumeChange::Mute)));
        assert_eq!(req(&["osd", "mic-mute"]), Request::Osd(OsdRequest::MicMute));
        assert_eq!(req(&["osd", "brightness", "-10"]), Request::Osd(OsdRequest::Brightness(-10)));
        assert_eq!(req(&["launcher"]), Request::Launcher { op: PanelOp::Toggle, mode: None });
        for (arg, mode) in [
            ("calc", LauncherMode::Calc),
            ("files", LauncherMode::Files),
            ("run", LauncherMode::Run),
            ("emoji", LauncherMode::Emoji),
            ("web", LauncherMode::Web),
        ] {
            assert_eq!(req(&["launcher", "--mode", arg]), Request::Launcher { op: PanelOp::Toggle, mode: Some(mode) });
        }
        assert_eq!(
            req(&["launcher", "--mode", "run", "--open"]),
            Request::Launcher { op: PanelOp::Open, mode: Some(LauncherMode::Run) }
        );
        assert_eq!(req(&["clipboard"]), Request::Clipboard { op: PanelOp::Toggle });
        assert_eq!(req(&["notifications", "--close"]), Request::Notifications { op: PanelOp::Close });
        assert_eq!(req(&["quick-settings", "--open"]), Request::QuickSettings { op: PanelOp::Open });
        assert_eq!(req(&["calendar"]), Request::Calendar { op: PanelOp::Toggle });
        assert_eq!(req(&["calendar", "--close"]), Request::Calendar { op: PanelOp::Close });
        assert_eq!(req(&["session"]), Request::Session { op: PanelOp::Toggle });
        assert_eq!(req(&["cheatsheet", "--open"]), Request::Cheatsheet { op: PanelOp::Open });
        assert_eq!(req(&["media"]), Request::Media { op: PanelOp::Toggle });
        assert_eq!(req(&["media", "--open"]), Request::Media { op: PanelOp::Open });
        assert_eq!(req(&["media", "--close"]), Request::Media { op: PanelOp::Close });
        for (arg, key) in [
            ("play-pause", MediaControl::PlayPause),
            ("next", MediaControl::Next),
            ("previous", MediaControl::Previous),
            ("stop", MediaControl::Stop),
        ] {
            assert_eq!(req(&["media", arg]), Request::MediaControl(key));
        }
        assert!(parse(&["media", "next", "--open"]).is_err(), "a media key takes no panel flags");
        assert_eq!(req(&["theme", "pick"]), Request::ThemeSwitcher { op: PanelOp::Toggle });
        assert_eq!(req(&["theme", "pick", "--open"]), Request::ThemeSwitcher { op: PanelOp::Open });
        assert_eq!(req(&["theme", "pick", "--close"]), Request::ThemeSwitcher { op: PanelOp::Close });
        for (arg, op) in [
            ("hide", ScratchOp::Hide),
            ("show", ScratchOp::Show),
            ("toggle", ScratchOp::Toggle),
            ("open", ScratchOp::Open),
            ("close", ScratchOp::Close),
        ] {
            assert_eq!(req(&["scratch", arg]), Request::Scratch(op));
        }
        assert_eq!(req(&["scratch", "hide", "--id", "7"]), Request::Scratch(ScratchOp::HideWindow(7)));
        assert_eq!(req(&["scratch", "show", "--id", "7"]), Request::Scratch(ScratchOp::ShowWindow(7)));
        assert!(to_request(&parse(&["scratch", "toggle", "--id", "7"]).unwrap().cmd).is_err());
        for (arg, op) in [
            ("toggle", CanvasOp::Toggle),
            ("zoom-in", CanvasOp::ZoomIn),
            ("zoom-out", CanvasOp::ZoomOut),
            ("fit", CanvasOp::Fit),
            ("home", CanvasOp::Home),
        ] {
            assert_eq!(req(&["canvas", arg]), Request::Canvas(op));
        }
        assert_eq!(req(&["screenshot"]), Request::Screenshot(ScreenshotRequest::default()));
        assert_eq!(
            req(&["screenshot", "--region"]),
            Request::Screenshot(ScreenshotRequest { mode: Some(CaptureMode::Region), ..Default::default() })
        );
        assert_eq!(
            req(&["screenshot", "--window", "--open"]),
            Request::Screenshot(ScreenshotRequest {
                mode: Some(CaptureMode::Window),
                record: false,
                op: PanelOp::Open
            })
        );
        assert_eq!(
            req(&["screenshot", "--output", "--record"]),
            Request::Screenshot(ScreenshotRequest { mode: Some(CaptureMode::Output), record: true, op: PanelOp::Toggle })
        );
        assert_eq!(
            req(&["screenshot", "--record", "--close"]),
            Request::Screenshot(ScreenshotRequest { mode: None, record: true, op: PanelOp::Close })
        );
        assert_eq!(req(&["record", "stop"]), Request::Record(RecordOp::Stop));
        assert_eq!(req(&["record", "toggle"]), Request::Record(RecordOp::Toggle));
        assert_eq!(req(&["lock"]), Request::Lock);
        assert_eq!(req(&["status"]), Request::Status);
        assert_eq!(req(&["ping"]), Request::Ping);
        assert_eq!(req(&["dismiss"]), Request::Dismiss);
        assert_eq!(req(&["quit-shell"]), Request::Quit);
    }

    #[test]
    fn local_commands_have_no_request() {
        for args in [
            &["theme", "get"][..],
            &["theme", "set", "moss"],
            &["theme", "cycle"],
            &["theme", "apply"],
            &["app", "quit"],
            &["claude-rc"],
            &["sys", "generate-hardware-config", "h"],
            &["completions", "zsh"],
        ] {
            assert!(to_request(&parse(args).unwrap().cmd).unwrap().is_none(), "{args:?}");
        }
    }

    #[test]
    fn bad_arguments_are_rejected() {
        for args in [
            &[][..],
            &["frobnicate"],
            &["osd"],
            &["osd", "volume"],
            &["scratch", "explode"],
            &["canvas", "spin"],
            &["launcher", "--mode", "nope"],
            &["launcher", "--open", "--close"],
            &["screenshot", "--region", "--window"],
            &["screenshot", "--window", "--output"],
            &["record"],
            &["record", "pause"],
            &["theme", "set"],
            &["theme", "set", "neon"],
            &["completions", "tcsh"],
            &["sys", "flclashx-sync"],
            &["sys", "flclashx-sync", "--src", "/a"],
            &["sys", "fm-pick", "0", "0", "0"],
        ] {
            assert!(parse(args).is_err(), "{args:?} should not parse");
        }
        // Parsing succeeds but the value is rejected when mapped.
        assert!(to_request(&parse(&["osd", "volume", "loud"]).unwrap().cmd).is_err());
        assert!(to_request(&parse(&["osd", "brightness", "bright"]).unwrap().cmd).is_err());
    }

    #[test]
    fn negative_numbers_are_values_not_flags() {
        assert_eq!(req(&["osd", "volume", "-5"]), Request::Osd(OsdRequest::Volume(VolumeChange::Step(-5))));
        assert_eq!(req(&["osd", "brightness", "-5"]), Request::Osd(OsdRequest::Brightness(-5)));
    }

    #[test]
    fn theme_arguments() {
        match parse(&["theme", "set", "Moss", "--no-reload"]) {
            // Names are case-sensitive possible values; the label is lowercase.
            Err(_) => {}
            Ok(_) => panic!("clap possible values are case sensitive"),
        }
        let Cmd::Theme { cmd: ThemeCmd::Set { name, no_reload } } = parse(&["theme", "set", "moss", "--no-reload"]).unwrap().cmd
        else {
            panic!()
        };
        assert_eq!((name, no_reload), (ThemeName::Moss, true));
        for (label, theme) in ThemeName::ALL.map(|t| (t.label(), t)) {
            let Cmd::Theme { cmd: ThemeCmd::Set { name, .. } } = parse(&["theme", "set", label]).unwrap().cmd else { panic!() };
            assert_eq!(name, theme);
        }
        let Cmd::Theme { cmd: ThemeCmd::Apply { quiet, no_reload } } = parse(&["theme", "apply", "--quiet"]).unwrap().cmd else {
            panic!()
        };
        assert_eq!((quiet, no_reload), (true, false));
    }

    #[test]
    fn json_flag_is_global() {
        assert!(parse(&["--json", "status"]).unwrap().json);
        assert!(parse(&["status", "--json"]).unwrap().json);
        assert!(!parse(&["status"]).unwrap().json);
    }

    #[test]
    fn sys_arguments() {
        let Cmd::Sys { cmd: SysCmd::GenerateHardwareConfig { host, flake_dir } } =
            parse(&["sys", "generate-hardware-config", "box", "--flake-dir", "/f"]).unwrap().cmd
        else {
            panic!()
        };
        assert_eq!((host.as_str(), flake_dir), ("box", Some(PathBuf::from("/f"))));
        let Cmd::Sys { cmd: SysCmd::FlclashxSync { src, dst } } =
            parse(&["sys", "flclashx-sync", "--src", "/s", "--dst", "/d"]).unwrap().cmd
        else {
            panic!()
        };
        assert_eq!((src, dst), (PathBuf::from("/s"), PathBuf::from("/d")));
        let Cmd::Sys { cmd: SysCmd::FmPick { multiple, directory, save, path, outfile, debug } } =
            parse(&["sys", "fm-pick", "1", "0", "0", "/home/u", "/tmp/out", "1"]).unwrap().cmd
        else {
            panic!()
        };
        assert_eq!((multiple.as_str(), directory.as_str(), save.as_str()), ("1", "0", "0"));
        assert_eq!((path.as_str(), outfile.as_str(), debug.as_deref()), ("/home/u", "/tmp/out", Some("1")));
        // debug is optional, path may be empty.
        let Cmd::Sys { cmd: SysCmd::FmPick { path, debug, .. } } =
            parse(&["sys", "fm-pick", "0", "0", "0", "", "/tmp/out"]).unwrap().cmd
        else {
            panic!()
        };
        assert_eq!((path.as_str(), debug), ("", None));
    }
}
