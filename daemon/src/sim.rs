//! Byte-level simulators for HID++ and Razer devices.
//!
//! They implement [`Transport`], so the real protocol code runs unchanged on
//! top of them. Used by the test-suite.

use crate::hid::Transport;
use crate::hidpp::features as f;
use crate::hidpp::{Message, REPORT_LONG, REPORT_SHORT};
use crate::razer::{self, Report, REPORT_LEN};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

// ---------------------------------------------------------------- HID++ ----

#[derive(Clone, Debug)]
pub struct SimLogi {
    pub name: String,
    pub device_type: u8,
    pub unit_id: [u8; 4],
    pub features: Vec<u16>,
    pub online: bool,
    pub battery: u8,
    pub charging: bool,
    pub voltage: u16,
    pub dpi: u16,
    pub rate_ms: u8,
    pub ext_rate_index: u8,
    pub onboard_mode: u8,
    pub zones: Vec<(u16, Vec<u16>)>,
    pub zone_effects: HashMap<u8, Vec<u8>>,
    pub controls: Vec<(u16, u8)>,
    pub diverted: Vec<u16>,
    pub sidetone: u8,
    pub eq: Vec<i8>,
    /// Reject every equalizer write, like the G733's firmware.
    pub eq_locked: bool,
    /// Directly connected through a dongle that answers for the device's
    /// identity while the device itself is off, and fails the rest with
    /// error 0x05 (the G733 does this).
    pub dongle: bool,
    /// An old device that only speaks HID++ 1.0 (battery in registers).
    pub hidpp10: bool,
    /// Fail this many lighting requests with error 0x05 (a headset still
    /// starting up after being switched on).
    pub led_warmup: u32,
    pub auto_off: u8,
    pub log: Vec<String>,
}

impl SimLogi {
    pub fn gaming_mouse(name: &str, unit: u32) -> SimLogi {
        SimLogi {
            name: name.into(),
            device_type: 3,
            unit_id: unit.to_be_bytes(),
            features: vec![
                f::ROOT,
                f::FEATURE_SET,
                f::DEVICE_FW_VERSION,
                f::DEVICE_NAME,
                f::UNIFIED_BATTERY,
                f::REPROG_CONTROLS_V4,
                f::ADJUSTABLE_DPI,
                f::REPORT_RATE,
                f::RGB_EFFECTS,
                f::ONBOARD_PROFILES,
            ],
            online: true,
            battery: 85,
            charging: false,
            voltage: 3950,
            dpi: 800,
            rate_ms: 1,
            ext_rate_index: 3,
            onboard_mode: 1,
            zones: vec![(1, vec![0x00, 0x01, 0x03, 0x0A]), (2, vec![0x00, 0x01, 0x03, 0x0A])],
            zone_effects: HashMap::new(),
            controls: vec![(0x0050, 0x01), (0x0051, 0x01), (0x0052, 0x31), (0x0053, 0x31), (0x0056, 0x31), (0x00ED, 0x30)],
            diverted: vec![],
            sidetone: 0,
            eq: vec![],
            eq_locked: false,
            dongle: false,
            hidpp10: false,
            led_warmup: 0,
            auto_off: 0,
            log: vec![],
        }
    }

    pub fn office_mouse(name: &str, unit: u32) -> SimLogi {
        let mut m = SimLogi::gaming_mouse(name, unit);
        m.features = vec![
            f::ROOT,
            f::FEATURE_SET,
            f::DEVICE_FW_VERSION,
            f::DEVICE_NAME,
            f::UNIFIED_BATTERY,
            f::REPROG_CONTROLS_V4,
            f::EXTENDED_ADJUSTABLE_DPI,
            f::EXTENDED_ADJUSTABLE_REPORT_RATE,
        ];
        m.battery = 42;
        m.charging = true;
        m.dpi = 1000;
        m.zones.clear();
        m.controls = vec![(0x0050, 0x01), (0x0051, 0x01), (0x0052, 0x31), (0x0053, 0x31), (0x0056, 0x31), (0x00C3, 0x30), (0x00C4, 0x30)];
        m
    }

