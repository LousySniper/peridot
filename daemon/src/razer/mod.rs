//! Razer USB control protocol: 90-byte feature reports.
//!
//! Layout: status, transaction id, remaining packets (BE16), protocol type,
//! data size, command class, command id, 80 argument bytes, XOR checksum of
//! bytes 2..88, reserved.

pub mod blackshark;
pub mod blackshark_x;
pub mod device;
pub mod devices;
pub mod table;

pub const REPORT_LEN: usize = 90;
pub const VARSTORE: u8 = 0x01;

pub const STATUS_NEW: u8 = 0x00;
pub const STATUS_BUSY: u8 = 0x01;
pub const STATUS_OK: u8 = 0x02;
pub const STATUS_FAIL: u8 = 0x03;
pub const STATUS_TIMEOUT: u8 = 0x04;
pub const STATUS_UNSUPPORTED: u8 = 0x05;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub status: u8,
    pub tid: u8,
    pub remaining: u16,
    pub protocol: u8,
    pub size: u8,
    pub class: u8,
    pub id: u8,
    pub args: [u8; 80],
}

impl Report {
    pub fn new(class: u8, id: u8, size: u8) -> Report {
        Report { status: STATUS_NEW, tid: 0xFF, remaining: 0, protocol: 0, size, class, id, args: [0; 80] }
    }

    pub fn with_args(mut self, args: &[u8]) -> Report {
        let n = args.len().min(80);
        self.args[..n].copy_from_slice(&args[..n]);
        self
    }

    pub fn encode(&self) -> [u8; REPORT_LEN] {
        let mut b = [0u8; REPORT_LEN];
        b[0] = self.status;
        b[1] = self.tid;
        b[2..4].copy_from_slice(&self.remaining.to_be_bytes());
        b[4] = self.protocol;
        b[5] = self.size;
        b[6] = self.class;
        b[7] = self.id;
        b[8..88].copy_from_slice(&self.args);
        b[88] = checksum(&b);
        b
    }

    pub fn decode(b: &[u8]) -> Option<Report> {
        if b.len() < REPORT_LEN {
            return None;
        }
        let mut args = [0u8; 80];
        args.copy_from_slice(&b[8..88]);
        Some(Report {
            status: b[0],
            tid: b[1],
            remaining: u16::from_be_bytes([b[2], b[3]]),
            protocol: b[4],
            size: b[5],
            class: b[6],
            id: b[7],
            args,
        })
    }

    pub fn arg_be16(&self, i: usize) -> u16 {
        u16::from_be_bytes([self.args[i], self.args[i + 1]])
    }
}

pub fn checksum(b: &[u8]) -> u8 {
    b[2..88].iter().fold(0u8, |acc, x| acc ^ x)
}

// ---- command builders (mirroring the OpenRazer driver) ----

pub fn get_firmware() -> Report {
    Report::new(0x00, 0x81, 0x02)
}
pub fn get_serial() -> Report {
    Report::new(0x00, 0x82, 0x16)
}
pub fn get_battery() -> Report {
    Report::new(0x07, 0x80, 0x02)
}
pub fn get_charging() -> Report {
    Report::new(0x07, 0x84, 0x02)
}
pub fn get_dpi() -> Report {
    Report::new(0x04, 0x85, 0x07).with_args(&[VARSTORE])
}
pub fn set_dpi(x: u16, y: u16) -> Report {
    let x = x.clamp(100, 45000);
    let y = y.clamp(100, 45000);
    Report::new(0x04, 0x05, 0x07).with_args(&[VARSTORE, (x >> 8) as u8, x as u8, (y >> 8) as u8, y as u8, 0, 0])
}
pub fn get_dpi_byte() -> Report {
    Report::new(0x04, 0x81, 0x03)
}
pub fn set_dpi_byte(x: u8, y: u8) -> Report {
    Report::new(0x04, 0x01, 0x03).with_args(&[x, y, 0])
}
pub fn get_dpi_stages() -> Report {
    Report::new(0x04, 0x86, 0x26).with_args(&[VARSTORE])
}
/// `active` is 1-based, as on the wire.
pub fn set_dpi_stages(stages: &[(u16, u16)], active: u8) -> Report {
    let mut r = Report::new(0x04, 0x06, 0x26);
    r.args[0] = VARSTORE;
    r.args[1] = active;
    r.args[2] = stages.len() as u8;
    let mut off = 3;
    for (i, (x, y)) in stages.iter().enumerate().take(5) {
        r.args[off] = i as u8;
        r.args[off + 1..off + 3].copy_from_slice(&x.to_be_bytes());
        r.args[off + 3..off + 5].copy_from_slice(&y.to_be_bytes());
        off += 7;
    }
    r
}
pub fn get_polling() -> Report {
    Report::new(0x00, 0x85, 0x01)
}
pub fn set_polling(hz: u16) -> Report {
    let code = match hz {
        1000 => 0x01,
        125 => 0x08,
        _ => 0x02,
    };
    Report::new(0x00, 0x05, 0x01).with_args(&[code])
}
pub fn get_polling2() -> Report {
    Report::new(0x00, 0xC0, 0x01)
}
pub fn polling2_code(hz: u16) -> u8 {
    match hz {
        8000 => 0x01,
        4000 => 0x02,
        2000 => 0x04,
        1000 => 0x08,
        250 => 0x20,
        125 => 0x40,
        _ => 0x10,
    }
}
pub fn polling2_hz(code: u8) -> u16 {
    match code {
        0x01 => 8000,
        0x02 => 4000,
        0x04 => 2000,
        0x08 => 1000,
        0x20 => 250,
        0x40 => 125,
        _ => 500,
    }
}
pub fn set_polling2(hz: u16, arg: u8) -> Report {
    Report::new(0x00, 0x40, 0x02).with_args(&[arg, polling2_code(hz)])
}
pub fn polling_hz(code: u8) -> u16 {
    match code {
        0x01 => 1000,
        0x08 => 125,
        _ => 500,
    }
}
pub fn get_idle() -> Report {
    Report::new(0x07, 0x83, 0x02)
}
pub fn set_idle(seconds: u16) -> Report {
    let s = seconds.clamp(60, 900);
    Report::new(0x07, 0x03, 0x02).with_args(&[(s >> 8) as u8, s as u8])
}
pub fn get_low_battery() -> Report {
    Report::new(0x07, 0x81, 0x01)
}
pub fn set_low_battery(raw: u8) -> Report {
    Report::new(0x07, 0x01, 0x01).with_args(&[raw.clamp(0x0C, 0x3F)])
}

