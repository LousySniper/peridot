//! Game controllers (DualSense, DualShock, Xbox, Switch Pro, 8BitDo...):
//! battery from the kernel's power_supply class, and a stick-drift fix.
//!
//! The kernel drivers already do the protocol work, so a controller here is
//! its evdev node plus the battery the HID driver exposes next to it. The
//! drift fix grabs that node and mirrors it through a uinput clone (same
//! name and ids, so games map it the same way) with the sticks re-centred and
//! a scaled dead zone applied. Force feedback sent to the clone is passed on
//! to the real controller, so rumble keeps working.

use crate::backend::{Backend, BackendEvent, DeviceHandle, Sink};
use crate::json::Json;
use crate::model::{self, Battery, Charge, Connection, DeviceState, Kind};
use crate::sys::{self, input_absinfo, input_event, input_id, uinput_abs_setup, uinput_setup};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const O_NONBLOCK: i32 = 0o4000;
const O_CLOEXEC: i32 = 0o2000000;
const BATTERY_REFRESH: Duration = Duration::from_secs(30);
/// Dead zone limits, in percent of the stick's travel.
pub const MAX_DEADZONE: u8 = 30;
const DEFAULT_DEADZONE: u8 = 8;
/// How long calibration samples the resting sticks.
const CALIBRATION: Duration = Duration::from_secs(2);
/// Vendor name prefixes that only make controller names long.
const VENDOR_PREFIXES: &[&str] = &["Sony Interactive Entertainment ", "Sony Computer Entertainment "];

/// A controller found in /sys/class/input.
#[derive(Clone, Debug, PartialEq)]
pub struct PadInfo {
    /// "event15"
    pub event: String,
    pub name: String,
    pub uniq: String,
    pub bus: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
    /// The power_supply directory with the battery, if the driver has one.
    pub battery: Option<PathBuf>,
}

impl PadInfo {
    pub fn node(&self) -> PathBuf {
        Path::new("/dev/input").join(&self.event)
    }

    pub fn id(&self) -> String {
        let uniq: String = self.uniq.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_lowercase();
        if uniq.is_empty() {
            format!("controller-{:04x}{:04x}-{}", self.vendor, self.product, self.event)
        } else {
            format!("controller-{}", uniq)
        }
    }

    pub fn display_name(&self) -> String {
        let mut n = self.name.as_str();
        for p in VENDOR_PREFIXES {
            n = n.strip_prefix(p).unwrap_or(n);
        }
        n.trim().to_string()
    }
}

fn read(p: &Path) -> Option<String> {
    std::fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

/// Parse a sysfs capability bitmap ("7fdb000000000000 0 0 0 0", highest word
/// first, 64-bit words) and test bit `n`.
pub fn has_bit(bitmap: &str, n: usize) -> bool {
    let words: Vec<&str> = bitmap.split_whitespace().collect();
    let Some(i) = words.len().checked_sub(1 + n / 64) else { return false };
    u64::from_str_radix(words[i], 16).map_or(false, |w| w & (1 << (n % 64)) != 0)
}

/// Gamepads: a south face button and an X/Y stick, on real (not virtual)
/// hardware, so neither Peridot's own clones nor Steam's virtual pads count.
pub fn enumerate() -> Vec<PadInfo> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/sys/class/input") else { return out };
    for e in entries.flatten() {
        let event = e.file_name().to_string_lossy().to_string();
        if !event.starts_with("event") {
            continue;
        }
        let dev = e.path().join("device");
        let Ok(real) = std::fs::canonicalize(&dev) else { continue };
        if real.to_string_lossy().contains("/devices/virtual/") {
            continue;
        }
        let keys = read(&dev.join("capabilities/key")).unwrap_or_default();
        let abs = read(&dev.join("capabilities/abs")).unwrap_or_default();
        if !has_bit(&keys, sys::BTN_SOUTH as usize) || !has_bit(&abs, 0) || !has_bit(&abs, 1) {
            continue;
        }
        let hex = |f: &str| u16::from_str_radix(&read(&dev.join("id").join(f)).unwrap_or_default(), 16).unwrap_or(0);
        out.push(PadInfo {
            event,
            name: read(&dev.join("name")).unwrap_or_else(|| "Controller".into()),
            uniq: read(&dev.join("uniq")).unwrap_or_default(),
            bus: hex("bustype"),
            vendor: hex("vendor"),
            product: hex("product"),
            version: hex("version"),
            battery: real.parent().and_then(Path::parent).and_then(find_battery),
        });
    }
    out.sort_by(|a, b| a.event.cmp(&b.event));
    out
}