    pub fn headset(name: &str, unit: u32) -> SimLogi {
        SimLogi {
            name: name.into(),
            device_type: 8,
            unit_id: unit.to_be_bytes(),
            features: vec![
                f::ROOT,
                f::FEATURE_SET,
                f::DEVICE_FW_VERSION,
                f::DEVICE_NAME,
                f::ADC_MEASUREMENT,
                f::COLOR_LED_EFFECTS,
                f::SIDETONE,
                f::EQUALIZER,
            ],
            online: true,
            battery: 0,
            charging: false,
            voltage: 3672,
            dpi: 0,
            rate_ms: 0,
            ext_rate_index: 0,
            onboard_mode: 0,
            zones: vec![(2, vec![0x00, 0x01, 0x02, 0x03]), (1, vec![0x00, 0x01, 0x02, 0x03])],
            zone_effects: HashMap::new(),
            controls: vec![],
            diverted: vec![],
            sidetone: 35,
            eq: vec![0, 2, 0, -1, 3],
            eq_locked: false,
            dongle: false,
            hidpp10: false,
            led_warmup: 0,
            auto_off: 30,
            log: vec![],
        }
    }

    pub fn index_of(&self, id: u16) -> Option<u8> {
        self.features.iter().position(|x| *x == id).map(|p| p as u8)
    }

