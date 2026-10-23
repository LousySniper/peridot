//! One HID++ 2.0 device (a mouse or headset behind a receiver, or connected
//! directly over USB).

use super::features::{self as f, Features};
use super::{be16, Channel, Error as HErr, Message};
use crate::actions::Action;
use crate::json::Json;
use crate::model::{
    self, invalid, Battery, Charge, Choice, Connection, Control, DeviceState, Direction, Dpi, Effect, Equalizer, Kind,
    Lighting, Polling, Range, Rgb, Zone,
};
use std::sync::Arc;

const MAX_SOFT_STAGES: usize = 5;
const DEFAULT_STAGES: [u16; 4] = [400, 800, 1600, 3200];

#[derive(Clone, Copy, Debug, PartialEq)]
enum DpiKind {
    None,
    Basic,
    Extended { y: bool },
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum RateKind {
    None,
    Basic,
    Extended,
}

#[derive(Clone, Debug)]
struct LedZone {
    index: u8,
    id: String,
    /// (effect index within the zone, effect id)
    effects: Vec<(u8, u16)>,
}

#[derive(Clone, Debug)]
struct Reprog {
    cid: u16,
    diverted: bool,
}

/// A button press or release from a control Peridot diverted.
#[derive(Clone, Debug, PartialEq)]
pub struct ButtonEvent {
    pub control: String,
    pub pressed: bool,
}

pub struct LogitechDevice {
    ch: Arc<Channel>,
    pub index: u8,
    features: Features,
    pub state: DeviceState,
    dpi_kind: DpiKind,
    rate_kind: RateKind,
    led_feature: Option<u16>,
    led_zones: Vec<LedZone>,
    /// Lighting brightness in percent, applied by the device when it has
    /// BRIGHTNESS_CONTROL, otherwise by scaling the effect colours.
    led_brightness: u8,
    /// BRIGHTNESS_CONTROL range: (min, max, has on/off switch).
    hw_brightness: Option<(u16, u16, bool)>,
    led_claimed: bool,
    reprog: Vec<Reprog>,
    pressed: Vec<u16>,
    shift_restore: Option<u16>,
    host_mode_forced: bool,
    /// Whether the headset accepts equalizer writes; None until probed.
    /// Some firmware (e.g. G733 with EQUALIZER v0/v1) reports its EQ but rejects every write;
    /// Peridot then applies the EQ in software (see `softeq`).
    eq_writable: Option<bool>,
    /// The last full read was cut short because the device didn't answer
    /// (e.g. a headset that was off while its dongle answered); read again
    /// once it does.
    pub incomplete: bool,
    /// Known only from the receiver's pairing table; never probed yet.
    pub placeholder: bool,
    /// HID++ 1.0 device: only the battery, read from registers.
    pub legacy: bool,
}

pub fn control_name(cid: u16) -> String {
    let n = match cid {
        0x0050 => "Left click",
        0x0051 => "Right click",
        0x0052 => "Middle click",
        0x0053 => "Back",
        0x0056 => "Forward",
        0x0059 => "Button 6",
        0x005B => "Scroll left",
        0x005D => "Scroll right",
        0x005E => "Button 9",
        0x005F => "Button 10",
        0x00C3 => "Gesture button",
        0x00C4 => "Smart Shift",
        0x00D7 => "Gesture button",
        0x00ED => "DPI button",
        0x00FD => "DPI switch",
        0x0054 => "Back (HID)",
        0x0057 => "Forward (HID)",
        0x00A9 => "Thumb button",
        0x01A0 => "Haptic sense",
        _ => return format!("Control {:04X}", cid),
    };
    n.to_string()
}

/// Band layout of the 10-band EQ in Logitech G headsets (G733, G PRO X...).
const G_SERIES_BANDS: [u32; 10] = [32, 64, 125, 250, 500, 1000, 2000, 4000, 8000, 16000];

/// Presets for the 10-band G-series layout, named like Razer's. Logitech
/// doesn't publish G HUB's curves, so these are Peridot's own.
fn g_series_presets(freqs: &[u32]) -> Vec<(String, String, Vec<i32>)> {
    if freqs != G_SERIES_BANDS {
        return Vec::new();
    }
    let p = |id: &str, name: &str, g: [i32; 10]| (id.to_string(), name.to_string(), g.to_vec());
    vec![
        p("default", "Default", [0; 10]),
        // Less rumble, more of the 2-8 kHz range where footsteps and reloads sit.
        p("game", "Game", [-3, -2, -1, 0, 0, 1, 3, 4, 3, 1]),
        p("music", "Music", [4, 3, 1, 0, -1, -1, 0, 1, 3, 3]),
        p("movie", "Movie", [5, 4, 2, 0, -1, 0, 1, 2, 3, 3]),
    ]
}

fn zone_name(location: u16) -> (String, String) {
    let (id, name) = match location {
        1 => ("primary", "Primary"),
        2 => ("logo", "Logo"),
        3 => ("left", "Left side"),
        4 => ("right", "Right side"),
        5 => ("combined", "Combined"),
        6..=11 => return (format!("primary{}", location - 5), format!("Primary {}", location - 5)),
        _ => return (format!("zone{}", location), format!("Zone {}", location)),
    };
    (id.to_string(), name.to_string())
}

/// Map a Logitech effect id to our vocabulary.
fn effect_of(id: u16) -> Option<Effect> {
    Some(match id {
        0x00 => Effect::Off,
        0x01 => Effect::Static,
        0x02 | 0x0A => Effect::Breathing,
        0x03 | 0x15 => Effect::Spectrum,
        0x04 | 0x16 => Effect::Wave,
        0x0B | 0x17 => Effect::Ripple,
        _ => return None,
    })
}

/// Encode the 10 effect parameter bytes for an effect id.
pub fn effect_params(id: u16, l: &Lighting) -> [u8; 10] {
    let mut p = [0u8; 10];
    let s = l.speed.min(100) as u32;
    // period in ms: speed 0 -> 10 s, speed 100 -> 1 s
    let period = (10000 - s * 90) as u16;
    let color = |p: &mut [u8; 10], at: usize| {
        p[at] = l.color.0;
        p[at + 1] = l.color.1;
        p[at + 2] = l.color.2;
    };
    match id {
        0x01 => color(&mut p, 0),
        0x02 => {
            color(&mut p, 0);
            p[3] = (s * 255 / 100) as u8;
        }
        0x03 => {
            p[5..7].copy_from_slice(&period.to_be_bytes());
            p[7] = 100;
        }
        0x04 => {
            p[6..8].copy_from_slice(&period.to_be_bytes());
            p[9] = if l.direction == Direction::Left { 6 } else { 1 };
        }
        0x0A => {
            color(&mut p, 0);
            p[3..5].copy_from_slice(&period.to_be_bytes());
            p[6] = 100;
        }
        0x0B => {
            color(&mut p, 0);
            let rp = (200 - s * 198 / 100) as u16;
            p[4..6].copy_from_slice(&rp.to_be_bytes());
        }
        0x15 => {
            p[1] = 255;
            p[6..8].copy_from_slice(&period.to_be_bytes());
            p[8] = 100;
        }
        0x16 => {
            p[1] = 255;
            p[6..8].copy_from_slice(&period.to_be_bytes());
            p[8] = 100;
            p[9] = if l.direction == Direction::Left { 6 } else { 1 };
        }
        0x17 => {
            color(&mut p, 0);
            p[3] = 255;
            let rp = (200 - s * 198 / 100) as u16;
            p[6..8].copy_from_slice(&rp.to_be_bytes());
        }
        _ => {}
    }
    p
}

/// `effect_params` with colours and intensity scaled to `pct` percent, for
/// devices without a brightness control of their own.
pub fn scaled_effect_params(id: u16, l: &Lighting, pct: u8) -> [u8; 10] {
    let k = pct.min(100) as u32;
    let sc = |v: u8| ((v as u32 * k + 50) / 100) as u8;
    let mut dim = l.clone();
    dim.color = Rgb(sc(l.color.0), sc(l.color.1), sc(l.color.2));
    let mut p = effect_params(id, &dim);
    // Intensity bytes (percent) of the colour-cycle and breathing effects.
    let at = match id {
        0x03 => Some(7),
        0x0A => Some(6),
        0x15 | 0x16 => Some(8),
        _ => None,
    };
    if let Some(i) = at {
        p[i] = (p[i] as u32 * k / 100).max(if k > 0 { 1 } else { 0 }) as u8;
    }
    p
}

/// Parse a HID++ DPI list (2-byte big-endian values, `0xE000 | step` markers
/// meaning "every `step` up to the next value", terminated by 0).
pub fn parse_dpi_list(bytes: &[u8]) -> Vec<u16> {
    let mut out: Vec<u16> = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        let v = be16(bytes, i);
        if v == 0 {
            break;
        }
        if v >> 13 == 0b111 {
            let step = v & 0x1FFF;
            let last = be16(bytes, i + 2);
            if let Some(&prev) = out.last() {
                if step > 0 && last > prev {
                    let mut x = prev as u32 + step as u32;
                    while x <= last as u32 {
                        out.push(x as u16);
                        x += step as u32;
                    }
                }
            }
            i += 4;
        } else {
            out.push(v);
            i += 2;
        }
    }
    out
}

