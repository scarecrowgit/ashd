//! Sunrise / sunset / solar elevation from latitude and longitude (NOAA's low-accuracy
//! formulas from the "General Solar Position Calculations" note: equation of time and
//! declination as Fourier series of the fractional year). Good to about two minutes
//! away from the polar circles, which is plenty for a colour-temperature schedule. No
//! network, no tables.

use chrono::{DateTime, Datelike as _, Duration, NaiveDate, Timelike as _, Utc};

/// Zenith of the sunrise / sunset instant: 90 degrees plus refraction and the solar disc.
const SUNRISE_ZENITH_DEG: f64 = 90.833;

/// What the sun does on one (UTC-dated) solar day.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SunDay {
    Normal { rise: DateTime<Utc>, set: DateTime<Utc> },
    /// The sun stays above the horizon.
    PolarDay,
    /// The sun stays below the horizon.
    PolarNight,
}

/// Equation of time (minutes) and solar declination (radians) for the fractional year
/// angle `gamma` (radians).
fn eqtime_decl(gamma: f64) -> (f64, f64) {
    let eqtime = 229.18
        * (0.000075 + 0.001868 * gamma.cos() - 0.032077 * gamma.sin() - 0.014615 * (2.0 * gamma).cos()
            - 0.040849 * (2.0 * gamma).sin());
    let decl = 0.006918 - 0.399912 * gamma.cos() + 0.070257 * gamma.sin() - 0.006758 * (2.0 * gamma).cos()
        + 0.000907 * (2.0 * gamma).sin()
        - 0.002697 * (3.0 * gamma).cos()
        + 0.00148 * (3.0 * gamma).sin();
    (eqtime, decl)
}

fn year_angle(date: NaiveDate, hour_utc: f64) -> f64 {
    let days = if date.leap_year() { 366.0 } else { 365.0 };
    2.0 * std::f64::consts::PI / days * (f64::from(date.ordinal0()) + (hour_utc - 12.0) / 24.0)
}

/// Sunrise and sunset around the solar noon of `date` at (`latitude`, `longitude`)
/// (degrees; north and east positive). The returned instants are not confined to `date`
/// (west of Greenwich the sunset falls on the next UTC day).
pub fn sun_day(date: NaiveDate, latitude: f64, longitude: f64) -> SunDay {
    let lat = latitude.clamp(-90.0, 90.0).to_radians();
    let midnight = date.and_hms_opt(0, 0, 0).expect("midnight exists").and_utc();
    // Two passes: evaluate the series at the solar noon, then at the instants found.
    let (mut rise_min, mut set_min) = (720.0 - 4.0 * longitude, 720.0 - 4.0 * longitude);
    for _ in 0..2 {
        let at = |m: f64| year_angle(date, m / 60.0);
        let (eq_r, decl_r) = eqtime_decl(at(rise_min));
        let (eq_s, decl_s) = eqtime_decl(at(set_min));
        let cos_ha = |decl: f64| SUNRISE_ZENITH_DEG.to_radians().cos() / (lat.cos() * decl.cos()) - lat.tan() * decl.tan();
        let (cr, cs) = (cos_ha(decl_r), cos_ha(decl_s));
        // Decide polar day / night from the noon evaluation of the first pass; later passes
        // only refine a normal day.
        if cr > 1.0 || cs > 1.0 {
            return SunDay::PolarNight;
        }
        if cr < -1.0 || cs < -1.0 {
            return SunDay::PolarDay;
        }
        let (ha_r, ha_s) = (cr.acos().to_degrees(), cs.acos().to_degrees());
        rise_min = 720.0 - 4.0 * (longitude + ha_r) - eq_r;
        set_min = 720.0 - 4.0 * (longitude - ha_s) - eq_s;
    }
    let instant = |minutes: f64| midnight + Duration::seconds((minutes * 60.0).round() as i64);
    SunDay::Normal { rise: instant(rise_min), set: instant(set_min) }
}

