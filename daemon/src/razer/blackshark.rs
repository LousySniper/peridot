//! Razer headsets that use the vendor audio protocol instead of the 90-byte
//! report protocol: BlackShark V2 Pro / V2 HyperSpeed / V3 / V3 Pro, Kraken V4
//! and Barracuda.
//!
//! They take 64-byte frames on a vendor HID interface, in one of two framings:
//!
//! - `Pa` (BlackShark V2 Pro 2023, Barracuda): requests carry a "PA" marker and
//!   are wrapped in a "remote mode" switch that routes them to the headset.
//!   Sources: openrazer PR #2862 / blackshark-control (V2 Pro),
//!   nanomad/openbarracuda and TarikTopalovic/razer-barracuda-2.4-linux
//!   (Barracuda), leag/razer-barracuda (Barracuda X).
//! - `Mt` (V2 HyperSpeed, V3, V3 Pro, Kraken V4): frames with a transaction id,
//!   a transport byte (0x80 through a dongle, 0x00 on a cable) and an XOR
//!   checksum. Sources: justik13/razer-blackshark-v2-hyperspeed-webhid,
//!   openrazer PR #2794 (V3 / V3 Pro), HeadsetControl PR #587 (Kraken V4).
//!
//! The V3 family has two quirks worth knowing. Its firmware only enables the
//! command channel if the host's first configuration-descriptor read is 255
//! bytes long, as Windows does (data/usb-quirk turns on the kernel's quirk for
//! that; third_party/razer-cfg255 does it on older kernels); and polling it
//! often drops the radio link, so Peridot reads it rarely and listens for the
//! values the headset pushes on its own.

use crate::backend::{Backend, BackendEvent, DeviceHandle, Sink};
use crate::hid::{DescriptorInfo, Transport};
use crate::json::Json;
use crate::model::{
    self, invalid, Battery, Charge, Choice, Connection, Device, DeviceState, Equalizer, Kind, Mode, Range,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const FRAME: usize = 64;
const MT_REPORT: u8 = 0x02;
const EQ_FREQS: [u32; 10] = [31, 63, 125, 250, 500, 1000, 2000, 4000, 8000, 16000];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    /// "PA" frames on report `rid`. `compact`: the header lacks the two zero
    /// bytes after the length (Barracuda dongles). `wide_len`: the length byte
    /// also counts the two argument-header bytes.
    Pa { rid: u8, compact: bool, wide_len: bool },
    /// Transaction frames; `domain` is the transport byte.
    Mt { domain: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    V2,
    V3,
    V3Pro,
    KrakenV4,
    Barracuda,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EqKind {
    None,
    /// V2 Pro: signed bytes, written through the preset apply sequence.
    PaSigned,
    /// V2 HyperSpeed: signed bytes plus an offset, one write.
    Offset(i32),
    /// V3 family: sign-magnitude bytes stored in this preset slot.
    Slot(u8),
}

#[derive(Clone, Copy, Debug)]
pub struct Model {
    pub pid: u16,
    pub name: &'static str,
    pub family: Family,
    pub flavor: Flavor,
    pub wireless: bool,
    /// USB interface of the control channel, when known.
    pub iface: Option<u8>,
    /// Highest sidetone level; 0 = no sidetone control.
    pub sidetone_max: i32,
    pub eq: EqKind,
    pub eq_min: i32,
    pub eq_max: i32,
    /// Auto power-off choices in minutes; empty = not controllable.
    pub auto_off: &'static [i32],
}

impl Model {
    const fn new(pid: u16, name: &'static str, family: Family, flavor: Flavor, wireless: bool) -> Model {
        Model {
            pid,
            name,
            family,
            flavor,
            wireless,
            iface: None,
            sidetone_max: 0,
            eq: EqKind::None,
            eq_min: 0,
            eq_max: 0,
            auto_off: &[],
        }
    }
    const fn iface(mut self, i: u8) -> Model {
        self.iface = Some(i);
        self
    }
    const fn sidetone(mut self, max: i32) -> Model {
        self.sidetone_max = max;
        self
    }
    const fn eq(mut self, kind: EqKind, min: i32, max: i32) -> Model {
        self.eq = kind;
        self.eq_min = min;
        self.eq_max = max;
        self
    }
    const fn auto_off(mut self, options: &'static [i32]) -> Model {
        self.auto_off = options;
        self
    }

    fn report_id(&self) -> u8 {
        match self.flavor {
            Flavor::Pa { rid, .. } => rid,
            Flavor::Mt { .. } => MT_REPORT,
        }
    }
}

const DONGLE: Flavor = Flavor::Mt { domain: 0x80 };
const CABLE: Flavor = Flavor::Mt { domain: 0x00 };
const PA_V2: Flavor = Flavor::Pa { rid: 0x02, compact: false, wide_len: false };
const PA_BARRACUDA: Flavor = Flavor::Pa { rid: 0x02, compact: false, wide_len: true };
const PA_COMPACT: Flavor = Flavor::Pa { rid: 0x01, compact: true, wide_len: false };

const OFF_V2: &[i32] = &[0, 10, 15, 30, 45, 60];
const OFF_15_60: &[i32] = &[0, 15, 30, 45, 60];
const OFF_V3: &[i32] = &[15, 30, 45, 60];

pub static MODELS: &[Model] = &[
    Model::new(0x0555, "Razer BlackShark V2 Pro", Family::V2, PA_V2, true)
        .sidetone(10)
        .eq(EqKind::PaSigned, -12, 12)
        .auto_off(OFF_15_60),
    Model::new(0x0565, "Razer BlackShark V2 HyperSpeed", Family::V2, DONGLE, true)
        .sidetone(15)
        .eq(EqKind::Offset(5), -9, 6)
        .auto_off(OFF_V2),
    Model::new(0x0566, "Razer BlackShark V2 HyperSpeed", Family::V2, CABLE, false)
        .sidetone(15)
        .eq(EqKind::Offset(5), -9, 6)
        .auto_off(OFF_V2),
    Model::new(0x056E, "Razer BlackShark V2 HyperSpeed", Family::V2, CABLE, false)
        .sidetone(15)
        .eq(EqKind::Offset(5), -9, 6)
        .auto_off(OFF_V2),
    Model::new(0x057A, "Razer BlackShark V3", Family::V3, DONGLE, true)
        .iface(5)
        .sidetone(15)
        .eq(EqKind::Slot(1), -6, 6)
        .auto_off(OFF_V3),
    Model::new(0x0579, "Razer BlackShark V3", Family::V3, CABLE, false)
        .iface(5)
        .sidetone(15)
        .eq(EqKind::Slot(1), -6, 6)
        .auto_off(OFF_V3),
    Model::new(0x0577, "Razer BlackShark V3 Pro", Family::V3Pro, DONGLE, true)
        .iface(5)
        .sidetone(15)
        .eq(EqKind::Slot(5), -6, 6)
        .auto_off(OFF_15_60),
    Model::new(0x0576, "Razer BlackShark V3 Pro", Family::V3Pro, CABLE, false)
        .iface(5)
        .sidetone(15)
        .eq(EqKind::Slot(5), -6, 6)
        .auto_off(OFF_15_60),
    Model::new(0x0A55, "Razer BlackShark V3 Pro for Xbox", Family::V3Pro, DONGLE, true)
        .iface(5)
        .sidetone(15)
        .eq(EqKind::Slot(5), -6, 6)
        .auto_off(OFF_15_60),
    Model::new(0x056C, "Razer Kraken V4", Family::KrakenV4, DONGLE, true).iface(5),
    Model::new(0x0584, "Razer Barracuda", Family::Barracuda, PA_BARRACUDA, true)
        .sidetone(15)
        .auto_off(&[0, 15, 30, 60]),
    Model::new(0x053C, "Razer Barracuda", Family::Barracuda, PA_COMPACT, true)
        .sidetone(15)
        .auto_off(OFF_15_60),
    Model::new(0x0552, "Razer Barracuda X", Family::Barracuda, PA_COMPACT, true).iface(3),
];

pub fn find_model(pid: u16) -> Option<&'static Model> {
    MODELS.iter().find(|m| m.pid == pid)
}

/// Whether this hidraw node is the model's control channel.
pub fn is_control_interface(model: &Model, d: &DescriptorInfo, interface: Option<u8>) -> bool {
    if let (Some(want), Some(have)) = (model.iface, interface) {
        return want == have;
    }
    d.has_report_id(model.report_id()) && d.usage_pages.iter().any(|p| *p >= 0xFF00)
}

// ---- framing ----

/// Build an Mt frame (report ID first). `count` is the argument count the
/// firmware reads; `payload` may carry extra padding after it.
pub fn mt_frame(seq: u8, domain: u8, cmd: u8, count: u8, payload: &[u8]) -> [u8; FRAME] {
    let mut out = [0u8; FRAME];
    out[0] = MT_REPORT;
    out[2] = seq;
    out[6] = (4 + payload.len()) as u8;
    out[9] = domain;
    out[10] = cmd;
    out[12] = count;
    out[13..13 + payload.len()].copy_from_slice(payload);
    out[62] = out[..62].iter().fold(0, |x, b| x ^ b);
    out
}

/// Build a Pa frame: `class` 0x08 for registers, 0x0E for routing.
pub fn pa_build(flavor: Flavor, class: u8, sub: u8, param: u8, args: &[u8], len: u8) -> [u8; FRAME] {
    let Flavor::Pa { rid, compact, .. } = flavor else { panic!("not a Pa flavor") };
    let mut b = [0u8; FRAME];
    b[0] = rid;
    b[1] = 0x80;
    b[2] = len;
    let o = if compact { 3 } else { 5 };
    b[o] = b'P';
    b[o + 1] = b'A';
    b[o + 2] = class;
    b[o + 3] = if compact { 0x08 } else { 0x00 };
    b[o + 4] = sub;
    b[o + 5] = param;
    b[o + 6..o + 6 + args.len()].copy_from_slice(args);
    b
}

/// A register request: `sub` 0x03 reads, 0x04 writes.
pub fn pa_frame(flavor: Flavor, sub: u8, cmd: u8, payload: &[u8]) -> [u8; FRAME] {
    let wide = matches!(flavor, Flavor::Pa { wide_len: true, .. });
    let mut args = vec![0u8, payload.len() as u8];
    args.extend_from_slice(payload);
    let len = 8 + payload.len() as u8 + if wide && !payload.is_empty() { 2 } else { 0 };
    pa_build(flavor, 0x08, sub, cmd, &args, len)
}

/// Route requests to the headset (on) or back to the dongle (off).
pub fn pa_remote(flavor: Flavor, on: bool) -> [u8; FRAME] {
    pa_build(flavor, 0x0E, 0x02, 0xE1, &[on as u8], 0x07)
}

/// One decoded reply or notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    pub cmd: u8,
    /// 1 = answer to a request, 2 = sent by the headset on its own.
    pub op: u8,
    pub seq: u8,
    pub payload: Vec<u8>,
}

