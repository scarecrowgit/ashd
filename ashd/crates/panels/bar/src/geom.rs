//! Bar geometry measured on desk-05 (bottom bar, l.1589–1772), det-12 (network button
//! states) and det-13 (session button). Theme-dependent sizes come from the theme.

/// Status block: `padding: 0 14px; gap: 12px` (l.1591).
pub const PAD_X: f32 = 14.0;
pub const GAP: f32 = 12.0;
/// Clock: mono 16/500, line-height 1.1; date: mono 9.5 caps (l.1593–1594).
pub const CLOCK_SIZE: f32 = 16.0;
pub const DATE_SIZE: f32 = 9.5;
pub const CLOCK_LINE: f32 = 1.1;
/// Separators: 1×22 `line` (l.1596).
pub const SEP_H: f32 = 22.0;
/// Meter groups: gap 6, caption mono 10, meter 34×4, value mono 10.5 (l.1598–1604).
pub const METER_GAP: f32 = 6.0;
pub const CAPTION_SIZE: f32 = 10.0;
pub const VALUE_SIZE: f32 = 10.5;
pub const METER_W: f32 = 34.0;
/// Status glyphs are 14 (l.1617); icon ↔ value gap 5 (volume) / 6 (network, det-12).
pub const STATUS_ICON: f32 = 14.0;
pub const ICON_GAP: f32 = 5.0;
pub const NET_GAP: f32 = 6.0;
/// The network/volume control: 30 high, padding 0 8 (det-12 l.79).
pub const CONTROL_H: f32 = 30.0;
pub const CONTROL_PAD_X: f32 = 8.0;
/// Open-popup marker: 2px accent underline (det-12 `inset 0 -2px 0 accent`).
pub const UNDERLINE_H: f32 = 2.0;
/// "no internet" warn dot: 6×6 at right −3, top −2 of the glyph (det-12).
pub const NET_DOT: f32 = 6.0;
/// Keyboard layout chip: 22 high, padding 0 7, mono 10.5/500, accent border (l.1646).
pub const CHIP_H: f32 = 22.0;
pub const CHIP_PAD_X: f32 = 7.0;
/// Bell: 26×26, 15px glyph, unread dot 6×6 at top 3 right 3 (l.1647–1654).
pub const BELL: f32 = 26.0;
pub const BELL_ICON: f32 = 15.0;
pub const BELL_DOT: f32 = 6.0;
pub const BELL_DOT_INSET: f32 = 3.0;

/// Bar end (l.1745–1770): right 14, gap 14; label group gap 10 (index mono 11, name /
/// mode mono 10.5); segmented: padding 2, gap 2, 1px line border, segments 28 high,
/// padding 0 10, gap 6, 14px glyph, sans 12/500.
pub const END_GAP: f32 = 14.0;
pub const END_LABEL_GAP: f32 = 10.0;
pub const END_INDEX_SIZE: f32 = 11.0;
pub const SEG_PAD: f32 = 2.0;
pub const SEG_H: f32 = 28.0;
pub const SEG_PAD_X: f32 = 10.0;
pub const SEG_ICON_GAP: f32 = 6.0;
pub const SEG_ICON: f32 = 14.0;
pub const SEG_TEXT: f32 = 12.0;
/// Session button: 32×32, 15px power glyph (det-13 l.284).
pub const SESSION: f32 = 32.0;
pub const SESSION_ICON: f32 = 15.0;

