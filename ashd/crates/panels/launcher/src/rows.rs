//! Result rows per mode.

use std::{collections::HashMap, time::SystemTime};

use gpui::App;
use nucleo_matcher::{
    Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};
use ashd_svc_apps::{App as DesktopApp, find_app_for_window, search};
use ashd_ui::Icon;

use crate::{
    LauncherPanel, emoji, files,
    history,
    model::{Act, Row, RowIcon, human_age, human_size, looks_like_command, needs_shell, substring_indices, web_targets},
};

/// App and window rows in a mixed result list.
const MIXED_LIMIT: usize = 6;
const EMPTY_APPS: usize = 6;
const EMPTY_WINDOWS: usize = 4;

fn ws_label(idx: u8) -> String {
    format!("ws {idx:02}")
}

fn app_row(app: &DesktopApp, indices: Vec<u32>, ws: Option<u8>) -> Row {
    let base = app.generic_name.clone().or_else(|| app.comment.clone()).unwrap_or_else(|| "Application".to_string());
    let subtitle = match ws {
        Some(ws) => format!("{base} · {}", ws_label(ws)),
        None => base,
    };
    Row::new(RowIcon::App(app.icon_path.clone()), app.name.clone(), subtitle, Act::LaunchApp(Box::new(app.clone())))
        .matched(indices)
}

struct Win<'a> {
    id: u64,
    title: String,
    app: Option<&'a DesktopApp>,
    app_id: String,
    ws: Option<u8>,
    focus: (u64, u32),
}