    /// Answer one HID++ 2.0 request. Err = HID++ 2.0 error code.
    fn respond(&mut self, fi: u8, func: u8, p: &[u8]) -> Result<Vec<u8>, u8> {
        let id = *self.features.get(fi as usize).ok_or(0x06u8)?;
        let pv = |i: usize| p.get(i).copied().unwrap_or(0);
        let be = |i: usize| ((pv(i) as u16) << 8) | pv(i + 1) as u16;
        Ok(match (id, func) {
            (f::ROOT, 0) => match self.index_of(be(0)) {
                Some(i) => vec![i, 0, 0],
                None => vec![0, 0, 0],
            },
            (f::ROOT, 1) => vec![4, 5, pv(2)],
            (f::FEATURE_SET, 0) => vec![(self.features.len() - 1) as u8],
            (f::FEATURE_SET, 1) => {
                let fid = *self.features.get(pv(0) as usize).ok_or(0x02u8)?;
                let ver = if fid == f::ADC_MEASUREMENT { 2 } else { 0 };
                vec![(fid >> 8) as u8, fid as u8, 0, ver]
            }
            (f::DEVICE_FW_VERSION, 0) => {
                let mut r = vec![1];
                r.extend_from_slice(&self.unit_id);
                r
            }
            (f::DEVICE_FW_VERSION, 1) => vec![0, b'R', b'Q', b'M', 0x12, 0x03, 0x00, 0x2A],
            (f::DEVICE_NAME, 0) => vec![self.name.len() as u8],
            (f::DEVICE_NAME, 1) => {
                let off = pv(0) as usize;
                let bytes = self.name.as_bytes();
                bytes[off.min(bytes.len())..(off + 16).min(bytes.len())].to_vec()
            }
            (f::DEVICE_NAME, 2) => vec![self.device_type],
            (f::UNIFIED_BATTERY, 0) => vec![0x0F, 0x01],
            (f::UNIFIED_BATTERY, 1) => {
                let level = if self.battery > 50 { 8 } else if self.battery > 20 { 4 } else { 2 };
                vec![self.battery, level, if self.charging { 1 } else { 0 }, 0]
            }
            (f::ADC_MEASUREMENT, 0) => vec![(self.voltage >> 8) as u8, self.voltage as u8, if self.charging { 3 } else { 1 }],
            (f::ADC_MEASUREMENT, 1) => vec![self.auto_off],
            (f::ADC_MEASUREMENT, 2) => {
                self.auto_off = pv(0);
                vec![self.auto_off]
            }
            (f::ADJUSTABLE_DPI, 0) => vec![1],
            (f::ADJUSTABLE_DPI, 1) => vec![0, 0x00, 0x64, 0xE0, 0x32, 0x64, 0x00, 0x00, 0x00],
            (f::ADJUSTABLE_DPI, 2) => vec![0, (self.dpi >> 8) as u8, self.dpi as u8, 0x03, 0x20],
            (f::ADJUSTABLE_DPI, 3) => {
                if self.onboard_mode == 1 {
                    // On-board mode ignores host DPI on real hardware.
                    return Ok(vec![0, 0, 0]);
                }
                self.dpi = be(1);
                self.log.push(format!("dpi={}", self.dpi));
                vec![0, pv(1), pv(2)]
            }
            (f::EXTENDED_ADJUSTABLE_DPI, 1) => vec![0, 0, 0x01],
            (f::EXTENDED_ADJUSTABLE_DPI, 2) => {
                if pv(2) == 0 {
                    vec![0, 0, 0, 0x00, 0xC8, 0xE0, 0x32, 0x1F, 0x40, 0x00, 0x00]
                } else {
                    vec![0, 0, pv(2), 0, 0]
                }
            }
            (f::EXTENDED_ADJUSTABLE_DPI, 5) => {
                vec![0, (self.dpi >> 8) as u8, self.dpi as u8, 0, 0, (self.dpi >> 8) as u8, self.dpi as u8, 0, 0, 1]
            }
            (f::EXTENDED_ADJUSTABLE_DPI, 6) => {
                self.dpi = be(1);
                self.log.push(format!("dpi={}", self.dpi));
                p.to_vec()
            }
            (f::REPORT_RATE, 0) => vec![0x8B],
            (f::REPORT_RATE, 1) => vec![self.rate_ms],
            (f::REPORT_RATE, 2) => {
                if ![1, 2, 4, 8].contains(&pv(0)) {
                    return Err(0x02);
                }
                self.rate_ms = pv(0);
                self.log.push(format!("rate_ms={}", self.rate_ms));
                vec![]
            }
            (f::EXTENDED_ADJUSTABLE_REPORT_RATE, 1) => vec![0x00, 0x0F],
            (f::EXTENDED_ADJUSTABLE_REPORT_RATE, 2) => vec![self.ext_rate_index],
            (f::EXTENDED_ADJUSTABLE_REPORT_RATE, 3) => {
                self.ext_rate_index = pv(0);
                vec![]
            }
            (f::ONBOARD_PROFILES, 1) => {
                self.onboard_mode = pv(0);
                self.log.push(format!("onboard={}", self.onboard_mode));
                vec![]
            }
            (f::ONBOARD_PROFILES, 2) => vec![self.onboard_mode],
            (f::RGB_EFFECTS, 0) => {
                let (z, e) = (pv(0), pv(1));
                if z == 0xFF {
                    vec![0xFF, 0xFF, self.zones.len() as u8, 0, 0, 0, 0]
                } else {
                    let (loc, effects) = self.zones.get(z as usize).ok_or(0x02u8)?;
                    if e == 0xFF {
                        vec![z, 0xFF, (loc >> 8) as u8, *loc as u8, effects.len() as u8, 0]
                    } else {
                        let id = *effects.get(e as usize).ok_or(0x02u8)?;
                        vec![z, e, (id >> 8) as u8, id as u8, 0, 0, 0x0B, 0xB8]
                    }
                }
            }
            (f::RGB_EFFECTS, 1) | (f::COLOR_LED_EFFECTS, 3) => {
                let z = pv(0);
                let (_, effects) = self.zones.get(z as usize).ok_or(0x02u8)?;
                if pv(1) as usize >= effects.len() {
                    return Err(0x02);
                }
                self.zone_effects.insert(z, p.to_vec());
                self.log.push(format!("zone{}=effect{}", z, pv(1)));
                vec![]
            }
            (f::RGB_EFFECTS, 5) | (f::COLOR_LED_EFFECTS, 8) => vec![],
            (f::COLOR_LED_EFFECTS, 0) => vec![self.zones.len() as u8, 0, 0, 0, 0],
            (f::COLOR_LED_EFFECTS, 1) => {
                let (loc, effects) = self.zones.get(pv(0) as usize).ok_or(0x02u8)?;
                vec![pv(0), (loc >> 8) as u8, *loc as u8, effects.len() as u8]
            }
            (f::COLOR_LED_EFFECTS, 2) => {
                let (_, effects) = self.zones.get(pv(0) as usize).ok_or(0x02u8)?;
                let id = *effects.get(pv(1) as usize).ok_or(0x02u8)?;
                vec![pv(0), pv(1), (id >> 8) as u8, id as u8, 0, 0, 0, 0]
            }
            (f::REPROG_CONTROLS_V4, 0) => vec![self.controls.len() as u8],
            (f::REPROG_CONTROLS_V4, 1) => {
                let (cid, flags) = *self.controls.get(pv(0) as usize).ok_or(0x02u8)?;
                vec![(cid >> 8) as u8, cid as u8, 0, 0, flags, pv(0), 1, 1, 0]
            }
            (f::REPROG_CONTROLS_V4, 3) => {
                let cid = be(0);
                if pv(2) & 0x02 != 0 {
                    if pv(2) & 0x01 != 0 {
                        if !self.diverted.contains(&cid) {
                            self.diverted.push(cid);
                        }
                    } else {
                        self.diverted.retain(|c| *c != cid);
                    }
                }
                self.log.push(format!("divert {:04x}={}", cid, pv(2) & 1));
                p[..5.min(p.len())].to_vec()
            }
            (f::SIDETONE, 0) => vec![self.sidetone],
            (f::SIDETONE, 1) => {
                self.sidetone = pv(0).min(100);
                vec![self.sidetone]
            }
            (f::EQUALIZER, 0) => vec![self.eq.len() as u8, 12, 0, (-12i8) as u8, 12],
            (f::EQUALIZER, 1) => {
                let freqs = [100u16, 300, 1000, 3000, 10000];
                let mut r = vec![pv(0)];
                for fq in freqs.iter().skip(pv(0) as usize).take(7) {
                    r.extend_from_slice(&fq.to_be_bytes());
                }
                r
            }
            (f::EQUALIZER, 2) => self.eq.iter().map(|g| *g as u8).collect(),
            (f::EQUALIZER, 3) => {
                if pv(0) != 0x02 || self.eq_locked {
                    return Err(0x02);
                }
                for (i, g) in self.eq.iter_mut().enumerate() {
                    *g = pv(1 + i) as i8;
                }
                vec![]
            }
            _ => return Err(0x07),
        })
    }
}

