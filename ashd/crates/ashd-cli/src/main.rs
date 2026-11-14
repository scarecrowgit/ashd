//! `ashd` — the CLI every keybind calls. Shell features go over ashd-ipc
//! (`$XDG_RUNTIME_DIR/ashd/shell.sock`); theme files, niri helpers and `sys` tasks run
//! locally. Exit status: 0 ok, 1 error, 2 ashd-shell not running.
//!
//! Owner: plumbing.

mod canvas;
mod claude_rc;
mod cli;
mod fm_pick;
mod lock;
mod osd;
mod scratch;
mod sys;
mod theme;
mod ui;

use std::process::ExitCode;

use anyhow::Context as _;
use clap::{CommandFactory as _, Parser as _};
use cli::{AppCmd, Cli, Cmd, SysCmd, ThemeCmd};
use ashd_ipc::{IpcError, Request, Response};
use ui::{EXIT_ERROR, EXIT_NOT_RUNNING, EXIT_OK, exit, friendly_ipc_error};

fn main() -> ExitCode {
    // clap exits with 2 on usage errors, which here means "shell not running": use 1.
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            let code = if e.use_stderr() { EXIT_ERROR } else { EXIT_OK };
            let _ = e.print();
            return exit(code);
        }
    };
    match run(cli) {
        Ok(code) => exit(code),
        Err(e) => {
            eprintln!("ashd: {e:#}");
            exit(EXIT_ERROR)
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<u8> {
    let json = cli.json;
    if let Some(req) = cli::to_request(&cli.cmd)? {
        return shell_command(req, json);
    }
    match cli.cmd {
        Cmd::Theme { cmd } => theme_command(cmd, json),
        Cmd::App { cmd: AppCmd::Quit } => {
            let n = ashd_niri::blocking::close_focused_app_windows().map_err(|e| match e {
                ashd_niri::NiriError::NoSocket => anyhow::anyhow!("not running under niri ($NIRI_SOCKET is not set)"),
                e => anyhow::Error::new(e).context("closing windows"),
            })?;
            if json {
                println!("{{\"closed\":{n}}}");
            }
            Ok(EXIT_OK)
        }
        Cmd::Sys { cmd } => sys_command(cmd),
        Cmd::Fm { args } => Err(theme::exec_fm(&args)),
        Cmd::ClaudeRc => {
            for line in claude_rc::run(&std::env::current_dir().context("reading the current directory")?)? {
                println!("{line}");
            }
            Ok(EXIT_OK)
        }
        Cmd::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "ashd", &mut std::io::stdout());
            Ok(EXIT_OK)
        }
        _ => unreachable!("mapped to a request by cli::to_request"),
    }
}

/// Send `req` to the shell and report the answer.
fn shell_command(req: Request, json: bool) -> anyhow::Result<u8> {
    match ashd_ipc::request(&req) {
        Ok(resp) => Ok(report(&req, &resp, json)),
        Err(IpcError::NotRunning(socket)) => not_running(&req, &socket, json),
        // `quit-shell` may close the connection before it answers.
        Err(IpcError::Closed) if req == Request::Quit => Ok(EXIT_OK),
        Err(e) => Err(anyhow::anyhow!(friendly_ipc_error(&e))),
    }
}

/// What to do when no shell is listening.
fn not_running(req: &Request, socket: &std::path::Path, json: bool) -> anyhow::Result<u8> {
    match req {
        // Media keys keep working without the shell.
        Request::Osd(osd) if osd::has_fallback(osd) => {
            osd::fallback(osd).context("ashd-shell is not running and the wpctl fallback failed")?;
            Ok(EXIT_OK)
        }
        Request::Lock => Err(lock::exec()),
        // Canvas actions are the compositor's: they work without the shell.
        Request::Canvas(op) => {
            canvas::run(*op)?;
            Ok(EXIT_OK)
        }
        // Hiding / showing a window is the compositor's job: it works without the shell.
        Request::Scratch(op) if scratch::is_local(*op) => {
            scratch::run(*op)?;
            Ok(EXIT_OK)
        }
        _ => {
            if json && *req == Request::Status {
                println!("{{\"running\":false}}");
            }
            eprintln!("ashd: {}", ui::not_running_message(socket));
            Ok(EXIT_NOT_RUNNING)
        }
    }
}

