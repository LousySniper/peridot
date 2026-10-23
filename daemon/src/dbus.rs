//! A small D-Bus client/server implementation over Unix sockets.
//!
//! Supports what Peridot needs: EXTERNAL auth, method calls and replies,
//! errors and signals, with full marshalling of the basic and container types.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const METHOD_CALL: u8 = 1;
pub const METHOD_RETURN: u8 = 2;
pub const ERROR: u8 = 3;
pub const SIGNAL: u8 = 4;

pub const FLAG_NO_REPLY_EXPECTED: u8 = 0x1;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Byte(u8),
    Bool(bool),
    I16(i16),
    U16(u16),
    I32(i32),
    U32(u32),
    I64(i64),
    U64(u64),
    Double(f64),
    Str(String),
    Path(String),
    Sig(String),
    Variant(Box<Value>),
    /// Element signature and items.
    Array(String, Vec<Value>),
    Struct(Vec<Value>),
    /// Key signature, value signature, entries.
    Dict(String, String, Vec<(Value, Value)>),
}

impl Value {
    pub fn signature(&self) -> String {
        match self {
            Value::Byte(_) => "y".into(),
            Value::Bool(_) => "b".into(),
            Value::I16(_) => "n".into(),
            Value::U16(_) => "q".into(),
            Value::I32(_) => "i".into(),
            Value::U32(_) => "u".into(),
            Value::I64(_) => "x".into(),
            Value::U64(_) => "t".into(),
            Value::Double(_) => "d".into(),
            Value::Str(_) => "s".into(),
            Value::Path(_) => "o".into(),
            Value::Sig(_) => "g".into(),
            Value::Variant(_) => "v".into(),
            Value::Array(e, _) => format!("a{}", e),
            Value::Struct(fields) => format!("({})", fields.iter().map(|f| f.signature()).collect::<String>()),
            Value::Dict(k, v, _) => format!("a{{{}{}}}", k, v),
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) | Value::Path(s) | Value::Sig(s) => Some(s),
            Value::Variant(v) => v.as_str(),
            _ => None,
        }
    }

    pub fn as_u32(&self) -> Option<u32> {
        match self {
            Value::U32(v) => Some(*v),
            Value::Variant(v) => v.as_u32(),
            _ => None,
        }
    }
}

pub fn s(v: impl Into<String>) -> Value {
    Value::Str(v.into())
}

#[derive(Clone, Debug, PartialEq)]
pub struct Error {
    pub name: String,
    pub message: String,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.name, self.message)
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error { name: "org.freedesktop.DBus.Error.IOError".into(), message: e.to_string() }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Message {
    pub msg_type: u8,
    pub flags: u8,
    pub serial: u32,
    pub path: Option<String>,
    pub interface: Option<String>,
    pub member: Option<String>,
    pub error_name: Option<String>,
    pub reply_serial: Option<u32>,
    pub destination: Option<String>,
    pub sender: Option<String>,
    pub body: Vec<Value>,
}

impl Message {
    pub fn method_call(dest: &str, path: &str, iface: &str, member: &str, body: Vec<Value>) -> Message {
        Message {
            msg_type: METHOD_CALL,
            destination: Some(dest.into()),
            path: Some(path.into()),
            interface: Some(iface.into()),
            member: Some(member.into()),
            body,
            ..Default::default()
        }
    }

    pub fn signal(path: &str, iface: &str, member: &str, body: Vec<Value>) -> Message {
        Message {
            msg_type: SIGNAL,
            path: Some(path.into()),
            interface: Some(iface.into()),
            member: Some(member.into()),
            body,
            ..Default::default()
        }
    }

    pub fn signature(&self) -> String {
        self.body.iter().map(|v| v.signature()).collect()
    }