struct SimNodeInner {
    devices: HashMap<u8, SimLogi>,
    queue: VecDeque<Vec<u8>>,
}

/// Features a G733-style dongle answers for while its headset is off.
fn is_identity(feature: Option<u16>) -> bool {
    matches!(feature, Some(f::ROOT | f::FEATURE_SET | f::DEVICE_FW_VERSION | f::DEVICE_NAME))
}

/// A simulated HID++ hidraw node: a receiver or a direct device (index 0xFF).
pub struct SimHidppNode {
    pub receiver: bool,
    pub label: String,
    inner: Mutex<SimNodeInner>,
    cond: Condvar,
    closed: AtomicBool,
}

impl SimHidppNode {
    pub fn new(label: &str, receiver: bool, devices: Vec<(u8, SimLogi)>) -> Arc<SimHidppNode> {
        Arc::new(SimHidppNode {
            receiver,
            label: label.into(),
            inner: Mutex::new(SimNodeInner { devices: devices.into_iter().collect(), queue: VecDeque::new() }),
            cond: Condvar::new(),
            closed: AtomicBool::new(false),
        })
    }

    fn push(&self, inner: &mut SimNodeInner, data: Vec<u8>) {
        inner.queue.push_back(data);
        self.cond.notify_all();
    }

    pub fn with_device<R>(&self, idx: u8, f: impl FnOnce(&mut SimLogi) -> R) -> Option<R> {
        let mut inner = self.inner.lock().unwrap();
        inner.devices.get_mut(&idx).map(f)
    }

    /// Simulate the device's diverted-button report (up to four held cids).
    pub fn press(&self, idx: u8, held: &[u16]) {
        let mut inner = self.inner.lock().unwrap();
        let Some(dev) = inner.devices.get(&idx) else { return };
        let Some(fi) = dev.index_of(f::REPROG_CONTROLS_V4) else { return };
        let mut params = Vec::new();
        for c in held.iter().take(4) {
            params.extend_from_slice(&c.to_be_bytes());
        }
        let m = Message { report: REPORT_LONG, device: idx, sub: fi, address: 0x00, params };
        self.push(&mut inner, m.encode());
    }

    /// Simulate the device going to sleep or waking up (receiver notification).
    pub fn set_online(&self, idx: u8, online: bool) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(d) = inner.devices.get_mut(&idx) {
            d.online = online;
        }
        let m = Message {
            report: REPORT_SHORT,
            device: idx,
            sub: 0x41,
            address: 0x04,
            params: vec![if online { 0x02 } else { 0x42 }, 0x93, 0x40],
        };
        self.push(&mut inner, m.encode());
    }

    /// Push a battery broadcast like a real device does.
    pub fn battery_event(&self, idx: u8, percent: u8, charging: bool) {
        let mut inner = self.inner.lock().unwrap();
        let Some(dev) = inner.devices.get_mut(&idx) else { return };
        dev.battery = percent;
        dev.charging = charging;
        let (fi, params) = if let Some(fi) = dev.index_of(f::UNIFIED_BATTERY) {
            (fi, vec![percent, 4, charging as u8, 0])
        } else if let Some(fi) = dev.index_of(f::ADC_MEASUREMENT) {
            let mv = dev.voltage.to_be_bytes();
            (fi, vec![mv[0], mv[1], 0x01 | if charging { 0x02 } else { 0 }])
        } else {
            return;
        };
        let m = Message { report: REPORT_LONG, device: idx, sub: fi, address: 0x00, params };
        self.push(&mut inner, m.encode());
    }

    pub fn unplug(&self) {
        self.closed.store(true, Ordering::Relaxed);
        self.cond.notify_all();
    }
}

