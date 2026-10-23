//! hidraw device access: enumeration through sysfs, report descriptor scanning,
//! raw reads/writes and feature reports.

use crate::sys;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

pub const BUS_USB: u16 = 0x03;
pub const BUS_BLUETOOTH: u16 = 0x05;

/// Everything we know about one hidraw node before opening it.
#[derive(Clone, Debug, Default)]
pub struct HidInfo {
    /// `/dev/hidrawN`
    pub path: PathBuf,
    /// `hidrawN`
    pub sys_name: String,
    pub bus: u16,
    pub vid: u16,
    pub pid: u16,
    pub name: String,
    pub phys: String,
    pub uniq: String,
    /// USB interface number, when on USB.
    pub interface: Option<u8>,
    /// sysfs path of the physical USB device (shared by all its interfaces).
    pub usb_device: Option<PathBuf>,
    pub descriptor: Vec<u8>,
}

impl HidInfo {
    pub fn report_info(&self) -> DescriptorInfo {
        DescriptorInfo::parse(&self.descriptor)
    }
}

/// Facts extracted from a HID report descriptor.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DescriptorInfo {
    pub report_ids: Vec<u8>,
    pub usage_pages: Vec<u16>,
    /// (usage page, usage) of each top-level application collection.
    pub applications: Vec<(u16, u16)>,
}

impl DescriptorInfo {
    pub fn parse(d: &[u8]) -> DescriptorInfo {
        let mut info = DescriptorInfo::default();
        let mut usage_page: u16 = 0;
        let mut usages: Vec<u32> = Vec::new();
        let mut depth = 0usize;
        let mut i = 0usize;
        while i < d.len() {
            let prefix = d[i];
            if prefix == 0xFE {
                // long item: bDataSize, bLongItemTag, data
                if i + 1 >= d.len() {
                    break;
                }
                i += 3 + d[i + 1] as usize;
                continue;
            }
            let size = match prefix & 0x03 {
                3 => 4,
                n => n as usize,
            };
            if i + 1 + size > d.len() {
                break;
            }
            let mut value: u32 = 0;
            for (k, b) in d[i + 1..i + 1 + size].iter().enumerate() {
                value |= (*b as u32) << (8 * k);
            }
            let ty = (prefix >> 2) & 0x03;
            let tag = prefix >> 4;
            match (ty, tag) {
                (1, 0x0) => {
                    usage_page = value as u16;
                    if !info.usage_pages.contains(&usage_page) {
                        info.usage_pages.push(usage_page);
                    }
                }
                (1, 0x8) => {
                    let id = value as u8;
                    if !info.report_ids.contains(&id) {
                        info.report_ids.push(id);
                    }
                }
                (2, 0x0) => usages.push(value),
                (0, 0xA) => {
                    // collection
                    if depth == 0 && value == 0x01 {
                        let u = usages.first().copied().unwrap_or(0);
                        let (page, usage) = if size == 4 { ((u >> 16) as u16, u as u16) } else { (usage_page, u as u16) };
                        info.applications.push((page, usage));
                    }
                    depth += 1;
                    usages.clear();
                }
                (0, 0xC) => {
                    depth = depth.saturating_sub(1);
                    usages.clear();
                }
                (0, _) => usages.clear(), // input/output/feature end the local state
                _ => {}
            }
            i += 1 + size;
        }
        info
    }

    pub fn has_report_id(&self, id: u8) -> bool {
        self.report_ids.contains(&id)
    }

    /// True if this interface carries HID++ short or long reports.
    pub fn is_hidpp(&self) -> bool {
        self.has_report_id(0x10) || self.has_report_id(0x11)
    }

    pub fn has_application(&self, page: u16, usage: u16) -> bool {
        self.applications.contains(&(page, usage))
    }
}

fn read_trim(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

/// Read the HID facts of one `/sys/class/hidraw/<name>` entry.
pub fn info_for(sys_name: &str) -> Option<HidInfo> {
    let class_dir = Path::new("/sys/class/hidraw").join(sys_name);
    let dev_dir = class_dir.join("device");
    let uevent = fs::read_to_string(dev_dir.join("uevent")).ok()?;
    let mut info = HidInfo {
        path: Path::new("/dev").join(sys_name),
        sys_name: sys_name.to_string(),
        ..Default::default()
    };
    for line in uevent.lines() {
        if let Some(v) = line.strip_prefix("HID_ID=") {
            let parts: Vec<&str> = v.split(':').collect();
            if parts.len() == 3 {
                info.bus = u16::from_str_radix(parts[0], 16).unwrap_or(0);
                info.vid = u32::from_str_radix(parts[1], 16).unwrap_or(0) as u16;
                info.pid = u32::from_str_radix(parts[2], 16).unwrap_or(0) as u16;
            }
        } else if let Some(v) = line.strip_prefix("HID_NAME=") {
            info.name = v.to_string();
        } else if let Some(v) = line.strip_prefix("HID_PHYS=") {
            info.phys = v.to_string();
        } else if let Some(v) = line.strip_prefix("HID_UNIQ=") {
            info.uniq = v.to_string();
        }
    }
    info.descriptor = fs::read(dev_dir.join("report_descriptor")).unwrap_or_default();

    if let Ok(real) = fs::canonicalize(&dev_dir) {
        // .../usb1/1-2/1-2:1.0/0003:046D:C539.0001
        if let Some(intf_dir) = real.parent() {
            if let Some(n) = read_trim(&intf_dir.join("bInterfaceNumber")) {
                info.interface = u8::from_str_radix(&n, 16).ok();
                info.usb_device = intf_dir.parent().map(Path::to_path_buf);
            }
        }
    }
    if info.interface.is_none() {
        if let Some(pos) = info.phys.rfind("/input") {
            info.interface = info.phys[pos + 6..].parse().ok();
        }
    }
    Some(info)
}

/// List every hidraw node currently present.
pub fn enumerate() -> Vec<HidInfo> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir("/sys/class/hidraw") {
        for e in entries.flatten() {
            if let Some(name) = e.file_name().to_str() {
                if let Some(info) = info_for(name) {
                    out.push(info);
                }
            }
        }
    }
    out.sort_by(|a, b| a.sys_name.cmp(&b.sys_name));
    out
}

