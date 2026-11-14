//! Notification body markup (spec 1.2 "Markup"): `<b> <i> <u> <a href> <img>` plus the
//! XML entities. The panel renders plain text, so tags are removed (links keep their
//! text, images become their `alt`), entities are decoded and control characters dropped.
//! Text that merely contains a `<` (a lot of apps send unescaped bodies) is kept as is.

const KNOWN_TAGS: &[&str] = &[
    "b", "i", "u", "a", "img", "br", "p", "span", "em", "strong", "big", "small", "s", "tt", "code", "sub", "sup", "font",
    "center", "div",
];

/// Longest tag we try to recognise; anything longer is literal text.
const MAX_TAG_LEN: usize = 512;

pub fn strip_markup(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        let after = &rest[lt + 1..];
        match parse_tag(after) {
            Some((consumed, replacement)) => {
                out.push_str(&replacement);
                rest = &after[consumed..];
            }
            None => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    sanitize(&decode_entities(&out))
}

/// Plain-text field (summary, app name): no markup handling, only control characters.
pub fn plain(input: &str) -> String {
    sanitize(input)
}

/// `after` starts right behind a `<`. Returns (bytes consumed incl. `>`, replacement text).
fn parse_tag(after: &str) -> Option<(usize, String)> {
    let end = after.char_indices().take(MAX_TAG_LEN).find(|&(_, c)| c == '>' || c == '<')?;
    if end.1 != '>' {
        return None;
    }
    let inner = &after[..end.0];
    let body = inner.trim_start_matches('/');
    let name: String = body
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    if name.is_empty() || !KNOWN_TAGS.contains(&name.as_str()) {
        return None;
    }
    let closing = inner.starts_with('/');
    let replacement = match name.as_str() {
        "br" => "\n".to_string(),
        "p" | "div" if closing => "\n".to_string(),
        "img" if !closing => attr(inner, "alt").unwrap_or_default(),
        _ => String::new(),
    };
    Some((end.0 + 1, replacement))
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let key = format!("{name}=");
    let at = lower.find(&key)? + key.len();
    let value = &tag[at..];
    let quote = value.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let value = &value[1..];
    let close = value.find(quote)?;
    Some(value[..close].to_string())
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let decoded = after.find(';').filter(|&semi| semi <= 10).and_then(|semi| {
            let ch = match &after[..semi] {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                "apos" => '\'',
                num if num.starts_with("#x") || num.starts_with("#X") => char::from_u32(u32::from_str_radix(&num[2..], 16).ok()?)?,
                num if num.starts_with('#') => char::from_u32(num[1..].parse().ok()?)?,
                _ => return None,
            };
            Some((ch, semi + 1))
        });
        match decoded {
            Some((ch, used)) => {
                out.push(ch);
                rest = &after[used..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Drop control characters (keep `\n`, turn `\t` and `\r\n` into plain whitespace).
fn sanitize(s: &str) -> String {
    let s = s.replace("\r\n", "\n");
    s.chars()
        .filter_map(|c| match c {
            '\n' => Some('\n'),
            '\t' | '\r' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect::<String>()
        .trim_end()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_spec_tags() {
        assert_eq!(strip_markup("hello <b>bold</b> and <i>it</i> <u>u</u>"), "hello bold and it u");
        assert_eq!(strip_markup("see <a href=\"https://x.y\">this link</a>!"), "see this link!");
        assert_eq!(strip_markup("pic <img src=\"a.png\" alt=\"a cat\"/> here"), "pic a cat here");
        assert_eq!(strip_markup("a<br/>b<br>c"), "a\nb\nc");
    }

    #[test]
    fn decodes_entities_once() {
        assert_eq!(strip_markup("1 &lt; 2 &amp;&amp; 3 &gt; 2 &quot;q&quot; &apos;s&apos;"), "1 < 2 && 3 > 2 \"q\" 's'");
        // escaped tags are text, not tags
        assert_eq!(strip_markup("&lt;b&gt;not bold&lt;/b&gt;"), "<b>not bold</b>");
        assert_eq!(strip_markup("&#65;&#x42;&#x1F600;"), "AB😀");
        assert_eq!(strip_markup("fish & chips &unknown; &"), "fish & chips &unknown; &");
    }

    #[test]
    fn keeps_unescaped_angle_brackets() {
        assert_eq!(strip_markup("a < b and c > d"), "a < b and c > d");
        assert_eq!(strip_markup("vec<String> and <foo>"), "vec<String> and <foo>");
        assert_eq!(strip_markup("trailing <"), "trailing <");
        assert_eq!(strip_markup("<script>alert(1)</script>"), "<script>alert(1)</script>");
    }

    #[test]
    fn drops_control_characters() {
        assert_eq!(strip_markup("a\u{0}b\u{7}c\td\r\ne  \n"), "abc d\ne");
        assert_eq!(plain("title\u{1b}[31m"), "title[31m");
    }

    #[test]
    fn unterminated_and_nested_tags() {
        assert_eq!(strip_markup("<b>bold <i>both</i></b> <b"), "bold both <b");
        assert_eq!(strip_markup("<B>UP</B>"), "UP");
    }
}
