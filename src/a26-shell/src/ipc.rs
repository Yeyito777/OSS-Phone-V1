use serde::Serialize;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
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
    Launch(AppId),
    /// `close APP`: as swiping it away in the app switcher (an app in
    /// front goes home first), so updates and tools can restart an app.
    Close(AppId),
    /// `open APP TARGET`: the app opens at a target, as when a notification
    /// is tapped (after unlocking, when locked).
    Open(AppId, String),
    /// `voice start|send|cancel`: as holding and releasing the power key
    /// (voice.rs).
    Voice(crate::voice::VoiceRequest),
    KeyboardShow(KeyboardPurpose),
    KeyboardHide,
    SwipeUp,
    VolumeUp,
    VolumeDown,
    VolumeSet(u8),
    /// `brightness` (read), `brightness set N`, `brightness auto on|off`.
    Brightness(Option<crate::display::DisplayRequest>),
    Power,
    /// `power menu [restart]`: the power menu, as if the key were held.
    PowerMenu(crate::power::PowerAction),
    /// `system poweroff|reboot`: power off or restart now, cleanly.
    System(crate::power::PowerAction),
    ScreenOff,
    ScreenOn,
    /// `light-probe X Y SIZE` / `light-probe off`: a debug frame, black with
    /// one white square, for locating the light sensor under the panel.
    LightProbe(Option<crate::gfx::RectF>),
    LeaseAcquire(AppId, LeaseKind, u64),
    LeaseRelease(AppId, LeaseKind),
    SimulateMemoryPressure,
    SuspendTest(u64),
    /// `settings set KEY VALUE` (docs/settings.md).
    SettingsSet(String, String),
    /// `layout ...`: the home screen's layout and edit mode
    /// (docs/home-screen.md).
    Layout(crate::home::LayoutCommand),
    /// `transition push|pop`: an app is about to show another page (the
    /// reply comes once its current page is snapshotted).
    Transition(bool),
    /// `transition ready`: the new page is fully drawn; the slide starts.
    TransitionReady,
    /// Answered once the X root shows what the panel does (a Wayland app
    /// shown from its own buffers is copied into its X window first), for
    /// a26-shellshot.
    ScreenSync,
    /// `haptic STYLE`: a tap haptic for an app's control (System's switches).
    /// (`haptic STYLE MS` plays that length once, for tuning.)
    Haptic(crate::notify_client::Haptic, Option<u32>),
    /// `statusbar [APP] color #RRGGBB [light|dark]` / `statusbar [APP]
    /// default`: the status bar over an app in its own colour, or back to
    /// black (status_bar.rs). Without APP: the app asking.
    StatusBar(Option<AppId>, Option<crate::status_bar::StatusStyle>),
    /// `clipboard get`: the clipboard's text, and whether it has an image
    /// (clipboard.rs).
    ClipboardGet,
    /// `clipboard get image`: the clipboard's image, in base64.
    ClipboardGetImage,
    /// `clipboard set "TEXT"` (a JSON string): the clipboard holds it.
    ClipboardSet(String),
    /// `clipboard set image/png BASE64`: the clipboard holds the image.
    ClipboardSetImage(crate::clipboard::ClipImage),
    /// A `clipboard set image/...` line not read yet: megabytes of base64,
    /// for a thread of its own to read (`read_clipboard_upload`).
    ClipboardUpload,
    /// `awake [APP] on|off`: the app in front keeps the screen on (no
    /// Auto-Lock dim or lock) while the process that asked runs. Without
    /// APP: the app asking.
    Awake(Option<AppId>, bool),
    /// `badge [APP] N`: the count on the app's home screen icon (0: none;
    /// badges.rs). Without APP: the app asking.
    Badge(Option<AppId>, u32),
    Quit,
    /// `restart`: quit cleanly, then run the binary on disk again in the
    /// same process. An update goes live; the session, its mounts and the
    /// supervisor carry on.
    Restart,
}

impl Command {
    /// Commands that stand in for the user's own touches and keys (tests,
    /// `ctl`): like real input they count for Auto-Lock. Requests apps make
    /// on the user's behalf (keyboard, brightness, settings) do not.
    pub fn is_user_input(&self) -> bool {
        matches!(
            self,
            Command::Digit(_)
                | Command::Backspace
                | Command::Submit
                | Command::Tap(..)
                | Command::PointerBegin(..)
                | Command::PointerMove(..)
                | Command::PointerEnd(..)
                | Command::Home
                | Command::Switcher
                | Command::SwipeUp
                | Command::VolumeUp
                | Command::VolumeDown
                | Command::Power
                | Command::Voice(_)
        )
    }
}

