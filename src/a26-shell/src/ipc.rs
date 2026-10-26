use serde::Serialize;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::keyboard::KeyboardPurpose;
use crate::lease::LeaseKind;
use crate::model::AppId;

#[derive(Debug, Clone)]
pub enum Command {
    Ping,
    State,
    Digit(u8),
    Backspace,
    Submit,
    Tap(i16, i16),
    PointerBegin(i16, i16),
    PointerMove(i16, i16),
    PointerEnd(i16, i16),
    Lock,
    Home,
    Switcher,
    Profile(bool),
    /// Diagnostics: skip drawing the frame nodes in this bitmask.
    ProfileSkip(u64),
    /// Diagnostics: the pixels a frame may redraw (kpx), which bounds how
    /// fast a background fades.
    ProfileBudget(u32),
    /// Diagnostics: log every composed frame's nodes.
    ProfileFrames(bool),
    LaunchSystem,
    LaunchBrowser,
    KeyboardShow(KeyboardPurpose),
    KeyboardHide,
    SwipeUp,
    VolumeUp,
    VolumeDown,
    VolumeSet(u8),
    Power,
    ScreenOff,
    ScreenOn,
    LeaseAcquire(AppId, LeaseKind, u64),
    LeaseRelease(AppId, LeaseKind),
    SimulateMemoryPressure,
    SuspendTest(u64),
    /// `settings set KEY VALUE` (docs/settings.md).
    SettingsSet(String, String),
    Quit,
}

pub struct IpcServer {
    listener: UnixListener,
    path: PathBuf,
}

impl IpcServer {
    pub fn bind(path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
        if path.exists() {
            fs::remove_file(path)?;
        }
        let listener = UnixListener::bind(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            path: path.to_path_buf(),
        })
    }

    pub fn accept_all(&self) -> Vec<(UnixStream, Result<Command, String>)> {
        let mut requests = Vec::new();
        loop {
            match self.listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(150)));
                    let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
                    let mut line = String::new();
                    let result = BufReader::new(&mut stream)
                        .read_line(&mut line)
                        .map_err(|error| error.to_string())
                        .and_then(|_| parse_command(line.trim()));
                    requests.push((stream, result));
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => {
                    eprintln!("IPC accept failed: {error}");
                    break;
                }
            }
        }
        requests
    }

    pub fn raw_fd(&self) -> RawFd {
        self.listener.as_raw_fd()
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn respond<T: Serialize>(mut stream: UnixStream, result: Result<&T, &str>) {
    #[derive(Serialize)]
    struct Envelope<'a, T: Serialize> {
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<&'a T>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<&'a str>,
    }
    let envelope = match result {
        Ok(value) => Envelope {
            ok: true,
            result: Some(value),
            error: None,
        },
        Err(error) => Envelope {
            ok: false,
            result: None,
            error: Some(error),
        },
    };
    let _ = serde_json::to_writer(&mut stream, &envelope);
    let _ = stream.write_all(b"\n");
    let _ = stream.flush();
}

