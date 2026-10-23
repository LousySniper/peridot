//! Glue between device drivers and the manager.
//!
//! A backend owns one physical hidraw node (or a simulated one) and the
//! logical devices behind it. It reports what happens through a [`Sink`].

use crate::json::Json;
use crate::model::Device;
use std::sync::{Arc, Mutex};

pub type DeviceHandle = Arc<Mutex<dyn Device>>;

pub enum BackendEvent {
    /// A device was probed and is ready (first time, or after reconnecting).
    /// The manager re-applies saved settings on this event.
    Ready(DeviceHandle),
    /// Snapshot changed (battery report, setting applied, went offline...).
    Changed(String, Json),
    /// Device disappeared for good (receiver unplugged, unpaired...).
    Removed(String),
    /// A diverted/grabbed button was pressed or released.
    Button { device: String, control: String, pressed: bool },
    /// The backend's node went away; `key` identifies the backend.
    Closed(String),
}

pub type Sink = Arc<dyn Fn(BackendEvent) + Send + Sync>;

/// Lock a device even if a previous holder panicked.
pub fn lock(handle: &DeviceHandle) -> std::sync::MutexGuard<'_, dyn Device + 'static> {
    handle.lock().unwrap_or_else(|p| p.into_inner())
}

pub fn snapshot(handle: &DeviceHandle) -> (String, Json) {
    let d = lock(handle);
    let s = d.state();
    (s.id.clone(), s.to_json())
}

/// Something the manager can stop when the node disappears.
pub trait Backend: Send {
    fn stop(&self);
}