/// The battery a HID driver registered under the controller's HID device.
fn find_battery(hid: &Path) -> Option<PathBuf> {
    let dir = std::fs::read_dir(hid.join("power_supply")).ok()?;
    dir.flatten().map(|e| e.path()).find(|p| p.join("capacity").exists())
}

pub fn read_battery(dir: &Path) -> Option<Battery> {
    let percent = read(&dir.join("capacity")).and_then(|s| s.parse::<u8>().ok());
    let charge = match read(&dir.join("status")).as_deref() {
        Some("Charging") => Charge::Charging,
        Some("Full") => Charge::Full,
        _ => Charge::Discharging,
    };
    percent.map(|p| Battery { percent: Some(p.min(100)), charge, voltage_mv: None, estimated: false })
}

// ---------------------------------------------------------------- sticks --

/// One stick's correction: where it rests and how big the dead zone is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stick {
    /// Percent of travel ignored around the centre.
    pub deadzone: u8,
    /// Measured resting position (raw units); None until calibrated.
    pub center: Option<(i32, i32)>,
}

impl Default for Stick {
    fn default() -> Self {
        Stick { deadzone: DEFAULT_DEADZONE, center: None }
    }
}

impl Stick {
    fn to_json(self) -> Json {
        Json::obj()
            .with("deadzone", self.deadzone as i64)
            .with("center", self.center.map(|(x, y)| Json::Arr(vec![Json::from(x as i64), Json::from(y as i64)])))
    }