/// Fill min/max/step (when the list is an arithmetic progression) or list.
fn dpi_from_values(values: &[u16]) -> Dpi {
    let mut d = Dpi { min: *values.first().unwrap_or(&0), max: *values.last().unwrap_or(&0), ..Default::default() };
    if values.len() >= 2 {
        let step = values[1] - values[0];
        if step > 0 && values.windows(2).all(|w| w[1].wrapping_sub(w[0]) == step) {
            d.step = step;
            return d;
        }
    }
    d.list = values.to_vec();
    d
}

pub fn decode_unified_battery(r: &[u8]) -> Battery {
    let soc = r.first().copied().unwrap_or(0);
    let level = r.get(1).copied().unwrap_or(0);
    let status = r.get(2).copied().unwrap_or(0);
    let (percent, estimated) = if soc > 0 {
        (Some(soc.min(100)), false)
    } else {
        let p = match level {
            l if l & 8 != 0 => Some(90),
            l if l & 4 != 0 => Some(50),
            l if l & 2 != 0 => Some(20),
            l if l & 1 != 0 => Some(5),
            _ => None,
        };
        (p, true)
    };
    let charge = match status {
        0 => Charge::Discharging,
        1 | 2 => Charge::Charging,
        3 => Charge::Full,
        _ => Charge::Error,
    };
    Battery { percent, charge, voltage_mv: None, estimated }
}

pub fn decode_battery_status(r: &[u8]) -> Battery {
    let level = r.first().copied().unwrap_or(0);
    let status = r.get(2).copied().unwrap_or(0);
    let charge = match status {
        0 => Charge::Discharging,
        1 | 2 | 4 => Charge::Charging,
        3 => Charge::Full,
        _ => Charge::Error,
    };
    let percent = if level > 0 { Some(level.min(100)) } else if charge == Charge::Full { Some(100) } else { None };
    Battery { percent, charge, voltage_mv: None, estimated: false }
}

/// HID++ 1.0 register 0x0D: `percent, ?, status` (status 0x30 discharging,
/// 0x50 charging, 0x90 full), as Solaar reads it.
pub fn decode_battery_charge_register(r: &[u8]) -> Option<Battery> {
    let percent = *r.first()?;
    let charge = match r.get(2).map(|b| b & 0xF0) {
        Some(0x50) => Charge::Charging,
        Some(0x90) => Charge::Full,
        _ => Charge::Discharging,
    };
    Some(Battery { percent: Some(percent.min(100)), charge, voltage_mv: None, estimated: false })
}

/// HID++ 1.0 register 0x07: a coarse level (7 full, 5 good, 3 low, 1
/// critical) and a charging byte, mapped to Solaar's approximate percentages.
pub fn decode_battery_status_register(r: &[u8]) -> Option<Battery> {
    let (level, charging) = (*r.first()?, r.get(1).copied().unwrap_or(0));
    let charge = if charging == 0 {
        Charge::Discharging
    } else if charging & 0x21 == 0x21 {
        Charge::Charging
    } else if charging & 0x22 == 0x22 {
        Charge::Full
    } else {
        Charge::Discharging
    };
    let percent = match level {
        7 => Some(90),
        5 => Some(50),
        3 => Some(20),
        1 => Some(5),
        0 if charging & 0x03 != 0 => None, // a charging notice without a level
        _ => Some(0),
    };
    Some(Battery { percent, charge, voltage_mv: None, estimated: true })
}

pub fn decode_battery_voltage(r: &[u8], headset: bool) -> Battery {
    let mv = be16(r, 0);
    let flags = r.get(2).copied().unwrap_or(0);
    let charge = if flags & 0x80 != 0 {
        match flags & 0x03 {
            0 => Charge::Charging,
            1 => Charge::Full,
            2 => Charge::Discharging,
            _ => Charge::Error,
        }
    } else {
        Charge::Discharging
    };
    Battery { percent: Some(crate::battery::percent_from_voltage(mv, headset)), charge, voltage_mv: Some(mv), estimated: true }
}

pub fn decode_adc(r: &[u8], headset: bool) -> Option<Battery> {
    let mv = be16(r, 0);
    let flags = r.get(2).copied().unwrap_or(0);
    if flags & 0x01 == 0 || mv == 0 {
        return None;
    }
    let charge = if flags & 0x02 != 0 { Charge::Charging } else { Charge::Discharging };
    Some(Battery {
        percent: Some(crate::battery::percent_from_voltage(mv, headset)),
        charge,
        voltage_mv: Some(mv),
        estimated: true,
    })
}

