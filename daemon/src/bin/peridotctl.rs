//! peridotctl: command-line client for peridotd.

use peridot::dbus::{self, Connection, Message};
use peridot::json::Json;
use std::process::ExitCode;
use std::time::Duration;

const HELP: &str = "\
peridotctl - control mice and headsets managed by peridotd

USAGE: peridotctl <command> [args]

COMMANDS:
  list                              devices with battery and connection
  show <device>                     everything about a device
  json [<device>]                   raw JSON (all devices or one)
  dpi <device> <value>              set sensitivity
  stages <device> <dpi,dpi,..> [n]  set DPI stages, n = active stage (1-based)
  rate <device> <hz>                set polling rate
  light <device> <effect> [#rrggbb] [--zone Z] [--speed 0-100]
                                    effects: off static breathing spectrum wave reactive ripple
  brightness <device> <0-100> [--zone Z]
  button <device> <control> <action>
                                    e.g. button mouse 0053 key:ctrl+c
  sidetone <device> <level>
  eq <device> <gain,gain,...>
  mode <device> <mode> <value>      e.g. mode blackshark anc ambient
  set <device> <key> <json>         any setting, raw
  refresh <device>
  forget <device>                   drop saved settings
  actions                           list button actions
  status                            daemon status and warnings
  monitor                           print device events as they happen

<device> is a device id or any part of its name (case-insensitive).
";

fn call(conn: &Connection, member: &str, args: Vec<dbus::Value>) -> Result<Message, String> {
    conn.call_timeout(peridot::BUS_NAME, peridot::OBJECT_PATH, peridot::INTERFACE, member, args, Duration::from_secs(30))
        .map_err(|e| {
            if e.name.contains("ServiceUnknown") || e.name.contains("NameHasNoOwner") {
                "peridotd is not running (start it with: systemctl --user start peridotd)".to_string()
            } else {
                e.message
            }
        })
}

fn json_reply(m: &Message) -> Result<Json, String> {
    Json::parse(m.arg_str(0).unwrap_or("null")).map_err(|e| format!("bad reply: {}", e))
}

fn devices(conn: &Connection) -> Result<Vec<Json>, String> {
    Ok(json_reply(&call(conn, "ListDevices", vec![])?)?.as_arr().cloned().unwrap_or_default())
}

fn resolve(conn: &Connection, needle: &str) -> Result<Json, String> {
    let list = devices(conn)?;
    if let Some(d) = list.iter().find(|d| d.get("id").as_str() == Some(needle)) {
        return Ok(d.clone());
    }
    let n = needle.to_lowercase();
    let hits: Vec<&Json> = list
        .iter()
        .filter(|d| {
            d.get("name").as_str().unwrap_or("").to_lowercase().contains(&n)
                || d.get("id").as_str().unwrap_or("").starts_with(&n)
                || d.get("kind").as_str() == Some(n.as_str())
        })
        .collect();
    match hits.len() {
        1 => Ok(hits[0].clone()),
        0 => Err(format!("no device matches '{}'", needle)),
        _ => Err(format!(
            "'{}' matches several devices: {}",
            needle,
            hits.iter().map(|d| d.get("name").as_str().unwrap_or("?")).collect::<Vec<_>>().join(", ")
        )),
    }
}

fn battery_text(d: &Json) -> String {
    let b = d.get("battery");
    if b.is_null() {
        return "—".into();
    }
    let mut s = match b.get("percent").as_i64() {
        Some(p) => format!("{}%", p),
        None => "?".into(),
    };
    if b.get("estimated").as_bool() == Some(true) {
        s = format!("~{}", s);
    }
    if b.get("charging").as_bool() == Some(true) {
        s.push_str(" ⚡");
    } else if b.get("full").as_bool() == Some(true) {
        s.push_str(" (full)");
    }
    s
}

fn print_list(list: &[Json]) {
    if list.is_empty() {
        println!("No devices found.");
        return;
    }
    println!("{:<28} {:<9} {:<10} {:<10} {}", "NAME", "KIND", "BATTERY", "LINK", "ID");
    for d in list {
        let link = if d.get("online").as_bool() == Some(false) { "asleep".to_string() } else { d.get("connection").as_str().unwrap_or("").to_string() };
        println!(
            "{:<28} {:<9} {:<10} {:<10} {}",
            d.get("name").as_str().unwrap_or("?"),
            d.get("kind").as_str().unwrap_or("?"),
            battery_text(d),
            link,
            d.get("id").as_str().unwrap_or("?")
        );
    }
}

fn print_device(d: &Json) {
    println!("{}", d.get("name").as_str().unwrap_or("?"));
    let mut info = vec![
        format!("id {}", d.get("id").as_str().unwrap_or("")),
        format!("{} {}", d.get("vendor").as_str().unwrap_or(""), d.get("kind").as_str().unwrap_or("")),
        format!("model {}", d.get("model").as_str().unwrap_or("")),
    ];
    if let Some(v) = d.get("via").as_str() {
        info.push(format!("via {}", v));
    }
    if let Some(f) = d.get("firmware").as_str() {
        info.push(format!("firmware {}", f));
    }
    println!("  {}", info.join(" · "));
    println!("  {}  battery {}", if d.get("online").as_bool() == Some(true) { "online " } else { "asleep " }, battery_text(d));
    let dpi = d.get("dpi");
    if !dpi.is_null() {
        let stages: Vec<String> = dpi
            .get("stages")
            .as_arr()
            .map(|a| {
                a.iter()
                    .enumerate()
                    .map(|(i, s)| {
                        let v = s.as_i64().unwrap_or(0);
                        if Some(i as i64) == dpi.get("active_stage").as_i64() { format!("[{}]", v) } else { v.to_string() }
                    })
                    .collect()
            })
            .unwrap_or_default();
        println!(
            "  DPI {} (range {}-{}), stages {}",
            dpi.get("value").as_i64().unwrap_or(0),
            dpi.get("min").as_i64().unwrap_or(0),
            dpi.get("max").as_i64().unwrap_or(0),
            stages.join(" ")
        );
    }
    let pr = d.get("polling_rate");
    if !pr.is_null() {
        println!("  polling {} Hz (supports {})", pr.get("value").as_i64().unwrap_or(0), pr.get("options").to_string_compact());
    }
    if let Some(zones) = d.get("lighting").get("zones").as_arr() {
        for z in zones {
            let cur = z.get("current");
            let now = if cur.is_null() { "unchanged".to_string() } else { format!("{} {}", cur.get("effect").as_str().unwrap_or(""), cur.get("color").as_str().unwrap_or("")) };
            let bright = z.get("brightness").as_i64().map(|b| format!(", brightness {}%", b)).unwrap_or_default();
            println!(
                "  light {:<10} {} (can do {}){}",
                z.get("id").as_str().unwrap_or(""),
                now,
                z.get("effects").as_arr().map(|a| a.iter().filter_map(|e| e.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default(),
                bright
            );
        }
    }
    if let Some(controls) = d.get("buttons").get("controls").as_arr() {
        for c in controls {
            println!(
                "  button {} {:<16} -> {}",
                c.get("id").as_str().unwrap_or(""),
                c.get("name").as_str().unwrap_or(""),
                c.get("action").as_str().unwrap_or("")
            );
        }
    }
    let audio = d.get("audio");
    if !audio.is_null() {
        if let Some(v) = audio.get("sidetone").get("value").as_i64() {
            match audio.get("sidetone").get("max").as_i64() {
                Some(max) if max != 100 => println!("  sidetone {} of {}", v, max),
                _ => println!("  sidetone {}%", v),
            }
        }
        if let Some(bands) = audio.get("equalizer").get("bands").as_arr() {
            let b: Vec<String> = bands
                .iter()
                .map(|x| format!("{}Hz:{:+}", x.get("freq").as_i64().unwrap_or(0), x.get("gain").as_i64().unwrap_or(0)))
                .collect();
            println!("  equalizer {}", b.join(" "));
        }
        if let Some(v) = audio.get("auto_off").get("value").as_i64() {
            println!("  auto power-off {} min", v);
        }
        for m in audio.get("modes").as_arr().cloned().unwrap_or_default() {
            let opts: Vec<&str> =
                m.get("options").as_arr().map(|o| o.iter().filter_map(|x| x.get("value").as_str()).collect()).unwrap_or_default();
            println!(
                "  {} ({}): {} (can be {})",
                m.get("name").as_str().unwrap_or("?"),
                m.get("id").as_str().unwrap_or("?"),
                m.get("value").as_str().unwrap_or("?"),
                opts.join(", ")
            );
        }
    }
    let power = d.get("power");
    if let Some(v) = power.get("idle_time").get("value").as_i64() {
        println!("  sleep after {} s idle", v);
    }
    if let Some(v) = power.get("low_battery").get("value").as_i64() {
        println!("  low battery warning at {}%", v);
    }
    if let Some(m) = d.get("onboard_mode").as_str() {
        println!("  on-board profile mode: {}", m);
    }
    for n in d.get("notes").as_arr().cloned().unwrap_or_default() {
        println!("  note: {}", n.as_str().unwrap_or(""));
    }
}

fn set(conn: &Connection, dev: &Json, key: &str, value: Json) -> Result<(), String> {
    let id = dev.get("id").as_str().unwrap_or("").to_string();
    let reply = call(conn, "SetSetting", vec![dbus::s(id), dbus::s(key), dbus::s(value.to_string_compact())]);
    match reply {
        Ok(m) => {
            let d = json_reply(&m)?;
            println!("✓ {} updated", d.get("name").as_str().unwrap_or("device"));
            Ok(())
        }
        Err(e) if e.contains("offline") => {
            println!("⏾ the device is asleep; the change was saved and will apply when it wakes up");
            Ok(())
        }
        Err(e) => Err(e),
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

fn run(args: Vec<String>) -> Result<(), String> {
    let cmd = args.first().map(String::as_str).unwrap_or("list");
    if matches!(cmd, "-h" | "--help" | "help") {
        print!("{}", HELP);
        return Ok(());
    }
    if cmd == "--version" {
        println!("peridotctl {}", peridot::VERSION);
        return Ok(());
    }
    let conn = Connection::session().map_err(|e| format!("cannot connect to the session bus: {}", e))?;
    let need = |n: usize| -> Result<(), String> {
        if args.len() < n {
            Err(format!("missing arguments; see 'peridotctl help'"))
        } else {
            Ok(())
        }
    };
    let num = |s: &str| -> Result<Json, String> { s.parse::<f64>().map(Json::Num).map_err(|_| format!("'{}' is not a number", s)) };
    match cmd {
        "list" | "ls" => print_list(&devices(&conn)?),
        "show" => {
            need(2)?;
            print_device(&resolve(&conn, &args[1])?);
        }
        "json" => {
            if args.len() > 1 {
                print!("{}", resolve(&conn, &args[1])?.to_string_pretty());
            } else {
                print!("{}", Json::Arr(devices(&conn)?).to_string_pretty());
            }
        }
        "dpi" => {
            need(3)?;
            set(&conn, &resolve(&conn, &args[1])?, "dpi", num(&args[2])?)?;
        }
        "stages" => {
            need(3)?;
            let stages: Result<Vec<Json>, String> = args[2].split(',').map(|s| num(s.trim())).collect();
            let active = args.get(3).and_then(|a| a.parse::<i64>().ok()).unwrap_or(1) - 1;
            set(&conn, &resolve(&conn, &args[1])?, "dpi_stages", Json::obj().with("stages", Json::Arr(stages?)).with("active", active))?;
        }
        "rate" => {
            need(3)?;
            set(&conn, &resolve(&conn, &args[1])?, "polling_rate", num(&args[2])?)?;
        }
        "light" => {
            need(3)?;
            let mut v = Json::obj().with("zone", flag(&args, "--zone").unwrap_or_else(|| "all".into())).with("effect", args[2].as_str());
            if let Some(c) = args.get(3).filter(|c| c.starts_with('#')) {
                v.set("color", c.as_str());
            }
            if let Some(s) = flag(&args, "--speed") {
                v.set("speed", num(&s)?);
            }
            set(&conn, &resolve(&conn, &args[1])?, "lighting", v)?;
        }
        "brightness" => {
            need(3)?;
            let v = Json::obj().with("zone", flag(&args, "--zone").unwrap_or_else(|| "all".into())).with("value", num(&args[2])?);
            set(&conn, &resolve(&conn, &args[1])?, "brightness", v)?;
        }
        "button" => {
            need(4)?;
            let v = Json::obj().with("control", args[2].as_str()).with("action", args[3].as_str());
            set(&conn, &resolve(&conn, &args[1])?, "button", v)?;
        }
        "sidetone" => {
            need(3)?;
            set(&conn, &resolve(&conn, &args[1])?, "sidetone", num(&args[2])?)?;
        }
        "eq" => {
            need(3)?;
            let gains: Result<Vec<Json>, String> = args[2].split(',').map(|s| num(s.trim())).collect();
            set(&conn, &resolve(&conn, &args[1])?, "equalizer", Json::Arr(gains?))?;
        }
        "mode" => {
            need(4)?;
            let v = Json::obj().with("id", args[2].as_str()).with("value", args[3].as_str());
            set(&conn, &resolve(&conn, &args[1])?, "mode", v)?;
        }
        "set" => {
            need(4)?;
            let value = Json::parse(&args[3]).unwrap_or_else(|_| Json::from(args[3].as_str()));
            set(&conn, &resolve(&conn, &args[1])?, &args[2], value)?;
        }
        "refresh" => {
            need(2)?;
            let d = resolve(&conn, &args[1])?;
            let m = call(&conn, "Refresh", vec![dbus::s(d.get("id").as_str().unwrap_or(""))])?;
            print_device(&json_reply(&m)?);
        }
        "forget" => {
            need(2)?;
            let d = resolve(&conn, &args[1])?;
            call(&conn, "ForgetDevice", vec![dbus::s(d.get("id").as_str().unwrap_or(""))])?;
            println!("✓ saved settings for {} removed", d.get("name").as_str().unwrap_or("device"));
        }
        "actions" => {
            let list = json_reply(&call(&conn, "GetActions", vec![])?)?;
            for a in list.as_arr().cloned().unwrap_or_default() {
                println!("{:<22} {:<12} {}", a.get("id").as_str().unwrap_or(""), a.get("group").as_str().unwrap_or(""), a.get("label").as_str().unwrap_or(""));
            }
            println!("\nAny key combination works too, e.g. key:ctrl+alt+t, key:super+e, key:f13");
        }
        "status" => {
            let s = json_reply(&call(&conn, "GetStatus", vec![])?)?;
            println!("peridotd {} · {} device(s)", s.get("version").as_str().unwrap_or("?"), s.get("devices").as_i64().unwrap_or(0));
            match s.get("uinput_error").as_str() {
                None => println!("shortcuts and media keys: available"),
                Some(e) => println!("shortcuts and media keys: unavailable. {}", e),
            }
            println!("settings: {}", s.get("config_dir").as_str().unwrap_or(""));
            for w in s.get("warnings").as_arr().cloned().unwrap_or_default() {
                println!("warning: {}", w.as_str().unwrap_or(""));
            }
        }
        "monitor" => {
            conn.add_match(&format!("type='signal',interface='{}'", peridot::INTERFACE)).map_err(|e| e.message)?;
            let rx = conn.incoming();
            println!("watching for device events (Ctrl+C to stop)");
            for m in rx {
                if m.msg_type != dbus::SIGNAL || m.interface.as_deref() != Some(peridot::INTERFACE) {
                    continue;
                }
                let member = m.member.clone().unwrap_or_default();
                let arg = m.arg_str(0).unwrap_or("");
                match member.as_str() {
                    "DeviceRemoved" => println!("- removed {}", arg),
                    _ => {
                        let d = Json::parse(arg).unwrap_or(Json::Null);
                        let sign = if member == "DeviceAdded" { "+" } else { "~" };
                        println!(
                            "{} {} · battery {} · {}",
                            sign,
                            d.get("name").as_str().unwrap_or("?"),
                            battery_text(&d),
                            if d.get("online").as_bool() == Some(true) { "online" } else { "asleep" }
                        );
                    }
                }
            }
        }
        other => return Err(format!("unknown command '{}'; see 'peridotctl help'", other)),
    }
    Ok(())
}

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("peridotctl: {}", e);
            ExitCode::FAILURE
        }
    }
}
