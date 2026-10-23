//! Logitech "Centurion" headsets: G PRO X 2 LIGHTSPEED and G522 LIGHTSPEED.
//!
//! They don't speak HID++ but a newer protocol on a vendor interface (usage
//! page 0xFFA0): 64-byte frames of `prefix, length, flags, payload`. The USB
//! dongle answers itself; features of the headset are reached through the
//! dongle's "bridge" feature, which wraps each request in a sub-message and
//! answers with an acknowledgement followed by the headset's reply. Feature
//! indexes are looked up by id (Root.getFeature), like HID++ 2.0.
//!
//! Byte layouts follow HeadsetControl's implementation and its G HUB captures
//! (lib/devices/protocols/logitech_centurion_protocol.hpp and the two device
//! files).

use crate::backend::{Backend, BackendEvent, DeviceHandle, Sink};
use crate::hid::Transport;
use crate::json::Json;
use crate::model::{self, invalid, Battery, Charge, Choice, Connection, DeviceState, Equalizer, Kind, Mode, Range};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const FRAME: usize = 64;
const SOFTWARE_ID: u8 = 0x01;
const BRIDGE_SEND: u8 = 0x10;
/// Largest sub-message that fits one frame (longer ones would need fragments).
const MAX_BRIDGE_PAYLOAD: usize = 56;
const READ_TIMEOUT_MS: i32 = 600;
const READS_PER_REQUEST: usize = 8;
const BATTERY_REFRESH: Duration = Duration::from_secs(60);

const ROOT: u16 = 0x0000;
const BRIDGE: u16 = 0x0003;
const BATTERY: u16 = 0x0104;
const AUTO_SLEEP: u16 = 0x0108;
const PARA_EQ: u16 = 0x020d;
const MIC_MUTE_LED: u16 = 0x0601;
const SIDETONE: u16 = 0x0604;
const VOICE_PROMPTS: u16 = 0x060b;
const LIGHTING: u16 = 0x0621;

const SIDETONE_MIC: u8 = 0x01;
const AUTO_OFF_OPTIONS: [i32; 9] = [0, 5, 10, 15, 20, 30, 45, 60, 90];

/// The G522's playback EQ: ten peaking bands at G HUB's default frequencies
/// and Q, gains in 0.05 dB steps around a raw zero of 120.
const G522_EQ_FREQS: [u32; 10] = [20, 50, 125, 250, 500, 1000, 2500, 5000, 10000, 20000];
const G522_EQ_Q_RAW: u8 = 22; // 22/32 = 0.688, G HUB's default
const G522_EQ_ZERO: i32 = 120;
const G522_EQ_MAX_DB: i32 = 6;
/// G HUB's "lights on" parameters for the G522.
const G522_LIGHTS_ON: [u8; 10] = [0x00, 0x00, 0x04, 0x00, 0xb8, 0xfc, 0xff, 0x00, 0xab, 0x64];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Model {
    pub pid: u16,
    pub name: &'static str,
    /// Bytes every frame starts with.
    pub prefix: &'static [u8],
    pub sidetone_max: u8,
    /// G522-style extras: fixed 10-band EQ, lights, voice prompts, mute LED.
    pub g522: bool,
}

pub const MODELS: &[Model] = &[
    Model { pid: 0x0af7, name: "G PRO X 2 LIGHTSPEED", prefix: &[0x51], sidetone_max: 100, g522: false },
    Model { pid: 0x0b18, name: "G522 LIGHTSPEED", prefix: &[0x50, 0x23], sidetone_max: 9, g522: true },
];

pub fn find_model(pid: u16) -> Option<&'static Model> {
    MODELS.iter().find(|m| m.pid == pid)
}

/// The vendor interface the protocol runs on.
pub fn is_control_interface(applications: &[(u16, u16)]) -> bool {
    applications.contains(&(0xFFA0, 0x0001))
}