    pub fn arg_str(&self, i: usize) -> Option<&str> {
        self.body.get(i).and_then(|v| v.as_str())
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut body = Writer::new(false);
        for v in &self.body {
            body.write(v);
        }
        let mut fields: Vec<Value> = Vec::new();
        let mut field = |code: u8, v: Value| fields.push(Value::Struct(vec![Value::Byte(code), Value::Variant(Box::new(v))]));
        if let Some(p) = &self.path {
            field(1, Value::Path(p.clone()));
        }
        if let Some(i) = &self.interface {
            field(2, Value::Str(i.clone()));
        }
        if let Some(m) = &self.member {
            field(3, Value::Str(m.clone()));
        }
        if let Some(e) = &self.error_name {
            field(4, Value::Str(e.clone()));
        }
        if let Some(r) = self.reply_serial {
            field(5, Value::U32(r));
        }
        if let Some(d) = &self.destination {
            field(6, Value::Str(d.clone()));
        }
        if let Some(s) = &self.sender {
            field(7, Value::Str(s.clone()));
        }
        let sig = self.signature();
        if !sig.is_empty() {
            field(8, Value::Sig(sig));
        }
        let mut w = Writer::new(false);
        w.buf.extend_from_slice(&[b'l', self.msg_type, self.flags, 1]);
        w.buf.extend_from_slice(&(body.buf.len() as u32).to_le_bytes());
        w.buf.extend_from_slice(&self.serial.to_le_bytes());
        w.write(&Value::Array("(yv)".into(), fields));
        w.align(8);
        w.buf.extend_from_slice(&body.buf);
        w.buf
    }

    pub fn decode(data: &[u8]) -> Result<Message, String> {
        if data.len() < 16 {
            return Err("message too short".into());
        }
        let big = match data[0] {
            b'l' => false,
            b'B' => true,
            _ => return Err("bad endianness marker".into()),
        };
        let mut r = Reader { buf: data, pos: 4, big };
        let body_len = r.u32()? as usize;
        let serial = r.u32()?;
        let fields = r.read_type("a(yv)")?;
        r.align(8)?;
        let body_start = r.pos;
        let mut m = Message { msg_type: data[1], flags: data[2], serial, ..Default::default() };
        let mut sig = String::new();
        if let Value::Array(_, items) = fields {
            for item in items {
                if let Value::Struct(f) = item {
                    let code = match f.first() {
                        Some(Value::Byte(c)) => *c,
                        _ => continue,
                    };
                    let v = match f.get(1) {
                        Some(Value::Variant(v)) => (**v).clone(),
                        _ => continue,
                    };
                    match code {
                        1 => m.path = v.as_str().map(String::from),
                        2 => m.interface = v.as_str().map(String::from),
                        3 => m.member = v.as_str().map(String::from),
                        4 => m.error_name = v.as_str().map(String::from),
                        5 => m.reply_serial = v.as_u32(),
                        6 => m.destination = v.as_str().map(String::from),
                        7 => m.sender = v.as_str().map(String::from),
                        8 => sig = v.as_str().unwrap_or("").to_string(),
                        _ => {}
                    }
                }
            }
        }
        if body_start + body_len > data.len() {
            return Err("truncated body".into());
        }
        let mut br = Reader { buf: &data[body_start..body_start + body_len], pos: 0, big };
        for t in split_signature(&sig)? {
            m.body.push(br.read_type(&t)?);
        }
        Ok(m)
    }
}

fn alignment(sig: &str) -> usize {
    match sig.as_bytes().first() {
        Some(b'y') | Some(b'g') | Some(b'v') => 1,
        Some(b'n') | Some(b'q') => 2,
        Some(b'b') | Some(b'i') | Some(b'u') | Some(b's') | Some(b'o') | Some(b'a') | Some(b'h') => 4,
        Some(b'x') | Some(b't') | Some(b'd') | Some(b'(') | Some(b'{') => 8,
        _ => 1,
    }
}

/// Split a signature into its complete types.
pub fn split_signature(sig: &str) -> Result<Vec<String>, String> {
    let b = sig.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let end = single_type_end(b, i)?;
        out.push(sig[i..end].to_string());
        i = end;
    }
    Ok(out)
}

fn single_type_end(b: &[u8], i: usize) -> Result<usize, String> {
    match b.get(i) {
        None => Err("unexpected end of signature".into()),
        Some(b'a') => single_type_end(b, i + 1),
        Some(b'(') | Some(b'{') => {
            let close = if b[i] == b'(' { b')' } else { b'}' };
            let mut j = i + 1;
            while b.get(j) != Some(&close) {
                if j >= b.len() {
                    return Err("unterminated container in signature".into());
                }
                j = single_type_end(b, j)?;
            }
            Ok(j + 1)
        }
        Some(c) if b"ybnqiuxtdsogvh".contains(c) => Ok(i + 1),
        Some(c) => Err(format!("invalid signature character '{}'", *c as char)),
    }
}

struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    fn new(_big: bool) -> Writer {
        Writer { buf: Vec::new() }
    }

    fn align(&mut self, n: usize) {
        while self.buf.len() % n != 0 {
            self.buf.push(0);
        }
    }

    fn u32(&mut self, v: u32) {
        self.align(4);
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    fn write(&mut self, v: &Value) {
        match v {
            Value::Byte(b) => self.buf.push(*b),
            Value::Bool(b) => self.u32(*b as u32),
            Value::I16(x) => {
                self.align(2);
                self.buf.extend_from_slice(&x.to_le_bytes());
            }
            Value::U16(x) => {
                self.align(2);
                self.buf.extend_from_slice(&x.to_le_bytes());
            }
            Value::I32(x) => {
                self.align(4);
                self.buf.extend_from_slice(&x.to_le_bytes());
            }
            Value::U32(x) => self.u32(*x),
            Value::I64(x) => {
                self.align(8);
                self.buf.extend_from_slice(&x.to_le_bytes());
            }
            Value::U64(x) => {
                self.align(8);
                self.buf.extend_from_slice(&x.to_le_bytes());
            }
            Value::Double(x) => {
                self.align(8);
                self.buf.extend_from_slice(&x.to_le_bytes());
            }
            Value::Str(s) | Value::Path(s) => {
                self.u32(s.len() as u32);
                self.buf.extend_from_slice(s.as_bytes());
                self.buf.push(0);
            }
            Value::Sig(s) => {
                self.buf.push(s.len() as u8);
                self.buf.extend_from_slice(s.as_bytes());
                self.buf.push(0);
            }
            Value::Variant(inner) => {
                self.write(&Value::Sig(inner.signature()));
                self.write(inner);
            }
            Value::Array(elem, items) => {
                self.align(4);
                let len_pos = self.buf.len();
                self.buf.extend_from_slice(&[0; 4]);
                self.align(alignment(elem));
                let start = self.buf.len();
                for item in items {
                    self.write(item);
                }
                let len = (self.buf.len() - start) as u32;
                self.buf[len_pos..len_pos + 4].copy_from_slice(&len.to_le_bytes());
            }
            Value::Struct(fields) => {
                self.align(8);
                for f in fields {
                    self.write(f);
                }
            }
            Value::Dict(_, _, entries) => {
                self.align(4);
                let len_pos = self.buf.len();
                self.buf.extend_from_slice(&[0; 4]);
                self.align(8);
                let start = self.buf.len();
                for (k, v) in entries {
                    self.align(8);
                    self.write(k);
                    self.write(v);
                }
                let len = (self.buf.len() - start) as u32;
                self.buf[len_pos..len_pos + 4].copy_from_slice(&len.to_le_bytes());
            }
        }
    }
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    big: bool,
}