pub fn parse(flavor: Flavor, data: &[u8]) -> Option<Reply> {
    match flavor {
        Flavor::Mt { .. } => {
            if data.len() < 14 || data[0] != MT_REPORT || !(1..=2).contains(&data[11]) {
                return None;
            }
            let n = (data[12] as usize).min(data.len() - 13);
            Some(Reply { cmd: data[10], op: data[11], seq: data[2], payload: data[13..13 + n].to_vec() })
        }
        Flavor::Pa { rid, compact, .. } => {
            if data.len() < 16 || data[0] != rid {
                return None;
            }
            // Sub-frames look like `00 LL 00 REG OP N DATA..` with LL = N + 3.
            for j in 7..data.len() - 3 {
                let n = data[j + 2] as usize;
                if data[j - 3] == 0
                    && data[j - 1] == 0
                    && (1..=2).contains(&data[j + 1])
                    && data[j - 2] as usize == n + 3
                    && n <= 60
                {
                    let end = (j + 3 + n).min(data.len());
                    return Some(Reply { cmd: data[j], op: data[j + 1], seq: 0, payload: data[j + 3..end].to_vec() });
                }
            }
            // The V2 Pro's documented layout: [12]=cmd [13]=1 [14]=len [15..].
            if !compact && data[13] == 0x01 {
                let n = (data[14] as usize).min(data.len() - 15);
                return Some(Reply { cmd: data[12], op: 1, seq: 0, payload: data[15..15 + n].to_vec() });
            }
            None
        }
    }
}

pub fn encode_sign_magnitude(v: i32) -> u8 {
    if v < 0 {
        0x80 | (v.unsigned_abs().min(0x7F) as u8)
    } else {
        v.min(0x7F) as u8
    }
}

pub fn decode_sign_magnitude(b: u8) -> i32 {
    if b & 0x80 != 0 {
        -((b & 0x7F) as i32)
    } else {
        b as i32
    }
}

const CMD_SERIAL: u8 = 0x00;
const CMD_FIRMWARE: u8 = 0x02;
const CMD_INIT: u8 = 0x02;
const CMD_ANC_GET: u8 = 0x12;
const CMD_PRESET_GET: u8 = 0x13;
const CMD_EQ_GET: u8 = 0x15;
const CMD_SIDETONE_GET: u8 = 0x18;
const CMD_SIDETONE_LVL_GET: u8 = 0x19;
const CMD_PREP: u8 = 0x1E;
const CMD_LINK: u8 = 0x20;
const CMD_BATTERY: u8 = 0x21;
const CMD_CHARGING: u8 = 0x2A;
const CMD_IDLE_GET: u8 = 0x2C;
const CMD_ANC: u8 = 0x92;
const CMD_PRESET: u8 = 0x93;
const CMD_EQ: u8 = 0x95;
const CMD_SIDETONE: u8 = 0x98;
const CMD_SIDETONE_LVL: u8 = 0x99;
const CMD_ENHANCE: u8 = 0x9D;
const CMD_EQ_ENABLE: u8 = 0x9E;
const CMD_IDLE_SET: u8 = 0xAC;
const CMD_EQ_META: u8 = 0xE0;
const CMD_EQ_GATE: u8 = 0xE1;
const CMD_EQ_COMMIT: u8 = 0xEB;
const PRESET_CUSTOM: u8 = 0xFF;

