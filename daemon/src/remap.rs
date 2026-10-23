//! Software button actions.
//!
//! - [`VirtualInput`]: a uinput device that emits the keys/buttons an action
//!   asks for.
//! - [`Grab`]: for mice without hardware diversion (Razer), grab the mouse's
//!   evdev node, swallow remapped buttons and mirror everything else through a
//!   uinput clone. Only active while at least one button is remapped.

use crate::actions::{self, Action};
use crate::sys::{self, input_event, input_id, uinput_setup};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const O_NONBLOCK: i32 = 0o4000;
const O_CLOEXEC: i32 = 0o2000000;

pub struct VirtualInput {
    file: File,
}

fn uinput_path() -> &'static str {
    if Path::new("/dev/uinput").exists() {
        "/dev/uinput"
    } else {
        "/dev/input/uinput"
    }
}

impl VirtualInput {
    /// Create a uinput device advertising the given key and relative codes.
    pub fn create(name: &str, keys: &[u16], rels: &[u16], id: (u16, u16)) -> io::Result<VirtualInput> {
        let file = OpenOptions::new().write(true).custom_flags(O_NONBLOCK | O_CLOEXEC).open(uinput_path())?;
        let fd = file.as_raw_fd();
        sys::ioctl_int(fd, sys::UI_SET_EVBIT, sys::EV_KEY as i32)?;
        sys::ioctl_int(fd, sys::UI_SET_EVBIT, sys::EV_SYN as i32)?;
        for k in keys {
            sys::ioctl_int(fd, sys::UI_SET_KEYBIT, *k as i32)?;
        }
        if !rels.is_empty() {
            sys::ioctl_int(fd, sys::UI_SET_EVBIT, sys::EV_REL as i32)?;
            for r in rels {
                sys::ioctl_int(fd, sys::UI_SET_RELBIT, *r as i32)?;
            }
        }
        let mut setup = uinput_setup {
            id: input_id { bustype: sys::BUS_VIRTUAL, vendor: id.0, product: id.1, version: 1 },
            name: [0; 80],
            ff_effects_max: 0,
        };
        let n = name.len().min(79);
        setup.name[..n].copy_from_slice(&name.as_bytes()[..n]);
        sys::ioctl_ptr(fd, sys::UI_DEV_SETUP, &setup)?;
        sys::ioctl_int(fd, sys::UI_DEV_CREATE, 0)?;
        Ok(VirtualInput { file })
    }

    /// The device used to perform button actions.
    pub fn for_actions() -> io::Result<VirtualInput> {
        let rels = [sys::REL_X, sys::REL_Y, sys::REL_WHEEL, sys::REL_HWHEEL];
        VirtualInput::create("Peridot virtual input", &actions::all_emittable_codes(), &rels, (0x1209, 0x5045))
    }

    pub fn emit(&self, events: &[(u16, u16, i32)]) -> io::Result<()> {
        let mut buf = Vec::with_capacity((events.len() + 1) * std::mem::size_of::<input_event>());
        for (t, c, v) in events.iter().copied().chain(std::iter::once((sys::EV_SYN, sys::SYN_REPORT, 0))) {
            let ev = input_event { sec: 0, usec: 0, type_: t, code: c, value: v };
            let bytes: [u8; std::mem::size_of::<input_event>()] = unsafe { std::mem::transmute(ev) };
            buf.extend_from_slice(&bytes);
        }
        (&self.file).write_all(&buf)
    }

    pub fn write_raw(&self, ev: &input_event) -> io::Result<()> {
        let bytes: [u8; std::mem::size_of::<input_event>()] = unsafe { std::mem::transmute(*ev) };
        (&self.file).write_all(&bytes)
    }

    /// Press or release what an action stands for.
    pub fn perform(&self, action: &Action, pressed: bool) -> io::Result<()> {
        let v = pressed as i32;
        match action {
            Action::Button(code) => self.emit(&[(sys::EV_KEY, *code, v)]),
            Action::Keys(codes) => {
                // modifiers first on press, last on release
                let evs: Vec<(u16, u16, i32)> = if pressed {
                    codes.iter().map(|c| (sys::EV_KEY, *c, 1)).collect()
                } else {
                    codes.iter().rev().map(|c| (sys::EV_KEY, *c, 0)).collect()
                };
                self.emit(&evs)
            }
            _ => Ok(()),
        }
    }
}

