//! Just enough EDID to name a monitor.

/// A human name for the display described by a 128+ byte EDID block: the "monitor name"
/// descriptor (tag 0xFC) when present, else `<vendor> <product code>`.
pub fn display_name(edid: &[u8]) -> Option<String> {
    if edid.len() < 128 || edid[..8] != [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00] {
        return None;
    }
    // Four 18-byte descriptors at 54, 72, 90, 108. Display descriptors start 00 00 00 <tag>.
    for off in [54usize, 72, 90, 108] {
        let d = &edid[off..off + 18];
        if d[0] == 0 && d[1] == 0 && d[2] == 0 && d[3] == 0xFC {
            let text: String = d[5..18].iter().take_while(|b| **b != 0x0A && **b != 0).map(|b| char::from(*b)).collect();
            let text = text.trim();
            if !text.is_empty() {
                return Some(text.to_string());
            }
        }
    }
    // Manufacturer id: three 5-bit letters in bytes 8-9 ("A" = 1).
    let m = u16::from_be_bytes([edid[8], edid[9]]);
    let letter = |shift: u16| char::from(b'@' + ((m >> shift) & 0x1F) as u8);
    let vendor: String = [letter(10), letter(5), letter(0)].iter().collect();
    let product = u16::from_le_bytes([edid[10], edid[11]]);
    Some(format!("{vendor} {product:04X}"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A syntactically valid 128-byte EDID with an optional monitor-name descriptor.
    pub(crate) fn sample(name: Option<&str>) -> Vec<u8> {
        let mut e = vec![0u8; 128];
        e[..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
        // "DEL" = D(4) E(5) L(12): 0b0_00100_00101_01100
        let m: u16 = (4 << 10) | (5 << 5) | 12;
        e[8..10].copy_from_slice(&m.to_be_bytes());
        e[10..12].copy_from_slice(&0x41A8u16.to_le_bytes());
        if let Some(n) = name {
            let d = &mut e[72..90];
            d[3] = 0xFC;
            let mut text = n.as_bytes().to_vec();
            text.push(0x0A);
            while text.len() < 13 {
                text.push(0x20);
            }
            d[5..18].copy_from_slice(&text[..13]);
        }
        e
    }

    #[test]
    fn reads_the_monitor_name_descriptor() {
        assert_eq!(display_name(&sample(Some("DELL U2723QE"))).as_deref(), Some("DELL U2723QE"));
        assert_eq!(display_name(&sample(Some("LG"))).as_deref(), Some("LG"));
    }

    #[test]
    fn falls_back_to_vendor_and_product() {
        assert_eq!(display_name(&sample(None)).as_deref(), Some("DEL 41A8"));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(display_name(&[]), None);
        assert_eq!(display_name(&[0u8; 128]), None);
        assert_eq!(display_name(&[0xFF; 200]), None);
    }
}