/// Print a shell answer; returns the exit code.
fn report(req: &Request, resp: &Response, json: bool) -> u8 {
    if json {
        match (resp, req) {
            // The bare status object, not wrapped in the response enum.
            (Response::Status(s), _) => println!("{}", serde_json::to_string_pretty(s).unwrap_or_default()),
            _ => println!("{}", serde_json::to_string(resp).unwrap_or_default()),
        }
        return if matches!(resp, Response::Error(_)) { EXIT_ERROR } else { EXIT_OK };
    }
    match resp {
        Response::Ok => EXIT_OK,
        Response::Pong { version, protocol, pid } => {
            println!("ashd-shell {version} (protocol {protocol}, pid {pid})");
            if *protocol != ashd_ipc::PROTOCOL_VERSION {
                eprintln!(
                    "ashd: warning: ashd-shell speaks protocol {protocol}, this ashd speaks {}",
                    ashd_ipc::PROTOCOL_VERSION
                );
            }
            EXIT_OK
        }
        Response::Theme(t) => {
            println!("{t}");
            EXIT_OK
        }
        Response::Status(s) => {
            print!("{}", ui::format_status(s));
            EXIT_OK
        }
        Response::Error(e) => {
            eprintln!("ashd: {e}");
            EXIT_ERROR
        }
    }
}

fn theme_command(cmd: ThemeCmd, json: bool) -> anyhow::Result<u8> {
    match cmd {
        ThemeCmd::Get => {
            println!("{}", theme::current());
            Ok(EXIT_OK)
        }
        // Mapped to `Request::ThemeSwitcher` by `cli::to_request`.
        ThemeCmd::Pick(_) => unreachable!("mapped to a request by cli::to_request"),
        ThemeCmd::Set { name, no_reload } => theme_set(name, !no_reload, json),
        ThemeCmd::Cycle { no_reload } => theme_set(theme::current().next(), !no_reload, json),
        ThemeCmd::Apply { quiet, no_reload } => {
            let (_, written, reload) = theme::apply(!no_reload)?;
            if !quiet {
                for p in written {
                    println!("{}", p.display());
                }
            }
            for w in reload.iter().flat_map(theme::reload_warnings) {
                eprintln!("ashd: warning: {w}");
            }
            Ok(EXIT_OK)
        }
    }
}

fn theme_set(name: ashd_tokens::ThemeName, reload: bool, json: bool) -> anyhow::Result<u8> {
    let out = theme::set(name, reload)?;
    for w in out.reload.iter().flat_map(theme::reload_warnings) {
        eprintln!("ashd: warning: {w}");
    }
    if json {
        println!("{}", serde_json::to_string(&Response::Theme(out.theme)).unwrap_or_default());
    } else {
        println!("{}", out.theme);
    }
    if let Some(e) = out.shell_error {
        eprintln!("ashd: theme saved, but ashd-shell could not switch: {e}");
        return Ok(EXIT_ERROR);
    }
    Ok(EXIT_OK)
}

fn sys_command(cmd: SysCmd) -> anyhow::Result<u8> {
    match cmd {
        SysCmd::GenerateHardwareConfig { host, flake_dir } => {
            let path = sys::generate_hardware_config(&host, &flake_dir.unwrap_or_default())?;
            println!("Wrote {}", path.display());
        }
        SysCmd::FlclashxSync { src, dst } => {
            if sys::flclashx_sync(&src, &dst)? == sys::SyncOutcome::Synced {
                println!("flclashx: installed {} into {}", src.display(), dst.display());
            }
        }
        SysCmd::FmPick { multiple, directory, save, path, outfile, debug } => {
            let home = std::env::var("HOME").unwrap_or_default();
            let args = fm_pick::fm_args(&multiple, &directory, &save, &path, &outfile, &home);
            return Err(fm_pick::exec(&args, debug.as_deref() == Some("1")));
        }
    }
    Ok(EXIT_OK)
}