/// Tray (not in the mockups; sized like the bell): 24×24 hit boxes around 16px icons,
/// 2px apart, a 16×24 chevron with a 12px glyph for what does not fit, 6px warn dot at
/// top 2 right 2 for NeedsAttention.
pub const TRAY_BOX: f32 = 24.0;
pub const TRAY_ICON: f32 = 16.0;
pub const TRAY_GAP: f32 = 2.0;
pub const TRAY_CHEVRON_W: f32 = 16.0;
pub const TRAY_CHEVRON_ICON: f32 = 12.0;
pub const TRAY_DOT: f32 = 6.0;
pub const TRAY_DOT_INSET: f32 = 2.0;
/// Tooltip: appears after this long over an icon, 6px above the bar.
pub const TRAY_TOOLTIP_DELAY_MS: u64 = 400;
pub const TRAY_TOOLTIP_GAP: f32 = 6.0;
/// One wheel notch in the `Scroll` unit apps expect (KDE: multiples of 120); a
/// touchpad needs this many pixels per notch.
pub const TRAY_SCROLL_NOTCH: i32 = 120;
pub const TRAY_SCROLL_PIXELS: f32 = 40.0;
/// A dbusmenu separator row: 1px rule with 4px margins.
pub const MENU_SEPARATOR_H: f32 = 9.0;
/// Leading slot (check / radio / icon) of a tray menu row.
pub const MENU_LEAD: f32 = 14.0;
/// Gap of the status row while the tray is shown. The Ash status block is 560 px with
/// only ~10 px to spare (a VPN label in the network control eats the rest), so the row
/// tightens from `GAP` (12) to make room for at least the chevron.
pub const TRAY_ROW_GAP: f32 = 8.0;
/// A tooltip hides itself after this long even if the pointer never "left" (GPUI keeps the
/// last position when the pointer exits a layer surface quickly).
pub const TRAY_TOOLTIP_TTL_MS: u64 = 5000;

/// Now-playing control in the tray area (not in the mockups; sized like a tray icon):
/// a 24 high box, 14px music glyph, title (mono value size) after a 6px gap, at most
/// 120 and at least 40 wide, otherwise glyph only.
pub const NOW_PLAYING_GLYPH_W: f32 = TRAY_BOX;
pub const NOW_PLAYING_ICON: f32 = STATUS_ICON;
pub const NOW_PLAYING_TITLE_GAP: f32 = 6.0;
pub const NOW_PLAYING_TITLE_MIN: f32 = 40.0;
pub const NOW_PLAYING_TITLE_MAX: f32 = 120.0;
/// The VPN name in the network control is cut at this width (a long name used to eat the
/// whole status pill).
pub const VPN_LABEL_MAX: f32 = 64.0;
/// A VPN label narrower than this many characters is left out (the shield alone says it):
/// a lone ellipsis next to the shield is worse than nothing.
pub const VPN_LABEL_MIN_CHARS: usize = 3;

/// The width the VPN label may take with `room` px left in the status block (what the tray
/// leaves): `None` when fewer than [`VPN_LABEL_MIN_CHARS`] characters of the label font fit.
pub fn vpn_label_width(room: f32) -> Option<f32> {
    let min = slot_w(VPN_LABEL_MIN_CHARS, CAPTION_SIZE);
    (room >= min).then(|| room.min(VPN_LABEL_MAX))
}

// ── fixed slots for the numeric readouts ─────────────────────────────────────────────────

/// Advance of a monospace glyph in em (JetBrains Mono, the shell's mono font; every mono
/// face the themes name is 0.6 em).
pub const MONO_ADVANCE: f32 = 0.6;
/// Widest text of each numeric readout: `100%`, `99.9G`, `150` / `mute`, `100%`.
pub const CPU_CHARS: usize = 4;
pub const MEM_CHARS: usize = 5;
pub const VOLUME_CHARS: usize = 4;
pub const BATTERY_CHARS: usize = 4;

/// Width of a slot for `chars` mono characters at `size`. Values are right-aligned in it,
/// so a digit appearing or disappearing never moves what is next to it.
pub fn slot_w(chars: usize, size: f32) -> f32 {
    (chars as f32 * size * MONO_ADVANCE).ceil()
}

/// What a status row can contain, for [`row_width`] / [`row_gap`].
#[derive(Clone, Copy, Debug)]
pub struct Parts {
    pub battery: bool,
    /// Characters of the network text (`2.5G`, `no net`) next to the glyph, if any.
    pub net_text: Option<usize>,
    /// The VPN shield, and the width its label gets.
    pub vpn_label: Option<f32>,
    /// The tray area, at least the width given (the chevron); `None`: no tray.
    pub tray: Option<f32>,
}

/// The gaps a row may use, widest first; with a tray the widest is [`TRAY_ROW_GAP`].
const GAPS: [f32; 3] = [GAP, TRAY_ROW_GAP, 6.0];

/// Left padding of the row: the clock text sits at `PAD_X` (mockup l.1591) counting its own
/// 4 px. The right side is the row's own gap: the bell's box is as far from the block's
/// border as from the layout chip (the mockup has 12 / 14).
pub const PAD_LEFT: f32 = PAD_X - 4.0;

