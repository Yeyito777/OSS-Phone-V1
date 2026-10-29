//! Voice: hold the power key to talk to Exocortex (docs/voice.md). Moon
//! shows that it listens and runs the voice hook, which records the
//! microphone and, on release, transcribes and sends what was said; the
//! hold's old job, the power menu, moves to the power key with a volume key
//! (as on iPhone). Without the hook, or with `power_hold` set to `menu`,
//! holding the key opens the power menu as before.

use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use crate::gfx::{Canvas, RectF};
use crate::model::{AppId, ShellState, View};
use crate::text::{self, Weight};
use crate::ui::{SECONDARY, SECONDARY_ALPHA, WHITE};

/// Holding the power key this long starts listening.
pub const HOLD_FOR_VOICE: Duration = Duration::from_millis(450);
/// Released sooner than this after listening began: a slip, nothing is sent.
pub const MIN_VOICE: Duration = Duration::from_millis(600);
/// The hook (`MOON_VOICE` overrides it off the phone).
pub const VOICE_HOOK: &str = "/usr/local/bin/exocortex-voice";
const FADE_IN: Duration = Duration::from_millis(160);
const FADE_OUT: Duration = Duration::from_millis(240);
/// The orb's breath while it is quiet.
const PULSE: Duration = Duration::from_millis(1400);
/// How fast the orb follows the voice: rising, and falling back.
const LEVEL_ATTACK: f32 = 0.05;
const LEVEL_RELEASE: f32 = 0.25;

/// The orb: a glow (a square layer) that swells with the voice, and over it
/// the core with the microphone, both centered on `ORB_CENTER`.
pub const HALO: f32 = 420.0;
pub const CORE: f32 = 256.0;
const CORE_RADIUS: f32 = 116.0;
pub const ORB_CENTER: (f32, f32) = (540.0, 1620.0);
/// The label layer, under the orb.
pub const LABEL: RectF = RectF::new(0.0, 1860.0, 1080.0, 200.0);
/// How dark the screen gets behind the orb.
pub const DIM: f32 = 0.6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceRequest {
    Start,
    Send,
    Cancel,
}

#[derive(Debug, Default)]
pub struct VoiceUi {
    /// The hook is installed and `power_hold` is `voice` (main.rs keeps it
    /// current).
    pub available: bool,
    /// Listening since (the overlay is up and the hook records).
    pub listening: Option<Instant>,
    /// Started on the lock screen: what was said is sent after unlocking.
    pub locked: bool,
    /// The overlay fading out after listening ended (since when).
    pub closing: Option<Instant>,
    /// For the main loop: start the hook, or tell it to send or cancel.
    pub requests: Vec<VoiceRequest>,
    /// `open APP TARGET` on the lock screen: opened after unlocking.
    pub open_after_unlock: Option<(AppId, String)>,
    /// How loud the voice is, 0..1, as the orb shows it (smoothed), the
    /// hook's last reading, and when the smoothing last advanced.
    pub level: f32,
    level_target: f32,
    level_at: Option<Instant>,
}

impl VoiceUi {
    pub fn visible(&self, now: Instant) -> bool {
        self.listening.is_some()
            || self
                .closing
                .is_some_and(|at| now.saturating_duration_since(at) < FADE_OUT)
    }

    /// The overlay's opacity (0 when hidden).
    pub fn alpha(&self, now: Instant) -> f32 {
        if let Some(at) = self.listening {
            (now.saturating_duration_since(at).as_secs_f32() / FADE_IN.as_secs_f32()).min(1.0)
        } else if let Some(at) = self.closing {
            1.0 - (now.saturating_duration_since(at).as_secs_f32() / FADE_OUT.as_secs_f32())
                .min(1.0)
        } else {
            0.0
        }
    }

    /// The glow's scale: a slow breath, swelling with the voice.
    pub fn halo_scale(&self, now: Instant) -> f32 {
        let Some(at) = self.listening.or(self.closing) else {
            return 1.0;
        };
        let phase = now.saturating_duration_since(at).as_secs_f32() / PULSE.as_secs_f32();
        1.0 + 0.05 * (phase * std::f32::consts::TAU).sin() + 0.5 * self.level
    }