/// The longest command line: `clipboard set image/png` with an 8 MiB image
/// in base64 (`clipboard set` with a 1 MiB text, escaped, takes at most 6).
const MAX_LINE: u64 = 12 << 20;

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
                    if is_clipboard_upload(&stream) {
                        requests.push((stream, Ok(Command::ClipboardUpload)));
                        continue;
                    }
                    let mut line = String::new();
                    let result = BufReader::new((&mut stream).take(MAX_LINE))
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

/// The process on the other end of a connection.
pub fn peer_pid(stream: &UnixStream) -> Option<u32> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: SO_PEERCRED fills exactly one ucred of the given length.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    (result == 0 && credentials.pid > 0).then_some(credentials.pid as u32)
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
    if let Some(value) = line.strip_prefix("clipboard set ") {
        let value = value.trim();
        if value.starts_with("image/") {
            return parse_clipboard_image(value).map(Command::ClipboardSetImage);
        }
        // The text is one JSON string, spaces and all.
        return serde_json::from_str::<String>(value)
            .map(Command::ClipboardSet)
            .map_err(|_| "clipboard set requires a JSON string".into());
    }
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
        "power" => match (parts.next(), parts.next()) {
            (None, _) => Command::Power,
            (Some("menu"), None) => Command::PowerMenu(crate::power::PowerAction::PowerOff),
            (Some("menu"), Some("restart")) => {
                Command::PowerMenu(crate::power::PowerAction::Restart)
            }
            _ => return Err("power takes nothing, or menu [restart]".into()),
        },
        "system" => match parts.next() {
            Some("poweroff") => Command::System(crate::power::PowerAction::PowerOff),
            Some("reboot") => Command::System(crate::power::PowerAction::Restart),
            _ => return Err("system requires poweroff or reboot".into()),
        },
        "quit" => Command::Quit,
        "restart" => Command::Restart,
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
        "close" => match parts.next().map(str::to_ascii_lowercase) {
            Some(value) => match AppId::parse(&value) {
                Some(app) => Command::Close(app),
                None => return Err("unknown app".into()),
            },
            None => return Err("close requires an app".into()),
        },
        "launch" => match parts.next() {
            Some(value) => match AppId::parse(&value.to_ascii_lowercase()) {
                Some(app) => Command::Launch(app),
                None => {
                    return Err("app must be System, Browser, Phone, Messages or Exocortex".into());
                }
            },
            None => return Err("missing app noun".into()),
        },
        "open" => match (parts.next().map(str::to_ascii_lowercase), parts.next()) {
            (Some(app), Some(target)) => match AppId::parse(&app) {
                Some(AppId::Browser) if !crate::browser_links::valid_url(target) => {
                    return Err("open browser requires an http or https URL".into());
                }
                Some(app) => Command::Open(app, target.to_owned()),
                None => return Err("unknown app".into()),
            },
            _ => return Err("open requires APP TARGET".into()),
        },
        "voice" => Command::Voice(match parts.next() {
            Some("start") => crate::voice::VoiceRequest::Start,
            Some("send") => crate::voice::VoiceRequest::Send,
            Some("cancel") => crate::voice::VoiceRequest::Cancel,
            _ => return Err("voice requires start, send or cancel".into()),
        }),
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
        "brightness" => match parts.next() {
            None => Command::Brightness(None),
            Some("set") => Command::Brightness(Some(crate::display::DisplayRequest::Set(
                parts
                    .next()
                    .and_then(|value| value.parse::<u8>().ok())
                    .filter(|value| *value <= 100)
                    .ok_or_else(|| "brightness set requires 0..100".to_string())?,
            ))),
            Some("auto") => match parts.next() {
                Some("on") => Command::Brightness(Some(crate::display::DisplayRequest::Auto(true))),
                Some("off") => {
                    Command::Brightness(Some(crate::display::DisplayRequest::Auto(false)))
                }
                _ => return Err("brightness auto requires on or off".into()),
            },
            Some(_) => return Err("brightness requires set N or auto on|off".into()),
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
        "layout" => return crate::home::LayoutCommand::parse(parts).map(Command::Layout),
        "transition" => match parts.next() {
            Some("push") => Command::Transition(true),
            Some("pop") => Command::Transition(false),
            Some("ready") => Command::TransitionReady,
            _ => return Err("transition requires push or pop".into()),
        },
        "haptic" => match parts.next().and_then(crate::notify_client::Haptic::parse) {
            Some(style) => Command::Haptic(
                style,
                match parts.next() {
                    None => None,
                    Some(ms) => Some(
                        ms.parse::<u32>()
                            .ok()
                            .filter(|ms| (5..=400).contains(ms))
                            .ok_or("haptic length is 5..400 ms")?,
                    ),
                },
            ),
            None => return Err("haptic requires selection, light, medium or error".into()),
        },
        "statusbar" => {
            let mut word = parts.next();
            let app = word.and_then(AppId::parse);
            if app.is_some() {
                word = parts.next();
            }
            match word {
                Some("color") => {
                    let color = parts
                        .next()
                        .ok_or("statusbar color requires #RRGGBB [light|dark]")?;
                    Command::StatusBar(
                        app,
                        Some(crate::status_bar::StatusStyle::parse(color, parts.next())?),
                    )
                }
                Some("default") => Command::StatusBar(app, None),
                _ => {
                    return Err(
                        "statusbar requires [APP] color #RRGGBB [light|dark] or [APP] default"
                            .into(),
                    );
                }
            }
        }
        "clipboard" => match (parts.next(), parts.next()) {
            (Some("get"), None) => Command::ClipboardGet,
            (Some("get"), Some("image")) => Command::ClipboardGetImage,
            _ => {
                return Err(
                    "clipboard requires get [image], set \"TEXT\" or set image/png BASE64".into(),
                );
            }
        },
        "awake" => {
            let mut word = parts.next();
            let app = word.and_then(AppId::parse);
            if app.is_some() {
                word = parts.next();
            }
            match word {
                Some("on") => Command::Awake(app, true),
                Some("off") => Command::Awake(app, false),
                _ => return Err("awake requires [APP] on or off".into()),
            }
        }
        "badge" => {
            let mut word = parts.next();
            let app = word.and_then(AppId::parse);
            if app.is_some() {
                word = parts.next();
            }
            let count = word
                .and_then(|word| word.parse::<u32>().ok())
                .filter(|count| *count <= crate::badges::MAX)
                .ok_or("badge requires [APP] N (0 to 99999; 0 clears it)")?;
            Command::Badge(app, count)
        }
        "light-probe" => match parts.next() {
            Some("off") => Command::LightProbe(None),
            Some(x) => {
                let number = |text: Option<&str>| -> Result<f32, String> {
                    text.and_then(|t| t.parse::<f32>().ok())
                        .filter(|v| v.is_finite() && (0.0..=4000.0).contains(v))
                        .ok_or_else(|| "light-probe requires X Y SIZE or off".to_string())
                };
                let x = number(Some(x))?;
                let y = number(parts.next())?;
                let size = number(parts.next())?;
                Command::LightProbe(Some(crate::gfx::RectF::new(x, y, size, size)))
            }
            None => return Err("light-probe requires X Y SIZE or off".into()),
        },
        "screen" => match parts.next() {
            Some("off") => Command::ScreenOff,
            Some("on") => Command::ScreenOn,
            Some("sync") => Command::ScreenSync,
            Some(_) => return Err("screen requires on, off or sync".into()),
            None => return Err("screen requires on, off or sync".into()),
        },
        "lease" => {
            let operation = parts
                .next()
                .ok_or_else(|| "lease requires acquire or release".to_string())?;
            let app = parts.next().and_then(AppId::parse).ok_or_else(|| {
                "lease app must be system, browser, phone, messages or exocortex".to_string()
            })?;
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

const CLIPBOARD_UPLOAD: &[u8] = b"clipboard set image/";

/// Whether a connection's line starts `clipboard set image/` (a look at
/// what has arrived, nothing read). A line that has not arrived that far
/// is read here as any other, and parses the same.
fn is_clipboard_upload(stream: &UnixStream) -> bool {
    let mut head = [0_u8; CLIPBOARD_UPLOAD.len()];
    // SAFETY: recv writes at most `head.len()` bytes into `head`.
    let peeked = unsafe {
        libc::recv(
            stream.as_raw_fd(),
            head.as_mut_ptr().cast(),
            head.len(),
            libc::MSG_PEEK,
        )
    };
    peeked == head.len() as isize && head == CLIPBOARD_UPLOAD
}

/// Reads and checks a `clipboard set image/...` line (on its own thread:
/// up to 12 MiB, then the base64).
pub fn read_clipboard_upload(stream: &UnixStream) -> Result<crate::clipboard::ClipImage, String> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut line = String::new();
    BufReader::with_capacity(1 << 16, stream.take(MAX_LINE))
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    match parse_command(line.trim())? {
        Command::ClipboardSetImage(image) => Ok(image),
        _ => Err("clipboard set image requires image/TYPE BASE64".into()),
    }
}

/// `image/png BASE64` (`clipboard set`): the image, checked before the
/// clipboard takes it.
fn parse_clipboard_image(value: &str) -> Result<crate::clipboard::ClipImage, String> {
    use crate::clipboard::{self, ClipImage, ImageKind};
    let (mime, data) = value.split_once(' ').unwrap_or((value, ""));
    let kind = ImageKind::parse(mime)
        .ok_or("clipboard images are image/png, image/jpeg, image/gif or image/webp")?;
    let data = data.trim();
    if data.len() / 4 * 3 > clipboard::MAX_IMAGE + 3 {
        return Err("clipboard image is over 8 MiB".into());
    }
    let bytes = clipboard::base64_decode(data)
        .filter(|bytes| !bytes.is_empty())
        .ok_or("clipboard set image requires its data in base64")?;
    ClipImage::new(kind, bytes).map_err(Into::into)
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
        assert!(matches!(
            parse_command("lease acquire exocortex transfer 120"),
            Ok(Command::LeaseAcquire(
                AppId::Exocortex,
                LeaseKind::Transfer,
                120
            ))
        ));
        assert!(parse_command("lease acquire browser arbitrary 5").is_err());
        assert!(parse_command("lease release browser media extra").is_err());
    }

    #[test]
    fn parses_open_and_voice() {
        assert!(matches!(
            parse_command("open exocortex 1793229556873-5bebb9"),
            Ok(Command::Open(AppId::Exocortex, target)) if target == "1793229556873-5bebb9"
        ));
        assert!(parse_command("open exocortex").is_err());
        assert!(parse_command("open nowhere x").is_err());
        assert!(parse_command("open exocortex a b").is_err());
        assert!(matches!(
            parse_command("voice start"),
            Ok(Command::Voice(crate::voice::VoiceRequest::Start))
        ));
        assert!(parse_command("voice").is_err());
        assert!(parse_command("voice send now").is_err());
        assert!(matches!(parse_command("restart"), Ok(Command::Restart)));
        assert!(matches!(
            parse_command("close Exocortex"),
            Ok(Command::Close(AppId::Exocortex))
        ));
        assert!(parse_command("close").is_err());
        assert!(parse_command("close nowhere").is_err());
        assert!(parse_command("restart now").is_err());
    }

    #[test]
    fn parses_layout_commands() {
        assert!(matches!(
            parse_command("layout reset"),
            Ok(Command::Layout(crate::home::LayoutCommand::Reset))
        ));
        assert!(matches!(
            parse_command("layout add browser"),
            Ok(Command::Layout(crate::home::LayoutCommand::Add(
                AppId::Browser
            )))
        ));
        assert!(parse_command("layout").is_err());
        assert!(parse_command("layout reset all").is_err());
        assert!(!Command::Layout(crate::home::LayoutCommand::Reset).is_user_input());
    }

    #[test]
    fn parses_multiline_keyboard_and_browser_links() {
        assert!(matches!(
            parse_command("keyboard show multiline"),
            Ok(Command::KeyboardShow(KeyboardPurpose::Multiline))
        ));
        assert!(matches!(
            parse_command("open browser https://example.com/a?b=c"),
            Ok(Command::Open(AppId::Browser, url)) if url == "https://example.com/a?b=c"
        ));
        assert!(parse_command("open browser file:///etc/passwd").is_err());
        assert!(parse_command("open browser example.com").is_err());
        // Other apps' targets are theirs to read.
        assert!(parse_command("open exocortex not-a-url").is_ok());
    }

    #[test]
    fn parses_status_bar_colours_for_the_caller_or_a_named_app() {
        use crate::status_bar::StatusStyle;
        let navy = StatusStyle {
            background: 0x00050f,
            dark_content: false,
        };
        assert!(matches!(
            parse_command("statusbar color #00050f"),
            Ok(Command::StatusBar(None, Some(style))) if style == navy
        ));
        let white_light = StatusStyle {
            background: 0xffffff,
            dark_content: false,
        };
        assert!(matches!(
            parse_command("statusbar exocortex color #ffffff light"),
            Ok(Command::StatusBar(Some(AppId::Exocortex), Some(style))) if style == white_light
        ));
        assert!(matches!(
            parse_command("statusbar default"),
            Ok(Command::StatusBar(None, None))
        ));
        assert!(matches!(
            parse_command("statusbar browser default"),
            Ok(Command::StatusBar(Some(AppId::Browser), None))
        ));
        for bad in [
            "statusbar",
            "statusbar color",
            "statusbar color blue",
            "statusbar color #00050f grey",
            "statusbar color #00050f dark extra",
            "statusbar nowhere color #000000",
            "statusbar default now",
        ] {
            assert!(parse_command(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn parses_clipboard_text_as_one_json_string() {
        assert!(matches!(
            parse_command("clipboard get"),
            Ok(Command::ClipboardGet)
        ));
        assert!(matches!(
            parse_command(r#"clipboard set "two  words\nand é""#),
            Ok(Command::ClipboardSet(text)) if text == "two  words\nand é"
        ));
        assert!(matches!(
            parse_command(r#"clipboard set """#),
            Ok(Command::ClipboardSet(text)) if text.is_empty()
        ));
        for bad in [
            "clipboard",
            "clipboard set",
            "clipboard set bare words",
            r#"clipboard set "a" "b""#,
            "clipboard get more",
            "clipboard get image now",
            "clipboard clear",
        ] {
            assert!(parse_command(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn parses_clipboard_images_in_base64() {
        use crate::clipboard::{ImageKind, base64_encode};
        assert!(matches!(
            parse_command("clipboard get image"),
            Ok(Command::ClipboardGetImage)
        ));
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
        let line = format!("clipboard set image/png {}", base64_encode(png));
        assert!(matches!(
            parse_command(&line),
            Ok(Command::ClipboardSetImage(image))
                if image.kind == ImageKind::Png && &image.bytes[..] == png
        ));
        let jpeg = format!("clipboard set image/jpeg {}", base64_encode(png));
        for bad in [
            "clipboard set image/png",
            "clipboard set image/png !!!!",
            "clipboard set image/svg+xml PHN2Zz4=",
            jpeg.as_str(),
        ] {
            assert!(parse_command(bad).is_err(), "{bad}");
        }
        // Over the limit: refused before decoding.
        let huge = format!("clipboard set image/png {}", "A".repeat(12 << 20));
        assert!(parse_command(&huge).is_err());
    }

    #[test]
    fn parses_awake_and_badges_for_the_caller_or_a_named_app() {
        assert!(matches!(
            parse_command("awake on"),
            Ok(Command::Awake(None, true))
        ));
        assert!(matches!(
            parse_command("awake exocortex off"),
            Ok(Command::Awake(Some(AppId::Exocortex), false))
        ));
        assert!(matches!(
            parse_command("badge 3"),
            Ok(Command::Badge(None, 3))
        ));
        assert!(matches!(
            parse_command("badge exocortex 0"),
            Ok(Command::Badge(Some(AppId::Exocortex), 0))
        ));
        assert!(matches!(
            parse_command("badge messages 99999"),
            Ok(Command::Badge(Some(AppId::Messages), 99999))
        ));
        for bad in [
            "awake",
            "awake yes",
            "awake exocortex",
            "awake on now",
            "awake nowhere on",
            "badge",
            "badge -1",
            "badge 100000",
            "badge exocortex",
            "badge exocortex 2 3",
            "badge nowhere 2",
        ] {
            assert!(parse_command(bad).is_err(), "{bad}");
        }
        assert!(!Command::Awake(None, true).is_user_input());
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
