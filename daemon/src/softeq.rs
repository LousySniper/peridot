//! Audio processing Peridot does on the computer for a headset, the way G HUB
//! does on Windows: an equalizer for headsets whose firmware won't take EQ
//! changes (the G733, for one), and virtual 7.1 surround.
//!
//! Each headset that uses any of these gets one PipeWire filter-chain hosted
//! by a `pipewire -c` child process, registered as a WirePlumber "smart
//! filter" for the headset's ALSA sink: whatever plays on the headset is
//! routed through it, while the headset stays the output people pick and its
//! own (hardware) volume stays in charge. EQ gains reach the running filter
//! live through `pw-cli`; switching surround rebuilds it.
//!
//! The filter starts the first time something is turned on and then stays
//! until the headset goes away: with everything off it passes sound through
//! untouched, while removing it would yank the sink from under the streams
//! playing into it, which makes many players pause.
//!
//! All PipeWire work happens on one thread so changes apply in order, and the
//! children die with it (PR_SET_PDEATHSIG fires when the forking thread exits).

use crate::json::Json;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Bandwidth of each band: one octave, matching the 10-band graphic layout.
const Q: f64 = 1.41;
/// How often running filters are checked (PipeWire restarts end them).
const WATCH: Duration = Duration::from_secs(5);
/// Head-related transfer functions for virtual surround (MIT KEMAR, shipped
/// with libmysofa, which PipeWire's sofa plugin links against).
const SOFA_FILES: &[&str] = &["/usr/share/libmysofa/default.sofa", "/usr/local/share/libmysofa/default.sofa"];
/// Virtual speaker directions in degrees (0 = front, counter-clockwise).
const SPEAKERS: [(&str, f64); 8] =
    [("FL", 30.0), ("FR", 330.0), ("FC", 0.0), ("LFE", 0.0), ("RL", 150.0), ("RR", 210.0), ("SL", 90.0), ("SR", 270.0)];
/// Level correction for the surround mix, measured with pink noise so that
/// stereo sound comes out about as loud as without surround (and doesn't clip).
const SURROUND_GAIN_DB: f64 = -4.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    /// Device id; one filter per device.
    pub id: String,
    /// Headset name, used for the sink description.
    pub name: String,
    /// USB vendor and product id of the headset's audio interface.
    pub usb: (u16, u16),
    /// Equalizer bands (frequencies, gains) when the EQ runs here.
    pub eq: Option<(Vec<u32>, Vec<i32>)>,
    pub surround: bool,
}

impl Request {
    fn wanted(&self) -> bool {
        self.surround || self.eq.as_ref().is_some_and(|(_, g)| g.iter().any(|x| *x != 0))
    }

    /// Whether `other` can be applied to a filter built for `self` without
    /// rebuilding it.
    fn same_shape(&self, other: &Request) -> bool {
        self.surround == other.surround
            && self.eq.as_ref().map(|e| &e.0) == other.eq.as_ref().map(|e| &e.0)
    }
}

/// What the app shows about a headset's computer-side audio.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    /// The filter is running.
    pub active: bool,
    /// The headset's audio format, e.g. "48 kHz · 16-bit · stereo".
    pub format: Option<String>,
    pub error: Option<String>,
}

pub type Report = Box<dyn Fn(&str, Status) + Send>;

enum Cmd {
    Apply(Request),
    Stop(String),
}

/// Handle to the audio thread, started on first use.
pub struct SoftEq {
    tx: Option<Sender<Cmd>>,
    thread: Option<JoinHandle<()>>,
    report: Option<Report>,
}

impl SoftEq {
    pub fn new(report: Report) -> SoftEq {
        SoftEq { tx: None, thread: None, report: Some(report) }
    }

    /// Bring the headset's filter in line with `req`, and report its status.
    pub fn apply(&mut self, req: Request) {
        self.send(Cmd::Apply(req));
    }

    /// Remove the headset's filter, e.g. because it was unplugged.
    pub fn stop(&mut self, id: &str) {
        if self.tx.is_some() {
            self.send(Cmd::Stop(id.to_string()));
        }
    }

