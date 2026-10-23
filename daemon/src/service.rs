//! The `io.github.peridot.Manager1` D-Bus interface.
//!
//! Every payload is a JSON string so clients (the Qt app, peridotctl, scripts)
//! don't need generated bindings. See docs/dbus-api.md for the schema.

use crate::dbus::{self, Connection, Message, Value};
use crate::json::Json;
use crate::manager::{Event, Manager};
use std::sync::mpsc::Sender;
use std::sync::Arc;

pub const INTROSPECTION: &str = r#"<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
<node>
  <interface name="io.github.peridot.Manager1">
    <method name="GetVersion"><arg type="s" direction="out"/></method>
    <method name="GetStatus"><arg type="s" direction="out"/></method>
    <method name="ListDevices"><arg type="s" direction="out"/></method>
    <method name="GetDevice"><arg name="id" type="s" direction="in"/><arg type="s" direction="out"/></method>
    <method name="SetSetting">
      <arg name="id" type="s" direction="in"/>
      <arg name="key" type="s" direction="in"/>
      <arg name="value" type="s" direction="in"/>
      <arg name="device" type="s" direction="out"/>
    </method>
    <method name="Refresh"><arg name="id" type="s" direction="in"/><arg type="s" direction="out"/></method>
    <method name="ForgetDevice"><arg name="id" type="s" direction="in"/></method>
    <method name="GetActions"><arg type="s" direction="out"/></method>
    <signal name="DeviceAdded"><arg name="device" type="s"/></signal>
    <signal name="DeviceChanged"><arg name="device" type="s"/></signal>
    <signal name="DeviceRemoved"><arg name="id" type="s"/></signal>
  </interface>
  <interface name="org.freedesktop.DBus.Introspectable">
    <method name="Introspect"><arg type="s" direction="out"/></method>
  </interface>
  <interface name="org.freedesktop.DBus.Peer">
    <method name="Ping"/>
    <method name="GetMachineId"><arg type="s" direction="out"/></method>
  </interface>
  <interface name="org.freedesktop.DBus.Properties">
    <method name="Get"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="out"/></method>
    <method name="GetAll"><arg type="s" direction="in"/><arg type="a{sv}" direction="out"/></method>
  </interface>
</node>
"#;

const ROOT_INTROSPECTION: &str = r#"<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
<node><node name="io"/></node>
"#;

/// Forward incoming D-Bus traffic into the manager's event loop.
pub fn spawn_dispatcher(conn: &Arc<Connection>, tx: Sender<Event>) {
    let rx = conn.incoming();
    let _ = std::thread::Builder::new().name("dbus-dispatch".into()).spawn(move || {
        for msg in rx {
            if msg.msg_type == dbus::METHOD_CALL && tx.send(Event::Call(msg)).is_err() {
                return;
            }
        }
        // Connection lost: the bus went away, nothing left to serve.
        crate::error!("lost the D-Bus connection; exiting");
        let _ = tx.send(Event::Quit);
    });
}

fn reply_json(conn: &Connection, call: &Message, j: &Json) {
    let _ = conn.reply(call, vec![dbus::s(j.to_string_compact())]);
}

fn intermediate_path(path: &str) -> bool {
    crate::OBJECT_PATH.starts_with(path) && path != crate::OBJECT_PATH
}

