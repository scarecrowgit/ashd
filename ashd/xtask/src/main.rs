//! `cargo xtask <task>` — the project's test harness (owner: verification). All Rust, no
//! shell scripts. See `xtask/README.md`.
//!
//! - `nested start|stop|env|run|list|gc`: the headless sway → niri session
//! - `shot`: screenshot of a nested session
//! - `shot-check`: ashd-shell frame geometry and token colours, by pixel probes
//! - `smoke`: every IPC request, shell liveness, a screenshot per step
//! - `motion`: slow-motion bursts: frames exist between the end states, reversal settles, idle draws nothing
//! - `fullscreen`: fullscreen workspaces and edge reveal against the ashd niri fork
//! - `ci`: check, clippy, test, shot-check, smoke, fullscreen

mod check;
mod ci;
mod fullscreen;
mod harness;
mod motion;
mod nested;
mod shot;
mod smoke;
mod util;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xtask", about = "ashd verification harness")]
struct Args {
    #[command(subcommand)]
    task: Task,
}

#[derive(Subcommand)]
enum Task {
    /// Headless nested niri session (sway headless → niri winit).
    Nested {
        #[command(subcommand)]
        cmd: nested::Cmd,
    },
    /// Screenshot of a nested session's output.
    Shot(shot::ShotArgs),
    /// Start ashd-shell in a nested session and check frame geometry / token colours.
    ShotCheck(check::CheckArgs),
    /// Drive every IPC request against ashd-shell in a nested session.
    Smoke(smoke::SmokeArgs),
    /// Slow-motion burst screenshots of every surface's open / close, reversal and idle frames.
    Motion(motion::MotionArgs),
    /// Fullscreen window: the rail's app entry, edge reveal of the rail and bar, overlays over it (ashd niri fork).
    Fullscreen(fullscreen::FullscreenArgs),
    /// check + clippy + test + shot-check + smoke + fullscreen.
    Ci(ci::CiArgs),
}

fn main() -> ExitCode {
    let result = match Args::parse().task {
        Task::Nested { cmd } => nested::dispatch(cmd),
        Task::Shot(a) => shot::dispatch(a),
        Task::ShotCheck(a) => check::dispatch(a),
        Task::Smoke(a) => smoke::dispatch(a),
        Task::Motion(a) => motion::dispatch(a),
        Task::Fullscreen(a) => fullscreen::dispatch(a),
        Task::Ci(a) => ci::dispatch(a),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("xtask: {e:#}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        super::Args::command().debug_assert();
    }
}
