//! Button actions: parsing, validation and the Linux input codes they emit.
//!
//! Action strings (stored in config and exchanged over D-Bus):
//!   default | disabled
//!   button:left|right|middle|back|forward
//!   key:ctrl+shift+t            (any combination of modifiers and one key)
//!   media:play_pause|next|previous|stop|volume_up|volume_down|mute
//!   dpi:up|down|cycle|shift      (shift = hold for the first stage)

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Default,
    Disabled,
    /// Linux BTN_* codes
    Button(u16),
    /// Linux KEY_* codes pressed in order, released in reverse
    Keys(Vec<u16>),
    Dpi(DpiOp),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DpiOp {
    Up,
    Down,
    Shift,
}

pub const BTN_LEFT: u16 = 0x110;
pub const BTN_RIGHT: u16 = 0x111;
pub const BTN_MIDDLE: u16 = 0x112;
pub const BTN_SIDE: u16 = 0x113;
pub const BTN_EXTRA: u16 = 0x114;
pub const BTN_FORWARD: u16 = 0x115;
pub const BTN_BACK: u16 = 0x116;
pub const BTN_TASK: u16 = 0x117;

const BUTTONS: &[(&str, u16)] = &[
    ("left", BTN_LEFT),
    ("right", BTN_RIGHT),
    ("middle", BTN_MIDDLE),
    ("back", BTN_SIDE),
    ("forward", BTN_EXTRA),
];

const MEDIA: &[(&str, u16)] = &[
    ("play_pause", 164),
    ("next", 163),
    ("previous", 165),
    ("stop", 166),
    ("volume_up", 115),
    ("volume_down", 114),
    ("mute", 113),
    ("mic_mute", 248),
    ("brightness_up", 225),
    ("brightness_down", 224),
    ("calculator", 140),
    ("browser", 150),
    ("mail", 155),
    ("files", 144),
    ("screenshot", 99),
];

const MODIFIERS: &[(&str, u16)] = &[
    ("ctrl", 29),
    ("control", 29),
    ("shift", 42),
    ("alt", 56),
    ("super", 125),
    ("meta", 125),
    ("win", 125),
    ("rctrl", 97),
    ("rshift", 54),
    ("altgr", 100),
    ("ralt", 100),
];

const KEYS: &[(&str, u16)] = &[
    ("esc", 1), ("escape", 1), ("1", 2), ("2", 3), ("3", 4), ("4", 5), ("5", 6), ("6", 7), ("7", 8), ("8", 9),
    ("9", 10), ("0", 11), ("minus", 12), ("-", 12), ("equal", 13), ("=", 13), ("backspace", 14), ("tab", 15),
    ("q", 16), ("w", 17), ("e", 18), ("r", 19), ("t", 20), ("y", 21), ("u", 22), ("i", 23), ("o", 24), ("p", 25),
    ("leftbrace", 26), ("[", 26), ("rightbrace", 27), ("]", 27), ("enter", 28), ("return", 28), ("a", 30),
    ("s", 31), ("d", 32), ("f", 33), ("g", 34), ("h", 35), ("j", 36), ("k", 37), ("l", 38), ("semicolon", 39),
    (";", 39), ("apostrophe", 40), ("'", 40), ("grave", 41), ("`", 41), ("backslash", 43), ("\\", 43),
    ("z", 44), ("x", 45), ("c", 46), ("v", 47), ("b", 48), ("n", 49), ("m", 50), ("comma", 51), (",", 51),
    ("dot", 52), (".", 52), ("slash", 53), ("/", 53), ("kpasterisk", 55), ("space", 57), ("capslock", 58),
    ("f1", 59), ("f2", 60), ("f3", 61), ("f4", 62), ("f5", 63), ("f6", 64), ("f7", 65), ("f8", 66), ("f9", 67),
    ("f10", 68), ("numlock", 69), ("scrolllock", 70), ("kp7", 71), ("kp8", 72), ("kp9", 73), ("kpminus", 74),
    ("kp4", 75), ("kp5", 76), ("kp6", 77), ("kpplus", 78), ("kp1", 79), ("kp2", 80), ("kp3", 81), ("kp0", 82),
    ("kpdot", 83), ("f11", 87), ("f12", 88), ("kpenter", 96), ("kpslash", 98), ("sysrq", 99), ("print", 99),
    ("home", 102), ("up", 103), ("pageup", 104), ("left", 105), ("right", 106), ("end", 107), ("down", 108),
    ("pagedown", 109), ("insert", 110), ("delete", 111), ("pause", 119), ("menu", 139), ("compose", 127),
    ("f13", 183), ("f14", 184), ("f15", 185), ("f16", 186), ("f17", 187), ("f18", 188), ("f19", 189),
    ("f20", 190), ("f21", 191), ("f22", 192), ("f23", 193), ("f24", 194), ("back", 158), ("forward", 159),
    ("refresh", 173), ("copy", 133), ("paste", 135), ("cut", 137), ("undo", 131), ("redo", 182), ("find", 136),
];