impl Reader<'_> {
    fn align(&mut self, n: usize) -> Result<(), String> {
        while self.pos % n != 0 {
            self.pos += 1;
        }
        if self.pos > self.buf.len() {
            return Err("alignment past end".into());
        }
        Ok(())
    }

    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        if self.pos + n > self.buf.len() {
            return Err("read past end of message".into());
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn u16(&mut self) -> Result<u16, String> {
        self.align(2)?;
        let b = self.take(2)?;
        let a = [b[0], b[1]];
        Ok(if self.big { u16::from_be_bytes(a) } else { u16::from_le_bytes(a) })
    }

    fn u32(&mut self) -> Result<u32, String> {
        self.align(4)?;
        let b = self.take(4)?;
        let a = [b[0], b[1], b[2], b[3]];
        Ok(if self.big { u32::from_be_bytes(a) } else { u32::from_le_bytes(a) })
    }

    fn u64(&mut self) -> Result<u64, String> {
        self.align(8)?;
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(if self.big { u64::from_be_bytes(a) } else { u64::from_le_bytes(a) })
    }

    fn string(&mut self) -> Result<String, String> {
        let len = self.u32()? as usize;
        let s = String::from_utf8(self.take(len)?.to_vec()).map_err(|_| "invalid utf-8".to_string())?;
        self.take(1)?;
        Ok(s)
    }

    fn sig(&mut self) -> Result<String, String> {
        let len = self.take(1)?[0] as usize;
        let s = String::from_utf8(self.take(len)?.to_vec()).map_err(|_| "invalid signature".to_string())?;
        self.take(1)?;
        Ok(s)
    }

    fn read_type(&mut self, t: &str) -> Result<Value, String> {
        let b = t.as_bytes();
        Ok(match b[0] {
            b'y' => Value::Byte(self.take(1)?[0]),
            b'b' => Value::Bool(self.u32()? != 0),
            b'n' => Value::I16(self.u16()? as i16),
            b'q' => Value::U16(self.u16()?),
            b'i' => Value::I32(self.u32()? as i32),
            b'u' | b'h' => Value::U32(self.u32()?),
            b'x' => Value::I64(self.u64()? as i64),
            b't' => Value::U64(self.u64()?),
            b'd' => Value::Double(f64::from_bits(self.u64()?)),
            b's' => Value::Str(self.string()?),
            b'o' => Value::Path(self.string()?),
            b'g' => Value::Sig(self.sig()?),
            b'v' => {
                let sig = self.sig()?;
                let types = split_signature(&sig)?;
                if types.len() != 1 {
                    return Err("variant must hold exactly one type".into());
                }
                Value::Variant(Box::new(self.read_type(&types[0])?))
            }
            b'a' => {
                let len = self.u32()? as usize;
                let elem = &t[1..];
                self.align(alignment(elem))?;
                let end = self.pos + len;
                if end > self.buf.len() {
                    return Err("array past end".into());
                }
                if elem.starts_with('{') {
                    let inner = split_signature(&elem[1..elem.len() - 1])?;
                    if inner.len() != 2 {
                        return Err("dict entry must have two types".into());
                    }
                    let mut entries = Vec::new();
                    while self.pos < end {
                        self.align(8)?;
                        let k = self.read_type(&inner[0])?;
                        let v = self.read_type(&inner[1])?;
                        entries.push((k, v));
                    }
                    Value::Dict(inner[0].clone(), inner[1].clone(), entries)
                } else {
                    let mut items = Vec::new();
                    while self.pos < end {
                        items.push(self.read_type(elem)?);
                    }
                    Value::Array(elem.to_string(), items)
                }
            }
            b'(' => {
                self.align(8)?;
                let inner = split_signature(&t[1..t.len() - 1])?;
                let mut fields = Vec::new();
                for f in inner {
                    fields.push(self.read_type(&f)?);
                }
                Value::Struct(fields)
            }
            c => return Err(format!("unsupported type '{}'", c as char)),
        })
    }
}

/// Read one complete message from a stream.
fn read_message(stream: &mut UnixStream) -> io::Result<Message> {
    let mut head = [0u8; 16];
    stream.read_exact(&mut head)?;
    let big = head[0] == b'B';
    let get = |i: usize| {
        let a = [head[i], head[i + 1], head[i + 2], head[i + 3]];
        if big { u32::from_be_bytes(a) } else { u32::from_le_bytes(a) }
    };
    let body_len = get(4) as usize;
    let fields_len = get(12) as usize;
    let header_len = (16 + fields_len + 7) & !7;
    let total = header_len + body_len;
    if total > 128 * 1024 * 1024 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "message too large"));
    }
    let mut data = vec![0u8; total];
    data[..16].copy_from_slice(&head);
    stream.read_exact(&mut data[16..])?;
    Message::decode(&data).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Resolve the session bus address.
pub fn session_address() -> String {
    if let Ok(a) = std::env::var("DBUS_SESSION_BUS_ADDRESS") {
        if !a.is_empty() {
            return a;
        }
    }
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| format!("/run/user/{}", crate::sys::uid()));
    format!("unix:path={}/bus", runtime)
}

fn connect_stream(address: &str) -> io::Result<UnixStream> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, format!("no usable address in '{}'", address));
    for addr in address.split(';') {
        let Some(rest) = addr.strip_prefix("unix:") else { continue };
        let mut path = None;
        let mut abstract_name = None;
        for kv in rest.split(',') {
            if let Some(v) = kv.strip_prefix("path=") {
                path = Some(unescape(v));
            } else if let Some(v) = kv.strip_prefix("abstract=") {
                abstract_name = Some(unescape(v));
            }
        }
        let result = if let Some(p) = path {
            UnixStream::connect(p)
        } else if let Some(name) = abstract_name {
            use std::os::linux::net::SocketAddrExt;
            std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes())
                .and_then(|a| UnixStream::connect_addr(&a))
        } else {
            continue;
        };
        match result {
            Ok(s) => return Ok(s),
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn unescape(v: &str) -> String {
    let b = v.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(x) = u8::from_str_radix(&v[i + 1..i + 3], 16) {
                out.push(x);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn read_line(stream: &mut UnixStream) -> io::Result<String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while !line.ends_with(b"\r\n") {
        stream.read_exact(&mut byte)?;
        line.push(byte[0]);
        if line.len() > 4096 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "auth line too long"));
        }
    }
    line.truncate(line.len() - 2);
    Ok(String::from_utf8_lossy(&line).into_owned())
}