#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    Timeout,
    /// The headset is off or out of range (the dongle still answers).
    Offline,
    /// The headset doesn't have the feature.
    NotSupported(u16),
    /// The headset or dongle refused the request.
    Rejected,
    Io(String),
}

impl Error {
    fn into_model(self, what: &str) -> model::Error {
        match self {
            Error::Offline | Error::Timeout => model::Error::Offline,
            Error::NotSupported(_) => model::Error::NotSupported(what.into()),
            Error::Rejected => model::Error::Device(format!("the headset refused the {} change", what)),
            Error::Io(e) => model::Error::Device(e),
        }
    }
}

/// `prefix, length, flags, payload`, zero-padded to 64 bytes. The length
/// counts the flags byte and the payload.
pub fn frame(prefix: &[u8], payload: &[u8], flags: u8) -> [u8; FRAME] {
    let mut f = [0u8; FRAME];
    let h = prefix.len();
    f[..h].copy_from_slice(prefix);
    f[h] = (payload.len() + 1) as u8;
    f[h + 1] = flags;
    let n = payload.len().min(FRAME - h - 2);
    f[h + 2..h + 2 + n].copy_from_slice(&payload[..n]);
    f
}

/// The payload of a frame, if it carries our prefix.
pub fn payload(prefix: &[u8], f: &[u8]) -> Option<Vec<u8>> {
    let h = prefix.len();
    if f.len() < h + 3 || &f[..h] != prefix {
        return None;
    }
    let len = f[h] as usize;
    if len <= 1 || len + h + 1 > f.len() {
        return None;
    }
    Some(f[h + 2..h + 1 + len].to_vec())
}

/// `bridge, 0x11, size, sub-message` where the sub-message is
/// `0x00, feature, function | software id, params`.
pub fn bridge_payload(bridge: u8, feature: u8, function: u8, params: &[u8]) -> Vec<u8> {
    let sub_len = params.len() + 3;
    let mut p = vec![bridge, BRIDGE_SEND | SOFTWARE_ID, ((sub_len >> 8) & 0x0F) as u8, (sub_len & 0xFF) as u8];
    p.extend_from_slice(&[0x00, feature, (function & 0xF0) | SOFTWARE_ID]);
    p.extend_from_slice(params);
    p
}

/// The bridge's connection event: `bridge, 0x00, 0x00, connected`.
fn connection_event(reply: &[u8], bridge: u8) -> Option<bool> {
    (reply.len() >= 4 && reply[0] == bridge && reply[1] == 0x00).then(|| reply[3] != 0)
}

/// A frame that came in while waiting for something else.
enum Unsolicited {
    Connected(bool),
}

pub struct Link {
    t: Arc<dyn Transport>,
    prefix: &'static [u8],
    bridge: Option<u8>,
    features: HashMap<u16, u8>,
    events: Vec<Unsolicited>,
}

impl Link {
    pub fn new(t: Arc<dyn Transport>, prefix: &'static [u8]) -> Link {
        Link { t, prefix, bridge: None, features: HashMap::new(), events: Vec::new() }
    }

    fn send(&self, payload: &[u8]) -> Result<(), Error> {
        self.t.write(&frame(self.prefix, payload, 0x00)).map_err(|e| Error::Io(e.to_string()))
    }

    fn read(&mut self, timeout_ms: i32) -> Result<Option<Vec<u8>>, Error> {
        let mut buf = [0u8; FRAME];
        let n = self.t.read_timeout(&mut buf, timeout_ms).map_err(|e| Error::Io(e.to_string()))?;
        if n == 0 {
            return Ok(None);
        }
        let Some(p) = payload(self.prefix, &buf[..n]) else { return Ok(Some(Vec::new())) };
        if let Some(b) = self.bridge {
            if let Some(on) = connection_event(&p, b) {
                self.events.push(Unsolicited::Connected(on));
                return Ok(Some(Vec::new()));
            }
        }
        Ok(Some(p))
    }

