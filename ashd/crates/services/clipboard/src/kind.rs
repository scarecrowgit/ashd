//! What a clipboard entry *is*: kind detection (pure, no I/O except the injected `exists`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Detected kind of an entry (drives the row icon and the preview pane).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Kind {
    #[default]
    Text,
    /// One `http://` / `https://` URL.
    Link,
    /// An absolute or `~/` path that looks real.
    Path,
    /// `#RGB` / `#RGBA` / `#RRGGBB` / `#RRGGBBAA`.
    Colour,
    Image,
    /// Multi-line text that looks like source code.
    Code,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::Link => "link",
            Kind::Path => "path",
            Kind::Colour => "colour",
            Kind::Image => "image",
            Kind::Code => "code",
        }
    }
}

/// An RGBA colour (the swatch value of a [`Kind::Colour`] entry).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Colour {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Colour {
    /// `#RRGGBB`, or `#RRGGBBAA` when not opaque (upper case).
    pub fn hex(&self) -> String {
        if self.a == 255 {
            format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
        } else {
            format!("#{:02X}{:02X}{:02X}{:02X}", self.r, self.g, self.b, self.a)
        }
    }

    /// `rgb(224, 164, 88)` or `rgba(224, 164, 88, 0.50)`.
    pub fn rgb(&self) -> String {
        if self.a == 255 {
            format!("rgb({}, {}, {})", self.r, self.g, self.b)
        } else {
            format!("rgba({}, {}, {}, {:.2})", self.r, self.g, self.b, f32::from(self.a) / 255.0)
        }
    }

    /// `hsl(34, 69%, 61%)` (alpha ignored).
    pub fn hsl(&self) -> String {
        let (r, g, b) = (f32::from(self.r) / 255.0, f32::from(self.g) / 255.0, f32::from(self.b) / 255.0);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let l = (max + min) / 2.0;
        let d = max - min;
        let (h, s) = if d <= f32::EPSILON {
            (0.0, 0.0)
        } else {
            let s = d / (1.0 - (2.0 * l - 1.0).abs());
            let h = if max == r {
                ((g - b) / d).rem_euclid(6.0)
            } else if max == g {
                (b - r) / d + 2.0
            } else {
                (r - g) / d + 4.0
            };
            (h * 60.0, s)
        };
        format!("hsl({:.0}, {:.0}%, {:.0}%)", h, s * 100.0, l * 100.0)
    }
}

/// Parse a whole string (surrounding whitespace allowed) as a hex colour.
pub fn parse_hex_colour(text: &str) -> Option<Colour> {
    let hex = text.trim().strip_prefix('#')?;
    if !hex.is_ascii() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let nib = |i: usize| u8::from_str_radix(&hex[i..=i], 16).ok().map(|n| n * 17);
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    match hex.len() {
        3 => Some(Colour { r: nib(0)?, g: nib(1)?, b: nib(2)?, a: 255 }),
        4 => Some(Colour { r: nib(0)?, g: nib(1)?, b: nib(2)?, a: nib(3)? }),
        6 => Some(Colour { r: byte(0)?, g: byte(2)?, b: byte(4)?, a: 255 }),
        8 => Some(Colour { r: byte(0)?, g: byte(2)?, b: byte(4)?, a: byte(6)? }),
        _ => None,
    }
}

/// Detection result for a text entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Detected {
    pub kind: Kind,
    pub colour: Option<Colour>,
}

/// Detect the kind of a text payload. `exists` answers "is there such a file" (injected so
/// the function stays pure in tests).
pub fn detect(text: &str, exists: impl Fn(&Path) -> bool) -> Detected {
    let plain = |kind| Detected { kind, colour: None };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return plain(Kind::Text);
    }
    let single_line = !trimmed.contains('\n');
    if single_line {
        if let Some(c) = parse_hex_colour(trimmed) {
            return Detected { kind: Kind::Colour, colour: Some(c) };
        }
        if is_link(trimmed) {
            return plain(Kind::Link);
        }
        if is_path(trimmed, &exists) {
            return plain(Kind::Path);
        }
        return plain(Kind::Text);
    }
    if looks_like_code(trimmed) { plain(Kind::Code) } else { plain(Kind::Text) }
}

fn is_link(s: &str) -> bool {
    let rest = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://"));
    rest.is_some_and(|r| !r.is_empty() && !r.starts_with('/') && !s.chars().any(char::is_whitespace))
}

/// Expand a leading `~/`.
pub fn expand_home(s: &str) -> PathBuf {
    match s.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().map(|h| h.join(rest)).unwrap_or_else(|| PathBuf::from(s)),
        None => PathBuf::from(s),
    }
}

