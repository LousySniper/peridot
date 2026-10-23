//! Logitech HID++ 1.0 / 2.0 protocol.
//!
//! Framing: `[report id, device index, feature index | sub id, function<<4 | swid
//! | register, params...]`. Short reports (0x10) are 7 bytes, long (0x11) 20.

pub mod channel;
pub mod device;
pub mod features;
pub mod receiver;

pub use channel::Channel;

pub const REPORT_SHORT: u8 = 0x10;
pub const REPORT_LONG: u8 = 0x11;
pub const REPORT_VERY_LONG: u8 = 0x12;
pub const SHORT_LEN: usize = 7;
pub const LONG_LEN: usize = 20;
pub const VERY_LONG_LEN: usize = 64;

/// Device index used for the receiver itself or a directly connected device.
pub const DEV_DIRECT: u8 = 0xFF;

// HID++ 1.0 sub ids
pub const SET_REGISTER: u8 = 0x80;
pub const GET_REGISTER: u8 = 0x81;
pub const SET_LONG_REGISTER: u8 = 0x82;
pub const GET_LONG_REGISTER: u8 = 0x83;
pub const ERROR_10: u8 = 0x8F;
pub const ERROR_20: u8 = 0xFF;
pub const NOTIF_DEVICE_DISCONNECTED: u8 = 0x40;
pub const NOTIF_DEVICE_CONNECTED: u8 = 0x41;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub report: u8,
    pub device: u8,
    /// Feature index (HID++ 2.0) or sub id (HID++ 1.0).
    pub sub: u8,
    /// `function << 4 | software id` (2.0) or register address (1.0).
    pub address: u8,
    pub params: Vec<u8>,
}

impl Message {
    /// A HID++ 1.0 notification's data: the byte in the address slot, then the params.
    pub fn params_with_address(&self) -> Vec<u8> {
        let mut v = vec![self.address];
        v.extend_from_slice(&self.params);
        v
    }

    pub fn encode(&self) -> Vec<u8> {
        let len = match self.report {
            REPORT_SHORT => SHORT_LEN,
            REPORT_LONG => LONG_LEN,
            _ => VERY_LONG_LEN,
        };
        let mut out = vec![0u8; len];
        out[0] = self.report;
        out[1] = self.device;
        out[2] = self.sub;
        out[3] = self.address;
        let n = self.params.len().min(len - 4);
        out[4..4 + n].copy_from_slice(&self.params[..n]);
        out
    }

    pub fn decode(data: &[u8]) -> Option<Message> {
        if data.len() < 4 {
            return None;
        }
        let expected = match data[0] {
            REPORT_SHORT => SHORT_LEN,
            REPORT_LONG => LONG_LEN,
            REPORT_VERY_LONG => VERY_LONG_LEN,
            _ => return None,
        };
        let end = expected.min(data.len());
        Some(Message { report: data[0], device: data[1], sub: data[2], address: data[3], params: data[4..end].to_vec() })
    }

    pub fn function(&self) -> u8 {
        self.address >> 4
    }

    pub fn swid(&self) -> u8 {
        self.address & 0x0F
    }