fn parse_command(line: &str) -> Result<Command, String> {
    let mut parts = line.split_whitespace();
    let name = parts.next().ok_or_else(|| "empty command".to_string())?;
    let parse_i16 = |value: Option<&str>, label: &str| {
        value
            .ok_or_else(|| format!("missing {label}"))?
            .parse::<i16>()
            .map_err(|_| format!("invalid {label}"))
    };
    let no_extra = |mut parts: std::str::SplitWhitespace<'_>| {
        if parts.next().is_some() {
            Err("too many arguments".to_string())
        } else {
            Ok(())
        }
    };

    let command = match name {
        "ping" => Command::Ping,
        "state" => Command::State,
        "backspace" => Command::Backspace,
        "submit" => Command::Submit,
        "lock" => Command::Lock,
        "home" => Command::Home,
        "switcher" => Command::Switcher,
        "swipe-up" => Command::SwipeUp,
        "profile" => match parts.next() {
            Some("on") => Command::Profile(true),
            Some("off") => Command::Profile(false),
            Some("budget") => Command::ProfileBudget(
                parts
                    .next()
                    .and_then(|v| v.parse().ok())
                    .filter(|kpx| (100..=3000).contains(kpx))
                    .ok_or("profile budget requires 100..3000 (kpx)")?,
            ),
            Some("frames") => Command::ProfileFrames(parts.next() == Some("on")),
            Some("skip") => Command::ProfileSkip(
                parts
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("profile skip requires a node bitmask")?,
            ),
            _ => return Err("profile requires on or off".into()),
        },
        "power" => Command::Power,
        "quit" => Command::Quit,
        "digit" => {
            let digit = parts
                .next()
                .ok_or_else(|| "missing digit".to_string())?
                .parse::<u8>()
                .map_err(|_| "invalid digit".to_string())?;
            if digit > 9 {
                return Err("digit must be 0 through 9".into());
            }
            Command::Digit(digit)
        }
        "tap" => Command::Tap(parse_i16(parts.next(), "x")?, parse_i16(parts.next(), "y")?),
        "pointer-begin" => {
            Command::PointerBegin(parse_i16(parts.next(), "x")?, parse_i16(parts.next(), "y")?)
        }
        "pointer-move" => {
            Command::PointerMove(parse_i16(parts.next(), "x")?, parse_i16(parts.next(), "y")?)
        }
        "pointer-end" => {
            Command::PointerEnd(parse_i16(parts.next(), "x")?, parse_i16(parts.next(), "y")?)
        }
        "launch" => match parts.next() {
            Some(value) if value.eq_ignore_ascii_case("system") => Command::LaunchSystem,
            Some(value) if value.eq_ignore_ascii_case("browser") => Command::LaunchBrowser,
            Some(_) => return Err("app must be System or Browser".into()),
            None => return Err("missing app noun".into()),
        },
        "keyboard" => match parts.next() {
            Some("show") => {
                let purpose = parts
                    .next()
                    .and_then(KeyboardPurpose::parse)
                    .ok_or_else(|| {
                        "keyboard show requires text, url, search, password, or number".to_string()
                    })?;
                Command::KeyboardShow(purpose)
            }
            Some("hide") => Command::KeyboardHide,
            Some(_) => return Err("keyboard requires show or hide".into()),
            None => return Err("keyboard requires show or hide".into()),
        },
        "volume" => match parts.next() {
            Some("up") => Command::VolumeUp,
            Some("down") => Command::VolumeDown,
            Some(value) => Command::VolumeSet(
                value
                    .parse::<u8>()
                    .map_err(|_| "invalid volume".to_string())?
                    .min(100),
            ),
            None => return Err("volume requires up, down, or 0..100".into()),
        },
        "settings" => match (parts.next(), parts.next(), parts.next()) {
            (Some("set"), Some(key), Some(value)) => {
                Command::SettingsSet(key.to_owned(), value.to_owned())
            }
            _ => return Err("settings requires set KEY VALUE".into()),
        },
        "screen" => match parts.next() {
            Some("off") => Command::ScreenOff,
            Some("on") => Command::ScreenOn,
            Some(_) => return Err("screen requires on or off".into()),
            None => return Err("screen requires on or off".into()),
        },
        "lease" => {
            let operation = parts
                .next()
                .ok_or_else(|| "lease requires acquire or release".to_string())?;
            let app = parts
                .next()
                .and_then(AppId::parse)
                .ok_or_else(|| "lease app must be system or browser".to_string())?;
            let kind = parts
                .next()
                .and_then(LeaseKind::parse)
                .ok_or_else(|| "lease kind must be media or transfer".to_string())?;
            match operation {
                "acquire" => {
                    let seconds = parts
                        .next()
                        .ok_or_else(|| "lease acquire requires seconds".to_string())?
                        .parse::<u64>()
                        .map_err(|_| "invalid lease duration".to_string())?;
                    Command::LeaseAcquire(app, kind, seconds)
                }
                "release" => Command::LeaseRelease(app, kind),
                _ => return Err("lease requires acquire or release".into()),
            }
        }
        "memory-pressure" => match parts.next() {
            Some("simulate") => Command::SimulateMemoryPressure,
            Some(_) => return Err("memory-pressure requires simulate".into()),
            None => return Err("memory-pressure requires simulate".into()),
        },
        "suspend" => match parts.next() {
            Some("test") => {
                let seconds = parts
                    .next()
                    .ok_or_else(|| "suspend test requires seconds".to_string())?
                    .parse::<u64>()
                    .map_err(|_| "invalid suspend test duration".to_string())?;
                if !(2..=30).contains(&seconds) {
                    return Err("suspend test duration must be 2 through 30 seconds".into());
                }
                Command::SuspendTest(seconds)
            }
            Some(_) => return Err("suspend requires test".into()),
            None => return Err("suspend requires test".into()),
        },
        _ => return Err(format!("unknown command: {name}")),
    };
    no_extra(parts)?;
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_switcher_and_profile_commands() {
        assert!(matches!(parse_command("switcher"), Ok(Command::Switcher)));
        assert!(matches!(
            parse_command("profile on"),
            Ok(Command::Profile(true))
        ));
        assert!(matches!(
            parse_command("profile off"),
            Ok(Command::Profile(false))
        ));
        assert!(parse_command("profile").is_err());
    }

    #[test]
    fn parses_keyboard_commands_without_a_second_protocol() {
        assert!(matches!(
            parse_command("keyboard show url"),
            Ok(Command::KeyboardShow(KeyboardPurpose::Url))
        ));
        assert!(matches!(
            parse_command("keyboard hide"),
            Ok(Command::KeyboardHide)
        ));
        assert!(parse_command("keyboard show secret").is_err());
        assert!(parse_command("keyboard show password extra").is_err());
    }

    #[test]
    fn parses_only_bounded_lease_vocabulary() {
        assert!(matches!(
            parse_command("lease acquire browser media 5"),
            Ok(Command::LeaseAcquire(AppId::Browser, LeaseKind::Media, 5))
        ));
        assert!(matches!(
            parse_command("lease release system transfer"),
            Ok(Command::LeaseRelease(AppId::System, LeaseKind::Transfer))
        ));
        assert!(parse_command("lease acquire browser arbitrary 5").is_err());
        assert!(parse_command("lease release browser media extra").is_err());
    }

    #[test]
    fn memory_pressure_simulation_is_an_exact_root_control() {
        assert!(matches!(
            parse_command("memory-pressure simulate"),
            Ok(Command::SimulateMemoryPressure)
        ));
        assert!(parse_command("memory-pressure").is_err());
        assert!(parse_command("memory-pressure kill browser").is_err());
    }

    #[test]
    fn suspend_test_alarm_is_strictly_bounded() {
        assert!(matches!(
            parse_command("suspend test 5"),
            Ok(Command::SuspendTest(5))
        ));
        assert!(parse_command("suspend test 1").is_err());
        assert!(parse_command("suspend test 31").is_err());
        assert!(parse_command("suspend now").is_err());
    }
}