    fn merge(&mut self, v: &Json) {
        if let Some(d) = v.get("deadzone").as_i64() {
            self.deadzone = d.clamp(0, MAX_DEADZONE as i64) as u8;
        }
        match v.get("center") {
            Json::Null if v.has("center") => self.center = None,
            Json::Arr(a) if a.len() == 2 => {
                if let (Some(x), Some(y)) = (a[0].as_i64(), a[1].as_i64()) {
                    self.center = Some((x as i32, y as i32));
                }
            }
            _ => {}
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sticks {
    pub enabled: bool,
    pub left: Stick,
    pub right: Stick,
}

/// An axis pair's range, from EVIOCGABS.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Range {
    pub min: i32,
    pub max: i32,
}

impl Range {
    /// The value a centred stick reports: 128 on 0..255, 0 on -32768..32767.
    fn center(self) -> i32 {
        (self.min + self.max + 1).div_euclid(2)
    }
    fn half(self) -> f64 {
        (self.max as f64 - self.min as f64) / 2.0
    }
    /// -1..1 from `rest`, scaled separately on each side so that full travel
    /// still reaches the edge when the stick rests off-centre.
    fn normalise(self, v: i32, rest: f64) -> f64 {
        let span = if v as f64 >= rest { self.max as f64 - rest } else { rest - self.min as f64 };
        (v as f64 - rest) / span.max(1.0)
    }
    fn denormalise(self, v: f64) -> i32 {
        let c = self.center() as f64;
        let span = if v >= 0.0 { self.max as f64 - c } else { c - self.min as f64 };
        (c + v * span).round().clamp(self.min as f64, self.max as f64) as i32
    }
}

/// Re-centre a stick on its measured rest position and apply a scaled radial
/// dead zone: inside it the stick reads centred, outside it the remaining
/// travel is stretched back to the full range so nothing is lost.
pub fn correct(raw: (i32, i32), range: (Range, Range), stick: Stick) -> (i32, i32) {
    let (rx, ry) = range;
    let (cx, cy) = stick.center.unwrap_or((rx.center(), ry.center()));
    let vx = rx.normalise(raw.0, cx as f64);
    let vy = ry.normalise(raw.1, cy as f64);
    let r = (vx * vx + vy * vy).sqrt();
    let dz = stick.deadzone as f64 / 100.0;
    let (ox, oy) = if r <= dz {
        (0.0, 0.0)
    } else {
        let scale = ((r - dz) / (1.0 - dz)).min(1.0) / r;
        (vx * scale, vy * scale)
    };
    (rx.denormalise(ox), ry.denormalise(oy))
}

/// From resting samples: the average position and a dead zone a little above
/// the largest wobble, in percent of travel.
pub fn calibrate(samples: &[(i32, i32)], range: (Range, Range)) -> Stick {
    if samples.is_empty() {
        return Stick::default();
    }
    let n = samples.len() as f64;
    let cx = samples.iter().map(|s| s.0 as f64).sum::<f64>() / n;
    let cy = samples.iter().map(|s| s.1 as f64).sum::<f64>() / n;
    let wobble = samples
        .iter()
        .map(|s| {
            let dx = (s.0 as f64 - cx) / range.0.half().max(1.0);
            let dy = (s.1 as f64 - cy) / range.1.half().max(1.0);
            (dx * dx + dy * dy).sqrt()
        })
        .fold(0.0, f64::max);
    // Twice the wobble plus a little, so the stick reads still even when it
    // settles a bit off its measured rest after being moved.
    let deadzone = ((wobble * 2.0 + 0.03) * 100.0).ceil().clamp(3.0, MAX_DEADZONE as f64) as u8;
    Stick { deadzone, center: Some((cx.round() as i32, cy.round() as i32)) }
}

fn absinfo(file: &File, code: u16) -> io::Result<input_absinfo> {
    let mut info = input_absinfo::default();
    let buf = unsafe {
        std::slice::from_raw_parts_mut(&mut info as *mut input_absinfo as *mut u8, std::mem::size_of::<input_absinfo>())
    };
    sys::ioctl_buf(file.as_raw_fd(), sys::eviocgabs(code), buf)?;
    Ok(info)
}

fn caps(file: &File, ev: u8, max: usize) -> Vec<u16> {
    let mut bits = vec![0u8; max / 8 + 1];
    if sys::ioctl_buf(file.as_raw_fd(), sys::eviocgbit(ev, bits.len()), &mut bits).is_err() {
        return vec![];
    }
    (0..max).filter(|i| bits.get(i / 8).is_some_and(|b| b & (1 << (i % 8)) != 0)).map(|i| i as u16).collect()
}

fn props(file: &File) -> Vec<u16> {
    let mut bits = [0u8; 4];
    if sys::ioctl_buf(file.as_raw_fd(), sys::eviocgprop(bits.len()), &mut bits).is_err() {
        return vec![];
    }
    (0..32).filter(|i| bits[i / 8] & (1 << (i % 8)) != 0).map(|i| i as u16).collect()
}

fn open_node(path: &Path, write: bool) -> io::Result<File> {
    OpenOptions::new().read(true).write(write).custom_flags(O_NONBLOCK | O_CLOEXEC).open(path)
}

/// The axes of each stick on this controller and their ranges.
#[derive(Clone, Copy, Debug)]
struct Layout {
    left: (Range, Range),
    right: Option<(Range, Range)>,
}

fn layout(file: &File) -> io::Result<Layout> {
    let r = |code| absinfo(file, code).map(|i| Range { min: i.minimum, max: i.maximum });
    let abs = caps(file, sys::EV_ABS as u8, 0x40);
    let right = if abs.contains(&sys::ABS_RX) && abs.contains(&sys::ABS_RY) {
        Some((r(sys::ABS_RX)?, r(sys::ABS_RY)?))
    } else {
        None
    };
    Ok(Layout { left: (r(sys::ABS_X)?, r(sys::ABS_Y)?), right })
}

// ------------------------------------------------------------- forwarder --

/// While the drift fix is on: the grab, the uinput clone and the thread
/// copying events between them.
struct Forwarder {
    stop: Arc<AtomicBool>,
    sticks: Arc<Mutex<Sticks>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Forwarder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn to_event(chunk: &[u8]) -> input_event {
    let mut raw = [0u8; std::mem::size_of::<input_event>()];
    raw.copy_from_slice(chunk);
    unsafe { std::mem::transmute(raw) }
}

fn write_event(file: &File, type_: u16, code: u16, value: i32) -> io::Result<()> {
    let ev = input_event { sec: 0, usec: 0, type_, code, value };
    let bytes: [u8; std::mem::size_of::<input_event>()] = unsafe { std::mem::transmute(ev) };
    let mut f = file;
    f.write_all(&bytes)
}

/// A uinput device that looks like `real` (same name, ids, buttons, axes,
/// properties and force-feedback effects).
fn clone_device(real: &File, info: &PadInfo) -> io::Result<File> {
    let ui = OpenOptions::new().read(true).write(true).custom_flags(O_NONBLOCK | O_CLOEXEC).open(
        if Path::new("/dev/uinput").exists() { "/dev/uinput" } else { "/dev/input/uinput" },
    )?;
    let fd = ui.as_raw_fd();
    let set = |req, v: u16| sys::ioctl_int(fd, req, v as i32);
    set(sys::UI_SET_EVBIT, sys::EV_SYN)?;
    let keys = caps(real, sys::EV_KEY as u8, 0x300);
    if !keys.is_empty() {
        set(sys::UI_SET_EVBIT, sys::EV_KEY)?;
        for k in keys {
            set(sys::UI_SET_KEYBIT, k)?;
        }
    }
    let abs = caps(real, sys::EV_ABS as u8, 0x40);
    if !abs.is_empty() {
        set(sys::UI_SET_EVBIT, sys::EV_ABS)?;
        for a in &abs {
            set(sys::UI_SET_ABSBIT, *a)?;
            let setup = uinput_abs_setup { code: *a, absinfo: absinfo(real, *a)? };
            sys::ioctl_ptr(fd, sys::UI_ABS_SETUP, &setup)?;
        }
    }
    let msc = caps(real, sys::EV_MSC as u8, 0x08);
    if !msc.is_empty() {
        set(sys::UI_SET_EVBIT, sys::EV_MSC)?;
        for m in msc {
            set(sys::UI_SET_MSCBIT, m)?;
        }
    }
    let ff = caps(real, sys::EV_FF as u8, 0x80);
    if !ff.is_empty() {
        set(sys::UI_SET_EVBIT, sys::EV_FF)?;
        for f in &ff {
            set(sys::UI_SET_FFBIT, *f)?;
        }
    }
    for p in props(real) {
        set(sys::UI_SET_PROPBIT, p)?;
    }
    let mut setup = uinput_setup {
        id: input_id { bustype: info.bus, vendor: info.vendor, product: info.product, version: info.version },
        name: [0; 80],
        ff_effects_max: if ff.is_empty() { 0 } else { 16 },
    };
    let n = info.name.len().min(79);
    setup.name[..n].copy_from_slice(&info.name.as_bytes()[..n]);
    sys::ioctl_ptr(fd, sys::UI_DEV_SETUP, &setup)?;
    sys::ioctl_int(fd, sys::UI_DEV_CREATE, 0)?;
    Ok(ui)
}

/// Rumble: effects uploaded to the clone are uploaded to the real controller,
/// with ids translated between the two.
struct FfBridge {
    ids: HashMap<i16, i16>,
}

impl FfBridge {
    fn upload(&mut self, ui: &File, real: &File, request: i32) {
        let mut buf = [0u8; sys::FF_UPLOAD_SIZE];
        buf[0..4].copy_from_slice(&(request as u32).to_ne_bytes());
        if sys::ioctl_buf(ui.as_raw_fd(), sys::UI_BEGIN_FF_UPLOAD, &mut buf).is_err() {
            return;
        }
        let mut effect = [0u8; sys::FF_EFFECT_SIZE];
        effect.copy_from_slice(&buf[8..8 + sys::FF_EFFECT_SIZE]);
        let virtual_id = i16::from_ne_bytes([effect[2], effect[3]]);
        let real_id = self.ids.get(&virtual_id).copied().unwrap_or(-1);
        effect[2..4].copy_from_slice(&real_id.to_ne_bytes());
        let retval = match sys::ioctl_buf(real.as_raw_fd(), sys::EVIOCSFF, &mut effect) {
            Ok(_) => {
                self.ids.insert(virtual_id, i16::from_ne_bytes([effect[2], effect[3]]));
                0
            }
            Err(e) => -e.raw_os_error().unwrap_or(5),
        };
        buf[4..8].copy_from_slice(&retval.to_ne_bytes());
        let _ = sys::ioctl_buf(ui.as_raw_fd(), sys::UI_END_FF_UPLOAD, &mut buf);
    }

    fn erase(&mut self, ui: &File, real: &File, request: i32) {
        let mut buf = [0u8; sys::FF_ERASE_SIZE];
        buf[0..4].copy_from_slice(&(request as u32).to_ne_bytes());
        if sys::ioctl_buf(ui.as_raw_fd(), sys::UI_BEGIN_FF_ERASE, &mut buf).is_err() {
            return;
        }
        let virtual_id = u32::from_ne_bytes([buf[8], buf[9], buf[10], buf[11]]) as i16;
        let retval = match self.ids.remove(&virtual_id) {
            Some(real_id) => match sys::ioctl_int(real.as_raw_fd(), sys::EVIOCRMFF, real_id as i32) {
                Ok(()) => 0,
                Err(e) => -e.raw_os_error().unwrap_or(5),
            },
            None => 0,
        };
        buf[4..8].copy_from_slice(&retval.to_ne_bytes());
        let _ = sys::ioctl_buf(ui.as_raw_fd(), sys::UI_END_FF_ERASE, &mut buf);
    }

    /// Play/stop and gain events written to the clone.
    fn play(&self, real: &File, ev: &input_event) {
        let code = if ev.code < 0x60 { self.ids.get(&(ev.code as i16)).map(|id| *id as u16) } else { Some(ev.code) };
        if let Some(code) = code {
            let _ = write_event(real, sys::EV_FF, code, ev.value);
        }
    }
}

impl Forwarder {
    fn start(info: &PadInfo, sticks: Sticks) -> io::Result<Forwarder> {
        let real = open_node(&info.node(), true).or_else(|_| open_node(&info.node(), false))?;
        let layout = layout(&real)?;
        let ui = clone_device(&real, info)?;
        sys::ioctl_int(real.as_raw_fd(), sys::EVIOCGRAB, 1)?;
        crate::info!("fixing stick drift on {} ({})", info.node().display(), info.name);
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Mutex::new(sticks));
        let (flag, cfg) = (stop.clone(), shared.clone());
        let node = info.node();
        let thread = std::thread::Builder::new().name("stick-fix".into()).spawn(move || {
            forward(&real, &ui, layout, &cfg, &flag);
            let _ = sys::ioctl_int(real.as_raw_fd(), sys::EVIOCGRAB, 0);
            let _ = sys::ioctl_int(ui.as_raw_fd(), sys::UI_DEV_DESTROY, 0);
            crate::info!("stopped fixing stick drift on {}", node.display());
        })?;
        Ok(Forwarder { stop, sticks: shared, thread: Some(thread) })
    }
}

fn forward(real: &File, ui: &File, layout: Layout, sticks: &Mutex<Sticks>, stop: &AtomicBool) {
    let size = std::mem::size_of::<input_event>();
    let mut buf = vec![0u8; size * 64];
    let mut ff = FfBridge { ids: HashMap::new() };
    // Latest raw stick positions, starting from where they are now.
    let now = |code| absinfo(real, code).map(|i| i.value).unwrap_or(0);
    let mut left = (now(sys::ABS_X), now(sys::ABS_Y));
    let mut right = (now(sys::ABS_RX), now(sys::ABS_RY));
    let mut sent: HashMap<u16, i32> = HashMap::new();
    while !stop.load(Ordering::Relaxed) {
        let ready = match sys::poll_any(&[real.as_raw_fd(), ui.as_raw_fd()], 250) {
            Ok(r) => r,
            Err(_) => break,
        };
        if ready[1] {
            if let Ok(n) = (&*ui).read(&mut buf) {
                for chunk in buf[..n].chunks_exact(size) {
                    let ev = to_event(chunk);
                    match (ev.type_, ev.code) {
                        (sys::EV_UINPUT, sys::UI_FF_UPLOAD) => ff.upload(ui, real, ev.value),
                        (sys::EV_UINPUT, sys::UI_FF_ERASE) => ff.erase(ui, real, ev.value),
                        (sys::EV_FF, _) => ff.play(real, &ev),
                        _ => {}
                    }
                }
            }
        }
        if !ready[0] {
            continue;
        }
        let n = match (&*real).read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
            Err(_) => break, // unplugged
        };
        let cfg = sticks.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let mut out: Vec<(u16, u16, i32)> = Vec::new();
        for chunk in buf[..n].chunks_exact(size) {
            let ev = to_event(chunk);
            if ev.type_ == sys::EV_ABS {
                let pair = match ev.code {
                    sys::ABS_X => Some((&mut left.0, true)),
                    sys::ABS_Y => Some((&mut left.1, true)),
                    sys::ABS_RX if layout.right.is_some() => Some((&mut right.0, false)),
                    sys::ABS_RY if layout.right.is_some() => Some((&mut right.1, false)),
                    _ => None,
                };
                if let Some((slot, is_left)) = pair {
                    *slot = ev.value;
                    let ((x, y), (cx, cy)) = if is_left {
                        (correct(left, layout.left, cfg.left), (sys::ABS_X, sys::ABS_Y))
                    } else {
                        (correct(right, layout.right.unwrap_or(layout.left), cfg.right), (sys::ABS_RX, sys::ABS_RY))
                    };
                    for (code, v) in [(cx, x), (cy, y)] {
                        if sent.get(&code) != Some(&v) {
                            sent.insert(code, v);
                            out.push((sys::EV_ABS, code, v));
                        }
                    }
                    continue;
                }
            }
            out.push((ev.type_, ev.code, ev.value));
        }
        for (t, c, v) in out {
            if write_event(ui, t, c, v).is_err() {
                return;
            }
        }
    }
}

// ---------------------------------------------------------------- device --

pub struct GamepadDevice {
    info: PadInfo,
    /// Whether the controller has a right stick (ABS_RX/RY).
    has_right: bool,
    pub state: DeviceState,
    sticks: Sticks,
    forwarder: Option<Forwarder>,
    fix_error: Option<String>,
}

impl GamepadDevice {
    pub fn new(info: PadInfo) -> GamepadDevice {
        let state = DeviceState {
            id: info.id(),
            name: info.display_name(),
            vendor: "controller",
            kind: Kind::Controller,
            connection: if info.bus == sys::BUS_BLUETOOTH { Connection::Bluetooth } else { Connection::Usb },
            online: true,
            model: format!("{:04x}:{:04x}", info.vendor, info.product),
            serial: (!info.uniq.is_empty()).then(|| info.uniq.clone()),
            battery: info.battery.as_deref().and_then(read_battery),
            ..Default::default()
        };
        let has_right = open_node(&info.node(), false).ok().and_then(|f| layout(&f).ok()).map_or(true, |l| l.right.is_some());
        let mut d = GamepadDevice { info, has_right, state, sticks: Sticks::default(), forwarder: None, fix_error: None };
        d.publish();
        d
    }

