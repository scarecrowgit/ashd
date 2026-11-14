//! `cargo xtask ci`: check, clippy, test, then the nested steps; prints a summary table.

use std::{
    process::{Command, ExitCode},
    time::{Duration, Instant},
};

use anyhow::Result;
use clap::Args;

use crate::{check, smoke, util};

#[derive(Args)]
pub struct CiArgs {
    /// Skip the nested-session steps (shot-check, smoke).
    #[arg(long)]
    fast: bool,
    /// niri config for the nested steps (with `NIRI=` the ashd fork: a config declaring
    /// `workspace "scratch" { hidden }` lets smoke hide / show a window).
    #[arg(long)]
    niri_config: Option<std::path::PathBuf>,
}

struct Row {
    name: &'static str,
    ok: bool,
    took: Duration,
}

fn cargo(args: &[&str]) -> bool {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    eprintln!("\n$ cargo {}", args.join(" "));
    Command::new(cargo)
        .current_dir(util::workspace_root())
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn timed(rows: &mut Vec<Row>, name: &'static str, f: impl FnOnce() -> bool) -> bool {
    let t = Instant::now();
    let ok = f();
    rows.push(Row { name, ok, took: t.elapsed() });
    ok
}

pub fn dispatch(args: CiArgs) -> Result<ExitCode> {
    let mut rows: Vec<Row> = Vec::new();
    timed(&mut rows, "cargo check --workspace --all-targets", || cargo(&["check", "--workspace", "--all-targets"]));
    timed(&mut rows, "cargo clippy --workspace --all-targets -D warnings", || {
        cargo(&["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"])
    });
    timed(&mut rows, "cargo test --workspace", || cargo(&["test", "--workspace"]));
    if !args.fast {
        let built = timed(&mut rows, "cargo build -p ashd-shell -p ashd-cli", || crate::harness::build_bins().is_ok());
        if built {
            let mut c = check::CheckArgs::defaults();
            c.no_build = true;
            c.theme = "all".into();
            c.niri_config = args.niri_config.clone();
            timed(&mut rows, "shot-check --theme all", || report(check::run(&c)));
            let mut s = smoke::SmokeArgs::defaults();
            s.no_build = true;
            s.niri_config = args.niri_config.clone();
            timed(&mut rows, "smoke", || report(smoke::run(&s)));
        }
    }
    println!("\n{:<58} {:<6} time", "step", "result");
    for r in &rows {
        println!("{:<58} {:<6} {:.1}s", r.name, if r.ok { "ok" } else { "FAIL" }, r.took.as_secs_f32());
    }
    let failed = rows.iter().filter(|r| !r.ok).count();
    println!("\nci: {} steps, {failed} failed", rows.len());
    Ok(if failed == 0 { ExitCode::SUCCESS } else { ExitCode::from(1) })
}

fn report(r: Result<bool>) -> bool {
    match r {
        Ok(b) => b,
        Err(e) => {
            eprintln!("xtask: {e:#}");
            false
        }
    }
}
