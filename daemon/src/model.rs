//! Vendor-neutral device model. Every driver fills a [`DeviceState`]; the D-Bus
//! API and the UI only ever see its JSON form, so both vendors look the same.

use crate::json::Json;

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    NotSupported(String),
    Invalid(String),
    Device(String),
    Offline,
    NotFound(String),
}

impl Error {
    pub fn dbus_name(&self) -> &'static str {
        match self {
            Error::NotSupported(_) => "io.github.peridot.Error.NotSupported",
            Error::Invalid(_) => "io.github.peridot.Error.InvalidArgs",
            Error::Device(_) => "io.github.peridot.Error.Device",
            Error::Offline => "io.github.peridot.Error.Offline",
            Error::NotFound(_) => "io.github.peridot.Error.NotFound",
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotSupported(s) => write!(f, "not supported: {}", s),
            Error::Invalid(s) => write!(f, "invalid value: {}", s),
            Error::Device(s) => write!(f, "device error: {}", s),
            Error::Offline => write!(f, "the device is offline (asleep, switched off or out of range)"),
            Error::NotFound(s) => write!(f, "no such device: {}", s),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Device(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn invalid<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Invalid(msg.into()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Kind {
    #[default]
    Other,
    Mouse,
    Headset,
    Keyboard,
    Controller,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Mouse => "mouse",
            Kind::Headset => "headset",
            Kind::Keyboard => "keyboard",
            Kind::Controller => "controller",
            Kind::Other => "other",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Connection {
    #[default]
    Usb,
    Receiver,
    Bluetooth,
    Simulated,
}

impl Connection {
    pub fn as_str(self) -> &'static str {
        match self {
            Connection::Usb => "usb",
            Connection::Receiver => "receiver",
            Connection::Bluetooth => "bluetooth",
            Connection::Simulated => "simulated",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Charge {
    Discharging,
    Charging,
    Full,
    Error,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Battery {
    pub percent: Option<u8>,
    pub charge: Charge,
    pub voltage_mv: Option<u16>,
    /// The percentage was derived from a voltage or a coarse level.
    pub estimated: bool,
}

impl Battery {
    pub fn level(&self) -> &'static str {
        match self.percent {
            Some(p) if p >= 80 => "full",
            Some(p) if p >= 30 => "good",
            Some(p) if p >= 10 => "low",
            Some(_) => "critical",
            None => "unknown",
        }
    }

    pub fn to_json(&self) -> Json {
        Json::obj()
            .with("percent", self.percent)
            .with("level", self.level())
            .with("charging", matches!(self.charge, Charge::Charging))
            .with("full", matches!(self.charge, Charge::Full))
            .with("charge_error", matches!(self.charge, Charge::Error))
            .with("voltage", self.voltage_mv)
            .with("estimated", self.estimated)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dpi {
    pub value: u16,
    pub min: u16,
    pub max: u16,
    /// Granularity for range-based sensors (0 when `list` is used).
    pub step: u16,
    /// Discrete supported values, for sensors that only accept a list.
    pub list: Vec<u16>,
    pub stages: Vec<u16>,
    pub active_stage: usize,
    pub max_stages: usize,
    /// Stages live in the mouse (Razer) rather than being cycled by Peridot.
    pub hardware_stages: bool,
}

impl Dpi {
    /// Snap an arbitrary value to something the sensor accepts.
    pub fn snap(&self, v: u16) -> u16 {
        if !self.list.is_empty() {
            return *self.list.iter().min_by_key(|x| (**x as i32 - v as i32).abs()).unwrap();
        }
        let v = v.clamp(self.min.max(1), self.max.max(self.min.max(1)));
        if self.step > 1 {
            let off = (v - self.min) as u32;
            let snapped = self.min as u32 + ((off + self.step as u32 / 2) / self.step as u32) * self.step as u32;
            return snapped.min(self.max as u32) as u16;
        }
        v
    }

    pub fn to_json(&self) -> Json {
        Json::obj()
            .with("value", self.value)
            .with("min", self.min)
            .with("max", self.max)
            .with("step", self.step)
            .with("list", self.list.clone())
            .with("stages", self.stages.clone())
            .with("active_stage", self.active_stage)
            .with("max_stages", self.max_stages)
            .with("hardware_stages", self.hardware_stages)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Polling {
    pub value: u16,
    pub options: Vec<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Off,
    Static,
    Breathing,
    Spectrum,
    Wave,
    Reactive,
    Ripple,
}

impl Effect {
    pub const ALL: [Effect; 7] =
        [Effect::Off, Effect::Static, Effect::Breathing, Effect::Spectrum, Effect::Wave, Effect::Reactive, Effect::Ripple];

    pub fn as_str(self) -> &'static str {
        match self {
            Effect::Off => "off",
            Effect::Static => "static",
            Effect::Breathing => "breathing",
            Effect::Spectrum => "spectrum",
            Effect::Wave => "wave",
            Effect::Reactive => "reactive",
            Effect::Ripple => "ripple",
        }
    }

    pub fn parse(s: &str) -> Option<Effect> {
        Effect::ALL.iter().copied().find(|e| e.as_str() == s)
    }

    pub fn uses_color(self) -> bool {
        matches!(self, Effect::Static | Effect::Breathing | Effect::Reactive | Effect::Ripple)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn parse(s: &str) -> Option<Rgb> {
        let h = s.trim().trim_start_matches('#');
        if h.len() != 6 {
            return None;
        }
        let v = u32::from_str_radix(h, 16).ok()?;
        Some(Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
    }

    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Direction {
    #[default]
    Right,
    Left,
}

/// One requested/remembered lighting configuration for a zone.
#[derive(Clone, Debug, PartialEq)]
pub struct Lighting {
    pub effect: Effect,
    pub color: Rgb,
    pub color2: Option<Rgb>,
    /// 0 (slow) ..= 100 (fast)
    pub speed: u8,
    pub direction: Direction,
}

impl Default for Lighting {
    fn default() -> Self {
        Lighting { effect: Effect::Static, color: Rgb(0x9b, 0xd1, 0x3e), color2: None, speed: 50, direction: Direction::Right }
    }
}

impl Lighting {
    pub fn from_json(v: &Json) -> Result<Lighting> {
        let effect = match v.get("effect").as_str() {
            Some(s) => Effect::parse(s).ok_or_else(|| Error::Invalid(format!("unknown effect '{}'", s)))?,
            None => return invalid("missing 'effect'"),
        };
        let mut l = Lighting { effect, ..Default::default() };
        if let Some(c) = v.get("color").as_str() {
            l.color = Rgb::parse(c).ok_or_else(|| Error::Invalid(format!("bad color '{}'", c)))?;
        }
        if let Some(c) = v.get("color2").as_str() {
            l.color2 = Some(Rgb::parse(c).ok_or_else(|| Error::Invalid(format!("bad color '{}'", c)))?);
        }
        if let Some(s) = v.get("speed").as_i64() {
            l.speed = s.clamp(0, 100) as u8;
        }
        if v.get("direction").as_str() == Some("left") {
            l.direction = Direction::Left;
        }
        Ok(l)
    }

    pub fn to_json(&self) -> Json {
        Json::obj()
            .with("effect", self.effect.as_str())
            .with("color", self.color.to_hex())
            .with("color2", self.color2.map(|c| c.to_hex()))
            .with("speed", self.speed)
            .with("direction", if self.direction == Direction::Left { "left" } else { "right" })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Zone {
    pub id: String,
    pub name: String,
    pub effects: Vec<Effect>,
    pub state: Option<Lighting>,
    pub brightness: Option<u8>,
    pub dual_color: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Control {
    pub id: String,
    pub name: String,
    pub action: String,
    /// Whether remapping this control is allowed (e.g. not the primary click).
    pub remappable: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Range {
    pub value: i32,
    pub min: i32,
    pub max: i32,
    pub step: i32,
}

impl Range {
    pub fn to_json(&self) -> Json {
        Json::obj().with("value", self.value).with("min", self.min).with("max", self.max).with("step", self.step)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Equalizer {
    pub freqs: Vec<u32>,
    pub gains: Vec<i32>,
    pub min: i32,
    pub max: i32,
    /// Applied by Peridot through PipeWire because the headset's firmware
    /// doesn't take EQ changes from the computer.
    pub software: bool,
    /// Presets tuned for this headset: (id, name, gains). Empty means the app
    /// offers its generic ones.
    pub presets: Vec<(String, String, Vec<i32>)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub value: i32,
    pub options: Vec<i32>,
}

/// A small set of named choices, e.g. noise cancelling off / on / ambient.
#[derive(Clone, Debug, PartialEq)]
pub struct Mode {
    pub id: String,
    pub name: String,
    pub hint: String,
    pub value: String,
    /// (value, label)
    pub options: Vec<(String, String)>,
}

impl Mode {
    pub fn to_json(&self) -> Json {
        let options: Vec<Json> =
            self.options.iter().map(|(v, l)| Json::obj().with("value", v.as_str()).with("label", l.as_str())).collect();
        Json::obj()
            .with("id", self.id.as_str())
            .with("name", self.name.as_str())
            .with("hint", self.hint.as_str())
            .with("value", self.value.as_str())
            .with("options", options)
    }
}

/// Everything the UI needs to render one device.
#[derive(Clone, Debug, Default)]
pub struct DeviceState {
    /// Stable identifier used for saved settings and the D-Bus API.
    pub id: String,
    pub name: String,
    pub vendor: &'static str,
    pub kind: Kind,
    pub connection: Connection,
    pub online: bool,
    /// e.g. "046d:c539" or "1532:00b7"
    pub model: String,
    pub via: Option<String>,
    pub serial: Option<String>,
    pub firmware: Option<String>,
    pub protocol: Option<String>,
    pub battery: Option<Battery>,
    pub dpi: Option<Dpi>,
    pub polling: Option<Polling>,
    pub zones: Vec<Zone>,
    pub controls: Vec<Control>,
    pub sidetone: Option<Range>,
    pub equalizer: Option<Equalizer>,
    /// Minutes until power-off (0 = never).
    pub auto_off: Option<Choice>,
    /// Headset modes such as noise cancelling. Set with key "mode".
    pub modes: Vec<Mode>,
    /// Seconds of inactivity before the device sleeps.
    pub idle_time: Option<Range>,
    /// Percent at which the device signals low battery.
    pub low_battery: Option<Range>,
    pub onboard_mode: Option<String>,
    /// Controllers: the stick-drift fix (see `gamepad`).
    pub sticks: Option<Json>,
    pub notes: Vec<String>,
}

impl DeviceState {
    pub fn to_json(&self) -> Json {
        let mut j = Json::obj()
            .with("id", self.id.as_str())
            .with("name", self.name.as_str())
            .with("vendor", self.vendor)
            .with("kind", self.kind.as_str())
            .with("connection", self.connection.as_str())
            .with("online", self.online)
            .with("model", self.model.as_str())
            .with("via", self.via.clone())
            .with("serial", self.serial.clone())
            .with("firmware", self.firmware.clone())
            .with("protocol", self.protocol.clone())
            .with("battery", self.battery.as_ref().map(Battery::to_json));
        j.set("dpi", self.dpi.as_ref().map(Dpi::to_json));
        j.set(
            "polling_rate",
            self.polling.as_ref().map(|p| Json::obj().with("value", p.value).with("options", p.options.clone())),
        );
        if self.zones.is_empty() {
            j.set("lighting", Json::Null);
        } else {
            let zones: Vec<Json> = self
                .zones
                .iter()
                .map(|z| {
                    Json::obj()
                        .with("id", z.id.as_str())
                        .with("name", z.name.as_str())
                        .with("effects", z.effects.iter().map(|e| e.as_str()).collect::<Vec<_>>())
                        .with("current", z.state.as_ref().map(Lighting::to_json))
                        .with("brightness", z.brightness)
                        .with("dual_color", z.dual_color)
                })
                .collect();
            j.set("lighting", Json::obj().with("zones", zones));
        }
        if self.controls.is_empty() {
            j.set("buttons", Json::Null);
        } else {
            let controls: Vec<Json> = self
                .controls
                .iter()
                .map(|c| {
                    Json::obj()
                        .with("id", c.id.as_str())
                        .with("name", c.name.as_str())
                        .with("action", c.action.as_str())
                        .with("remappable", c.remappable)
                })
                .collect();
            j.set("buttons", Json::obj().with("controls", controls));
        }
        let mut audio = Json::obj();
        if let Some(s) = &self.sidetone {
            audio.set("sidetone", s.to_json());
        }
        if let Some(eq) = &self.equalizer {
            let bands: Vec<Json> = eq
                .freqs
                .iter()
                .zip(eq.gains.iter())
                .map(|(f, g)| Json::obj().with("freq", *f).with("gain", *g))
                .collect();
            audio.set(
                "equalizer",
                Json::obj()
                    .with("bands", bands)
                    .with("min", eq.min)
                    .with("max", eq.max)
                    .with("software", eq.software)
                    .with(
                        "presets",
                        eq.presets
                            .iter()
                            .map(|(id, name, gains)| {
                                Json::obj().with("id", id.as_str()).with("name", name.as_str()).with("gains", gains.clone())
                            })
                            .collect::<Vec<_>>(),
                    ),
            );
        }
        if let Some(a) = &self.auto_off {
            audio.set("auto_off", Json::obj().with("value", a.value).with("options", a.options.clone()));
        }
        if !self.modes.is_empty() {
            audio.set("modes", self.modes.iter().map(Mode::to_json).collect::<Vec<_>>());
        }
        j.set("audio", if audio.as_obj().map_or(true, |o| o.is_empty()) { Json::Null } else { audio });
        let mut power = Json::obj();
        if let Some(r) = &self.idle_time {
            power.set("idle_time", r.to_json());
        }
        if let Some(r) = &self.low_battery {
            power.set("low_battery", r.to_json());
        }
        j.set("power", if power.as_obj().map_or(true, |o| o.is_empty()) { Json::Null } else { power });
        j.set("onboard_mode", self.onboard_mode.clone());
        j.set("sticks", self.sticks.clone());
        j.set("notes", self.notes.clone());
        j
    }
}

/// A driver for one logical device.
pub trait Device: Send {
    fn state(&self) -> &DeviceState;
    /// Re-read volatile values (battery, current DPI...).
    fn refresh(&mut self) -> Result<()>;
    /// Change one setting. See the D-Bus API docs for keys and values.
    fn apply(&mut self, key: &str, value: &Json) -> Result<()>;
}

/// Software-managed DPI stage cycling shared by drivers that lack hardware
/// stages. Returns the new active index.
pub fn cycle_stage(dpi: &Dpi, op: &str) -> Option<usize> {
    if dpi.stages.is_empty() {
        return None;
    }
    let n = dpi.stages.len();
    let cur = dpi.active_stage.min(n - 1);
    Some(match op {
        "up" => (cur + 1) % n,
        "down" => (cur + n - 1) % n,
        "first" => 0,
        _ => return None,
    })
}

/// Validate and normalise a stage list from JSON.
pub fn parse_stages(v: &Json, dpi: &Dpi) -> Result<(Vec<u16>, usize)> {
    let arr = v.get("stages").as_arr().ok_or_else(|| Error::Invalid("missing 'stages' array".into()))?;
    if arr.is_empty() {
        return invalid("at least one DPI stage is required");
    }
    let max = if dpi.max_stages > 0 { dpi.max_stages } else { 8 };
    if arr.len() > max {
        return invalid(format!("at most {} DPI stages are supported", max));
    }
    let mut stages = Vec::new();
    for s in arr {
        let n = s.as_i64().ok_or_else(|| Error::Invalid("stage values must be numbers".into()))?;
        if n <= 0 || n > u16::MAX as i64 {
            return invalid(format!("DPI {} out of range", n));
        }
        stages.push(dpi.snap(n as u16));
    }
    let active = v.get("active").as_i64().unwrap_or(0).clamp(0, stages.len() as i64 - 1) as usize;
    Ok((stages, active))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpi_snap() {
        let d = Dpi { min: 100, max: 25600, step: 50, ..Default::default() };
        assert_eq!(d.snap(1620), 1600);
        assert_eq!(d.snap(1630), 1650);
        assert_eq!(d.snap(10), 100);
        assert_eq!(d.snap(60000), 25600);
        let l = Dpi { list: vec![400, 800, 1600, 3200], ..Default::default() };
        assert_eq!(l.snap(1000), 800);
    }

    #[test]
    fn lighting_json() {
        let j = Json::parse(r##"{"effect":"breathing","color":"#ff0080","speed":200}"##).unwrap();
        let l = Lighting::from_json(&j).unwrap();
        assert_eq!(l.effect, Effect::Breathing);
        assert_eq!(l.color, Rgb(255, 0, 128));
        assert_eq!(l.speed, 100);
        assert!(Lighting::from_json(&Json::parse(r#"{"effect":"disco"}"#).unwrap()).is_err());
    }

    #[test]
    fn stage_parsing_and_cycling() {
        let d = Dpi { min: 100, max: 3200, step: 100, max_stages: 4, stages: vec![400, 800], active_stage: 1, ..Default::default() };
        let (s, a) = parse_stages(&Json::parse(r#"{"stages":[450,1600,9000],"active":5}"#).unwrap(), &d).unwrap();
        assert_eq!(s, vec![500, 1600, 3200]);
        assert_eq!(a, 2);
        assert!(parse_stages(&Json::parse(r#"{"stages":[1,2,3,4,5]}"#).unwrap(), &d).is_err());
        assert_eq!(cycle_stage(&d, "up"), Some(0));
        assert_eq!(cycle_stage(&d, "down"), Some(0));
    }
}
