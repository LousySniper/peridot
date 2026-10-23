//! Thin FFI layer over the Linux syscalls the standard library doesn't expose:
//! ioctl (hidraw feature reports, evdev grab, uinput), poll, and netlink sockets.

#![allow(non_camel_case_types)]

use std::io;
use std::os::raw::{c_int, c_short, c_ulong, c_void};
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};

#[repr(C)]
pub struct pollfd {
    pub fd: c_int,
    pub events: c_short,
    pub revents: c_short,
}

pub const POLLIN: c_short = 0x001;
pub const POLLERR: c_short = 0x008;
pub const POLLHUP: c_short = 0x010;
pub const POLLNVAL: c_short = 0x020;

extern "C" {
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    #[link_name = "poll"]
    fn c_poll(fds: *mut pollfd, nfds: c_ulong, timeout: c_int) -> c_int;
    fn socket(domain: c_int, ty: c_int, protocol: c_int) -> c_int;
    fn bind(fd: c_int, addr: *const c_void, len: u32) -> c_int;
    fn recv(fd: c_int, buf: *mut c_void, len: usize, flags: c_int) -> isize;
    fn setsockopt(fd: c_int, level: c_int, name: c_int, val: *const c_void, len: u32) -> c_int;
    fn getuid() -> u32;
    fn prctl(option: c_int, ...) -> c_int;
    fn signal(sig: c_int, handler: usize) -> usize;
}

// ---- ioctl number encoding (asm-generic) ----
const IOC_NONE: c_ulong = 0;
const IOC_WRITE: c_ulong = 1;
const IOC_READ: c_ulong = 2;

const fn ioc(dir: c_ulong, ty: u8, nr: u8, size: usize) -> c_ulong {
    (dir << 30) | ((size as c_ulong) << 16) | ((ty as c_ulong) << 8) | nr as c_ulong
}

pub const fn hidiocsfeature(len: usize) -> c_ulong {
    ioc(IOC_WRITE | IOC_READ, b'H', 0x06, len)
}
pub const fn hidiocgfeature(len: usize) -> c_ulong {
    ioc(IOC_WRITE | IOC_READ, b'H', 0x07, len)
}
pub const fn hidiocginput(len: usize) -> c_ulong {
    ioc(IOC_WRITE | IOC_READ, b'H', 0x0A, len)
}
pub const EVIOCGRAB: c_ulong = ioc(IOC_WRITE, b'E', 0x90, 4);
pub const fn eviocgbit(ev: u8, len: usize) -> c_ulong {
    ioc(IOC_READ, b'E', 0x20 + ev, len)
}
pub const fn eviocgname(len: usize) -> c_ulong {
    ioc(IOC_READ, b'E', 0x06, len)
}
pub const UI_SET_EVBIT: c_ulong = ioc(IOC_WRITE, b'U', 100, 4);
pub const UI_SET_KEYBIT: c_ulong = ioc(IOC_WRITE, b'U', 101, 4);
pub const UI_SET_RELBIT: c_ulong = ioc(IOC_WRITE, b'U', 102, 4);
pub const UI_DEV_CREATE: c_ulong = ioc(IOC_NONE, b'U', 1, 0);
pub const UI_DEV_DESTROY: c_ulong = ioc(IOC_NONE, b'U', 2, 0);
pub const UI_DEV_SETUP: c_ulong = ioc(IOC_WRITE, b'U', 3, std::mem::size_of::<uinput_setup>());
pub const UI_SET_ABSBIT: c_ulong = ioc(IOC_WRITE, b'U', 103, 4);
pub const UI_SET_MSCBIT: c_ulong = ioc(IOC_WRITE, b'U', 104, 4);
pub const UI_SET_FFBIT: c_ulong = ioc(IOC_WRITE, b'U', 107, 4);
pub const UI_SET_PROPBIT: c_ulong = ioc(IOC_WRITE, b'U', 110, 4);
pub const UI_ABS_SETUP: c_ulong = ioc(IOC_WRITE, b'U', 4, std::mem::size_of::<uinput_abs_setup>());
/// Force feedback is passed through as raw `struct ff_effect` (48 bytes on 64-bit).
pub const FF_EFFECT_SIZE: usize = 48;
/// `struct uinput_ff_upload`: request_id, retval, effect, old.
pub const FF_UPLOAD_SIZE: usize = 8 + 2 * FF_EFFECT_SIZE;
/// `struct uinput_ff_erase`: request_id, retval, effect_id.
pub const FF_ERASE_SIZE: usize = 12;
pub const UI_BEGIN_FF_UPLOAD: c_ulong = ioc(IOC_READ | IOC_WRITE, b'U', 200, FF_UPLOAD_SIZE);
pub const UI_END_FF_UPLOAD: c_ulong = ioc(IOC_WRITE, b'U', 201, FF_UPLOAD_SIZE);
pub const UI_BEGIN_FF_ERASE: c_ulong = ioc(IOC_READ | IOC_WRITE, b'U', 202, FF_ERASE_SIZE);
pub const UI_END_FF_ERASE: c_ulong = ioc(IOC_WRITE, b'U', 203, FF_ERASE_SIZE);
pub const EVIOCSFF: c_ulong = ioc(IOC_WRITE, b'E', 0x80, FF_EFFECT_SIZE);
pub const EVIOCRMFF: c_ulong = ioc(IOC_WRITE, b'E', 0x81, 4);
pub const fn eviocgabs(code: u16) -> c_ulong {
    ioc(IOC_READ, b'E', 0x40 + code as u8, std::mem::size_of::<input_absinfo>())
}
pub const fn eviocgprop(len: usize) -> c_ulong {
    ioc(IOC_READ, b'E', 0x09, len)
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct input_absinfo {
    pub value: i32,
    pub minimum: i32,
    pub maximum: i32,
    pub fuzz: i32,
    pub flat: i32,
    pub resolution: i32,
}

#[repr(C)]
pub struct uinput_abs_setup {
    pub code: u16,
    pub absinfo: input_absinfo,
}

#[repr(C)]
pub struct input_id {
    pub bustype: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
}

#[repr(C)]
pub struct uinput_setup {
    pub id: input_id,
    pub name: [u8; 80],
    pub ff_effects_max: u32,
}

/// `struct input_event` with a native `struct timeval`.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct input_event {
    pub sec: isize,
    pub usec: isize,
    pub type_: u16,
    pub code: u16,
    pub value: i32,
}