    /// A request to the dongle itself.
    fn direct(&mut self, index: u8, function: u8, params: &[u8]) -> Result<Vec<u8>, Error> {
        let mut p = vec![index, (function & 0xF0) | SOFTWARE_ID];
        p.extend_from_slice(params);
        self.send(&p)?;
        for _ in 0..READS_PER_REQUEST {
            match self.read(READ_TIMEOUT_MS)? {
                None => return Err(Error::Timeout),
                Some(r) if r.len() >= 2 && r[0] == index && r[1] == p[1] => return Ok(r[2..].to_vec()),
                Some(r) if r.len() >= 4 && r[0] == 0xFF && r[1] == index => return Err(Error::Rejected),
                Some(_) => {}
            }
        }
        Err(Error::Timeout)
    }

    fn bridge_index(&mut self) -> Result<u8, Error> {
        if let Some(b) = self.bridge {
            return Ok(b);
        }
        let r = self.direct(0x00, 0x00, &BRIDGE.to_be_bytes())?;
        match r.first() {
            Some(0) | None => Err(Error::NotSupported(BRIDGE)),
            Some(b) => {
                self.bridge = Some(*b);
                Ok(*b)
            }
        }
    }

    /// A request to the headset behind the dongle.
    fn bridged(&mut self, feature: u8, function: u8, params: &[u8]) -> Result<Vec<u8>, Error> {
        let bridge = self.bridge_index()?;
        if params.len() + 3 > MAX_BRIDGE_PAYLOAD {
            return Err(Error::Io("request too long for one frame".into()));
        }
        self.send(&bridge_payload(bridge, feature, function, params))?;
        let mut acked = false;
        for _ in 0..READS_PER_REQUEST {
            let Some(r) = self.read(READ_TIMEOUT_MS)? else { break };
            if r.len() >= 3 && r[0] == 0xFF && r[1] == bridge && r[2] == BRIDGE_SEND | SOFTWARE_ID {
                return Err(self.offline_or(Error::Rejected));
            }
            if r.len() < 2 || r[0] != bridge || r[1] >> 4 != BRIDGE_SEND >> 4 {
                continue;
            }
            match r[1] & 0x0F {
                SOFTWARE_ID => acked = true,
                0x00 if r.len() >= 7 && r[4] == 0x00 && (r[5] == feature || (r[5] == 0xFF && r[6] == feature)) => {
                    if r[5] == 0xFF {
                        return Err(Error::Rejected);
                    }
                    return Ok(r[7..].to_vec());
                }
                _ => {}
            }
        }
        Err(if acked { self.offline_or(Error::Timeout) } else { Error::Timeout })
    }

    /// After a failed bridged request: report the headset offline if the
    /// bridge says nothing is connected.
    fn offline_or(&mut self, e: Error) -> Error {
        let Some(bridge) = self.bridge else { return e };
        match self.direct(bridge, 0x00, &[]) {
            Ok(s) if s.len() >= 4 && s[2] == 0x00 && s[3] == 0x00 => Error::Offline,
            _ => e,
        }
    }

    /// The headset's index for a feature.
    fn feature(&mut self, id: u16) -> Result<u8, Error> {
        if let Some(i) = self.features.get(&id) {
            return Ok(*i);
        }
        let r = self.bridged(0x00, 0x00, &id.to_be_bytes())?;
        match r.first() {
            Some(0) if id != ROOT => Err(Error::NotSupported(id)),
            None => Err(Error::NotSupported(id)),
            Some(i) => {
                self.features.insert(id, *i);
                Ok(*i)
            }
        }
    }

    pub fn call(&mut self, id: u16, function: u8, params: &[u8]) -> Result<Vec<u8>, Error> {
        let index = self.feature(id)?;
        self.bridged(index, function, params)
    }

