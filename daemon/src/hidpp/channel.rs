//! Request/response transport for HID++ over one hidraw node.
//!
//! A reader thread pulls every input report off the device. Reports that answer
//! the outstanding request are handed to the waiting caller; everything else
//! (connection events, battery broadcasts, diverted buttons) goes to the
//! listener. Requests are serialized per channel: receivers and wireless links
//! behave much better with one request in flight.

use super::{Error, Message, Result, ERROR_10, ERROR_20, GET_LONG_REGISTER, GET_REGISTER, REPORT_LONG, REPORT_SHORT};
use super::{SET_LONG_REGISTER, SET_REGISTER};
use crate::hid::Transport;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone)]
pub enum ChannelEvent {
    Message(Message),
    Closed,
}

struct Pending {
    device: u8,
    sub: u8,
    address: u8,
    tx: SyncSender<Result<Message>>,
}

pub struct Channel {
    transport: Arc<dyn Transport>,
    pending: Mutex<Option<Pending>>,
    listener: Mutex<Option<Sender<ChannelEvent>>>,
    request_lock: Mutex<()>,
    closed: AtomicBool,
    swid: AtomicU8,
    short_ok: bool,
}

impl Channel {
    /// Open a channel and start its reader thread. `short_ok` is false for
    /// devices whose descriptor only declares long HID++ reports.
    pub fn open(transport: Arc<dyn Transport>, short_ok: bool) -> Arc<Channel> {
        let ch = Arc::new(Channel {
            transport: transport.clone(),
            pending: Mutex::new(None),
            listener: Mutex::new(None),
            request_lock: Mutex::new(()),
            closed: AtomicBool::new(false),
            swid: AtomicU8::new(1),
            short_ok,
        });
        let weak = Arc::downgrade(&ch);
        let name = format!("hidpp-{}", transport.describe().rsplit('/').next().unwrap_or("dev"));
        let _ = std::thread::Builder::new().name(name).spawn(move || reader(transport, weak));
        ch
    }

    pub fn set_listener(&self, tx: Sender<ChannelEvent>) {
        *self.listener.lock().unwrap() = Some(tx);
    }

    pub fn subscribe(&self) -> Receiver<ChannelEvent> {
        let (tx, rx) = mpsc::channel();
        self.set_listener(tx);
        rx
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
    }

    pub fn describe(&self) -> String {
        self.transport.describe()
    }

    fn next_swid(&self) -> u8 {
        // Software ids 0x1..=0xF; 0 is reserved for device-initiated events.
        let v = self.swid.fetch_add(1, Ordering::Relaxed);
        (v % 15) + 1
    }

    /// HID++ 2.0 feature call. Returns the reply parameters.
    pub fn feature_request(&self, device: u8, feature_index: u8, function: u8, params: &[u8]) -> Result<Vec<u8>> {
        self.feature_request_timeout(device, feature_index, function, params, DEFAULT_TIMEOUT)
    }

    pub fn feature_request_timeout(
        &self,
        device: u8,
        feature_index: u8,
        function: u8,
        params: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        let report = if params.len() <= 3 && self.short_ok { REPORT_SHORT } else { REPORT_LONG };
        let mut attempt = 0;
        loop {
            let address = (function << 4) | self.next_swid();
            let msg = Message { report, device, sub: feature_index, address, params: params.to_vec() };
            match self.transact(msg, timeout) {
                Err(Error::V20(0x08)) if attempt < 2 => {
                    attempt += 1;
                    std::thread::sleep(Duration::from_millis(60));
                }
                other => return other.map(|m| m.params),
            }
        }
    }

    /// HID++ 1.0 register read.
    pub fn read_register(&self, device: u8, register: u8, long: bool, params: &[u8]) -> Result<Vec<u8>> {
        let sub = if long { GET_LONG_REGISTER } else { GET_REGISTER };
        let msg = Message { report: REPORT_SHORT, device, sub, address: register, params: params.to_vec() };
        self.transact(msg, DEFAULT_TIMEOUT).map(|m| m.params)
    }