// Extended matrix (class 0x0F)
pub fn ext_effect(led: u8, effect: u8, size: u8) -> Report {
    Report::new(0x0F, 0x02, size).with_args(&[VARSTORE, led, effect])
}
pub fn ext_brightness(led: u8, value: u8) -> Report {
    Report::new(0x0F, 0x04, 0x03).with_args(&[VARSTORE, led, value])
}
pub fn ext_get_brightness(led: u8) -> Report {
    Report::new(0x0F, 0x84, 0x03).with_args(&[VARSTORE, led])
}

// Standard LED (class 0x03)
pub fn std_led_state(led: u8, on: bool) -> Report {
    Report::new(0x03, 0x00, 0x03).with_args(&[VARSTORE, led, on as u8])
}
pub fn std_led_rgb(led: u8, r: u8, g: u8, b: u8) -> Report {
    Report::new(0x03, 0x01, 0x05).with_args(&[VARSTORE, led, r, g, b])
}
pub fn std_led_effect(led: u8, effect: u8) -> Report {
    Report::new(0x03, 0x02, 0x03).with_args(&[VARSTORE, led, effect.min(5)])
}
pub fn std_brightness(led: u8, value: u8) -> Report {
    Report::new(0x03, 0x03, 0x03).with_args(&[VARSTORE, led, value])
}
/// Older keyboards and mats: class 0x03 id 0x0A, the effect id first.
pub fn std_matrix_effect(args: &[u8]) -> Report {
    Report::new(0x03, 0x0A, args.len() as u8).with_args(args)
}

pub fn std_get_brightness(led: u8) -> Report {
    Report::new(0x03, 0x83, 0x03).with_args(&[VARSTORE, led])
}

// "Mouse extended" (class 0x03 id 0x0D)
pub fn mext_effect(led: u8, effect: u8, size: u8) -> Report {
    Report::new(0x03, 0x0D, size).with_args(&[VARSTORE, led, effect])
}

pub fn find_model(pid: u16) -> Option<&'static table::Model> {
    devices::MODELS.iter().find(|m| m.pid == pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_and_roundtrip() {
        let mut r = set_dpi(1600, 1600);
        r.tid = 0x1F;
        let b = r.encode();
        assert_eq!(b[1], 0x1F);
        assert_eq!(&b[5..8], &[0x07, 0x04, 0x05]);
        assert_eq!(&b[8..13], &[0x01, 0x06, 0x40, 0x06, 0x40]);
        let x = b[2..88].iter().fold(0, |a, v| a ^ v);
        assert_eq!(b[88], x);
        assert_eq!(Report::decode(&b).unwrap(), r);
    }

    #[test]
    fn stages_layout() {
        let r = set_dpi_stages(&[(400, 400), (1600, 1600)], 2);
        assert_eq!(&r.args[..3], &[VARSTORE, 2, 2]);
        assert_eq!(&r.args[3..10], &[0, 0x01, 0x90, 0x01, 0x90, 0, 0]);
        assert_eq!(&r.args[10..15], &[1, 0x06, 0x40, 0x06, 0x40]);
    }

    #[test]
    fn table_sanity() {
        assert!(devices::MODELS.len() > 100);
        let da = find_model(0x00b7).unwrap();
        assert!(da.has(table::BATTERY) && da.has(table::DPI_STAGES));
        assert_eq!(da.tid, 0x1F);
        let b3 = find_model(0x0099).unwrap();
        assert_eq!(b3.iface, 3);
        assert_eq!(b3.led, table::LedProto::Extended);
        for m in devices::MODELS {
            assert!(!m.name.is_empty());
            assert!(m.dpi_max >= 1000, "{}", m.name);
        }
    }
}