impl LogitechDevice {
    /// Discover features and read the full state. `model` is "vid:pid" of the
    /// USB node plus the wireless product id when behind a receiver.
    pub fn probe(
        ch: Arc<Channel>,
        index: u8,
        connection: Connection,
        model: String,
        via: Option<String>,
        protocol: (u8, u8),
    ) -> Result<LogitechDevice, HErr> {
        let features = Features::discover(&ch, index)?;
        let mut dev = LogitechDevice {
            ch,
            index,
            features,
            state: DeviceState {
                vendor: "logitech",
                connection,
                online: true,
                model,
                via,
                protocol: Some(format!("HID++ {}.{}", protocol.0, protocol.1)),
                ..Default::default()
            },
            dpi_kind: DpiKind::None,
            rate_kind: RateKind::None,
            led_feature: None,
            led_zones: Vec::new(),
            led_brightness: 100,
            hw_brightness: None,
            led_claimed: false,
            reprog: Vec::new(),
            pressed: Vec::new(),
            shift_restore: None,
            host_mode_forced: false,
            eq_writable: None,
            incomplete: false,
            placeholder: false,
            legacy: false,
        };
        dev.state.name = dev.read_name().unwrap_or_else(|_| "Logitech device".into());
        let (fw, unit) = dev.read_firmware().unwrap_or((None, None));
        dev.state.firmware = fw;
        dev.state.serial = unit.clone();
        dev.state.id = match unit {
            Some(u) => format!("logitech-{}", u.to_ascii_lowercase()),
            None => format!("logitech-{}-{}", dev.state.model.replace(':', ""), index),
        };
        dev.state.kind = dev.read_kind();
        dev.read_all();
        Ok(dev)
    }

    /// A paired device that is asleep: we only know what the receiver told us.
    pub fn placeholder(
        ch: Arc<Channel>,
        index: u8,
        name: String,
        model: String,
        via: Option<String>,
        serial: Option<String>,
    ) -> LogitechDevice {
        let id = match &serial {
            Some(s) => format!("logitech-{}", s.to_ascii_lowercase()),
            None => format!("logitech-{}-{}", model.replace(':', ""), index),
        };
        LogitechDevice {
            ch,
            index,
            features: Features::default(),
            state: DeviceState {
                id,
                name,
                vendor: "logitech",
                kind: Kind::Mouse,
                connection: Connection::Receiver,
                online: false,
                model,
                via,
                serial,
                ..Default::default()
            },
            dpi_kind: DpiKind::None,
            rate_kind: RateKind::None,
            led_feature: None,
            led_zones: Vec::new(),
            led_brightness: 100,
            hw_brightness: None,
            led_claimed: false,
            reprog: Vec::new(),
            pressed: Vec::new(),
            shift_restore: None,
            host_mode_forced: false,
            eq_writable: None,
            incomplete: false,
            placeholder: true,
            legacy: false,
        }
    }

    fn req(&self, feature: u16, function: u8, params: &[u8]) -> Result<Vec<u8>, HErr> {
        let idx = self.features.require(feature)?;
        self.ch.feature_request(self.index, idx, function, params)
    }

    pub fn features(&self) -> &Features {
        &self.features
    }

    fn read_name(&self) -> Result<String, HErr> {
        let len = self.req(f::DEVICE_NAME, 0, &[])?.first().copied().unwrap_or(0) as usize;
        let mut name = Vec::new();
        while name.len() < len {
            let chunk = self.req(f::DEVICE_NAME, 1, &[name.len() as u8])?;
            let take = (len - name.len()).min(chunk.len());
            if take == 0 {
                break;
            }
            name.extend_from_slice(&chunk[..take]);
        }
        let s = String::from_utf8_lossy(&name).trim_end_matches('\0').trim().to_string();
        Ok(s)
    }

    fn read_kind(&self) -> Kind {
        let has = |id| self.features.has(id);
        if has(f::SIDETONE) || has(f::EQUALIZER) {
            return Kind::Headset;
        }
        if let Ok(r) = self.req(f::DEVICE_NAME, 2, &[]) {
            match r.first() {
                Some(0) | Some(2) => return Kind::Keyboard,
                Some(3) | Some(4) | Some(5) => return Kind::Mouse,
                Some(8) => return Kind::Headset,
                _ => {}
            }
        }
        if has(f::ADJUSTABLE_DPI) || has(f::EXTENDED_ADJUSTABLE_DPI) {
            Kind::Mouse
        } else if has(f::ADC_MEASUREMENT) {
            Kind::Headset
        } else {
            Kind::Other
        }
    }

    fn read_firmware(&self) -> Result<(Option<String>, Option<String>), HErr> {
        let r = self.req(f::DEVICE_FW_VERSION, 0, &[])?;
        let count = r.first().copied().unwrap_or(0);
        let unit = if r.len() >= 5 && r[1..5].iter().any(|b| *b != 0) {
            Some(r[1..5].iter().map(|b| format!("{:02X}", b)).collect::<String>())
        } else {
            None
        };
        let mut fw = None;
        for e in 0..count.min(4) {
            let i = self.req(f::DEVICE_FW_VERSION, 1, &[e])?;
            if i.first().copied().unwrap_or(0xFF) & 0x0F == 0 {
                let prefix: String = i[1..4].iter().filter(|b| b.is_ascii_graphic()).map(|b| *b as char).collect();
                fw = Some(format!("{} {:02X}.{:02X}.B{:04X}", prefix, i[4], i[5], be16(&i, 6)).trim().to_string());
                break;
            }
        }
        Ok((fw, unit))
    }

    /// Read everything we can show. Individual failures just hide a section.
    pub fn read_all(&mut self) {
        let headset = self.state.kind == Kind::Headset;
        self.incomplete = false;
        match self.read_battery(headset) {
            Ok(b) => self.state.battery = b,
            Err(e) if e.is_offline() => {
                self.state.online = false;
                self.incomplete = true;
                return;
            }
            Err(e) => crate::debug!("{}: battery: {}", self.state.name, e),
        }
        self.read_onboard_mode();
        let sections: [(&str, fn(&mut Self) -> Result<(), HErr>); 5] = [
            ("dpi", Self::read_dpi),
            ("report rate", Self::read_rate),
            ("lighting", Self::read_leds),
            ("controls", Self::read_controls),
            ("audio", Self::read_audio),
        ];
        for (what, read) in sections {
            if let Err(e) = read(self) {
                crate::debug!("{}: {}: {}", self.state.name, what, e);
                self.incomplete |= e.is_offline();
            }
        }
        self.state.notes.clear();
        if self.state.onboard_mode.as_deref() == Some("onboard") && self.state.dpi.is_some() {
            self.state.notes.push(
                "The mouse is using its on-board profile. Changing DPI or polling rate switches it to host mode.".into(),
            );
        }
    }

