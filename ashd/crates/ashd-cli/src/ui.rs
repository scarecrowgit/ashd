//! Output formatting and exit codes.

use std::process::ExitCode;

use ashd_ipc::{IpcError, ShellStatus};

pub const EXIT_OK: u8 = 0;
pub const EXIT_ERROR: u8 = 1;
pub const EXIT_NOT_RUNNING: u8 = 2;

pub fn exit(code: u8) -> ExitCode {
    ExitCode::from(code)
}

pub fn not_running_message(socket: &std::path::Path) -> String {
    format!("ashd-shell is not running (no listener on {})", socket.display())
}

/// One line a person can act on.
pub fn friendly_ipc_error(e: &IpcError) -> String {
    match e {
        IpcError::NotRunning(p) => not_running_message(p),
        e if e.is_timeout() => {
            format!("ashd-shell did not answer within {} s (is it hung?)", ashd_ipc::Client::TIMEOUT.as_secs())
        }
        IpcError::Closed => "ashd-shell closed the connection without answering".into(),
        IpcError::Protocol(e) => format!("ashd-shell sent something unreadable: {e}"),
        other => other.to_string(),
    }
}

/// `ashd status` for humans.
pub fn format_status(s: &ShellStatus) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "ashd-shell {}", s.version);
    let _ = writeln!(out, "theme:  {}", s.theme);
    let _ = writeln!(out, "niri:   {}", if s.niri_connected { "connected" } else { "not connected" });
    let _ = writeln!(out, "modal:  {}", s.modal.as_deref().unwrap_or("none"));
    if s.surfaces.is_empty() {
        let _ = writeln!(out, "surfaces: none");
    } else {
        let _ = writeln!(out, "surfaces:");
        let kind_w = s.surfaces.iter().map(|x| x.kind.len()).max().unwrap_or(0);
        let out_w = s.surfaces.iter().map(|x| x.output.len()).max().unwrap_or(0);
        for x in &s.surfaces {
            let _ = writeln!(out, "  {:<kind_w$}  {:<out_w$}  {} x {}", x.kind, x.output, x.width, x.height);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use ashd_ipc::SurfaceStatus;
    use ashd_tokens::ThemeName;

    use super::*;

    fn surface(kind: &str, output: &str, w: f32, h: f32) -> SurfaceStatus {
        SurfaceStatus { kind: kind.into(), output: output.into(), width: w, height: h }
    }

    #[test]
    fn status_text() {
        let s = ShellStatus {
            version: "0.1.0".into(),
            theme: ThemeName::Moss,
            niri_connected: true,
            modal: Some("launcher".into()),
            surfaces: vec![surface("bar", "DP-1", 1920.0, 46.0), surface("overlay-modal", "DP-1", 1920.0, 1080.0)],
        };
        assert_eq!(
            format_status(&s),
            "ashd-shell 0.1.0\ntheme:  moss\nniri:   connected\nmodal:  launcher\nsurfaces:\n  \
             bar            DP-1  1920 x 46\n  overlay-modal  DP-1  1920 x 1080\n"
        );
        let empty = ShellStatus { surfaces: vec![], modal: None, niri_connected: false, ..s };
        assert_eq!(
            format_status(&empty),
            "ashd-shell 0.1.0\ntheme:  moss\nniri:   not connected\nmodal:  none\nsurfaces: none\n"
        );
    }

    #[test]
    fn friendly_errors() {
        let p = std::path::PathBuf::from("/run/user/1/ashd/shell.sock");
        assert_eq!(
            friendly_ipc_error(&IpcError::NotRunning(p)),
            "ashd-shell is not running (no listener on /run/user/1/ashd/shell.sock)"
        );
        let t = IpcError::Io(std::io::Error::new(std::io::ErrorKind::TimedOut, "x"));
        assert!(friendly_ipc_error(&t).contains("did not answer within 5 s"));
        assert!(friendly_ipc_error(&IpcError::Closed).contains("without answering"));
        assert_eq!(friendly_ipc_error(&IpcError::Remote("nope".into())), "ashd-shell: nope");
    }
}
