//! polkit's agent-helper line protocol (polkit 127, `src/polkitagent/polkitagenthelper-pam.c`
//! and `polkitagentsession.c`).
//!
//! The agent writes, each terminated by `\n`:
//! - socket-activated helper (`/run/polkit/agent-helper.socket`): the user name, then the
//!   cookie, then one line per response;
//! - setuid helper (`polkit-agent-helper-1 <user>`): the cookie on stdin, then responses.
//!
//! The helper writes lines `PAM_PROMPT_ECHO_OFF <msg>`, `PAM_PROMPT_ECHO_ON <msg>`,
//! `PAM_ERROR_MSG <msg>`, `PAM_TEXT_INFO <msg>` (the message `g_strescape`d) and finally
//! `SUCCESS` or `FAILURE`. On success the helper itself — running as root — has already
//! called `AuthenticationAgentResponse2` on the authority (only uid 0 may call it), so the
//! agent only completes its `BeginAuthentication` call.

/// The longest response the helper reads in one line (`PAM_MAX_RESP_SIZE` is 512, `fgets`
/// keeps room for the newline and the NUL). Longer input would be split into two
/// responses, so it is refused before it is written.
pub const MAX_RESPONSE: usize = 510;

/// One line from the helper.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HelperLine {
    /// A prompt; `echo` = the answer may be shown (`PAM_PROMPT_ECHO_ON`).
    Prompt { text: String, echo: bool },
    Error(String),
    Info(String),
    Success,
    Failure,
    /// Anything else (polkit's own agent treats it as a failure).
    Unknown(String),
}

/// Parse one line (without or with its trailing newline), as `io_watch_have_data` does:
/// unescape first, then match the prefix.
pub fn parse_line(raw: &[u8]) -> HelperLine {
    let raw = raw.strip_suffix(b"\n").unwrap_or(raw);
    let line = strcompress(raw);
    let rest = |prefix: &str| line.strip_prefix(prefix).map(str::to_owned);
    if let Some(s) = rest("PAM_PROMPT_ECHO_OFF ") {
        HelperLine::Prompt { text: s, echo: false }
    } else if let Some(s) = rest("PAM_PROMPT_ECHO_ON ") {
        HelperLine::Prompt { text: s, echo: true }
    } else if let Some(s) = rest("PAM_ERROR_MSG ") {
        HelperLine::Error(s)
    } else if let Some(s) = rest("PAM_TEXT_INFO ") {
        HelperLine::Info(s)
    } else if line.starts_with("SUCCESS") {
        HelperLine::Success
    } else if line.starts_with("FAILURE") {
        HelperLine::Failure
    } else {
        HelperLine::Unknown(line)
    }
}

/// `g_strcompress`: undo `g_strescape` (`\b \f \n \r \t \v \\ \"` and `\ooo` octal), then
/// decode as UTF-8 (lossy — the text is only displayed).
pub fn strcompress(s: &[u8]) -> String {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        i += 1;
        if c != b'\\' || i >= s.len() {
            out.push(c);
            continue;
        }
        let e = s[i];
        i += 1;
        match e {
            b'0'..=b'7' => {
                let mut v: u32 = u32::from(e - b'0');
                let mut n = 1;
                while n < 3 && i < s.len() && (b'0'..=b'7').contains(&s[i]) {
                    v = v * 8 + u32::from(s[i] - b'0');
                    i += 1;
                    n += 1;
                }
                out.push((v & 0xff) as u8);
            }
            b'b' => out.push(0x08),
            b'f' => out.push(0x0c),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'v' => out.push(0x0b),
            other => out.push(other),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `g_strescape` (no exceptions): the inverse of [`strcompress`]. Used by the fake helper
/// in tests and the nested-session example.
pub fn strescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            0x08 => out.push_str("\\b"),
            0x0c => out.push_str("\\f"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x0b => out.push_str("\\v"),
            b'\\' => out.push_str("\\\\"),
            b'"' => out.push_str("\\\""),
            b if !(0x20..0x7f).contains(&b) => out.push_str(&format!("\\{b:03o}")),
            b => out.push(b as char),
        }
    }
    out
}

/// Why a response cannot be sent to the helper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResponseError {
    TooLong,
    Newline,
}

impl std::fmt::Display for ResponseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ResponseError::TooLong => "The password is too long.",
            ResponseError::Newline => "The password cannot contain a line break.",
        })
    }
}

/// Check a response before it is written as one line.
pub fn check_response(response: &str) -> Result<(), ResponseError> {
    if response.len() > MAX_RESPONSE {
        Err(ResponseError::TooLong)
    } else if response.contains(['\n', '\r', '\0']) {
        Err(ResponseError::Newline)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_line_kind() {
        assert_eq!(parse_line(b"PAM_PROMPT_ECHO_OFF Password: \n"), HelperLine::Prompt { text: "Password: ".into(), echo: false });
        assert_eq!(parse_line(b"PAM_PROMPT_ECHO_ON Login:"), HelperLine::Prompt { text: "Login:".into(), echo: true });
        assert_eq!(parse_line(b"PAM_ERROR_MSG Account locked\n"), HelperLine::Error("Account locked".into()));
        assert_eq!(parse_line(b"PAM_TEXT_INFO Place your finger\n"), HelperLine::Info("Place your finger".into()));
        assert_eq!(parse_line(b"SUCCESS\n"), HelperLine::Success);
        assert_eq!(parse_line(b"FAILURE\n"), HelperLine::Failure);
        assert_eq!(parse_line(b"what\n"), HelperLine::Unknown("what".into()));
        // a prefix without its space is not a prompt
        assert_eq!(parse_line(b"PAM_PROMPT_ECHO_OFF"), HelperLine::Unknown("PAM_PROMPT_ECHO_OFF".into()));
    }

    #[test]
    fn unescapes_like_g_strcompress() {
        assert_eq!(strcompress(br#"a\nb\t\"q\"\\"#), "a\nb\t\"q\"\\");
        // UTF-8 bytes escaped as octal by g_strescape
        assert_eq!(strcompress(br"Pa\303\237wort:"), "Paßwort:");
        assert_eq!(strcompress(br"\101\0x"), "A\0x");
        // trailing backslash is kept
        assert_eq!(strcompress(br"x\"), "x\\");
    }

    #[test]
    fn escape_round_trips() {
        for s in ["Password: ", "Paßwort:\n", "tab\there \"quoted\" \\ back", "\u{1}ctl"] {
            let e = strescape(s);
            assert!(!e.contains('\n'));
            assert_eq!(strcompress(e.as_bytes()), s);
        }
        let prompt = format!("PAM_PROMPT_ECHO_OFF {}\n", strescape("Mot de passe :"));
        assert_eq!(parse_line(prompt.as_bytes()), HelperLine::Prompt { text: "Mot de passe :".into(), echo: false });
    }

    #[test]
    fn responses_must_fit_one_line() {
        assert_eq!(check_response("hunter2"), Ok(()));
        assert_eq!(check_response(""), Ok(()));
        assert_eq!(check_response("a\nb"), Err(ResponseError::Newline));
        assert_eq!(check_response(&"x".repeat(MAX_RESPONSE)), Ok(()));
        assert_eq!(check_response(&"x".repeat(MAX_RESPONSE + 1)), Err(ResponseError::TooLong));
    }
}
