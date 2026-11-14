//! Keyboard → [`GreeterInput`], plus the layout badge.
//!
//! xkbcommon runs inside smithay-client-toolkit (keymap from the compositor, compose
//! table from the locale): a key event arrives with its keysym and the composed UTF-8.
//! The layout names come from the keymap text sctk hands to `update_keymap`.

use ashd_lockui::GreeterInput;
use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym, Modifiers};

/// Translate a pressed (or repeated) key.
pub fn translate(event: &KeyEvent, mods: &Modifiers) -> Option<GreeterInput> {
    let sym = event.keysym;
    if mods.ctrl {
        return match sym {
            Keysym::u | Keysym::U | Keysym::w | Keysym::W | Keysym::BackSpace => Some(GreeterInput::ClearLine),
            Keysym::a | Keysym::A => Some(GreeterInput::SelectAll),
            Keysym::Return | Keysym::KP_Enter => Some(GreeterInput::Submit),
            _ => None,
        };
    }
    match sym {
        Keysym::Return | Keysym::KP_Enter | Keysym::ISO_Enter => return Some(GreeterInput::Submit),
        Keysym::BackSpace => return Some(GreeterInput::Backspace),
        Keysym::Escape => return Some(GreeterInput::Escape),
        Keysym::ISO_Left_Tab => return Some(GreeterInput::BackTab),
        Keysym::Tab => return Some(if mods.shift { GreeterInput::BackTab } else { GreeterInput::Tab }),
        Keysym::Down => return Some(GreeterInput::NextSession),
        Keysym::Up => return Some(GreeterInput::PrevSession),
        _ => {}
    }
    if mods.alt || mods.logo {
        return None;
    }
    let text = event.utf8.as_deref()?;
    let mut chars = text.chars().filter(|c| !c.is_control());
    let c = chars.next()?;
    // Composed sequences are a single char; anything longer is unexpected: take the first.
    Some(GreeterInput::Char(c))
}

/// Layouts (xkb groups) of a keymap, in group order, as short badge names ("EN", "RU").
pub fn layout_names(keymap: &str) -> Vec<String> {
    // Long names, one per group: `name[Group1]="English (US)";` (libxkbcommon ≥ 1.8
    // writes `name[1]=…`).
    let mut long = Vec::new();
    if let Some(i) = keymap.find("xkb_symbols") {
        for line in keymap[i..].lines() {
            let l = line.trim();
            if l.starts_with("name[")
                && let Some(q0) = l.find('"')
                && let Some(q1) = l[q0 + 1..].find('"')
            {
                long.push(l[q0 + 1..q0 + 1 + q1].to_string());
            }
        }
    }
    // Layout codes from the symbols name: `"pc+us+ru:2+inet(evdev)"`, or with newer
    // libxkbcommon `"pc_us_ru_2_inet(evdev)"`.
    let mut codes = Vec::new();
    if let Some(i) = keymap.find("xkb_symbols") {
        let rest = &keymap[i..];
        if let Some(q0) = rest.find('"')
            && let Some(q1) = rest[q0 + 1..].find('"')
        {
            let include = &rest[q0 + 1..q0 + 1 + q1];
            let mut depth = 0;
            let mut token = String::new();
            let mut tokens = Vec::new();
            for ch in include.chars() {
                match ch {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    '+' | '_' | ':' if depth == 0 => tokens.push(std::mem::take(&mut token)),
                    _ if depth == 0 => token.push(ch),
                    _ => {}
                }
            }
            tokens.push(token);
            codes = tokens.into_iter().filter(|t| is_layout_code(t)).collect();
        }
    }
    if !codes.is_empty() && (long.is_empty() || codes.len() == long.len()) {
        return codes.iter().map(|c| short_name(c)).collect();
    }
    long.iter().map(|l| long_to_short(l)).collect()
}

const NOT_LAYOUTS: &[&str] = &[
    "pc", "inet", "group", "level3", "level5", "compose", "ctrl", "altwin", "capslock", "terminate", "keypad",
    "srvr_ctrl", "shift", "lv3", "lv5", "kpdl", "nbsp", "japan", "eurosign", "rupeesign", "numpad", "caps", "evdev",
];

fn is_layout_code(s: &str) -> bool {
    (2..=3).contains(&s.len()) && s.chars().all(|c| c.is_ascii_lowercase()) && !NOT_LAYOUTS.contains(&s)
}