pub fn handle_call(m: &mut Manager, call: Message) {
    let Some(conn) = m.conn().cloned() else { return };
    let iface = call.interface.clone().unwrap_or_default();
    let member = call.member.clone().unwrap_or_default();
    let path = call.path.clone().unwrap_or_default();

    if iface == "org.freedesktop.DBus.Introspectable" || (iface.is_empty() && member == "Introspect") {
        let xml = if path == crate::OBJECT_PATH {
            INTROSPECTION.to_string()
        } else if path == "/" || intermediate_path(&path) {
            // walk down toward our object so tools like busctl can find it
            let rest = crate::OBJECT_PATH.trim_start_matches(path.trim_end_matches('/')).trim_start_matches('/');
            let next = rest.split('/').next().unwrap_or("");
            ROOT_INTROSPECTION.replace("<node name=\"io\"/>", &format!("<node name=\"{}\"/>", next))
        } else {
            let _ = conn.reply_error(&call, "org.freedesktop.DBus.Error.UnknownObject", "no such object");
            return;
        };
        let _ = conn.reply(&call, vec![dbus::s(xml)]);
        return;
    }
    if iface == "org.freedesktop.DBus.Peer" {
        match member.as_str() {
            "Ping" => {
                let _ = conn.reply(&call, vec![]);
            }
            "GetMachineId" => {
                let id = std::fs::read_to_string("/etc/machine-id").unwrap_or_default();
                let _ = conn.reply(&call, vec![dbus::s(id.trim())]);
            }
            _ => {
                let _ = conn.reply_error(&call, "org.freedesktop.DBus.Error.UnknownMethod", "unknown method");
            }
        }
        return;
    }
    if iface == "org.freedesktop.DBus.Properties" {
        if member == "GetAll" {
            let _ = conn.reply(&call, vec![Value::Dict("s".into(), "v".into(), vec![])]);
        } else {
            let _ = conn.reply_error(&call, "org.freedesktop.DBus.Error.UnknownProperty", "no properties");
        }
        return;
    }
    if path != crate::OBJECT_PATH {
        let _ = conn.reply_error(&call, "org.freedesktop.DBus.Error.UnknownObject", "no such object");
        return;
    }
    if !iface.is_empty() && iface != crate::INTERFACE {
        let _ = conn.reply_error(&call, "org.freedesktop.DBus.Error.UnknownInterface", "unknown interface");
        return;
    }
    let arg = |i: usize| call.arg_str(i).map(String::from);
    let bad_args = |conn: &Connection| {
        let _ = conn.reply_error(&call, "org.freedesktop.DBus.Error.InvalidArgs", "wrong arguments");
    };
    match member.as_str() {
        "GetVersion" => {
            let _ = conn.reply(&call, vec![dbus::s(crate::VERSION)]);
        }
        "GetStatus" => {
            let s = m.status_json();
            reply_json(&conn, &call, &s);
        }
        "ListDevices" => reply_json(&conn, &call, &m.list_json()),
        "GetDevice" => match arg(0).and_then(|id| m.device_json(&id)) {
            Some(j) => reply_json(&conn, &call, &j),
            None => {
                let _ = conn.reply_error(&call, "io.github.peridot.Error.NotFound", "no such device");
            }
        },
        "SetSetting" => {
            let (Some(id), Some(key), Some(value)) = (arg(0), arg(1), arg(2)) else {
                return bad_args(&conn);
            };
            let value = match Json::parse(&value) {
                Ok(v) => v,
                Err(e) => {
                    let _ = conn.reply_error(&call, "io.github.peridot.Error.InvalidArgs", &format!("value is not JSON: {}", e));
                    return;
                }
            };
            if !m.has_device(&id) {
                let _ = conn.reply_error(&call, "io.github.peridot.Error.NotFound", "no such device");
                return;
            }
            m.apply(&id, &key, value, Some(call), true);
        }
        "Refresh" => {
            let Some(id) = arg(0) else { return bad_args(&conn) };
            if !m.has_device(&id) {
                let _ = conn.reply_error(&call, "io.github.peridot.Error.NotFound", "no such device");
                return;
            }
            m.refresh(&id, Some(call));
        }
        "ForgetDevice" => {
            let Some(id) = arg(0) else { return bad_args(&conn) };
            m.forget(&id);
            let _ = conn.reply(&call, vec![]);
        }
        "GetActions" => {
            let list: Vec<Json> = crate::actions::catalogue()
                .into_iter()
                .map(|(id, label, group)| Json::obj().with("id", id).with("label", label).with("group", group))
                .collect();
            reply_json(&conn, &call, &Json::Arr(list));
        }
        _ => {
            let _ = conn.reply_error(&call, "org.freedesktop.DBus.Error.UnknownMethod", &format!("unknown method {}", member));
        }
    }
}