    /// The core's scale: a small bounce with the voice.
    pub fn core_scale(&self) -> f32 {
        1.0 + 0.1 * self.level
    }

    /// A reading from the hook (`None`: nothing new), and the smoothing up
    /// to `now`: it rises fast and falls back slowly, as a meter does.
    pub fn advance_level(&mut self, reading: Option<f32>, now: Instant) {
        if self.listening.is_none() {
            self.level_target = 0.0;
        } else if let Some(level) = reading {
            self.level_target = level.clamp(0.0, 1.0);
        }
        let dt = self
            .level_at
            .map_or(0.0, |at| now.saturating_duration_since(at).as_secs_f32());
        self.level_at = Some(now);
        let tau = if self.level_target > self.level {
            LEVEL_ATTACK
        } else {
            LEVEL_RELEASE
        };
        self.level += (self.level_target - self.level) * (1.0 - (-dt / tau).exp());
    }
}

impl ShellState {
    /// Holding the power key talks to Exocortex rather than opening the
    /// power menu.
    pub fn voice_on_hold(&self) -> bool {
        self.voice.available
    }

    pub fn voice_listening(&self) -> bool {
        self.voice.listening.is_some()
    }

    /// Starts listening: the overlay, a buzz (whatever the tap-haptics
    /// setting: the user may not be looking), and the hook.
    pub fn voice_start(&mut self, now: Instant) -> bool {
        if !self.screen_awake || self.power_takes_over() || self.voice.listening.is_some() {
            return false;
        }
        self.alerts_cancel_touch();
        self.control_cancel_touch();
        self.call_cancel_touch();
        self.power_cancel_touch();
        self.keyboard.hide();
        self.pointer = None;
        self.pressed = None;
        if let Some(app) = AppId::from_view(self.view)
            && !self.alerts_cover_app(now)
        {
            self.pending_capture = Some(app);
        }
        self.voice.listening = Some(now);
        self.voice.locked = self.view == View::Locked;
        self.voice.closing = None;
        self.voice.requests.push(VoiceRequest::Start);
        self.alerts.outbox.push(crate::alerts::Outgoing::Haptic(
            crate::notify_client::Haptic::Medium,
            None,
        ));
        self.last_action = "voice_listening".into();
        self.redraw = true;
        true
    }

    /// Stops listening: sends what was said (`send`, and long enough), or
    /// drops it. On the lock screen the passcode comes up: the hook sends
    /// once the phone is unlocked. Auto-Lock counts from here: it waited
    /// while Moon listened, and a hold longer than its timeout must not turn
    /// the screen off the moment the key comes up.
    pub fn voice_stop(&mut self, send: bool, now: Instant) {
        let Some(at) = self.voice.listening.take() else {
            return;
        };
        self.last_input = now;
        self.idle_dimmed = false;
        let send = send && now.saturating_duration_since(at) >= MIN_VOICE;
        self.voice.requests.push(if send {
            VoiceRequest::Send
        } else {
            VoiceRequest::Cancel
        });
        self.voice.closing = Some(now);
        if send && self.view == View::Locked {
            self.show_passcode();
        }
        self.last_action = if send {
            "voice_sent"
        } else {
            "voice_cancelled"
        }
        .into();
        self.redraw = true;
    }

    pub fn voice_tick(&mut self, now: Instant) {
        if self
            .voice
            .closing
            .is_some_and(|at| now.saturating_duration_since(at) >= FADE_OUT)
        {
            self.voice.closing = None;
            self.redraw = true;
        }
    }

    /// `open APP TARGET`: the app opens at that target (it reads Moon's
    /// `open_target`), after unlocking when locked. Never wakes the screen.
    pub fn open_target(&mut self, app: AppId, target: String) {
        if !self.screen_awake {
            return;
        }
        if self.view == View::Locked {
            self.voice.open_after_unlock = Some((app, target));
            self.show_passcode();
            return;
        }
        self.open_target = Some(crate::model::OpenTarget::new(app, target));
        if AppId::from_view(self.view) != Some(app) {
            self.open_app(app);
        }
        self.redraw = true;
    }