const ANC_OFF: u8 = 0x00;
const ANC_ON: u8 = 0x01;
const ANC_AMBIENT: u8 = 0x50;

/// The V3's 0xE0 apply parameters for factory slots 0..4, from Synapse captures.
const V3_SLOT_META: [[u8; 3]; 5] = [[0x00, 0x00, 0x01], [0x01, 0x01, 0x01], [0x03, 0x03, 0x01], [0x02, 0x02, 0x01], [0x04, 0x0A, 0x00]];

pub struct BlackShark {
    t: Arc<dyn Transport>,
    pub model: &'static Model,
    seq: u8,
    pub state: DeviceState,
    timeout: Duration,
    /// Send RF_WAKE keep-alives (output report 5) so the dongle keeps
    /// forwarding the headset's notifications.
    pub keepalive: bool,
    last_keepalive: Option<Instant>,
    anc_level: u8,
    /// The headset announced it came online; read everything again.
    pub wake: bool,
    /// The snapshot changed outside a request (a notification arrived).
    pub changed: bool,
    answered: bool,
}

impl BlackShark {
    pub fn new(t: Arc<dyn Transport>, model: &'static Model) -> BlackShark {
        let v3 = matches!(model.family, Family::V3 | Family::V3Pro);
        BlackShark {
            t,
            model,
            seq: 0,
            state: DeviceState {
                id: format!("razer-{:04x}", model.pid),
                name: model.name.to_string(),
                vendor: "razer",
                kind: Kind::Headset,
                connection: if model.wireless { Connection::Receiver } else { Connection::Usb },
                online: false,
                model: format!("1532:{:04x}", model.pid),
                protocol: Some("Razer audio (vendor HID)".into()),
                ..Default::default()
            },
            timeout: Duration::from_millis(if v3 { 800 } else { 400 }),
            keepalive: false,
            last_keepalive: None,
            anc_level: 4,
            wake: false,
            changed: false,
            answered: false,
        }
    }

    pub fn with_timeout(mut self, t: Duration) -> BlackShark {
        self.timeout = t;
        self
    }

    fn v3(&self) -> bool {
        matches!(self.model.family, Family::V3 | Family::V3Pro)
    }

    fn pro(&self) -> bool {
        self.model.family == Family::V3Pro
    }

    fn fixed_seq(&self) -> bool {
        matches!(self.model.family, Family::V3 | Family::V3Pro | Family::KrakenV4)
    }

    fn write(&self, frame: &[u8]) -> model::Result<()> {
        self.t.write(frame).map_err(model::Error::from)
    }

    fn next_seq(&mut self) -> u8 {
        if self.fixed_seq() {
            return 0x60;
        }
        let s = 0x60 | (self.seq & 0x1F);
        self.seq = self.seq.wrapping_add(1);
        s
    }

    fn mt_send(&mut self, cmd: u8, count: u8, payload: &[u8]) -> model::Result<u8> {
        let Flavor::Mt { domain } = self.model.flavor else { unreachable!() };
        let seq = self.next_seq();
        self.write(&mt_frame(seq, domain, cmd, count, payload))?;
        Ok(seq)
    }

    /// Process everything waiting in the input queue.
    pub fn pump(&mut self) -> model::Result<()> {
        let mut buf = [0u8; FRAME + 1];
        loop {
            let n = self.t.read_timeout(&mut buf, 0)?;
            if n == 0 {
                return Ok(());
            }
            if let Some(r) = parse(self.model.flavor, &buf[..n]) {
                self.absorb(&r);
            }
        }
    }

