//! peridotd: the Peridot device daemon (runs as a systemd user service).

use peridot::dbus::Connection;
use peridot::log::{self, Level};
use peridot::manager::{Manager, Options};
use std::path::PathBuf;
use std::process::ExitCode;

const HELP: &str = "\
peridotd - device daemon for Logitech and Razer mice and headsets

USAGE: peridotd [OPTIONS]

OPTIONS:
  --config-dir DIR    where saved settings live (default: ~/.config/peridot)
  -v, --verbose       more logging (repeat for protocol traces)
  -q, --quiet         only warnings and errors
  --version           print version
  -h, --help          print this help

Logging can also be set with PERIDOT_LOG=error|warn|info|debug|trace.
";

fn main() -> ExitCode {
    let mut level = std::env::var("PERIDOT_LOG").ok().and_then(|s| log::level_from_str(&s)).unwrap_or(Level::Info);
    let mut config_dir: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config-dir" => config_dir = args.next().map(PathBuf::from),
            "-v" | "--verbose" => level = if level >= Level::Debug { Level::Trace } else { Level::Debug },
            "-vv" => level = Level::Trace,
            "-q" | "--quiet" => level = Level::Warn,
            "--version" => {
                println!("peridotd {}", peridot::VERSION);
                return ExitCode::SUCCESS;
            }
            "-h" | "--help" => {
                print!("{}", HELP);
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("unknown option '{}'\n\n{}", other, HELP);
                return ExitCode::from(2);
            }
        }
    }
    log::init(level);

    let config_dir = config_dir.unwrap_or_else(peridot::config::default_dir);

    let conn = match Connection::session() {
        Ok(c) => c,
        Err(e) => {
            peridot::error!("cannot connect to the session bus: {}", e);
            return ExitCode::FAILURE;
        }
    };
    match conn.request_name(peridot::BUS_NAME) {
        Ok(true) => {}
        Ok(false) => {
            peridot::error!("{} is already owned: another peridotd is running", peridot::BUS_NAME);
            return ExitCode::FAILURE;
        }
        Err(e) => {
            peridot::error!("cannot register {}: {}", peridot::BUS_NAME, e);
            return ExitCode::FAILURE;
        }
    }
    peridot::info!("peridotd {} on {}", peridot::VERSION, conn.unique_name());

    peridot::sys::catch_termination();
    let mut manager = Manager::new(Options { config_dir }, Some(conn.clone()));
    peridot::service::spawn_dispatcher(&conn, manager.sender());
    manager.start();
    manager.run();
    ExitCode::SUCCESS
}