impl LauncherPanel {
    pub(crate) fn app_rows(&mut self, q: &str, cx: &App) -> Vec<Row> {
        let apps = self.ctx.services.apps.state.read(cx).apps.clone();
        let niri = self.ctx.niri.read(cx);
        let mut wins: Vec<Win> = niri
            .windows
            .values()
            .map(|w| {
                let app_id = w.app_id.clone().unwrap_or_default();
                Win {
                    id: w.id,
                    title: w.title.clone().filter(|t| !t.is_empty()).unwrap_or_else(|| app_id.clone()),
                    app: find_app_for_window(&apps, &app_id),
                    app_id,
                    // the number the user sees (hidden scratch workspaces skipped), not niri's idx
                    ws: w.workspace_id.and_then(|id| niri.desk_number(id)).map(|n| n.min(u8::MAX as usize) as u8),
                    focus: w.focus_timestamp.map(|t| (t.secs, t.nanos)).unwrap_or_default(),
                }
            })
            .collect();
        wins.sort_by(|a, b| b.focus.cmp(&a.focus).then(a.id.cmp(&b.id)));
        // workspace of the most recently used window of each app (the subtitle "· ws 01")
        let mut running: HashMap<&str, u8> = HashMap::new();
        for w in &wins {
            if let (Some(app), Some(ws)) = (w.app, w.ws) {
                running.entry(app.id.as_str()).or_insert(ws);
            }
        }
        let window_row = |w: &Win| {
            let sub_app = w.app.map(|a| a.name.clone()).unwrap_or_else(|| w.app_id.clone());
            let subtitle = match w.ws {
                Some(ws) => format!("{sub_app} · {}", ws_label(ws)),
                None => sub_app,
            };
            Row::new(RowIcon::App(w.app.and_then(|a| a.icon_path.clone())), w.title.clone(), subtitle, Act::FocusWindow(w.id))
                .hint("window")
        };

        if q.trim().is_empty() {
            let hits = search(&apps, "", EMPTY_APPS);
            // launched apps first under "recent", the rest under "apps"
            let (mut seen_recent, mut seen_apps) = (false, false);
            let mut rows: Vec<Row> = Vec::new();
            for h in &hits {
                let a = &apps[h.index];
                let r = app_row(a, Vec::new(), running.get(a.id.as_str()).copied());
                let recent = a.launch_count > 0;
                rows.push(match (recent, seen_recent, seen_apps) {
                    (true, false, _) => {
                        seen_recent = true;
                        r.section("recent")
                    }
                    (false, _, false) => {
                        seen_apps = true;
                        r.section("apps")
                    }
                    _ => r,
                });
            }
            for (i, w) in wins.iter().take(EMPTY_WINDOWS).enumerate() {
                let r = window_row(w);
                rows.push(if i == 0 { r.section("windows") } else { r });
            }
            return rows;
        }

        let pattern = Pattern::parse(q.trim(), CaseMatching::Smart, Normalization::Smart);
        let mut scored: Vec<(u32, Row)> = Vec::new();
        let mut app_hits = 0;
        for h in search(&apps, q, MIXED_LIMIT) {
            app_hits += 1;
            let a = &apps[h.index];
            scored.push((h.score, app_row(a, h.name_indices, running.get(a.id.as_str()).copied())));
        }
        let mut buf = Vec::new();
        let mut idx = Vec::new();
        for w in &wins {
            let hay = format!("{} {}", w.title, w.app_id);
            if let Some(score) = pattern.score(Utf32Str::new(&hay, &mut buf), &mut self.matcher).map(|s| s * 7 / 10) {
                idx.clear();
                let mut matched = Vec::new();
                if pattern.indices(Utf32Str::new(&w.title, &mut buf), &mut self.matcher, &mut idx).is_some() {
                    idx.sort_unstable();
                    idx.dedup();
                    matched = idx.clone();
                }
                scored.push((score, window_row(w).matched(matched)));
            }
        }
        scored.sort_by_key(|b| std::cmp::Reverse(b.0));
        let mut rows: Vec<Row> = scored.into_iter().take(MIXED_LIMIT).map(|(_, r)| r).collect();

        // one matching command from the `>` history, then the jump into file search
        let mut cmd_hit: Option<Row> = None;
        let now = history::now();
        for (cmd, at) in &self.run_history {
            idx.clear();
            if pattern.score(Utf32Str::new(cmd, &mut buf), &mut self.matcher).is_some() {
                if pattern.indices(Utf32Str::new(cmd, &mut buf), &mut self.matcher, &mut idx).is_some() {
                    idx.sort_unstable();
                    idx.dedup();
                }
                cmd_hit = Some(
                    Row::command(
                        RowIcon::Glyph(Icon::Terminal),
                        cmd.clone(),
                        format!("command · last run {}", human_age(now.saturating_sub(*at), true)),
                        cmd,
                    )
                    .matched(idx.clone())
                    .hint("run"),
                );
                break;
            }
        }
        // A query that is no application (or reads as a command line) can be run: output
        // appears right here. Nothing else in the list is for it, so it comes before the
        // jump into the file search.
        if app_hits == 0 || looks_like_command(q) {
            let line = q.trim();
            if cmd_hit.as_ref().is_some_and(|r| r.title == line) {
                cmd_hit = None;
            }
            rows.extend(cmd_hit.take());
            rows.push(
                Row::command(
                    RowIcon::Glyph(Icon::Terminal),
                    format!("Run “{}”", line.chars().take(48).collect::<String>() + if line.chars().count() > 48 { "…" } else { "" }),
                    format!("command{} · output shown here", if needs_shell(line) { " · through your shell" } else { "" }),
                    line,
                )
                .hint("run"),
            );
        } else {
            rows.extend(cmd_hit);
        }
        let short: String = q.trim().chars().take(24).collect();
        rows.push(
            Row::new(
                RowIcon::Glyph(Icon::Folder),
                format!("Files — {short}…"),
                format!("search your files for “{}”", q.trim()),
                Act::Mode(ashd_ipc::LauncherMode::Files, q.trim().to_string()),
            )
            .hint("/"),
        );
        rows
    }

