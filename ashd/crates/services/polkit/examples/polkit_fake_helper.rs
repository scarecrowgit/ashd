//! A fake `polkit-agent-helper-1` speaking the setuid-helper protocol, for tests only:
//! `polkit_fake_helper <user>`, cookie on stdin, one `Password: ` prompt, `SUCCESS` for
//! the password `hunter2`, else `FAILURE`. It authenticates nothing and talks to no bus.

use std::io::{BufRead as _, Write as _};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(_user), None) = (args.next(), args.next()) else {
        eprintln!("usage: polkit_fake_helper <user>  (cookie on stdin)");
        std::process::exit(1);
    };
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let mut out = std::io::stdout().lock();
    let Some(Ok(_cookie)) = lines.next() else { std::process::exit(1) };
    let _ = writeln!(out, "PAM_PROMPT_ECHO_OFF Password: ");
    let _ = out.flush();
    let ok = matches!(lines.next(), Some(Ok(answer)) if answer == "hunter2");
    let _ = writeln!(out, "{}", if ok { "SUCCESS" } else { "FAILURE" });
    let _ = out.flush();
}
