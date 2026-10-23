//! Razer BlackShark V3 X (HyperSpeed dongle 1532:057D, cable 1532:057C).
//!
//! Unlike the other BlackSharks this one uses 64-byte feature reports on
//! report ID 0x07, sent with SET_REPORT and answered through GET_REPORT:
//!
//! ```text
//! [0] 0x07  [1] status (2 = ok)  [2] transaction id  [6] argument count
//! [7] class (0x00 info, 0x07 power, 0x18 audio)  [8] command (get = set | 0x80)
//! [9..] arguments  [62] XOR of bytes 3..=60
//! ```
//!
//! Source: openrazer PR #2899 (razerblackshark_driver.c), validated there
//! against both product ids.

use crate::backend::{Backend, BackendEvent, DeviceHandle, Sink};
use crate::hid::{DescriptorInfo, Transport};
use crate::json::Json;
use crate::model::{
    self, invalid, Battery, Charge, Choice, Connection, Device, DeviceState, Equalizer, Kind, Mode, Range,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const REPORT_ID: u8 = 0x07;
const LEN: usize = 64;
const EQ_FREQS: [u32; 10] = [31, 63, 125, 250, 500, 1000, 2000, 4000, 8000, 16000];
const EQ_MAX: i32 = 6;
const PRESETS: [(&str, &str); 4] = [("0", "Default"), ("1", "Game"), ("2", "Music"), ("3", "Movie")];
const AUTO_OFF: &[i32] = &[0, 15, 30, 45, 60];

const CLASS_INFO: u8 = 0x00;
const CLASS_POWER: u8 = 0x07;
const CLASS_AUDIO: u8 = 0x18;

const SERIAL_GET: u8 = 0x82;
const FIRMWARE_GET: u8 = 0x87;
const BATTERY_GET: u8 = 0x80;
const CHARGING_GET: u8 = 0x84;
const SLEEP_SET: u8 = 0x03;
const SLEEP_GET: u8 = 0x83;
const SAVE_SET: u8 = 0x08;
const SAVE_GET: u8 = 0x88;
const PRESET_SET: u8 = 0x00;
const PRESET_GET: u8 = 0x80;
const EQ_SET: u8 = 0x01;
const EQ_GET: u8 = 0x81;
const TONE_SET: u8 = 0x03;
const TONE_GET: u8 = 0x83;
const MICNC_SET: u8 = 0x0F;
const MICNC_GET: u8 = 0x8F;

pub fn is_model(pid: u16) -> bool {
    matches!(pid, 0x057C | 0x057D)
}

pub fn is_control_interface(d: &DescriptorInfo) -> bool {
    d.has_report_id(REPORT_ID)
}

fn crc(r: &[u8]) -> u8 {
    r[3..=60].iter().fold(0, |x, b| x ^ b)
}

pub fn frame(tid: u8, class: u8, cmd: u8, args: &[u8]) -> [u8; LEN] {
    let mut r = [0u8; LEN];
    r[0] = REPORT_ID;
    r[2] = tid;
    r[6] = args.len() as u8;
    r[7] = class;
    r[8] = cmd;
    r[9..9 + args.len()].copy_from_slice(args);
    r[62] = crc(&r);
    r
}

fn encode(db: i32) -> u8 {
    if db < 0 {
        0x80 | (-db).min(0x7F) as u8
    } else {
        db.min(0x7F) as u8
    }
}

fn decode(b: u8) -> i32 {
    if b & 0x80 != 0 {
        -((b & 0x7F) as i32)
    } else {
        b as i32
    }
}

pub struct BlackSharkX {
    t: Arc<dyn Transport>,
    tid: u8,
    wait: Duration,
    pub state: DeviceState,
}

impl BlackSharkX {
    pub fn new(t: Arc<dyn Transport>, pid: u16) -> BlackSharkX {
        let wireless = pid == 0x057D;
        BlackSharkX {
            t,
            tid: 0,
            wait: Duration::from_millis(50),
            state: DeviceState {
                id: format!("razer-{:04x}", pid),
                name: "Razer BlackShark V3 X".into(),
                vendor: "razer",
                kind: Kind::Headset,
                connection: if wireless { Connection::Receiver } else { Connection::Usb },
                online: false,
                model: format!("1532:{:04x}", pid),
                protocol: Some("Razer audio (feature reports)".into()),
                ..Default::default()
            },
        }
    }

    pub fn with_wait(mut self, wait: Duration) -> BlackSharkX {
        self.wait = wait;
        self
    }

    /// One request/response. Offline when the dongle can't reach the headset.
    fn exchange(&mut self, class: u8, cmd: u8, args: &[u8], min_args: usize) -> model::Result<Vec<u8>> {
        for _ in 0..4 {
            let tid = self.tid;
            self.tid = self.tid.wrapping_add(1);
            let req = frame(tid, class, cmd, args);
            self.t.set_feature(&req).map_err(model::Error::from)?;
            std::thread::sleep(self.wait);
            let mut r = [0u8; LEN];
            r[0] = REPORT_ID;
            let n = self.t.get_feature(&mut r).map_err(model::Error::from)?;
            if n < LEN || r[0] != REPORT_ID || r[2] != tid || r[7] != class || r[8] != cmd || r[62] != crc(&r)
                || (r[6] as usize) < min_args
            {
                std::thread::sleep(self.wait);
                continue; // not ready yet
            }
            return match r[1] {
                0x01 | 0x02 => Ok(r[9..9 + (r[6] as usize).min(52)].to_vec()),
                0x05 => Err(model::Error::NotSupported(format!("command {:02x}:{:02x}", class, cmd))),
                0x04 => Err(model::Error::Offline),
                _ => Err(model::Error::Device(format!("headset refused {:02x}:{:02x}", class, cmd))),
            };
        }
        Err(model::Error::Offline)
    }

    fn get(&mut self, class: u8, cmd: u8) -> model::Result<Vec<u8>> {
        let a = self.exchange(class, cmd, &[0, 0], 1)?;
        // An unlinked headset makes the dongle answer with all 0xFF.
        if !a.is_empty() && a.iter().all(|b| *b == 0xFF) {
            return Err(model::Error::Offline);
        }
        Ok(a)
    }

    fn get_byte(&mut self, class: u8, cmd: u8, index: usize) -> model::Result<u8> {
        self.get(class, cmd)?.get(index).copied().ok_or_else(|| model::Error::Device("short reply".into()))
    }

    fn set(&mut self, class: u8, cmd: u8, args: &[u8]) -> model::Result<()> {
        self.exchange(class, cmd, args, 0)?;
        // A setting takes up to ~200 ms to commit.
        std::thread::sleep(self.wait * 5);
        Ok(())
    }

    fn read_battery(&mut self) -> model::Result<Battery> {
        let pct = self.get_byte(CLASS_POWER, BATTERY_GET, 1)?;
        if pct > 100 {
            return Err(model::Error::Offline);
        }
        let charging = self.get_byte(CLASS_POWER, CHARGING_GET, 1).map(|v| v == 1).unwrap_or(false);
        let charge = match (charging, pct) {
            (true, 100) => Charge::Full,
            (true, _) => Charge::Charging,
            _ => Charge::Discharging,
        };
        Ok(Battery { percent: Some(pct), charge, voltage_mv: None, estimated: false })
    }

    fn read_eq(&mut self) -> model::Result<()> {
        let a = self.get(CLASS_AUDIO, EQ_GET)?;
        if a.len() >= 12 {
            let gains = (0..10).map(|i| decode(a[2 + i]).clamp(-EQ_MAX, EQ_MAX)).collect();
            self.state.equalizer = Some(Equalizer { freqs: EQ_FREQS.to_vec(), gains, min: -EQ_MAX, max: EQ_MAX, software: false, presets: vec![] });
        }
        Ok(())
    }

    fn read_auto_off(&mut self) -> model::Result<()> {
        let saving = self.get_byte(CLASS_POWER, SAVE_GET, 1)?;
        let s = self.get(CLASS_POWER, SLEEP_GET)?;
        let seconds = ((s.first().copied().unwrap_or(0) as u32) << 8) | s.get(1).copied().unwrap_or(0) as u32;
        let minutes = if saving == 0 { 0 } else { ((seconds + 30) / 60) as i32 };
        let mut options = AUTO_OFF.to_vec();
        if !options.contains(&minutes) {
            options.push(minutes);
            options.sort_unstable();
        }
        self.state.auto_off = Some(Choice { value: minutes, options });
        Ok(())
    }

    pub fn read_all(&mut self) -> model::Result<()> {
        let battery = match self.read_battery() {
            Ok(b) => b,
            Err(e) => {
                if e == model::Error::Offline {
                    self.state.online = false;
                }
                return Err(e);
            }
        };
        self.state.battery = Some(battery);
        self.state.online = true;
        if self.state.serial.is_none() {
            if let Ok(s) = self.get(CLASS_INFO, SERIAL_GET) {
                let serial: String =
                    s.iter().take_while(|b| **b != 0).filter(|b| b.is_ascii_graphic()).map(|b| *b as char).collect();
                if serial.len() >= 4 {
                    self.state.id = format!("razer-{}", serial.to_ascii_lowercase());
                    self.state.serial = Some(serial);
                }
            }
            if let Ok(f) = self.get(CLASS_INFO, FIRMWARE_GET) {
                if f.len() >= 3 && f[..3] != [0, 0, 0] {
                    self.state.firmware = Some(format!("{}.{}.{}", f[0], f[1], f[2]));
                }
            }
        }
        if let Ok(v) = self.get_byte(CLASS_AUDIO, TONE_GET, 2) {
            self.state.sidetone = Some(Range { value: (v as i32).min(15), min: 0, max: 15, step: 1 });
        }
        let _ = self.read_eq();
        let _ = self.read_auto_off();
        self.state.modes.clear();
        if let Ok(p) = self.get_byte(CLASS_AUDIO, PRESET_GET, 3) {
            self.state.modes.push(Mode {
                id: "eq_preset".into(),
                name: "Equalizer preset".into(),
                hint: "The equalizer below edits the selected preset.".into(),
                value: p.min(3).to_string(),
                options: PRESETS.iter().map(|(v, l)| (v.to_string(), l.to_string())).collect(),
            });
        }
        if let Ok(v) = self.get_byte(CLASS_AUDIO, MICNC_GET, 1) {
            self.state.modes.push(Mode {
                id: "mic_nc".into(),
                name: "Mic noise cancelling".into(),
                hint: "Filters background noise from your microphone.".into(),
                value: if v == 1 { "on" } else { "off" }.into(),
                options: vec![("off".into(), "Off".into()), ("on".into(), "On".into())],
            });
        }
        Ok(())
    }
}

impl Device for BlackSharkX {
    fn state(&self) -> &DeviceState {
        &self.state
    }

    fn refresh(&mut self) -> model::Result<()> {
        match self.read_battery() {
            Ok(b) => {
                self.state.battery = Some(b);
                self.state.online = true;
                Ok(())
            }
            Err(e) => {
                if e == model::Error::Offline {
                    self.state.online = false;
                }
                Err(e)
            }
        }
    }

    fn apply(&mut self, key: &str, value: &Json) -> model::Result<()> {
        if !self.state.online {
            return Err(model::Error::Offline);
        }
        let num = |v: &Json| v.as_i64().ok_or_else(|| model::Error::Invalid("expected a number".into()));
        match key {
            "sidetone" => {
                let v = num(value)?.clamp(0, 15) as u8;
                self.set(CLASS_AUDIO, TONE_SET, &[0x00, 0x03, v])?;
                if let Some(s) = self.state.sidetone.as_mut() {
                    s.value = v as i32;
                }
                Ok(())
            }
            "equalizer" => {
                let arr = value.as_arr().or_else(|| value.get("gains").as_arr()).ok_or_else(|| {
                    model::Error::Invalid("equalizer expects an array of gains".into())
                })?;
                if arr.len() != EQ_FREQS.len() {
                    return invalid(format!("expected {} bands", EQ_FREQS.len()));
                }
                let gains: Vec<i32> = arr.iter().map(|g| (g.as_i64().unwrap_or(0) as i32).clamp(-EQ_MAX, EQ_MAX)).collect();
                let mut args = vec![0x00, 0x01];
                args.extend(gains.iter().map(|g| encode(*g)));
                self.set(CLASS_AUDIO, EQ_SET, &args)?;
                if let Some(e) = self.state.equalizer.as_mut() {
                    e.gains = gains;
                }
                Ok(())
            }
            "auto_off" => {
                let minutes = num(value)?.clamp(0, 1000) as u32;
                if minutes == 0 {
                    self.set(CLASS_POWER, SAVE_SET, &[0x00, 0x00])?;
                } else {
                    let s = (minutes * 60).min(0xFFFE) as u16;
                    self.set(CLASS_POWER, SLEEP_SET, &s.to_be_bytes())?;
                    self.set(CLASS_POWER, SAVE_SET, &[0x00, 0x01])?;
                }
                let _ = self.read_auto_off();
                Ok(())
            }
            "mode" => {
                let id = value.get("id").as_str().unwrap_or("");
                let v = value.get("value").as_str().unwrap_or("");
                match id {
                    "eq_preset" => {
                        let p: u8 = v.parse().ok().filter(|p| *p <= 3).ok_or_else(|| {
                            model::Error::Invalid(format!("unknown preset '{}'", v))
                        })?;
                        self.set(CLASS_AUDIO, PRESET_SET, &[0x00, 0x01, 0x00, p])?;
                        // Each preset has its own bands.
                        let _ = self.read_eq();
                    }
                    "mic_nc" => {
                        let on = match v {
                            "on" => 1,
                            "off" => 0,
                            _ => return invalid(format!("unknown value '{}'", v)),
                        };
                        self.set(CLASS_AUDIO, MICNC_SET, &[0x00, on])?;
                    }
                    _ => return Err(model::Error::NotSupported(format!("mode '{}'", id))),
                }
                if let Some(m) = self.state.modes.iter_mut().find(|m| m.id == id) {
                    m.value = v.into();
                }
                Ok(())
            }
            _ => Err(model::Error::NotSupported(format!("setting '{}'", key))),
        }
    }
}

pub struct BlackSharkXBackend {
    stop: Arc<AtomicBool>,
}

impl Backend for BlackSharkXBackend {
    fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

pub fn start(t: Arc<dyn Transport>, pid: u16, key: String, sink: Sink) -> BlackSharkXBackend {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let _ = std::thread::Builder::new().name(format!("blackshark-x-{}", key)).spawn(move || {
        let dev = Arc::new(Mutex::new(BlackSharkX::new(t.clone(), pid)));
        let mut announced: Option<String> = None;
        let mut last_poll: Option<Instant> = None;
        while !flag.load(Ordering::Relaxed) {
            let interval = {
                let d = dev.lock().unwrap_or_else(|p| p.into_inner());
                if d.state.online { Duration::from_secs(60) } else { Duration::from_secs(8) }
            };
            if last_poll.map_or(true, |p| p.elapsed() >= interval) {
                last_poll = Some(Instant::now());
                let (was_online, result, id, snap) = {
                    let mut d = dev.lock().unwrap_or_else(|p| p.into_inner());
                    let was = d.state.online;
                    let r = if was { d.refresh() } else { d.read_all() };
                    (was, r, d.state.id.clone(), d.state.to_json())
                };
                if matches!(&result, Err(model::Error::Device(e)) if e.contains("No such device")) {
                    break;
                }
                if let Some(prev) = &announced {
                    if *prev != id {
                        sink(BackendEvent::Removed(prev.clone()));
                        announced = None;
                    }
                }
                match (&announced, result.is_ok(), was_online) {
                    (None, _, _) => {
                        crate::info!("{}: Razer BlackShark V3 X found (id {})", t.describe(), id);
                        let h: DeviceHandle = dev.clone();
                        sink(BackendEvent::Ready(h));
                        announced = Some(id);
                    }
                    (Some(_), true, false) => {
                        let h: DeviceHandle = dev.clone();
                        sink(BackendEvent::Ready(h));
                    }
                    _ => sink(BackendEvent::Changed(id, snap)),
                }
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        if let Some(id) = announced {
            sink(BackendEvent::Removed(id));
        }
        sink(BackendEvent::Closed(key));
    });
    BlackSharkXBackend { stop }
}

pub mod fake {
    //! A V3 X that answers like the real firmware, for tests.
    use super::*;
    use std::collections::HashMap;
    use std::io;

    pub struct FakeX {
        pub linked: Mutex<bool>,
        pub regs: Mutex<HashMap<(u8, u8), Vec<u8>>>,
        last: Mutex<Option<[u8; LEN]>>,
    }

    impl FakeX {
        pub fn new() -> Arc<FakeX> {
            let mut regs = HashMap::new();
            regs.insert((CLASS_POWER, BATTERY_GET), vec![0, 66]);
            regs.insert((CLASS_POWER, CHARGING_GET), vec![0, 0]);
            regs.insert((CLASS_POWER, SAVE_GET), vec![0, 1]);
            regs.insert((CLASS_POWER, SLEEP_GET), vec![0x07, 0x08]); // 1800 s
            regs.insert((CLASS_INFO, SERIAL_GET), b"BSX24070012\0\0".to_vec());
            regs.insert((CLASS_INFO, FIRMWARE_GET), vec![1, 2, 7]);
            regs.insert((CLASS_AUDIO, TONE_GET), vec![0, 3, 5]);
            regs.insert((CLASS_AUDIO, PRESET_GET), vec![0, 1, 0, 1]);
            regs.insert((CLASS_AUDIO, EQ_GET), vec![0, 1, 0, 2, 0x82, 0, 0, 0, 0, 0, 1, 3]);
            regs.insert((CLASS_AUDIO, MICNC_GET), vec![0, 0]);
            Arc::new(FakeX { linked: Mutex::new(true), regs: Mutex::new(regs), last: Mutex::new(None) })
        }
    }

    impl Transport for FakeX {
        fn write(&self, _: &[u8]) -> io::Result<()> {
            Ok(())
        }
        fn read_timeout(&self, _: &mut [u8], _: i32) -> io::Result<usize> {
            Ok(0)
        }
        fn set_feature(&self, d: &[u8]) -> io::Result<()> {
            let mut r = [0u8; LEN];
            r.copy_from_slice(&d[..LEN]);
            let (class, cmd, n) = (r[7], r[8], r[6] as usize);
            let mut regs = self.regs.lock().unwrap();
            let reply: Vec<u8> = if !*self.linked.lock().unwrap() && class != CLASS_INFO {
                vec![0xFF; 4]
            } else if cmd & 0x80 == 0 {
                regs.insert((class, cmd | 0x80), r[9..9 + n].to_vec());
                vec![]
            } else {
                regs.get(&(class, cmd)).cloned().unwrap_or_default()
            };
            r[1] = 0x02;
            r[6] = reply.len() as u8;
            r[9..].fill(0);
            r[9..9 + reply.len()].copy_from_slice(&reply);
            r[62] = crc(&r);
            *self.last.lock().unwrap() = Some(r);
            Ok(())
        }
        fn get_feature(&self, buf: &mut [u8]) -> io::Result<usize> {
            match *self.last.lock().unwrap() {
                Some(r) => {
                    buf[..LEN].copy_from_slice(&r);
                    Ok(LEN)
                }
                None => Ok(0),
            }
        }
        fn describe(&self) -> String {
            "fake-v3x".into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeX;
    use super::*;

    fn open() -> (Arc<FakeX>, BlackSharkX) {
        let f = FakeX::new();
        let t: Arc<dyn Transport> = f.clone();
        (f, BlackSharkX::new(t, 0x057D).with_wait(Duration::from_millis(1)))
    }

    #[test]
    fn frame_layout() {
        let f = frame(5, CLASS_POWER, BATTERY_GET, &[0, 0]);
        assert_eq!(&f[..11], &[0x07, 0, 5, 0, 0, 0, 2, 0x07, 0x80, 0, 0]);
        assert_eq!(f[62], 2 ^ 0x07 ^ 0x80);
    }

    #[test]
    fn reads_and_writes() {
        let (fake, mut d) = open();
        d.read_all().unwrap();
        let s = d.state().clone();
        assert!(s.online);
        assert_eq!(s.id, "razer-bsx24070012");
        assert_eq!(s.firmware.as_deref(), Some("1.2.7"));
        assert_eq!(s.battery.as_ref().unwrap().percent, Some(66));
        assert_eq!(s.sidetone.as_ref().unwrap().value, 5);
        assert_eq!(s.auto_off.as_ref().unwrap().value, 30);
        assert_eq!(s.equalizer.as_ref().unwrap().gains, vec![0, 2, -2, 0, 0, 0, 0, 0, 1, 3]);
        assert_eq!(s.modes[0].value, "1");
        d.apply("sidetone", &Json::Num(9.0)).unwrap();
        d.apply("equalizer", &Json::parse("[1,1,1,1,1,1,1,1,1,-6]").unwrap()).unwrap();
        d.apply("mode", &Json::parse(r#"{"id":"mic_nc","value":"on"}"#).unwrap()).unwrap();
        d.apply("auto_off", &Json::Num(15.0)).unwrap();
        let regs = fake.regs.lock().unwrap();
        assert_eq!(regs[&(CLASS_AUDIO, TONE_GET)], vec![0, 3, 9]);
        assert_eq!(regs[&(CLASS_AUDIO, EQ_GET)][2..], [1, 1, 1, 1, 1, 1, 1, 1, 1, 0x86]);
        assert_eq!(regs[&(CLASS_AUDIO, MICNC_GET)], vec![0, 1]);
        assert_eq!(regs[&(CLASS_POWER, SLEEP_GET)], vec![0x03, 0x84]);
        drop(regs);
        assert_eq!(d.state().auto_off.as_ref().unwrap().value, 15);
    }

    #[test]
    fn unlinked_headset_is_offline() {
        let (fake, mut d) = open();
        *fake.linked.lock().unwrap() = false;
        assert_eq!(d.read_all(), Err(model::Error::Offline));
        assert!(!d.state().online);
    }
}
