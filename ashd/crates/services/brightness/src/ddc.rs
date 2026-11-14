//! DDC/CI over I2C: the protocol subset needed for brightness (VCP feature 0x10).
//!
//! **Untested on real hardware** — every byte here is verified against the DDC/CI framing
//! rules by unit tests with a fake monitor ([`tests::FakeMonitor`]) and never against a
//! real display (some monitors misbehave on DDC traffic; see the crate docs).
//!
//! Framing (VESA DDC/CI 1.1; the monitor's 7-bit I2C address is 0x37, i.e. 0x6E as a
//! write address; the host's virtual address on the wire is 0x51):
//!
//! * *Get VCP Feature* request: `51 82 01 <code> <chk>`, `chk = 6E ^ 51 ^ 82 ^ 01 ^ code`.
//! * its reply (the monitor needs >= 40 ms before it can answer):
//!   `6E <0x80|len=8> 02 <result> <code> <type> <max hi> <max lo> <cur hi> <cur lo> <chk>`,
//!   `chk = 50 ^ (xor of all preceding bytes)`; `result` 0 = ok, 1 = unsupported feature.
//! * *Set VCP Feature*: `51 84 03 <code> <hi> <lo> <chk>`, `chk = 6E ^ (xor of the bytes
//!   before it)`; no reply, but the monitor is busy for ~50 ms afterwards.

use std::{io, time::Duration};

/// VCP code of the luminance / brightness feature.
pub const VCP_BRIGHTNESS: u8 = 0x10;
/// 7-bit I2C slave address of DDC/CI.
pub const DDC_ADDR: u16 = 0x37;

const DEST_WRITE: u8 = 0x6E;
const HOST: u8 = 0x51;
const REPLY_SEED: u8 = 0x50;
const OP_GET: u8 = 0x01;
const OP_GET_REPLY: u8 = 0x02;
const OP_SET: u8 = 0x03;
/// Length of a *Get VCP Feature* reply on the wire.
pub const GET_REPLY_LEN: usize = 11;

/// Something a DDC transaction can fail on.
#[derive(Debug)]
pub enum DdcError {
    Io(io::Error),
    /// The reply did not parse (bad checksum, wrong length / opcode, null message).
    BadReply(&'static str),
    /// The monitor does not implement the feature (result code 1).
    Unsupported,
    /// The reply was for another VCP code.
    WrongCode { asked: u8, got: u8 },
}

impl std::fmt::Display for DdcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DdcError::Io(e) => write!(f, "i2c: {e}"),
            DdcError::BadReply(why) => write!(f, "bad DDC/CI reply: {why}"),
            DdcError::Unsupported => write!(f, "feature not supported by the monitor"),
            DdcError::WrongCode { asked, got } => write!(f, "asked for VCP {asked:#04x}, got {got:#04x}"),
        }
    }
}

impl From<io::Error> for DdcError {
    fn from(e: io::Error) -> Self {
        DdcError::Io(e)
    }
}

/// A VCP feature's value and its range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Vcp {
    pub current: u16,
    pub max: u16,
}

impl Vcp {
    /// 0.0..=1.0 (0 when the monitor reports a zero range).
    pub fn fraction(self) -> f32 {
        if self.max == 0 { 0.0 } else { (f32::from(self.current) / f32::from(self.max)).clamp(0.0, 1.0) }
    }
}

/// The bus the monitor sits on: plain `write` / `read` transactions to the DDC address.
pub trait I2cBus: Send {
    fn write(&mut self, data: &[u8]) -> io::Result<()>;
    /// Read up to `buf.len()` bytes; returns how many arrived.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
}

impl I2cBus for Box<dyn I2cBus> {
    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        (**self).write(data)
    }
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (**self).read(buf)
    }
}

/// Delays the protocol needs (zeroed in tests).
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    /// Between a *Get* request and reading its reply.
    pub reply_delay: Duration,
    /// After a *Set* (and between retries): the monitor is busy.
    pub busy_delay: Duration,
    /// Attempts per transaction.
    pub attempts: u32,
}

impl Timing {
    pub const REAL: Timing = Timing { reply_delay: Duration::from_millis(50), busy_delay: Duration::from_millis(50), attempts: 3 };
    pub const INSTANT: Timing = Timing { reply_delay: Duration::ZERO, busy_delay: Duration::ZERO, attempts: 3 };
}

/// `51 82 01 <code> <chk>`.
pub fn get_request(code: u8) -> [u8; 5] {
    let mut f = [HOST, 0x82, OP_GET, code, 0];
    f[4] = checksum(DEST_WRITE, &f[..4]);
    f
}