impl Drop for VirtualInput {
    fn drop(&mut self) {
        let _ = sys::ioctl_int(self.file.as_raw_fd(), sys::UI_DEV_DESTROY, 0);
    }
}

fn test_bit(bits: &[u8], n: usize) -> bool {
    bits.get(n / 8).map_or(false, |b| b & (1 << (n % 8)) != 0)
}

fn caps(file: &File, ev: u8, max: usize) -> Vec<u16> {
    let mut bits = vec![0u8; max / 8 + 1];
    if sys::ioctl_buf(file.as_raw_fd(), sys::eviocgbit(ev, bits.len()), &mut bits).is_err() {
        return vec![];
    }
    (0..max).filter(|i| test_bit(&bits, *i)).map(|i| i as u16).collect()
}

fn device_name(file: &File) -> String {
    let mut buf = [0u8; 256];
    match sys::ioctl_buf(file.as_raw_fd(), sys::eviocgname(buf.len()), &mut buf) {
        Ok(_) => String::from_utf8_lossy(&buf[..buf.iter().position(|b| *b == 0).unwrap_or(0)]).into_owned(),
        Err(_) => "mouse".into(),
    }
}

/// An exclusive grab of one evdev node, translating remapped buttons.
pub struct Grab {
    stop: Arc<AtomicBool>,
    pub path: PathBuf,
}

impl Drop for Grab {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Grab {
    /// Grab `path` if it produces any of `codes`. `on_button(code, pressed)` is
    /// called for swallowed buttons; all other events pass through.
    pub fn start<F>(path: &Path, codes: Vec<u16>, on_button: F) -> io::Result<Option<Grab>>
    where
        F: Fn(u16, bool) + Send + 'static,
    {
        let file = OpenOptions::new().read(true).custom_flags(O_NONBLOCK | O_CLOEXEC).open(path)?;
        let keys = caps(&file, sys::EV_KEY as u8, 0x300);
        if !codes.iter().any(|c| keys.contains(c)) {
            return Ok(None);
        }
        let rels = caps(&file, sys::EV_REL as u8, 0x10);
        let name = format!("{} (Peridot)", device_name(&file));
        let mirror = VirtualInput::create(&name, &keys, &rels, (0x1209, 0x5046))?;
        sys::ioctl_int(file.as_raw_fd(), sys::EVIOCGRAB, 1)?;
        crate::info!("remapping buttons on {} ({})", path.display(), name);
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let p = path.to_path_buf();
        std::thread::Builder::new().name("grab".into()).spawn(move || {
            let mut file = file;
            let size = std::mem::size_of::<input_event>();
            let mut buf = vec![0u8; size * 64];
            let mut swallowed_in_frame = false;
            while !flag.load(Ordering::Relaxed) {
                match sys::poll_readable(file.as_raw_fd(), 250) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(_) => break,
                }
                let n = match file.read(&mut buf) {
                    Ok(n) => n,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                    Err(_) => break,
                };
                for chunk in buf[..n].chunks_exact(size) {
                    let mut raw = [0u8; std::mem::size_of::<input_event>()];
                    raw.copy_from_slice(chunk);
                    let ev: input_event = unsafe { std::mem::transmute(raw) };
                    if ev.type_ == sys::EV_KEY && codes.contains(&ev.code) {
                        if ev.value != 2 {
                            on_button(ev.code, ev.value != 0);
                        }
                        swallowed_in_frame = true;
                        continue;
                    }
                    if ev.type_ == sys::EV_MSC && swallowed_in_frame {
                        continue; // scan code of a swallowed button
                    }
                    if ev.type_ == sys::EV_SYN {
                        swallowed_in_frame = false;
                    }
                    if mirror.write_raw(&ev).is_err() {
                        break;
                    }
                }
            }
            let _ = sys::ioctl_int(file.as_raw_fd(), sys::EVIOCGRAB, 0);
            crate::info!("stopped remapping on {}", p.display());
        })?;
        Ok(Some(Grab { stop, path: path.to_path_buf() }))
    }
}