fn is_path(s: &str, exists: &impl Fn(&Path) -> bool) -> bool {
    let absolute = s.starts_with('/') && !s.starts_with("//") && s.len() > 1;
    let home = s.starts_with("~/") && s.len() > 2;
    if !(absolute || home) || s.len() > 4096 || s.chars().any(char::is_control) {
        return false;
    }
    if exists(&expand_home(s)) {
        return true;
    }
    // Not on disk (another machine, deleted): accept a plain single-token path with at
    // least one real component — but never prose or a command line.
    !s.chars().any(char::is_whitespace) && s.trim_start_matches(['/', '~']).chars().next().is_some_and(|c| c.is_alphanumeric() || c == '.' || c == '_')
}

/// Multi-line text where at least half the non-empty lines look like code.
fn looks_like_code(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() < 2 {
        return false;
    }
    const STARTS: [&str; 16] = [
        "//", "#include", "#!/", "fn ", "pub ", "def ", "class ", "import ", "from ", "let ", "const ", "use ", "if ",
        "for ", "return ", "<",
    ];
    const CONTAINS: [&str; 9] = ["=>", "->", "::", "();", " = ", "&&", "||", "</", "{}"];
    let sig = lines
        .iter()
        .filter(|l| {
            let t = l.trim();
            let indented = l.starts_with("    ") || l.starts_with('\t');
            t.ends_with(['{', '}', ';', ')', ']'])
                || STARTS.iter().any(|p| t.starts_with(p))
                || CONTAINS.iter().any(|p| t.contains(p))
                || (indented && t.chars().any(|c| "(){}[];=<>".contains(c)))
        })
        .count();
    sig * 2 >= lines.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_files(_: &Path) -> bool {
        false
    }

    fn kind(s: &str) -> Kind {
        detect(s, no_files).kind
    }

    #[test]
    fn colours() {
        assert_eq!(parse_hex_colour("#E0A458"), Some(Colour { r: 0xE0, g: 0xA4, b: 0x58, a: 255 }));
        assert_eq!(parse_hex_colour(" #fa0 "), Some(Colour { r: 0xFF, g: 0xAA, b: 0x00, a: 255 }));
        assert_eq!(parse_hex_colour("#fa08"), Some(Colour { r: 0xFF, g: 0xAA, b: 0x00, a: 0x88 }));
        assert_eq!(parse_hex_colour("#11223344"), Some(Colour { r: 0x11, g: 0x22, b: 0x33, a: 0x44 }));
        assert_eq!(parse_hex_colour("#12345"), None);
        assert_eq!(parse_hex_colour("#GGGGGG"), None);
        assert_eq!(parse_hex_colour("E0A458"), None);
        assert_eq!(parse_hex_colour("#é1é"), None);
        let d = detect("#E0A458", no_files);
        assert_eq!(d.kind, Kind::Colour);
        assert_eq!(d.colour.map(|c| c.hex()).as_deref(), Some("#E0A458"));
    }

    #[test]
    fn colour_formats() {
        let c = Colour { r: 224, g: 164, b: 88, a: 255 };
        assert_eq!(c.rgb(), "rgb(224, 164, 88)");
        assert_eq!(c.hsl(), "hsl(34, 69%, 61%)");
        let c = Colour { r: 0, g: 0, b: 0, a: 128 };
        assert_eq!(c.hex(), "#00000080");
        assert_eq!(c.rgb(), "rgba(0, 0, 0, 0.50)");
    }

    #[test]
    fn links() {
        assert_eq!(kind("https://github.com/YaLTeR/niri/wiki"), Kind::Link);
        assert_eq!(kind("  http://localhost:8080/x?y=1 \n"), Kind::Link);
        assert_eq!(kind("https://"), Kind::Text);
        assert_eq!(kind("see https://example.com now"), Kind::Text);
        assert_eq!(kind("ftp://example.com"), Kind::Text);
    }

    #[test]
    fn paths() {
        assert_eq!(kind("/etc/hosts"), Kind::Path);
        assert_eq!(kind("~/dots/niri/config.kdl"), Kind::Path);
        assert_eq!(detect("/home/me/My Documents/a b.txt", |_| true).kind, Kind::Path);
        // spaces and not on disk: prose / command, not a path
        assert_eq!(kind("/usr/bin/foo --flag value"), Kind::Text);
        assert_eq!(kind("/"), Kind::Text);
        assert_eq!(kind("//double"), Kind::Text);
        assert_eq!(kind("relative/path.txt"), Kind::Text);
    }

    #[test]
    fn code_vs_text() {
        assert_eq!(kind("fn main() {\n    println!(\"hi\");\n}"), Kind::Code);
        assert_eq!(kind("let x = 1;\nlet y = 2;"), Kind::Code);
        assert_eq!(kind("def f(x):\n    return x + 1"), Kind::Code);
        assert_eq!(kind("niri msg action focus-column-right"), Kind::Text);
        assert_eq!(kind("Dear Alex,\nthanks for the update.\nSee you soon"), Kind::Text);
        assert_eq!(kind(""), Kind::Text);
    }
}
