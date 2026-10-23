//! Classic Razer Kraken headsets (Kraken 7.1, 7.1 Chroma, 7.1 V2, Tournament
//! Edition, Ultimate, Kitty V2): lighting only.
//!
//! These don't use the 90-byte Razer reports. Settings are memory writes: a
//! 37-byte output report `0x04, destination, length, address (BE), data` on
//! interface 3, at addresses that differ between the older ("Rainie") and
//! newer ("Kylie") generations. Layouts follow OpenRazer's razerkraken driver.

use crate::backend::{Backend, BackendEvent, DeviceHandle, Sink};
use crate::hid::Transport;
use crate::json::Json;
use crate::model::{self, invalid, Connection, DeviceState, Effect, Kind, Lighting, Zone};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const REPORT_LEN: usize = 37;
const REPORT_ID: u8 = 0x04;
const WRITE: u8 = 0x40;
/// The interface OpenRazer sends to (wIndex 3).
pub const INTERFACE: u8 = 3;

// Effect byte bits
const ON: u8 = 1 << 0;
const BREATHING_1: u8 = 1 << 1;
const SPECTRUM: u8 = 1 << 2;
const SYNC: u8 = 1 << 3;
const BREATHING_2: u8 = 1 << 4;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Generation {
    /// Kraken 7.1 (single colour, on/off only)
    Classic,
    /// Kraken 7.1 Chroma
    Rainie,
    /// 7.1 V2, Tournament Edition, Ultimate, Kitty V2
    Kylie,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Model {
    pub pid: u16,
    pub name: &'static str,
    generation: Generation,
}

pub const MODELS: &[Model] = &[
    Model { pid: 0x0501, name: "Razer Kraken 7.1", generation: Generation::Classic },
    Model { pid: 0x0506, name: "Razer Kraken 7.1", generation: Generation::Classic },
    Model { pid: 0x0504, name: "Razer Kraken 7.1 Chroma", generation: Generation::Rainie },
    Model { pid: 0x0510, name: "Razer Kraken 7.1 V2", generation: Generation::Kylie },
    Model { pid: 0x0520, name: "Razer Kraken Tournament Edition", generation: Generation::Kylie },
    Model { pid: 0x0527, name: "Razer Kraken Ultimate", generation: Generation::Kylie },
    Model { pid: 0x0560, name: "Razer Kraken Kitty V2", generation: Generation::Kylie },
];

pub fn find_model(pid: u16) -> Option<&'static Model> {
    MODELS.iter().find(|m| m.pid == pid)
}

impl Model {
    fn led_address(&self) -> u16 {
        if self.generation == Generation::Kylie { 0x172D } else { 0x1008 }
    }
    /// Colour of single-colour breathing (and of static, which shares it).
    fn breathing1_address(&self) -> u16 {
        if self.generation == Generation::Kylie { 0x1741 } else { 0x15DE }
    }
    fn effects(&self) -> Vec<Effect> {
        match self.generation {
            Generation::Classic => vec![Effect::Off, Effect::Static],
            _ => vec![Effect::Off, Effect::Static, Effect::Breathing, Effect::Spectrum],
        }
    }
}

/// A memory write: `0x04, 0x40, length, address, data`.
pub fn write_report(address: u16, data: &[u8]) -> [u8; REPORT_LEN] {
    let mut r = [0u8; REPORT_LEN];
    r[0] = REPORT_ID;
    r[1] = WRITE;
    r[2] = data.len() as u8;
    r[3..5].copy_from_slice(&address.to_be_bytes());
    r[5..5 + data.len()].copy_from_slice(data);
    r
}

/// The writes that set `l`, in order.
fn lighting_writes(m: &Model, l: &Lighting) -> model::Result<Vec<[u8; REPORT_LEN]>> {
    let rgb = [l.color.0, l.color.1, l.color.2];
    let led = |bits: u8| write_report(m.led_address(), &[bits]);
    Ok(match l.effect {
        Effect::Off => vec![led(0)],
        Effect::Static if m.generation == Generation::Classic => vec![led(ON)],
        Effect::Static => vec![write_report(m.breathing1_address(), &rgb), led(ON)],
        Effect::Spectrum => vec![led(ON | SPECTRUM)],
        Effect::Breathing => match l.color2 {
            // Two colours: the Kylie generation's second breathing slot.
            Some(c2) if m.generation == Generation::Kylie => vec![
                write_report(0x1745, &rgb),
                write_report(0x1745 + 4, &[c2.0, c2.1, c2.2]),
                led(ON | BREATHING_2 | SYNC),
            ],
            _ => vec![write_report(m.breathing1_address(), &rgb), led(ON | BREATHING_1 | SYNC)],
        },
        other => return Err(model::Error::NotSupported(other.as_str().into())),
    })
}