/// Executes actions and keeps software grabs in sync with button mappings.
pub struct Engine {
    vinput: Option<VirtualInput>,
    vinput_error: Option<String>,
    grabs: HashMap<String, (Vec<(u16, String)>, Vec<Grab>)>,
}

impl Default for Engine {
    fn default() -> Self {
        Engine::new()
    }
}

impl Engine {
    pub fn new() -> Engine {
        Engine { vinput: None, vinput_error: None, grabs: HashMap::new() }
    }

    /// Lazily create the virtual device. Returns an error description when
    /// /dev/uinput isn't accessible.
    pub fn ensure_vinput(&mut self) -> Result<(), String> {
        if self.vinput.is_some() {
            return Ok(());
        }
        match VirtualInput::for_actions() {
            Ok(v) => {
                self.vinput = Some(v);
                self.vinput_error = None;
                Ok(())
            }
            Err(e) => {
                let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
                let modules_gone = !kernel.trim().is_empty()
                    && !Path::new("/lib/modules").join(kernel.trim()).exists();
                let msg = match (e.kind(), e.raw_os_error()) {
                    // ENODEV/ENOENT: the uinput driver isn't loaded in the running kernel.
                    _ if modules_gone => "Shortcuts and media keys need a reboot: your kernel was updated and \
                         the running one can't load the uinput module anymore."
                        .to_string(),
                    (io::ErrorKind::NotFound, _) | (_, Some(19)) => {
                        "Shortcuts and media keys need the uinput kernel module. Run: sudo modprobe uinput".to_string()
                    }
                    (io::ErrorKind::PermissionDenied, _) => {
                        "Shortcuts and media keys need access to /dev/uinput: install the Peridot udev rules and log in again"
                            .to_string()
                    }
                    _ => format!("Cannot create the virtual input device for shortcuts and media keys: {}", e),
                };
                if self.vinput_error.as_deref() != Some(msg.as_str()) {
                    crate::warn!("{}", msg);
                }
                self.vinput_error = Some(msg.clone());
                Err(msg)
            }
        }
    }

    pub fn last_error(&self) -> Option<&str> {
        self.vinput_error.as_deref()
    }

    pub fn perform(&mut self, action: &Action, pressed: bool) {
        if !matches!(action, Action::Button(_) | Action::Keys(_)) {
            return;
        }
        if self.ensure_vinput().is_err() {
            return;
        }
        if let Some(v) = &self.vinput {
            if let Err(e) = v.perform(action, pressed) {
                crate::warn!("emitting action failed: {}", e);
            }
        }
    }

    /// Make the evdev grabs for `device` match `mapped` (evdev code, action).
    /// `nodes` lists the device's event nodes; `on_button` reports presses.
    pub fn sync_grabs<F>(&mut self, device: &str, mapped: Vec<(u16, String)>, nodes: &[PathBuf], on_button: F)
    where
        F: Fn(u16, bool) + Send + Clone + 'static,
    {
        let current = self.grabs.get(device).map(|(m, _)| m.clone());
        let codes: Vec<u16> = mapped.iter().map(|(c, _)| *c).collect();
        let same_codes = current.as_ref().map(|m| m.iter().map(|(c, _)| *c).collect::<Vec<_>>()) == Some(codes.clone());
        if same_codes {
            if let Some(e) = self.grabs.get_mut(device) {
                e.0 = mapped;
            }
            return;
        }
        self.grabs.remove(device);
        if codes.is_empty() {
            return;
        }
        let mut grabs = Vec::new();
        for n in nodes {
            match Grab::start(n, codes.clone(), on_button.clone()) {
                Ok(Some(g)) => grabs.push(g),
                Ok(None) => {}
                Err(e) => crate::warn!("cannot grab {} for remapping: {}", n.display(), e),
            }
        }
        self.grabs.insert(device.to_string(), (mapped, grabs));
    }

    pub fn drop_device(&mut self, device: &str) {
        self.grabs.remove(device);
    }
}