    pub fn param(&self, i: usize) -> u8 {
        self.params.get(i).copied().unwrap_or(0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Timeout,
    Closed,
    Io(String),
    /// HID++ 1.0 error code (receivers, registers, offline devices).
    V10(u8),
    /// HID++ 2.0 error code.
    V20(u8),
    /// The device doesn't have the feature we need.
    NoFeature(u16),
    BadReply(String),
}

impl Error {
    /// True when the device is paired but currently unreachable (asleep/off).
    pub fn is_offline(&self) -> bool {
        matches!(self, Error::V10(0x09) | Error::V10(0x08) | Error::V10(0x04) | Error::Timeout)
    }
}

pub fn error_name_10(code: u8) -> &'static str {
    match code {
        0x01 => "invalid sub-id",
        0x02 => "invalid address",
        0x03 => "invalid value",
        0x04 => "connection failed",
        0x05 => "too many devices",
        0x06 => "already exists",
        0x07 => "busy",
        0x08 => "unknown device",
        0x09 => "device offline",
        0x0A => "request unavailable",
        0x0B => "invalid parameter",
        0x0C => "wrong PIN",
        _ => "unknown error",
    }
}

pub fn error_name_20(code: u8) -> &'static str {
    match code {
        0x01 => "unknown",
        0x02 => "invalid argument",
        0x03 => "out of range",
        0x04 => "hardware error",
        0x05 => "internal error",
        0x06 => "invalid feature index",
        0x07 => "invalid function",
        0x08 => "busy",
        0x09 => "unsupported",
        _ => "unknown error",
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Timeout => write!(f, "no reply from device"),
            Error::Closed => write!(f, "device connection closed"),
            Error::Io(e) => write!(f, "I/O error: {}", e),
            Error::V10(c) => write!(f, "HID++ 1.0 error 0x{:02x} ({})", c, error_name_10(*c)),
            Error::V20(c) => write!(f, "HID++ 2.0 error 0x{:02x} ({})", c, error_name_20(*c)),
            Error::NoFeature(id) => write!(f, "feature 0x{:04x} not supported", id),
            Error::BadReply(s) => write!(f, "malformed reply: {}", s),
        }
    }
}

impl From<Error> for crate::model::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::Timeout | Error::V10(0x09) | Error::V10(0x08) => crate::model::Error::Offline,
            Error::NoFeature(_) => crate::model::Error::NotSupported(e.to_string()),
            Error::V20(0x02) | Error::V20(0x03) => crate::model::Error::Invalid(e.to_string()),
            other => crate::model::Error::Device(other.to_string()),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn be16(b: &[u8], i: usize) -> u16 {
    ((b.get(i).copied().unwrap_or(0) as u16) << 8) | b.get(i + 1).copied().unwrap_or(0) as u16
}

/// Logitech USB receivers (device indices 1..=6 behind one hidraw node).
pub fn is_receiver(pid: u16) -> bool {
    matches!(
        pid,
        0xC517 | 0xC518 | 0xC51A | 0xC51B | 0xC521 | 0xC525 | 0xC526 | 0xC52B | 0xC52E | 0xC52F | 0xC531 | 0xC532
            | 0xC534 | 0xC537 | 0xC539 | 0xC53A | 0xC53D | 0xC53F | 0xC541 | 0xC545 | 0xC547 | 0xC548 | 0xC54D
            | 0xC54E | 0xC54F | 0xC550
    )
}

pub fn receiver_name(pid: u16) -> &'static str {
    match pid {
        0xC52B | 0xC532 => "Unifying Receiver",
        0xC548 => "Bolt Receiver",
        0xC539 | 0xC53A | 0xC53D | 0xC53F | 0xC541 | 0xC545 | 0xC547 | 0xC54D | 0xC54E | 0xC54F | 0xC550 => {
            "Lightspeed Receiver"
        }
        _ => "Nano Receiver",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode() {
        let m = Message { report: REPORT_SHORT, device: 1, sub: 0x00, address: 0x1A, params: vec![0, 0, 0x5A] };
        let raw = m.encode();
        assert_eq!(raw, vec![0x10, 0x01, 0x00, 0x1A, 0x00, 0x00, 0x5A]);
        let back = Message::decode(&raw).unwrap();
        assert_eq!(back.params, vec![0, 0, 0x5A]);
        assert_eq!(back.function(), 1);
        assert_eq!(back.swid(), 0xA);
        let long = Message { report: REPORT_LONG, device: 0xFF, sub: 3, address: 0x10, params: vec![1; 30] }.encode();
        assert_eq!(long.len(), LONG_LEN);
        assert!(Message::decode(&[0x20, 0, 0, 0]).is_none());
    }
}
