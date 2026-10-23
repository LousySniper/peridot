//! Razer mouse driver and its backend thread.

use super::table::{self as t, LedProto, Model};
use super::*;
use crate::actions::Action;
use crate::backend::{Backend, BackendEvent, DeviceHandle, Sink};
use crate::hid::Transport;
use crate::json::Json;
use crate::model::{
    self, invalid, Battery, Charge, Connection, Control, Device, DeviceState, Direction, Dpi, Effect, Kind, Lighting,
    Polling, Range, Zone,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const REFRESH: Duration = Duration::from_secs(60);
const PROBE_RETRY: Duration = Duration::from_secs(30);

pub struct RazerDevice {
    t: Arc<dyn Transport>,
    pub model: &'static Model,
    tid: u8,
    pub state: DeviceState,
    shift_restore: Option<u16>,
}

/// Mouse buttons we can remap through the input layer (id = evdev code).
pub const REMAPPABLE_BUTTONS: &[(u16, &str, bool)] = &[
    (crate::actions::BTN_LEFT, "Left click", false),
    (crate::actions::BTN_RIGHT, "Right click", true),
    (crate::actions::BTN_MIDDLE, "Middle click", true),
    (crate::actions::BTN_SIDE, "Back", true),
    (crate::actions::BTN_EXTRA, "Forward", true),
];

fn display_name(name: &str) -> String {
    let mut n = name.to_string();
    for suffix in [" (Wired)", " (Wireless)", " (Receiver)", " (USB Receiver)", " (Bluetooth)"] {
        if let Some(stripped) = n.strip_suffix(suffix) {
            n = stripped.to_string();
        }
    }
    n
}

fn pct_to_raw(p: u8) -> u8 {
    ((p.min(100) as u32 * 255 + 50) / 100) as u8
}

fn raw_to_pct(r: u8) -> u8 {
    ((r as u32 * 100 + 127) / 255) as u8
}

impl RazerDevice {
    pub fn probe(t: Arc<dyn Transport>, model: &'static Model, connection: Connection) -> model::Result<RazerDevice> {
        let mut dev = RazerDevice {
            t,
            model,
            tid: model.tid,
            state: DeviceState {
                vendor: "razer",
                kind: Kind::Mouse,
                connection,
                online: true,
                model: format!("1532:{:04x}", model.pid),
                name: display_name(model.name),
                protocol: Some("Razer USB".into()),
                ..Default::default()
            },
            shift_restore: None,
        };
        // Confirm the transaction id; fall back to the common ones if the
        // table's guess gets no answer.
        let mut fw = dev.cmd(get_firmware(), dev.tid);
        if fw.is_err() {
            for tid in [0x1F, 0x3F, 0xFF, 0x08] {
                if tid == dev.tid {
                    continue;
                }
                if let Ok(r) = dev.cmd(get_firmware(), tid) {
                    dev.tid = tid;
                    fw = Ok(r);
                    break;
                }
            }
        }
        let fw = fw?;
        dev.state.firmware = Some(format!("v{}.{}", fw.args[0], fw.args[1]));
        if let Ok(s) = dev.cmd(get_serial(), dev.tid) {
            let serial: String = s.args[..22].iter().take_while(|b| **b != 0).map(|b| *b as char).collect();
            let serial = serial.trim().to_string();
            if !serial.is_empty() && serial.chars().any(|c| c != '0') {
                dev.state.serial = Some(serial);
            }
        }
        dev.state.id = match &dev.state.serial {
            Some(s) => format!("razer-{}", s.to_ascii_lowercase()),
            None => format!("razer-{:04x}", model.pid),
        };
        dev.read_all();
        Ok(dev)
    }

    /// Send one command and read the answer.
    fn cmd(&self, mut r: Report, tid: u8) -> model::Result<Report> {
        r.tid = tid;
        let mut out = [0u8; REPORT_LEN + 1];
        out[1..].copy_from_slice(&r.encode());
        let wait = Duration::from_micros(self.model.wait_us as u64);
        let mut last = model::Error::Device("no response".into());
        for attempt in 0..3 {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(10));
            }
            self.t.set_feature(&out)?;
            std::thread::sleep(wait);
            let mut resp = [0u8; REPORT_LEN + 1];
            self.t.get_feature(&mut resp)?;
            let Some(rep) = Report::decode(&resp[1..]) else { continue };
            if rep.class != r.class || rep.id != r.id || rep.remaining != r.remaining {
                last = model::Error::Device("response does not match request".into());
                continue;
            }
            match rep.status {
                STATUS_OK | STATUS_BUSY => return Ok(rep),
                STATUS_TIMEOUT => return Err(model::Error::Offline),
                STATUS_UNSUPPORTED => return Err(model::Error::NotSupported("command not supported by device".into())),
                STATUS_FAIL => last = model::Error::Device("command failed".into()),
                _ => last = model::Error::Device(format!("unexpected status {}", rep.status)),
            }
        }
        Err(last)
    }

    fn c(&self, r: Report) -> model::Result<Report> {
        self.cmd(r, self.tid)
    }

    fn led_cmd(&self, r: Report) -> model::Result<Report> {
        self.cmd(r, self.model.led_tid)
    }

    fn read_battery(&self) -> model::Result<Option<Battery>> {
        if !self.model.has(t::BATTERY) {
            return Ok(None);
        }
        let raw = self.c(get_battery())?.args[1];
        let charging = self.model.has(t::CHARGING) && self.c(get_charging()).map(|r| r.args[1] == 1).unwrap_or(false);
        let percent = raw_to_pct(raw);
        let charge = if charging && percent >= 100 {
            Charge::Full
        } else if charging {
            Charge::Charging
        } else {
            Charge::Discharging
        };
        Ok(Some(Battery { percent: Some(percent), charge, voltage_mv: None, estimated: false }))
    }

    pub fn read_all(&mut self) {
        match self.read_battery() {
            Ok(b) => self.state.battery = b,
            Err(model::Error::Offline) => {
                self.state.online = false;
                return;
            }
            Err(e) => crate::debug!("{}: battery: {}", self.state.name, e),
        }
        self.state.online = true;
        let m = self.model;
        if m.has(t::DPI) && !m.has(t::DPI_BYTE) {
            let prev = self.state.dpi.clone();
            if let Ok(r) = self.c(get_dpi()) {
                let mut dpi = Dpi {
                    value: r.arg_be16(1),
                    min: 100,
                    max: m.dpi_max,
                    step: 50,
                    max_stages: 5,
                    hardware_stages: m.has(t::DPI_STAGES),
                    ..Default::default()
                };
                if m.has(t::DPI_STAGES) {
                    if let Ok(s) = self.c(get_dpi_stages()) {
                        let count = (s.args[2] as usize).min(5);
                        for i in 0..count {
                            let off = 3 + 7 * i;
                            dpi.stages.push(s.arg_be16(off + 1));
                        }
                        dpi.active_stage = (s.args[1] as usize).saturating_sub(1).min(count.saturating_sub(1));
                    }
                }
                if dpi.stages.is_empty() {
                    match prev {
                        Some(p) if !p.stages.is_empty() => {
                            dpi.stages = p.stages;
                            dpi.active_stage = p.active_stage;
                        }
                        _ => {
                            dpi.stages = vec![400, 800, 1600, 3200].into_iter().filter(|v| *v <= m.dpi_max).collect();
                            dpi.active_stage = dpi.stages.iter().position(|v| *v == dpi.value).unwrap_or(0);
                        }
                    }
                }
                self.state.dpi = Some(dpi);
            }
        }
        if m.has(t::POLL) {
            let value = if m.has(t::POLL2) {
                self.c(get_polling2()).ok().map(|r| polling2_hz(r.args[1]))
            } else {
                self.c(get_polling()).ok().map(|r| polling_hz(r.args[0]))
            };
            if let Some(v) = value {
                let mut options = m.poll_rates.to_vec();
                if !options.contains(&v) {
                    options.push(v);
                    options.sort_unstable();
                }
                self.state.polling = Some(Polling { value: v, options });
            }
        }
        if m.has(t::IDLE) {
            if let Ok(r) = self.c(get_idle()) {
                let v = r.arg_be16(0) as i32;
                self.state.idle_time = Some(Range { value: v.clamp(60, 900), min: 60, max: 900, step: 60 });
            }
        }
        if m.has(t::LOW_BATTERY) {
            if let Ok(r) = self.c(get_low_battery()) {
                let p = raw_to_pct(r.args[0]) as i32;
                self.state.low_battery = Some(Range { value: p.clamp(5, 25), min: 5, max: 25, step: 1 });
            }
        }
        if m.led != LedProto::None && self.state.zones.is_empty() {
            for z in m.zones {
                let mut effects = Vec::new();
                for (bit, e) in [
                    (t::FX_OFF, Effect::Off),
                    (t::FX_STATIC, Effect::Static),
                    (t::FX_BREATH, Effect::Breathing),
                    (t::FX_SPECTRUM, Effect::Spectrum),
                    (t::FX_WAVE, Effect::Wave),
                    (t::FX_REACTIVE, Effect::Reactive),
                ] {
                    if z.effects & bit != 0 {
                        effects.push(e);
                    }
                }
                if m.led == LedProto::Standard {
                    effects.retain(|e| !matches!(e, Effect::Wave | Effect::Reactive));
                }
                if !effects.contains(&Effect::Off) {
                    effects.insert(0, Effect::Off);
                }
                self.state.zones.push(Zone {
                    id: z.id.to_string(),
                    name: z.name.to_string(),
                    effects,
                    state: None,
                    brightness: None,
                    dual_color: m.led != LedProto::Standard,
                });
            }
        }
        let zones: Vec<(usize, u8, bool)> =
            m.zones.iter().enumerate().map(|(i, z)| (i, z.led, z.brightness)).collect();
        for (i, led, has_brightness) in zones {
            if !has_brightness || i >= self.state.zones.len() {
                continue;
            }
            let r = match m.led {
                LedProto::Standard => self.led_cmd(std_get_brightness(led)),
                _ => self.led_cmd(ext_get_brightness(led)),
            };
            if let Ok(r) = r {
                self.state.zones[i].brightness = Some(raw_to_pct(r.args[2]));
            }
        }
        if self.state.controls.is_empty() {
            for (code, name, remappable) in REMAPPABLE_BUTTONS {
                self.state.controls.push(Control {
                    id: format!("{:04x}", code),
                    name: name.to_string(),
                    action: "default".into(),
                    remappable: *remappable,
                });
            }
        }
        self.state.notes = vec!["Button remapping on Razer mice is done in software by Peridot.".into()];
    }

    fn write_dpi(&mut self, v: u16) -> model::Result<u16> {
        let d = self.state.dpi.clone().ok_or_else(|| model::Error::NotSupported("DPI".into()))?;
        let v = d.snap(v);
        self.c(set_dpi(v, v))?;
        if let Some(d) = self.state.dpi.as_mut() {
            d.value = v;
        }
        Ok(v)
    }

    fn write_stages(&mut self, stages: Vec<u16>, active: usize) -> model::Result<()> {
        let hw = self.state.dpi.as_ref().map(|d| d.hardware_stages).unwrap_or(false);
        if hw {
            let pairs: Vec<(u16, u16)> = stages.iter().map(|s| (*s, *s)).collect();
            self.c(set_dpi_stages(&pairs, (active + 1) as u8))?;
        }
        self.write_dpi(stages[active])?;
        if let Some(d) = self.state.dpi.as_mut() {
            d.stages = stages;
            d.active_stage = active;
        }
        Ok(())
    }

    fn write_lighting(&mut self, zone: &str, l: &Lighting) -> model::Result<()> {
        let m = self.model;
        let targets: Vec<usize> = (0..self.state.zones.len())
            .filter(|i| zone == "all" || self.state.zones[*i].id == zone)
            .collect();
        if targets.is_empty() {
            return invalid(format!("unknown lighting zone '{}'", zone));
        }
        let mut applied = 0;
        for i in targets {
            if !self.state.zones[i].effects.contains(&l.effect) {
                if zone == "all" {
                    continue;
                }
                return invalid(format!("{} can't do {}", self.state.zones[i].name, l.effect.as_str()));
            }
            let led = m.zones[i].led;
            let (r, g, b) = (l.color.0, l.color.1, l.color.2);
            match m.led {
                LedProto::Extended => {
                    let mut rep = match l.effect {
                        Effect::Off => ext_effect(led, 0x00, 0x06),
                        Effect::Static => {
                            let mut x = ext_effect(led, 0x01, 0x09);
                            x.args[5] = 1;
                            x.args[6..9].copy_from_slice(&[r, g, b]);
                            x
                        }
                        Effect::Breathing => match l.color2 {
                            Some(c2) => {
                                let mut x = ext_effect(led, 0x02, 0x0C);
                                x.args[3] = 2;
                                x.args[5] = 2;
                                x.args[6..9].copy_from_slice(&[r, g, b]);
                                x.args[9..12].copy_from_slice(&[c2.0, c2.1, c2.2]);
                                x
                            }
                            None => {
                                let mut x = ext_effect(led, 0x02, 0x09);
                                x.args[3] = 1;
                                x.args[5] = 1;
                                x.args[6..9].copy_from_slice(&[r, g, b]);
                                x
                            }
                        },
                        Effect::Spectrum => ext_effect(led, 0x03, 0x06),
                        Effect::Wave => {
                            let mut x = ext_effect(led, 0x04, 0x06);
                            x.args[3] = if l.direction == Direction::Left { 2 } else { 1 };
                            x.args[4] = (0x60 - (l.speed as u32 * 0x50 / 100)) as u8;
                            x
                        }
                        Effect::Reactive => {
                            let mut x = ext_effect(led, 0x05, 0x09);
                            x.args[4] = 1 + (l.speed as u32 * 3 / 100) as u8;
                            x.args[5] = 1;
                            x.args[6..9].copy_from_slice(&[r, g, b]);
                            x
                        }
                        Effect::Ripple => return Err(model::Error::NotSupported("ripple".into())),
                    };
                    rep.tid = m.led_tid;
                    self.led_cmd(rep)?;
                }
                LedProto::MouseExtended => {
                    let rep = match l.effect {
                        Effect::Off => mext_effect(led, 0x00, 0x03),
                        Effect::Static => mext_effect(led, 0x06, 0x06).with_args(&[VARSTORE, led, 0x06, r, g, b]),
                        Effect::Spectrum => mext_effect(led, 0x04, 0x03),
                        Effect::Breathing => match l.color2 {
                            Some(c2) => mext_effect(led, 0x03, 0x0A)
                                .with_args(&[VARSTORE, led, 0x03, 2, r, g, b, c2.0, c2.1, c2.2]),
                            None => mext_effect(led, 0x03, 0x0A).with_args(&[VARSTORE, led, 0x03, 1, r, g, b]),
                        },
                        Effect::Reactive => {
                            let speed = 1 + (l.speed as u32 * 3 / 100) as u8;
                            mext_effect(led, 0x02, 0x07).with_args(&[VARSTORE, led, 0x02, speed, r, g, b])
                        }
                        _ => return Err(model::Error::NotSupported(l.effect.as_str().into())),
                    };
                    self.led_cmd(rep)?;
                }
                LedProto::Standard => match l.effect {
                    Effect::Off => {
                        self.led_cmd(std_led_state(led, false))?;
                    }
                    Effect::Static | Effect::Breathing => {
                        self.led_cmd(std_led_state(led, true))?;
                        self.led_cmd(std_led_rgb(led, r, g, b))?;
                        self.led_cmd(std_led_effect(led, if l.effect == Effect::Static { 0x00 } else { 0x02 }))?;
                    }
                    Effect::Spectrum => {
                        self.led_cmd(std_led_state(led, true))?;
                        self.led_cmd(std_led_effect(led, 0x04))?;
                    }
                    _ => return Err(model::Error::NotSupported(l.effect.as_str().into())),
                },
                LedProto::None => return Err(model::Error::NotSupported("lighting".into())),
            }
            self.state.zones[i].state = Some(l.clone());
            applied += 1;
        }
        if applied == 0 {
            return invalid(format!("no zone supports {}", l.effect.as_str()));
        }
        Ok(())
    }

    fn write_brightness(&mut self, zone: &str, pct: u8) -> model::Result<()> {
        let m = self.model;
        let mut applied = 0;
        for i in 0..self.state.zones.len() {
            if zone != "all" && self.state.zones[i].id != zone {
                continue;
            }
            if !m.zones[i].brightness {
                continue;
            }
            let led = m.zones[i].led;
            let raw = pct_to_raw(pct);
            match m.led {
                LedProto::Standard => self.led_cmd(std_brightness(led, raw))?,
                _ => self.led_cmd(ext_brightness(led, raw))?,
            };
            self.state.zones[i].brightness = Some(pct.min(100));
            applied += 1;
        }
        if applied == 0 {
            return Err(model::Error::NotSupported("brightness for this zone".into()));
        }
        Ok(())
    }
}