/// Width of the status row for `parts` with `gap` between items, every numeric readout at
/// its widest value, the layout chip and the bell present.
pub fn row_width(parts: Parts, gap: f32) -> f32 {
    let value = |chars: usize| slot_w(chars, VALUE_SIZE);
    // clock: 4 px padding each side, `14:51` at 16 px; the date line is narrower or equal
    let clock = 2.0 * 4.0 + slot_w(5, CLOCK_SIZE).max(slot_w(10, DATE_SIZE));
    let meter = |chars: usize| slot_w(3, CAPTION_SIZE) + METER_GAP + METER_W + METER_GAP + value(chars);
    // network: glyph, text, shield + label; then the volume glyph and value
    let mut net = STATUS_ICON;
    if let Some(chars) = parts.net_text {
        net += NET_GAP + value(chars);
    }
    if let Some(label) = parts.vpn_label {
        net += NET_GAP + VPN_ICON + VPN_GAP + label;
    }
    let netvol = 2.0 * CONTROL_PAD_X + net + GAP + STATUS_ICON + ICON_GAP + value(VOLUME_CHARS);
    let battery = parts.battery.then(|| STATUS_ICON + ICON_GAP + value(BATTERY_CHARS));
    let chip = CHIP_H.max(2.0 * CHIP_PAD_X + 2.0 + value(2));
    let items: Vec<f32> = [clock, 1.0, meter(CPU_CHARS), meter(MEM_CHARS), 1.0, netvol]
        .into_iter()
        .chain(battery)
        .chain([1.0, chip])
        .chain(parts.tray)
        .chain([BELL])
        .collect();
    PAD_LEFT + gap + items.iter().sum::<f32>() + gap * (items.len() as f32 - 1.0)
}

/// Room the tray area can take: what is left of [`BLOCK_MAX_W`] once everything else (and
/// the gap the tray adds) is placed at `gap`. `parts.tray` is ignored.
pub fn tray_avail(parts: Parts, gap: f32) -> f32 {
    (BLOCK_MAX_W - row_width(Parts { tray: Some(0.0), ..parts }, gap)).max(0.0)
}

/// Does the row still fit `budget` at the tightest gap with the VPN shield (and no label)?
/// When not, the VPN indicator is left out rather than drawn cut in half.
pub fn vpn_fits(parts: Parts, budget: f32) -> bool {
    row_width(Parts { vpn_label: Some(0.0), ..parts }, GAPS[GAPS.len() - 1]) <= budget
}

/// The widest gap between the items of the row at which `parts` still fit `budget` (the
/// status block's width); 6 px at the very least. A tray caps it at [`TRAY_ROW_GAP`], so
/// the icons get room.
pub fn row_gap(parts: Parts, budget: f32) -> f32 {
    GAPS.into_iter()
        .filter(|g| parts.tray.is_none() || *g <= TRAY_ROW_GAP)
        .find(|g| row_width(parts, *g) <= budget)
        .unwrap_or(GAPS[GAPS.len() - 1])
}

/// Width of the status block at most (`metrics::STATUS_W`) minus its own frame: the block
/// hugs its content and only above this do the shrink rules (gap ladder, network control,
/// VPN shield) apply.
pub const BLOCK_MAX_W: f32 = ashd_tokens::metrics::STATUS_W - 2.0;

