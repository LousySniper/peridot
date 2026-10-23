//! The daemon core: owns backends and devices, persists settings, performs
//! button actions and answers D-Bus calls. Everything funnels through one
//! event loop; device I/O happens on per-device worker threads so a slow
//! wireless device never stalls the others.

use crate::actions::{Action, DpiOp};
use crate::backend::{self, Backend, BackendEvent, DeviceHandle, Sink};
use crate::config::{self, Store};
use crate::dbus::{self, Connection};
use crate::hid::{self, HidInfo, HidRaw, Hotplug, Transport};
use crate::centurion;
use crate::hidpp;
use crate::json::Json;
use crate::model::{self, Connection as Link};
use crate::razer;
use crate::remap::Engine;
use crate::softeq::{self, SoftEq};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub enum Event {
    Hotplug(Hotplug),
    Backend(BackendEvent),
    Call(dbus::Message),
    Applied(Applied),
    /// Status of a headset's computer-side audio (see `softeq`).
    Audio(String, softeq::Status),
    Quit,
}

pub struct Applied {
    pub id: String,
    pub key: String,
    pub value: Json,
    pub reply_to: Option<dbus::Message>,
    pub persist: bool,
    pub result: Result<Json, model::Error>,
}

type Job = Box<dyn FnOnce() + Send>;

struct Entry {
    handle: DeviceHandle,
    snapshot: Json,
    worker: Sender<Job>,
}

/// Settings Peridot applies on the computer rather than in the headset.
const PC_KEYS: &[&str] = &["surround"];

/// A headset's computer-side audio settings and what the audio thread reported.
#[derive(Default)]
struct PcAudio {
    surround: bool,
    status: softeq::Status,
}

pub struct Options {
    pub config_dir: PathBuf,
}

pub struct Manager {
    tx: Sender<Event>,
    rx: Receiver<Event>,
    conn: Option<Arc<Connection>>,
    store: Store,
    backends: HashMap<String, Box<dyn Backend>>,
    backend_nodes: HashMap<String, HidInfo>,
    seen: HashSet<String>,
    devices: HashMap<String, Entry>,
    order: Vec<String>,
    engine: Engine,
    softeq: SoftEq,
    pc: HashMap<String, PcAudio>,
    /// The EQ curve each device's user tuned by hand, kept while they try presets.
    custom_eq: HashMap<String, Json>,
    warnings: Vec<String>,
}

fn worker_thread(id: &str) -> Sender<Job> {
    let (tx, rx) = mpsc::channel::<Job>();
    let _ = std::thread::Builder::new().name(format!("dev-{}", id)).spawn(move || {
        for job in rx {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err() {
                crate::error!("a device job panicked; continuing");
            }
        }
    });
    tx
}

impl Manager {
    pub fn new(opts: Options, conn: Option<Arc<Connection>>) -> Manager {
        let (tx, rx) = mpsc::channel();
        let audio_tx = tx.clone();
        Manager {
            tx,
            rx,
            conn,
            store: Store::new(opts.config_dir),
            backends: HashMap::new(),
            backend_nodes: HashMap::new(),
            seen: HashSet::new(),
            devices: HashMap::new(),
            order: Vec::new(),
            engine: Engine::new(),
            softeq: SoftEq::new(Box::new(move |id: &str, status| {
                let _ = audio_tx.send(Event::Audio(id.to_string(), status));
            })),
            pc: HashMap::new(),
            custom_eq: HashMap::new(),
            warnings: Vec::new(),
        }
    }

    pub fn sender(&self) -> Sender<Event> {
        self.tx.clone()
    }

    fn sink(&self) -> Sink {
        let tx = Mutex::new(self.tx.clone());
        Arc::new(move |e| {
            let _ = tx.lock().unwrap().send(Event::Backend(e));
        })
    }

    fn warn_once(&mut self, msg: String) {
        if !self.warnings.contains(&msg) {
            crate::warn!("{}", msg);
            self.warnings.push(msg);
        }
    }

    // ---------------------------------------------------------- discovery --

