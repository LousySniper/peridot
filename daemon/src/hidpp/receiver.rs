//! Backend for one Logitech HID++ node: either a receiver with up to six
//! paired devices, or a directly connected (USB/Bluetooth) device.

use super::channel::ChannelEvent;
use super::device::LogitechDevice;
use super::features::protocol_version;
use super::{is_receiver, receiver_name, Channel, Message, DEV_DIRECT, NOTIF_DEVICE_CONNECTED, NOTIF_DEVICE_DISCONNECTED};
use crate::backend::{Backend, BackendEvent, DeviceHandle, Sink};
use crate::hid::Transport;
use crate::model::{self, Connection, Device, Kind};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BATTERY_REFRESH: Duration = Duration::from_secs(300);
const DIRECT_RETRY: Duration = Duration::from_secs(15);

pub struct LogitechBackend {
    ch: Arc<Channel>,
    stop: Arc<AtomicBool>,
}

impl Backend for LogitechBackend {
    fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        self.ch.close();
    }
}

pub struct Params {
    pub key: String,
    pub pid: u16,
    pub connection: Connection,
    pub short_ok: bool,
}

pub fn start(transport: Arc<dyn Transport>, params: Params, sink: Sink) -> LogitechBackend {
    let ch = Channel::open(transport, params.short_ok);
    let stop = Arc::new(AtomicBool::new(false));
    let backend = LogitechBackend { ch: ch.clone(), stop: stop.clone() };
    let name = format!("logi-{}", params.key);
    let _ = std::thread::Builder::new().name(name).spawn(move || {
        let mut w = Worker { ch, params, sink, stop, slots: HashMap::new(), wpids: HashMap::new(), unsupported: Vec::new() };
        w.run();
    });
    backend
}

struct Worker {
    ch: Arc<Channel>,
    params: Params,
    sink: Sink,
    stop: Arc<AtomicBool>,
    slots: HashMap<u8, Arc<Mutex<LogitechDevice>>>,
    wpids: HashMap<u8, u16>,
    unsupported: Vec<u8>,
}

impl Worker {
    fn receiver(&self) -> bool {
        is_receiver(self.params.pid)
    }

    fn emit_changed(&self, dev: &Arc<Mutex<LogitechDevice>>) {
        let d = dev.lock().unwrap_or_else(|p| p.into_inner());
        (self.sink)(BackendEvent::Changed(d.state.id.clone(), d.state.to_json()));
    }

    fn run(&mut self) {
        let rx = self.ch.subscribe();
        if self.receiver() {
            crate::info!("{}: {} found", self.ch.describe(), receiver_name(self.params.pid));
            // Ask for connect/disconnect notifications, then for a replay of the
            // current link state of every paired device.
            if let Err(e) = self.ch.write_register(DEV_DIRECT, 0x00, &[0x00, 0x09, 0x00]) {
                crate::debug!("{}: enabling notifications failed: {}", self.ch.describe(), e);
            }
            let _ = self.ch.write_register(DEV_DIRECT, 0x02, &[0x02, 0x00, 0x00]);
            // Collect the replayed link announcements first: they carry each
            // device's wireless product id and tell us who is asleep.
            let mut announced: HashMap<u8, bool> = HashMap::new();
            let deadline = Instant::now() + Duration::from_millis(400);
            while let Some(left) = deadline.checked_duration_since(Instant::now()) {
                match rx.recv_timeout(left) {
                    Ok(ChannelEvent::Message(m)) if m.sub == NOTIF_DEVICE_CONNECTED && (1..=6).contains(&m.device) => {
                        let wpid = ((m.param(2) as u16) << 8) | m.param(1) as u16;
                        if wpid != 0 {
                            self.wpids.insert(m.device, wpid);
                        }
                        announced.insert(m.device, m.param(0) & 0x40 == 0);
                    }
                    Ok(ChannelEvent::Message(_)) => {}
                    _ => break,
                }
            }
            for idx in 1..=6u8 {
                if self.stop.load(Ordering::Relaxed) {
                    break;
                }
                match announced.get(&idx) {
                    Some(true) => self.try_probe(idx, Duration::from_millis(1000)),
                    Some(false) => {
                        crate::debug!("{}: device {} is paired but asleep", self.ch.describe(), idx);
                        self.add_placeholder(idx);
                    }
                    // Receivers that don't replay links: ping the slot.
                    None => self.try_probe(idx, Duration::from_millis(400)),
                }
            }
        } else {
            self.try_probe(DEV_DIRECT, Duration::from_millis(1500));
        }

        let mut last_refresh = Instant::now();
        let mut last_retry = Instant::now();
        loop {
            if self.stop.load(Ordering::Relaxed) {
                break;
            }
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(ChannelEvent::Message(m)) => self.handle(m),
                Ok(ChannelEvent::Closed) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
            if !self.receiver()
                && !self.slots.contains_key(&DEV_DIRECT)
                && !self.unsupported.contains(&DEV_DIRECT)
                && last_retry.elapsed() > DIRECT_RETRY
            {
                last_retry = Instant::now();
                self.try_probe(DEV_DIRECT, Duration::from_millis(800));
            }
            if last_refresh.elapsed() > BATTERY_REFRESH {
                last_refresh = Instant::now();
                self.refresh_all();
            }
        }
        for dev in self.slots.values() {
            let id = dev.lock().unwrap_or_else(|p| p.into_inner()).state.id.clone();
            (self.sink)(BackendEvent::Removed(id));
        }
        (self.sink)(BackendEvent::Closed(self.params.key.clone()));
    }