    /// Take in a value the headset sent outside a matching request.
    fn absorb(&mut self, r: &Reply) {
        let Some(&v) = r.payload.first() else { return };
        if v == 0xFF {
            return; // "not ready"
        }
        crate::trace!("{}: notification 0x{:02x} op {} {:02x?}", self.model.name, r.cmd, r.op, r.payload);
        match r.cmd {
            CMD_BATTERY if v <= 100 => {
                if !self.state.online {
                    self.wake = true;
                }
                let b = self.state.battery.get_or_insert(Battery {
                    percent: None,
                    charge: Charge::Discharging,
                    voltage_mv: None,
                    estimated: false,
                });
                if b.percent != Some(v) {
                    b.percent = Some(v);
                    b.charge = charge_for(b.charge == Charge::Charging || b.charge == Charge::Full, v);
                    self.changed = true;
                }
            }
            CMD_CHARGING if v <= 1 => {
                if let Some(b) = self.state.battery.as_mut() {
                    let c = charge_for(v == 1, b.percent.unwrap_or(0));
                    if b.charge != c {
                        b.charge = c;
                        self.changed = true;
                    }
                }
            }
            CMD_LINK if r.op == 2 => {
                if v == 1 && !self.state.online {
                    self.wake = true;
                } else if v == 0 && self.state.online {
                    self.state.online = false;
                    self.changed = true;
                }
            }
            CMD_SIDETONE_LVL_GET if self.v3() && v as i32 <= self.model.sidetone_max => {
                if let Some(s) = self.state.sidetone.as_mut() {
                    if s.value != v as i32 {
                        s.value = v as i32;
                        self.changed = true;
                    }
                }
            }
            CMD_IDLE_GET if self.v3() && v <= 60 => {
                if let Some(a) = self.state.auto_off.as_mut() {
                    if a.value != v as i32 {
                        a.value = v as i32;
                        if !a.options.contains(&a.value) {
                            a.options.push(a.value);
                            a.options.sort_unstable();
                        }
                        self.changed = true;
                    }
                }
            }
            CMD_ANC_GET if self.pro() => {
                if let Some(level) = r.payload.get(1).copied().filter(|l| (1..=4).contains(l)) {
                    self.anc_level = level;
                }
                if let Some(value) = anc_name(v) {
                    if let Some(m) = self.state.modes.iter_mut().find(|m| m.id == "anc") {
                        if m.value != value {
                            m.value = value.into();
                            self.changed = true;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn await_reply(&mut self, cmd: u8, seq: u8, timeout: Duration) -> model::Result<Option<Vec<u8>>> {
        let deadline = Instant::now() + timeout;
        let mut buf = [0u8; FRAME + 1];
        let mut last_poll = Instant::now();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            let slice = left.min(Duration::from_millis(50));
            let mut n = self.t.read_timeout(&mut buf, slice.as_millis().max(1) as i32)?;
            if n == 0 && self.v3() && last_poll.elapsed() >= Duration::from_millis(100) {
                // Some V3 dongles only answer through GET_REPORT.
                last_poll = Instant::now();
                buf[0] = MT_REPORT;
                n = self.t.get_input(&mut buf[..FRAME]).unwrap_or(0);
            }
            if n == 0 {
                continue;
            }
            let Some(r) = parse(self.model.flavor, &buf[..n]) else { continue };
            if r.cmd == cmd && r.op == 1 && (self.fixed_seq() || matches!(self.model.flavor, Flavor::Pa { .. }) || r.seq == seq) {
                self.answered = true;
                return Ok(Some(r.payload));
            }
            self.absorb(&r);
        }
    }

    fn remote(&self, on: bool) -> model::Result<()> {
        self.write(&pa_remote(self.model.flavor, on))?;
        std::thread::sleep(Duration::from_millis(35));
        Ok(())
    }

    /// The V3's wake sequence; without it the firmware ignores requests.
    fn handshake(&mut self) -> model::Result<()> {
        for (cmd, domain) in [(CMD_INIT, 0x00), (CMD_CHARGING, 0x00), (CMD_CHARGING, 0x80)] {
            self.write(&mt_frame(0x60, domain, cmd, 0, &[]))?;
            std::thread::sleep(Duration::from_millis(3));
        }
        std::thread::sleep(Duration::from_millis(40));
        self.pump()
    }

    /// Send the RF_WAKE keep-alive when it's due.
    pub fn keepalive_tick(&mut self) {
        if !self.keepalive || self.last_keepalive.map_or(false, |t| t.elapsed() < Duration::from_millis(3500)) {
            return;
        }
        self.last_keepalive = Some(Instant::now());
        if let Err(e) = self.t.write(&[0x05, 0x00]) {
            crate::debug!("{}: RF keep-alive not accepted ({}); stopping it", self.model.name, e);
            self.keepalive = false;
        }
    }

    /// Read a value from the headset. Offline if it doesn't answer.
    fn query(&mut self, cmd: u8) -> model::Result<Vec<u8>> {
        self.query_args(cmd, 0, &[])
    }

    fn query_args(&mut self, cmd: u8, count: u8, args: &[u8]) -> model::Result<Vec<u8>> {
        match self.model.flavor {
            Flavor::Mt { .. } => {
                let tries = if self.v3() { 1 } else { 2 };
                for _ in 0..tries {
                    let seq = self.mt_send(cmd, count, args)?;
                    if let Some(p) = self.await_reply(cmd, seq, self.timeout)? {
                        if p.len() == 1 && p[0] == 0xFF {
                            // The dongle answers for a headset that isn't linked.
                            return Err(model::Error::Offline);
                        }
                        return Ok(p);
                    }
                }
                Err(model::Error::Offline)
            }
            flavor @ Flavor::Pa { .. } => {
                self.pump()?;
                self.remote(true)?; // wakes a dozing link
                let timeout = Duration::from_millis(150).max(self.timeout / 3);
                let result = (|| {
                    for _ in 0..3 {
                        self.remote(true)?;
                        self.write(&pa_frame(flavor, 0x03, cmd, args))?;
                        if let Some(p) = self.await_reply(cmd, 0, timeout)? {
                            return Ok(p);
                        }
                    }
                    Err(model::Error::Offline)
                })();
                let _ = self.remote(false);
                result
            }
        }
    }

    fn query_byte(&mut self, cmd: u8) -> model::Result<u8> {
        self.query(cmd)?.first().copied().ok_or_else(|| model::Error::Device(format!("empty reply to 0x{:02x}", cmd)))
    }

    /// Write a one-byte setting.
    fn set(&mut self, cmd: u8, value: u8) -> model::Result<()> {
        match self.model.flavor {
            Flavor::Mt { .. } => {
                // The V3 Pro pads single-value writes with a zero byte.
                let payload: &[u8] = if self.pro() { &[value, 0] } else { &[value] };
                let seq = self.mt_send(cmd, 1, payload)?;
                match self.await_reply(cmd, seq, self.timeout)? {
                    Some(_) => Ok(()),
                    // V3 dongles don't always acknowledge writes.
                    None if self.v3() => Ok(()),
                    // Otherwise a missing ack means the headset is off.
                    None => Err(model::Error::Offline),
                }
            }
            flavor @ Flavor::Pa { .. } => {
                self.remote(true)?;
                self.remote(true)?;
                self.write(&pa_frame(flavor, 0x04, cmd, &[value]))?;
                self.remote(false)
            }
        }
    }

    fn read_battery(&mut self) -> model::Result<Battery> {
        if self.v3() && !self.state.online {
            self.handshake()?;
        }
        let pct = if self.pro() { self.query_args(CMD_BATTERY, 0, &[0x00])? } else { self.query(CMD_BATTERY)? };
        let pct = *pct.first().ok_or_else(|| model::Error::Device("empty battery reply".into()))?;
        if pct > 100 {
            return Err(model::Error::Device(format!("implausible battery value {}", pct)));
        }
        let charging = self.query_byte(CMD_CHARGING).map(|v| v == 1).unwrap_or(false);
        Ok(Battery { percent: Some(pct), charge: charge_for(charging, pct), voltage_mv: None, estimated: false })
    }

    fn mark_offline(&mut self) {
        self.state.online = false;
        self.state.notes = if self.v3() && !self.answered { v3_silent_notes() } else { vec![] };
    }

    /// Read everything. Returns Offline if the headset is off (dongle only).
    pub fn read_all(&mut self) -> model::Result<()> {
        let battery = match self.read_battery() {
            Ok(b) => b,
            Err(model::Error::Offline) => {
                self.mark_offline();
                return Err(model::Error::Offline);
            }
            Err(e) => return Err(e),
        };
        self.state.battery = Some(battery);
        self.state.online = true;
        self.state.notes.clear();
        let m = self.model;
        let basic = matches!(m.family, Family::KrakenV4) || m.pid == 0x0552;
        if self.state.serial.is_none() && !basic {
            if let Ok(s) = self.query(CMD_SERIAL) {
                let serial: String =
                    s.iter().take_while(|b| **b != 0).filter(|b| b.is_ascii_graphic()).map(|b| *b as char).collect();
                if serial.len() >= 4 {
                    self.state.id = format!("razer-{}", serial.to_ascii_lowercase());
                    self.state.serial = Some(serial);
                }
            }
            if m.family == Family::V2 {
                if let Ok(f) = self.query(CMD_FIRMWARE) {
                    self.state.firmware = Some(match f.len() {
                        0 | 1 => "?".into(),
                        2 | 3 => format!("{}.{}", f[0], f[1]),
                        _ => format!("{}.{}.{}.{}", f[0], f[1], f[2], f[3]),
                    });
                }
            }
        }
        if m.sidetone_max > 0 {
            let level = if m.family == Family::V2 {
                let on = self.query_byte(CMD_SIDETONE_GET).unwrap_or(0);
                if on != 0 { self.query_byte(CMD_SIDETONE_LVL_GET).unwrap_or(0) as i32 } else { 0 }
            } else {
                self.query_byte(CMD_SIDETONE_LVL_GET).map(|v| v as i32).unwrap_or(0)
            };
            self.state.sidetone = Some(Range { value: level.clamp(0, m.sidetone_max), min: 0, max: m.sidetone_max, step: 1 });
        }
        let gains: Option<Vec<i32>> = match m.eq {
            EqKind::None => None,
            EqKind::PaSigned | EqKind::Offset(_) => self.query(CMD_EQ_GET).ok().map(|raw| {
                (0..EQ_FREQS.len()).map(|i| raw.get(i).copied().unwrap_or(0) as i8 as i32).collect()
            }),
            EqKind::Slot(slot) => match self.query_args(CMD_EQ_GET, 1, &[slot]) {
                Ok(raw) if raw.first() == Some(&slot) => {
                    Some((0..EQ_FREQS.len()).map(|i| decode_sign_magnitude(raw.get(1 + i).copied().unwrap_or(0))).collect())
                }
                // Unreadable: start flat rather than hide the equalizer.
                _ => Some(vec![0; EQ_FREQS.len()]),
            },
        };
        if let Some(g) = gains {
            let gains = g.into_iter().map(|v| v.clamp(m.eq_min, m.eq_max)).collect();
            self.state.equalizer = Some(Equalizer { freqs: EQ_FREQS.to_vec(), gains, min: m.eq_min, max: m.eq_max, software: false, presets: vec![] });
            if m.eq == EqKind::Slot(1) {
                self.state.notes.push("Your equalizer is stored in the headset's Game preset.".into());
            }
        }
        if !m.auto_off.is_empty() {
            if let Ok(v) = self.query_byte(CMD_IDLE_GET) {
                let mut options = m.auto_off.to_vec();
                if !options.contains(&(v as i32)) {
                    options.push(v as i32);
                    options.sort_unstable();
                }
                self.state.auto_off = Some(Choice { value: v as i32, options });
            }
        }
        if self.pro() {
            let current = match self.query(CMD_ANC_GET) {
                Ok(p) => {
                    if let Some(level) = p.get(1).copied().filter(|l| (1..=4).contains(l)) {
                        self.anc_level = level;
                    }
                    p.first().and_then(|v| anc_name(*v)).unwrap_or("off")
                }
                Err(_) => "off",
            };
            self.state.modes = vec![Mode {
                id: "anc".into(),
                name: "Noise cancelling".into(),
                hint: "Block outside noise, or let it in with Ambient.".into(),
                value: current.into(),
                options: vec![
                    ("off".into(), "Off".into()),
                    ("anc".into(), "On".into()),
                    ("ambient".into(), "Ambient".into()),
                ],
            }];
        }
        Ok(())
    }

    fn write_eq(&mut self, gains: &[i32]) -> model::Result<()> {
        let m = self.model;
        match m.eq {
            EqKind::None => Err(model::Error::NotSupported("equalizer".into())),
            EqKind::Offset(offset) => {
                let wire: Vec<u8> = gains.iter().map(|g| (g + offset) as i8 as u8).collect();
                self.set(CMD_EQ_ENABLE, 1)?;
                self.set(CMD_ENHANCE, 0)?;
                self.set(CMD_PRESET, PRESET_CUSTOM)?;
                let seq = self.mt_send(CMD_EQ, wire.len() as u8, &wire)?;
                if self.await_reply(CMD_EQ, seq, self.timeout)?.is_none() {
                    return Err(model::Error::Offline);
                }
                std::thread::sleep(Duration::from_millis(40));
                self.set(CMD_PRESET, PRESET_CUSTOM)
            }
            EqKind::PaSigned => {
                let wire: Vec<u8> = gains.iter().map(|g| *g as i8 as u8).collect();
                // Make sure the custom slot is active so no factory preset is overwritten.
                if self.query_byte(CMD_PRESET_GET).ok() != Some(PRESET_CUSTOM) {
                    self.pa_apply(None)?;
                }
                self.pa_apply(Some(&wire))?;
                std::thread::sleep(Duration::from_millis(100));
                self.pa_apply(None)
            }
            EqKind::Slot(slot) => {
                let pro = self.pro();
                let pad = |mut v: Vec<u8>| {
                    if pro {
                        v.push(0);
                    }
                    v
                };
                let mut bands = vec![slot];
                bands.extend(gains.iter().map(|g| encode_sign_magnitude(*g)));
                let meta = match V3_SLOT_META.get(slot as usize) {
                    Some([a, b, c]) if !pro => vec![slot, *a, 1, 1, *b, *c],
                    _ => vec![slot, 0, 1, 1, 0, 0],
                };
                let mut commit = vec![0u8; 11];
                commit[0] = slot;
                commit[3] = 1;
                commit[4] = 1;
                let steps: [(u8, u8, Vec<u8>); 5] = [
                    (CMD_EQ_GATE, 1, pad(vec![0x01])),
                    (CMD_EQ, 0x0B, pad(bands)),
                    (CMD_EQ_META, 0x06, pad(meta)),
                    (CMD_EQ_GATE, 1, pad(vec![0x02])),
                    (CMD_EQ_COMMIT, 0x0B, pad(commit)),
                ];
                for (cmd, count, payload) in steps {
                    let seq = self.mt_send(cmd, count, &payload)?;
                    // Acks are optional here; give each step a moment.
                    let _ = self.await_reply(cmd, seq, Duration::from_millis(60))?;
                }
                Ok(())
            }
        }
    }

    /// The V2 Pro's preset apply sequence, as captured from Synapse.
    fn pa_apply(&mut self, bands: Option<&[u8]>) -> model::Result<()> {
        let f = self.model.flavor;
        self.remote(true)?;
        self.remote(true)?;
        self.write(&pa_frame(f, 0x03, CMD_PREP, &[]))?;
        self.remote(true)?;
        self.write(&pa_frame(f, 0x04, CMD_PRESET, &[PRESET_CUSTOM]))?;
        self.remote(true)?;
        self.write(&pa_frame(f, 0x04, CMD_ENHANCE, &[0x01]))?;
        if let Some(b) = bands {
            self.remote(true)?;
            self.write(&pa_frame(f, 0x0D, CMD_EQ, b))?;
        }
        self.remote(true)
    }
}

fn charge_for(charging: bool, pct: u8) -> Charge {
    match (charging, pct) {
        (true, 100) => Charge::Full,
        (true, _) => Charge::Charging,
        _ => Charge::Discharging,
    }
}

fn anc_name(v: u8) -> Option<&'static str> {
    match v {
        ANC_OFF => Some("off"),
        ANC_ON => Some("anc"),
        ANC_AMBIENT => Some("ambient"),
        _ => None,
    }
}

fn v3_silent_notes() -> Vec<String> {
    let quirk = std::fs::read_to_string("/sys/module/usbcore/parameters/quirks")
        .map(|q| q.to_ascii_lowercase().contains("1532:057"))
        .unwrap_or(false);
    let module = std::path::Path::new("/sys/module/razer_cfg255").exists();
    vec![if quirk || module {
        "The headset isn't answering. Turn it on; if it is on, unplug its USB dongle (or cable) and plug it back in \
         once so it starts accepting settings."
            .into()
    } else {
        "The headset isn't answering. The BlackShark V3 needs a kernel option to accept settings: run ./install.sh \
         from the Peridot folder again (Linux 7.3 or newer), then unplug the dongle (or cable) and plug it back in."
            .into()
    }]
}

impl Device for BlackShark {
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
                    self.mark_offline();
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
                if self.state.sidetone.is_none() {
                    return Err(model::Error::NotSupported("sidetone".into()));
                }
                let v = num(value)?.clamp(0, self.model.sidetone_max as i64) as u8;
                if self.v3() {
                    // Level 0 is silent; the V3 has no separate switch to read back.
                    self.set(CMD_SIDETONE, 1)?;
                    self.set(CMD_SIDETONE_LVL, v)?;
                } else {
                    self.set(CMD_SIDETONE, (v > 0) as u8)?;
                    if v > 0 || self.model.family == Family::Barracuda {
                        self.set(CMD_SIDETONE_LVL, v)?;
                    }
                }
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
                self.write_eq(&gains)?;
                if let Some(e) = self.state.equalizer.as_mut() {
                    e.gains = gains;
                }
                Ok(())
            }
            "auto_off" => {
                if self.state.auto_off.is_none() {
                    return Err(model::Error::NotSupported("auto power-off".into()));
                }
                let v = num(value)?.clamp(0, 255) as u8;
                self.set(CMD_IDLE_SET, v)?;
                if let Some(a) = self.state.auto_off.as_mut() {
                    a.value = v as i32;
                    if !a.options.contains(&(v as i32)) {
                        a.options.push(v as i32);
                        a.options.sort_unstable();
                    }
                }
                Ok(())
            }
            "mode" => {
                let id = value.get("id").as_str().unwrap_or("");
                let choice = value.get("value").as_str().unwrap_or("");
                if id != "anc" || !self.state.modes.iter().any(|m| m.id == "anc") {
                    return Err(model::Error::NotSupported(format!("mode '{}'", id)));
                }
                let mode = match choice {
                    "off" => ANC_OFF,
                    "anc" => ANC_ON,
                    "ambient" => ANC_AMBIENT,
                    _ => return invalid(format!("unknown noise cancelling mode '{}'", choice)),
                };
                let level = self.anc_level;
                let seq = self.mt_send(CMD_ANC, 2, &[mode, level])?;
                let _ = self.await_reply(CMD_ANC, seq, self.timeout)?;
                if let Some(m) = self.state.modes.iter_mut().find(|m| m.id == "anc") {
                    m.value = choice.into();
                }
                Ok(())
            }
            _ => Err(model::Error::NotSupported(format!("setting '{}'", key))),
        }
    }
}

pub struct BlackSharkBackend {
    stop: Arc<AtomicBool>,
}

impl Backend for BlackSharkBackend {
    fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// How often to poll (online, offline). The V3 drops its radio link when
/// polled often, so it's mostly left to its own notifications.
fn poll_intervals(family: Family) -> (Duration, Duration) {
    match family {
        Family::V2 => (Duration::from_secs(60), Duration::from_secs(5)),
        Family::V3 | Family::V3Pro => (Duration::from_secs(300), Duration::from_secs(20)),
        Family::KrakenV4 | Family::Barracuda => (Duration::from_secs(120), Duration::from_secs(10)),
    }
}

/// Watch the headset: announce it, notice it turning on/off, track battery.
/// `rf_wake`: the interface accepts the V3 Pro's report-5 keep-alive.
pub fn start(t: Arc<dyn Transport>, model: &'static Model, key: String, rf_wake: bool, sink: Sink) -> BlackSharkBackend {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let _ = std::thread::Builder::new().name(format!("blackshark-{}", key)).spawn(move || {
        let mut dev = BlackShark::new(t.clone(), model);
        dev.keepalive = rf_wake && model.family == Family::V3Pro && model.wireless;
        let dev = Arc::new(Mutex::new(dev));
        let (online_every, offline_every) = poll_intervals(model.family);
        let mut announced: Option<String> = None;
        let mut last_poll: Option<Instant> = None;
        let gone = |r: &model::Result<()>| matches!(r, Err(model::Error::Device(e)) if e.contains("No such device"));
        while !flag.load(Ordering::Relaxed) {
            // Notifications first: battery, charging and link changes.
            let (pumped, wake, pushed) = {
                let mut d = dev.lock().unwrap_or_else(|p| p.into_inner());
                let r = d.pump();
                d.keepalive_tick();
                let wake = std::mem::take(&mut d.wake);
                let pushed = std::mem::take(&mut d.changed).then(|| (d.state.id.clone(), d.state.to_json()));
                (r, wake, pushed)
            };
            if gone(&pumped) {
                break;
            }
            if let (Some(_), Some((id, snap))) = (&announced, pushed) {
                sink(BackendEvent::Changed(id, snap));
            }
            let interval = {
                let d = dev.lock().unwrap_or_else(|p| p.into_inner());
                if d.state.online { online_every } else { offline_every }
            };
            if wake {
                // Give the radio link a moment to settle before talking to it.
                std::thread::sleep(Duration::from_millis(600));
            }
            if wake || last_poll.map_or(true, |p| p.elapsed() >= interval) {
                last_poll = Some(Instant::now());
                let (was_online, result, id, snap) = {
                    let mut d = dev.lock().unwrap_or_else(|p| p.into_inner());
                    let was = d.state.online;
                    let r = if was { d.refresh() } else { d.read_all() };
                    d.changed = false;
                    (was, r, d.state.id.clone(), d.state.to_json())
                };
                if gone(&result) {
                    break;
                }
                if let Some(prev) = &announced {
                    if *prev != id {
                        // The serial became known: re-key the device.
                        sink(BackendEvent::Removed(prev.clone()));
                        announced = None;
                    }
                }
                match (&announced, result.is_ok(), was_online) {
                    (None, _, _) => {
                        crate::info!("{}: {} found (id {})", t.describe(), model.name, id);
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
    BlackSharkBackend { stop }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, VecDeque};
    use std::io;

    #[test]
    fn mt_frame_layout_and_checksum() {
        let f = mt_frame(0x60, 0x80, CMD_BATTERY, 0, &[]);
        assert_eq!(&f[..11], &[0x02, 0x00, 0x60, 0, 0, 0, 0x04, 0, 0, 0x80, 0x21]);
        // The Kraken V4's captured battery query (HeadsetControl PR #587).
        assert_eq!(f[62], 0xC7);
        let c = mt_frame(0x60, 0x80, CMD_CHARGING, 0, &[]);
        assert_eq!(c[62], 0xCC);
        let s = mt_frame(0x61, 0x80, CMD_SIDETONE_LVL, 1, &[7]);
        assert_eq!((s[6], s[12], s[13]), (0x05, 1, 7));
        let p = mt_frame(0x60, 0x80, CMD_SIDETONE_LVL, 1, &[7, 0]);
        assert_eq!((p[6], p[12], p[13], p[14]), (0x06, 1, 7, 0));
    }

    #[test]
    fn pa_frame_layouts() {
        let f = pa_frame(PA_V2, 0x04, CMD_IDLE_SET, &[30]);
        assert_eq!(&f[..14], &[0x02, 0x80, 9, 0, 0, b'P', b'A', 0x08, 0, 0x04, 0xAC, 0, 1, 30]);
        let r = pa_remote(PA_V2, true);
        assert_eq!((r[2], r[7], r[9], r[10], r[11]), (0x07, 0x0E, 0x02, 0xE1, 1));
        // Barracuda (TarikTopalovic): 01 80 09 50 41 08 08 04 <param> 00 01 <value>
        let c = pa_frame(PA_COMPACT, 0x04, CMD_IDLE_SET, &[15]);
        assert_eq!(&c[..12], &[0x01, 0x80, 0x09, 0x50, 0x41, 0x08, 0x08, 0x04, 0xAC, 0, 1, 15]);
        let g = pa_frame(PA_COMPACT, 0x03, CMD_BATTERY, &[]);
        assert_eq!(&g[..9], &[0x01, 0x80, 0x08, 0x50, 0x41, 0x08, 0x08, 0x03, 0x21]);
        // RF refresh: 01 80 07 50 41 0e 08 02 e1 01
        let rr = pa_remote(PA_COMPACT, true);
        assert_eq!(&rr[..10], &[0x01, 0x80, 0x07, 0x50, 0x41, 0x0E, 0x08, 0x02, 0xE1, 0x01]);
        // openbarracuda: ... 04 AC 00 01 3C with length 0x0B
        let o = pa_frame(PA_BARRACUDA, 0x04, CMD_IDLE_SET, &[60]);
        assert_eq!((o[2], o[9], o[10], o[11], o[12], o[13]), (0x0B, 0x04, 0xAC, 0, 1, 60));
    }

    #[test]
    fn parses_captured_replies() {
        // Kraken V4 battery reply.
        let mut k = [0u8; 64];
        k[..14].copy_from_slice(&[0x02, 0x02, 0x60, 0, 0, 0, 0x05, 0, 0x80, 0x80, 0x21, 0x01, 0x01, 0x2C]);
        assert_eq!(parse(DONGLE, &k), Some(Reply { cmd: 0x21, op: 1, seq: 0x60, payload: vec![44] }));
        // Barracuda X battery notification: 01 80 0e 50 49 08 .. 04 00 21 02 01 VV
        let mut b = [0u8; 64];
        b[..17].copy_from_slice(&[0x01, 0x80, 0x0E, 0x50, 0x49, 0x08, 0xF7, 0x0D, 0x32, 0x69, 0x00, 0x04, 0x00, 0x21, 0x02, 0x01, 57]);
        assert_eq!(parse(PA_COMPACT, &b), Some(Reply { cmd: 0x21, op: 2, seq: 0, payload: vec![57] }));
        assert_eq!(decode_sign_magnitude(encode_sign_magnitude(-4)), -4);
        assert_eq!(encode_sign_magnitude(-1), 0x81);
    }

    /// Answers like a headset of any family.
    struct FakeHeadset {
        flavor: Flavor,
        on: Mutex<bool>,
        regs: Mutex<HashMap<u8, Vec<u8>>>,
        out: Mutex<VecDeque<Vec<u8>>>,
        written: Mutex<Vec<Vec<u8>>>,
    }

    impl FakeHeadset {
        fn new(model: &Model) -> Arc<FakeHeadset> {
            let mut regs = HashMap::new();
            regs.insert(CMD_BATTERY, vec![73]);
            regs.insert(CMD_CHARGING, vec![1]);
            regs.insert(CMD_SERIAL, b"BS2311A0042\0\0\0\0".to_vec());
            regs.insert(CMD_FIRMWARE, vec![1, 3]);
            regs.insert(CMD_SIDETONE_GET, vec![1]);
            regs.insert(CMD_SIDETONE_LVL_GET, vec![4]);
            let eq = match model.eq {
                EqKind::Slot(s) => vec![s, 0, 2, 0x82, 0, 0, 0, 0, 0, 1, 3],
                _ => vec![0, 2, 0xFE, 0, 0, 0, 0, 0, 1, 3],
            };
            regs.insert(CMD_EQ_GET, eq);
            regs.insert(CMD_IDLE_GET, vec![30]);
            regs.insert(CMD_PRESET_GET, vec![0xFF]);
            regs.insert(CMD_ANC_GET, vec![ANC_ON, 3]);
            Arc::new(FakeHeadset {
                flavor: model.flavor,
                on: Mutex::new(true),
                regs: Mutex::new(regs),
                out: Mutex::new(VecDeque::new()),
                written: Mutex::new(vec![]),
            })
        }

        fn push(&self, frame: Vec<u8>) {
            self.out.lock().unwrap().push_back(frame);
        }

        fn mt_reply(cmd: u8, op: u8, data: &[u8]) -> Vec<u8> {
            let mut r = vec![0u8; 64];
            r[0] = 0x02;
            r[1] = 0x02;
            r[2] = 0x60;
            r[10] = cmd;
            r[11] = op;
            r[12] = data.len() as u8;
            r[13..13 + data.len()].copy_from_slice(data);
            r
        }
    }

    impl Transport for FakeHeadset {
        fn write(&self, d: &[u8]) -> io::Result<()> {
            self.written.lock().unwrap().push(d.to_vec());
            if !*self.on.lock().unwrap() || d.len() < 64 {
                return Ok(());
            }
            let (cmd, seq, payload) = match self.flavor {
                Flavor::Mt { .. } => (d[10], d[2], d[13..13 + d[6] as usize - 4].to_vec()),
                Flavor::Pa { compact, .. } => {
                    let o = if compact { 3 } else { 5 };
                    if d[o + 5] == 0xE1 {
                        return Ok(());
                    }
                    let n = d[o + 7] as usize;
                    (d[o + 5], 0, d[o + 8..o + 8 + n].to_vec())
                }
            };
            let mut regs = self.regs.lock().unwrap();
            let reply_data = if cmd & 0x80 != 0 {
                regs.insert(cmd & 0x7F, payload.clone());
                payload
            } else {
                regs.get(&cmd).cloned().unwrap_or_default()
            };
            let r = match self.flavor {
                Flavor::Mt { .. } => {
                    let mut r = FakeHeadset::mt_reply(cmd, 1, &reply_data);
                    r[2] = seq;
                    r
                }
                Flavor::Pa { compact, rid, .. } => {
                    // `00 LL 00 REG 01 N DATA` like the real dongles.
                    let mut r = vec![0u8; 64];
                    r[0] = rid;
                    let j = if compact { 13 } else { 12 };
                    r[j - 2] = reply_data.len() as u8 + 3;
                    r[j] = cmd;
                    r[j + 1] = 1;
                    r[j + 2] = reply_data.len() as u8;
                    r[j + 3..j + 3 + reply_data.len()].copy_from_slice(&reply_data);
                    r
                }
            };
            self.out.lock().unwrap().push_back(r);
            Ok(())
        }
        fn read_timeout(&self, buf: &mut [u8], _ms: i32) -> io::Result<usize> {
            match self.out.lock().unwrap().pop_front() {
                Some(r) => {
                    buf[..r.len()].copy_from_slice(&r);
                    Ok(r.len())
                }
                None => Ok(0),
            }
        }
        fn set_feature(&self, _: &[u8]) -> io::Result<()> {
            Ok(())
        }
        fn get_feature(&self, _: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
        fn describe(&self) -> String {
            "fake-headset".into()
        }
    }

    fn open(pid: u16) -> (Arc<FakeHeadset>, BlackShark) {
        let model = find_model(pid).unwrap();
        let fake = FakeHeadset::new(model);
        let t: Arc<dyn Transport> = fake.clone();
        (fake, BlackShark::new(t, model).with_timeout(Duration::from_millis(20)))
    }

    fn check(pid: u16) {
        let (fake, mut d) = open(pid);
        let model = d.model;
        d.read_all().unwrap();
        let s = d.state().clone();
        assert!(s.online);
        assert_eq!(s.battery.as_ref().unwrap().percent, Some(73));
        assert_eq!(s.battery.as_ref().unwrap().charge, Charge::Charging);
        if model.family == Family::KrakenV4 || pid == 0x0552 {
            assert!(s.sidetone.is_none() && s.equalizer.is_none() && s.auto_off.is_none());
            assert_eq!(d.apply("sidetone", &Json::Num(3.0)), Err(model::Error::NotSupported("sidetone".into())));
            return;
        }
        assert_eq!(s.id, "razer-bs2311a0042");
        assert_eq!(s.sidetone.as_ref().unwrap().value, 4);
        assert_eq!(s.auto_off.as_ref().unwrap().value, 30);

        d.apply("sidetone", &Json::Num(6.0)).unwrap();
        d.apply("auto_off", &Json::Num(15.0)).unwrap();
        let regs = fake.regs.lock().unwrap().clone();
        assert_eq!(regs[&(CMD_SIDETONE_LVL & 0x7F)][0], 6);
        assert_eq!(regs[&(CMD_IDLE_SET & 0x7F)][0], 15);

        match model.eq {
            EqKind::None => assert!(s.equalizer.is_none()),
            kind => {
                assert_eq!(s.equalizer.as_ref().unwrap().gains, vec![0, 2, -2, 0, 0, 0, 0, 0, 1, 3]);
                d.apply("equalizer", &Json::parse("[1,1,1,1,1,1,1,1,1,-1]").unwrap()).unwrap();
                let regs = fake.regs.lock().unwrap();
                let wire = &regs[&(CMD_EQ & 0x7F)];
                match kind {
                    EqKind::Offset(o) => assert_eq!(wire, &[&[(1 + o) as u8; 9][..], &[(o - 1) as u8]].concat()),
                    EqKind::PaSigned => assert_eq!(wire, &[&[1u8; 9][..], &[0xFF]].concat()),
                    EqKind::Slot(slot) => {
                        assert_eq!(wire[0], slot);
                        assert_eq!(&wire[1..11], &[1, 1, 1, 1, 1, 1, 1, 1, 1, 0x81]);
                        assert_eq!(wire.len(), if model.family == Family::V3Pro { 12 } else { 11 });
                    }
                    EqKind::None => unreachable!(),
                }
            }
        }
    }

    #[test]
    fn every_model_end_to_end() {
        for m in MODELS {
            check(m.pid);
        }
    }

    #[test]
    fn v3_pro_battery_query_and_noise_cancelling() {
        let (fake, mut d) = open(0x0577);
        d.read_all().unwrap();
        let written = fake.written.lock().unwrap().clone();
        // Handshake: 0x02 then 0x2A on both transport bytes.
        assert_eq!((written[0][10], written[0][9]), (CMD_INIT, 0x00));
        assert_eq!((written[2][10], written[2][9]), (CMD_CHARGING, 0x80));
        // V3 Pro battery query carries one zero argument byte (data size 5).
        let q = written.iter().find(|w| w[10] == CMD_BATTERY).unwrap();
        assert_eq!((q[6], q[12], q[13]), (5, 0, 0));
        assert_eq!(d.state().modes[0].value, "anc");
        d.apply("mode", &Json::parse(r#"{"id":"anc","value":"ambient"}"#).unwrap()).unwrap();
        assert_eq!(fake.regs.lock().unwrap()[&(CMD_ANC & 0x7F)], vec![ANC_AMBIENT, 3]);
        assert_eq!(d.state().modes[0].value, "ambient");
    }

    #[test]
    fn notifications_update_state() {
        let (fake, mut d) = open(0x057A);
        d.read_all().unwrap();
        fake.push(FakeHeadset::mt_reply(CMD_BATTERY, 2, &[51]));
        fake.push(FakeHeadset::mt_reply(CMD_CHARGING, 2, &[0]));
        d.pump().unwrap();
        assert!(d.changed);
        let b = d.state().battery.clone().unwrap();
        assert_eq!((b.percent, b.charge), (Some(51), Charge::Discharging));
        fake.push(FakeHeadset::mt_reply(CMD_LINK, 2, &[0]));
        d.pump().unwrap();
        assert!(!d.state().online);
        fake.push(FakeHeadset::mt_reply(CMD_LINK, 2, &[1]));
        d.pump().unwrap();
        assert!(d.wake);
    }

    #[test]
    fn v3_not_linked_is_offline_with_advice() {
        let (fake, mut d) = open(0x057A);
        fake.regs.lock().unwrap().insert(CMD_BATTERY, vec![0xFF]);
        assert_eq!(d.read_all(), Err(model::Error::Offline));
        assert!(!d.state().online);
        // It did answer (0xFF), so no kernel-module advice.
        assert!(d.state().notes.is_empty());
        *fake.on.lock().unwrap() = false;
        let (_f2, mut silent) = open(0x0579);
        *_f2.on.lock().unwrap() = false;
        assert_eq!(silent.read_all(), Err(model::Error::Offline));
        assert_eq!(silent.state().notes.len(), 1);
    }

    #[test]
    fn headset_off_reports_offline() {
        let (fake, mut d) = open(0x0565);
        *fake.on.lock().unwrap() = false;
        assert_eq!(d.read_all(), Err(model::Error::Offline));
        assert!(!d.state().online);
        assert_eq!(d.apply("sidetone", &Json::Num(3.0)), Err(model::Error::Offline));
    }
}
