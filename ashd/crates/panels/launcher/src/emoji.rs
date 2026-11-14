//! `:` emoji search over the `emojis` crate (CLDR names + gemoji shortcodes).

use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};

pub const MAX_RESULTS: usize = 60;

#[derive(Clone, Debug, PartialEq)]
pub struct EmojiHit {
    pub glyph: &'static str,
    pub name: String,
    pub shortcode: Option<&'static str>,
    pub group: String,
    pub indices: Vec<u32>,
}

fn all() -> impl Iterator<Item = &'static emojis::Emoji> {
    // skin-tone variants are reachable through their base emoji only
    emojis::iter().filter(|e| e.skin_tone().is_none_or(|t| t == emojis::SkinTone::Default))
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// `TravelAndPlaces` → `Travel and places`.
fn humanize(debug_name: &str) -> String {
    let mut out = String::new();
    for (i, c) in debug_name.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push(' ');
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out.replace(" and ", " & ")
}

fn hit(e: &'static emojis::Emoji, indices: Vec<u32>) -> EmojiHit {
    EmojiHit {
        glyph: e.as_str(),
        name: capitalise(e.name()),
        shortcode: e.shortcode(),
        group: humanize(&format!("{:?}", e.group())),
        indices,
    }
}

/// Empty query: the first smileys (`recent` glyphs first). Otherwise fuzzy over name and shortcodes.
pub fn search(query: &str, matcher: &mut Matcher, recent: &[String]) -> Vec<EmojiHit> {
    let q = query.trim();
    if q.is_empty() {
        let mut out: Vec<EmojiHit> = recent.iter().filter_map(|g| emojis::get(g)).map(|e| hit(e, Vec::new())).collect();
        for e in emojis::Group::SmileysAndEmotion.emojis() {
            if out.len() >= MAX_RESULTS {
                break;
            }
            if e.skin_tone().is_none_or(|t| t == emojis::SkinTone::Default) && !out.iter().any(|h| h.glyph == e.as_str()) {
                out.push(hit(e, Vec::new()));
            }
        }
        return out;
    }
    let pattern = Pattern::parse(q, CaseMatching::Ignore, Normalization::Smart);
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, &'static emojis::Emoji)> = Vec::new();
    for e in all() {
        let mut hay = e.name().to_string();
        for s in e.shortcodes() {
            hay.push(' ');
            hay.push_str(s);
        }
        if let Some(s) = pattern.score(Utf32Str::new(&hay, &mut buf), matcher) {
            scored.push((s, e));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.name().len().cmp(&b.1.name().len())));
    // drop loose subsequence matches far behind the best hit
    let best = scored.first().map(|s| s.0).unwrap_or(0);
    scored.retain(|s| s.0 * 2 >= best);
    scored.truncate(MAX_RESULTS);
    let mut idx = Vec::new();
    scored
        .into_iter()
        .map(|(_, e)| {
            idx.clear();
            let name = capitalise(e.name());
            let mut indices = Vec::new();
            if pattern.indices(Utf32Str::new(&name, &mut buf), matcher, &mut idx).is_some() {
                idx.sort_unstable();
                idx.dedup();
                indices = idx.clone();
            }
            hit(e, indices)
        })
        .collect()
}

pub fn matcher() -> Matcher {
    Matcher::new(Config::DEFAULT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_by_name_and_shortcode() {
        let mut m = matcher();
        let hits = search("rocket", &mut m, &[]);
        assert_eq!(hits[0].glyph, "🚀");
        assert_eq!(hits[0].name, "Rocket");
        assert_eq!(hits[0].group, "Travel & places");
        assert!(hits.len() < 12, "loose matches are dropped: {}", hits.len());
        assert_eq!(hits[0].indices, [0, 1, 2, 3, 4, 5]);
        let hits = search("thumbsup", &mut m, &[]);
        assert!(hits.iter().any(|h| h.glyph == "👍"));
        assert!(search("qzxqzxqzx", &mut m, &[]).is_empty());
    }

    #[test]
    fn empty_query_shows_recent_then_smileys() {
        let mut m = matcher();
        let hits = search("", &mut m, &["🚀".to_string()]);
        assert_eq!(hits[0].glyph, "🚀");
        assert_eq!(hits.len(), MAX_RESULTS);
        assert!(hits.iter().skip(1).all(|h| h.glyph != "🚀"));
    }
}