impl Transport for SimHidppNode {
    fn write(&self, data: &[u8]) -> io::Result<()> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(io::Error::from_raw_os_error(19));
        }
        let Some(m) = Message::decode(data) else { return Ok(()) };
        let mut inner = self.inner.lock().unwrap();
        let reply_report = |len: usize| if len > 3 { REPORT_LONG } else { REPORT_SHORT };
        // Receiver registers (HID++ 1.0)
        if m.device == 0xFF && self.receiver {
            match m.sub {
                0x80 | 0x81 | 0x82 | 0x83 => {
                    let mut r = m.clone();
                    if m.sub == 0x83 {
                        r.report = REPORT_LONG;
                        let sub = m.param(0);
                        if m.address == 0xB5 && (0x40..0x46).contains(&sub) {
                            if let Some(d) = inner.devices.get(&(sub - 0x40 + 1)) {
                                let name = d.name.as_bytes();
                                let n = name.len().min(14);
                                r.params = vec![sub, n as u8];
                                r.params.extend_from_slice(&name[..n]);
                            }
                        } else if m.address == 0xB5 && (0x20..0x26).contains(&sub) {
                            // Pairing info; byte 7 is the device kind (2 = mouse).
                            if let Some(d) = inner.devices.get(&(sub - 0x20 + 1)) {
                                r.params = vec![sub, 0, 0, 0x10, 0x1b, 0, 0, if d.hidpp10 { 2 } else { 0 }];
                            }
                        } else if m.address == 0xB5 && (0x30..0x36).contains(&sub) {
                            if let Some(d) = inner.devices.get(&(sub - 0x30 + 1)) {
                                r.params = vec![sub];
                                r.params.extend_from_slice(&d.unit_id);
                            }
                        }
                    }
                    self.push(&mut inner, r.encode());
                    if m.sub == 0x80 && m.address == 0x02 {
                        let idxs: Vec<(u8, bool)> = inner.devices.iter().map(|(i, d)| (*i, d.online)).collect();
                        for (i, online) in idxs {
                            let n = Message {
                                report: REPORT_SHORT,
                                device: i,
                                sub: 0x41,
                                address: 0x04,
                                params: vec![if online { 0x02 } else { 0x42 }, 0x93, 0x40],
                            };
                            self.push(&mut inner, n.encode());
                        }
                    }
                }
                _ => {
                    let e = Message { report: REPORT_SHORT, device: 0xFF, sub: 0x8F, address: m.sub, params: vec![m.address, 0x01] };
                    self.push(&mut inner, e.encode());
                }
            }
            return Ok(());
        }
        let reply = match inner.devices.get_mut(&m.device) {
            None => Message {
                report: REPORT_SHORT,
                device: m.device,
                sub: 0x8F,
                address: m.sub,
                params: vec![m.address, if self.receiver { 0x08 } else { 0x01 }],
            },
            Some(d) if !d.online && d.dongle => {
                // Like the G733's dongle: it answers for the headset's identity
                // and fails everything else with error 0x05.
                let result = if is_identity(d.features.get(m.sub as usize).copied()) {
                    d.respond(m.sub, m.function(), &m.params)
                } else {
                    Err(0x05)
                };
                match result {
                    Ok(params) => Message { report: reply_report(params.len()).max(m.report), device: m.device, sub: m.sub, address: m.address, params },
                    Err(code) => Message { report: REPORT_LONG, device: m.device, sub: 0xFF, address: m.sub, params: vec![m.address, code] },
                }
            }
            Some(d) if d.online && d.hidpp10 => match (m.sub, m.address) {
                // Register 0x0D: percent, ?, 0x30 = discharging
                (0x81, 0x0D) => Message { report: REPORT_SHORT, device: m.device, sub: 0x81, address: 0x0D, params: vec![d.battery, 0, 0x30] },
                _ => Message { report: REPORT_SHORT, device: m.device, sub: 0x8F, address: m.sub, params: vec![m.address, 0x01] },
            },
            Some(d) if !d.online => {
                if self.receiver {
                    Message { report: REPORT_SHORT, device: m.device, sub: 0x8F, address: m.sub, params: vec![m.address, 0x09] }
                } else {
                    return Ok(()); // a powered-off direct device just doesn't answer
                }
            }
            Some(d) if d.led_warmup > 0 && d.features.get(m.sub as usize) == Some(&f::COLOR_LED_EFFECTS) => {
                d.led_warmup -= 1;
                Message { report: REPORT_LONG, device: m.device, sub: 0xFF, address: m.sub, params: vec![m.address, 0x05] }
            }
            Some(d) => match d.respond(m.sub, m.function(), &m.params) {
                Ok(params) => Message { report: reply_report(params.len()).max(m.report), device: m.device, sub: m.sub, address: m.address, params },
                Err(code) => Message { report: REPORT_LONG, device: m.device, sub: 0xFF, address: m.sub, params: vec![m.address, code] },
            },
        };
        self.push(&mut inner, reply.encode());
        Ok(())
    }

    fn read_timeout(&self, buf: &mut [u8], timeout_ms: i32) -> io::Result<usize> {
        let mut inner = self.inner.lock().unwrap();
        if inner.queue.is_empty() {
            let (g, _) = self.cond.wait_timeout(inner, Duration::from_millis(timeout_ms.max(0) as u64)).unwrap();
            inner = g;
        }
        if self.closed.load(Ordering::Relaxed) {
            return Err(io::Error::from_raw_os_error(19));
        }
        match inner.queue.pop_front() {
            Some(d) => {
                let n = d.len().min(buf.len());
                buf[..n].copy_from_slice(&d[..n]);
                Ok(n)
            }
            None => Ok(0),
        }
    }

    fn set_feature(&self, _data: &[u8]) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "no feature reports"))
    }

    fn get_feature(&self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "no feature reports"))
    }

    fn describe(&self) -> String {
        format!("sim:{}", self.label)
    }
}