pub struct KrakenDevice {
    t: Arc<dyn Transport>,
    model: &'static Model,
    pub state: DeviceState,
}

impl KrakenDevice {
    pub fn new(t: Arc<dyn Transport>, model: &'static Model) -> KrakenDevice {
        let state = DeviceState {
            id: format!("razer-{:04x}", model.pid),
            name: model.name.into(),
            vendor: "razer",
            kind: Kind::Headset,
            connection: Connection::Usb,
            online: true,
            model: format!("1532:{:04x}", model.pid),
            protocol: Some("Razer Kraken".into()),
            zones: vec![Zone {
                id: "logo".into(),
                name: "Lighting".into(),
                effects: model.effects(),
                state: None,
                brightness: None,
                dual_color: model.generation == Generation::Kylie,
            }],
            ..Default::default()
        };
        KrakenDevice { t, model, state }
    }
}

impl model::Device for KrakenDevice {
    fn state(&self) -> &DeviceState {
        &self.state
    }

    fn refresh(&mut self) -> model::Result<()> {
        Ok(())
    }

    fn apply(&mut self, key: &str, value: &Json) -> model::Result<()> {
        if key != "lighting" {
            return Err(model::Error::NotSupported(key.into()));
        }
        let l = Lighting::from_json(value)?;
        if !self.state.zones[0].effects.contains(&l.effect) {
            return invalid(format!("{} can't do {}", self.state.name, l.effect.as_str()));
        }
        for r in lighting_writes(self.model, &l)? {
            self.t.write(&r)?;
            // The headset writes to EEPROM: about 15 ms per byte.
            std::thread::sleep(Duration::from_millis(15 * r[2] as u64 + 10));
        }
        self.state.zones[0].state = Some(l);
        Ok(())
    }
}

pub struct KrakenBackend {
    stop: Arc<AtomicBool>,
}

impl Backend for KrakenBackend {
    fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

pub fn start(t: Arc<dyn Transport>, model: &'static Model, key: String, sink: Sink) -> KrakenBackend {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let _ = std::thread::Builder::new().name(format!("kraken-{}", key)).spawn(move || {
        let dev = Arc::new(Mutex::new(KrakenDevice::new(t, model)));
        let id = dev.lock().unwrap_or_else(|p| p.into_inner()).state.id.clone();
        crate::info!("{}: {} ready", key, model.name);
        let handle: DeviceHandle = dev;
        sink(BackendEvent::Ready(handle));
        while !flag.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(500));
        }
        sink(BackendEvent::Removed(id));
        sink(BackendEvent::Closed(key));
    });
    KrakenBackend { stop }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Rgb;

    fn l(effect: Effect, c2: Option<Rgb>) -> Lighting {
        Lighting { effect, color: Rgb(0x10, 0x20, 0x30), color2: c2, speed: 50, direction: model::Direction::Right }
    }

    #[test]
    fn kylie_writes() {
        let m = find_model(0x0527).unwrap();
        let w = lighting_writes(m, &l(Effect::Static, None)).unwrap();
        assert_eq!(&w[0][..8], &[0x04, 0x40, 0x03, 0x17, 0x41, 0x10, 0x20, 0x30]);
        assert_eq!(&w[1][..6], &[0x04, 0x40, 0x01, 0x17, 0x2D, ON]);
        let w = lighting_writes(m, &l(Effect::Breathing, Some(Rgb(1, 2, 3)))).unwrap();
        assert_eq!(&w[1][3..8], &[0x17, 0x49, 1, 2, 3]);
        assert_eq!(w[2][5], ON | BREATHING_2 | SYNC);
        assert_eq!(lighting_writes(m, &l(Effect::Spectrum, None)).unwrap()[0][5], ON | SPECTRUM);
    }

    #[test]
    fn older_generations() {
        let chroma = find_model(0x0504).unwrap();
        let w = lighting_writes(chroma, &l(Effect::Breathing, Some(Rgb(1, 2, 3)))).unwrap();
        // Single-colour breathing only, at the Rainie addresses.
        assert_eq!(&w[0][3..5], &[0x15, 0xDE]);
        assert_eq!(&w[1][3..6], &[0x10, 0x08, ON | BREATHING_1 | SYNC]);
        let classic = find_model(0x0501).unwrap();
        assert_eq!(lighting_writes(classic, &l(Effect::Static, None)).unwrap().len(), 1);
        assert_eq!(classic.effects(), vec![Effect::Off, Effect::Static]);
    }
}
