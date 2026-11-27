use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Сколько сетей помнить. Свежие профили в начале списка, старые вытесняются.
const MAX_PROFILES: usize = 20;

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
    /// Прозрачный фон окна (Mica) в Windows 11.
    pub mica: bool,
    /// Какая стратегия работала в какой сети: имя сети и файл стратегии.
    pub profiles: Vec<(String, String)>,
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
            mica: true,
            profiles: Vec::new(),
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

    /// Стратегия, которая работала в этой сети.
    pub fn profile(&self, net: &str) -> Option<&str> {
        self.profiles.iter().find(|(n, _)| n == net).map(|(_, s)| s.as_str())
    }

    /// Запомнить стратегию для сети: после подбора или ручного выбора.
    pub fn remember(&mut self, net: &str, strategy: &str) {
        if net.is_empty() {
            return;
        }
        self.profiles.retain(|(n, _)| n != net);
        self.profiles.insert(0, (net.to_string(), strategy.to_string()));
        self.profiles.truncate(MAX_PROFILES);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_networks() {
        let mut s = Settings::default();
        s.remember("Home", "general.bat");
        s.remember("Phone", "general (ALT).bat");
        s.remember("Home", "general (ALT2).bat");
        assert_eq!(s.profile("Home"), Some("general (ALT2).bat"));
        assert_eq!(s.profile("Phone"), Some("general (ALT).bat"));
        assert_eq!(s.profiles[0].0, "Home");
        assert_eq!(s.profiles.len(), 2);
        s.remember("", "general.bat");
        assert_eq!(s.profiles.len(), 2);
        for n in 0..30 {
            s.remember(&format!("net{n}"), "general.bat");
        }
        assert_eq!(s.profiles.len(), MAX_PROFILES);
        assert_eq!(s.profile("Home"), None);
        // Настройки прошлых версий без профилей читаются как раньше.
        let old = Settings::parse(r#"{"strategy":"general (ALT).bat","games":true}"#);
        assert!(old.profiles.is_empty() && old.games && old.mica);
        assert_eq!(old.strategy, "general (ALT).bat");
        // Профили переживают сохранение.
        let back = Settings::parse(&serde_json::to_string(&s).unwrap());
        assert_eq!(back.profiles, s.profiles);
    }
}
