//! Saved per-device settings.
//!
//! Each device gets `~/.config/peridot/devices/<id>.json`:
//! `{"name": "...", "settings": {"<storage key>": <value>, ...}}`.
//! Storage keys are setting keys, qualified by zone or control where a setting
//! targets one (`lighting:logo`, `button:0053`).

use crate::json::Json;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct Store {
    dir: PathBuf,
}

/// Order in which saved settings are re-applied to a reconnecting device.
const APPLY_ORDER: &[&str] = &[
    "onboard_mode",
    "polling_rate",
    "dpi_stages",
    "dpi",
    "lighting:all",
    "lighting",
    "brightness:all",
    "brightness",
    "button",
    "sidetone",
    "equalizer",
    "auto_off",
    "idle_time",
    "low_battery",
    "sticks",
];

pub fn default_dir() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        if !x.is_empty() {
            return Path::new(&x).join("peridot");
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    Path::new(&home).join(".config/peridot")
}

/// The storage key for a setting, or None if it shouldn't be persisted.
pub fn storage_key(key: &str, value: &Json) -> Option<String> {
    match key {
        "lighting" | "brightness" => Some(format!("{}:{}", key, value.get("zone").as_str().unwrap_or("all"))),
        "button" => value.get("control").as_str().map(|c| format!("button:{}", c.trim_start_matches("0x"))),
        "dpi" | "dpi_cycle" | "dpi_stages" => Some("dpi_stages".into()),
        "calibrate_sticks" => Some("sticks".into()),
        "polling_rate" | "sidetone" | "equalizer" | "auto_off" | "idle_time" | "low_battery" | "onboard_mode"
        | "surround" | "sticks" => {
            Some(key.into())
        }
        _ => None,
    }
}

fn safe_name(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

impl Store {
    pub fn new(dir: PathBuf) -> Store {
        Store { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, id: &str) -> PathBuf {
        self.dir.join("devices").join(format!("{}.json", safe_name(id)))
    }

    pub fn load(&self, id: &str) -> Json {
        let text = match fs::read_to_string(self.path(id)) {
            Ok(t) => t,
            Err(_) => return Json::obj().with("settings", Json::obj()),
        };
        match Json::parse(&text) {
            Ok(j) if j.get("settings").as_obj().is_some() => j,
            Ok(_) | Err(_) => {
                crate::warn!("ignoring unreadable settings file for {}", id);
                Json::obj().with("settings", Json::obj())
            }
        }
    }

    pub fn settings(&self, id: &str) -> Vec<(String, Json)> {
        self.load(id).get("settings").as_obj().cloned().unwrap_or_default()
    }

    fn write(&self, id: &str, doc: &Json) -> std::io::Result<()> {
        let path = self.path(id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(doc.to_string_pretty().as_bytes())?;
            f.sync_all()?;
        }
        fs::rename(tmp, path)
    }

    /// Remember a setting. A setting for zone "all" replaces per-zone ones.
    pub fn remember(&self, id: &str, name: &str, skey: &str, value: Json) {
        let mut doc = self.load(id);
        doc.set("name", name);
        let mut settings = doc.remove("settings").unwrap_or_else(Json::obj);
        if let Some((family, "all")) = skey.split_once(':') {
            if let Json::Obj(items) = &mut settings {
                items.retain(|(k, _)| !k.starts_with(&format!("{}:", family)));
            }
        }
        if skey == "dpi_stages" {
            settings.remove("dpi");
        }
        settings.set(skey, value);
        doc.set("settings", settings);
        if let Err(e) = self.write(id, &doc) {
            crate::warn!("could not save settings for {}: {}", id, e);
        }
    }

    pub fn forget(&self, id: &str) {
        let _ = fs::remove_file(self.path(id));
    }

    /// Saved settings in a safe re-apply order, as (setting key, value).
    pub fn replay_plan(&self, id: &str) -> Vec<(String, Json)> {
        let saved = self.settings(id);
        let mut plan = Vec::new();
        for family in APPLY_ORDER {
            for (k, v) in &saved {
                let matches = if family.contains(':') {
                    k == family
                } else {
                    k == family || (k.starts_with(&format!("{}:", family)) && !k.ends_with(":all"))
                };
                if matches {
                    let key = k.split(':').next().unwrap_or(k).to_string();
                    plan.push((key, v.clone()));
                }
            }
        }
        plan
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remember_and_replay() {
        let dir = std::env::temp_dir().join(format!("peridot-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let store = Store::new(dir.clone());
        let id = "logitech-abcd";
        let logo = Json::parse(r##"{"zone":"logo","effect":"static","color":"#ff0000"}"##).unwrap();
        store.remember(id, "Mouse", &storage_key("lighting", &logo).unwrap(), logo.clone());
        store.remember(id, "Mouse", "button:0053", Json::parse(r#"{"control":"0053","action":"media:next"}"#).unwrap());
        store.remember(id, "Mouse", "polling_rate", Json::Num(500.0));
        store.remember(id, "Mouse", "onboard_mode", Json::from("host"));
        let plan = store.replay_plan(id);
        let keys: Vec<&str> = plan.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["onboard_mode", "polling_rate", "lighting", "button"]);
        // "all" replaces the per-zone entry
        let all = Json::parse(r#"{"zone":"all","effect":"spectrum"}"#).unwrap();
        store.remember(id, "Mouse", &storage_key("lighting", &all).unwrap(), all);
        let s = store.settings(id);
        assert!(s.iter().any(|(k, _)| k == "lighting:all"));
        assert!(!s.iter().any(|(k, _)| k == "lighting:logo"));
        assert_eq!(storage_key("dpi_shift", &Json::Bool(true)), None);
        store.forget(id);
        assert!(store.settings(id).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
