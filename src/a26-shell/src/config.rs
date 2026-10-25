use serde::Deserialize;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Config {
    pub pin_salt: Vec<u8>,
    pub pin_hash: Vec<u8>,
    pub pin_length: usize,
    pub start_locked: bool,
    pub initial_volume: u8,
    pub keyboard_repeat_delay_ms: u64,
    pub keyboard_repeat_rate_hz: u32,
    pub deep_suspend: bool,
    /// Show the clock as 21:41 rather than 9:41.
    pub clock_24h: bool,
    pub socket_path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct ConfigFile {
    pin_salt_hex: String,
    pin_hash_hex: String,
    #[serde(default = "default_pin_length")]
    pin_length: usize,
    #[serde(default = "default_true")]
    start_locked: bool,
    #[serde(default = "default_volume")]
    initial_volume: u8,
    #[serde(default = "default_keyboard_repeat_delay_ms")]
    keyboard_repeat_delay_ms: u64,
    #[serde(default = "default_keyboard_repeat_rate_hz")]
    keyboard_repeat_rate_hz: u32,
    /// Deep suspend currently resumes through a warm reboot, so screen-off
    /// keeps the session resident unless this is explicitly enabled.
    #[serde(default)]
    deep_suspend: bool,
    #[serde(default)]
    clock_24h: bool,
    #[serde(default = "default_socket")]
    socket_path: PathBuf,
}

fn default_pin_length() -> usize {
    6
}
fn default_true() -> bool {
    true
}
fn default_volume() -> u8 {
    50
}
fn default_keyboard_repeat_delay_ms() -> u64 {
    200
}
fn default_keyboard_repeat_rate_hz() -> u32 {
    45
}
fn default_socket() -> PathBuf {
    "/run/a26-shell/control.sock".into()
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, Box<dyn Error>> {
        let contents = fs::read_to_string(path)?;
        let file: ConfigFile = serde_json::from_str(&contents)?;
        let pin_salt = decode_hex(&file.pin_salt_hex)?;
        let pin_hash = decode_hex(&file.pin_hash_hex)?;
        if pin_salt.len() < 16 {
            return Err("PIN salt must contain at least 16 bytes".into());
        }
        if pin_hash.len() != 32 {
            return Err("PIN hash must be a SHA-256 digest".into());
        }
        if !(4..=12).contains(&file.pin_length) {
            return Err("PIN length must be between 4 and 12".into());
        }
        if !(50..=2_000).contains(&file.keyboard_repeat_delay_ms) {
            return Err("keyboard repeat delay must be between 50 and 2000 ms".into());
        }
        if !(1..=100).contains(&file.keyboard_repeat_rate_hz) {
            return Err("keyboard repeat rate must be between 1 and 100 Hz".into());
        }
        Ok(Self {
            pin_salt,
            pin_hash,
            pin_length: file.pin_length,
            start_locked: file.start_locked,
            initial_volume: file.initial_volume.min(100),
            keyboard_repeat_delay_ms: file.keyboard_repeat_delay_ms,
            keyboard_repeat_rate_hz: file.keyboard_repeat_rate_hz,
            deep_suspend: file.deep_suspend,
            clock_24h: file.clock_24h,
            socket_path: file.socket_path,
        })
    }
}

fn decode_hex(input: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    if input.len() % 2 != 0 {
        return Err("hex value has odd length".into());
    }
    input
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair)?;
            Ok(u8::from_str_radix(text, 16)?)
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()
}