    pub fn start(&mut self) {
        let tx = Mutex::new(self.tx.clone());
        if let Err(e) = hid::spawn_hotplug_monitor(move |ev| {
            let _ = tx.lock().unwrap().send(Event::Hotplug(ev));
        }) {
            self.warn_once(format!("hotplug monitoring unavailable ({}); rescanning periodically", e));
        }
        self.rescan();
    }

    fn rescan(&mut self) {
        for info in hid::enumerate() {
            if !self.seen.contains(&info.sys_name) {
                self.consider(info);
            }
        }
    }

    fn consider(&mut self, info: HidInfo) {
        self.seen.insert(info.sys_name.clone());
        let key = info.sys_name.clone();
        match info.vid {
            0x046D => {
                let desc = info.report_info();
                if let Some(model) = centurion::find_model(info.pid) {
                    if !centurion::is_control_interface(&desc.applications) {
                        return;
                    }
                    let t: Arc<dyn Transport> = match HidRaw::open(&info.path) {
                        Ok(t) => Arc::new(t),
                        Err(e) => return self.open_failed(&info, e),
                    };
                    crate::debug!("{}: Logitech Centurion node ({})", key, info.name);
                    let be = centurion::start(t, model, key.clone(), self.sink());
                    self.backends.insert(key.clone(), Box::new(be));
                    self.backend_nodes.insert(key, info);
                    return;
                }
                if !desc.is_hidpp() {
                    return;
                }
                let t: Arc<dyn Transport> = match HidRaw::open(&info.path) {
                    Ok(t) => Arc::new(t),
                    Err(e) => return self.open_failed(&info, e),
                };
                let connection = if info.bus == hid::BUS_BLUETOOTH { Link::Bluetooth } else { Link::Usb };
                crate::debug!("{}: Logitech HID++ node ({})", key, info.name);
                let be = hidpp::receiver::start(
                    t,
                    hidpp::receiver::Params {
                        key: key.clone(),
                        pid: info.pid,
                        connection,
                        short_ok: desc.has_report_id(0x10),
                    },
                    self.sink(),
                );
                self.backends.insert(key.clone(), Box::new(be));
                self.backend_nodes.insert(key, info);
            }
            0x1532 => {
                if razer::blackshark_x::is_model(info.pid) {
                    if !razer::blackshark_x::is_control_interface(&info.report_info()) {
                        return;
                    }
                    let t: Arc<dyn Transport> = match HidRaw::open(&info.path) {
                        Ok(t) => Arc::new(t),
                        Err(e) => return self.open_failed(&info, e),
                    };
                    let be = razer::blackshark_x::start(t, info.pid, key.clone(), self.sink());
                    self.backends.insert(key.clone(), Box::new(be));
                    self.backend_nodes.insert(key, info);
                    return;
                }
                if let Some(model) = razer::blackshark::find_model(info.pid) {
                    let desc = info.report_info();
                    if !razer::blackshark::is_control_interface(model, &desc, info.interface) {
                        return;
                    }
                    let t: Arc<dyn Transport> = match HidRaw::open(&info.path) {
                        Ok(t) => Arc::new(t),
                        Err(e) => return self.open_failed(&info, e),
                    };
                    let rf_wake = desc.has_report_id(0x05);
                    let be = razer::blackshark::start(t, model, key.clone(), rf_wake, self.sink());
                    self.backends.insert(key.clone(), Box::new(be));
                    self.backend_nodes.insert(key, info);
                    return;
                }
                if let Some(model) = razer::kraken::find_model(info.pid) {
                    if info.interface != Some(razer::kraken::INTERFACE) {
                        return;
                    }
                    let t: Arc<dyn Transport> = match HidRaw::open(&info.path) {
                        Ok(t) => Arc::new(t),
                        Err(e) => return self.open_failed(&info, e),
                    };
                    let be = razer::kraken::start(t, model, key.clone(), self.sink());
                    self.backends.insert(key.clone(), Box::new(be));
                    self.backend_nodes.insert(key, info);
                    return;
                }
                let Some(model) = razer::find_model(info.pid) else {
                    crate::debug!("{}: unknown Razer product {:04x}", key, info.pid);
                    self.warn_once(format!(
                        "{} (1532:{:04x}) isn't supported yet. Please report it with the output of: lsusb -d 1532:{:04x} -v",
                        info.name, info.pid, info.pid
                    ));
                    return;
                };
                if info.interface.unwrap_or(0) != model.iface {
                    return;
                }
                let t: Arc<dyn Transport> = match HidRaw::open(&info.path) {
                    Ok(t) => Arc::new(t),
                    Err(e) => return self.open_failed(&info, e),
                };
                let wireless = model.name.contains("Wireless") || model.name.contains("Receiver") || model.name.contains("HyperSpeed");
                let link = if wireless { Link::Receiver } else { Link::Usb };
                let be = razer::device::start(t, model, key.clone(), link, self.sink());
                self.backends.insert(key.clone(), Box::new(be));
                self.backend_nodes.insert(key, info);
            }
            _ => {}
        }
    }