    /// Wait briefly for unsolicited frames (the headset powering on or off).
    fn poll(&mut self, timeout_ms: i32) -> Vec<Unsolicited> {
        let _ = self.read(timeout_ms);
        std::mem::take(&mut self.events)
    }
}

pub fn decode_battery(r: &[u8]) -> Option<Battery> {
    let level = *r.first()?;
    if level > 100 {
        return None;
    }
    let charge = match r.get(2) {
        Some(1) | Some(2) => Charge::Charging,
        Some(3) if level == 100 => Charge::Full,
        _ => Charge::Discharging,
    };
    Some(Battery { percent: Some(level), charge, voltage_mv: None, estimated: false })
}

/// The G522's 53-byte playback EQ parameters for whole-dB gains.
pub fn g522_eq_params(gains: &[i32]) -> Vec<u8> {
    let mut p = vec![0x00, 0x00, 0x00];
    for (i, f) in G522_EQ_FREQS.iter().enumerate() {
        let g = gains.get(i).copied().unwrap_or(0).clamp(-G522_EQ_MAX_DB, G522_EQ_MAX_DB);
        let raw = (g * 20 + G522_EQ_ZERO) as u16; // 0.05 dB steps
        p.extend_from_slice(&[(f >> 8) as u8, (*f & 0xFF) as u8, G522_EQ_Q_RAW, (raw >> 8) as u8, (raw & 0xFF) as u8]);
    }
    p
}

pub struct CenturionDevice {
    link: Link,
    model: &'static Model,
    pub state: DeviceState,
    /// PRO X 2: the parametric EQ slot in use.
    eq_slot: u8,
    /// The last full read was cut short because the headset was off.
    incomplete: bool,
}

fn on_off(id: &str, name: &str, hint: &str, on: bool) -> Mode {
    Mode {
        id: id.into(),
        name: name.into(),
        hint: hint.into(),
        value: if on { "on" } else { "off" }.into(),
        options: vec![("off".into(), "Off".into()), ("on".into(), "On".into())],
    }
}

impl CenturionDevice {
    pub fn new(t: Arc<dyn Transport>, model: &'static Model) -> CenturionDevice {
        let state = DeviceState {
            id: format!("logitech-{:04x}", model.pid),
            name: model.name.into(),
            vendor: "logitech",
            kind: Kind::Headset,
            connection: Connection::Receiver,
            online: false,
            model: format!("046d:{:04x}", model.pid),
            via: Some("LIGHTSPEED receiver".into()),
            protocol: Some("Logitech Centurion".into()),
            ..Default::default()
        };
        CenturionDevice { link: Link::new(t, model.prefix), model, state, eq_slot: 0, incomplete: true }
    }

    /// Read what the headset shows; a powered-off headset leaves it for later.
    pub fn read_all(&mut self) {
        self.incomplete = false;
        match self.link.call(BATTERY, 0x00, &[]) {
            Ok(r) => {
                self.state.battery = decode_battery(&r);
                self.state.online = true;
            }
            Err(Error::Offline) | Err(Error::Timeout) => {
                self.state.online = false;
                self.incomplete = true;
                return;
            }
            Err(e) => crate::debug!("{}: battery: {:?}", self.state.name, e),
        }
        self.state.online = true;
        let max = self.model.sidetone_max as i32;
        match self.link.call(SIDETONE, 0x00, &[]) {
            Ok(r) => {
                let raw = if r.len() >= 4 { r[3] } else { r.get(1).copied().unwrap_or(0) } as i32;
                let pct = (raw.min(max) * 100 + max / 2) / max.max(1);
                self.state.sidetone = Some(Range { value: pct, min: 0, max: 100, step: 1 });
            }
            Err(e) => self.note_error("sidetone", e),
        }
        match self.link.call(AUTO_SLEEP, 0x00, &[]) {
            Ok(r) => {
                let v = r.first().copied().unwrap_or(0) as i32;
                let mut options = AUTO_OFF_OPTIONS.to_vec();
                if !options.contains(&v) {
                    options.push(v);
                    options.sort_unstable();
                }
                self.state.auto_off = Some(Choice { value: v, options });
            }
            Err(e) => self.note_error("auto power-off", e),
        }
        if let Err(e) = self.read_eq() {
            self.note_error("equalizer", e);
        }
        if self.model.g522 {
            let lights = self.link.feature(LIGHTING).is_ok();
            let prompts = self.link.feature(VOICE_PROMPTS).is_ok();
            let mute = self.link.feature(MIC_MUTE_LED).is_ok();
            // The headset can't report these, so keep what was last chosen.
            let keep = |id: &str, default: bool| -> bool {
                self.state.modes.iter().find(|m| m.id == id).map_or(default, |m| m.value == "on")
            };
            let mut modes = Vec::new();
            if lights {
                modes.push(on_off("lights", "Lighting", "The headset's RGB lighting.", keep("lights", true)));
            }
            if prompts {
                modes.push(on_off("voice_prompts", "Voice prompts", "Spoken announcements in the headset.", keep("voice_prompts", true)));
            }
            if mute {
                modes.push(on_off("mute_led", "Mic mute light", "Light up the microphone while it's muted.", keep("mute_led", true)));
            }
            self.state.modes = modes;
        }
    }