    fn read_battery(&self, headset: bool) -> Result<Option<Battery>, HErr> {
        if self.legacy {
            // Register 0x0D has a percentage; older devices only 0x07's coarse level.
            return match self.ch.read_register(self.index, 0x0D, false, &[]) {
                Ok(r) => Ok(decode_battery_charge_register(&r)),
                Err(e) if e.is_offline() => Err(e),
                Err(_) => Ok(decode_battery_status_register(&self.ch.read_register(self.index, 0x07, false, &[])?)),
            };
        }
        if self.features.has(f::UNIFIED_BATTERY) {
            return Ok(Some(decode_unified_battery(&self.req(f::UNIFIED_BATTERY, 1, &[])?)));
        }
        if self.features.has(f::BATTERY_STATUS) {
            return Ok(Some(decode_battery_status(&self.req(f::BATTERY_STATUS, 0, &[])?)));
        }
        if self.features.has(f::BATTERY_VOLTAGE) {
            return Ok(Some(decode_battery_voltage(&self.req(f::BATTERY_VOLTAGE, 0, &[])?, headset)));
        }
        if self.features.has(f::ADC_MEASUREMENT) {
            return Ok(decode_adc(&self.req(f::ADC_MEASUREMENT, 0, &[])?, headset));
        }
        Ok(None)
    }

    fn read_onboard_mode(&mut self) {
        if !self.features.has(f::ONBOARD_PROFILES) {
            return;
        }
        if let Ok(r) = self.req(f::ONBOARD_PROFILES, 2, &[]) {
            self.state.onboard_mode = Some(if r.first() == Some(&1) { "onboard" } else { "host" }.into());
        }
    }

    fn read_dpi(&mut self) -> Result<(), HErr> {
        let prev_stages = self.state.dpi.as_ref().map(|d| (d.stages.clone(), d.active_stage));
        let mut dpi;
        if self.features.has(f::ADJUSTABLE_DPI) {
            let list = self.req(f::ADJUSTABLE_DPI, 1, &[0])?;
            let values = parse_dpi_list(list.get(1..).unwrap_or(&[]));
            if values.is_empty() {
                return Ok(());
            }
            dpi = dpi_from_values(&values);
            let cur = self.req(f::ADJUSTABLE_DPI, 2, &[0])?;
            dpi.value = be16(&cur, 1);
            self.dpi_kind = DpiKind::Basic;
        } else if self.features.has(f::EXTENDED_ADJUSTABLE_DPI) {
            let caps = self.req(f::EXTENDED_ADJUSTABLE_DPI, 1, &[0])?;
            let y = caps.get(2).copied().unwrap_or(0) & 0x01 != 0;
            let mut bytes = Vec::new();
            for page in 0..16u8 {
                let r = self.req(f::EXTENDED_ADJUSTABLE_DPI, 2, &[0, 0, page])?;
                bytes.extend_from_slice(r.get(3..).unwrap_or(&[]));
                let n = bytes.len();
                if n < 2 || (bytes[n - 2] == 0 && bytes[n - 1] == 0) || r.len() <= 3 {
                    break;
                }
            }
            let values = parse_dpi_list(&bytes);
            if values.is_empty() {
                return Ok(());
            }
            dpi = dpi_from_values(&values);
            let cur = self.req(f::EXTENDED_ADJUSTABLE_DPI, 5, &[0])?;
            dpi.value = be16(&cur, 1);
            self.dpi_kind = DpiKind::Extended { y };
        } else {
            return Ok(());
        }
        dpi.max_stages = MAX_SOFT_STAGES;
        dpi.hardware_stages = false;
        match prev_stages {
            Some((s, a)) if !s.is_empty() => {
                dpi.stages = s;
                dpi.active_stage = a;
            }
            _ => {
                let mut stages: Vec<u16> =
                    DEFAULT_STAGES.iter().map(|v| dpi.snap(*v)).filter(|v| *v >= dpi.min && *v <= dpi.max).collect();
                stages.dedup();
                if !stages.contains(&dpi.value) && dpi.value > 0 {
                    stages.push(dpi.value);
                    stages.sort_unstable();
                }
                dpi.active_stage = stages.iter().position(|v| *v == dpi.value).unwrap_or(0);
                dpi.stages = stages;
            }
        }
        self.state.dpi = Some(dpi);
        Ok(())
    }

    fn read_rate(&mut self) -> Result<(), HErr> {
        if self.features.has(f::REPORT_RATE) {
            let mask = self.req(f::REPORT_RATE, 0, &[])?.first().copied().unwrap_or(0);
            let mut options: Vec<u16> = (0..8).filter(|i| mask & (1 << i) != 0).map(|i| 1000 / (i + 1) as u16).collect();
            options.sort_unstable();
            let ms = self.req(f::REPORT_RATE, 1, &[])?.first().copied().unwrap_or(1).max(1);
            self.state.polling = Some(Polling { value: 1000 / ms as u16, options });
            self.rate_kind = RateKind::Basic;
        } else if self.features.has(f::EXTENDED_ADJUSTABLE_REPORT_RATE) {
            let r = self.req(f::EXTENDED_ADJUSTABLE_REPORT_RATE, 1, &[])?;
            let mask = be16(&r, 0);
            let options: Vec<u16> = (0..7).filter(|i| mask & (1 << i) != 0).map(|i| 125u16 << i).collect();
            let idx = self.req(f::EXTENDED_ADJUSTABLE_REPORT_RATE, 2, &[])?.first().copied().unwrap_or(3);
            self.state.polling = Some(Polling { value: 125u16 << idx.min(6), options });
            self.rate_kind = RateKind::Extended;
        }
        Ok(())
    }

