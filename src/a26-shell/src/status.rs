use std::fs;

const BATTERY_CAPACITY: &str = "/sys/class/power_supply/battery/capacity";
const BATTERY_STATUS: &str = "/sys/class/power_supply/battery/status";
const WIFI_OPERSTATE: &str = "/sys/class/net/wlan0/operstate";
const WIFI_CARRIER: &str = "/sys/class/net/wlan0/carrier";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceStatus {
    pub battery_percent: Option<u8>,
    pub battery_charging: bool,
    pub wifi_connected: bool,
}

/// Reads a status file; `MOON_STATUS_ROOT` prefixes the sysfs paths so the
/// shell can be previewed with simulated battery and Wi-Fi state.
fn read_status(path: &str) -> std::io::Result<String> {
    match std::env::var_os("MOON_STATUS_ROOT") {
        Some(root) => fs::read_to_string(std::path::Path::new(&root).join(&path[1..])),
        None => fs::read_to_string(path),
    }
}

impl DeviceStatus {
    pub fn read() -> Self {
        let battery_percent = read_status(BATTERY_CAPACITY)
            .ok()
            .and_then(|value| value.trim().parse::<u8>().ok())
            .map(|value| value.min(100));
        // Plugged in: charging, or full on the charger (the A26 reports
        // `Full` at 100 % on a charger, as iPhone shows the bolt).
        let battery_charging = read_status(BATTERY_STATUS)
            .is_ok_and(|value| matches!(value.trim(), "Charging" | "Full"));
        let wifi_connected = read_status(WIFI_OPERSTATE).is_ok_and(|value| value.trim() == "up")
            && read_status(WIFI_CARRIER).is_ok_and(|value| value.trim() == "1");
        Self {
            battery_percent,
            battery_charging,
            wifi_connected,
        }
    }
}