impl Device for RazerDevice {
    fn state(&self) -> &DeviceState {
        &self.state
    }

    fn refresh(&mut self) -> model::Result<()> {
        match self.read_battery() {
            Ok(b) => {
                self.state.battery = b;
                if !self.state.online {
                    self.read_all();
                }
                self.state.online = true;
                Ok(())
            }
            Err(model::Error::Offline) => {
                self.state.online = false;
                Err(model::Error::Offline)
            }
            Err(e) => Err(e),
        }
    }

    fn apply(&mut self, key: &str, value: &Json) -> model::Result<()> {
        let num = |v: &Json, what: &str| v.as_i64().ok_or_else(|| model::Error::Invalid(format!("{} must be a number", what)));
        match key {
            "dpi" => {
                let v = self.write_dpi(num(value, "DPI")?.clamp(100, 65535) as u16)?;
                if let Some(d) = self.state.dpi.clone() {
                    if !d.stages.is_empty() {
                        let mut stages = d.stages.clone();
                        let active = d.active_stage.min(stages.len() - 1);
                        stages[active] = v;
                        stages.sort_unstable();
                        stages.dedup();
                        let active = stages.iter().position(|s| *s == v).unwrap_or(0);
                        self.write_stages(stages, active)?;
                    }
                }
                Ok(())
            }
            "dpi_stages" => {
                let d = self.state.dpi.clone().ok_or_else(|| model::Error::NotSupported("DPI".into()))?;
                let (stages, active) = model::parse_stages(value, &d)?;
                self.write_stages(stages, active)
            }
            "dpi_cycle" => {
                let d = self.state.dpi.clone().ok_or_else(|| model::Error::NotSupported("DPI".into()))?;
                let op = value.as_str().unwrap_or("up");
                let idx = model::cycle_stage(&d, op).ok_or_else(|| model::Error::Invalid(op.into()))?;
                self.write_stages(d.stages.clone(), idx)
            }
            "dpi_shift" => {
                let d = self.state.dpi.clone().ok_or_else(|| model::Error::NotSupported("DPI".into()))?;
                if value.as_bool().unwrap_or(false) {
                    if self.shift_restore.is_none() {
                        self.shift_restore = Some(d.value);
                        self.write_dpi(d.stages.iter().copied().min().unwrap_or(400))?;
                    }
                } else if let Some(prev) = self.shift_restore.take() {
                    self.write_dpi(prev)?;
                }
                Ok(())
            }
            "polling_rate" => {
                let hz = num(value, "rate")? as u16;
                let p = self.state.polling.clone().ok_or_else(|| model::Error::NotSupported("polling rate".into()))?;
                if !p.options.contains(&hz) {
                    return invalid(format!("{} Hz is not supported (choose from {:?})", hz, p.options));
                }
                if self.model.has(t::POLL2) {
                    self.c(set_polling2(hz, 0x00))?;
                    if self.model.has(t::POLL2_DUAL) {
                        self.cmd(set_polling2(hz, 0x01), self.model.poll_dual_tid)?;
                    }
                } else {
                    self.c(set_polling(hz))?;
                }
                if let Some(p) = self.state.polling.as_mut() {
                    p.value = hz;
                }
                Ok(())
            }
            "lighting" => {
                let l = Lighting::from_json(value)?;
                let zone = value.get("zone").as_str().unwrap_or("all").to_string();
                self.write_lighting(&zone, &l)
            }
            "brightness" => {
                let zone = value.get("zone").as_str().unwrap_or("all").to_string();
                let v = num(value.get("value"), "brightness")?.clamp(0, 100) as u8;
                self.write_brightness(&zone, v)
            }
            "idle_time" => {
                if self.state.idle_time.is_none() {
                    return Err(model::Error::NotSupported("idle time".into()));
                }
                let s = (num(value, "seconds")?.clamp(60, 900) / 60 * 60) as u16;
                self.c(set_idle(s))?;
                if let Some(r) = self.state.idle_time.as_mut() {
                    r.value = s as i32;
                }
                Ok(())
            }
            "low_battery" => {
                if self.state.low_battery.is_none() {
                    return Err(model::Error::NotSupported("low battery threshold".into()));
                }
                let p = num(value, "percent")?.clamp(5, 25) as u8;
                self.c(set_low_battery(pct_to_raw(p)))?;
                if let Some(r) = self.state.low_battery.as_mut() {
                    r.value = p as i32;
                }
                Ok(())
            }
            "button" => {
                let control = value.get("control").as_str().ok_or_else(|| model::Error::Invalid("missing control".into()))?;
                let action = Action::parse(value.get("action").as_str().unwrap_or("default")).map_err(model::Error::Invalid)?;
                let c = self
                    .state
                    .controls
                    .iter_mut()
                    .find(|c| c.id == control.trim_start_matches("0x"))
                    .ok_or_else(|| model::Error::Invalid(format!("unknown control '{}'", control)))?;
                if !c.remappable && action != Action::Default {
                    return invalid(format!("{} can't be remapped", c.name));
                }
                c.action = action.to_string_canonical();
                Ok(())
            }
            _ => Err(model::Error::NotSupported(format!("setting '{}'", key))),
        }
    }
}