    fn read_leds(&mut self) -> Result<(), HErr> {
        let prev: Vec<Zone> = std::mem::take(&mut self.state.zones);
        self.led_zones.clear();
        if self.features.has(f::BRIGHTNESS_CONTROL) {
            if let Ok(info) = self.req(f::BRIGHTNESS_CONTROL, 0, &[]) {
                let max = be16(&info, 0).max(1);
                let min = be16(&info, 4).min(max);
                let on_off = info.get(3).copied().unwrap_or(0) & 0x04 != 0;
                self.hw_brightness = Some((min, max, on_off));
                let mut cur = self.req(f::BRIGHTNESS_CONTROL, 1, &[]).map(|r| be16(&r, 0)).unwrap_or(max);
                if on_off && self.req(f::BRIGHTNESS_CONTROL, 3, &[]).map(|r| r.first().copied().unwrap_or(1) & 1 == 0).unwrap_or(false) {
                    cur = 0;
                }
                self.led_brightness = ((cur.min(max) as u32 * 100 + max as u32 / 2) / max as u32) as u8;
            }
        }
        let feature = if self.features.has(f::RGB_EFFECTS) {
            f::RGB_EFFECTS
        } else if self.features.has(f::COLOR_LED_EFFECTS) {
            f::COLOR_LED_EFFECTS
        } else {
            return Ok(());
        };
        let count = if feature == f::RGB_EFFECTS {
            self.req(feature, 0, &[0xFF, 0xFF, 0x00])?.get(2).copied().unwrap_or(0)
        } else {
            self.req(feature, 0, &[])?.first().copied().unwrap_or(0)
        };
        for z in 0..count.min(8) {
            let (location, n) = if feature == f::RGB_EFFECTS {
                let r = self.req(feature, 0, &[z, 0xFF, 0x00])?;
                (be16(&r, 2), r.get(4).copied().unwrap_or(0))
            } else {
                let r = self.req(feature, 1, &[z])?;
                (be16(&r, 1), r.get(3).copied().unwrap_or(0))
            };
            let mut effects = Vec::new();
            for e in 0..n.min(16) {
                let r = if feature == f::RGB_EFFECTS {
                    self.req(feature, 0, &[z, e, 0x00])?
                } else {
                    self.req(feature, 2, &[z, e])?
                };
                effects.push((e, be16(&r, 2)));
            }
            let (id, name) = zone_name(location);
            let mut ours: Vec<Effect> = Vec::new();
            for (_, eid) in &effects {
                if let Some(e) = effect_of(*eid) {
                    if !ours.contains(&e) {
                        ours.push(e);
                    }
                }
            }
            ours.sort_by_key(|e| Effect::ALL.iter().position(|x| x == e));
            if ours.is_empty() {
                continue;
            }
            let state = prev.iter().find(|p| p.id == id).and_then(|p| p.state.clone());
            let brightness = Some(self.led_brightness);
            self.state.zones.push(Zone { id: id.clone(), name, effects: ours, state, brightness, dual_color: false });
            self.led_zones.push(LedZone { index: z, id, effects });
        }
        self.led_feature = Some(feature);
        Ok(())
    }

    fn read_controls(&mut self) -> Result<(), HErr> {
        if !self.features.has(f::REPROG_CONTROLS_V4) {
            return Ok(());
        }
        let prev: Vec<Control> = std::mem::take(&mut self.state.controls);
        self.reprog.clear();
        let count = self.req(f::REPROG_CONTROLS_V4, 0, &[])?.first().copied().unwrap_or(0);
        for i in 0..count {
            let r = self.req(f::REPROG_CONTROLS_V4, 1, &[i])?;
            let cid = be16(&r, 0);
            let flags = r.get(4).copied().unwrap_or(0) as u16 | ((r.get(8).copied().unwrap_or(0) as u16) << 8);
            let divertable = flags & 0x20 != 0;
            let mouse = flags & 0x01 != 0 || matches!(cid, 0x00C3 | 0x00C4 | 0x00D7 | 0x00ED | 0x00FD | 0x00A9);
            if !divertable || !mouse || self.reprog.iter().any(|c| c.cid == cid) {
                continue;
            }
            let id = format!("{:04x}", cid);
            let action = prev.iter().find(|c| c.id == id).map(|c| c.action.clone()).unwrap_or_else(|| "default".into());
            self.state.controls.push(Control { id, name: control_name(cid), action, remappable: cid != 0x0050 });
            self.reprog.push(Reprog { cid, diverted: false });
        }
        Ok(())
    }

    fn read_audio(&mut self) -> Result<(), HErr> {
        if self.features.has(f::SIDETONE) {
            let v = self.req(f::SIDETONE, 0, &[])?.first().copied().unwrap_or(0) as i32;
            self.state.sidetone = Some(Range { value: v.min(100), min: 0, max: 100, step: 1 });
        }
        if self.features.has(f::EQUALIZER) {
            let info = self.req(f::EQUALIZER, 0, &[])?;
            let count = info.first().copied().unwrap_or(0) as usize;
            let range = info.get(1).copied().unwrap_or(12) as i32;
            let mut min = info.get(3).copied().unwrap_or(0) as i8 as i32;
            let mut max = info.get(4).copied().unwrap_or(0) as i8 as i32;
            if min == 0 {
                min = -range;
            }
            if max == 0 {
                max = range;
            }
            if count > 0 && count <= 20 {
                let mut freqs = Vec::new();
                for g in 0..count.div_ceil(7) {
                    let r = self.req(f::EQUALIZER, 1, &[(g * 7) as u8])?;
                    for b in 0..7 {
                        if g * 7 + b >= count {
                            break;
                        }
                        freqs.push(be16(&r, 1 + 2 * b) as u32);
                    }
                }
                let cur = self.req(f::EQUALIZER, 2, &[0x00])?;
                let gains: Vec<i32> =
                    (0..count).map(|i| (cur.get(i).copied().unwrap_or(0) as i8 as i32).clamp(min, max)).collect();
                if self.eq_writable.is_none() && self.features.version(f::EQUALIZER).unwrap_or(0) < 2 {
                    // Writing back the current gains changes nothing, but tells us whether
                    // the firmware lets the host set the EQ at all.
                    let mut payload = vec![0x02];
                    payload.extend(gains.iter().map(|g| *g as i8 as u8));
                    match self.req(f::EQUALIZER, 3, &payload) {
                        Ok(_) => self.eq_writable = Some(true),
                        Err(HErr::V20(_)) => {
                            crate::info!("{}: the headset rejects equalizer changes, applying the equalizer in software", self.state.name);
                            self.eq_writable = Some(false);
                        }
                        Err(e) => return Err(e),
                    }
                }
                if self.eq_writable == Some(false) {
                    // Peridot applies this one through PipeWire; keep what the user set
                    // rather than the headset's (unused) stored table.
                    let gains = match &self.state.equalizer {
                        Some(e) if e.software && e.gains.len() == count => e.gains.clone(),
                        _ => vec![0; count],
                    };
                    let presets = g_series_presets(&freqs);
                    self.state.equalizer = Some(Equalizer { freqs, gains, min, max, software: true, presets });
                } else {
                    let presets = g_series_presets(&freqs);
                    self.state.equalizer = Some(Equalizer { freqs, gains, min, max, software: false, presets });
                }
            }
        }
        if self.features.has(f::ADC_MEASUREMENT) && self.features.version(f::ADC_MEASUREMENT).unwrap_or(0) >= 2 {
            if let Ok(r) = self.req(f::ADC_MEASUREMENT, 1, &[]) {
                let v = r.first().copied().unwrap_or(0) as i32;
                let mut options = vec![0, 5, 10, 15, 20, 30, 45, 60, 90];
                if !options.contains(&v) {
                    options.push(v);
                    options.sort_unstable();
                }
                self.state.auto_off = Some(Choice { value: v, options });
            }
        }
        Ok(())
    }