    /// Mirror the stick settings into the snapshot.
    fn publish(&mut self) {
        let mut j = Json::obj()
            .with("enabled", self.sticks.enabled)
            .with("left", self.sticks.left.to_json())
            .with("active", self.forwarder.is_some())
            .with("error", self.fix_error.clone());
        if self.has_right {
            j.set("right", self.sticks.right.to_json());
        }
        self.state.sticks = Some(j);
    }

    fn sync_forwarder(&mut self) {
        if !self.sticks.enabled {
            self.forwarder = None;
            self.fix_error = None;
        } else if let Some(f) = &self.forwarder {
            *f.sticks.lock().unwrap_or_else(|p| p.into_inner()) = self.sticks.clone();
        } else {
            match Forwarder::start(&self.info, self.sticks.clone()) {
                Ok(f) => {
                    self.forwarder = Some(f);
                    self.fix_error = None;
                }
                Err(e) => {
                    crate::warn!("{}: can't fix stick drift: {}", self.state.name, e);
                    self.fix_error = Some(format!("Peridot can't take over this controller ({})", e));
                }
            }
        }
        self.publish();
    }

    /// Sample the resting sticks and set their centres and dead zones.
    fn calibrate(&mut self) -> model::Result<()> {
        let file = open_node(&self.info.node(), false)?;
        let layout = layout(&file)?;
        let mut left = Vec::new();
        let mut right = Vec::new();
        let start = Instant::now();
        while start.elapsed() < CALIBRATION {
            let v = |code| absinfo(&file, code).map(|i| i.value);
            left.push((v(sys::ABS_X)?, v(sys::ABS_Y)?));
            if layout.right.is_some() {
                right.push((v(sys::ABS_RX)?, v(sys::ABS_RY)?));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.sticks.left = calibrate(&left, layout.left);
        if let Some(r) = layout.right {
            self.sticks.right = calibrate(&right, r);
        }
        self.sticks.enabled = true;
        crate::info!(
            "{}: sticks calibrated: left {:?} {}%, right {:?} {}%",
            self.state.name,
            self.sticks.left.center,
            self.sticks.left.deadzone,
            self.sticks.right.center,
            self.sticks.right.deadzone
        );
        self.sync_forwarder();
        Ok(())
    }
}

impl model::Device for GamepadDevice {
    fn state(&self) -> &DeviceState {
        &self.state
    }

    fn refresh(&mut self) -> model::Result<()> {
        self.state.battery = self.info.battery.as_deref().and_then(read_battery);
        Ok(())
    }

    fn apply(&mut self, key: &str, value: &Json) -> model::Result<()> {
        match key {
            "sticks" => {
                if let Some(on) = value.get("enabled").as_bool() {
                    self.sticks.enabled = on;
                }
                self.sticks.left.merge(value.get("left"));
                self.sticks.right.merge(value.get("right"));
                self.sync_forwarder();
                match &self.fix_error {
                    Some(e) if self.sticks.enabled => Err(model::Error::Device(e.clone())),
                    _ => Ok(()),
                }
            }
            "calibrate_sticks" => self.calibrate(),
            _ => Err(model::Error::NotSupported(key.into())),
        }
    }
}

// --------------------------------------------------------------- backend --

pub struct GamepadBackend {
    stop: Arc<AtomicBool>,
}

impl Backend for GamepadBackend {
    fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Watch one controller: announce it, keep its battery fresh, and report it
/// gone when its node disappears.
pub fn start(info: PadInfo, sink: Sink) -> GamepadBackend {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let _ = std::thread::Builder::new().name(format!("pad-{}", info.event)).spawn(move || {
        let key = info.event.clone();
        let node = info.node();
        let dev = Arc::new(Mutex::new(GamepadDevice::new(info)));
        let id = dev.lock().unwrap_or_else(|p| p.into_inner()).state.id.clone();
        let handle: DeviceHandle = dev.clone();
        sink(BackendEvent::Ready(handle));
        let mut last = Instant::now();
        while !flag.load(Ordering::Relaxed) && node.exists() {
            std::thread::sleep(Duration::from_millis(500));
            if last.elapsed() >= BATTERY_REFRESH {
                last = Instant::now();
                let mut d = dev.lock().unwrap_or_else(|p| p.into_inner());
                let _ = model::Device::refresh(&mut *d);
                sink(BackendEvent::Changed(id.clone(), d.state.to_json()));
            }
        }
        // Stop forwarding before the device goes away.
        dev.lock().unwrap_or_else(|p| p.into_inner()).forwarder = None;
        sink(BackendEvent::Removed(id));
        sink(BackendEvent::Closed(key));
    });
    GamepadBackend { stop }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAD: (Range, Range) = (Range { min: 0, max: 255 }, Range { min: 0, max: 255 });

    #[test]
    fn reads_sysfs_bitmaps() {
        let keys = "7fdb000000000000 0 0 0 0";
        assert!(has_bit(keys, 0x130)); // BTN_SOUTH
        assert!(!has_bit(keys, 0x120));
        assert!(has_bit("3003f", 0) && has_bit("3003f", 4) && has_bit("3003f", 0x11));
        assert!(!has_bit("3003f", 6));
        assert!(!has_bit("", 0));
    }

    #[test]
    fn drift_inside_the_dead_zone_reads_centred() {
        let stick = Stick { deadzone: 8, center: Some((131, 125)) };
        // Resting a few units off nominal centre: centred after correction.
        assert_eq!(correct((133, 124), PAD, stick), (128, 128));
        assert_eq!(correct((131, 125), PAD, stick), (128, 128));
    }

    #[test]
    fn full_deflection_still_reaches_the_edge() {
        let stick = Stick { deadzone: 10, center: Some((130, 128)) };
        assert_eq!(correct((255, 128), PAD, stick).0, 255);
        assert_eq!(correct((0, 128), PAD, stick).0, 0);
        assert_eq!(correct((128, 0), PAD, stick).1, 0);
        // Just past the dead zone the output starts near centre, not with a jump.
        let (x, _) = correct((130 + 15, 128), PAD, stick);
        assert!((128..=132).contains(&x), "{}", x);
    }

    #[test]
    fn signed_ranges_work_too() {
        let r = Range { min: -32768, max: 32767 };
        let stick = Stick { deadzone: 5, center: None };
        assert_eq!(correct((900, -700), (r, r), stick), (0, 0));
        assert_eq!(correct((32767, 0), (r, r), stick).0, 32767);
    }

    #[test]
    fn calibration_finds_the_rest_position_and_a_dead_zone_above_the_wobble() {
        let samples = [(131, 125), (132, 125), (131, 126), (130, 125), (131, 124)];
        let s = calibrate(&samples, PAD);
        assert_eq!(s.center, Some((131, 125)));
        assert!((3..=8).contains(&s.deadzone), "{}", s.deadzone);
        let shaky: Vec<(i32, i32)> = (0..20).map(|i| (128 + (i % 5) * 4 - 8, 128)).collect();
        assert!(calibrate(&shaky, PAD).deadzone > s.deadzone);
    }

    #[test]
    fn ids_and_names() {
        let info = PadInfo {
            event: "event15".into(),
            name: "Sony Interactive Entertainment DualSense Wireless Controller".into(),
            uniq: "aa:bb:cc:dd:ee:ff".into(),
            bus: 3,
            vendor: 0x054c,
            product: 0x0ce6,
            version: 0x8111,
            battery: None,
        };
        assert_eq!(info.id(), "controller-aabbccddeeff");
        assert_eq!(info.display_name(), "DualSense Wireless Controller");
        let nameless = PadInfo { uniq: String::new(), ..info };
        assert_eq!(nameless.id(), "controller-054c0ce6-event15");
    }

    #[test]
    fn stick_settings_merge() {
        let mut s = Stick::default();
        s.merge(&Json::parse(r#"{"deadzone": 50, "center": [130, 127]}"#).unwrap());
        assert_eq!(s, Stick { deadzone: MAX_DEADZONE, center: Some((130, 127)) });
        s.merge(&Json::parse(r#"{"center": null}"#).unwrap());
        assert_eq!(s.center, None);
    }
}