    fn open_failed(&mut self, info: &HidInfo, e: std::io::Error) {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            self.warn_once(format!(
                "No permission to open {} ({}). Install the Peridot udev rules and replug the device.",
                info.path.display(),
                info.name
            ));
        } else {
            crate::warn!("cannot open {}: {}", info.path.display(), e);
        }
    }

    // ---------------------------------------------------------- events -----

    pub fn run(&mut self) {
        let mut last_scan = Instant::now();
        loop {
            match self.rx.recv_timeout(Duration::from_secs(1)) {
                Ok(Event::Quit) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(ev) => self.handle(ev),
                Err(RecvTimeoutError::Timeout) => {}
            }
            if crate::sys::terminating() {
                crate::info!("shutting down");
                break;
            }
            if last_scan.elapsed() > Duration::from_secs(10) {
                last_scan = Instant::now();
                self.rescan();
            }
        }
        self.softeq.shutdown();
        for (_, b) in self.backends.drain() {
            b.stop();
        }
    }

    pub fn handle(&mut self, ev: Event) {
        match ev {
            Event::Hotplug(Hotplug::Added(name)) => {
                if let Some(info) = hid::info_for(&name) {
                    self.consider(info);
                }
            }
            Event::Hotplug(Hotplug::Removed(name)) => {
                self.seen.remove(&name);
                if let Some(b) = self.backends.get(&name) {
                    b.stop();
                }
            }
            Event::Backend(be) => self.handle_backend(be),
            Event::Call(msg) => crate::service::handle_call(self, msg),
            Event::Applied(a) => self.finish_apply(a),
            Event::Audio(id, status) => {
                if let Some(pc) = self.pc.get_mut(&id) {
                    if pc.status != status {
                        pc.status = status;
                        self.refresh_snapshot(&id);
                    }
                }
            }
            Event::Quit => {}
        }
    }

    fn emit(&self, member: &str, arg: String) {
        if let Some(c) = &self.conn {
            let _ = c.emit(crate::OBJECT_PATH, crate::INTERFACE, member, vec![dbus::s(arg)]);
        }
    }

    fn handle_backend(&mut self, ev: BackendEvent) {
        match ev {
            BackendEvent::Ready(handle) => {
                let (id, snap) = backend::snapshot(&handle);
                if !self.pc.contains_key(&id) {
                    let saved = self.store.settings(&id);
                    if let Some((_, gains)) = saved.iter().find(|(k, _)| k == "equalizer_custom") {
                        self.custom_eq.insert(id.clone(), gains.clone());
                    }
                    let surround = saved.iter().any(|(k, v)| k == "surround" && v.as_bool() == Some(true));
                    self.pc.insert(id.clone(), PcAudio { surround, ..Default::default() });
                }
                let snap = self.decorate(&id, snap);
                let new = !self.devices.contains_key(&id);
                let worker = match self.devices.get(&id) {
                    Some(e) => e.worker.clone(),
                    None => worker_thread(&id),
                };
                self.devices.insert(id.clone(), Entry { handle: handle.clone(), snapshot: snap.clone(), worker });
                if new {
                    self.order.push(id.clone());
                    crate::info!("device ready: {} ({})", snap.get("name").as_str().unwrap_or("?"), id);
                    self.emit("DeviceAdded", snap.to_string_compact());
                } else {
                    self.emit("DeviceChanged", snap.to_string_compact());
                }
                self.replay_settings(&id);
                self.sync_pc_audio(&id);
            }
            BackendEvent::Changed(id, snap) => {
                let snap = self.decorate(&id, snap);
                if let Some(e) = self.devices.get_mut(&id) {
                    if e.snapshot != snap {
                        e.snapshot = snap.clone();
                        self.emit("DeviceChanged", snap.to_string_compact());
                    }
                }
            }
            BackendEvent::Removed(id) => {
                if self.devices.remove(&id).is_some() {
                    self.order.retain(|x| *x != id);
                    self.engine.drop_device(&id);
                    self.softeq.stop(&id);
                    self.pc.remove(&id);
                    crate::info!("device removed: {}", id);
                    self.emit("DeviceRemoved", id);
                }
            }
            BackendEvent::Button { device, control, pressed } => self.on_button(&device, &control, pressed),
            BackendEvent::Closed(key) => {
                self.backends.remove(&key);
                self.backend_nodes.remove(&key);
                self.seen.remove(&key);
            }
        }
    }

    // ---------------------------------------------------------- settings ---

    /// Queue a setting change on the device's worker.
    pub fn apply(&mut self, id: &str, key: &str, value: Json, reply_to: Option<dbus::Message>, persist: bool) {
        if PC_KEYS.contains(&key) && self.devices.contains_key(id) {
            let result = self.apply_pc(id, key, &value, persist);
            if let (Some(c), Some(call)) = (&self.conn, &reply_to) {
                match result {
                    Ok(()) => {
                        let snap = self.device_json(id).unwrap_or(Json::Null);
                        let _ = c.reply(call, vec![dbus::s(snap.to_string_compact())]);
                    }
                    Err(e) => {
                        let _ = c.reply_error(call, e.dbus_name(), &e.to_string());
                    }
                }
            }
            return;
        }
        // {"gains": [...], "custom": true} is the user's own curve (not a
        // preset): remember it so they can go back to it.
        let value = if key == "equalizer" && value.get("custom").as_bool() == Some(true) {
            let gains = value.get("gains").clone();
            if gains.as_arr().is_some() && self.devices.contains_key(id) {
                if persist {
                    let name = self.devices[id].snapshot.get("name").as_str().unwrap_or_default().to_string();
                    self.store.remember(id, &name, "equalizer_custom", gains.clone());
                }
                self.custom_eq.insert(id.to_string(), gains.clone());
            }
            gains
        } else {
            value
        };
        let Some(entry) = self.devices.get(id) else {
            if let (Some(c), Some(call)) = (&self.conn, &reply_to) {
                let e = model::Error::NotFound(id.into());
                let _ = c.reply_error(call, e.dbus_name(), &e.to_string());
            }
            return;
        };
        let handle = entry.handle.clone();
        let tx = self.tx.clone();
        let (id, key) = (id.to_string(), key.to_string());
        let _ = entry.worker.send(Box::new(move || {
            let result = {
                let mut d = backend::lock(&handle);
                d.apply(&key, &value).map(|_| d.state().to_json())
            };
            let _ = tx.send(Event::Applied(Applied { id, key, value, reply_to, persist, result }));
        }));
    }

    pub fn refresh(&mut self, id: &str, reply_to: Option<dbus::Message>) {
        let Some(entry) = self.devices.get(id) else { return };
        let handle = entry.handle.clone();
        let tx = self.tx.clone();
        let id = id.to_string();
        let _ = entry.worker.send(Box::new(move || {
            let result = {
                let mut d = backend::lock(&handle);
                d.refresh().map(|_| d.state().to_json())
            };
            let _ = tx.send(Event::Applied(Applied {
                id,
                key: "refresh".into(),
                value: Json::Null,
                reply_to,
                persist: false,
                result,
            }));
        }));
    }

    fn finish_apply(&mut self, mut a: Applied) {
        if let Ok(s) = &a.result {
            a.result = Ok(self.decorate(&a.id, s.clone()));
        }
        let snapshot = match &a.result {
            Ok(s) => Some(s.clone()),
            Err(_) => self.devices.get(&a.id).map(|e| backend::lock(&e.handle).state().to_json()).map(|s| self.decorate(&a.id, s)),
        };
        // Persist on success, and also when the device is merely asleep so the
        // change lands the next time it wakes up.
        let keep = a.persist && matches!(a.result, Ok(_) | Err(model::Error::Offline));
        if keep {
            if let Some(skey) = config::storage_key(&a.key, &a.value) {
                let name = snapshot.as_ref().and_then(|s| s.get("name").as_str().map(String::from)).unwrap_or_default();
                // DPI is stored as the resulting stage set when we know it; a
                // plain value set while the device was never seen is kept as-is.
                let dpi_now = snapshot.as_ref().map(|s| s.get("dpi").clone()).unwrap_or(Json::Null);
                let entry = if skey != "dpi_stages" {
                    Some((skey, a.value.clone()))
                } else if !dpi_now.is_null() && a.result.is_ok() {
                    let v = Json::obj().with("stages", dpi_now.get("stages").clone()).with("active", dpi_now.get("active_stage").clone());
                    Some((skey, v))
                } else if a.key == "dpi" {
                    Some(("dpi".to_string(), a.value.clone()))
                } else if a.key == "dpi_stages" {
                    Some((skey, a.value.clone()))
                } else {
                    None
                };
                if let Some((k, v)) = entry {
                    self.store.remember(&a.id, &name, &k, v);
                }
            }
        }
        if let Some(snap) = snapshot {
            if let Some(e) = self.devices.get_mut(&a.id) {
                if e.snapshot != snap {
                    e.snapshot = snap.clone();
                    self.emit("DeviceChanged", snap.to_string_compact());
                }
            }
            if a.key == "button" || a.key == "replay" {
                self.sync_software_remaps(&a.id);
            }
            if a.key == "equalizer" || a.key == "replay" {
                self.sync_pc_audio(&a.id);
            }
        }
        if let (Some(c), Some(call)) = (&self.conn, &a.reply_to) {
            match &a.result {
                Ok(s) => {
                    let _ = c.reply(call, vec![dbus::s(s.to_string_compact())]);
                }
                Err(e) => {
                    let _ = c.reply_error(call, e.dbus_name(), &e.to_string());
                }
            }
        } else if let Err(e) = &a.result {
            if a.key != "refresh" && !matches!(e, model::Error::Offline) {
                crate::warn!("{}: applying {} failed: {}", a.id, a.key, e);
            }
        }
    }

    /// Re-apply every saved setting after a device (re)appears.
    fn replay_settings(&mut self, id: &str) {
        // Computer-side settings were loaded on Ready; the device has no use for them.
        let plan: Vec<(String, Json)> =
            self.store.replay_plan(id).into_iter().filter(|(k, _)| !PC_KEYS.contains(&k.as_str())).collect();
        let Some(entry) = self.devices.get(id) else { return };
        if !entry.snapshot.get("online").as_bool().unwrap_or(false) {
            return;
        }
        if plan.is_empty() {
            self.sync_software_remaps(id);
            return;
        }
        crate::debug!("{}: re-applying {} saved settings", id, plan.len());
        let handle = entry.handle.clone();
        let tx = self.tx.clone();
        let id = id.to_string();
        let _ = entry.worker.send(Box::new(move || {
            let mut failures = Vec::new();
            let snap = {
                let mut d = backend::lock(&handle);
                for (key, value) in &plan {
                    if let Err(e) = d.apply(key, value) {
                        failures.push(format!("{}: {}", key, e));
                    }
                }
                d.state().to_json()
            };
            for f in &failures {
                crate::debug!("{}: replay {}", id, f);
            }
            let _ = tx.send(Event::Applied(Applied {
                id,
                key: "replay".into(),
                value: Json::Null,
                reply_to: None,
                persist: false,
                result: Ok(snap),
            }));
        }));
    }

    // ---------------------------------------------------------- buttons ----

    fn control_action(&self, device: &str, control: &str) -> Option<Action> {
        let snap = &self.devices.get(device)?.snapshot;
        let controls = snap.get("buttons").get("controls").as_arr()?;
        let c = controls.iter().find(|c| c.get("id").as_str() == Some(control))?;
        Action::parse(c.get("action").as_str()?).ok()
    }

    fn on_button(&mut self, device: &str, control: &str, pressed: bool) {
        let Some(action) = self.control_action(device, control) else { return };
        crate::debug!("{}: control {} {} -> {}", device, control, if pressed { "down" } else { "up" }, action.to_string_canonical());
        match action {
            Action::Default | Action::Disabled => {}
            Action::Button(_) | Action::Keys(_) => {
                self.engine.perform(&action, pressed);
            }
            Action::Dpi(DpiOp::Up) if pressed => self.apply(device, "dpi_cycle", Json::from("up"), None, true),
            Action::Dpi(DpiOp::Down) if pressed => self.apply(device, "dpi_cycle", Json::from("down"), None, true),
            Action::Dpi(DpiOp::Shift) => self.apply(device, "dpi_shift", Json::Bool(pressed), None, false),
            Action::Dpi(_) => {}
        }
    }

    // ------------------------------------------------- computer-side audio --

    /// Headsets whose USB audio PipeWire can process: anything we know the USB
    /// ids of.
    fn pc_capable(snap: &Json) -> Option<(u16, u16)> {
        if snap.get("kind").as_str() != Some("headset") {
            return None;
        }
        parse_usb_id(snap.get("model").as_str().unwrap_or(""))
    }

    /// Add what the manager knows to a device snapshot: the user's custom EQ
    /// curve and `audio.pc` (computer-side settings).
    fn decorate(&self, id: &str, snap: Json) -> Json {
        let mut snap = self.decorate_custom_eq(id, snap);
        if Self::pc_capable(&snap).is_none() {
            return snap;
        }
        let Some(pc) = self.pc.get(id) else { return snap };
        let section = Json::obj()
            .with("surround", pc.surround)
            .with("active", pc.status.active)
            .with("format", pc.status.format.clone())
            .with("error", pc.status.error.clone());
        if snap.get("audio").is_null() {
            snap.set("audio", Json::obj());
        }
        if let Some(audio) = snap.get_mut("audio") {
            audio.set("pc", section);
        }
        snap
    }

    fn decorate_custom_eq(&self, id: &str, mut snap: Json) -> Json {
        if let (Some(custom), Some(eq)) =
            (self.custom_eq.get(id), snap.get_mut("audio").and_then(|a| a.get_mut("equalizer")))
        {
            if !eq.is_null() {
                eq.set("custom", custom.clone());
            }
        }
        snap
    }

    /// Re-decorate a device's stored snapshot and tell the UI if it changed.
    fn refresh_snapshot(&mut self, id: &str) {
        let Some(e) = self.devices.get(id) else { return };
        let snap = self.decorate(id, e.snapshot.clone());
        if let Some(e) = self.devices.get_mut(id) {
            if e.snapshot != snap {
                e.snapshot = snap.clone();
                self.emit("DeviceChanged", snap.to_string_compact());
            }
        }
    }

    fn apply_pc(&mut self, id: &str, key: &str, value: &Json, persist: bool) -> Result<(), model::Error> {
        let snap = self.devices.get(id).map(|e| e.snapshot.clone()).unwrap_or(Json::Null);
        if Self::pc_capable(&snap).is_none() {
            return Err(model::Error::NotSupported(key.replace('_', " ")));
        }
        let on = value.as_bool().ok_or_else(|| model::Error::Invalid(format!("{} expects true or false", key)))?;
        self.pc.entry(id.to_string()).or_default().surround = on;
        if persist {
            let name = snap.get("name").as_str().unwrap_or_default().to_string();
            self.store.remember(id, &name, key, Json::Bool(on));
        }
        self.refresh_snapshot(id);
        self.sync_pc_audio(id);
        Ok(())
    }

    /// Bring the headset's PipeWire filter in line with its settings.
    fn sync_pc_audio(&mut self, id: &str) {
        let Some(entry) = self.devices.get(id) else { return };
        let Some(usb) = Self::pc_capable(&entry.snapshot) else { return };
        let Some(pc) = self.pc.get(id) else { return };
        let eq = entry.snapshot.get("audio").get("equalizer");
        let eq = (eq.get("software").as_bool() == Some(true)).then(|| {
            let bands = eq.get("bands").as_arr().cloned().unwrap_or_default();
            (
                bands.iter().map(|b| b.get("freq").as_i64().unwrap_or(0) as u32).collect(),
                bands.iter().map(|b| b.get("gain").as_i64().unwrap_or(0) as i32).collect(),
            )
        });
        self.softeq.apply(softeq::Request {
            id: id.to_string(),
            name: entry.snapshot.get("name").as_str().unwrap_or("Headset").to_string(),
            usb,
            eq,
            surround: pc.surround,
        });
    }

    /// For devices without hardware diversion (Razer), grab their input node
    /// while any button is remapped.
    fn sync_software_remaps(&mut self, id: &str) {
        let Some(entry) = self.devices.get(id) else { return };
        if entry.snapshot.get("vendor").as_str() != Some("razer") {
            return;
        }
        let mut mapped: Vec<(u16, String)> = Vec::new();
        if let Some(controls) = entry.snapshot.get("buttons").get("controls").as_arr() {
            for c in controls {
                let action = c.get("action").as_str().unwrap_or("default");
                if action != "default" {
                    if let Some(code) = c.get("id").as_str().and_then(|s| u16::from_str_radix(s, 16).ok()) {
                        mapped.push((code, action.to_string()));
                    }
                }
            }
        }
        let nodes = self.event_nodes_for(id);
        if !mapped.is_empty() {
            let _ = self.engine.ensure_vinput();
        }
        let tx = Mutex::new(self.tx.clone());
        let tx = Arc::new(tx);
        let dev = id.to_string();
        self.engine.sync_grabs(id, mapped, &nodes, move |code, pressed| {
            let _ = tx.lock().unwrap().send(Event::Backend(BackendEvent::Button {
                device: dev.clone(),
                control: format!("{:04x}", code),
                pressed,
            }));
        });
    }

    /// evdev nodes of every interface of the USB device behind a Razer id.
    fn event_nodes_for(&self, id: &str) -> Vec<PathBuf> {
        let model = self.devices.get(id).and_then(|e| e.snapshot.get("model").as_str().map(String::from));
        let Some(model) = model else { return vec![] };
        let usb: Vec<PathBuf> = self
            .backend_nodes
            .values()
            .filter(|i| format!("{:04x}:{:04x}", i.vid, i.pid) == model)
            .filter_map(|i| i.usb_device.clone())
            .collect();
        let mut out = Vec::new();
        for info in hid::enumerate() {
            if info.usb_device.as_ref().map_or(false, |u| usb.contains(u)) {
                out.extend(hid::event_nodes_for(&info.sys_name));
            }
        }
        out.sort();
        out.dedup();
        out
    }

    // ---------------------------------------------------------- queries ----

    pub fn list_json(&self) -> Json {
        Json::Arr(self.order.iter().filter_map(|id| self.devices.get(id)).map(|e| e.snapshot.clone()).collect())
    }

    pub fn device_json(&self, id: &str) -> Option<Json> {
        self.devices.get(id).map(|e| e.snapshot.clone())
    }

    pub fn status_json(&mut self) -> Json {
        // Retried on every call, so actions start working once uinput appears.
        let uinput_ok = self.engine.ensure_vinput().is_ok();
        let uinput_error = if uinput_ok { None } else { self.engine.last_error().map(String::from) };
        Json::obj()
            .with("version", crate::VERSION)
            .with("uinput_error", uinput_error)
            .with("devices", self.devices.len())
            .with("uinput", uinput_ok)
            .with("config_dir", self.store.dir().display().to_string())
            .with("warnings", self.warnings.clone())
    }

    pub fn forget(&mut self, id: &str) {
        self.store.forget(id);
        self.custom_eq.remove(id);
    }

    pub fn has_device(&self, id: &str) -> bool {
        self.devices.contains_key(id)
    }

    pub fn conn(&self) -> Option<&Arc<Connection>> {
        self.conn.as_ref()
    }

    pub fn backend_count(&self) -> usize {
        self.backends.len()
    }
}

/// The USB ids at the start of a device model, "046d:0b1f" -> (0x046d, 0x0b1f).
fn parse_usb_id(model: &str) -> Option<(u16, u16)> {
    let mut parts = model.split(':');
    let v = u16::from_str_radix(parts.next()?, 16).ok()?;
    let p = u16::from_str_radix(parts.next()?, 16).ok()?;
    Some((v, p))
}