    fn mark_offline_on<T>(&mut self, r: Result<T, HErr>) -> model::Result<T> {
        match r {
            Err(e) if e.is_offline() => {
                self.state.online = false;
                Err(model::Error::Offline)
            }
            Err(e) => Err(e.into()),
            Ok(v) => Ok(v),
        }
    }

    /// Switch to host mode so DPI/report-rate writes take effect.
    fn ensure_host_mode(&mut self) -> model::Result<()> {
        if self.state.onboard_mode.as_deref() == Some("onboard") {
            let r = self.req(f::ONBOARD_PROFILES, 1, &[0x02]);
            self.mark_offline_on(r)?;
            self.state.onboard_mode = Some("host".into());
            self.host_mode_forced = true;
            self.state.notes.clear();
        }
        Ok(())
    }

    fn write_dpi(&mut self, value: u16) -> model::Result<u16> {
        let dpi = self.state.dpi.clone().ok_or_else(|| model::Error::NotSupported("DPI".into()))?;
        let v = dpi.snap(value);
        self.ensure_host_mode()?;
        let r = match self.dpi_kind {
            DpiKind::Basic => self.req(f::ADJUSTABLE_DPI, 3, &[0, (v >> 8) as u8, v as u8]).map(|_| ()),
            DpiKind::Extended { y } => {
                let lod = self.req(f::EXTENDED_ADJUSTABLE_DPI, 5, &[0]).ok().and_then(|r| r.get(9).copied()).unwrap_or(1);
                let yv = if y { v } else { 0 };
                self.req(f::EXTENDED_ADJUSTABLE_DPI, 6, &[0, (v >> 8) as u8, v as u8, (yv >> 8) as u8, yv as u8, lod])
                    .map(|_| ())
            }
            DpiKind::None => return Err(model::Error::NotSupported("DPI".into())),
        };
        self.mark_offline_on(r)?;
        if let Some(d) = self.state.dpi.as_mut() {
            d.value = v;
        }
        Ok(v)
    }

    fn write_rate(&mut self, hz: u16) -> model::Result<()> {
        let p = self.state.polling.clone().ok_or_else(|| model::Error::NotSupported("polling rate".into()))?;
        if !p.options.contains(&hz) {
            return invalid(format!("{} Hz is not supported (choose from {:?})", hz, p.options));
        }
        self.ensure_host_mode()?;
        let r = match self.rate_kind {
            RateKind::Basic => self.req(f::REPORT_RATE, 2, &[(1000 / hz) as u8]).map(|_| ()),
            RateKind::Extended => {
                let idx = (0..7u8).find(|i| 125u16 << i == hz).unwrap_or(3);
                self.req(f::EXTENDED_ADJUSTABLE_REPORT_RATE, 3, &[idx]).map(|_| ())
            }
            RateKind::None => return Err(model::Error::NotSupported("polling rate".into())),
        };
        self.mark_offline_on(r)?;
        if let Some(p) = self.state.polling.as_mut() {
            p.value = hz;
        }
        Ok(())
    }

    fn claim_leds(&mut self) {
        if self.led_claimed {
            return;
        }
        let _ = match self.led_feature {
            Some(f::RGB_EFFECTS) => self.req(f::RGB_EFFECTS, 5, &[0x01, 0x03, 0x04]),
            Some(f::COLOR_LED_EFFECTS) => self.req(f::COLOR_LED_EFFECTS, 8, &[0x01]),
            _ => return,
        };
        self.led_claimed = true;
    }

    fn write_lighting(&mut self, zone_id: &str, l: &Lighting) -> model::Result<()> {
        let feature = self.led_feature.ok_or_else(|| model::Error::NotSupported("lighting".into()))?;
        let targets: Vec<LedZone> = if zone_id == "all" {
            self.led_zones.clone()
        } else {
            self.led_zones.iter().filter(|z| z.id == zone_id).cloned().collect()
        };
        if targets.is_empty() {
            return invalid(format!("unknown lighting zone '{}'", zone_id));
        }
        self.claim_leds();
        let mut applied = 0;
        for z in &targets {
            // Prefer the richer effect ids when a zone offers both variants.
            let pick = z
                .effects
                .iter()
                .filter(|(_, id)| effect_of(*id) == Some(l.effect))
                .max_by_key(|(_, id)| match id {
                    0x0A => 3,
                    0x15 | 0x16 | 0x17 => 2,
                    _ => 1,
                })
                .copied();
            let Some((eidx, eid)) = pick else {
                if zone_id == "all" {
                    continue;
                }
                return invalid(format!("zone '{}' can't do {}", z.id, l.effect.as_str()));
            };
            let params = if self.hw_brightness.is_some() {
                effect_params(eid, l)
            } else {
                scaled_effect_params(eid, l, self.led_brightness)
            };
            let mut payload = vec![z.index, eidx];
            payload.extend_from_slice(&params);
            let r = if feature == f::RGB_EFFECTS {
                payload.push(0x01);
                self.req(feature, 1, &payload)
            } else {
                self.req(feature, 3, &payload)
            };
            self.mark_offline_on(r)?;
            if let Some(zs) = self.state.zones.iter_mut().find(|s| s.id == z.id) {
                zs.state = Some(l.clone());
            }
            applied += 1;
        }
        if applied == 0 {
            return invalid(format!("no zone supports {}", l.effect.as_str()));
        }
        Ok(())
    }

    fn write_brightness(&mut self, pct: u8) -> model::Result<()> {
        if self.led_feature.is_none() && self.hw_brightness.is_none() {
            return Err(model::Error::NotSupported("brightness".into()));
        }
        let pct = pct.min(100);
        if let Some((min, max, on_off)) = self.hw_brightness {
            if on_off {
                let r = self.req(f::BRIGHTNESS_CONTROL, 4, &[(pct > 0) as u8]);
                self.mark_offline_on(r)?;
            }
            if pct > 0 || !on_off {
                let v = ((max as u32 * pct as u32 + 50) / 100).max(min as u32) as u16;
                let r = self.req(f::BRIGHTNESS_CONTROL, 2, &v.to_be_bytes());
                self.mark_offline_on(r)?;
            }
            self.led_brightness = pct;
        } else {
            self.led_brightness = pct;
            // Re-send each zone's effect at the new brightness.
            let current: Vec<(String, Lighting)> =
                self.state.zones.iter().filter_map(|z| z.state.clone().map(|l| (z.id.clone(), l))).collect();
            for (zone, l) in current {
                self.write_lighting(&zone, &l)?;
            }
        }
        for z in self.state.zones.iter_mut() {
            z.brightness = Some(pct);
        }
        Ok(())
    }

    fn set_divert(&mut self, cid: u16, divert: bool) -> model::Result<()> {
        let flags = if divert { 0x03 } else { 0x02 };
        let r = self.req(f::REPROG_CONTROLS_V4, 3, &[(cid >> 8) as u8, cid as u8, flags, 0, 0]);
        self.mark_offline_on(r)?;
        if let Some(c) = self.reprog.iter_mut().find(|c| c.cid == cid) {
            c.diverted = divert;
        }
        Ok(())
    }

