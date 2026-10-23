//! Peridot: a peripheral manager for Logitech (HID++) and Razer mice and
//! headsets on Linux.
//!
//! The crate is dependency-free on purpose. Layers, bottom up:
//! - [`sys`], [`hid`]: hidraw, ioctl, netlink hotplug
//! - [`hidpp`], [`centurion`], [`razer`]: vendor protocols and drivers
//! - [`model`]: the vendor-neutral device model the UI consumes
//! - [`manager`], [`service`], [`dbus`]: the daemon and its D-Bus API
//! - [`remap`]: software button actions (uinput, evdev grabs)
//! - [`softeq`]: PipeWire equalizer for headsets that can't store one

pub mod log;

pub mod actions;
pub mod backend;
pub mod battery;
pub mod centurion;
pub mod config;
pub mod dbus;
pub mod hid;
pub mod hidpp;
pub mod json;
pub mod manager;
pub mod model;
pub mod razer;
pub mod remap;
pub mod service;
pub mod softeq;
/// Byte-level device simulators used by the test-suite.
#[doc(hidden)]
pub mod sim;
pub mod sys;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const BUS_NAME: &str = "io.github.peridot.Peridot1";
pub const OBJECT_PATH: &str = "/io/github/peridot/Peridot1";
pub const INTERFACE: &str = "io.github.peridot.Manager1";
