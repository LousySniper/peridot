//! HID++ 2.0 feature identifiers and per-device feature table discovery.

use super::{be16, Channel, Error, Result};
use std::time::Duration;

pub const ROOT: u16 = 0x0000;
pub const FEATURE_SET: u16 = 0x0001;
pub const DEVICE_FW_VERSION: u16 = 0x0003;
pub const DEVICE_NAME: u16 = 0x0005;
pub const BATTERY_STATUS: u16 = 0x1000;
pub const BATTERY_VOLTAGE: u16 = 0x1001;
pub const UNIFIED_BATTERY: u16 = 0x1004;
pub const REPROG_CONTROLS_V4: u16 = 0x1B04;
pub const ADC_MEASUREMENT: u16 = 0x1F20;
pub const ADJUSTABLE_DPI: u16 = 0x2201;
pub const EXTENDED_ADJUSTABLE_DPI: u16 = 0x2202;
pub const REPORT_RATE: u16 = 0x8060;
pub const EXTENDED_ADJUSTABLE_REPORT_RATE: u16 = 0x8061;
pub const BRIGHTNESS_CONTROL: u16 = 0x8040;
pub const COLOR_LED_EFFECTS: u16 = 0x8070;
pub const RGB_EFFECTS: u16 = 0x8071;
pub const ONBOARD_PROFILES: u16 = 0x8100;
pub const SIDETONE: u16 = 0x8300;
pub const EQUALIZER: u16 = 0x8310;

pub fn feature_name(id: u16) -> &'static str {
    match id {
        ROOT => "ROOT",
        FEATURE_SET => "FEATURE_SET",
        0x0002 => "FEATURE_INFO",
        DEVICE_FW_VERSION => "DEVICE_FW_VERSION",
        0x0004 => "DEVICE_UNIT_ID",
        DEVICE_NAME => "DEVICE_NAME",
        0x0007 => "DEVICE_FRIENDLY_NAME",
        0x0008 => "KEEP_ALIVE",
        0x0020 => "CONFIG_CHANGE",
        0x0021 => "CRYPTO_ID",
        0x00C2 => "DFUCONTROL_SIGNED",
        0x00D0 => "DFU",
        BATTERY_STATUS => "BATTERY_STATUS",
        BATTERY_VOLTAGE => "BATTERY_VOLTAGE",
        UNIFIED_BATTERY => "UNIFIED_BATTERY",
        0x1300 => "LED_CONTROL",
        0x1802 => "DEVICE_RESET",
        0x1814 => "CHANGE_HOST",
        0x1815 => "HOSTS_INFO",
        0x1B00 => "REPROG_CONTROLS",
        REPROG_CONTROLS_V4 => "REPROG_CONTROLS_V4",
        0x1D4B => "WIRELESS_DEVICE_STATUS",
        ADC_MEASUREMENT => "ADC_MEASUREMENT",
        0x2100 => "VERTICAL_SCROLLING",
        0x2110 => "SMART_SHIFT",
        0x2111 => "SMART_SHIFT_ENHANCED",
        0x2121 => "HIRES_WHEEL",
        0x2150 => "THUMB_WHEEL",
        0x2200 => "MOUSE_POINTER",
        ADJUSTABLE_DPI => "ADJUSTABLE_DPI",
        EXTENDED_ADJUSTABLE_DPI => "EXTENDED_ADJUSTABLE_DPI",
        0x2205 => "POINTER_SPEED",
        0x2250 => "XY_STATS",
        0x40A3 => "FN_INVERSION",
        0x4521 => "DISABLE_KEYS",
        REPORT_RATE => "REPORT_RATE",
        EXTENDED_ADJUSTABLE_REPORT_RATE => "EXTENDED_ADJUSTABLE_REPORT_RATE",
        BRIGHTNESS_CONTROL => "BRIGHTNESS_CONTROL",
        COLOR_LED_EFFECTS => "COLOR_LED_EFFECTS",
        RGB_EFFECTS => "RGB_EFFECTS",
        0x8080 => "PER_KEY_LIGHTING",
        0x8081 => "PER_KEY_LIGHTING_V2",
        0x8090 => "MODE_STATUS",
        ONBOARD_PROFILES => "ONBOARD_PROFILES",
        0x8110 => "MOUSE_BUTTON_SPY",
        0x8111 => "LATENCY_MONITORING",
        SIDETONE => "SIDETONE",
        EQUALIZER => "EQUALIZER",
        0x8320 => "HEADSET_OUT",
        _ => "UNKNOWN",
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FeatureEntry {
    pub id: u16,
    pub index: u8,
    pub version: u8,
    pub flags: u8,
}

/// The device's feature-id -> feature-index map.
#[derive(Clone, Debug, Default)]
pub struct Features {
    pub entries: Vec<FeatureEntry>,
}

impl Features {
    pub fn index(&self, id: u16) -> Option<u8> {
        self.entries.iter().find(|e| e.id == id).map(|e| e.index)
    }

    pub fn version(&self, id: u16) -> Option<u8> {
        self.entries.iter().find(|e| e.id == id).map(|e| e.version)
    }

    pub fn has(&self, id: u16) -> bool {
        self.index(id).is_some()
    }

    pub fn id_at(&self, index: u8) -> Option<u16> {
        self.entries.iter().find(|e| e.index == index).map(|e| e.id)
    }

    pub fn require(&self, id: u16) -> Result<u8> {
        self.index(id).ok_or(Error::NoFeature(id))
    }

    /// Walk the FEATURE_SET feature to build the full table.
    pub fn discover(ch: &Channel, device: u8) -> Result<Features> {
        let mut entries = vec![FeatureEntry { id: ROOT, index: 0, version: 0, flags: 0 }];
        let reply = ch.feature_request(device, 0, 0, &[0x00, 0x01])?;
        let fs_index = reply.first().copied().unwrap_or(0);
        if fs_index == 0 {
            return Ok(Features { entries });
        }
        let count = ch.feature_request(device, fs_index, 0, &[])?.first().copied().unwrap_or(0);
        for i in 1..=count {
            let r = ch.feature_request(device, fs_index, 1, &[i])?;
            entries.push(FeatureEntry {
                id: be16(&r, 0),
                index: i,
                flags: r.get(2).copied().unwrap_or(0),
                version: r.get(3).copied().unwrap_or(0),
            });
        }
        Ok(Features { entries })
    }
}

/// Ask for the HID++ protocol version with a ping. Returns (major, minor).
/// HID++ 1.0 devices answer with an "invalid sub-id" error.
pub fn protocol_version(ch: &Channel, device: u8, timeout: Duration) -> Result<(u8, u8)> {
    match ch.feature_request_timeout(device, 0, 1, &[0, 0, 0x5A], timeout) {
        Ok(r) => Ok((r.first().copied().unwrap_or(0), r.get(1).copied().unwrap_or(0))),
        Err(Error::V10(0x01)) => Ok((1, 0)),
        Err(e) => Err(e),
    }
}
