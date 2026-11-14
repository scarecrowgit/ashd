//! Authentication for ashd-lock: PAM service `ashd-lock` (`/etc/pam.d/ashd-lock`, from
//! `modules/desktop/lock.nix`) through `pam-client`, run on a worker thread.
//!
//! The password lives in `Zeroizing` buffers. One unavoidable copy: pam-client takes the
//! conversation reply as a `CString`, `strdup`s it for libpam (which wipes and frees
//! its copy) and drops ours — `CString`'s drop only clears the first byte. We build that
//! `CString` in place from the zeroizing buffer (no reallocation) to keep it to one.
//!
//! Feature `test-auth` (debug builds only, never in the Nix build) replaces PAM with a
//! comparison against `$ASHD_LOCK_TEST_PASSWORD` so the nested test session can unlock.

use std::{
    ffi::{CStr, CString},
    sync::mpsc,
};

use zeroize::Zeroizing;

#[cfg(all(feature = "test-auth", not(debug_assertions)))]
compile_error!(
    "ashd-lock: the `test-auth` feature replaces PAM with an environment-variable password; it must never be \
     enabled in a release build"
);

/// PAM service name (`/etc/pam.d/ashd-lock`, provided by the NixOS module).
pub const SERVICE: &str = "ashd-lock";

/// Outcome of one attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthOutcome {
    Ok,
    /// Wrong password (counts as a failed attempt).
    Denied,
    /// Any other refusal, with the text to show (account expired, PAM broken, …).
    Error(String),
}

/// Text PAM wants shown (info messages) is forwarded as `Info`.
#[derive(Debug)]
pub enum AuthEvent {
    Info(String),
    Done(AuthOutcome),
}

/// Conversation: answers every hidden prompt with the password; visible prompts get the
/// user name; info/error texts are collected for the UI.
struct Conversation {
    user: String,
    password: Zeroizing<Vec<u8>>,
    info: Vec<String>,
}

impl pam_client::ConversationHandler for Conversation {
    fn prompt_echo_on(&mut self, _prompt: &CStr) -> Result<CString, pam_client::ErrorCode> {
        CString::new(self.user.clone()).map_err(|_| pam_client::ErrorCode::CONV_ERR)
    }

    fn prompt_echo_off(&mut self, _prompt: &CStr) -> Result<CString, pam_client::ErrorCode> {
        // A copy of the zeroizing buffer with room for the NUL, so CString::new does not
        // reallocate; see the module docs for its fate.
        let mut bytes = Vec::with_capacity(self.password.len() + 1);
        bytes.extend_from_slice(&self.password);
        CString::new(bytes).map_err(|e| {
            let mut v = e.into_vec();
            zeroize::Zeroize::zeroize(&mut v);
            pam_client::ErrorCode::CONV_ERR
        })
    }

    fn text_info(&mut self, msg: &CStr) {
        self.info.push(msg.to_string_lossy().into_owned());
    }

    fn error_msg(&mut self, msg: &CStr) {
        self.info.push(msg.to_string_lossy().into_owned());
    }
}

#[cfg_attr(feature = "test-auth", allow(dead_code))]
fn pam_authenticate(user: &str, password: Zeroizing<String>, info: &mut Vec<String>) -> AuthOutcome {
    use pam_client::{Context, ErrorCode, Flag};
    let conv = Conversation {
        user: user.to_string(),
        password: Zeroizing::new(password.as_bytes().to_vec()),
        info: Vec::new(),
    };
    drop(password);
    let mut ctx = match Context::new(SERVICE, Some(user), conv) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("pam_start({SERVICE}) failed: {e}");
            return AuthOutcome::Error(format!("PAM service {SERVICE} unavailable"));
        }
    };
    let result = ctx.authenticate(Flag::NONE).and_then(|()| ctx.acct_mgmt(Flag::NONE));
    info.append(&mut ctx.conversation_mut().info);
    match result {
        Ok(()) => AuthOutcome::Ok,
        Err(e) => match e.code() {
            ErrorCode::AUTH_ERR | ErrorCode::CRED_INSUFFICIENT => AuthOutcome::Denied,
            // Correct password, expired: the session is already this user's — unlock, and
            // leave changing the password to a terminal.
            ErrorCode::NEW_AUTHTOK_REQD => {
                tracing::warn!("password expired for {user}; unlocking anyway (change it with `passwd`)");
                AuthOutcome::Ok
            }
            ErrorCode::ACCT_EXPIRED => AuthOutcome::Error("Account expired".into()),
            ErrorCode::PERM_DENIED => AuthOutcome::Error("Permission denied".into()),
            ErrorCode::USER_UNKNOWN => AuthOutcome::Error(format!("Unknown user {user}")),
            ErrorCode::MAXTRIES => AuthOutcome::Error("Too many attempts (PAM)".into()),
            ErrorCode::AUTHINFO_UNAVAIL => AuthOutcome::Error("Authentication service unavailable".into()),
            code => {
                tracing::error!("PAM error {code:?}: {e}");
                AuthOutcome::Error(format!("PAM error: {}", e.message().unwrap_or("unknown")))
            }
        },
    }
}

#[cfg(feature = "test-auth")]
fn test_authenticate(password: Zeroizing<String>) -> AuthOutcome {
    std::thread::sleep(std::time::Duration::from_millis(400)); // let the spinner show
    match std::env::var("ASHD_LOCK_TEST_PASSWORD") {
        Ok(expected) if !expected.is_empty() => {
            let expected = Zeroizing::new(expected);
            if constant_time_eq(expected.as_bytes(), password.as_bytes()) {
                AuthOutcome::Ok
            } else {
                AuthOutcome::Denied
            }
        }
        _ => AuthOutcome::Error("test-auth: ASHD_LOCK_TEST_PASSWORD is not set".into()),
    }
}

#[cfg(feature = "test-auth")]
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Authenticate `user` with `password`. Blocking: run off the render loop.
pub fn authenticate(user: &str, password: Zeroizing<String>, info: &mut Vec<String>) -> AuthOutcome {
    #[cfg(feature = "test-auth")]
    {
        let _ = (user, &info);
        test_authenticate(password)
    }
    #[cfg(not(feature = "test-auth"))]
    {
        pam_authenticate(user, password, info)
    }
}

/// Worker thread: receives passwords, sends events back through `reply`.
pub fn spawn_worker(
    user: String,
    reply: impl Fn(AuthEvent) + Send + 'static,
) -> std::io::Result<mpsc::Sender<Zeroizing<String>>> {
    let (tx, rx) = mpsc::channel::<Zeroizing<String>>();
    std::thread::Builder::new().name("ashd-lock-auth".into()).spawn(move || {
        for password in rx {
            let mut info = Vec::new();
            let outcome = authenticate(&user, password, &mut info);
            for i in info {
                reply(AuthEvent::Info(i));
            }
            reply(AuthEvent::Done(outcome));
        }
    })?;
    Ok(tx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversation_answers_hidden_prompts_with_the_password() {
        use pam_client::ConversationHandler as _;
        let mut c = Conversation { user: "me".into(), password: Zeroizing::new(b"s3cret".to_vec()), info: vec![] };
        let p = c.prompt_echo_off(c"Password: ").unwrap();
        assert_eq!(p.as_bytes(), b"s3cret");
        assert_eq!(c.prompt_echo_on(c"login: ").unwrap().as_bytes(), b"me");
        c.text_info(c"expires soon");
        assert_eq!(c.info, vec!["expires soon".to_string()]);
    }

    #[cfg(feature = "test-auth")]
    #[test]
    fn test_auth_compares_against_env() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }
}