    fn set_stage(&mut self, idx: usize) -> model::Result<()> {
        let v = {
            let d = self.state.dpi.as_ref().ok_or_else(|| model::Error::NotSupported("DPI".into()))?;
            *d.stages.get(idx).ok_or_else(|| model::Error::Invalid("stage index".into()))?
        };
        self.write_dpi(v)?;
        if let Some(d) = self.state.dpi.as_mut() {
            d.active_stage = idx;
        }
        Ok(())
    }

    /// Handle a device-initiated report addressed to this device. Returns the
    /// button transitions it produced (if any) and whether state changed.
    pub fn handle_notification(&mut self, msg: &Message) -> (Vec<ButtonEvent>, bool) {
        if self.legacy {
            // Battery notifications carry the register's contents.
            let b = match msg.sub {
                0x0D => decode_battery_charge_register(&msg.params_with_address()),
                0x07 => decode_battery_status_register(&msg.params_with_address()),
                _ => return (vec![], false),
            };
            let changed = b.is_some();
            if changed {
                self.state.battery = b;
            }
            return (vec![], changed);
        }
        if msg.swid() != 0 {
            return (vec![], false);
        }
        let Some(feature) = self.features.id_at(msg.sub) else { return (vec![], false) };
        let headset = self.state.kind == Kind::Headset;
        match (feature, msg.function()) {
            (f::UNIFIED_BATTERY, 0) => {
                self.state.battery = Some(decode_unified_battery(&msg.params));
                (vec![], true)
            }
            (f::BATTERY_STATUS, 0) => {
                self.state.battery = Some(decode_battery_status(&msg.params));
                (vec![], true)
            }
            (f::BATTERY_VOLTAGE, 0) => {
                self.state.battery = Some(decode_battery_voltage(&msg.params, headset));
                (vec![], true)
            }
            (f::ADC_MEASUREMENT, 0) => {
                self.state.battery = decode_adc(&msg.params, headset);
                (vec![], true)
            }
            (f::REPROG_CONTROLS_V4, 0) => {
                let mut now = Vec::new();
                for i in 0..4 {
                    let cid = be16(&msg.params, i * 2);
                    if cid != 0 {
                        now.push(cid);
                    }
                }
                let mut events = Vec::new();
                for cid in &now {
                    if !self.pressed.contains(cid) {
                        events.push(ButtonEvent { control: format!("{:04x}", cid), pressed: true });
                    }
                }
                for cid in &self.pressed {
                    if !now.contains(cid) {
                        events.push(ButtonEvent { control: format!("{:04x}", cid), pressed: false });
                    }
                }
                self.pressed = now;
                (events, false)
            }
            (f::ONBOARD_PROFILES, _) => {
                self.read_onboard_mode();
                (vec![], true)
            }
            _ => (vec![], false),
        }
    }

    pub fn set_online(&mut self, online: bool) {
        self.state.online = online;
        if !online {
            self.pressed.clear();
            self.led_claimed = false;
            for c in &mut self.reprog {
                c.diverted = false;
            }
        }
    }
}

impl model::Device for LogitechDevice {
    fn state(&self) -> &DeviceState {
        &self.state
    }

    fn refresh(&mut self) -> model::Result<()> {
        if self.placeholder {
            return Err(model::Error::Offline);
        }
        if self.incomplete {
            // Answering again: read what was missed while it wasn't.
            self.read_all();
            return if self.state.online { Ok(()) } else { Err(model::Error::Offline) };
        }
        let headset = self.state.kind == Kind::Headset;
        let r = self.read_battery(headset);
        let b = self.mark_offline_on(r)?;
        self.state.battery = b;
        self.state.online = true;
        Ok(())
    }