    fn refresh_all(&self) {
        for (idx, dev) in &self.slots {
            let (online, incomplete) = {
                let d = dev.lock().unwrap_or_else(|p| p.into_inner());
                (d.state.online, d.incomplete)
            };
            // A directly connected headset that was off answers instantly, so
            // check it too in case its "switched on" notice was missed.
            let retry = !online && incomplete && *idx == DEV_DIRECT;
            if online || retry {
                let back = {
                    let mut d = dev.lock().unwrap_or_else(|p| p.into_inner());
                    let _ = d.refresh();
                    !online && d.state.online
                };
                if back {
                    let dyn_handle: DeviceHandle = dev.clone();
                    (self.sink)(BackendEvent::Ready(dyn_handle));
                } else {
                    self.emit_changed(dev);
                }
            }
        }
    }

    fn model_for(&self, idx: u8) -> String {
        match self.wpids.get(&idx) {
            Some(w) if self.receiver() => format!("046d:{:04x}", w),
            _ => format!("046d:{:04x}", self.params.pid),
        }
    }

    /// Name and serial of a paired device, from the receiver's pairing registers.
    fn pairing_info(&self, idx: u8) -> (Option<String>, Option<String>) {
        let hex = |b: &[u8]| -> Option<String> {
            if b.len() == 4 && b.iter().any(|x| *x != 0) {
                Some(b.iter().map(|x| format!("{:02X}", x)).collect())
            } else {
                None
            }
        };
        if self.params.pid == 0xC548 {
            let name = self.ch.read_register(DEV_DIRECT, 0xB5, true, &[0x60 + idx, 0x01]).ok().and_then(|r| {
                let len = (r.get(2).copied().unwrap_or(0) as usize).min(14);
                r.get(3..3 + len).map(|b| String::from_utf8_lossy(b).trim().to_string())
            });
            let serial = self.ch.read_register(DEV_DIRECT, 0xB5, true, &[0x50 + idx]).ok().and_then(|r| r.get(4..8).and_then(hex));
            (name.filter(|n| !n.is_empty()), serial)
        } else {
            let name = self.ch.read_register(DEV_DIRECT, 0xB5, true, &[0x40 + idx - 1]).ok().and_then(|r| {
                let len = (r.get(1).copied().unwrap_or(0) as usize).min(14);
                r.get(2..2 + len).map(|b| String::from_utf8_lossy(b).trim().to_string())
            });
            let serial = self.ch.read_register(DEV_DIRECT, 0xB5, true, &[0x30 + idx - 1]).ok().and_then(|r| r.get(1..5).and_then(hex));
            (name.filter(|n| !n.is_empty()), serial)
        }
    }

