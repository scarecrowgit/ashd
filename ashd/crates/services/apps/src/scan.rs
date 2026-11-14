//! Desktop entry scanning: XDG data dirs → `Vec<App>`.

use std::{
    collections::{HashMap, HashSet},
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
};

use freedesktop_desktop_entry::DesktopEntry;

use crate::{App, icons::IconResolver};

/// Everything a scan depends on, so tests never touch the process environment.
#[derive(Clone, Debug, Default)]
pub struct ScanEnv {
    /// `<dir>/applications` is scanned for every entry; highest priority first.
    pub data_dirs: Vec<PathBuf>,
    pub locales: Vec<String>,
    /// `XDG_CURRENT_DESKTOP`, lower-cased and split on `:`.
    pub desktops: Vec<String>,
    pub icon_theme: String,
    /// `$PATH` entries, for `TryExec`.
    pub path: Vec<PathBuf>,
}

impl ScanEnv {
    pub fn from_env() -> Self {
        Self {
            data_dirs: xdg_data_dirs(),
            locales: {
                let l = freedesktop_desktop_entry::get_languages_from_env();
                if l.is_empty() { vec!["en".to_string()] } else { l }
            },
            desktops: std::env::var("XDG_CURRENT_DESKTOP")
                .map(|d| d.to_ascii_lowercase().split(':').filter(|s| !s.is_empty()).map(str::to_string).collect())
                .unwrap_or_default(),
            icon_theme: crate::icons::current_icon_theme(),
            path: std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default(),
        }
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// `$XDG_DATA_HOME` then `$XDG_DATA_DIRS` (NixOS puts `/run/current-system/sw/share`,
/// `~/.nix-profile/share` and `/etc/profiles/per-user/$USER/share` there). Only when
/// `XDG_DATA_DIRS` is unset are the usual NixOS locations added as a fallback.
pub fn xdg_data_dirs() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let home = dirs::home_dir();
    if let Some(h) = env_path("XDG_DATA_HOME").or_else(|| home.as_ref().map(|h| h.join(".local/share"))) {
        out.push(h);
    }
    match env_path("XDG_DATA_DIRS") {
        Some(v) => out.extend(std::env::split_paths(&v).filter(|p| p.is_absolute())),
        None => {
            out.push("/usr/local/share".into());
            out.push("/usr/share".into());
            let user = std::env::var("USER").unwrap_or_default();
            let mut nix = vec![];
            if let Some(h) = &home {
                nix.push(h.join(".nix-profile/share"));
            }
            if !user.is_empty() {
                nix.push(PathBuf::from(format!("/etc/profiles/per-user/{user}/share")));
            }
            nix.push("/nix/var/nix/profiles/default/share".into());
            nix.push("/run/current-system/sw/share".into());
            out.extend(nix.into_iter().filter(|p| p.is_dir()));
        }
    }
    let mut seen = HashSet::new();
    out.retain(|p| seen.insert(p.clone()));
    out
}

/// All `*.desktop` files below `dir` (symlinks followed, loops guarded), sorted.
fn desktop_files(dir: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, seen: &mut HashSet<PathBuf>, out: &mut Vec<PathBuf>) {
        if depth > 4 {
            return;
        }
        let Ok(canon) = dir.canonicalize() else { return };
        if !seen.insert(canon) {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort();
        for p in entries {
            let Ok(meta) = p.metadata() else { continue };
            if meta.is_dir() {
                walk(&p, depth + 1, seen, out);
            } else if meta.is_file() && p.extension().is_some_and(|e| e == "desktop") {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, 0, &mut HashSet::new(), &mut out);
    out
}

/// Scan every data dir. Earlier dirs win for equal ids; a `Hidden=true` entry in an
/// earlier dir masks the later one (spec: "treat as if it does not exist").
pub fn scan(env: &ScanEnv, history: &HashMap<String, (u32, u64)>) -> Vec<App> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut icons = IconResolver::new(&env.icon_theme, &env.data_dirs);
    let mut apps = Vec::new();
    for dir in &env.data_dirs {
        for path in desktop_files(&dir.join("applications")) {
            let Ok(entry) = DesktopEntry::from_path(&path, Some(&env.locales)) else { continue };
            if !seen.insert(entry.appid.clone()) {
                continue;
            }
            if let Some(mut app) = build_app(&entry, env, &mut icons) {
                if let Some(&(count, last)) = history.get(&app.id) {
                    app.launch_count = count;
                    app.last_launch = last;
                }
                apps.push(app);
            }
        }
    }
    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.id.cmp(&b.id)));
    apps
}

fn split_list(v: Option<Vec<&str>>) -> Vec<&str> {
    v.unwrap_or_default().into_iter().map(str::trim).filter(|s| !s.is_empty()).collect()
}