    fn apply(&mut self, key: &str, value: &Json) -> model::Result<()> {
        if self.placeholder {
            // Nothing is known about the device yet; the manager keeps the
            // setting and replays it once the device wakes up and is probed.
            return Err(model::Error::Offline);
        }
        if self.legacy {
            return Err(model::Error::NotSupported(key.into()));
        }
        match key {
            "dpi" => {
                let v = value.as_i64().ok_or_else(|| model::Error::Invalid("DPI must be a number".into()))?;
                let v = self.write_dpi(v.clamp(1, 65535) as u16)?;
                if let Some(d) = self.state.dpi.as_mut() {
                    if let Some(pos) = d.stages.iter().position(|s| *s == v) {
                        d.active_stage = pos;
                    } else if let Some(s) = d.stages.get_mut(d.active_stage) {
                        *s = v;
                        d.stages.sort_unstable();
                        d.active_stage = d.stages.iter().position(|s| *s == v).unwrap_or(0);
                    }
                }
                Ok(())
            }
            "dpi_stages" => {
                let dpi = self.state.dpi.clone().ok_or_else(|| model::Error::NotSupported("DPI".into()))?;
                let (stages, active) = model::parse_stages(value, &dpi)?;
                if let Some(d) = self.state.dpi.as_mut() {
                    d.stages = stages;
                    d.active_stage = active;
                }
                self.set_stage(active)
            }
            "dpi_cycle" => {
                let dpi = self.state.dpi.clone().ok_or_else(|| model::Error::NotSupported("DPI".into()))?;
                let op = value.as_str().unwrap_or("up");
                let idx = model::cycle_stage(&dpi, op).ok_or_else(|| model::Error::Invalid(op.into()))?;
                self.set_stage(idx)
            }
            "dpi_shift" => {
                let dpi = self.state.dpi.clone().ok_or_else(|| model::Error::NotSupported("DPI".into()))?;
                if value.as_bool().unwrap_or(false) {
                    if self.shift_restore.is_none() {
                        self.shift_restore = Some(dpi.value);
                        let low = dpi.stages.iter().copied().min().unwrap_or(dpi.min.max(400));
                        self.write_dpi(low)?;
                    }
                } else if let Some(prev) = self.shift_restore.take() {
                    self.write_dpi(prev)?;
                }
                Ok(())
            }
            "polling_rate" => {
                let hz = value.as_i64().ok_or_else(|| model::Error::Invalid("rate must be a number".into()))?;
                self.write_rate(hz.clamp(1, 65535) as u16)
            }
            "lighting" => {
                let l = Lighting::from_json(value)?;
                let zone = value.get("zone").as_str().unwrap_or("all").to_string();
                self.write_lighting(&zone, &l)
            }
            "brightness" => {
                let v = value.get("value").as_i64().or_else(|| value.as_i64()).ok_or_else(|| {
                    model::Error::Invalid("brightness expects {\"value\": 0-100}".into())
                })?;
                self.write_brightness(v.clamp(0, 100) as u8)
            }
            "button" => {
                let control = value.get("control").as_str().ok_or_else(|| model::Error::Invalid("missing control".into()))?;
                let action_s = value.get("action").as_str().unwrap_or("default");
                let action = Action::parse(action_s).map_err(model::Error::Invalid)?;
                let cid = u16::from_str_radix(control.trim_start_matches("0x"), 16)
                    .map_err(|_| model::Error::Invalid(format!("bad control id '{}'", control)))?;
                let ctl = self
                    .state
                    .controls
                    .iter()
                    .find(|c| c.id == format!("{:04x}", cid))
                    .cloned()
                    .ok_or_else(|| model::Error::Invalid(format!("unknown control '{}'", control)))?;
                if !ctl.remappable && action != Action::Default {
                    return invalid(format!("{} can't be remapped", ctl.name));
                }
                self.set_divert(cid, action != Action::Default)?;
                let canonical = action.to_string_canonical();
                if let Some(c) = self.state.controls.iter_mut().find(|c| c.id == ctl.id) {
                    c.action = canonical;
                }
                Ok(())
            }
            "sidetone" => {
                let v = value.as_i64().ok_or_else(|| model::Error::Invalid("sidetone must be a number".into()))?;
                let v = v.clamp(0, 100) as u8;
                let r = self.req(f::SIDETONE, 1, &[v]);
                self.mark_offline_on(r)?;
                if let Some(s) = self.state.sidetone.as_mut() {
                    s.value = v as i32;
                }
                Ok(())
            }
            "equalizer" => {
                let eq = self.state.equalizer.clone().ok_or_else(|| model::Error::NotSupported("equalizer".into()))?;
                let arr = value.as_arr().or_else(|| value.get("gains").as_arr()).ok_or_else(|| {
                    model::Error::Invalid("equalizer expects an array of gains".into())
                })?;
                if arr.len() != eq.gains.len() {
                    return invalid(format!("expected {} bands", eq.gains.len()));
                }
                let gains: Vec<i32> = arr.iter().map(|g| (g.as_i64().unwrap_or(0) as i32).clamp(eq.min, eq.max)).collect();
                if eq.software {
                    // The manager hands the new gains to PipeWire.
                    if let Some(e) = self.state.equalizer.as_mut() {
                        e.gains = gains;
                    }
                    return Ok(());
                }
                let mut payload = vec![0x02];
                payload.extend(gains.iter().map(|g| *g as i8 as u8));
                let r = self.req(f::EQUALIZER, 3, &payload);
                self.mark_offline_on(r)?;
                if let Some(e) = self.state.equalizer.as_mut() {
                    e.gains = gains;
                }
                Ok(())
            }
            "auto_off" => {
                let v = value.as_i64().ok_or_else(|| model::Error::Invalid("minutes must be a number".into()))?;
                if self.state.auto_off.is_none() {
                    return Err(model::Error::NotSupported("auto power-off".into()));
                }
                let v = v.clamp(0, 255) as u8;
                let r = self.req(f::ADC_MEASUREMENT, 2, &[v]);
                self.mark_offline_on(r)?;
                if let Some(a) = self.state.auto_off.as_mut() {
                    a.value = v as i32;
                }
                Ok(())
            }
            "onboard_mode" => {
                if !self.features.has(f::ONBOARD_PROFILES) {
                    return Err(model::Error::NotSupported("on-board profiles".into()));
                }
                let mode = match value.as_str() {
                    Some("onboard") => 1u8,
                    Some("host") => 2u8,
                    _ => return invalid("onboard_mode must be 'host' or 'onboard'"),
                };
                let r = self.req(f::ONBOARD_PROFILES, 1, &[mode]);
                self.mark_offline_on(r)?;
                self.state.onboard_mode = Some(if mode == 1 { "onboard" } else { "host" }.into());
                // Re-read what the active profile now reports.
                let _ = self.read_dpi();
                let _ = self.read_rate();
                Ok(())
            }
            _ => Err(model::Error::NotSupported(format!("setting '{}'", key))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Rgb;

    #[test]
    fn dpi_list_parsing() {
        // 200, step 50 up to 400, then 800 and 1600 discrete
        let bytes = [0x00, 0xC8, 0xE0, 0x32, 0x01, 0x90, 0x03, 0x20, 0x06, 0x40, 0x00, 0x00];
        assert_eq!(parse_dpi_list(&bytes), vec![200, 250, 300, 350, 400, 800, 1600]);
        let d = dpi_from_values(&parse_dpi_list(&[0x00, 0x64, 0xE0, 0x32, 0x64, 0x00, 0, 0]));
        assert_eq!((d.min, d.max, d.step), (100, 25600, 50));
        assert!(d.list.is_empty());
    }

    #[test]
    fn battery_decoding() {
        let b = decode_unified_battery(&[73, 4, 1, 0]);
        assert_eq!(b.percent, Some(73));
        assert_eq!(b.charge, Charge::Charging);
        let b = decode_unified_battery(&[0, 2, 0, 0]);
        assert_eq!(b.percent, Some(20));
        assert!(b.estimated);
        let b = decode_battery_voltage(&[0x0F, 0x50, 0x80], false);
        assert_eq!(b.voltage_mv, Some(3920));
        assert_eq!(b.charge, Charge::Charging);
        assert!(decode_adc(&[0x0F, 0x50, 0x00], true).is_none());
        assert_eq!(decode_adc(&[0x0F, 0x50, 0x03], true).unwrap().charge, Charge::Charging);
    }

    #[test]
    fn effect_encoding() {
        let l = Lighting { effect: Effect::Static, color: Rgb(1, 2, 3), ..Default::default() };
        assert_eq!(&effect_params(0x01, &l)[..3], &[1, 2, 3]);
        let o = Lighting { effect: Effect::Static, color: Rgb(200, 100, 0), ..Default::default() };
        assert_eq!(&scaled_effect_params(0x01, &o, 50)[..3], &[100, 50, 0]);
        let cyc = Lighting { effect: Effect::Spectrum, ..Default::default() };
        assert_eq!(scaled_effect_params(0x03, &cyc, 40)[7], 40);
        assert_eq!(scaled_effect_params(0x03, &cyc, 100), effect_params(0x03, &cyc));
        let b = Lighting { effect: Effect::Breathing, color: Rgb(9, 8, 7), speed: 100, ..Default::default() };
        let p = effect_params(0x0A, &b);
        assert_eq!(&p[..3], &[9, 8, 7]);
        assert_eq!(u16::from_be_bytes([p[3], p[4]]), 1000);
        let w = Lighting { effect: Effect::Wave, direction: Direction::Left, speed: 0, ..Default::default() };
        let p = effect_params(0x04, &w);
        assert_eq!(u16::from_be_bytes([p[6], p[7]]), 10000);
        assert_eq!(p[9], 6);
    }
}