// ---------------------------------------------------------------- Razer ----

/// Key under which [`SimRazerState::effects`] keeps standard-matrix effects.
pub const STD_MATRIX: u8 = 0xFE;

#[derive(Clone, Debug)]
pub struct SimRazerState {
    pub pid: u16,
    pub serial: String,
    pub battery_raw: u8,
    pub charging: bool,
    pub asleep: bool,
    pub dpi: (u16, u16),
    pub stages: Vec<u16>,
    pub active: u8,
    pub poll_code: u8,
    pub poll2_code: u8,
    pub idle: u16,
    pub low_raw: u8,
    pub brightness: HashMap<u8, u8>,
    pub effects: HashMap<u8, Vec<u8>>,
    pub log: Vec<String>,
}

pub struct SimRazer {
    pub state: Mutex<SimRazerState>,
    pending: Mutex<Option<[u8; REPORT_LEN]>>,
    closed: AtomicBool,
}

impl SimRazer {
    pub fn new(pid: u16, serial: &str) -> Arc<SimRazer> {
        Arc::new(SimRazer {
            state: Mutex::new(SimRazerState {
                pid,
                serial: serial.into(),
                battery_raw: 163,
                charging: false,
                asleep: false,
                dpi: (1600, 1600),
                stages: vec![400, 800, 1600, 3200],
                active: 3,
                poll_code: 0x01,
                poll2_code: 0x08,
                idle: 300,
                low_raw: 0x26,
                brightness: HashMap::new(),
                effects: HashMap::new(),
                log: vec![],
            }),
            pending: Mutex::new(None),
            closed: AtomicBool::new(false),
        })
    }

    pub fn unplug(&self) {
        self.closed.store(true, Ordering::Relaxed);
    }

