//! `/` file search: `plocate` when it is on `PATH` and has a database, otherwise a bounded
//! walk of `$HOME` (depth, entry count and time limits). Runs on a background thread.

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime},
};

use ignore::WalkBuilder;

/// Hits shown (and stat'ed).
pub const MAX_HITS: usize = 80;
const PLOCATE_LIMIT: &str = "400";
const WALK_DEPTH: usize = 6;
const WALK_ENTRIES: usize = 250_000;
const WALK_TIME: Duration = Duration::from_millis(1800);

/// Directory names the fallback walk never enters (caches, toolchains, build output).
const SKIP_DIRS: [&str; 12] = [
    ".cache", ".cargo", ".rustup", "node_modules", "target", ".git", ".npm", ".mozilla", ".local", ".steam", ".var",
    ".wine",
];
/// Prefixes plocate results are dropped from (store paths and virtual filesystems).
const SKIP_PREFIXES: [&str; 5] = ["/nix/store", "/proc", "/sys", "/dev", "/run"];

#[derive(Clone, Debug, PartialEq)]
pub struct FileHit {
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: Option<SystemTime>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileResults {
    pub query: String,
    /// `"plocate"` or `"scan"`.
    pub source: &'static str,
    pub hits: Vec<FileHit>,
    /// Matches found before ranking and truncation to [`MAX_HITS`].
    pub found: usize,
}

pub fn tokens(query: &str) -> Vec<String> {
    query.split_whitespace().map(str::to_lowercase).collect()
}

pub fn find_plocate() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join("plocate")).find(|p| p.is_file())
}

pub fn search(query: &str, home: &Path, plocate: Option<&Path>) -> FileResults {
    let toks = tokens(query);
    if toks.is_empty() {
        return FileResults { query: query.to_string(), source: "scan", ..Default::default() };
    }
    let (source, paths) = match plocate.and_then(|p| run_plocate(p, &toks)) {
        Some(paths) => ("plocate", paths),
        None => ("scan", walk(home, &toks)),
    };
    let found = paths.len();
    let hits = rank(paths, &toks, home).into_iter().take(MAX_HITS).filter_map(stat).collect();
    FileResults { query: query.to_string(), source, hits, found }
}

/// `None` when plocate cannot answer (no database, spawn failure): fall back to the walk.
fn run_plocate(bin: &Path, toks: &[String]) -> Option<Vec<PathBuf>> {
    let out = Command::new(bin)
        .args(["-i", "-A", "-l", PLOCATE_LIMIT, "--"])
        .args(toks)
        .env_remove("LD_LIBRARY_PATH")
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .ok()?;
    // plocate exits 1 for "no match" without printing anything; errors print to stderr.
    if !out.stderr.is_empty() || !matches!(out.status.code(), Some(0 | 1)) {
        return None;
    }
    let paths = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .filter(|l| !SKIP_PREFIXES.iter().any(|p| l.starts_with(p)))
        .map(PathBuf::from)
        .collect();
    Some(paths)
}

fn walk(home: &Path, toks: &[String]) -> Vec<PathBuf> {
    let start = Instant::now();
    let mut seen = 0usize;
    let mut out = Vec::new();
    let walker = WalkBuilder::new(home)
        .hidden(false)
        .ignore(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .parents(false)
        .follow_links(false)
        .max_depth(Some(WALK_DEPTH))
        .filter_entry(|e| {
            !(e.depth() > 0
                && e.file_type().is_some_and(|t| t.is_dir())
                && e.file_name().to_str().is_some_and(|n| SKIP_DIRS.contains(&n)))
        })
        .build();
    for entry in walker.flatten() {
        seen += 1;
        if seen > WALK_ENTRIES || (seen.is_multiple_of(512) && start.elapsed() > WALK_TIME) || out.len() >= 400 {
            break;
        }
        if entry.depth() == 0 {
            continue;
        }
        let rel = entry.path().strip_prefix(home).unwrap_or(entry.path()).to_string_lossy().to_lowercase();
        if toks.iter().all(|t| rel.contains(t.as_str())) {
            out.push(entry.into_path());
        }
    }
    out
}

/// Basename matches first, then under `$HOME`, then shallower / shorter paths.
fn rank(mut paths: Vec<PathBuf>, toks: &[String], home: &Path) -> Vec<PathBuf> {
    let key = |p: &PathBuf| {
        let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        let in_name = toks.iter().all(|t| name.contains(t.as_str()));
        (!in_name, !p.starts_with(home), p.components().count(), p.as_os_str().len())
    };
    paths.sort_by_cached_key(key);
    paths
}

fn stat(path: PathBuf) -> Option<FileHit> {
    let meta = path.metadata().ok()?;
    Some(FileHit { is_dir: meta.is_dir(), size: meta.len(), mtime: meta.modified().ok(), path })
}

/// `/home/me/.config/niri` → `~/.config/niri`.
pub fn tilde(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(root: &Path, rel: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "x").unwrap();
    }

    #[test]
    fn walk_finds_by_all_tokens_ranks_names_and_skips_noise() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        touch(h, ".config/niri/config.kdl");
        touch(h, ".config/niri/binds.kdl");
        touch(h, ".config/kitty/kitty.conf");
        touch(h, "src/conf/notes.txt");
        touch(h, ".cache/big/config.kdl");
        touch(h, "node_modules/x/config.kdl");
        let r = search("conf kdl", h, None);
        assert_eq!(r.source, "scan");
        let names: Vec<_> = r.hits.iter().map(|x| x.path.file_name().unwrap().to_string_lossy().to_string()).collect();
        assert_eq!(names, ["config.kdl", "binds.kdl"], "{names:?}");
        assert_eq!(tilde(&r.hits[0].path, h), "~/.config/niri/config.kdl");
        assert_eq!(r.found, 2);
        assert!(!r.hits[0].is_dir && r.hits[0].size == 1 && r.hits[0].mtime.is_some());
        // a single token matches path components too; basename hits come first
        let r = search("niri", h, None);
        assert_eq!(r.hits[0].path.file_name().unwrap(), "niri");
        assert!(r.hits[0].is_dir);
        assert_eq!(r.hits.len(), 3);
        assert!(search("   ", h, None).hits.is_empty());
        assert!(search("nonexistentzzz", h, None).hits.is_empty());
    }

    #[test]
    fn walk_respects_depth_limit() {
        let home = tempfile::tempdir().unwrap();
        touch(home.path(), "a/b/c/d/e/f/g/deep.txt");
        touch(home.path(), "a/b/shallow.txt");
        let r = search("txt", home.path(), None);
        assert_eq!(r.hits.len(), 1);
        assert!(r.hits[0].path.ends_with("shallow.txt"));
    }

    #[test]
    fn missing_plocate_binary_falls_back_to_walk() {
        let home = tempfile::tempdir().unwrap();
        touch(home.path(), "findme.txt");
        let r = search("findme", home.path(), Some(Path::new("/definitely/not/plocate")));
        assert_eq!(r.source, "scan");
        assert_eq!(r.hits.len(), 1);
    }
}