    /// HID++ 1.0 register write (short registers take 3 bytes, long ones 16).
    pub fn write_register(&self, device: u8, register: u8, params: &[u8]) -> Result<Vec<u8>> {
        let (sub, report) = if params.len() > 3 { (SET_LONG_REGISTER, REPORT_LONG) } else { (SET_REGISTER, REPORT_SHORT) };
        let msg = Message { report, device, sub, address: register, params: params.to_vec() };
        self.transact(msg, DEFAULT_TIMEOUT).map(|m| m.params)
    }

    /// Send without waiting for a reply.
    pub fn send(&self, msg: &Message) -> Result<()> {
        self.transport.write(&msg.encode()).map_err(|e| Error::Io(e.to_string()))
    }

    fn transact(&self, msg: Message, timeout: Duration) -> Result<Message> {
        if self.is_closed() {
            return Err(Error::Closed);
        }
        let _guard = self.request_lock.lock().unwrap_or_else(|p| p.into_inner());
        let (tx, rx) = mpsc::sync_channel(1);
        *self.pending.lock().unwrap() =
            Some(Pending { device: msg.device, sub: msg.sub, address: msg.address, tx });
        if let Err(e) = self.transport.write(&msg.encode()) {
            *self.pending.lock().unwrap() = None;
            if e.raw_os_error() == Some(19) || e.raw_os_error() == Some(5) {
                self.close();
                return Err(Error::Closed);
            }
            return Err(Error::Io(e.to_string()));
        }
        let result = rx.recv_timeout(timeout);
        *self.pending.lock().unwrap() = None;
        match result {
            Ok(r) => r,
            Err(_) if self.is_closed() => Err(Error::Closed),
            Err(_) => Err(Error::Timeout),
        }
    }

    /// Route one incoming report: complete the pending request if it answers
    /// it, otherwise forward it to the listener.
    fn dispatch(&self, msg: Message) {
        let mut pending = self.pending.lock().unwrap();
        if let Some(p) = pending.as_ref() {
            if let Some(result) = match_reply(p, &msg) {
                let p = pending.take().unwrap();
                let _ = p.tx.try_send(result);
                return;
            }
        }
        drop(pending);
        if let Some(l) = self.listener.lock().unwrap().as_ref() {
            let _ = l.send(ChannelEvent::Message(msg));
        }
    }

    fn fail_all(&self) {
        if let Some(p) = self.pending.lock().unwrap().take() {
            let _ = p.tx.try_send(Err(Error::Closed));
        }
        if let Some(l) = self.listener.lock().unwrap().as_ref() {
            let _ = l.send(ChannelEvent::Closed);
        }
    }
}

fn same_device(requested: u8, got: u8) -> bool {
    // Some directly-connected devices answer requests for 0xFF with index 0.
    requested == got || (requested == super::DEV_DIRECT && got == 0x00)
}

fn match_reply(p: &Pending, m: &Message) -> Option<Result<Message>> {
    if !same_device(p.device, m.device) {
        return None;
    }
    if m.sub == p.sub && m.address == p.address {
        return Some(Ok(m.clone()));
    }
    // Error replies echo the request's sub id and address in the payload.
    if m.address == p.sub && m.param(0) == p.address {
        if m.sub == ERROR_20 {
            return Some(Err(Error::V20(m.param(1))));
        }
        if m.sub == ERROR_10 {
            return Some(Err(Error::V10(m.param(1))));
        }
    }
    None
}

fn reader(transport: Arc<dyn Transport>, channel: Weak<Channel>) {
    let mut buf = [0u8; 64];
    loop {
        let result = transport.read_timeout(&mut buf, 250);
        let Some(ch) = channel.upgrade() else { return };
        if ch.is_closed() {
            ch.fail_all();
            return;
        }
        match result {
            Ok(0) => {}
            Ok(n) => {
                if let Some(msg) = Message::decode(&buf[..n]) {
                    ch.dispatch(msg);
                }
            }
            Err(e) => {
                crate::debug!("{}: read failed: {}", transport.describe(), e);
                ch.close();
                ch.fail_all();
                return;
            }
        }
    }
}
