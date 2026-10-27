//! The user's appearance settings and Auto-Lock (docs/settings.md).
//! Brightness lives in its own store (docs/display-brightness.md).
//!
//! Moon owns the store: it is the only writer of the file, applies every
//! change at once, and takes changes from the System app over its control
//! socket (`settings set KEY VALUE`). The file is JSON, written atomically,
//! read once at startup.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const SETTINGS_PATH: &str = "/var/lib/a26-shell/settings.json";

/// Auto-Lock choices offered by the System app (seconds; 0 = never).
pub const AUTO_LOCK_CHOICES: [u32; 6] = [15, 30, 60, 120, 300, 0];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Format version of the file.
    pub version: u32,
    /// A built-in wallpaper id (see wallpaper.rs).
    pub wallpaper: String,
    /// Seconds without input before the screen dims and turns off; 0 never.
    pub auto_lock_s: u32,
    /// Short vibrator taps on keys and toggles.
    pub haptics: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            wallpaper: "moon".into(),
            auto_lock_s: 30,
            haptics: true,
        }
    }
}

impl Settings {
    /// The store's path (`MOON_SETTINGS` overrides it off the phone).
    pub fn path() -> PathBuf {
        std::env::var_os("MOON_SETTINGS")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(SETTINGS_PATH))
    }

    /// The saved settings, or the defaults for anything missing or invalid.
    pub fn load(path: &Path) -> Self {
        let settings: Self = fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        settings.sanitized()
    }

    fn sanitized(mut self) -> Self {
        let defaults = Self::default();
        if !crate::wallpaper::exists(&self.wallpaper) {
            self.wallpaper = defaults.wallpaper;
        }
        if !AUTO_LOCK_CHOICES.contains(&self.auto_lock_s) {
            self.auto_lock_s = defaults.auto_lock_s;
        }
        self.version = 1;
        self
    }

    /// Writes the file atomically (a temporary file, then rename).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("json.new");
        let mut file = fs::File::create(&temporary)?;
        file.write_all(serde_json::to_string_pretty(self)?.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    }

    /// Applies `settings set KEY VALUE`.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), &'static str> {
        match key {
            "wallpaper" => {
                if !crate::wallpaper::exists(value) {
                    return Err("unknown wallpaper");
                }
                self.wallpaper = value.to_owned();
            }
            "auto_lock_s" => {
                self.auto_lock_s = value
                    .parse::<u32>()
                    .ok()
                    .filter(|v| AUTO_LOCK_CHOICES.contains(v))
                    .ok_or("auto_lock_s must be 15, 30, 60, 120, 300 or 0")?;
            }
            "haptics" => {
                self.haptics = match value {
                    "on" => true,
                    "off" => false,
                    _ => return Err("haptics must be on or off"),
                };
            }
            _ => return Err("unknown setting"),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_defaults_and_rejects_bad_values() {
        let dir = std::env::temp_dir().join(format!("moon-settings-{}", std::process::id()));
        let path = dir.join("settings.json");
        let _ = fs::remove_file(&path);
        assert_eq!(Settings::load(&path), Settings::default());
        let mut settings = Settings::default();
        assert!(settings.set("wallpaper", "black").is_ok());
        assert!(settings.set("wallpaper", "nope").is_err());
        assert!(settings.set("auto_lock_s", "45").is_err());
        assert!(settings.set("auto_lock_s", "0").is_ok());
        assert!(settings.set("haptics", "off").is_ok());
        assert!(!settings.haptics);
        assert!(settings.set("haptics", "maybe").is_err());
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path), settings);
        // Unknown keys and a newer field never break loading.
        fs::write(&path, r#"{"wallpaper":"black","future":1,"auto_lock_s":7}"#).unwrap();
        let loaded = Settings::load(&path);
        assert_eq!(loaded.wallpaper, "black");
        assert_eq!(loaded.auto_lock_s, 30);
        let _ = fs::remove_dir_all(&dir);
    }
}
