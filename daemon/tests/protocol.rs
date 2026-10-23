//! End-to-end protocol tests: real drivers against byte-level simulators.

use peridot::backend::{self, BackendEvent, Backend, DeviceHandle};
use peridot::hid::Transport;
use peridot::hidpp::receiver::{self, Params};
use peridot::json::Json;
use peridot::model::{Connection, Device};
use peridot::razer;
use peridot::centurion::{self, CenturionDevice};
use peridot::sim::{SimCenturion, SimHidppNode, SimLogi, SimRazer, STD_MATRIX};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn sink() -> (backend::Sink, Receiver<BackendEvent>) {
    let (tx, rx) = channel();
    let tx = Mutex::new(tx);
    (Arc::new(move |e| {
        let _ = tx.lock().unwrap().send(e);
    }), rx)
}

fn wait_ready(rx: &Receiver<BackendEvent>) -> DeviceHandle {
    loop {
        match rx.recv_timeout(Duration::from_secs(10)).expect("timed out waiting for device") {
            BackendEvent::Ready(h) => return h,
            _ => continue,
        }
    }
}

fn j(s: &str) -> Json {
    Json::parse(s).unwrap()
}

#[test]
fn logitech_receiver_end_to_end() {
    let mut offline = SimLogi::office_mouse("MX Master 3S", 0x22334455);
    offline.online = false;
    let node = SimHidppNode::new(
        "receiver",
        true,
        vec![(1, SimLogi::gaming_mouse("PRO X Wireless", 0x0A0B0C0D)), (2, offline)],
    );
    let (sink, rx) = sink();
    let t: Arc<dyn Transport> = node.clone();
    let be = receiver::start(t, Params { key: "r".into(), pid: 0xC539, connection: Connection::Usb, short_ok: true }, sink);

    let dev = wait_ready(&rx);
    {
        let d = backend::lock(&dev);
        let s = d.state();
        assert_eq!(s.name, "PRO X Wireless");
        assert_eq!(s.id, "logitech-0a0b0c0d");
        assert_eq!(s.kind.as_str(), "mouse");
        assert_eq!(s.via.as_deref(), Some("Lightspeed Receiver"));
        assert_eq!(s.model, "046d:4093");
        assert_eq!(s.firmware.as_deref(), Some("RQM 12.03.B002A"));
        assert_eq!(s.battery.as_ref().unwrap().percent, Some(85));
        let dpi = s.dpi.as_ref().unwrap();
        assert_eq!((dpi.value, dpi.min, dpi.max, dpi.step), (800, 100, 25600, 50));
        assert_eq!(s.polling.as_ref().unwrap().options, vec![125, 250, 500, 1000]);
        assert_eq!(s.polling.as_ref().unwrap().value, 1000);
        assert_eq!(s.zones.len(), 2);
        assert_eq!(s.zones[1].id, "logo");
        let effects: Vec<&str> = s.zones[0].effects.iter().map(|e| e.as_str()).collect();
        assert_eq!(effects, vec!["off", "static", "breathing", "spectrum"]);
        let ids: Vec<&str> = s.controls.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["0052", "0053", "0056", "00ed"]);
        assert_eq!(s.onboard_mode.as_deref(), Some("onboard"));
        assert!(!s.notes.is_empty());
    }

    // The sleeping mouse shows up as a placeholder from the pairing registers.
    let sleeper = wait_ready(&rx);
    {
        let d = backend::lock(&sleeper);
        assert_eq!(d.state().name, "MX Master 3S");
        assert_eq!(d.state().id, "logitech-22334455");
        assert!(!d.state().online);
    }
    assert_eq!(
        backend::lock(&sleeper).apply("dpi", &Json::Num(1000.0)),
        Err(peridot::model::Error::Offline)
    );

    // DPI change forces host mode on a mouse running its on-board profile.
    backend::lock(&dev).apply("dpi", &Json::Num(1620.0)).unwrap();
    node.with_device(1, |d| {
        assert_eq!(d.onboard_mode, 2);
        assert_eq!(d.dpi, 1600);
    });
    assert_eq!(backend::lock(&dev).state().onboard_mode.as_deref(), Some("host"));

    backend::lock(&dev).apply("polling_rate", &Json::Num(500.0)).unwrap();
    node.with_device(1, |d| assert_eq!(d.rate_ms, 2));
    assert!(backend::lock(&dev).apply("polling_rate", &Json::Num(333.0)).is_err());

    backend::lock(&dev).apply("dpi_stages", &j(r#"{"stages":[400,1200,2400],"active":2}"#)).unwrap();
    node.with_device(1, |d| assert_eq!(d.dpi, 2400));
    backend::lock(&dev).apply("dpi_cycle", &Json::from("up")).unwrap();
    node.with_device(1, |d| assert_eq!(d.dpi, 400));

    backend::lock(&dev)
        .apply("lighting", &j(r##"{"zone":"logo","effect":"breathing","color":"#ff0000","speed":100}"##))
        .unwrap();
    node.with_device(1, |d| {
        let p = d.zone_effects.get(&1).expect("logo zone written");
        assert_eq!(p[1], 3); // effect index of 0x0A in that zone
        assert_eq!(&p[2..5], &[0xFF, 0x00, 0x00]);
        assert_eq!(p[12], 0x01); // "persist" flag after the 10 parameter bytes
    });
    assert!(backend::lock(&dev).apply("lighting", &j(r#"{"zone":"logo","effect":"wave"}"#)).is_err());

    // Remap the back button: Peridot diverts it and reports presses.
    backend::lock(&dev).apply("button", &j(r#"{"control":"0053","action":"key:ctrl+c"}"#)).unwrap();
    node.with_device(1, |d| assert!(d.diverted.contains(&0x0053)));
    node.press(1, &[0x0053]);
    node.press(1, &[]);
    let mut got = Vec::new();
    while got.len() < 2 {
        if let BackendEvent::Button { control, pressed, device } = rx.recv_timeout(Duration::from_secs(5)).unwrap() {
            assert_eq!(device, "logitech-0a0b0c0d");
            got.push((control, pressed));
        }
    }
    assert_eq!(got, vec![("0053".to_string(), true), ("0053".to_string(), false)]);
    backend::lock(&dev).apply("button", &j(r#"{"control":"0053","action":"default"}"#)).unwrap();
    node.with_device(1, |d| assert!(!d.diverted.contains(&0x0053)));

    // Battery broadcast updates the snapshot.
    node.battery_event(1, 50, true);
    loop {
        if let BackendEvent::Changed(id, snap) = rx.recv_timeout(Duration::from_secs(5)).unwrap() {
            assert_eq!(id, "logitech-0a0b0c0d");
            assert_eq!(snap.get("battery").get("percent").as_i64(), Some(50));
            assert_eq!(snap.get("battery").get("charging").as_bool(), Some(true));
            break;
        }
    }

    // The second device wakes up and gets probed.
    node.set_online(2, true);
    let dev2 = wait_ready(&rx);
    {
        let d = backend::lock(&dev2);
        assert_eq!(d.state().name, "MX Master 3S");
        let dpi = d.state().dpi.as_ref().unwrap();
        assert_eq!((dpi.min, dpi.max, dpi.step), (200, 8000, 50));
        assert_eq!(d.state().polling.as_ref().unwrap().options, vec![125, 250, 500, 1000]);
        assert!(d.state().controls.iter().any(|c| c.name == "Gesture button"));
    }
    backend::lock(&dev2).apply("dpi", &Json::Num(1750.0)).unwrap();
    node.with_device(2, |d| assert_eq!(d.dpi, 1750));

    // Sleep -> offline snapshot.
    node.set_online(2, false);
    loop {
        if let BackendEvent::Changed(id, snap) = rx.recv_timeout(Duration::from_secs(5)).unwrap() {
            if id == "logitech-22334455" {
                assert_eq!(snap.get("online").as_bool(), Some(false));
                break;
            }
        }
    }

    // Unplug: both devices removed, backend closed.
    node.unplug();
    let mut removed = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
            BackendEvent::Removed(id) => removed.push(id),
            BackendEvent::Closed(key) => {
                assert_eq!(key, "r");
                break;
            }
            _ => {}
        }
    }
    removed.sort();
    assert_eq!(removed, vec!["logitech-0a0b0c0d", "logitech-22334455"]);
    be.stop();
}

#[test]
fn logitech_headset_direct() {
    let node = SimHidppNode::new("headset", false, vec![(0xFF, SimLogi::headset("G733 Gaming Headset", 0x77))]);
    let (sink, rx) = sink();
    let t: Arc<dyn Transport> = node.clone();
    let be = receiver::start(t, Params { key: "h".into(), pid: 0x0AB5, connection: Connection::Usb, short_ok: false }, sink);
    let dev = wait_ready(&rx);
    {
        let d = backend::lock(&dev);
        let s = d.state();
        assert_eq!(s.kind.as_str(), "headset");
        assert_eq!(s.sidetone.as_ref().unwrap().value, 35);
        let eq = s.equalizer.as_ref().unwrap();
        assert_eq!(eq.freqs, vec![100, 300, 1000, 3000, 10000]);
        assert_eq!(eq.gains, vec![0, 2, 0, -1, 3]);
        assert_eq!((eq.min, eq.max), (-12, 12));
        assert_eq!(s.auto_off.as_ref().unwrap().value, 30);
        let b = s.battery.as_ref().unwrap();
        assert_eq!(b.voltage_mv, Some(3672));
        assert!(b.estimated);
        assert_eq!(s.zones.len(), 2);
        assert_eq!(s.connection, Connection::Usb);
    }
    backend::lock(&dev).apply("sidetone", &Json::Num(70.0)).unwrap();
    backend::lock(&dev).apply("equalizer", &j("[5,-3,0,0,12]")).unwrap();
    backend::lock(&dev).apply("auto_off", &Json::Num(10.0)).unwrap();
    backend::lock(&dev).apply("lighting", &j(r##"{"zone":"all","effect":"static","color":"#00ff00"}"##)).unwrap();
    node.with_device(0xFF, |d| {
        assert_eq!(d.sidetone, 70);
        assert_eq!(d.eq, vec![5, -3, 0, 0, 12]);
        assert_eq!(d.auto_off, 10);
        assert_eq!(d.zone_effects.len(), 2);
    });
    assert!(backend::lock(&dev).apply("equalizer", &j("[1,2]")).is_err());
    be.stop();
}

#[test]
fn logitech_headset_switched_on_after_startup_gets_all_its_settings() {
    // The G733's dongle answers for the headset's name and features while the
    // headset is off; everything else waits until it's switched on.
    let mut sim = SimLogi::headset("G733 Gaming Headset", 0x79);
    sim.online = false;
    sim.dongle = true;
    let node = SimHidppNode::new("headset", false, vec![(0xFF, sim)]);
    let (sink, rx) = sink();
    let t: Arc<dyn Transport> = node.clone();
    let be = receiver::start(t, Params { key: "h".into(), pid: 0x0B1F, connection: Connection::Usb, short_ok: false }, sink);
    let dev = wait_ready(&rx);
    {
        let d = backend::lock(&dev);
        assert_eq!(d.state().name, "G733 Gaming Headset");
        assert!(!d.state().online);
        assert!(d.state().equalizer.is_none());
    }
    node.with_device(0xFF, |d| d.online = true);
    node.battery_event(0xFF, 80, false);
    let dev = wait_ready(&rx);
    {
        let d = backend::lock(&dev);
        let s = d.state();
        assert!(s.online);
        assert!(s.battery.is_some());
        assert_eq!(s.sidetone.as_ref().unwrap().value, 35);
        assert_eq!(s.equalizer.as_ref().unwrap().gains, vec![0, 2, 0, -1, 3]);
        assert_eq!(s.zones.len(), 2);
        assert!(s.auto_off.is_some());
    }
    be.stop();
}

#[test]
fn logitech_headset_with_locked_equalizer_gets_a_software_one() {
    let mut sim = SimLogi::headset("G733 Gaming Headset", 0x78);
    sim.eq_locked = true;
    let node = SimHidppNode::new("headset", false, vec![(0xFF, sim)]);
    let (sink, rx) = sink();
    let t: Arc<dyn Transport> = node.clone();
    let be = receiver::start(t, Params { key: "h".into(), pid: 0x0B1F, connection: Connection::Usb, short_ok: false }, sink);
    let dev = wait_ready(&rx);
    {
        let d = backend::lock(&dev);
        let eq = d.state().equalizer.clone().unwrap();
        assert!(eq.software);
        // The headset's stored table isn't in use, so the software EQ starts flat.
        assert_eq!(eq.gains, vec![0, 0, 0, 0, 0]);
        assert!(d.state().notes.is_empty());
        let snap = d.state().to_json();
        assert_eq!(snap.get("audio").get("equalizer").get("software").as_bool(), Some(true));
    }
    backend::lock(&dev).apply("equalizer", &j("[5,-3,0,0,20]")).unwrap();
    assert_eq!(backend::lock(&dev).state().equalizer.as_ref().unwrap().gains, vec![5, -3, 0, 0, 12]);
    node.with_device(0xFF, |d| assert_eq!(d.eq, vec![0, 2, 0, -1, 3]));
    be.stop();
}

#[test]
fn logitech_hidpp10_device_gets_its_battery() {
    let mut old = SimLogi::office_mouse("M705", 0x0a0b0c0d);
    old.hidpp10 = true;
    old.battery = 70;
    let node = SimHidppNode::new("receiver", true, vec![(1, old)]);
    let (sink, rx) = sink();
    let t: Arc<dyn Transport> = node.clone();
    let be = receiver::start(t, Params { key: "r".into(), pid: 0xC52B, connection: Connection::Receiver, short_ok: true }, sink);
    let dev = wait_ready(&rx);
    {
        let d = backend::lock(&dev);
        let s = d.state();
        assert_eq!(s.name, "M705");
        assert_eq!(s.kind.as_str(), "mouse");
        assert_eq!(s.protocol.as_deref(), Some("HID++ 1.0"));
        assert_eq!(s.battery.as_ref().unwrap().percent, Some(70));
        assert!(s.dpi.is_none() && s.zones.is_empty());
    }
    assert!(backend::lock(&dev).apply("dpi", &Json::Num(800.0)).is_err());
    be.stop();
}

#[test]
fn logitech_g522_settings() {
    let sim = SimCenturion::new(true);
    let t: Arc<dyn Transport> = sim.clone();
    let mut dev = CenturionDevice::new(t, centurion::find_model(0x0b18).unwrap());
    dev.read_all();
    let s = dev.state().clone();
    assert!(s.online);
    assert_eq!(s.name, "G522 LIGHTSPEED");
    assert_eq!(s.battery.as_ref().unwrap().percent, Some(80));
    assert_eq!(s.sidetone.as_ref().unwrap().value, 44); // 4 of 9
    assert_eq!(s.auto_off.as_ref().unwrap().value, 30);
    assert_eq!(s.equalizer.as_ref().unwrap().freqs.len(), 10);
    assert_eq!(s.modes.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec!["lights", "voice_prompts", "mute_led"]);

    dev.apply("sidetone", &Json::Num(100.0)).unwrap();
    dev.apply("auto_off", &Json::Num(15.0)).unwrap();
    dev.apply("equalizer", &j("[6,0,0,0,0,-3,0,0,0,0]")).unwrap();
    dev.apply("mode", &j(r#"{"id":"lights","value":"off"}"#)).unwrap();
    dev.apply("mode", &j(r#"{"id":"voice_prompts","value":"on"}"#)).unwrap();
    let st = sim.state.lock().unwrap();
    assert_eq!(st.sidetone, 9);
    assert_eq!(st.auto_sleep, [15, 0x01, 0x10]);
    assert_eq!(st.eq, centurion::g522_eq_params(&[6, 0, 0, 0, 0, -3, 0, 0, 0, 0]));
    assert_eq!(st.lights, vec![0; 10]);
    assert_eq!(st.voice_prompts, Some(true));
}

#[test]
fn logitech_g522_switched_on_after_startup() {
    let sim = SimCenturion::new(false);
    let (sink, rx) = sink();
    let t: Arc<dyn Transport> = sim.clone();
    let be = centurion::start(t, centurion::find_model(0x0b18).unwrap(), "c".into(), sink);
    let dev = wait_ready(&rx);
    assert!(!backend::lock(&dev).state().online);
    assert!(backend::lock(&dev).state().sidetone.is_none());
    sim.power_on();
    let dev = wait_ready(&rx);
    {
        let d = backend::lock(&dev);
        assert!(d.state().online);
        assert!(d.state().sidetone.is_some() && d.state().equalizer.is_some());
    }
    be.stop();
}

fn razer_probe(pid: u16) -> (Arc<SimRazer>, razer::device::RazerDevice) {
    let sim = SimRazer::new(pid, "XX0000000000123");
    let model = razer::find_model(pid).unwrap();
    let t: Arc<dyn Transport> = sim.clone();
    let dev = razer::device::RazerDevice::probe(t, model, Connection::Usb).unwrap();
    (sim, dev)
}

#[test]
fn razer_keyboard_with_extended_lighting() {
    let (sim, mut dev) = razer_probe(0x025e); // Cynosa V2
    let s = dev.state().clone();
    assert_eq!(s.name, "Razer Cynosa V2");
    assert_eq!(s.kind.as_str(), "keyboard");
    assert!(s.controls.is_empty() && s.notes.is_empty() && s.dpi.is_none());
    assert_eq!(s.zones.len(), 1);
    assert_eq!(s.zones[0].id, "backlight");
    dev.apply("lighting", &j(r##"{"zone":"all","effect":"static","color":"#ff8000"}"##)).unwrap();
    dev.apply("brightness", &j(r#"{"zone":"all","value":50}"#)).unwrap();
    let st = sim.state.lock().unwrap();
    let fx = &st.effects[&0x05];
    assert_eq!((fx[1], fx[2]), (0x05, 0x01)); // backlight LED, static
    assert_eq!(&fx[6..9], &[0xff, 0x80, 0x00]);
    assert_eq!(st.brightness[&0x05], 128);
}

#[test]
fn razer_keyboard_with_standard_matrix_lighting() {
    let (sim, mut dev) = razer_probe(0x0203); // BlackWidow Chroma
    assert_eq!(dev.state().kind.as_str(), "keyboard");
    dev.apply("lighting", &j(r##"{"zone":"backlight","effect":"static","color":"#00ff00"}"##)).unwrap();
    assert_eq!(&sim.state.lock().unwrap().effects[&STD_MATRIX][..4], &[0x06, 0x00, 0xff, 0x00]);
    dev.apply("lighting", &j(r##"{"zone":"backlight","effect":"wave","direction":"left"}"##)).unwrap();
    assert_eq!(&sim.state.lock().unwrap().effects[&STD_MATRIX][..2], &[0x01, 0x02]);
    dev.apply("lighting", &j(r##"{"zone":"backlight","effect":"breathing","color":"#0000ff","color2":"#ff0000"}"##)).unwrap();
    assert_eq!(&sim.state.lock().unwrap().effects[&STD_MATRIX][..8], &[0x03, 0x02, 0, 0, 0xff, 0xff, 0, 0]);
    dev.apply("brightness", &j(r#"{"zone":"all","value":100}"#)).unwrap();
    assert_eq!(sim.state.lock().unwrap().brightness[&0x05], 255);
}

#[test]
fn razer_mouse_mat_lighting() {
    let (sim, mut dev) = razer_probe(0x0c02); // Goliathus Extended
    let s = dev.state().clone();
    assert_eq!(s.kind.as_str(), "other");
    assert!(s.controls.is_empty());
    assert_eq!(s.zones[0].id, "all");
    dev.apply("lighting", &j(r##"{"zone":"all","effect":"spectrum"}"##)).unwrap();
    assert_eq!(sim.state.lock().unwrap().effects[&0x00][2], 0x03);
}

#[test]
fn razer_mouse_without_lighting() {
    let sim = SimRazer::new(0x00b7, "PM2310H12345678");
    let model = razer::find_model(0x00b7).unwrap();
    let t: Arc<dyn Transport> = sim.clone();
    let mut dev = razer::device::RazerDevice::probe(t, model, Connection::Receiver).unwrap();
    let s = dev.state().clone();
    assert_eq!(s.name, "Razer DeathAdder V3 Pro");
    assert_eq!(s.id, "razer-pm2310h12345678");
    assert_eq!(s.firmware.as_deref(), Some("v1.4"));
    assert_eq!(s.battery.as_ref().unwrap().percent, Some(64));
    let dpi = s.dpi.as_ref().unwrap();
    assert_eq!(dpi.stages, vec![400, 800, 1600, 3200]);
    assert_eq!(dpi.active_stage, 2);
    assert!(dpi.hardware_stages);
    assert_eq!(dpi.max, 35000);
    assert_eq!(s.idle_time.as_ref().unwrap().value, 300);
    assert_eq!(s.low_battery.as_ref().unwrap().value, 15);
    assert!(s.zones.is_empty());

    dev.apply("dpi_stages", &j(r#"{"stages":[800,1600,26000],"active":1}"#)).unwrap();
    dev.apply("polling_rate", &Json::Num(500.0)).unwrap();
    dev.apply("idle_time", &Json::Num(600.0)).unwrap();
    dev.apply("low_battery", &Json::Num(10.0)).unwrap();
    {
        let st = sim.state.lock().unwrap();
        assert_eq!(st.stages, vec![800, 1600, 26000]);
        assert_eq!(st.active, 2);
        assert_eq!(st.dpi, (1600, 1600));
        assert_eq!(st.poll_code, 0x02);
        assert_eq!(st.idle, 600);
        assert_eq!(st.low_raw, 26);
    }
    dev.apply("dpi_shift", &Json::Bool(true)).unwrap();
    assert_eq!(sim.state.lock().unwrap().dpi.0, 800);
    dev.apply("dpi_shift", &Json::Bool(false)).unwrap();
    assert_eq!(sim.state.lock().unwrap().dpi.0, 1600);
    assert!(dev.apply("lighting", &j(r#"{"effect":"static"}"#)).is_err());

    // Asleep -> offline, then back.
    sim.state.lock().unwrap().asleep = true;
    assert!(dev.refresh().is_err());
    assert!(!dev.state().online);
    sim.state.lock().unwrap().asleep = false;
    dev.refresh().unwrap();
    assert!(dev.state().online);
}

#[test]
fn razer_mouse_with_lighting() {
    let sim = SimRazer::new(0x0084, "");
    let model = razer::find_model(0x0084).unwrap();
    let t: Arc<dyn Transport> = sim.clone();
    let mut dev = razer::device::RazerDevice::probe(t, model, Connection::Usb).unwrap();
    let s = dev.state().clone();
    assert_eq!(s.id, "razer-0084");
    assert_eq!(s.name, "Razer DeathAdder V2");
    assert!(s.battery.is_none());
    let zones: Vec<&str> = s.zones.iter().map(|z| z.id.as_str()).collect();
    assert_eq!(zones, vec!["logo", "scroll"]);
    assert_eq!(s.zones[0].brightness, Some(78));
    dev.apply("lighting", &j(r##"{"zone":"all","effect":"static","color":"#112233"}"##)).unwrap();
    dev.apply("lighting", &j(r##"{"zone":"logo","effect":"breathing","color":"#ff0000","color2":"#0000ff"}"##)).unwrap();
    dev.apply("brightness", &j(r#"{"zone":"scroll","value":50}"#)).unwrap();
    {
        let st = sim.state.lock().unwrap();
        assert_eq!(&st.effects[&0x01][..9], &[1, 0x01, 0x01, 0, 0, 1, 0x11, 0x22, 0x33]);
        assert_eq!(&st.effects[&0x04][..12], &[1, 0x04, 0x02, 2, 0, 2, 0xFF, 0, 0, 0, 0, 0xFF]);
        assert_eq!(st.brightness[&0x01], 128);
    }
    assert_eq!(dev.state().zones[1].brightness, Some(50));
    dev.apply("button", &j(r#"{"control":"0113","action":"media:next"}"#)).unwrap();
    assert_eq!(dev.state().controls.iter().find(|c| c.id == "0113").unwrap().action, "media:next");
    assert!(dev.apply("button", &j(r#"{"control":"0110","action":"media:next"}"#)).is_err());
}

#[test]
fn razer_backend_lifecycle() {
    let sim = SimRazer::new(0x0099, "");
    let model = razer::find_model(0x0099).unwrap();
    let (sink, rx) = sink();
    let t: Arc<dyn Transport> = sim.clone();
    let be = razer::device::start(t, model, "z".into(), Connection::Usb, sink);
    let h = wait_ready(&rx);
    assert_eq!(backend::lock(&h).state().name, "Razer Basilisk V3");
    be.stop();
    let mut closed = false;
    while let Ok(e) = rx.recv_timeout(Duration::from_secs(5)) {
        if let BackendEvent::Closed(k) = e {
            assert_eq!(k, "z");
            closed = true;
            break;
        }
    }
    assert!(closed);
}