    /// Unlocked: an `open` that came while locked happens now.
    pub fn voice_unlocked(&mut self) {
        if let Some((app, target)) = self.voice.open_after_unlock.take() {
            self.open_target(app, target);
        }
    }
}

/// The hook's processes: the one recording, and those still transcribing
/// and sending (reaped as they exit).
///
/// The contract: `HOOK record` starts recording at once and reads one line
/// on stdin when the key comes up: `send` (transcribe and send it), `send
/// locked` (held on the lock screen: send only once the phone is unlocked)
/// or `cancel`; end of input (Moon gone) is `cancel`. While it records it
/// may print `level L` lines on stdout (L in 0..1, how loud the voice is),
/// which the orb follows; Moon closes stdout when the key comes up.
#[derive(Default)]
pub struct VoiceHook {
    recording: Option<Recording>,
    finishing: Vec<Child>,
}

struct Recording {
    child: Child,
    stdin: ChildStdin,
    /// Non-blocking; the bytes of an unfinished line wait in `pending`.
    stdout: ChildStdout,
    pending: Vec<u8>,
}

impl VoiceHook {
    pub fn path() -> PathBuf {
        std::env::var_os("MOON_VOICE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(VOICE_HOOK))
    }

    /// The hook is there and executable.
    pub fn installed() -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(Self::path())
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }

    /// `locked`: the hold began on the lock screen.
    pub fn handle(&mut self, request: VoiceRequest, locked: bool) {
        match request {
            VoiceRequest::Start => {
                self.finish("cancel");
                let path = Self::path();
                match Command::new(&path)
                    .arg("record")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .spawn()
                {
                    Ok(mut child) => match (child.stdin.take(), child.stdout.take()) {
                        (Some(stdin), Some(stdout)) => {
                            // SAFETY: a valid descriptor this handle owns.
                            unsafe {
                                let fd = stdout.as_raw_fd();
                                let flags = libc::fcntl(fd, libc::F_GETFL);
                                libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
                            }
                            self.recording = Some(Recording {
                                child,
                                stdin,
                                stdout,
                                pending: Vec::new(),
                            });
                        }
                        _ => self.finishing.push(child),
                    },
                    Err(error) => eprintln!("voice: cannot run {}: {error}", path.display()),
                }
            }
            VoiceRequest::Send if locked => self.finish("send locked"),
            VoiceRequest::Send => self.finish("send"),
            VoiceRequest::Cancel => self.finish("cancel"),
        }
    }

    fn finish(&mut self, word: &str) {
        if let Some(mut recording) = self.recording.take() {
            if let Err(error) = writeln!(recording.stdin, "{word}") {
                eprintln!("voice: telling the hook to {word} failed: {error}");
            }
            // Dropping stdin and stdout closes both pipes.
            self.finishing.push(recording.child);
        }
    }