/// `51 84 03 <code> <hi> <lo> <chk>`.
pub fn set_request(code: u8, value: u16) -> [u8; 7] {
    let [hi, lo] = value.to_be_bytes();
    let mut f = [HOST, 0x84, OP_SET, code, hi, lo, 0];
    f[6] = checksum(DEST_WRITE, &f[..6]);
    f
}

fn checksum(seed: u8, bytes: &[u8]) -> u8 {
    bytes.iter().fold(seed, |acc, b| acc ^ b)
}

/// Parse an 11-byte *Get VCP Feature* reply for `code`.
pub fn parse_get_reply(code: u8, reply: &[u8]) -> Result<Vcp, DdcError> {
    if reply.len() < GET_REPLY_LEN {
        return Err(DdcError::BadReply("short"));
    }
    let r = &reply[..GET_REPLY_LEN];
    if r[0] != DEST_WRITE {
        return Err(DdcError::BadReply("source address"));
    }
    if r[1] & 0x80 == 0 || r[1] & 0x7F != 8 {
        return Err(DdcError::BadReply("length"));
    }
    if checksum(REPLY_SEED, &r[..GET_REPLY_LEN - 1]) != r[GET_REPLY_LEN - 1] {
        return Err(DdcError::BadReply("checksum"));
    }
    if r[2] != OP_GET_REPLY {
        return Err(DdcError::BadReply("opcode"));
    }
    match r[3] {
        0 => {}
        1 => return Err(DdcError::Unsupported),
        _ => return Err(DdcError::BadReply("result code")),
    }
    if r[4] != code {
        return Err(DdcError::WrongCode { asked: code, got: r[4] });
    }
    Ok(Vcp { max: u16::from_be_bytes([r[6], r[7]]), current: u16::from_be_bytes([r[8], r[9]]) })
}

/// One monitor.
pub struct DdcCi<B: I2cBus> {
    bus: B,
    timing: Timing,
}

impl<B: I2cBus> DdcCi<B> {
    pub fn new(bus: B, timing: Timing) -> Self {
        Self { bus, timing }
    }