fn authenticate(stream: &mut UnixStream) -> io::Result<()> {
    let uid = crate::sys::uid().to_string();
    let hex: String = uid.bytes().map(|b| format!("{:02x}", b)).collect();
    stream.write_all(format!("\0AUTH EXTERNAL {}\r\n", hex).as_bytes())?;
    let reply = read_line(stream)?;
    if !reply.starts_with("OK ") {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("D-Bus authentication failed: {}", reply)));
    }
    stream.write_all(b"BEGIN\r\n")?;
    Ok(())
}

pub struct Connection {
    writer: Mutex<UnixStream>,
    serial: AtomicU32,
    pending: Mutex<HashMap<u32, Sender<Message>>>,
    incoming: Mutex<Option<Sender<Message>>>,
    unique_name: Mutex<String>,
}

impl Connection {
    pub fn session() -> io::Result<Arc<Connection>> {
        Connection::connect(&session_address())
    }

    pub fn connect(address: &str) -> io::Result<Arc<Connection>> {
        let mut stream = connect_stream(address)?;
        authenticate(&mut stream)?;
        let reader = stream.try_clone()?;
        let conn = Arc::new(Connection {
            writer: Mutex::new(stream),
            serial: AtomicU32::new(1),
            pending: Mutex::new(HashMap::new()),
            incoming: Mutex::new(None),
            unique_name: Mutex::new(String::new()),
        });
        let weak = Arc::downgrade(&conn);
        std::thread::Builder::new().name("dbus-reader".into()).spawn(move || {
            let mut reader = reader;
            loop {
                let msg = match read_message(&mut reader) {
                    Ok(m) => m,
                    Err(e) => {
                        crate::debug!("dbus: connection closed: {}", e);
                        if let Some(c) = weak.upgrade() {
                            c.pending.lock().unwrap().clear();
                            *c.incoming.lock().unwrap() = None;
                        }
                        return;
                    }
                };
                let Some(conn) = weak.upgrade() else { return };
                if matches!(msg.msg_type, METHOD_RETURN | ERROR) {
                    if let Some(rs) = msg.reply_serial {
                        if let Some(tx) = conn.pending.lock().unwrap().remove(&rs) {
                            let _ = tx.send(msg);
                            continue;
                        }
                    }
                }
                let incoming = conn.incoming.lock().unwrap().clone();
                if let Some(tx) = incoming {
                    let _ = tx.send(msg);
                }
            }
        })?;
        let hello = conn.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "Hello", vec![])
            .map_err(|e| io::Error::other(e.to_string()))?;
        *conn.unique_name.lock().unwrap() = hello.arg_str(0).unwrap_or("").to_string();
        Ok(conn)
    }

    pub fn unique_name(&self) -> String {
        self.unique_name.lock().unwrap().clone()
    }

    /// Route method calls and signals addressed to us into a channel.
    pub fn incoming(&self) -> Receiver<Message> {
        let (tx, rx) = mpsc::channel();
        *self.incoming.lock().unwrap() = Some(tx);
        rx
    }

    pub fn send(&self, mut msg: Message) -> io::Result<u32> {
        let serial = self.serial.fetch_add(1, Ordering::Relaxed);
        msg.serial = serial;
        let data = msg.encode();
        let mut w = self.writer.lock().unwrap();
        w.write_all(&data)?;
        Ok(serial)
    }

    pub fn call_timeout(
        &self,
        dest: &str,
        path: &str,
        iface: &str,
        member: &str,
        body: Vec<Value>,
        timeout: Duration,
    ) -> Result<Message, Error> {
        let msg = Message::method_call(dest, path, iface, member, body);
        let (tx, rx) = mpsc::channel();
        // Reserve the serial before sending so the reply can't race us.
        let serial = self.serial.fetch_add(1, Ordering::Relaxed);
        self.pending.lock().unwrap().insert(serial, tx);
        let mut m = msg;
        m.serial = serial;
        {
            let mut w = self.writer.lock().unwrap();
            if let Err(e) = w.write_all(&m.encode()) {
                self.pending.lock().unwrap().remove(&serial);
                return Err(e.into());
            }
        }
        match rx.recv_timeout(timeout) {
            Ok(reply) if reply.msg_type == ERROR => Err(Error {
                name: reply.error_name.clone().unwrap_or_default(),
                message: reply.arg_str(0).unwrap_or("").to_string(),
            }),
            Ok(reply) => Ok(reply),
            Err(_) => {
                self.pending.lock().unwrap().remove(&serial);
                Err(Error { name: "org.freedesktop.DBus.Error.NoReply".into(), message: "no reply".into() })
            }
        }
    }

    pub fn call(&self, dest: &str, path: &str, iface: &str, member: &str, body: Vec<Value>) -> Result<Message, Error> {
        self.call_timeout(dest, path, iface, member, body, Duration::from_secs(25))
    }

    pub fn reply(&self, call: &Message, body: Vec<Value>) -> io::Result<()> {
        if call.flags & FLAG_NO_REPLY_EXPECTED != 0 {
            return Ok(());
        }
        let m = Message {
            msg_type: METHOD_RETURN,
            reply_serial: Some(call.serial),
            destination: call.sender.clone(),
            body,
            ..Default::default()
        };
        self.send(m).map(|_| ())
    }

    pub fn reply_error(&self, call: &Message, name: &str, text: &str) -> io::Result<()> {
        if call.flags & FLAG_NO_REPLY_EXPECTED != 0 {
            return Ok(());
        }
        let m = Message {
            msg_type: ERROR,
            reply_serial: Some(call.serial),
            destination: call.sender.clone(),
            error_name: Some(name.into()),
            body: vec![s(text)],
            ..Default::default()
        };
        self.send(m).map(|_| ())
    }

    pub fn emit(&self, path: &str, iface: &str, member: &str, body: Vec<Value>) -> io::Result<()> {
        self.send(Message::signal(path, iface, member, body)).map(|_| ())
    }

    /// RequestName with DO_NOT_QUEUE. Returns true if we became the owner.
    pub fn request_name(&self, name: &str) -> Result<bool, Error> {
        let r = self.call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "RequestName",
            vec![s(name), Value::U32(4)],
        )?;
        Ok(matches!(r.body.first(), Some(Value::U32(1)) | Some(Value::U32(4))))
    }

    pub fn add_match(&self, rule: &str) -> Result<(), Error> {
        self.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "AddMatch", vec![s(rule)])
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_split() {
        assert_eq!(split_signature("sa{sv}(ii)as").unwrap(), vec!["s", "a{sv}", "(ii)", "as"]);
        assert!(split_signature("a").is_err());
        assert!(split_signature("(ii").is_err());
    }

    #[test]
    fn roundtrip() {
        let body = vec![
            s("héllo"),
            Value::U32(7),
            Value::Bool(true),
            Value::Array("s".into(), vec![s("a"), s("bc")]),
            Value::Dict("s".into(), "v".into(), vec![(s("k"), Value::Variant(Box::new(Value::I64(-5))))]),
            Value::Struct(vec![Value::Byte(1), Value::Double(2.5), Value::Path("/a/b".into())]),
            Value::Array("y".into(), vec![]),
        ];
        let mut m = Message::method_call("dest.x", "/obj", "if.ace", "Member", body.clone());
        m.serial = 42;
        m.sender = Some(":1.5".into());
        let enc = m.encode();
        let dec = Message::decode(&enc).unwrap();
        assert_eq!(dec.body, body);
        assert_eq!(dec.member.as_deref(), Some("Member"));
        assert_eq!(dec.serial, 42);
        assert_eq!(dec.sender.as_deref(), Some(":1.5"));
        assert_eq!(dec.signature(), "subasa{sv}(ydo)ay");
    }

    #[test]
    fn decodes_big_endian() {
        // METHOD_RETURN, serial 2, reply_serial 1, body "s" = "ok"
        let mut d = vec![b'B', 2, 0, 1];
        d.extend_from_slice(&7u32.to_be_bytes()); // body len
        d.extend_from_slice(&2u32.to_be_bytes()); // serial
        let mut fields = Vec::new();
        fields.extend_from_slice(&[5, 1, b'u', 0]);
        fields.extend_from_slice(&1u32.to_be_bytes());
        fields.extend_from_slice(&[8, 1, b'g', 0, 1, b's', 0]);
        d.extend_from_slice(&(fields.len() as u32).to_be_bytes());
        d.extend_from_slice(&fields);
        while d.len() % 8 != 0 {
            d.push(0);
        }
        d.extend_from_slice(&2u32.to_be_bytes());
        d.extend_from_slice(b"ok\0");
        let m = Message::decode(&d).unwrap();
        assert_eq!(m.reply_serial, Some(1));
        assert_eq!(m.arg_str(0), Some("ok"));
    }
}