    fn add_placeholder(&mut self, idx: u8) {
        let (name, serial) = self.pairing_info(idx);
        let Some(name) = name else { return };
        let dev = LogitechDevice::placeholder(
            self.ch.clone(),
            idx,
            name,
            self.model_for(idx),
            Some(receiver_name(self.params.pid).to_string()),
            serial,
        );
        crate::info!("{}: {} is paired but asleep", self.ch.describe(), dev.state.name);
        let handle = Arc::new(Mutex::new(dev));
        self.slots.insert(idx, handle.clone());
        let dyn_handle: DeviceHandle = handle;
        (self.sink)(BackendEvent::Ready(dyn_handle));
    }

    /// An older (HID++ 1.0) device paired to the receiver: battery only.
    fn add_legacy(&mut self, idx: u8) {
        let (name, serial) = self.pairing_info(idx);
        let mut dev = LogitechDevice::placeholder(
            self.ch.clone(),
            idx,
            name.unwrap_or_else(|| "Logitech device".into()),
            self.model_for(idx),
            Some(receiver_name(self.params.pid).to_string()),
            serial,
        );
        // Unifying pairing info byte 7: 1 keyboard, 2 mouse, 3 numpad, 8 trackball, 9 touchpad.
        if self.params.pid != 0xC548 {
            if let Ok(r) = self.ch.read_register(DEV_DIRECT, 0xB5, true, &[0x20 + idx - 1]) {
                dev.state.kind = match r.get(7).map(|b| b & 0x0F) {
                    Some(1) | Some(3) => Kind::Keyboard,
                    Some(2) | Some(8) | Some(9) => Kind::Mouse,
                    _ => Kind::Other,
                };
            }
        }
        dev.placeholder = false;
        dev.legacy = true;
        dev.state.protocol = Some("HID++ 1.0".into());
        dev.state.online = true;
        if let Err(e) = model::Device::refresh(&mut dev) {
            crate::debug!("{}: {}: battery: {}", self.ch.describe(), dev.state.name, e);
        }
        crate::info!("{}: {} ready (HID++ 1.0, battery only)", self.ch.describe(), dev.state.name);
        let handle = Arc::new(Mutex::new(dev));
        self.slots.insert(idx, handle.clone());
        let dyn_handle: DeviceHandle = handle;
        (self.sink)(BackendEvent::Ready(dyn_handle));
    }

    fn try_probe(&mut self, idx: u8, timeout: Duration) {
        let version = match protocol_version(&self.ch, idx, timeout) {
            Ok(v) => v,
            Err(e) => {
                crate::debug!("{}: device {} not reachable: {}", self.ch.describe(), idx, e);
                return;
            }
        };
        if version.0 < 2 {
            if self.receiver() && (1..=6).contains(&idx) {
                self.add_legacy(idx);
            } else if !self.unsupported.contains(&idx) {
                crate::info!("{}: device {} speaks HID++ 1.0 only; not supported", self.ch.describe(), idx);
                self.unsupported.push(idx);
            }
            return;
        }
        let (connection, via) = if self.receiver() {
            (Connection::Receiver, Some(receiver_name(self.params.pid).to_string()))
        } else {
            (self.params.connection, None)
        };
        let model = self.model_for(idx);
        match LogitechDevice::probe(self.ch.clone(), idx, connection, model, via, version) {
            Ok(dev) => {
                crate::info!(
                    "{}: {} ready ({} features, id {})",
                    self.ch.describe(),
                    dev.state.name,
                    dev.features().entries.len(),
                    dev.state.id
                );
                if let Some(prev) = self.slots.get(&idx) {
                    let prev_id = prev.lock().unwrap_or_else(|p| p.into_inner()).state.id.clone();
                    if prev_id != dev.state.id {
                        (self.sink)(BackendEvent::Removed(prev_id));
                    }
                }
                let handle = Arc::new(Mutex::new(dev));
                self.slots.insert(idx, handle.clone());
                let dyn_handle: DeviceHandle = handle;
                (self.sink)(BackendEvent::Ready(dyn_handle));
            }
            Err(e) => crate::warn!("{}: probing device {} failed: {}", self.ch.describe(), idx, e),
        }
    }

