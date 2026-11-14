//! Fuzzy search over apps (nucleo).

use std::{cell::RefCell, time::SystemTime};

use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};

use crate::{App, Match};

thread_local! {
    static MATCHER: RefCell<Matcher> = RefCell::new(Matcher::new(Config::DEFAULT));
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Extra score for apps the user launches: log of the count, plus a little for recent use.
fn frecency(app: &App, now: u64) -> u32 {
    if app.launch_count == 0 {
        return 0;
    }
    let count = ((app.launch_count.min(200) as f32) + 1.0).ln() * 14.0;
    let age_days = now.saturating_sub(app.last_launch) / 86_400;
    let recent = match age_days {
        0 => 20.0,
        1..=6 => 10.0,
        7..=30 => 4.0,
        _ => 0.0,
    };
    (count + recent) as u32
}

/// Fuzzy-search `apps` (name first; generic name, keywords, comment, id and the executable
/// count for less), best first, at most `limit` hits. Empty query: most-launched first,
/// then most recent, then by name (the order of `apps`). `name_indices` are char indices
/// into `App::name` and are only filled when the name itself matched.
pub fn search(apps: &[App], query: &str, limit: usize) -> Vec<Match> {
    let q = query.trim();
    let now = now_secs();
    if q.is_empty() {
        let mut hits: Vec<Match> = apps
            .iter()
            .enumerate()
            .map(|(index, a)| Match { index, score: a.launch_count, name_indices: Vec::new() })
            .collect();
        hits.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then(apps[b.index].last_launch.cmp(&apps[a.index].last_launch))
                .then(a.index.cmp(&b.index))
        });
        hits.truncate(limit);
        return hits;
    }
    let pattern = Pattern::parse(q, CaseMatching::Smart, Normalization::Smart);
    MATCHER.with_borrow_mut(|m| {
        let mut buf = Vec::new();
        let mut hits: Vec<Match> = Vec::new();
        for (index, app) in apps.iter().enumerate() {
            let mut score = |hay: &str, weight: u32, m: &mut Matcher| {
                pattern.score(Utf32Str::new(hay, &mut buf), m).map(|s| s * weight / 100)
            };
            let mut best = score(&app.name, 100, m);
            let mut consider = |s: Option<u32>| {
                if let Some(s) = s
                    && best.is_none_or(|b| s > b)
                {
                    best = Some(s);
                }
            };
            if let Some(g) = &app.generic_name {
                consider(score(g, 60, m));
            }
            if !app.keywords.is_empty() {
                consider(score(&app.keywords.join(" "), 50, m));
            }
            if let Some(c) = &app.comment {
                consider(score(c, 30, m));
            }
            consider(score(&app.id, 40, m));
            if let Some(bin) = app.exec.first().and_then(|e| e.rsplit('/').next()) {
                consider(score(bin, 40, m));
            }
            if let Some(s) = best {
                hits.push(Match { index, score: s + frecency(app, now), name_indices: Vec::new() });
            }
        }
        hits.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then(apps[a.index].name.len().cmp(&apps[b.index].name.len()))
                .then(a.index.cmp(&b.index))
        });
        hits.truncate(limit);
        let mut idx: Vec<u32> = Vec::new();
        for h in &mut hits {
            idx.clear();
            let name = &apps[h.index].name;
            if pattern.indices(Utf32Str::new(name, &mut buf), m, &mut idx).is_some() {
                idx.sort_unstable();
                idx.dedup();
                h.name_indices = idx.clone();
            }
        }
        hits
    })
}