    pub fn get_vcp(&mut self, code: u8) -> Result<Vcp, DdcError> {
        let mut last = DdcError::BadReply("no attempt");
        for attempt in 0..self.timing.attempts.max(1) {
            if attempt > 0 {
                std::thread::sleep(self.timing.busy_delay);
            }
            match self.try_get(code) {
                Ok(v) => return Ok(v),
                // A feature the monitor does not have will not appear on a retry.
                Err(e @ DdcError::Unsupported) => return Err(e),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    fn try_get(&mut self, code: u8) -> Result<Vcp, DdcError> {
        self.bus.write(&get_request(code))?;
        std::thread::sleep(self.timing.reply_delay);
        let mut buf = [0u8; GET_REPLY_LEN];
        let n = self.bus.read(&mut buf)?;
        parse_get_reply(code, &buf[..n])
    }

    /// Set a feature. The bus gives no acknowledgement beyond the write succeeding, so the
    /// caller verifies with [`get_vcp`](Self::get_vcp) when it cares.
    pub fn set_vcp(&mut self, code: u8, value: u16) -> Result<(), DdcError> {
        let mut last = None;
        for attempt in 0..self.timing.attempts.max(1) {
            if attempt > 0 {
                std::thread::sleep(self.timing.busy_delay);
            }
            match self.bus.write(&set_request(code, value)) {
                Ok(()) => {
                    std::thread::sleep(self.timing.busy_delay);
                    return Ok(());
                }
                Err(e) => last = Some(e),
            }
        }
        Err(DdcError::Io(last.unwrap_or_else(|| io::Error::other("no attempt"))))
    }

    pub fn brightness(&mut self) -> Result<Vcp, DdcError> {
        self.get_vcp(VCP_BRIGHTNESS)
    }

    /// Set the brightness to a fraction of the monitor's maximum.
    pub fn set_brightness(&mut self, max: u16, fraction: f32) -> Result<(), DdcError> {
        let raw = (fraction.clamp(0.0, 1.0) * f32::from(max)).round() as u16;
        self.set_vcp(VCP_BRIGHTNESS, raw.min(max))
    }
}

/// `/dev/i2c-N` with the DDC address selected.
pub struct DevI2c {
    file: std::fs::File,
}

impl DevI2c {
    /// `I2C_SLAVE` ioctl request number.
    const I2C_SLAVE: libc::c_ulong = 0x0703;

    /// Open `path` read-write and address the monitor. Needs the `i2c-dev` module and
    /// membership of the `i2c` group; fails cleanly otherwise.
    pub fn open(path: &std::path::Path) -> io::Result<Self> {
        use std::os::fd::AsRawFd as _;
        let file = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
        // SAFETY: plain ioctl on a descriptor we own, with an integer argument.
        let rc = unsafe { libc::ioctl(file.as_raw_fd(), Self::I2C_SLAVE, libc::c_ulong::from(DDC_ADDR)) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { file })
    }
}

impl I2cBus for DevI2c {
    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        use std::io::Write as _;
        self.file.write_all(data)
    }
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        use std::io::Read as _;
        self.file.read(buf)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// A monitor that speaks DDC/CI strictly: it NACKs frames with a bad checksum, answers
    /// *Get VCP* for brightness (and reports other codes as unsupported), obeys *Set*.
    #[derive(Clone)]
    pub(crate) struct FakeMonitor {
        pub shared: Arc<Mutex<MonitorState>>,
    }

    pub(crate) struct MonitorState {
        pub brightness: u16,
        pub max: u16,
        pub reply: Vec<u8>,
        /// How many upcoming *Get* replies to garble (to exercise retries).
        pub garble: u32,
        /// Every frame the host wrote, verbatim.
        pub wire: Vec<Vec<u8>>,
        /// Fail the next N writes with an I/O error (NACK).
        pub nack: u32,
    }

    impl FakeMonitor {
        pub(crate) fn new(brightness: u16, max: u16) -> Self {
            Self { shared: Arc::new(Mutex::new(MonitorState { brightness, max, reply: Vec::new(), garble: 0, wire: Vec::new(), nack: 0 })) }
        }

        fn reply_frame(code: u8, result: u8, max: u16, cur: u16) -> Vec<u8> {
            let mut f = vec![0x6E, 0x88, 0x02, result, code, 0x00];
            f.extend(max.to_be_bytes());
            f.extend(cur.to_be_bytes());
            let chk = f.iter().fold(0x50u8, |a, b| a ^ b);
            f.push(chk);
            f
        }
    }

    impl I2cBus for FakeMonitor {
        fn write(&mut self, data: &[u8]) -> io::Result<()> {
            let mut m = self.shared.lock().unwrap();
            m.wire.push(data.to_vec());
            if m.nack > 0 {
                m.nack -= 1;
                return Err(io::Error::other("NACK"));
            }
            // A valid frame: host address, length byte, payload, checksum seeded with 0x6E.
            let valid = data.len() >= 3 && data[0] == 0x51 && usize::from(data[1] & 0x7F) == data.len() - 3 && data.iter().fold(0x6Eu8, |a, b| a ^ b) == 0;
            if !valid {
                return Err(io::Error::other("bad frame NACKed"));
            }
            match (data[2], data.len()) {
                (0x01, 5) => {
                    let code = data[3];
                    let mut frame = if code == VCP_BRIGHTNESS {
                        Self::reply_frame(code, 0, m.max, m.brightness)
                    } else {
                        Self::reply_frame(code, 1, 0, 0)
                    };
                    if m.garble > 0 {
                        m.garble -= 1;
                        let last = frame.len() - 1;
                        frame[last] ^= 0xFF;
                    }
                    m.reply = frame;
                    Ok(())
                }
                (0x03, 7) if data[3] == VCP_BRIGHTNESS => {
                    let v = u16::from_be_bytes([data[4], data[5]]);
                    if v > m.max {
                        return Err(io::Error::other("out of range"));
                    }
                    m.brightness = v;
                    Ok(())
                }
                _ => Err(io::Error::other("unsupported opcode")),
            }
        }

        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let mut m = self.shared.lock().unwrap();
            let n = m.reply.len().min(buf.len());
            buf[..n].copy_from_slice(&m.reply[..n]);
            m.reply.clear();
            Ok(n)
        }
    }

    #[test]
    fn request_frames_match_the_spec_examples() {
        // ddcutil's well-known "get brightness": 51 82 01 10 and checksum 6E^51^82^01^10 = AC.
        assert_eq!(get_request(0x10), [0x51, 0x82, 0x01, 0x10, 0xAC]);
        // set brightness to 0x0032: 51 84 03 10 00 32, chk = 6E^51^84^03^10^00^32.
        let f = set_request(0x10, 0x32);
        assert_eq!(&f[..6], &[0x51, 0x84, 0x03, 0x10, 0x00, 0x32]);
        assert_eq!(f[6], 0x6E ^ 0x51 ^ 0x84 ^ 0x03 ^ 0x10 ^ 0x32);
        // Every request verifies to zero when the checksum is folded in.
        for v in [0u16, 1, 50, 255, 1000, u16::MAX] {
            assert_eq!(set_request(0x10, v).iter().fold(0x6Eu8, |a, b| a ^ b), 0);
        }
        assert_eq!(get_request(0x12).iter().fold(0x6Eu8, |a, b| a ^ b), 0);
    }

    #[test]
    fn reply_parsing_checks_everything() {
        let ok = FakeMonitor::reply_frame(0x10, 0, 100, 37);
        assert_eq!(parse_get_reply(0x10, &ok).unwrap(), Vcp { current: 37, max: 100 });
        assert!((parse_get_reply(0x10, &ok).unwrap().fraction() - 0.37).abs() < 1e-6);

        let mut bad_chk = ok.clone();
        bad_chk[10] ^= 1;
        assert!(matches!(parse_get_reply(0x10, &bad_chk), Err(DdcError::BadReply("checksum"))));
        assert!(matches!(parse_get_reply(0x10, &ok[..10]), Err(DdcError::BadReply("short"))));
        let unsupported = FakeMonitor::reply_frame(0x10, 1, 0, 0);
        assert!(matches!(parse_get_reply(0x10, &unsupported), Err(DdcError::Unsupported)));
        let other = FakeMonitor::reply_frame(0x12, 0, 100, 5);
        assert!(matches!(parse_get_reply(0x10, &other), Err(DdcError::WrongCode { asked: 0x10, got: 0x12 })));
        let mut wrong_src = ok.clone();
        wrong_src[0] = 0x6F;
        assert!(parse_get_reply(0x10, &wrong_src).is_err());
        let mut wrong_op = ok;
        wrong_op[2] = 0x09;
        wrong_op[10] = wrong_op[..10].iter().fold(0x50u8, |a, b| a ^ b);
        assert!(matches!(parse_get_reply(0x10, &wrong_op), Err(DdcError::BadReply("opcode"))));
        // The all-zero "null message" some monitors send while busy is not a reply.
        assert!(parse_get_reply(0x10, &[0u8; 11]).is_err());
    }

    #[test]
    fn get_and_set_round_trip_against_the_fake_monitor() {
        let mon = FakeMonitor::new(40, 100);
        let mut ddc = DdcCi::new(mon.clone(), Timing::INSTANT);
        assert_eq!(ddc.brightness().unwrap(), Vcp { current: 40, max: 100 });
        ddc.set_brightness(100, 0.65).unwrap();
        assert_eq!(mon.shared.lock().unwrap().brightness, 65);
        assert_eq!(ddc.brightness().unwrap().current, 65);
        // The wire carried exactly the spec frames.
        let wire = mon.shared.lock().unwrap().wire.clone();
        assert_eq!(wire[0], get_request(0x10));
        assert_eq!(wire[1], set_request(0x10, 65));
    }

    #[test]
    fn a_monitor_with_a_larger_range_is_scaled() {
        let mon = FakeMonitor::new(0, 255);
        let mut ddc = DdcCi::new(mon.clone(), Timing::INSTANT);
        ddc.set_brightness(255, 0.5).unwrap();
        assert_eq!(mon.shared.lock().unwrap().brightness, 128);
        ddc.set_brightness(255, 2.0).unwrap();
        assert_eq!(mon.shared.lock().unwrap().brightness, 255, "clamped to the maximum");
        ddc.set_brightness(255, -1.0).unwrap();
        assert_eq!(mon.shared.lock().unwrap().brightness, 0);
    }

    #[test]
    fn garbled_replies_and_nacks_are_retried_a_limited_number_of_times() {
        let mon = FakeMonitor::new(40, 100);
        mon.shared.lock().unwrap().garble = 2;
        let mut ddc = DdcCi::new(mon.clone(), Timing::INSTANT);
        assert_eq!(ddc.brightness().unwrap().current, 40, "third attempt succeeds");

        mon.shared.lock().unwrap().garble = 3;
        assert!(ddc.brightness().is_err(), "three bad replies in a row give up");

        let mon = FakeMonitor::new(40, 100);
        mon.shared.lock().unwrap().nack = 2;
        let mut ddc = DdcCi::new(mon.clone(), Timing::INSTANT);
        ddc.set_brightness(100, 0.1).unwrap();
        assert_eq!(mon.shared.lock().unwrap().brightness, 10);
        mon.shared.lock().unwrap().nack = 5;
        assert!(matches!(ddc.set_brightness(100, 0.2), Err(DdcError::Io(_))));
        assert_eq!(mon.shared.lock().unwrap().brightness, 10, "a failed write changed nothing");
    }

    #[test]
    fn unsupported_features_fail_fast() {
        let mon = FakeMonitor::new(40, 100);
        let mut ddc = DdcCi::new(mon.clone(), Timing::INSTANT);
        assert!(matches!(ddc.get_vcp(0x99), Err(DdcError::Unsupported)));
        let gets = mon.shared.lock().unwrap().wire.iter().filter(|w| w[2] == 0x01).count();
        assert_eq!(gets, 1, "no retries for an unsupported feature");
    }
}