    /// The newest `level` the recording hook printed since the last call.
    pub fn level(&mut self) -> Option<f32> {
        let recording = self.recording.as_mut()?;
        let mut bytes = [0_u8; 512];
        loop {
            match recording.stdout.read(&mut bytes) {
                Ok(0) => break,
                Ok(count) => recording.pending.extend_from_slice(&bytes[..count]),
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let end = recording.pending.iter().rposition(|byte| *byte == b'\n')?;
        let level = recording.pending[..end]
            .split(|byte| *byte == b'\n')
            .filter_map(|line| {
                std::str::from_utf8(line)
                    .ok()?
                    .strip_prefix("level ")?
                    .trim()
                    .parse()
                    .ok()
            })
            .last();
        recording.pending.drain(..=end);
        level
    }

    pub fn reap(&mut self) {
        self.finishing
            .retain_mut(|child| !matches!(child.try_wait(), Ok(Some(_)) | Err(_)));
    }

    /// Something to reap soon.
    pub fn busy(&self) -> bool {
        !self.finishing.is_empty()
    }
}

/// The orb's glow: soft blue-violet, brightening toward the middle (filled
/// to the center, so a swelling glow shows no gap around the core). Drawn
/// once; the frame scales it to breathe and to follow the voice.
pub fn draw_halo(canvas: &mut Canvas) {
    canvas.clear(0);
    let c = HALO / 2.0;
    // Many faint discs, widest first, down to half the core's size.
    let steps = 36;
    for step in 0..steps {
        let t = step as f32 / steps as f32;
        let radius = c - t * (c - CORE_RADIUS / 2.0);
        let color = crate::gfx::mix(0x5e5ce6, 0x64d2ff, t);
        canvas.fill_circle(c, c, radius, color, 0.035 + t * 0.03);
    }
}

/// The orb's core: a blue-violet disc with a microphone.
pub fn draw_core(canvas: &mut Canvas) {
    canvas.clear(0);
    let c = CORE / 2.0;
    canvas.fill_circle(c, c, CORE_RADIUS, 0x0a84ff, 1.0);
    canvas.fill_circle(c, c, CORE_RADIUS, 0x5e5ce6, 0.35);
    crate::calls::draw_microphone(canvas, c, c + 2.0, 1.35, WHITE);
}

/// "Listening…", and what releasing does.
pub fn draw_label(canvas: &mut Canvas, locked: bool) {
    canvas.clear(0);
    let cx = LABEL.w / 2.0;
    text::draw_centered(
        canvas,
        "Listening…",
        cx,
        70.0,
        Weight::SemiBold,
        60.0,
        WHITE,
        1.0,
    );
    let hint = if locked {
        "Release, then unlock to send to Exocortex"
    } else {
        "Release to send to Exocortex"
    };
    text::draw_centered(
        canvas,
        hint,
        cx,
        140.0,
        Weight::Regular,
        40.0,
        SECONDARY,
        SECONDARY_ALPHA,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::power::HOLD_FOR_MENU;

    fn voice_state() -> ShellState {
        let mut state = ShellState::new(false, 50);
        state.voice.available = true;
        state
    }

    #[test]
    fn holding_the_power_key_listens_and_releasing_sends() {
        let mut state = voice_state();
        let start = Instant::now();
        state.power_key_down(start);
        state.power_tick(start + Duration::from_millis(200));
        assert!(!state.voice_listening());
        state.power_tick(start + HOLD_FOR_VOICE);
        assert!(state.voice_listening());
        assert_eq!(state.voice.requests, [VoiceRequest::Start]);
        assert!(state.shell_covers_app() || !state.view.is_app());
        // Still held past the menu's time: no menu.
        state.power_tick(start + HOLD_FOR_MENU + Duration::from_millis(100));
        assert!(state.power.menu.is_none());
        state.voice.listening = Some(start);
        state.voice_stop(true, start + Duration::from_secs(3));
        state.power_key_up();
        assert!(!state.voice_listening());
        assert_eq!(
            state.voice.requests,
            [VoiceRequest::Start, VoiceRequest::Send]
        );
        assert!(state.fade.is_none(), "the release does not sleep");
        assert!(state.voice.visible(start + Duration::from_secs(3)));
        state.voice_tick(start + Duration::from_secs(4));
        assert!(!state.voice.visible(start + Duration::from_secs(4)));
    }

    #[test]
    fn a_hold_longer_than_auto_lock_does_not_sleep_on_release() {
        let mut state = voice_state();
        state.settings.auto_lock_s = 30;
        let start = Instant::now();
        state.last_input = start;
        assert!(state.voice_start(start));
        let release = start + Duration::from_secs(45);
        state.voice_stop(true, release);
        assert_eq!(state.last_input, release);
        assert!(!state.idle_dimmed);
    }

    #[test]
    fn a_slip_is_cancelled_and_a_tap_still_sleeps() {
        let mut state = voice_state();
        let start = Instant::now();
        state.power_key_down(start);
        state.power_key_up();
        assert!(state.fade.is_some_and(|fade| fade.out), "a tap sleeps");
        assert!(state.voice.requests.is_empty());

        let mut state = voice_state();
        let now = Instant::now();
        assert!(state.voice_start(now));
        state.voice_stop(true, now + Duration::from_millis(100));
        assert_eq!(
            state.voice.requests,
            [VoiceRequest::Start, VoiceRequest::Cancel]
        );
    }

    #[test]
    fn power_and_volume_open_the_menu_and_drop_the_recording() {
        let mut state = voice_state();
        let start = Instant::now();
        state.power_key_down(start);
        assert!(state.power_chord());
        assert!(state.power.menu.as_ref().is_some_and(|menu| menu.open));
        state.power_key_up();
        assert!(state.fade.is_none());
        assert!(state.power.menu.as_ref().is_some_and(|menu| menu.open));

        let mut state = voice_state();
        state.power_key_down(start);
        state.power_tick(start + HOLD_FOR_VOICE);
        assert!(state.voice_listening());
        assert!(state.power_chord());
        assert!(!state.voice_listening());
        assert_eq!(
            state.voice.requests,
            [VoiceRequest::Start, VoiceRequest::Cancel]
        );
        assert!(state.power.menu.as_ref().is_some_and(|menu| menu.open));
        // A volume key alone is just volume.
        let mut state = voice_state();
        assert!(!state.power_chord());
    }

    #[test]
    fn without_the_hook_the_hold_opens_the_menu() {
        let mut state = ShellState::new(false, 50);
        let start = Instant::now();
        state.power_key_down(start);
        state.power_tick(start + HOLD_FOR_VOICE);
        assert!(!state.voice_listening());
        state.power_tick(start + HOLD_FOR_MENU);
        assert!(state.power.menu.as_ref().is_some_and(|menu| menu.open));
    }

    #[test]
    fn a_hold_from_a_dark_screen_wakes_and_listens_on_the_lock_screen() {
        let mut state = voice_state();
        state.screen_off();
        let start = Instant::now();
        state.power_key_wake(start);
        assert!(state.screen_awake);
        state.power_tick(start + HOLD_FOR_VOICE);
        assert!(state.voice_listening());
        assert!(state.voice.locked);
        state.voice.listening = Some(start);
        state.voice_stop(true, start + Duration::from_secs(2));
        state.power_key_up();
        assert!(state.screen_awake, "the release does not sleep");
        assert_eq!(state.view, View::Locked);
        assert_eq!(state.lock_face, crate::model::LockFace::Passcode);
        assert_eq!(
            state.voice.requests,
            [VoiceRequest::Start, VoiceRequest::Send]
        );

        // A tap on a dark screen only wakes it.
        let mut state = voice_state();
        state.screen_off();
        state.power_key_wake(Instant::now());
        state.power_key_up();
        assert!(state.screen_awake);
        assert!(state.voice.requests.is_empty());
    }

    #[test]
    fn the_orb_follows_the_voice_fast_up_and_slowly_down() {
        let mut state = voice_state();
        let start = Instant::now();
        state.voice_start(start);
        state.voice.advance_level(None, start);
        state
            .voice
            .advance_level(Some(1.0), start + Duration::from_millis(100));
        let loud = state.voice.level;
        assert!(loud > 0.8, "rises within 100 ms: {loud}");
        assert!(state.voice.halo_scale(start) > state.voice.core_scale());
        state
            .voice
            .advance_level(Some(0.0), start + Duration::from_millis(200));
        assert!(state.voice.level > 0.5 * loud, "falls back slowly");
        // Done listening: back to rest.
        state.voice_stop(false, start + Duration::from_millis(300));
        state
            .voice
            .advance_level(None, start + Duration::from_secs(3));
        assert!(state.voice.level < 0.01);
    }

    #[test]
    fn open_waits_for_unlock() {
        let mut state = ShellState::new(true, 50);
        state.open_target(AppId::Exocortex, "1793-abc".into());
        assert!(state.open_target.is_none());
        assert_eq!(state.view, View::Locked);
        assert_eq!(state.lock_face, crate::model::LockFace::Passcode);
        // What submit_pin does on the right passcode.
        state.view = View::Launcher;
        state.voice_unlocked();
        assert_eq!(state.view, AppId::Exocortex.view());
        assert_eq!(
            state.open_target.as_ref().map(|open| open.target.as_str()),
            Some("1793-abc")
        );
        assert!(state.voice.open_after_unlock.is_none());
    }
}