/// Input event nodes (`/dev/input/eventN`) belonging to one HID interface.
pub fn event_nodes_for(sys_name: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let input_dir = Path::new("/sys/class/hidraw").join(sys_name).join("device/input");
    if let Ok(inputs) = fs::read_dir(&input_dir) {
        for input in inputs.flatten() {
            if let Ok(children) = fs::read_dir(input.path()) {
                for c in children.flatten() {
                    let name = c.file_name().to_string_lossy().to_string();
                    if name.starts_with("event") {
                        out.push(Path::new("/dev/input").join(name));
                    }
                }
            }
        }
    }
    out.sort();
    out
}

/// Byte-level access to a HID device. Implemented by real hidraw nodes and by
/// the simulated devices used by the tests.
pub trait Transport: Send + Sync {
    /// Send an output report (first byte is the report ID).
    fn write(&self, data: &[u8]) -> io::Result<()>;
    /// Read one input report. Returns Ok(0) on timeout.
    fn read_timeout(&self, buf: &mut [u8], timeout_ms: i32) -> io::Result<usize>;
    /// SET_REPORT(Feature). First byte is the report ID.
    fn set_feature(&self, data: &[u8]) -> io::Result<()>;
    /// GET_REPORT(Feature). `buf[0]` holds the report ID on entry.
    fn get_feature(&self, buf: &mut [u8]) -> io::Result<usize>;
    /// GET_REPORT(Input) over the control pipe. `buf[0]` holds the report ID.
    fn get_input(&self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
    /// Human-readable location, for logs.
    fn describe(&self) -> String;
}

pub struct HidRaw {
    file: File,
    path: PathBuf,
}

impl HidRaw {
    pub fn open(path: &Path) -> io::Result<HidRaw> {
        const O_NONBLOCK: i32 = 0o4000;
        const O_CLOEXEC: i32 = 0o2000000;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(O_NONBLOCK | O_CLOEXEC)
            .open(path)?;
        Ok(HidRaw { file, path: path.to_path_buf() })
    }
}

impl Transport for HidRaw {
    fn write(&self, data: &[u8]) -> io::Result<()> {
        crate::trace!("hid {} <- {}", self.path.display(), crate::log::hex(data));
        let mut f = &self.file;
        loop {
            match f.write(data) {
                Ok(_) => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    fn read_timeout(&self, buf: &mut [u8], timeout_ms: i32) -> io::Result<usize> {
        if !sys::poll_readable(self.file.as_raw_fd(), timeout_ms)? {
            return Ok(0);
        }
        let mut f = &self.file;
        match f.read(buf) {
            Ok(n) => {
                crate::trace!("hid {} -> {}", self.path.display(), crate::log::hex(&buf[..n]));
                Ok(n)
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(0),
            Err(e) => Err(e),
        }
    }

    fn set_feature(&self, data: &[u8]) -> io::Result<()> {
        let mut buf = data.to_vec();
        sys::ioctl_buf(self.file.as_raw_fd(), sys::hidiocsfeature(buf.len()), &mut buf)?;
        Ok(())
    }

    fn get_feature(&self, buf: &mut [u8]) -> io::Result<usize> {
        sys::ioctl_buf(self.file.as_raw_fd(), sys::hidiocgfeature(buf.len()), buf)
    }

    fn get_input(&self, buf: &mut [u8]) -> io::Result<usize> {
        sys::ioctl_buf(self.file.as_raw_fd(), sys::hidiocginput(buf.len()), buf)
    }

    fn describe(&self) -> String {
        self.path.display().to_string()
    }
}

/// Hotplug notifications for hidraw nodes.
#[derive(Clone, Debug, PartialEq)]
pub enum Hotplug {
    Added(String),
    Removed(String),
}

/// Parse one netlink uevent datagram (kernel or libudev format) into a hidraw
/// hotplug event.
pub fn parse_uevent(msg: &[u8]) -> Option<Hotplug> {
    let props: &[u8] = if msg.starts_with(b"libudev\0") {
        // struct udev_monitor_netlink_header: prefix[8], magic(be32), header_size,
        // properties_off, properties_len (host endian) ...
        if msg.len() < 24 {
            return None;
        }
        let off = u32::from_ne_bytes([msg[16], msg[17], msg[18], msg[19]]) as usize;
        let len = u32::from_ne_bytes([msg[20], msg[21], msg[22], msg[23]]) as usize;
        msg.get(off..off.checked_add(len)?)?
    } else {
        // "action@devpath\0KEY=VALUE\0..."
        let first_nul = msg.iter().position(|b| *b == 0)?;
        &msg[first_nul + 1..]
    };
    let mut action = None;
    let mut subsystem = None;
    let mut devname = None;
    for kv in props.split(|b| *b == 0) {
        let s = String::from_utf8_lossy(kv);
        if let Some(v) = s.strip_prefix("ACTION=") {
            action = Some(v.to_string());
        } else if let Some(v) = s.strip_prefix("SUBSYSTEM=") {
            subsystem = Some(v.to_string());
        } else if let Some(v) = s.strip_prefix("DEVNAME=") {
            devname = Some(v.trim_start_matches("/dev/").to_string());
        }
    }
    if subsystem.as_deref() != Some("hidraw") {
        return None;
    }
    let name = devname?;
    match action.as_deref() {
        Some("add") => Some(Hotplug::Added(name)),
        Some("remove") => Some(Hotplug::Removed(name)),
        _ => None,
    }
}

/// Spawn a thread that forwards hidraw hotplug events. Prefers udev's
/// re-broadcast (permissions are applied by then); falls back to kernel events.
pub fn spawn_hotplug_monitor<F: Fn(Hotplug) + Send + 'static>(callback: F) -> io::Result<()> {
    let (sock, from_udev) = match sys::uevent_socket(2) {
        Ok(s) => (s, true),
        Err(_) => (sys::uevent_socket(1)?, false),
    };
    crate::debug!("hotplug monitor listening ({})", if from_udev { "udev" } else { "kernel" });
    std::thread::Builder::new().name("hotplug".into()).spawn(move || {
        let mut buf = vec![0u8; 16384];
        loop {
            match sys::recv_bytes(&sock, &mut buf) {
                Ok(n) => {
                    if let Some(ev) = parse_uevent(&buf[..n]) {
                        if !from_udev {
                            // kernel events race udev's permission rules
                            std::thread::sleep(std::time::Duration::from_millis(400));
                        }
                        callback(ev);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    crate::warn!("hotplug monitor stopped: {}", e);
                    return;
                }
            }
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Report descriptor of a Logitech Lightspeed receiver's HID++ interface.
    const HIDPP_DESC: &[u8] = &[
        0x06, 0x00, 0xFF, 0x09, 0x01, 0xA1, 0x01, 0x85, 0x10, 0x75, 0x08, 0x95, 0x06, 0x15, 0x00, 0x26, 0xFF, 0x00,
        0x09, 0x01, 0x81, 0x00, 0x09, 0x01, 0x91, 0x00, 0xC0, 0x06, 0x00, 0xFF, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x11,
        0x75, 0x08, 0x95, 0x13, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x09, 0x02, 0x81, 0x00, 0x09, 0x02, 0x91, 0x00, 0xC0,
    ];

    #[test]
    fn descriptor_scan() {
        let info = DescriptorInfo::parse(HIDPP_DESC);
        assert!(info.is_hidpp());
        assert_eq!(info.report_ids, vec![0x10, 0x11]);
        assert_eq!(info.applications, vec![(0xFF00, 1), (0xFF00, 2)]);
        let mouse = [0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x09, 0x01, 0xA1, 0x00, 0xC0, 0xC0];
        let m = DescriptorInfo::parse(&mouse);
        assert!(!m.is_hidpp());
        assert!(m.has_application(0x01, 0x02));
    }

    #[test]
    fn uevent_parsing() {
        let kernel = b"add@/devices/x/hidraw/hidraw4\0ACTION=add\0DEVPATH=/devices/x\0SUBSYSTEM=hidraw\0MAJOR=240\0DEVNAME=hidraw4\0";
        assert_eq!(parse_uevent(kernel), Some(Hotplug::Added("hidraw4".into())));
        let mut udev = Vec::new();
        udev.extend_from_slice(b"libudev\0");
        udev.extend_from_slice(&0xfeedcafeu32.to_be_bytes());
        udev.extend_from_slice(&40u32.to_ne_bytes()); // header size
        udev.extend_from_slice(&40u32.to_ne_bytes()); // properties offset
        let props = b"ACTION=remove\0SUBSYSTEM=hidraw\0DEVNAME=/dev/hidraw7\0";
        udev.extend_from_slice(&(props.len() as u32).to_ne_bytes());
        udev.resize(40, 0);
        udev.extend_from_slice(props);
        assert_eq!(parse_uevent(&udev), Some(Hotplug::Removed("hidraw7".into())));
        let other = b"add@/devices/y\0ACTION=add\0SUBSYSTEM=usb\0DEVNAME=bus/usb/001/002\0";
        assert_eq!(parse_uevent(other), None);
    }
}