    fn handle(&self, req: &Report) -> Report {
        let mut s = self.state.lock().unwrap();
        let mut r = req.clone();
        r.status = razer::STATUS_OK;
        let model = razer::find_model(s.pid);
        let tid_ok = model.map_or(true, |m| req.tid == m.tid || req.tid == m.led_tid || req.tid == m.poll_dual_tid);
        if !tid_ok {
            r.status = razer::STATUS_FAIL;
            return r;
        }
        if s.asleep && req.class != 0x00 {
            r.status = razer::STATUS_TIMEOUT;
            return r;
        }
        let a = req.args;
        match (req.class, req.id) {
            (0x00, 0x81) => {
                r.args[0] = 1;
                r.args[1] = 4;
            }
            (0x00, 0x82) => {
                for (i, b) in s.serial.bytes().take(22).enumerate() {
                    r.args[i] = b;
                }
            }
            (0x07, 0x80) => r.args[1] = s.battery_raw,
            (0x07, 0x84) => r.args[1] = s.charging as u8,
            (0x04, 0x85) => {
                r.args[1..3].copy_from_slice(&s.dpi.0.to_be_bytes());
                r.args[3..5].copy_from_slice(&s.dpi.1.to_be_bytes());
            }
            (0x04, 0x05) => {
                s.dpi = (u16::from_be_bytes([a[1], a[2]]), u16::from_be_bytes([a[3], a[4]]));
                let d = s.dpi.0;
                s.log.push(format!("dpi={}", d));
            }
            (0x04, 0x86) => {
                r.args[1] = s.active;
                r.args[2] = s.stages.len() as u8;
                for (i, v) in s.stages.iter().enumerate() {
                    let off = 3 + 7 * i;
                    r.args[off] = i as u8;
                    r.args[off + 1..off + 3].copy_from_slice(&v.to_be_bytes());
                    r.args[off + 3..off + 5].copy_from_slice(&v.to_be_bytes());
                }
            }
            (0x04, 0x06) => {
                s.active = a[1];
                let n = a[2] as usize;
                s.stages = (0..n).map(|i| u16::from_be_bytes([a[3 + 7 * i + 1], a[3 + 7 * i + 2]])).collect();
                let (active, stages) = (s.active, s.stages.clone());
                s.log.push(format!("stages={:?} active={}", stages, active));
            }
            (0x00, 0x85) => r.args[0] = s.poll_code,
            (0x00, 0x05) => {
                s.poll_code = a[0];
                s.log.push(format!("poll={}", a[0]));
            }
            (0x00, 0xC0) => r.args[1] = s.poll2_code,
            (0x00, 0x40) => {
                s.poll2_code = a[1];
                s.log.push(format!("poll2={}:{}", a[0], a[1]));
            }
            (0x07, 0x83) => r.args[0..2].copy_from_slice(&s.idle.to_be_bytes()),
            (0x07, 0x03) => s.idle = u16::from_be_bytes([a[0], a[1]]),
            (0x07, 0x81) => r.args[0] = s.low_raw,
            (0x07, 0x01) => s.low_raw = a[0],
            (0x0F, 0x84) | (0x03, 0x83) => r.args[2] = *s.brightness.get(&a[1]).unwrap_or(&200),
            (0x0F, 0x04) | (0x03, 0x03) => {
                s.brightness.insert(a[1], a[2]);
            }
            (0x0F, 0x02) | (0x03, 0x0D) | (0x03, 0x00) | (0x03, 0x01) | (0x03, 0x02) => {
                s.effects.insert(a[1], a[..12].to_vec());
                s.log.push(format!("led{:02x}=fx{:02x}", a[1], a[2]));
            }
            (0x03, 0x0A) => {
                // Standard matrix effects have no LED argument.
                s.effects.insert(STD_MATRIX, a[..8].to_vec());
                s.log.push(format!("matrix=fx{:02x}", a[0]));
            }
            _ => r.status = razer::STATUS_UNSUPPORTED,
        }
        r
    }
}

impl Transport for SimRazer {
    fn write(&self, _data: &[u8]) -> io::Result<()> {
        Ok(())
    }

    fn read_timeout(&self, _buf: &mut [u8], timeout_ms: i32) -> io::Result<usize> {
        std::thread::sleep(Duration::from_millis(timeout_ms.max(0) as u64));
        Ok(0)
    }

    fn set_feature(&self, data: &[u8]) -> io::Result<()> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(io::Error::from_raw_os_error(19));
        }
        let Some(req) = Report::decode(&data[1..]) else {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "short report"));
        };
        if razer::checksum(&data[1..]) != data[1 + 88] {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "bad checksum"));
        }
        let resp = self.handle(&req).encode();
        *self.pending.lock().unwrap() = Some(resp);
        Ok(())
    }

    fn get_feature(&self, buf: &mut [u8]) -> io::Result<usize> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(io::Error::from_raw_os_error(19));
        }
        let resp = self.pending.lock().unwrap().take().unwrap_or([0u8; REPORT_LEN]);
        buf[0] = 0;
        buf[1..1 + REPORT_LEN].copy_from_slice(&resp);
        Ok(REPORT_LEN + 1)
    }

    fn describe(&self) -> String {
        format!("sim:razer-{:04x}", self.state.lock().unwrap().pid)
    }
}

// ---------------------------------------------------------------------------
// Logitech Centurion (G522 framing): a dongle with a bridge at index 3 and a
// headset behind it, answering like HeadsetControl's scripted tests.

pub struct SimCenturionState {
    /// The headset is on (the dongle always answers).
    pub online: bool,
    pub battery: u8,
    pub sidetone: u8,
    pub auto_sleep: [u8; 3],
    pub eq: Vec<u8>,
    pub lights: Vec<u8>,
    pub voice_prompts: Option<bool>,
    queue: VecDeque<Vec<u8>>,
}

pub struct SimCenturion {
    pub state: Mutex<SimCenturionState>,
}

const CENT_PREFIX: [u8; 2] = [0x50, 0x23];
const CENT_BRIDGE: u8 = 0x03;
/// (feature id, index in the headset)
const CENT_FEATURES: [(u16, u8); 7] =
    [(0x0104, 0x02), (0x020d, 0x05), (0x0604, 0x0d), (0x0621, 0x12), (0x0108, 0x14), (0x060b, 0x1a), (0x0601, 0x1b)];

impl SimCenturion {
    pub fn new(online: bool) -> Arc<SimCenturion> {
        Arc::new(SimCenturion {
            state: Mutex::new(SimCenturionState {
                online,
                battery: 80,
                sidetone: 4,
                auto_sleep: [30, 0x01, 0x10],
                eq: vec![],
                lights: vec![],
                voice_prompts: None,
                queue: VecDeque::new(),
            }),
        })
    }