    fn note_error(&mut self, what: &str, e: Error) {
        if matches!(e, Error::Offline | Error::Timeout) {
            self.incomplete = true;
        }
        crate::debug!("{}: {}: {:?}", self.state.name, what, e);
    }

    fn read_eq(&mut self) -> Result<(), Error> {
        if self.model.g522 {
            self.link.feature(PARA_EQ)?;
            // The headset can't report its curve; start from what was last set.
            let gains = self.state.equalizer.as_ref().map_or(vec![0; 10], |e| e.gains.clone());
            self.state.equalizer = Some(Equalizer {
                freqs: G522_EQ_FREQS.to_vec(),
                gains,
                min: -G522_EQ_MAX_DB,
                max: G522_EQ_MAX_DB,
                software: false,
                presets: vec![],
            });
            return Ok(());
        }
        // PRO X 2: range, active slot, then the slot's bands.
        let info = self.link.call(PARA_EQ, 0x00, &[])?;
        if info.len() < 5 {
            return Err(Error::Rejected);
        }
        let slot = self.link.call(PARA_EQ, 0x30, &[0x00])?;
        self.eq_slot = slot.first().copied().unwrap_or(0);
        let bands = self.link.call(PARA_EQ, 0x10, &[0x00, self.eq_slot])?;
        let (mut freqs, mut gains) = (Vec::new(), Vec::new());
        for b in bands.chunks_exact(3) {
            let f = u16::from_be_bytes([b[0], b[1]]);
            if f == 0 {
                break;
            }
            freqs.push(f as u32);
            gains.push(b[2] as i8 as i32);
        }
        if freqs.is_empty() {
            return Err(Error::Rejected);
        }
        self.state.equalizer = Some(Equalizer {
            freqs,
            gains,
            min: info[3] as i8 as i32,
            max: info[4] as i8 as i32,
            software: false,
            presets: vec![],
        });
        Ok(())
    }

    fn write_eq(&mut self, gains: Vec<i32>) -> Result<(), Error> {
        let eq = self.state.equalizer.clone().ok_or(Error::NotSupported(PARA_EQ))?;
        let gains: Vec<i32> = gains.iter().map(|g| (*g).clamp(eq.min, eq.max)).collect();
        if self.model.g522 {
            self.link.call(PARA_EQ, 0x20, &g522_eq_params(&gains))?;
        } else {
            let mut p = vec![0x00, self.eq_slot];
            for (f, g) in eq.freqs.iter().zip(&gains) {
                p.extend_from_slice(&[(f >> 8) as u8, (*f & 0xFF) as u8, *g as i8 as u8]);
            }
            self.link.call(PARA_EQ, 0x20, &p)?;
        }
        if let Some(e) = self.state.equalizer.as_mut() {
            e.gains = gains;
        }
        Ok(())
    }