fn cvt(ret: c_int) -> io::Result<c_int> {
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

/// ioctl with a mutable buffer argument.
pub fn ioctl_buf(fd: RawFd, req: c_ulong, buf: &mut [u8]) -> io::Result<usize> {
    let r = unsafe { ioctl(fd, req, buf.as_mut_ptr()) };
    cvt(r).map(|n| n as usize)
}

/// ioctl with an integer argument passed by value.
pub fn ioctl_int(fd: RawFd, req: c_ulong, arg: c_int) -> io::Result<()> {
    let r = unsafe { ioctl(fd, req, arg as c_ulong) };
    cvt(r).map(|_| ())
}

/// ioctl with a pointer to a struct.
pub fn ioctl_ptr<T>(fd: RawFd, req: c_ulong, arg: *const T) -> io::Result<()> {
    let r = unsafe { ioctl(fd, req, arg) };
    cvt(r).map(|_| ())
}

/// Wait for readability on one fd. Returns Ok(true) if readable, Ok(false) on
/// timeout and Err on hangup/error. `timeout_ms < 0` waits forever.
pub fn poll_readable(fd: RawFd, timeout_ms: i32) -> io::Result<bool> {
    let mut p = pollfd { fd, events: POLLIN, revents: 0 };
    loop {
        let r = unsafe { c_poll(&mut p, 1, timeout_ms) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        if r == 0 {
            return Ok(false);
        }
        if p.revents & POLLIN != 0 {
            return Ok(true);
        }
        if p.revents & (POLLERR | POLLHUP | POLLNVAL) != 0 {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "device hung up"));
        }
        return Ok(false);
    }
}

/// Wait until any of `fds` is readable. Returns which ones are; an error or
/// hangup on one of them is an error.
pub fn poll_any(fds: &[RawFd], timeout_ms: i32) -> io::Result<Vec<bool>> {
    let mut ps: Vec<pollfd> = fds.iter().map(|fd| pollfd { fd: *fd, events: POLLIN, revents: 0 }).collect();
    loop {
        let r = unsafe { c_poll(ps.as_mut_ptr(), ps.len() as c_ulong, timeout_ms) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        if ps.iter().any(|p| p.revents & (POLLERR | POLLHUP | POLLNVAL) != 0) {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "device hung up"));
        }
        return Ok(ps.iter().map(|p| p.revents & POLLIN != 0).collect());
    }
}

pub fn uid() -> u32 {
    unsafe { getuid() }
}