    fn reply(s: &mut SimCenturionState, payload: &[u8]) {
        s.queue.push_back(crate::centurion::frame(&CENT_PREFIX, payload, 0).to_vec());
    }

    /// The headset switches on and the dongle announces it.
    pub fn power_on(&self) {
        let mut s = self.state.lock().unwrap();
        s.online = true;
        Self::reply(&mut s, &[CENT_BRIDGE, 0x00, 0x00, 0x01]);
    }

    fn headset(s: &mut SimCenturionState, feature: u8, function: u8, params: &[u8]) -> Option<Vec<u8>> {
        let id = CENT_FEATURES.iter().find(|(_, i)| *i == feature).map(|(id, _)| *id);
        Some(match (feature, id, function) {
            (0x00, _, 0x00) => {
                let wanted = u16::from_be_bytes([params[0], params[1]]);
                vec![CENT_FEATURES.iter().find(|(id, _)| *id == wanted).map_or(0, |(_, i)| *i), 0x00, 0x01]
            }
            (_, Some(0x0104), 0x00) => vec![s.battery, s.battery, 0x00],
            (_, Some(0x0604), 0x00) => vec![0x01, 0x01, 0x39, s.sidetone],
            (_, Some(0x0604), 0x10) => {
                s.sidetone = *params.last()?;
                vec![]
            }
            (_, Some(0x0108), 0x00) => s.auto_sleep.to_vec(),
            (_, Some(0x0108), 0x10) => {
                s.auto_sleep.copy_from_slice(params.get(..3)?);
                vec![]
            }
            (_, Some(0x020d), 0x20) => {
                s.eq = params.to_vec();
                vec![]
            }
            (_, Some(0x0621), 0x30) => {
                s.lights = params.to_vec();
                vec![]
            }
            (_, Some(0x060b), 0x50) => {
                s.voice_prompts = Some(params.get(1) == Some(&1));
                vec![]
            }
            (_, Some(0x0601), 0x20) => vec![],
            _ => return None,
        })
    }
}

impl Transport for SimCenturion {
    fn write(&self, data: &[u8]) -> io::Result<()> {
        let mut s = self.state.lock().unwrap();
        let Some(p) = crate::centurion::payload(&CENT_PREFIX, data) else { return Ok(()) };
        let (index, fsw) = (p[0], p[1]);
        if index == 0x00 && fsw == 0x01 {
            // Root.getFeature on the dongle: only the bridge exists.
            let id = u16::from_be_bytes([p[2], p[3]]);
            Self::reply(&mut s, &[0x00, 0x01, if id == 0x0003 { CENT_BRIDGE } else { 0 }, 0x00, 0x01]);
        } else if index == CENT_BRIDGE && fsw == 0x01 {
            // Bridge status: [?, ?, connected, connected]
            let on = s.online as u8;
            Self::reply(&mut s, &[CENT_BRIDGE, 0x01, 0x00, on, on, on]);
        } else if index == CENT_BRIDGE && fsw == 0x11 {
            Self::reply(&mut s, &[CENT_BRIDGE, 0x11]); // ACK
            if !s.online {
                return Ok(()); // the headset never answers
            }
            let (feature, function, params) = (p[5], p[6] & 0xF0, p[7..].to_vec());
            match Self::headset(&mut s, feature, function, &params) {
                Some(answer) => {
                    let size = (answer.len() + 3) as u8;
                    let mut r = vec![CENT_BRIDGE, 0x10, 0x00, size, 0x00, feature, function | 0x01];
                    r.extend_from_slice(&answer);
                    Self::reply(&mut s, &r);
                }
                None => Self::reply(&mut s, &[CENT_BRIDGE, 0x10, 0x00, 0x05, 0x00, 0xFF, feature, function, 0x02]),
            }
        }
        Ok(())
    }

    fn read_timeout(&self, buf: &mut [u8], _timeout_ms: i32) -> io::Result<usize> {
        let mut s = self.state.lock().unwrap();
        match s.queue.pop_front() {
            Some(f) => {
                let n = f.len().min(buf.len());
                buf[..n].copy_from_slice(&f[..n]);
                Ok(n)
            }
            None => Ok(0),
        }
    }

    fn set_feature(&self, _data: &[u8]) -> io::Result<()> {
        Ok(())
    }

    fn get_feature(&self, _buf: &mut [u8]) -> io::Result<usize> {
        Ok(0)
    }

    fn describe(&self) -> String {
        "sim:centurion".into()
    }
}