/// The VPN shield glyph and its gap to the label.
pub const VPN_ICON: f32 = 12.0;
pub const VPN_GAP: f32 = 4.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_fit_their_widest_text() {
        // JetBrains Mono at 10.5 px: 6.3 px per glyph, `99.9G` = 31.5 -> 32
        assert_eq!(slot_w(MEM_CHARS, VALUE_SIZE), 32.0);
        assert_eq!(slot_w(CPU_CHARS, VALUE_SIZE), 26.0);
        for (chars, text) in [(CPU_CHARS, "100%"), (MEM_CHARS, "99.9G"), (VOLUME_CHARS, "mute"), (VOLUME_CHARS, "150"), (BATTERY_CHARS, "100%")] {
            assert!(text.chars().count() <= chars, "{text}");
            assert!(slot_w(chars, VALUE_SIZE) >= text.chars().count() as f32 * VALUE_SIZE * MONO_ADVANCE, "{text}");
        }
    }

    #[test]
    fn the_worst_rows_fit_the_status_block() {
        let budget = BLOCK_MAX_W;
        // a desktop on a wire with a VPN: speed text, shield (the label takes what is left)
        let desktop = Parts { battery: false, net_text: Some(4), vpn_label: Some(0.0), tray: Some(TRAY_CHEVRON_W) };
        // a laptop on Wi-Fi: battery, no network text
        let laptop = Parts { battery: true, net_text: None, vpn_label: None, tray: Some(TRAY_CHEVRON_W) };
        for (name, parts) in [("desktop", desktop), ("laptop", laptop)] {
            let gap = row_gap(parts, budget);
            assert!(gap >= 6.0, "{name}: gap {gap}");
            let w = row_width(parts, gap);
            assert!(w <= budget, "{name}: {w} px does not fit {budget} px");
        }
    }

    #[test]
    fn the_block_hugs_a_typical_row_and_the_tray_gets_the_rest() {
        // a desktop on a wire, no battery, no VPN, no tray: under the maximum at the full gap
        let desktop = Parts { battery: false, net_text: Some(2), vpn_label: None, tray: None };
        let w = row_width(desktop, GAP);
        assert!(w < BLOCK_MAX_W, "{w}");
        // a laptop on Wi-Fi (battery) needs the next gap of the ladder
        assert_eq!(row_gap(Parts { battery: true, net_text: None, ..desktop }, BLOCK_MAX_W), TRAY_ROW_GAP);
        // the bell follows the chip: no spacer item between them (chip 29 + gap + bell 26)
        let with_bell = row_width(desktop, GAP);
        let without = row_width(Parts { tray: Some(0.0), ..desktop }, GAP);
        assert_eq!(without - with_bell, GAP, "a tray adds one item and one gap, nothing else");
        // the tray's room: everything the other items leave, never negative
        let room = tray_avail(desktop, TRAY_ROW_GAP);
        assert!(room >= TRAY_BOX, "{room}");
        let all = Parts { battery: true, net_text: Some(4), vpn_label: Some(0.0), tray: None };
        assert_eq!(tray_avail(all, 6.0), 0.0);
        // filling the room exactly lands on the maximum
        let full = Parts { tray: Some(room), ..desktop };
        assert!((row_width(full, TRAY_ROW_GAP) - BLOCK_MAX_W).abs() < 0.01);
    }

    #[test]
    fn a_vpn_label_with_no_room_for_three_characters_is_left_out() {
        assert_eq!(vpn_label_width(0.0), None);
        assert_eq!(vpn_label_width(10.0), None, "a lone ellipsis");
        assert_eq!(vpn_label_width(17.9), None);
        assert_eq!(vpn_label_width(18.0), Some(18.0));
        assert_eq!(vpn_label_width(200.0), Some(VPN_LABEL_MAX));
    }

    #[test]
    fn roomy_rows_keep_the_design_gap_and_full_rows_tighten() {
        let budget = BLOCK_MAX_W;
        let bare = Parts { battery: false, net_text: None, vpn_label: None, tray: None };
        assert_eq!(row_gap(bare, budget), GAP);
        assert!(row_width(bare, GAP) <= budget);
        // a tray never gets more than TRAY_ROW_GAP between items
        assert_eq!(row_gap(Parts { tray: Some(TRAY_CHEVRON_W), ..bare }, budget), TRAY_ROW_GAP);
        // everything at once (laptop + wire + VPN + now-playing glyph): the gap goes to the
        // floor and the rest is the network control's to clip, never the bell
        let all = Parts {
            battery: true,
            net_text: Some(4),
            vpn_label: Some(0.0),
            tray: Some(NOW_PLAYING_GLYPH_W + TRAY_GAP + TRAY_CHEVRON_W),
        };
        assert_eq!(row_gap(all, budget), 6.0);
        assert!(row_width(all, 6.0) > budget);
        // ... and then the VPN indicator is dropped instead of drawn cut in half
        assert!(!vpn_fits(all, budget));
        assert!(vpn_fits(Parts { battery: false, net_text: Some(4), vpn_label: None, tray: Some(TRAY_CHEVRON_W) }, budget));
        // widths are monotonic in the gap
        assert!(row_width(bare, 12.0) > row_width(bare, 8.0));
    }
}