    fn set_mode(&mut self, id: &str, on: bool) -> Result<(), Error> {
        match id {
            "lights" => self.link.call(LIGHTING, 0x30, if on { &G522_LIGHTS_ON } else { &[0u8; 10] }),
            "voice_prompts" => self.link.call(VOICE_PROMPTS, 0x50, &[0x00, on as u8]),
            "mute_led" => self.link.call(MIC_MUTE_LED, 0x20, &[on as u8]),
            _ => return Err(Error::NotSupported(0)),
        }?;
        if let Some(m) = self.state.modes.iter_mut().find(|m| m.id == id) {
            m.value = if on { "on" } else { "off" }.into();
        }
        Ok(())
    }

    /// Handle frames that arrived on their own. Returns whether the headset
    /// came back on (so settings should be re-applied) and whether anything
    /// changed.
    fn poll(&mut self, timeout_ms: i32) -> (bool, bool) {
        let mut back = false;
        let mut changed = false;
        for e in self.link.poll(timeout_ms) {
            match e {
                Unsolicited::Connected(true) if !self.state.online || self.incomplete => {
                    // Give the headset a moment to finish connecting.
                    std::thread::sleep(Duration::from_millis(500));
                    self.read_all();
                    back = self.state.online;
                    changed = true;
                }
                Unsolicited::Connected(false) if self.state.online => {
                    self.state.online = false;
                    changed = true;
                }
                _ => {}
            }
        }
        (back, changed)
    }
}

impl model::Device for CenturionDevice {
    fn state(&self) -> &DeviceState {
        &self.state
    }

    fn refresh(&mut self) -> model::Result<()> {
        if self.incomplete {
            self.read_all();
            return if self.state.online { Ok(()) } else { Err(model::Error::Offline) };
        }
        match self.link.call(BATTERY, 0x00, &[]) {
            Ok(r) => {
                self.state.battery = decode_battery(&r);
                self.state.online = true;
                Ok(())
            }
            Err(e) => {
                if matches!(e, Error::Offline | Error::Timeout) {
                    self.state.online = false;
                }
                Err(e.into_model("battery"))
            }
        }
    }

    fn apply(&mut self, key: &str, value: &Json) -> model::Result<()> {
        match key {
            "sidetone" => {
                let pct = value.as_i64().ok_or_else(|| model::Error::Invalid("sidetone must be a number".into()))?;
                let pct = pct.clamp(0, 100) as i32;
                let max = self.model.sidetone_max as i32;
                let raw = ((pct * max + 50) / 100) as u8;
                // Newer firmware wants a 0xFF between the microphone and the level.
                let v2 = self.model.g522 || self.link.call(SIDETONE, 0x00, &[]).map_or(true, |r| r.len() >= 4);
                let params: &[u8] = if v2 { &[SIDETONE_MIC, 0xFF, raw] } else { &[SIDETONE_MIC, raw] };
                self.link.call(SIDETONE, 0x10, params).map_err(|e| e.into_model("sidetone"))?;
                if let Some(s) = self.state.sidetone.as_mut() {
                    s.value = pct;
                }
                Ok(())
            }
            "auto_off" => {
                let m = value.as_i64().ok_or_else(|| model::Error::Invalid("minutes must be a number".into()))?;
                let m = m.clamp(0, 255) as u8;
                // The G522 keeps two more bytes that have to be written back.
                let current = self.link.call(AUTO_SLEEP, 0x00, &[]).unwrap_or_default();
                let params = if current.len() >= 3 { vec![m, current[1], current[2]] } else { vec![m] };
                self.link.call(AUTO_SLEEP, 0x10, &params).map_err(|e| e.into_model("auto power-off"))?;
                if let Some(a) = self.state.auto_off.as_mut() {
                    a.value = m as i32;
                    if !a.options.contains(&a.value) {
                        a.options.push(a.value);
                        a.options.sort_unstable();
                    }
                }
                Ok(())
            }
            "equalizer" => {
                let arr = value.as_arr().or_else(|| value.get("gains").as_arr()).ok_or_else(|| {
                    model::Error::Invalid("equalizer expects an array of gains".into())
                })?;
                let n = self.state.equalizer.as_ref().map_or(0, |e| e.gains.len());
                if arr.len() != n {
                    return invalid(format!("expected {} bands", n));
                }
                let gains = arr.iter().map(|g| g.as_i64().unwrap_or(0) as i32).collect();
                self.write_eq(gains).map_err(|e| e.into_model("equalizer"))
            }
            "mode" => {
                let id = value.get("id").as_str().unwrap_or_default().to_string();
                let on = value.get("value").as_str() == Some("on");
                self.set_mode(&id, on).map_err(|e| e.into_model(&id.replace('_', " ")))
            }
            _ => Err(model::Error::NotSupported(key.into())),
        }
    }
}