    fn handle(&mut self, m: Message) {
        let idx = m.device;
        if self.receiver() && m.sub == NOTIF_DEVICE_CONNECTED && (1..=6).contains(&idx) {
            let online = m.param(0) & 0x40 == 0;
            let wpid = ((m.param(2) as u16) << 8) | m.param(1) as u16;
            if wpid != 0 {
                self.wpids.insert(idx, wpid);
            }
            let is_placeholder =
                self.slots.get(&idx).map_or(false, |d| d.lock().unwrap_or_else(|p| p.into_inner()).placeholder);
            if online && is_placeholder {
                std::thread::sleep(Duration::from_millis(150));
                self.try_probe(idx, Duration::from_millis(1000));
                return;
            }
            match self.slots.get(&idx).cloned() {
                Some(dev) if online && dev.lock().unwrap_or_else(|p| p.into_inner()).state.online => {
                    // Duplicate announcement for a device we already consider online.
                }
                Some(dev) if online => {
                    // Back from sleep: re-read and let the manager re-apply settings,
                    // since many devices forget host settings when they power down.
                    std::thread::sleep(Duration::from_millis(150));
                    {
                        let mut d = dev.lock().unwrap_or_else(|p| p.into_inner());
                        d.set_online(true);
                        d.read_all();
                    }
                    let dyn_handle: DeviceHandle = dev;
                    (self.sink)(BackendEvent::Ready(dyn_handle));
                }
                Some(dev) => {
                    dev.lock().unwrap_or_else(|p| p.into_inner()).set_online(false);
                    self.emit_changed(&dev);
                }
                None if online => {
                    std::thread::sleep(Duration::from_millis(150));
                    self.try_probe(idx, Duration::from_millis(1000));
                }
                None => {}
            }
            return;
        }
        if self.receiver() && m.sub == NOTIF_DEVICE_DISCONNECTED && (1..=6).contains(&idx) {
            if let Some(dev) = self.slots.remove(&idx) {
                let id = dev.lock().unwrap_or_else(|p| p.into_inner()).state.id.clone();
                (self.sink)(BackendEvent::Removed(id));
            }
            return;
        }
        if let Some(dev) = self.slots.get(&idx).cloned() {
            if dev.lock().unwrap_or_else(|p| p.into_inner()).placeholder {
                if m.sub < 0x80 && m.swid() == 0 {
                    self.try_probe(idx, Duration::from_millis(1000));
                }
                return;
            }
            let (events, changed, came_back, id) = {
                let mut d = dev.lock().unwrap_or_else(|p| p.into_inner());
                let mut came_back = !d.state.online && m.sub < 0x80 && m.swid() == 0;
                if came_back {
                    d.set_online(true);
                    if d.incomplete {
                        // It wasn't answering when probed (a headset that was off
                        // while its dongle answered): read the rest now.
                        d.read_all();
                        came_back = d.state.online;
                    }
                }
                let (events, changed) = d.handle_notification(&m);
                (events, changed, came_back, d.state.id.clone())
            };
            for e in events {
                (self.sink)(BackendEvent::Button { device: id.clone(), control: e.control, pressed: e.pressed });
            }
            if came_back {
                let dyn_handle: DeviceHandle = dev;
                (self.sink)(BackendEvent::Ready(dyn_handle));
            } else if changed {
                self.emit_changed(&dev);
            }
        } else if self.receiver() && (1..=6).contains(&idx) && m.sub < 0x80 && m.swid() == 0 {
            if !self.unsupported.contains(&idx) {
                self.try_probe(idx, Duration::from_millis(1000));
            }
        }
    }
}