/// Solar elevation above the horizon in degrees at `when` (no refraction).
pub fn elevation_deg(when: DateTime<Utc>, latitude: f64, longitude: f64) -> f64 {
    let hour = f64::from(when.hour()) + f64::from(when.minute()) / 60.0 + f64::from(when.second()) / 3600.0;
    let (eqtime, decl) = eqtime_decl(year_angle(when.date_naive(), hour));
    let true_solar_minutes = hour * 60.0 + eqtime + 4.0 * longitude;
    let hour_angle = (true_solar_minutes / 4.0 - 180.0).to_radians();
    let lat = latitude.to_radians();
    let cos_zenith = lat.sin() * decl.sin() + lat.cos() * decl.cos() * hour_angle.cos();
    90.0 - cos_zenith.clamp(-1.0, 1.0).acos().to_degrees()
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;

    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn minutes_of(t: DateTime<Utc>, base: NaiveDate) -> f64 {
        (t - base.and_hms_opt(0, 0, 0).unwrap().and_utc()).num_seconds() as f64 / 60.0
    }

    #[track_caller]
    fn assert_near(actual: f64, expected_h: u32, expected_m: u32, tol_min: f64, what: &str) {
        let expected = f64::from(expected_h * 60 + expected_m);
        assert!((actual - expected).abs() <= tol_min, "{what}: {:.1} min vs {expected} min (tolerance {tol_min})", actual);
    }

    #[test]
    fn london_midsummer() {
        // 2024-06-21: sunrise 04:43 BST, sunset 21:21 BST (03:43 / 20:21 UTC).
        let day = d(2024, 6, 21);
        let SunDay::Normal { rise, set } = sun_day(day, 51.5, -0.1) else { panic!("normal day") };
        assert_near(minutes_of(rise, day), 3, 43, 4.0, "sunrise");
        assert_near(minutes_of(set, day), 20, 21, 4.0, "sunset");
    }

    #[test]
    fn moscow_midwinter() {
        // 2024-12-21: sunrise 08:57 MSK, sunset 15:57 MSK (05:57 / 12:57 UTC).
        let day = d(2024, 12, 21);
        let SunDay::Normal { rise, set } = sun_day(day, 55.75, 37.62) else { panic!("normal day") };
        assert_near(minutes_of(rise, day), 5, 57, 5.0, "sunrise");
        assert_near(minutes_of(set, day), 12, 57, 5.0, "sunset");
    }

    #[test]
    fn equinox_at_the_equator_is_twelve_hours() {
        let day = d(2024, 3, 20);
        let SunDay::Normal { rise, set } = sun_day(day, 0.0, 0.0) else { panic!("normal day") };
        let len = (set - rise).num_minutes();
        assert!((len - 727).abs() <= 8, "day length {len} min (refraction makes it ~12h07)");
        assert_near(minutes_of(rise, day), 6, 0, 10.0, "sunrise");
    }

    #[test]
    fn west_of_greenwich_the_sunset_is_on_the_next_utc_day() {
        // San Francisco, 2024-06-21: sunset 20:34 PDT = 03:34 UTC next day.
        let day = d(2024, 6, 21);
        let SunDay::Normal { rise, set } = sun_day(day, 37.77, -122.42) else { panic!("normal day") };
        assert!(set > rise);
        assert_eq!(set.date_naive(), d(2024, 6, 22));
        assert_near(minutes_of(set, day), 27, 34, 5.0, "sunset");
    }

    #[test]
    fn polar_day_and_night() {
        assert_eq!(sun_day(d(2024, 6, 21), 78.2, 15.6), SunDay::PolarDay);
        assert_eq!(sun_day(d(2024, 12, 21), 78.2, 15.6), SunDay::PolarNight);
        assert_eq!(sun_day(d(2024, 12, 21), -78.2, 15.6), SunDay::PolarDay);
    }

    #[test]
    fn elevation_matches_the_known_extremes() {
        // Solar noon at the equator on the equinox: sun nearly overhead.
        let noon = Utc.with_ymd_and_hms(2024, 3, 20, 12, 0, 0).unwrap();
        assert!(elevation_deg(noon, 0.0, 0.0) > 85.0);
        // London midsummer noon: 90 - 51.5 + 23.44 = 61.9 degrees.
        let noon = Utc.with_ymd_and_hms(2024, 6, 21, 12, 0, 0).unwrap();
        assert!((elevation_deg(noon, 51.5, 0.0) - 61.9).abs() < 1.0);
        // Midnight: below the horizon.
        let night = Utc.with_ymd_and_hms(2024, 6, 21, 0, 0, 0).unwrap();
        assert!(elevation_deg(night, 51.5, 0.0) < 0.0);
    }

    #[test]
    fn elevation_crosses_the_horizon_at_sunrise_and_sunset() {
        let day = d(2024, 9, 10);
        let SunDay::Normal { rise, set } = sun_day(day, 48.85, 2.35) else { panic!("normal day") };
        // The sunrise definition includes refraction (-0.833 degrees).
        for t in [rise, set] {
            let e = elevation_deg(t, 48.85, 2.35);
            assert!((e + 0.83).abs() < 0.6, "elevation at the event: {e}");
        }
    }
}