static TERMINATING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn on_terminate(_sig: c_int) {
    TERMINATING.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Turn SIGTERM and SIGINT into a flag the main loop polls (see [`terminating`]),
/// so the daemon can clean up before it exits.
pub fn catch_termination() {
    const SIGINT: c_int = 2;
    const SIGTERM: c_int = 15;
    unsafe {
        signal(SIGTERM, on_terminate as extern "C" fn(c_int) as usize);
        signal(SIGINT, on_terminate as extern "C" fn(c_int) as usize);
    }
}

pub fn terminating() -> bool {
    TERMINATING.load(std::sync::atomic::Ordering::SeqCst)
}

/// Ask the kernel to send SIGTERM to the calling process when the thread that
/// forked it exits. Meant for `Command::pre_exec`.
pub fn term_with_parent_thread() -> io::Result<()> {
    const PR_SET_PDEATHSIG: c_int = 1;
    const SIGTERM: c_int = 15;
    cvt(unsafe { prctl(PR_SET_PDEATHSIG, SIGTERM) }).map(|_| ())
}

// ---- netlink uevent socket ----
const AF_NETLINK: c_int = 16;
const SOCK_DGRAM: c_int = 2;
const SOCK_CLOEXEC: c_int = 0o2000000;
const NETLINK_KOBJECT_UEVENT: c_int = 15;
const SOL_SOCKET: c_int = 1;
const SO_RCVBUF: c_int = 8;

#[repr(C)]
struct sockaddr_nl {
    nl_family: u16,
    nl_pad: u16,
    nl_pid: u32,
    nl_groups: u32,
}

/// Open a NETLINK_KOBJECT_UEVENT socket subscribed to `groups`
/// (1 = raw kernel events, 2 = events re-broadcast by udev after rules ran).
pub fn uevent_socket(groups: u32) -> io::Result<OwnedFd> {
    let fd = cvt(unsafe { socket(AF_NETLINK, SOCK_DGRAM | SOCK_CLOEXEC, NETLINK_KOBJECT_UEVENT) })?;
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    let size: c_int = 1 << 20;
    unsafe {
        setsockopt(fd, SOL_SOCKET, SO_RCVBUF, &size as *const c_int as *const c_void, 4);
    }
    let addr = sockaddr_nl { nl_family: AF_NETLINK as u16, nl_pad: 0, nl_pid: 0, nl_groups: groups };
    cvt(unsafe { bind(fd, &addr as *const sockaddr_nl as *const c_void, std::mem::size_of::<sockaddr_nl>() as u32) })?;
    Ok(owned)
}

pub fn recv_bytes(fd: &OwnedFd, buf: &mut [u8]) -> io::Result<usize> {
    let r = unsafe { recv(fd.as_raw_fd(), buf.as_mut_ptr() as *mut c_void, buf.len(), 0) };
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r as usize)
    }
}

// ---- input event codes used across the crate ----
pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_REL: u16 = 0x02;
pub const EV_ABS: u16 = 0x03;
pub const EV_MSC: u16 = 0x04;
pub const EV_FF: u16 = 0x15;
pub const EV_UINPUT: u16 = 0x0101;
pub const UI_FF_UPLOAD: u16 = 1;
pub const UI_FF_ERASE: u16 = 2;
pub const ABS_X: u16 = 0x00;
pub const ABS_Y: u16 = 0x01;
pub const ABS_RX: u16 = 0x03;
pub const ABS_RY: u16 = 0x04;
pub const BTN_SOUTH: u16 = 0x130;
pub const BUS_BLUETOOTH: u16 = 0x05;
pub const SYN_REPORT: u16 = 0;
pub const REL_X: u16 = 0x00;
pub const REL_Y: u16 = 0x01;
pub const REL_HWHEEL: u16 = 0x06;
pub const REL_WHEEL: u16 = 0x08;
pub const REL_WHEEL_HI_RES: u16 = 0x0b;
pub const REL_HWHEEL_HI_RES: u16 = 0x0c;
pub const BUS_VIRTUAL: u16 = 0x06;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_numbers_match_kernel_headers() {
        // Values cross-checked against <linux/hidraw.h>, <linux/input.h>, <linux/uinput.h> on x86_64.
        assert_eq!(hidiocsfeature(91), 0xC05B4806);
        assert_eq!(hidiocgfeature(91), 0xC05B4807);
        assert_eq!(EVIOCGRAB, 0x40044590);
        assert_eq!(eviocgabs(0), 0x80184540);
        assert_eq!(EVIOCSFF, 0x40304580);
        assert_eq!(EVIOCRMFF, 0x40044581);
        assert_eq!(UI_SET_ABSBIT, 0x40045567);
        assert_eq!(UI_SET_FFBIT, 0x4004556b);
        assert_eq!(UI_ABS_SETUP, 0x401c5504);
        assert_eq!(UI_BEGIN_FF_UPLOAD, 0xc06855c8);
        assert_eq!(UI_END_FF_UPLOAD, 0x406855c9);
        assert_eq!(UI_BEGIN_FF_ERASE, 0xc00c55ca);
        assert_eq!(UI_END_FF_ERASE, 0x400c55cb);
        assert_eq!(UI_SET_EVBIT, 0x40045564);
        assert_eq!(UI_DEV_CREATE, 0x5501);
        assert_eq!(UI_DEV_SETUP, 0x405c5503);
        assert_eq!(std::mem::size_of::<input_event>(), 24);
    }
}