pub struct RazerBackend {
    stop: Arc<AtomicBool>,
}

impl Backend for RazerBackend {
    fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

pub fn start(t: Arc<dyn Transport>, model: &'static Model, key: String, connection: Connection, sink: Sink) -> RazerBackend {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let _ = std::thread::Builder::new().name(format!("razer-{}", key)).spawn(move || {
        let mut handle: Option<Arc<Mutex<RazerDevice>>> = None;
        let mut last_probe: Option<Instant> = None;
        let mut last_refresh = Instant::now();
        while !flag.load(Ordering::Relaxed) {
            if handle.is_none() && last_probe.map_or(true, |p| p.elapsed() > PROBE_RETRY) {
                last_probe = Some(Instant::now());
                match RazerDevice::probe(t.clone(), model, connection) {
                    Ok(dev) => {
                        crate::info!("{}: {} ready (id {})", t.describe(), dev.state.name, dev.state.id);
                        let h = Arc::new(Mutex::new(dev));
                        handle = Some(h.clone());
                        let dyn_handle: DeviceHandle = h;
                        sink(BackendEvent::Ready(dyn_handle));
                    }
                    Err(model::Error::Device(e)) if e.contains("No such device") => break,
                    Err(e) => crate::debug!("{}: probe failed: {}", t.describe(), e),
                }
            }
            if let Some(h) = &handle {
                if last_refresh.elapsed() > REFRESH {
                    last_refresh = Instant::now();
                    let (was_online, result, id, snap) = {
                        let mut d = h.lock().unwrap_or_else(|p| p.into_inner());
                        let was = d.state.online;
                        let r = d.refresh();
                        (was, r, d.state.id.clone(), d.state.to_json())
                    };
                    match result {
                        Err(model::Error::Device(e)) if e.contains("No such device") => break,
                        Ok(()) if !was_online => {
                            let dyn_handle: DeviceHandle = h.clone();
                            sink(BackendEvent::Ready(dyn_handle));
                        }
                        _ => sink(BackendEvent::Changed(id, snap)),
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        if let Some(h) = handle {
            let id = h.lock().unwrap_or_else(|p| p.into_inner()).state.id.clone();
            sink(BackendEvent::Removed(id));
        }
        sink(BackendEvent::Closed(key));
    });
    RazerBackend { stop }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions() {
        assert_eq!(raw_to_pct(255), 100);
        assert_eq!(raw_to_pct(0), 0);
        assert_eq!(pct_to_raw(100), 255);
        assert_eq!(raw_to_pct(pct_to_raw(15)), 15);
        assert_eq!(display_name("Razer DeathAdder V3 Pro (Wireless)"), "Razer DeathAdder V3 Pro");
    }
}