pub struct CenturionBackend {
    stop: Arc<AtomicBool>,
}

impl Backend for CenturionBackend {
    fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Run one headset: announce it, follow it powering on and off, and keep the
/// battery fresh.
pub fn start(t: Arc<dyn Transport>, model: &'static Model, key: String, sink: Sink) -> CenturionBackend {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let _ = std::thread::Builder::new().name(format!("centurion-{}", key)).spawn(move || {
        let dev = Arc::new(Mutex::new(CenturionDevice::new(t, model)));
        let id = {
            let mut d = dev.lock().unwrap_or_else(|p| p.into_inner());
            d.read_all();
            crate::info!("{}: {} ready ({})", key, d.state.name, if d.state.online { "on" } else { "off" });
            d.state.id.clone()
        };
        let handle: DeviceHandle = dev.clone();
        sink(BackendEvent::Ready(handle.clone()));
        let mut last = Instant::now();
        while !flag.load(Ordering::Relaxed) {
            let (back, changed, snap) = {
                let mut d = dev.lock().unwrap_or_else(|p| p.into_inner());
                let (back, mut changed) = d.poll(150);
                if last.elapsed() >= BATTERY_REFRESH {
                    last = Instant::now();
                    let was = d.state.online;
                    let _ = model::Device::refresh(&mut *d);
                    changed = true;
                    if !was && d.state.online {
                        // Answering again after being off: settings need re-applying.
                        drop(d);
                        sink(BackendEvent::Ready(handle.clone()));
                        std::thread::sleep(Duration::from_millis(350));
                        continue;
                    }
                }
                (back, changed, d.state.to_json())
            };
            if back {
                sink(BackendEvent::Ready(handle.clone()));
            } else if changed {
                sink(BackendEvent::Changed(id.clone(), snap));
            }
            std::thread::sleep(Duration::from_millis(350));
        }
        sink(BackendEvent::Removed(id));
        sink(BackendEvent::Closed(key));
    });
    CenturionBackend { stop }
}

#[cfg(test)]
mod tests {
    use super::*;

    const G522: &[u8] = &[0x50, 0x23];

    fn starts_with(f: &[u8; FRAME], expected: &[u8]) {
        assert_eq!(&f[..expected.len()], expected);
        assert!(f[expected.len()..].iter().all(|b| *b == 0), "zero padding");
    }

