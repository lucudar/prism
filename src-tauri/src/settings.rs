use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub strategy: String,
    pub engine_on: bool,
    pub games: bool,
    pub ai: bool,
    pub tg_on: bool,
    pub tg_port: u16,
    pub tg_secret: String,
    pub autostart: bool,
    pub tray: bool,
    pub auto_update: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            strategy: "general.bat".into(),
            engine_on: false,
            games: false,
            ai: false,
            tg_on: false,
            tg_port: 1443,
            tg_secret: String::new(),
            autostart: false,
            tray: true,
            auto_update: true,
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        Self::parse(&fs::read_to_string(path).unwrap_or_default())
    }

    pub fn parse(text: &str) -> Self {
        serde_json::from_str(text).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) {
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(t) = serde_json::to_string_pretty(self) {
            let _ = crate::util::write_atomic(path, t);
        }
    }
}
