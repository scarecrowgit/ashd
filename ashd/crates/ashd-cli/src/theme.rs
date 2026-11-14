//! `ashd theme …` and `ashd fm`: the theme sequence itself (persist → write the selected
//! consumers' files → reload unless `ASHD_NO_RELOAD` / `ASHD_FAKE_POWER`) is `ashd-theme`'s;
//! this module adds telling the shell.

use std::{os::unix::process::CommandExt as _, path::PathBuf, process::Command};

use anyhow::Context as _;
use ashd_ipc::{IpcError, Request, Response, ThemeRequest};
use ashd_theme::ReloadReport;
use ashd_tokens::{ThemeName, state};

/// `ashd theme set`: the theme the user should see plus the shell's complaint, if any (the
/// files are written regardless).
pub struct SetOutcome {
    pub theme: ThemeName,
    pub reload: Option<ReloadReport>,
    pub shell_error: Option<String>,
}

pub fn set(name: ThemeName, reload: bool) -> anyhow::Result<SetOutcome> {
    let applied = ashd_theme::set(name, reload).context("writing the theme files")?;
    // A shell that is not running is fine: it reads the persisted theme when it starts.
    let shell_error = match ashd_ipc::request(&Request::Theme(ThemeRequest::Set(name))) {
        Ok(Response::Error(e)) => Some(e),
        Ok(_) | Err(IpcError::NotRunning(_)) => None,
        Err(e) => Some(crate::ui::friendly_ipc_error(&e)),
    };
    Ok(SetOutcome { theme: name, reload: applied.reload, shell_error })
}

/// The theme `ashd theme get` / `cycle` start from: the shell's if it answers, else the
/// persisted one.
pub fn current() -> ThemeName {
    match ashd_ipc::request(&Request::Theme(ThemeRequest::Get)) {
        Ok(Response::Theme(t)) => t,
        _ => state::current_theme(),
    }
}

/// `ashd theme apply`: rewrite the files of the persisted theme.
pub fn apply(reload: bool) -> anyhow::Result<(ThemeName, Vec<PathBuf>, Option<ReloadReport>)> {
    let a = ashd_theme::reapply_current(reload).context("writing the theme files")?;
    Ok((a.theme, a.written, a.reload))
}

/// Human-readable warnings for a reload that did not fully work.
pub fn reload_warnings(report: &ReloadReport) -> Vec<String> {
    report.warnings()
}

/// `ashd fm [-- args…]`: replace this process with fm, themed (`--theme <current>`) unless
/// fm is unchecked in the theme switcher. Only returns on failure.
pub fn exec_fm(args: &[String]) -> anyhow::Error {
    let (bin, argv) = ashd_theme::fm_command(args);
    let err = Command::new(&bin).args(&argv).exec();
    if err.kind() == std::io::ErrorKind::NotFound {
        anyhow::anyhow!("`{}` not found (set $ASHD_FM_BIN or put fm in PATH)", bin.to_string_lossy())
    } else {
        anyhow::Error::new(err).context(format!("running {}", bin.to_string_lossy()))
    }
}