    #[test]
    fn frames_match_headsetcontrol() {
        // Root.getFeature(bridge 0x0003), direct, on the G522.
        starts_with(&frame(G522, &[0x00, 0x01, 0x00, 0x03], 0), &[0x50, 0x23, 0x05, 0x00, 0x00, 0x01, 0x00, 0x03]);
        // Bridged Root.getFeature(battery 0x0104) through bridge index 3.
        let p = bridge_payload(0x03, 0x00, 0x00, &[0x01, 0x04]);
        starts_with(&frame(G522, &p, 0), &[0x50, 0x23, 0x0a, 0x00, 0x03, 0x11, 0x00, 0x05, 0x00, 0x00, 0x01, 0x01, 0x04]);
        // Battery read (feature index 2, function 0).
        let p = bridge_payload(0x03, 0x02, 0x00, &[]);
        starts_with(&frame(G522, &p, 0), &[0x50, 0x23, 0x08, 0x00, 0x03, 0x11, 0x00, 0x03, 0x00, 0x02, 0x01]);
        // Auto-sleep 15 minutes, keeping the two other bytes.
        let p = bridge_payload(0x03, 0x14, 0x10, &[0x0f, 0x01, 0x10]);
        starts_with(&frame(G522, &p, 0), &[0x50, 0x23, 0x0b, 0x00, 0x03, 0x11, 0x00, 0x06, 0x00, 0x14, 0x11, 0x0f, 0x01, 0x10]);
        // Voice prompts on.
        let p = bridge_payload(0x03, 0x1a, 0x50, &[0x00, 0x01]);
        starts_with(&frame(G522, &p, 0), &[0x50, 0x23, 0x0a, 0x00, 0x03, 0x11, 0x00, 0x05, 0x00, 0x1a, 0x51, 0x00, 0x01]);
        // The PRO X 2 uses report id 0x51.
        assert_eq!(&frame(&[0x51], &[0x02, 0x11, 0x99], 0)[..6], &[0x51, 4, 0, 0x02, 0x11, 0x99]);
    }

    #[test]
    fn replies_and_battery() {
        let f = frame(&[0x51], &[0x07, 0x10, 0x00, 0x05, 0x00, 0x04, 0x20, 0x2A, 0xF8], 0);
        assert_eq!(payload(&[0x51], &f).unwrap(), vec![0x07, 0x10, 0x00, 0x05, 0x00, 0x04, 0x20, 0x2A, 0xF8]);
        assert_eq!(payload(&[0x50, 0x23], &f), None);
        let b = decode_battery(&[87, 87, 0x02]).unwrap();
        assert_eq!((b.percent, b.charge), (Some(87), Charge::Charging));
        assert_eq!(decode_battery(&[42, 42, 0x00]).unwrap().charge, Charge::Discharging);
        assert_eq!(decode_battery(&[101, 0, 0]), None);
        assert_eq!(connection_event(&[0x03, 0x00, 0x00, 0x01], 0x03), Some(true));
        assert_eq!(connection_event(&[0x03, 0x00, 0x00, 0x00], 0x03), Some(false));
        assert_eq!(connection_event(&[0x03, 0x10, 0x00, 0x01], 0x03), None);
    }

    #[test]
    fn g522_equalizer_matches_g_hub_captures() {
        let flat = g522_eq_params(&[0; 10]);
        assert_eq!(flat.len(), 53);
        // Band 6 (1 kHz) at +3 / -3 / +6 / -6 dB, G HUB's default Q.
        let band6 = |db: i32| {
            let mut g = [0; 10];
            g[5] = db;
            g522_eq_params(&g)[3 + 5 * 5..3 + 6 * 5].to_vec()
        };
        assert_eq!(band6(3), vec![0x03, 0xe8, 0x16, 0x00, 0xb4]);
        assert_eq!(band6(-3), vec![0x03, 0xe8, 0x16, 0x00, 0x3c]);
        assert_eq!(band6(6), vec![0x03, 0xe8, 0x16, 0x00, 0xf0]);
        assert_eq!(band6(-6), vec![0x03, 0xe8, 0x16, 0x00, 0x00]);
        assert_eq!(band6(0), vec![0x03, 0xe8, 0x16, 0x00, 0x78]);
        assert_eq!(band6(12), band6(6), "clamped to the headset's range");
    }
}
