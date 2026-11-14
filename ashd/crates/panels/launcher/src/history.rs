//! `>` command history: `<state dir>/launcher-run-history`, one `<unix secs>\t<command>`
//! line per run, appended; compacted when it grows. Disk access only from background tasks.

use std::{
    io::Write as _,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_LINES: usize = 400;
const KEEP: usize = 200;

pub fn path() -> PathBuf {
    ashd_svc_apps::state_dir().join("launcher-run-history")
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Distinct commands, most recently run first, with the time of the last run.
pub fn parse(text: &str) -> Vec<(String, u64)> {
    let mut out: Vec<(String, u64)> = Vec::new();
    for line in text.lines().rev() {
        let Some((secs, cmd)) = line.split_once('\t') else { continue };
        let (Ok(secs), cmd) = (secs.parse::<u64>(), cmd.trim()) else { continue };
        if !cmd.is_empty() && !out.iter().any(|(c, _)| c == cmd) {
            out.push((cmd.to_string(), secs));
        }
    }
    out.sort_by_key(|b| std::cmp::Reverse(b.1));
    out
}

pub fn load() -> Vec<(String, u64)> {
    std::fs::read_to_string(path()).map(|t| parse(&t)).unwrap_or_default()
}

pub fn append(command: &str, at: u64) {
    let command = command.replace(['\n', '\t'], " ");
    let p = path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
        let _ = writeln!(f, "{at}\t{command}");
    }
    if let Ok(text) = std::fs::read_to_string(&p)
        && text.lines().count() > MAX_LINES
    {
        let lines: Vec<&str> = text.lines().collect();
        let tail = lines[lines.len() - KEEP..].join("\n");
        let _ = std::fs::write(&p, tail + "\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_first_deduplicated() {
        let h = parse("10\tls\n20\thtop\n30\tls\nbogus\n40\t\n");
        assert_eq!(h, vec![("ls".to_string(), 30), ("htop".to_string(), 20)]);
    }
}
