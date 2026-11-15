//! Worst-case values for the status block, for screenshots and layout checks in the nested
//! session: `ASHD_BAR_FIXTURE=worst` (honoured only with `ASHD_NESTED=1`, never on a real
//! desktop; `worst-desktop` is the same without a battery). CPU 100 %, memory at its widest label, battery 100 % charging, volume 150 %,
//! an unread notification. The network readout comes from `ASHD_NET_MOCK` (`vpn` shows the
//! widest one: wired speed plus a VPN name), the layout chip from the nested niri config.

use ashd_svc_sysinfo::{BatteryInfo, BatteryStatus, SysinfoState};

/// What to force.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fixture {
    /// Every readout at its widest value.
    Worst,
    /// The same on a machine without a battery.
    WorstDesktop,
    /// [`Fixture::Worst`] without the forced unread dot: a real notification flips it (the
    /// motion harness watches the dot pop in).
    WorstRead,
    /// The opposite: the narrowest readouts (`0%`, `1.2G`, volume 50, no battery, nothing
    /// unread), so the status block has room for tray items and the motion harness can watch
    /// them come and go.
    Calm,
}

/// The fixture requested by the environment, if any.
pub fn from_env() -> Option<Fixture> {
    let nested = std::env::var_os("ASHD_NESTED").is_some_and(|v| v == "1");
    parse(nested, std::env::var("ASHD_BAR_FIXTURE").ok().as_deref())
}

fn parse(nested: bool, value: Option<&str>) -> Option<Fixture> {
    match (nested, value) {
        (true, Some("worst")) => Some(Fixture::Worst),
        (true, Some("worst-desktop")) => Some(Fixture::WorstDesktop),
        (true, Some("worst-read")) => Some(Fixture::WorstRead),
        (true, Some("calm")) => Some(Fixture::Calm),
        _ => None,
    }
}

/// The forced readouts.
pub struct Forced {
    /// Volume percent and muted flag of the default sink.
    pub volume: (u32, bool),
    pub unread: bool,
}

impl Fixture {
    pub fn apply(self, sys: &mut SysinfoState) -> Forced {
        match self {
            Fixture::Calm => {
                sys.ready = true;
                sys.cpu = 0.0;
                sys.mem_total = 16 << 30;
                sys.mem_used = (1.2 * (1u64 << 30) as f64) as u64;
                sys.battery = None;
                Forced { volume: (50, false), unread: false }
            }
            Fixture::Worst | Fixture::WorstDesktop | Fixture::WorstRead => {
                sys.ready = true;
                sys.cpu = 1.0;
                // 99.9 GiB of 128: the widest memory label (`99.9G`)
                sys.mem_total = 128 << 30;
                sys.mem_used = ((99.9 * (1u64 << 30) as f64) as u64).min(sys.mem_total);
                sys.battery = (self != Fixture::WorstDesktop).then_some(BatteryInfo { percent: 100, status: BatteryStatus::Charging });
                Forced { volume: (150, false), unread: self != Fixture::WorstRead }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_in_the_nested_session() {
        assert_eq!(parse(true, Some("worst")), Some(Fixture::Worst));
        assert_eq!(parse(true, Some("worst-desktop")), Some(Fixture::WorstDesktop));
        assert_eq!(parse(true, Some("worst-read")), Some(Fixture::WorstRead));
        assert_eq!(parse(true, Some("calm")), Some(Fixture::Calm));
        assert_eq!(parse(false, Some("worst")), None);
        assert_eq!(parse(true, Some("nope")), None);
        assert_eq!(parse(true, None), None);
    }

    #[test]
    fn worst_is_the_widest_label() {
        let mut sys = SysinfoState::default();
        let forced = Fixture::Worst.apply(&mut sys);
        assert_eq!(sys.mem_used_label(), "99.9G");
        assert_eq!(sys.cpu, 1.0);
        assert_eq!(forced.volume, (150, false));
        assert!(sys.battery.is_some());
        let mut desktop = SysinfoState::default();
        Fixture::WorstDesktop.apply(&mut desktop);
        assert!(desktop.battery.is_none());
        assert!(forced.unread);
        assert!(!Fixture::WorstRead.apply(&mut SysinfoState::default()).unread);
        let mut calm = SysinfoState::default();
        let forced = Fixture::Calm.apply(&mut calm);
        assert!(calm.battery.is_none() && !forced.unread);
        assert_eq!(calm.mem_used_label(), "1.2G");
    }
}
