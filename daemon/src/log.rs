//! Tiny leveled logger writing to stderr.
//!
//! When running under systemd (JOURNAL_STREAM is set) each line is prefixed
//! with a `<N>` syslog priority so journald records the right level.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(u8)]
pub enum Level {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
    Trace = 4,
}

static LEVEL: AtomicU8 = AtomicU8::new(Level::Info as u8);
static JOURNAL: AtomicBool = AtomicBool::new(false);

pub fn init(level: Level) {
    LEVEL.store(level as u8, Ordering::Relaxed);
    JOURNAL.store(std::env::var_os("JOURNAL_STREAM").is_some(), Ordering::Relaxed);
}

pub fn level_from_str(s: &str) -> Option<Level> {
    Some(match s.to_ascii_lowercase().as_str() {
        "error" => Level::Error,
        "warn" | "warning" => Level::Warn,
        "info" => Level::Info,
        "debug" => Level::Debug,
        "trace" => Level::Trace,
        _ => return None,
    })
}

pub fn enabled(level: Level) -> bool {
    (level as u8) <= LEVEL.load(Ordering::Relaxed)
}

pub fn log(level: Level, args: std::fmt::Arguments) {
    if !enabled(level) {
        return;
    }
    let mut err = std::io::stderr().lock();
    if JOURNAL.load(Ordering::Relaxed) {
        let prio = match level {
            Level::Error => 3,
            Level::Warn => 4,
            Level::Info => 6,
            Level::Debug | Level::Trace => 7,
        };
        let _ = writeln!(err, "<{}>{}", prio, args);
    } else {
        let tag = match level {
            Level::Error => "ERROR",
            Level::Warn => "WARN ",
            Level::Info => "INFO ",
            Level::Debug => "DEBUG",
            Level::Trace => "TRACE",
        };
        let _ = writeln!(err, "{} {}", tag, args);
    }
}

#[macro_export]
macro_rules! error {
    ($($t:tt)*) => { $crate::log::log($crate::log::Level::Error, format_args!($($t)*)) };
}
#[macro_export]
macro_rules! warn {
    ($($t:tt)*) => { $crate::log::log($crate::log::Level::Warn, format_args!($($t)*)) };
}
#[macro_export]
macro_rules! info {
    ($($t:tt)*) => { $crate::log::log($crate::log::Level::Info, format_args!($($t)*)) };
}
#[macro_export]
macro_rules! debug {
    ($($t:tt)*) => { $crate::log::log($crate::log::Level::Debug, format_args!($($t)*)) };
}
#[macro_export]
macro_rules! trace {
    ($($t:tt)*) => { $crate::log::log($crate::log::Level::Trace, format_args!($($t)*)) };
}

/// Hex dump helper for protocol tracing.
pub fn hex(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len() * 3);
    for (i, b) in data.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&format!("{:02x}", b));
    }
    s
}