    pub(crate) fn file_rows(&self, _cx: &App) -> Vec<Row> {
        let Some(res) = &self.files.results else { return Vec::new() };
        let home = dirs::home_dir().unwrap_or_default();
        let toks = files::tokens(&res.query);
        let now = SystemTime::now();
        res.hits
            .iter()
            .map(|h| {
                let name = h.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let dir = h.path.parent().map(|p| files::tilde(p, &home)).unwrap_or_default();
                let age = h
                    .mtime
                    .and_then(|m| now.duration_since(m).ok())
                    .map(|d| human_age(d.as_secs(), false))
                    .unwrap_or_default();
                let subtitle = if h.is_dir {
                    format!("{dir} · folder · {age}")
                } else {
                    format!("{dir} · {} · {age}", human_size(h.size))
                };
                let folder = h.path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| h.path.clone());
                let open = |p: &std::path::Path| Act::Spawn(vec!["xdg-open".into(), p.display().to_string()]);
                Row::new(
                    RowIcon::Glyph(if h.is_dir { Icon::Folder } else { Icon::File }),
                    name.clone(),
                    subtitle,
                    open(&h.path),
                )
                .matched(substring_indices(&name, &toks))
                .alt(open(&folder))
            })
            .collect()
    }

    pub(crate) fn run_rows(&mut self, q: &str) -> Vec<Row> {
        let q = q.trim();
        let now = history::now();
        let hist_row = |cmd: &str, at: u64, matched: Vec<u32>| {
            Row::command(
                RowIcon::Glyph(Icon::Terminal),
                cmd,
                format!("command · last run {}", human_age(now.saturating_sub(at), true)),
                cmd,
            )
            .matched(matched)
        };
        if q.is_empty() {
            return self.run_history.iter().take(crate::MAX_VISIBLE + 2).map(|(c, t)| hist_row(c, *t, Vec::new())).collect();
        }
        let shell_note = if crate::model::needs_shell(q) { " · through your shell" } else { "" };
        let mut rows = vec![
            Row::command(RowIcon::Glyph(Icon::Terminal), q, format!("run command{shell_note} · output shown here"), q),
        ];
        let pattern = Pattern::parse(q, CaseMatching::Smart, Normalization::Smart);
        let (mut buf, mut idx) = (Vec::new(), Vec::new());
        let mut hits: Vec<(u32, usize, Vec<u32>)> = Vec::new();
        for (i, (cmd, _)) in self.run_history.iter().enumerate() {
            if cmd == q {
                continue;
            }
            if let Some(s) = pattern.score(Utf32Str::new(cmd, &mut buf), &mut self.matcher) {
                idx.clear();
                pattern.indices(Utf32Str::new(cmd, &mut buf), &mut self.matcher, &mut idx);
                idx.sort_unstable();
                idx.dedup();
                hits.push((s, i, idx.clone()));
            }
        }
        hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        for (_, i, m) in hits.into_iter().take(crate::MAX_VISIBLE) {
            let (c, t) = &self.run_history[i];
            rows.push(hist_row(c, *t, m));
        }
        rows
    }

    pub(crate) fn emoji_rows(&mut self, q: &str) -> Vec<Row> {
        emoji::search(q, &mut self.matcher, &self.recent_emoji)
            .into_iter()
            .map(|h| {
                let sub = match h.shortcode {
                    Some(s) => format!(":{s}: · {}", h.group),
                    None => h.group.clone(),
                };
                Row::new(RowIcon::Text(h.glyph.to_string()), h.name, sub, Act::Copy(h.glyph.to_string())).matched(h.indices)
            })
            .collect()
    }

    pub(crate) fn web_rows(&mut self, q: &str) -> Vec<Row> {
        let q = q.trim();
        if q.is_empty() {
            return Vec::new();
        }
        let custom = std::env::var("ASHD_SEARCH_URL").ok();
        web_targets(q, custom.as_deref())
            .into_iter()
            .map(|(name, url, query)| {
                Row::new(
                    RowIcon::Glyph(Icon::Globe),
                    format!("Search {name}"),
                    format!("“{query}”"),
                    Act::Spawn(vec!["xdg-open".into(), url]),
                )
            })
            .collect()
    }
}