fn lookup(table: &[(&'static str, u16)], name: &str) -> Option<u16> {
    table.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

fn reverse(table: &[(&'static str, u16)], code: u16) -> Option<&'static str> {
    table.iter().find(|(_, c)| *c == code).map(|(n, _)| *n)
}

impl Action {
    pub fn parse(s: &str) -> Result<Action, String> {
        let s = s.trim();
        if s.is_empty() || s == "default" {
            return Ok(Action::Default);
        }
        if s == "disabled" {
            return Ok(Action::Disabled);
        }
        let (kind, arg) = s.split_once(':').ok_or_else(|| format!("invalid action '{}'", s))?;
        let arg = arg.trim().to_ascii_lowercase();
        match kind {
            "button" => lookup(BUTTONS, &arg).map(Action::Button).ok_or_else(|| format!("unknown button '{}'", arg)),
            "media" => lookup(MEDIA, &arg).map(|c| Action::Keys(vec![c])).ok_or_else(|| format!("unknown media key '{}'", arg)),
            "dpi" => match arg.as_str() {
                "up" | "cycle" => Ok(Action::Dpi(DpiOp::Up)),
                "down" => Ok(Action::Dpi(DpiOp::Down)),
                "shift" => Ok(Action::Dpi(DpiOp::Shift)),
                _ => Err(format!("unknown DPI action '{}'", arg)),
            },
            "key" => {
                let mut codes = Vec::new();
                let parts: Vec<&str> = arg.split('+').map(str::trim).filter(|p| !p.is_empty()).collect();
                if parts.is_empty() {
                    return Err("empty key combination".into());
                }
                let mut main_keys = 0;
                for p in parts {
                    if let Some(c) = lookup(MODIFIERS, p) {
                        codes.push(c);
                    } else if let Some(c) = lookup(KEYS, p) {
                        codes.push(c);
                        main_keys += 1;
                    } else {
                        return Err(format!("unknown key '{}'", p));
                    }
                }
                if main_keys > 1 {
                    return Err("a combination can contain only one non-modifier key".into());
                }
                Ok(Action::Keys(codes))
            }
            _ => Err(format!("unknown action type '{}'", kind)),
        }
    }

    /// Canonical string form.
    pub fn to_string_canonical(&self) -> String {
        match self {
            Action::Default => "default".into(),
            Action::Disabled => "disabled".into(),
            Action::Button(c) => format!("button:{}", reverse(BUTTONS, *c).unwrap_or("left")),
            Action::Dpi(DpiOp::Up) => "dpi:up".into(),
            Action::Dpi(DpiOp::Down) => "dpi:down".into(),
            Action::Dpi(DpiOp::Shift) => "dpi:shift".into(),
            Action::Keys(codes) => {
                if codes.len() == 1 {
                    if let Some(m) = reverse(MEDIA, codes[0]) {
                        return format!("media:{}", m);
                    }
                }
                let names: Vec<&str> = codes
                    .iter()
                    .map(|c| reverse(MODIFIERS, *c).or_else(|| reverse(KEYS, *c)).unwrap_or("?"))
                    .collect();
                format!("key:{}", names.join("+"))
            }
        }
    }

    /// Input codes this action can emit (for configuring the virtual device).
    pub fn codes(&self) -> Vec<u16> {
        match self {
            Action::Button(c) => vec![*c],
            Action::Keys(k) => k.clone(),
            _ => vec![],
        }
    }
}

/// Every key/button code the virtual input device should advertise.
pub fn all_emittable_codes() -> Vec<u16> {
    let mut v: Vec<u16> = KEYS.iter().chain(MODIFIERS).chain(MEDIA).chain(BUTTONS).map(|(_, c)| *c).collect();
    v.extend([BTN_FORWARD, BTN_BACK, BTN_TASK]);
    v.sort_unstable();
    v.dedup();
    v
}

/// Catalogue of actions offered in the UI (id, label, group).
pub fn catalogue() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        ("default", "Default", "General"),
        ("disabled", "Disabled", "General"),
        ("button:left", "Left click", "Mouse"),
        ("button:right", "Right click", "Mouse"),
        ("button:middle", "Middle click", "Mouse"),
        ("button:back", "Back", "Mouse"),
        ("button:forward", "Forward", "Mouse"),
        ("dpi:up", "DPI up", "Sensitivity"),
        ("dpi:down", "DPI down", "Sensitivity"),
        ("dpi:shift", "Sniper (hold for lowest DPI)", "Sensitivity"),
        ("media:play_pause", "Play / pause", "Media"),
        ("media:next", "Next track", "Media"),
        ("media:previous", "Previous track", "Media"),
        ("media:volume_up", "Volume up", "Media"),
        ("media:volume_down", "Volume down", "Media"),
        ("media:mute", "Mute", "Media"),
        ("media:mic_mute", "Microphone mute", "Media"),
        ("key:ctrl+c", "Copy", "Editing"),
        ("key:ctrl+v", "Paste", "Editing"),
        ("key:ctrl+z", "Undo", "Editing"),
        ("key:super", "Show activities / launcher", "Desktop"),
        ("key:super+tab", "Switch windows", "Desktop"),
        ("key:ctrl+alt+right", "Next desktop", "Desktop"),
        ("key:ctrl+alt+left", "Previous desktop", "Desktop"),
        ("media:screenshot", "Screenshot", "Desktop"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_format() {
        assert_eq!(Action::parse("default").unwrap(), Action::Default);
        assert_eq!(Action::parse("button:back").unwrap(), Action::Button(BTN_SIDE));
        assert_eq!(Action::parse("key:Ctrl+Shift+T").unwrap(), Action::Keys(vec![29, 42, 20]));
        assert_eq!(Action::parse("media:next").unwrap().to_string_canonical(), "media:next");
        assert_eq!(Action::parse("key:ctrl+shift+t").unwrap().to_string_canonical(), "key:ctrl+shift+t");
        assert_eq!(Action::parse("dpi:cycle").unwrap(), Action::Dpi(DpiOp::Up));
        assert!(Action::parse("key:a+b").is_err());
        assert!(Action::parse("key:").is_err());
        assert!(Action::parse("teleport:home").is_err());
        assert!(Action::parse("key:ctrl+nonsense").is_err());
    }

    #[test]
    fn catalogue_entries_parse() {
        for (id, _, _) in catalogue() {
            let a = Action::parse(id).unwrap();
            assert_eq!(a.to_string_canonical(), Action::parse(&a.to_string_canonical()).unwrap().to_string_canonical());
        }
        assert!(all_emittable_codes().contains(&BTN_LEFT));
    }
}