fn short_name(code: &str) -> String {
    match code {
        "us" | "gb" | "uk" => "EN".into(),
        "ua" => "UK".into(),
        "se" => "SV".into(),
        "dk" => "DA".into(),
        "cz" => "CS".into(),
        "gr" => "EL".into(),
        "jp" => "JA".into(),
        "kr" => "KO".into(),
        "il" => "HE".into(),
        "by" => "BE".into(),
        "kz" => "KK".into(),
        other => other.to_uppercase(),
    }
}

fn long_to_short(long: &str) -> String {
    let word = long.split(|c: char| !c.is_alphabetic()).find(|w| !w.is_empty()).unwrap_or("??");
    match word {
        "English" => "EN".into(),
        "German" => "DE".into(),
        "Spanish" => "ES".into(),
        "Ukrainian" => "UK".into(),
        other => other.chars().take(2).collect::<String>().to_uppercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(sym: Keysym, utf8: Option<&str>) -> KeyEvent {
        KeyEvent { time: 0, raw_code: 0, keysym: sym, utf8: utf8.map(String::from) }
    }

    #[test]
    fn translates_editing_keys() {
        let m = Modifiers::default();
        assert_eq!(translate(&key(Keysym::Return, Some("\r")), &m), Some(GreeterInput::Submit));
        assert_eq!(translate(&key(Keysym::BackSpace, Some("\u{8}")), &m), Some(GreeterInput::Backspace));
        assert_eq!(translate(&key(Keysym::Escape, Some("\u{1b}")), &m), Some(GreeterInput::Escape));
        assert_eq!(translate(&key(Keysym::a, Some("a")), &m), Some(GreeterInput::Char('a')));
        assert_eq!(translate(&key(Keysym::Cyrillic_a, Some("а")), &m), Some(GreeterInput::Char('а')));
        assert_eq!(translate(&key(Keysym::dead_acute, None), &m), None, "pending compose");
        assert_eq!(translate(&key(Keysym::Shift_L, Some("")), &m), None);
        let ctrl = Modifiers { ctrl: true, ..Default::default() };
        assert_eq!(translate(&key(Keysym::u, Some("\u{15}")), &ctrl), Some(GreeterInput::ClearLine));
        assert_eq!(translate(&key(Keysym::a, Some("\u{1}")), &ctrl), Some(GreeterInput::SelectAll));
        assert_eq!(translate(&key(Keysym::c, Some("\u{3}")), &ctrl), None);
    }

    #[test]
    fn tab_and_arrows() {
        let m = Modifiers::default();
        let shift = Modifiers { shift: true, ..Default::default() };
        assert_eq!(translate(&key(Keysym::Tab, Some("\t")), &m), Some(GreeterInput::Tab));
        assert_eq!(translate(&key(Keysym::Tab, Some("\t")), &shift), Some(GreeterInput::BackTab));
        assert_eq!(translate(&key(Keysym::ISO_Left_Tab, None), &shift), Some(GreeterInput::BackTab));
        assert_eq!(translate(&key(Keysym::Down, None), &m), Some(GreeterInput::NextSession));
        assert_eq!(translate(&key(Keysym::Up, None), &m), Some(GreeterInput::PrevSession));
        let alt = Modifiers { alt: true, ..Default::default() };
        assert_eq!(translate(&key(Keysym::a, Some("a")), &alt), None);
    }

    #[test]
    fn layout_names_from_symbols() {
        let km = "xkb_keymap {\n xkb_symbols \"pc+us+ru:2+inet(evdev)+group(alt_shift_toggle)\" {\n name[Group1]=\"English (US)\";\n name[Group2]=\"Russian\";\n";
        assert_eq!(layout_names(km), vec!["EN", "RU"]);
        let km = "xkb_symbols \"pc+de(nodeadkeys)+inet(evdev)\" {";
        assert_eq!(layout_names(km), vec!["DE"]);
        // libxkbcommon ≥ 1.8 (what niri sends)
        let km = "xkb_symbols \"pc_us_ru_2_inet(evdev)\" {\n\tname[1]=\"English (US)\";\n\tname[2]=\"Russian\";\n";
        assert_eq!(layout_names(km), vec!["EN", "RU"]);
    }

    #[test]
    fn layout_names_fallback_to_long_names() {
        let km = "xkb_symbols {\n\tname[Group1]=\"English (US)\";\n\tname[Group2]=\"French\";\n";
        assert_eq!(layout_names(km), vec!["EN", "FR"]);
    }
}