fn build_app(entry: &DesktopEntry, env: &ScanEnv, icons: &mut IconResolver) -> Option<App> {
    if entry.hidden() || entry.no_display() {
        return None;
    }
    if entry.type_().is_some_and(|t| t != "Application") {
        return None;
    }
    let in_current = |list: &[&str]| list.iter().any(|d| env.desktops.iter().any(|c| c.eq_ignore_ascii_case(d)));
    let only = split_list(entry.only_show_in());
    if !only.is_empty() && !in_current(&only) {
        return None;
    }
    if in_current(&split_list(entry.not_show_in())) {
        return None;
    }
    if let Some(te) = entry.try_exec().map(str::trim).filter(|t| !t.is_empty())
        && !executable_exists(te, &env.path)
    {
        return None;
    }
    let name = entry.name(&env.locales)?.trim().to_string();
    if name.is_empty() {
        return None;
    }
    let exec = parse_exec(entry.exec()?);
    if exec.is_empty() {
        return None;
    }
    let non_empty = |s: std::borrow::Cow<'_, str>| {
        let s = s.trim();
        (!s.is_empty()).then(|| s.to_string())
    };
    let icon_name = entry.icon().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let icon_path = icon_name.as_deref().and_then(|n| icons.resolve(n));
    Some(App {
        id: entry.appid.clone(),
        name,
        generic_name: entry.generic_name(&env.locales).and_then(non_empty),
        comment: entry.comment(&env.locales).and_then(non_empty),
        exec,
        icon_name,
        icon_path,
        terminal: entry.terminal(),
        keywords: entry
            .keywords(&env.locales)
            .unwrap_or_default()
            .into_iter()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .collect(),
        categories: split_list(entry.categories()).into_iter().map(str::to_string).collect(),
        startup_wm_class: entry.startup_wm_class().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string),
        desktop_file: entry.path.clone(),
        launch_count: 0,
        last_launch: 0,
    })
}

fn executable_exists(name: &str, path: &[PathBuf]) -> bool {
    let is_exec = |p: &Path| p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
    if name.contains('/') {
        return is_exec(Path::new(name));
    }
    path.iter().any(|d| is_exec(&d.join(name)))
}

/// Remove `%f %F %u %U %i %c %k …` from a field, unescape `%%`. Returns whether a field
/// code was removed.
fn strip_field_codes(arg: &str) -> (String, bool) {
    let mut out = String::with_capacity(arg.len());
    let mut had_code = false;
    let mut chars = arg.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            Some('%') => {
                chars.next();
                out.push('%');
            }
            Some('f' | 'F' | 'u' | 'U' | 'd' | 'D' | 'n' | 'N' | 'i' | 'c' | 'k' | 'v' | 'm') => {
                chars.next();
                had_code = true;
            }
            _ => out.push('%'),
        }
    }
    (out, had_code)
}

/// Split an `Exec` value (Desktop Entry quoting rules) into argv and drop the field
/// codes. An argument that was only a field code disappears.
pub fn parse_exec(exec: &str) -> Vec<String> {
    let mut args: Vec<(String, bool)> = Vec::new();
    let (mut arg, mut in_arg, mut quoted) = (String::new(), false, false);
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_arg = true;
                quoted = true;
                loop {
                    match chars.next() {
                        None | Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '`' | '$' | '\\')) => arg.push(c),
                            Some(c) => {
                                arg.push('\\');
                                arg.push(c);
                            }
                            None => break,
                        },
                        Some(c) => arg.push(c),
                    }
                }
            }
            '\\' => {
                in_arg = true;
                arg.push(chars.next().unwrap_or('\\'));
            }
            c if c.is_ascii_whitespace() => {
                if in_arg {
                    args.push((std::mem::take(&mut arg), quoted));
                    in_arg = false;
                    quoted = false;
                }
            }
            c => {
                in_arg = true;
                arg.push(c);
            }
        }
    }
    if in_arg {
        args.push((arg, quoted));
    }
    args.into_iter()
        .filter_map(|(arg, quoted)| {
            if quoted {
                return Some(arg);
            }
            let (clean, had_code) = strip_field_codes(&arg);
            (!(clean.is_empty() && had_code)).then_some(clean)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_strips_field_codes() {
        assert_eq!(parse_exec("firefox %u"), ["firefox"]);
        assert_eq!(parse_exec("gimp-2.10 %F"), ["gimp-2.10"]);
        assert_eq!(parse_exec("app --name=x %i %c %k"), ["app", "--name=x"]);
        assert_eq!(parse_exec("app 100%% %U"), ["app", "100%"]);
        assert_eq!(parse_exec("app --file=%f"), ["app", "--file="]);
    }

    #[test]
    fn exec_honours_quotes() {
        assert_eq!(
            parse_exec(r#"sh -c "echo \"hi\" $HOME %f" x"#),
            ["sh", "-c", r#"echo "hi" $HOME %f"#, "x"]
        );
        assert_eq!(parse_exec(r#""/opt/My App/run" --flag"#), ["/opt/My App/run", "--flag"]);
        assert!(parse_exec("   ").is_empty());
    }
}
