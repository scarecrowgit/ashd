//! Colour temperature -> gamma ramps. Pure maths.
//!
//! The blackbody colour is Tanner Helland's well-known polynomial/logarithmic fit of the
//! Planckian locus (valid 1000-40000 K). It is normalised by its own value at 6500 K, so
//! 6500 K is *exactly* the identity ramp and colder temperatures only ever attenuate
//! channels (red stays at 1, blue drops first).

/// Neutral white: the identity ramp.
pub const NEUTRAL_K: u32 = 6500;
/// Warmest temperature the UI offers.
pub const MIN_K: u32 = 1000;

/// Per-channel multipliers (each 0..=1) of a blackbody at `kelvin`, 6500 K = (1, 1, 1).
/// Temperatures outside [`MIN_K`], [`NEUTRAL_K`] are clamped.
pub fn kelvin_to_rgb(kelvin: f32) -> [f32; 3] {
    let k = kelvin.clamp(MIN_K as f32, NEUTRAL_K as f32);
    let raw = helland(k);
    let white = helland(NEUTRAL_K as f32);
    [0, 1, 2].map(|i| (raw[i] / white[i]).clamp(0.0, 1.0))
}

/// Un-normalised blackbody colour, channels 0..=255.
fn helland(kelvin: f32) -> [f32; 3] {
    let t = f64::from(kelvin) / 100.0;
    let red = if t <= 66.0 { 255.0 } else { 329.698_727_446 * (t - 60.0).powf(-0.133_204_759_2) };
    let green = if t <= 66.0 {
        99.470_802_586_1 * t.ln() - 161.119_568_166_1
    } else {
        288.122_169_528_3 * (t - 60.0).powf(-0.075_514_849_2)
    };
    let blue = if t >= 66.0 {
        255.0
    } else if t <= 19.0 {
        0.0
    } else {
        138.517_731_223_1 * (t - 10.0).ln() - 305.044_792_730_7
    };
    [red, green, blue].map(|c| c.clamp(1.0, 255.0) as f32)
}

/// Interpolate between two temperatures in mired (1e6 / K): equal steps look equally
/// big, unlike steps in Kelvin.
pub fn lerp_kelvin(from: f32, to: f32, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t == 0.0 {
        return from;
    }
    if t == 1.0 {
        return to;
    }
    let (a, b) = (1.0e6 / from.max(1.0), 1.0e6 / to.max(1.0));
    1.0e6 / (a + (b - a) * t)
}

/// The table `zwlr_gamma_control_v1.set_gamma` wants: `size` red entries, then `size`
/// green, then `size` blue, each a `u16` ramp scaled by the channel multiplier.
pub fn build_ramp(size: u32, rgb: [f32; 3]) -> Vec<u16> {
    let size = size.max(2) as usize;
    let mut out = Vec::with_capacity(size * 3);
    for mult in rgb {
        for i in 0..size {
            let v = i as f64 / (size - 1) as f64;
            out.push((v * f64::from(mult) * 65535.0).round().clamp(0.0, 65535.0) as u16);
        }
    }
    out
}

/// [`build_ramp`] as the bytes of the shared-memory file (native endianness).
pub fn ramp_bytes(ramp: &[u16]) -> Vec<u8> {
    ramp.iter().flat_map(|v| v.to_ne_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_is_exactly_identity() {
        assert_eq!(kelvin_to_rgb(6500.0), [1.0, 1.0, 1.0]);
        for size in [2u32, 256, 1024, 4096] {
            let r = build_ramp(size, kelvin_to_rgb(6500.0));
            assert_eq!(r.len(), size as usize * 3);
            let n = size as usize;
            for ch in 0..3 {
                assert_eq!(r[ch * n], 0);
                assert_eq!(r[ch * n + n - 1], 65535);
                for i in 0..n {
                    let ideal = (i as f64 / (n - 1) as f64 * 65535.0).round() as u16;
                    assert_eq!(r[ch * n + i], ideal, "size {size} channel {ch} entry {i}");
                }
            }
        }
    }

    #[test]
    fn warmer_means_less_blue_then_less_green_and_red_stays() {
        let mut prev = [1.0f32; 3];
        for k in (1000..=6400).rev().step_by(100) {
            let c = kelvin_to_rgb(k as f32);
            assert_eq!(c[0], 1.0, "{k} K: red untouched");
            assert!(c[1] <= prev[1] + 1e-6 && c[2] <= prev[2] + 1e-6, "{k} K monotonic");
            assert!(c[2] <= c[1] + 1e-6 && c[1] <= c[0], "{k} K: blue <= green <= red");
            prev = c;
        }
    }

    #[test]
    fn known_points_are_plausible() {
        // Reference values of the same fit (Helland), normalised by 6500 K.
        let c = kelvin_to_rgb(3400.0);
        assert!((c[1] - 0.746).abs() < 0.01, "green at 3400 K was {}", c[1]);
        assert!((c[2] - 0.541).abs() < 0.01, "blue at 3400 K was {}", c[2]);
        let c = kelvin_to_rgb(1900.0); // candle-ish
        assert!(c[1] < 0.6 && c[2] < 0.15, "{c:?}");
        let c = kelvin_to_rgb(1000.0);
        assert!(c[2] < 0.01 && c[1] < 0.4 && c.iter().all(|v| (0.0..=1.0).contains(v)), "{c:?}");
    }

    #[test]
    fn out_of_range_temperatures_clamp() {
        assert_eq!(kelvin_to_rgb(10_000.0), [1.0, 1.0, 1.0]);
        assert_eq!(kelvin_to_rgb(20.0), kelvin_to_rgb(1000.0));
    }

    #[test]
    fn ramps_scale_the_channels() {
        let r = build_ramp(256, [1.0, 0.5, 0.0]);
        assert_eq!(r[255], 65535);
        assert_eq!(r[256 + 255], 32768);
        assert_eq!(r[512 + 255], 0);
        assert!(r[..256].windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn mired_interpolation_hits_the_ends_and_is_monotone() {
        assert!((lerp_kelvin(6500.0, 3400.0, 0.0) - 6500.0).abs() < 0.01);
        assert!((lerp_kelvin(6500.0, 3400.0, 1.0) - 3400.0).abs() < 0.01);
        let mid = lerp_kelvin(6500.0, 3400.0, 0.5);
        assert!(mid < 6500.0 && mid > 3400.0);
        // mired midpoint is the harmonic mean, below the arithmetic one
        assert!(mid < (6500.0 + 3400.0) / 2.0);
        let mut prev = 6500.0;
        for i in 1..=20 {
            let k = lerp_kelvin(6500.0, 3400.0, i as f32 / 20.0);
            assert!(k < prev);
            prev = k;
        }
    }

    #[test]
    fn bytes_are_native_endian_u16() {
        let b = ramp_bytes(&[0x0102, 0xfffe]);
        assert_eq!(b, [0x0102u16.to_ne_bytes(), 0xfffeu16.to_ne_bytes()].concat());
    }
}