    /// Remove every filter; returns once done.
    pub fn shutdown(&mut self) {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    fn send(&mut self, cmd: Cmd) {
        if self.tx.is_none() {
            let (tx, rx) = mpsc::channel();
            let report = self.report.take().unwrap_or_else(|| Box::new(|_: &str, _: Status| {}));
            self.thread = std::thread::Builder::new().name("softeq".into()).spawn(move || run(rx, report)).ok();
            self.tx = Some(tx);
        }
        if let Some(tx) = &self.tx {
            let _ = tx.send(cmd);
        }
    }
}

struct Running {
    req: Request,
    child: Child,
    node: String,
    conf: PathBuf,
}

/// The headset's ALSA sink in PipeWire.
#[derive(Clone, Debug, PartialEq)]
pub struct Sink {
    pub name: String,
    pub card: Option<i64>,
}

fn run(rx: Receiver<Cmd>, report: Report) {
    let mut running: HashMap<String, Running> = HashMap::new();
    let mut formats: HashMap<String, Option<String>> = HashMap::new();
    loop {
        let first = match rx.recv_timeout(WATCH) {
            Ok(c) => c,
            Err(RecvTimeoutError::Timeout) => {
                watch(&mut running);
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        // Slider drags send a burst of changes; only the last one per headset matters.
        let mut batch = vec![first];
        while let Ok(c) = rx.try_recv() {
            batch.push(c);
        }
        let mut latest: Vec<Cmd> = Vec::new();
        for c in batch {
            let id = match &c {
                Cmd::Apply(r) => r.id.clone(),
                Cmd::Stop(id) => id.clone(),
            };
            latest.retain(|o| match o {
                Cmd::Apply(r) => r.id != id,
                Cmd::Stop(i) => *i != id,
            });
            latest.push(c);
        }
        for c in latest {
            match c {
                Cmd::Apply(req) => {
                    let id = req.id.clone();
                    let error = apply(&mut running, &mut formats, req).err();
                    let status = Status {
                        active: running.contains_key(&id),
                        format: formats.get(&id).cloned().flatten(),
                        error,
                    };
                    report(&id, status);
                }
                Cmd::Stop(id) => {
                    formats.remove(&id);
                    if let Some(r) = running.remove(&id) {
                        stop(r);
                    }
                }
            }
        }
    }
    for (_, r) in running.drain() {
        stop(r);
    }
}

fn apply(
    running: &mut HashMap<String, Running>,
    formats: &mut HashMap<String, Option<String>>,
    req: Request,
) -> Result<(), String> {
    if let Some(r) = running.get_mut(&req.id) {
        let alive = matches!(r.child.try_wait(), Ok(None));
        if alive && r.req.same_shape(&req) {
            let gains = req.eq.as_ref().map(|e| &e.1);
            if gains == r.req.eq.as_ref().map(|e| &e.1) {
                r.req = req;
                return Ok(());
            }
            match set_gains(&r.node, gains.map_or(&[][..], |g| g.as_slice()), req.surround) {
                Ok(()) => {
                    r.req = req;
                    return Ok(());
                }
                Err(e) => crate::debug!("{}: live EQ update failed ({}), restarting the filter", req.id, e),
            }
        }
        if let Some(r) = running.remove(&req.id) {
            stop(r);
        }
    }
    // Look the headset up once, for the format shown in the app.
    if !formats.contains_key(&req.id) {
        let sink = pw_dump().ok().and_then(|d| find_sink(&d, req.usb.0, req.usb.1));
        formats.insert(req.id.clone(), sink.and_then(|s| s.card).and_then(playback_format));
    }
    if !req.wanted() {
        return Ok(());
    }
    let r = start(&req)?;
    crate::info!("{}: audio filter running in PipeWire as '{}'", req.id, r.node);
    running.insert(req.id.clone(), r);
    Ok(())
}

/// Keep each filter transparent in level, and restart filters whose process
/// ended (PipeWire restarted, or it crashed).
fn watch(running: &mut HashMap<String, Running>) {
    let mut dead = Vec::new();
    for (id, r) in running.iter_mut() {
        if matches!(r.child.try_wait(), Ok(None)) {
            unity_volume(&r.node);
        } else {
            dead.push(id.clone());
        }
    }
    for id in dead {
        if let Some(r) = running.remove(&id) {
            let _ = std::fs::remove_file(&r.conf);
            match start(&r.req) {
                Ok(n) => {
                    crate::info!("{}: audio filter restarted", id);
                    running.insert(id, n);
                }
                Err(e) => crate::debug!("{}: audio filter can't restart yet: {}", id, e),
            }
        }
    }
}

fn start(req: &Request) -> Result<Running, String> {
    let dump = pw_dump()?;
    let sink = find_sink(&dump, req.usb.0, req.usb.1)
        .ok_or_else(|| "PipeWire doesn't list this headset as an audio output".to_string())?;
    let sofa = if req.surround {
        Some(
            SOFA_FILES
                .iter()
                .map(Path::new)
                .find(|p| p.exists())
                .ok_or("virtual surround needs libmysofa's HRTF data (install libmysofa)")?,
        )
    } else {
        None
    };
    let node = node_name(&req.id);
    let conf = runtime_dir().join(format!("eq-{}.conf", safe(&req.id)));
    if let Some(d) = conf.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {}", d.display(), e))?;
    }
    let spec = Spec {
        node: &node,
        description: &format!("{} (Peridot)", req.name),
        target: &sink.name,
        eq: req.eq.as_ref().map(|(f, g)| (f.as_slice(), g.as_slice())),
        surround: sofa,
    };
    std::fs::write(&conf, filter_conf(&spec)).map_err(|e| format!("{}: {}", conf.display(), e))?;
    let mut cmd = Command::new("pipewire");
    cmd.arg("-c").arg(&conf).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    unsafe {
        cmd.pre_exec(crate::sys::term_with_parent_thread);
    }
    let mut child = cmd.spawn().map_err(|e| format!("can't start pipewire: {}", e))?;

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if sinks().iter().any(|s| *s == node) {
            break;
        }
        if let Ok(Some(status)) = child.try_wait() {
            let _ = std::fs::remove_file(&conf);
            return Err(format!("the audio filter exited ({})", status));
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&conf);
            return Err("the audio filter didn't show up in PipeWire".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // WirePlumber may restore a volume it remembers for this node name.
    unity_volume(&node);
    Ok(Running { req: req.clone(), child, node, conf })
}

fn stop(mut r: Running) {
    let _ = r.child.kill();
    let _ = r.child.wait();
    let _ = std::fs::remove_file(&r.conf);
    crate::info!("{}: audio filter removed", r.req.id);
}

fn set_gains(node: &str, gains: &[i32], surround: bool) -> Result<(), String> {
    let out = Command::new("pw-cli")
        .args(["set-param", node, "Props", &props(gains, surround)])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("pw-cli: {}", e))?;
    // pw-cli reports errors on stderr but still exits with 0.
    let err = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() || err.contains("rror") {
        return Err(err.trim().to_string());
    }
    Ok(())
}

fn pactl(args: &[&str]) -> Option<String> {
    let out = Command::new("pactl").args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The filter passes sound through at full level; volume is the headset's job.
fn unity_volume(node: &str) {
    pactl(&["set-sink-volume", node, "100%"]);
    pactl(&["set-sink-mute", node, "0"]);
}

fn sinks() -> Vec<String> {
    pactl(&["list", "short", "sinks"])
        .map(|s| s.lines().filter_map(|l| l.split('\t').nth(1).map(String::from)).collect())
        .unwrap_or_default()
}

fn pw_dump() -> Result<Json, String> {
    let out = Command::new("pw-dump")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("pw-dump: {} (is PipeWire installed?)", e))?;
    if !out.status.success() {
        return Err("pw-dump failed (is PipeWire running?)".into());
    }
    Json::parse(&String::from_utf8_lossy(&out.stdout)).map_err(|e| format!("pw-dump output: {}", e))
}

fn runtime_dir() -> PathBuf {
    match std::env::var("XDG_RUNTIME_DIR") {
        Ok(d) if !d.is_empty() => PathBuf::from(d).join("peridot"),
        _ => std::env::temp_dir().join(format!("peridot-{}", crate::sys::uid())),
    }
}

fn safe(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
}

pub fn node_name(id: &str) -> String {
    format!("peridot_eq_{}", safe(id))
}

fn props_of(o: &Json) -> &Json {
    o.get("info").get("props")
}

fn usb_id(v: &Json) -> Option<u16> {
    u16::from_str_radix(v.as_str()?.trim_start_matches("0x"), 16).ok()
}

/// The ALSA output node of the USB audio device with these ids, from `pw-dump`.
pub fn find_sink(dump: &Json, vid: u16, pid: u16) -> Option<Sink> {
    let objects = dump.as_arr()?;
    let devices: Vec<i64> = objects
        .iter()
        .filter(|o| o.get("type").as_str() == Some("PipeWire:Interface:Device"))
        .filter(|o| {
            let p = props_of(o);
            usb_id(p.get("device.vendor.id")) == Some(vid) && usb_id(p.get("device.product.id")) == Some(pid)
        })
        .filter_map(|o| o.get("id").as_i64())
        .collect();
    objects
        .iter()
        .filter(|o| o.get("type").as_str() == Some("PipeWire:Interface:Node"))
        .map(props_of)
        .filter(|p| p.get("media.class").as_str() == Some("Audio/Sink"))
        .filter(|p| p.get("device.id").as_i64().is_some_and(|d| devices.contains(&d)))
        .find_map(|p| {
            let name = p.get("node.name").as_str()?.to_string();
            Some(Sink { name, card: p.get("alsa.card").as_i64() })
        })
}

fn playback_format(card: i64) -> Option<String> {
    let text = std::fs::read_to_string(format!("/proc/asound/card{}/stream0", card)).ok()?;
    describe_format(&text)
}

/// Summarise the playback part of /proc/asound/cardN/stream0, e.g.
/// "48 kHz · 16-bit · stereo".
pub fn describe_format(stream: &str) -> Option<String> {
    let playback = stream.split("Playback:").nth(1)?.split("Capture:").next()?;
    let (mut rates, mut bits, mut channels) = (Vec::new(), 0u32, 0u32);
    for line in playback.lines().map(str::trim) {
        if let Some(v) = line.strip_prefix("Rates:") {
            rates.extend(v.split(',').filter_map(|r| r.trim().parse::<u32>().ok()));
        } else if let Some(v) = line.strip_prefix("Bits:") {
            bits = bits.max(v.trim().parse().unwrap_or(0));
        } else if let Some(v) = line.strip_prefix("Channels:") {
            channels = channels.max(v.trim().parse().unwrap_or(0));
        }
    }
    rates.sort_unstable();
    rates.dedup();
    let khz = |r: u32| {
        let k = r as f64 / 1000.0;
        if k.fract() == 0.0 { format!("{}", k as u32) } else { format!("{:.1}", k) }
    };
    let rate = match (rates.first(), rates.last()) {
        (Some(a), Some(b)) if a == b => format!("{} kHz", khz(*a)),
        (Some(a), Some(b)) => format!("{}–{} kHz", khz(*a), khz(*b)),
        _ => return None,
    };
    let mut parts = vec![rate];
    if bits > 0 {
        parts.push(format!("{}-bit", bits));
    }
    parts.push(match channels {
        1 => "mono".into(),
        2 => "stereo".into(),
        n => format!("{} channels", n),
    });
    Some(parts.join(" · "))
}

/// Linear gain that leaves some headroom for boosted bands. Cutting by the
/// full boost never clips but makes a bass boost sound like everything else
/// got quieter; half is the usual compromise, since a narrow boost rarely
/// raises the peaks by its full amount.
fn preamp(gains: &[i32]) -> f64 {
    let boost = gains.iter().copied().max().unwrap_or(0).max(0);
    10f64.powf(-boost as f64 / 40.0)
}

/// Prefixes of the EQ chains: one shared chain in stereo (PipeWire runs a copy
/// per channel), explicit left and right chains after the surround mix.
fn chains(surround: bool) -> &'static [&'static str] {
    if surround {
        &["l_", "r_"]
    } else {
        &[""]
    }
}

/// SPA Props for `pw-cli set-param`.
pub fn props(gains: &[i32], surround: bool) -> String {
    let mut s = String::from("{ params = [");
    for c in chains(surround) {
        let _ = write!(s, " \"{}preamp:Mult\" {:.6}", c, preamp(gains));
        for (i, g) in gains.iter().enumerate() {
            let _ = write!(s, " \"{}band{}:Gain\" {:.1}", c, i, *g as f64);
        }
    }
    s.push_str(" ] }");
    s
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// What to build into a headset's filter.
pub struct Spec<'a> {
    pub node: &'a str,
    pub description: &'a str,
    /// The headset's ALSA sink.
    pub target: &'a str,
    pub eq: Option<(&'a [u32], &'a [i32])>,
    /// HRTF file to use for virtual 7.1 surround.
    pub surround: Option<&'a Path>,
}

const INDENT: &str = "                    ";

/// Append an EQ chain (preamp, then one peaking filter per band); returns its
/// first and last node names.
fn eq_chain(nodes: &mut String, links: &mut String, prefix: &str, freqs: &[u32], gains: &[i32]) -> (String, String) {
    let _ = writeln!(
        nodes,
        "{INDENT}{{ type = builtin name = {prefix}preamp label = linear control = {{ \"Mult\" = {:.6} }} }}",
        preamp(gains)
    );
    let mut last = format!("{}preamp", prefix);
    for (i, f) in freqs.iter().enumerate() {
        let g = gains.get(i).copied().unwrap_or(0);
        let _ = writeln!(
            nodes,
            "{INDENT}{{ type = builtin name = {prefix}band{i} label = bq_peaking control = {{ \"Freq\" = {:.1} \"Q\" = {} \"Gain\" = {:.1} }} }}",
            *f as f64, Q, g as f64
        );
        let _ = writeln!(links, "{INDENT}{{ output = \"{last}:Out\" input = \"{prefix}band{i}:In\" }}");
        last = format!("{}band{}", prefix, i);
    }
    (format!("{}preamp", prefix), last)
}

/// A standalone PipeWire config hosting the headset's filter-chain.
pub fn filter_conf(spec: &Spec) -> String {
    let mut nodes = String::new();
    let mut links = String::new();
    let mut ports = String::new();
    let channels;
    if let Some(sofa) = spec.surround {
        // One spatializer per speaker, mixed down to left and right.
        channels = "audio.channels = 8\n                audio.position = [ FL FR FC LFE RL RR SL SR ]";
        let gain = 10f64.powf(SURROUND_GAIN_DB / 20.0);
        let mix_gains: String = (1..=SPEAKERS.len()).map(|i| format!("\"Gain {}\" = {:.6} ", i, gain)).collect();
        for (ch, azimuth) in SPEAKERS {
            let _ = writeln!(
                nodes,
                "{INDENT}{{ type = sofa label = spatializer name = sp{ch} config = {{ filename = {} }} control = {{ \"Azimuth\" = {:.1} \"Elevation\" = 0.0 \"Radius\" = 3.0 }} }}",
                quote(&sofa.display().to_string()),
                azimuth
            );
        }
        let mut outputs = Vec::new();
        for (side, prefix) in [("L", "l_"), ("R", "r_")] {
            let _ = writeln!(nodes, "{INDENT}{{ type = builtin label = mixer name = mix{side} control = {{ {mix_gains}}} }}");
            for (i, (ch, _)) in SPEAKERS.iter().enumerate() {
                let _ = writeln!(links, "{INDENT}{{ output = \"sp{ch}:Out {side}\" input = \"mix{side}:In {}\" }}", i + 1);
            }
            match spec.eq {
                Some((freqs, gains)) => {
                    let (first, last) = eq_chain(&mut nodes, &mut links, prefix, freqs, gains);
                    let _ = writeln!(links, "{INDENT}{{ output = \"mix{side}:Out\" input = \"{first}:In\" }}");
                    outputs.push(format!("\"{}:Out\"", last));
                }
                None => outputs.push(format!("\"mix{}:Out\"", side)),
            }
        }
        let inputs: Vec<String> = SPEAKERS.iter().map(|(ch, _)| format!("\"sp{}:In\"", ch)).collect();
        let _ = write!(
            ports,
            "                inputs  = [ {} ]\n                outputs = [ {} ]\n",
            inputs.join(" "),
            outputs.join(" ")
        );
    } else {
        // Mono graph; PipeWire runs a copy per channel.
        channels = "audio.channels = 2\n                audio.position = [ FL FR ]";
        match spec.eq {
            Some((freqs, gains)) => {
                eq_chain(&mut nodes, &mut links, "", freqs, gains);
            }
            None => {
                let _ = writeln!(nodes, "{INDENT}{{ type = builtin name = pass label = copy }}");
            }
        }
    }
    format!(
        r#"# Written by peridotd; removed when the headset goes away.
context.properties = {{ log.level = 0 }}
context.spa-libs = {{
    audio.convert.* = audioconvert/libspa-audioconvert
    support.*       = support/libspa-support
}}
context.modules = [
    {{ name = libpipewire-module-rt flags = [ ifexists nofail ] }}
    {{ name = libpipewire-module-protocol-native }}
    {{ name = libpipewire-module-client-node }}
    {{ name = libpipewire-module-adapter }}
    {{ name = libpipewire-module-filter-chain
        args = {{
            node.description = {desc}
            media.name       = {desc}
            filter.graph = {{
                nodes = [
{nodes}                ]
                links = [
{links}                ]
{ports}            }}
            capture.props = {{
                node.name   = {node}
                media.class = Audio/Sink
                device.icon-name = audio-headset
                {channels}
                filter.smart = true
                filter.smart.name = {node}
                filter.smart.target = {{ node.name = {target} }}
            }}
            playback.props = {{
                node.name          = {output}
                node.passive       = true
                audio.channels     = 2
                audio.position     = [ FL FR ]
                stream.dont-remix  = true
            }}
        }}
    }}
]
"#,
        desc = quote(spec.description),
        node = quote(spec.node),
        output = quote(&format!("{}.output", spec.node)),
        target = quote(spec.target),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = r#"[
      {"id": 48, "type": "PipeWire:Interface:Device", "info": {"props": {
        "device.vendor.id": "0x046d", "device.product.id": "0x0b1f", "media.class": "Audio/Device"}}},
      {"id": 62, "type": "PipeWire:Interface:Node", "info": {"props": {
        "device.id": 48, "media.class": "Audio/Source", "node.name": "alsa_input.usb-Logitech_G733.mono-fallback"}}},
      {"id": 45, "type": "PipeWire:Interface:Node", "info": {"props": {
        "device.id": 48, "alsa.card": 0, "media.class": "Audio/Sink", "node.name": "alsa_output.usb-Logitech_G733.analog-stereo"}}},
      {"id": 50, "type": "PipeWire:Interface:Node", "info": {"props": {
        "device.id": 49, "media.class": "Audio/Sink", "node.name": "alsa_output.pci.analog-stereo"}}},
      {"id": 51, "type": "PipeWire:Interface:Link", "info": null}
    ]"#;

    const G733_STREAM: &str = "Logitech G733 Gaming Headset at usb-0000:00:14.0-1, full speed : USB Audio

Playback:
  Status: Stop
  Interface 1
    Altset 1
    Format: S16_LE
    Channels: 2
    Endpoint: 0x01 (1 OUT) (ADAPTIVE)
    Rates: 48000
    Bits: 16
    Channel map: FL FR

Capture:
  Status: Stop
  Interface 2
    Altset 1
    Format: S16_LE
    Channels: 1
    Rates: 16000, 48000
    Bits: 16
";

    #[test]
    fn finds_the_headset_output() {
        let dump = Json::parse(DUMP).unwrap();
        let sink = find_sink(&dump, 0x046d, 0x0b1f).unwrap();
        assert_eq!(sink.name, "alsa_output.usb-Logitech_G733.analog-stereo");
        assert_eq!(sink.card, Some(0));
        assert_eq!(find_sink(&dump, 0x046d, 0x0aaa), None);
    }

    #[test]
    fn describes_the_playback_format() {
        assert_eq!(describe_format(G733_STREAM).as_deref(), Some("48 kHz · 16-bit · stereo"));
        let multi = "Playback:\n  Rates: 44100, 48000, 96000\n  Bits: 24\n  Channels: 2\n  Rates: 48000\n";
        assert_eq!(describe_format(multi).as_deref(), Some("44.1–96 kHz · 24-bit · stereo"));
        assert_eq!(describe_format("Capture:\n  Rates: 48000\n"), None);
    }

    #[test]
    fn props_lower_the_preamp_by_half_the_largest_boost() {
        assert_eq!(props(&[0, -3], false), "{ params = [ \"preamp:Mult\" 1.000000 \"band0:Gain\" 0.0 \"band1:Gain\" -3.0 ] }");
        assert!(props(&[12, 0], false).starts_with("{ params = [ \"preamp:Mult\" 0.501187 "));
        let s = props(&[3], true);
        assert!(s.contains("\"l_band0:Gain\" 3.0") && s.contains("\"r_band0:Gain\" 3.0") && !s.contains("\" band0"));
    }

    fn spec<'a>(eq: Option<(&'a [u32], &'a [i32])>, surround: Option<&'a Path>) -> Spec<'a> {
        Spec { node: "peridot_eq_x", description: "G733 \"EQ\"", target: "alsa_output.g733", eq, surround }
    }

    #[test]
    fn stereo_config_chains_every_band() {
        let c = filter_conf(&spec(Some((&[32, 64, 125], &[3, 0, -2])), None));
        assert!(c.contains("node.description = \"G733 \\\"EQ\\\"\""));
        assert!(c.contains("name = band2 label = bq_peaking control = { \"Freq\" = 125.0 \"Q\" = 1.41 \"Gain\" = -2.0 }"));
        assert!(c.contains("{ output = \"preamp:Out\" input = \"band0:In\" }"));
        assert!(c.contains("{ output = \"band1:Out\" input = \"band2:In\" }"));
        assert!(!c.contains("input = \"band3:In\""));
        assert!(!c.contains("inputs"));
        assert!(c.contains("filter.smart.target = { node.name = \"alsa_output.g733\" }"));
        assert_eq!(node_name("logitech-ffffffff"), "peridot_eq_logitech_ffffffff");
    }

    #[test]
    fn passthrough_config_without_eq() {
        let c = filter_conf(&spec(None, None));
        assert!(c.contains("name = pass label = copy"));
    }

    #[test]
    fn surround_config_spatialises_eight_speakers_then_equalises() {
        let sofa = Path::new("/usr/share/libmysofa/default.sofa");
        let c = filter_conf(&spec(Some((&[32, 64], &[2, 0])), Some(sofa)));
        assert_eq!(c.matches("label = spatializer").count(), 8);
        assert!(c.contains("name = spSL config = { filename = \"/usr/share/libmysofa/default.sofa\" } control = { \"Azimuth\" = 90.0"));
        assert!(c.contains("{ output = \"spRR:Out L\" input = \"mixL:In 6\" }"));
        assert!(c.contains("\"Gain 8\" = 0.630957"));
        assert!(c.contains("{ output = \"mixR:Out\" input = \"r_preamp:In\" }"));
        assert!(c.contains("{ output = \"l_preamp:Out\" input = \"l_band0:In\" }"));
        assert!(c.contains("inputs  = [ \"spFL:In\" \"spFR:In\" \"spFC:In\" \"spLFE:In\" \"spRL:In\" \"spRR:In\" \"spSL:In\" \"spSR:In\" ]"));
        assert!(c.contains("outputs = [ \"l_band1:Out\" \"r_band1:Out\" ]"));
        assert!(c.contains("audio.position = [ FL FR FC LFE RL RR SL SR ]"));
        let plain = filter_conf(&spec(None, Some(sofa)));
        assert!(plain.contains("outputs = [ \"mixL:Out\" \"mixR:Out\" ]"));
    }

    #[test]
    fn shape_changes_need_a_rebuild() {
        let a = Request {
            id: "x".into(),
            name: "G733".into(),
            usb: (0x046d, 0x0b1f),
            eq: Some((vec![32, 64], vec![0, 0])),
            surround: false,
        };
        assert!(!a.wanted());
        let louder = Request { eq: Some((vec![32, 64], vec![3, 0])), ..a.clone() };
        assert!(louder.wanted() && a.same_shape(&louder));
        assert!(!a.same_shape(&Request { surround: true, ..a.clone() }));
        assert!(Request { surround: true, ..a.clone() }.wanted());
    }
}
