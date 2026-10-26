//! moon-notifyd's I/O: the socket, timers, alert sound and vibrator around the
//! pure `Engine`. One thread blocking in poll(2); nothing polls on a timer
//! while idle (docs/notifications-protocol.md §7).

use std::collections::VecDeque;
use std::error::Error;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use super::engine::{Effect, Engine};
use super::model::State;
use super::sound::{self, Pattern, Tone};

pub struct Paths {
    pub run_dir: PathBuf,
    pub socket: PathBuf,
    pub state_dir: PathBuf,
    pub alert_fifo: PathBuf,
    pub config: PathBuf,
    pub group: PathBuf,
}

impl Paths {
    /// The real paths, or all of them under `MOON_NOTIFY_ROOT` (tests and
    /// development off the phone).
    pub fn from_environment() -> Self {
        match std::env::var_os("MOON_NOTIFY_ROOT") {
            Some(root) => Self::under(Path::new(&root)),
            None => Self::system(),
        }
    }

    pub fn under(root: &Path) -> Self {
        let system = Self::system();
        let join = |path: PathBuf| root.join(path.strip_prefix("/").unwrap_or(&path));
        Self {
            run_dir: join(system.run_dir),
            socket: join(system.socket),
            state_dir: join(system.state_dir),
            alert_fifo: join(system.alert_fifo),
            config: join(system.config),
            group: join(system.group),
        }
    }

    pub fn system() -> Self {
        Self {
            run_dir: "/run/moon-notify".into(),
            socket: "/run/moon-notify/notify.sock".into(),
            state_dir: "/var/lib/moon-notify".into(),
            alert_fifo: "/run/moon-audio/alerts".into(),
            config: "/etc/a26-shell/config.json".into(),
            group: "/etc/group".into(),
        }
    }
}

const MAX_LINE: usize = 16 * 1024;
const MAX_BACKLOG: usize = 4 * 1024 * 1024;
const VIBRATE_PULSE_MS: u32 = 500;
const VIBRATE_PERIOD: Duration = Duration::from_secs(1);
/// moon-audiod closes the FIFO for ~250 ms after each sound while its stream
/// drains, so an open can fail briefly even when audio is up: retry soon.
const FIFO_RETRY: Duration = Duration::from_millis(250);
/// A notification chime that cannot start within this long is dropped.
const CHIME_PATIENCE: Duration = Duration::from_secs(2);
/// 100 ms of audio per refill; the pipe holds ~340 ms.
const RENDER_FRAMES: usize = 4800;
/// Audio is produced in real time, at most this far ahead of the clock (the
/// rest waits in the pipe), so a fast reader can never make us spin.
const AHEAD_FRAMES: u64 = 48_000 * 35 / 100;
/// Refill when at least this much budget has accrued (50 ms).
const REFILL_FRAMES: u64 = 2400;

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

fn now_s() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

pub fn run(paths: Paths) -> Result<(), Box<dyn Error>> {
    // SAFETY: installing async-signal-safe handlers that only store a flag.
    unsafe {
        libc::signal(
            libc::SIGTERM,
            on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGINT,
            on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    set_timezone(&paths.config);
    fs::create_dir_all(&paths.state_dir)?;
    fs::set_permissions(&paths.state_dir, fs::Permissions::from_mode(0o700))?;
    let state = load_state(&paths.state_dir);
    let (engine, startup_events) = Engine::start(state, now_s());
    let group = group_id(&paths.group, "moon-notify");
    let listener = bind(&paths, group)?;
    let mut daemon = Daemon {
        engine,
        paths,
        group,
        listener,
        clients: Vec::new(),
        alarm_timer: AlarmTimer::new()?,
        vibe_timer: timerfd(libc::CLOCK_MONOTONIC)?,
        vibrating: false,
        player: Player::default(),
        last_wake_written: None,
    };
    eprintln!(
        "moon-notifyd ready socket={} clock={} group={:?}",
        daemon.paths.socket.display(),
        daemon.alarm_timer.clock_name,
        group
    );
    daemon.broadcast(startup_events);
    daemon.after_change()?;
    while !STOP.load(Ordering::SeqCst) {
        daemon.poll_once()?;
    }
    sound::stop_vibration();
    daemon.save()?;
    let _ = fs::remove_file(&daemon.paths.socket);
    eprintln!("moon-notifyd stopped");
    Ok(())
}

struct Client {
    stream: UnixStream,
    input: Vec<u8>,
    output: VecDeque<u8>,
    subscribed: bool,
    closing: bool,
}

struct Daemon {
    engine: Engine,
    paths: Paths,
    group: Option<u32>,
    listener: UnixListener,
    clients: Vec<Client>,
    alarm_timer: AlarmTimer,
    vibe_timer: OwnedFd,
    vibrating: bool,
    player: Player,
    last_wake_written: Option<Option<i64>>,
}

impl Daemon {
    fn poll_once(&mut self) -> Result<(), Box<dyn Error>> {
        let mut fds = vec![
            pollfd(self.listener.as_raw_fd(), libc::POLLIN),
            pollfd(self.alarm_timer.fd.as_raw_fd(), libc::POLLIN),
            pollfd(self.vibe_timer.as_raw_fd(), libc::POLLIN),
        ];
        let fifo_index = self
            .player
            .fifo
            .as_ref()
            .filter(|_| self.player.can_write())
            .map(|fifo| {
                fds.push(pollfd(fifo.as_raw_fd(), libc::POLLOUT));
                fds.len() - 1
            });
        let first_client = fds.len();
        for client in &self.clients {
            let mut events = libc::POLLIN;
            if !client.output.is_empty() {
                events |= libc::POLLOUT;
            }
            fds.push(pollfd(client.stream.as_raw_fd(), events));
        }
        let timeout = self.player.wake_at().map_or(-1, |at| {
            at.saturating_duration_since(Instant::now()).as_millis() as i32 + 1
        });
        // SAFETY: fds is a valid, initialized array of the given length.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == ErrorKind::Interrupted {
                return Ok(());
            }
            return Err(error.into());
        }

        if fds[1].revents & libc::POLLIN != 0 {
            let events = match self.alarm_timer.read() {
                TimerRead::Expired => self.engine.tick(now_s()),
                TimerRead::ClockChanged => {
                    eprintln!("wall clock changed; rescheduling");
                    self.engine.clock_changed(now_s())
                }
                TimerRead::Spurious => Vec::new(),
            };
            self.broadcast(events);
        }
        if fds[2].revents & libc::POLLIN != 0 {
            drain_timerfd(self.vibe_timer.as_raw_fd());
            if self.vibrating {
                let _ = sound::vibrate(VIBRATE_PULSE_MS);
            }
        }
        if fifo_index.is_some_and(|index| fds[index].revents != 0) || self.player.refill_due() {
            self.player.pump();
        }
        let mut index = first_client;
        let mut client = 0;
        while client < self.clients.len() {
            let revents = fds[index].revents;
            index += 1;
            if revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                self.read_client(client);
            }
            if revents & libc::POLLOUT != 0 {
                flush(&mut self.clients[client]);
            }
            client += 1;
        }
        if fds[0].revents & libc::POLLIN != 0 {
            self.accept();
        }
        self.clients.retain(|c| !(c.closing && c.output.is_empty()));
        self.after_change()
    }

    fn accept(&mut self) {
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    let _ = stream.set_nonblocking(true);
                    let allowed = peer_allowed(&stream, self.group);
                    let mut client = Client {
                        stream,
                        input: Vec::new(),
                        output: VecDeque::new(),
                        subscribed: false,
                        closing: false,
                    };
                    if !allowed {
                        queue(
                            &mut client,
                            &json!({"id": null, "ok": false, "error": {
                            "code": "permission_denied", "message": "root or moon-notify group required"}}),
                        );
                        client.closing = true;
                        flush(&mut client);
                    }
                    self.clients.push(client);
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) => {
                    eprintln!("accept failed: {error}");
                    break;
                }
            }
        }
    }

    fn read_client(&mut self, index: usize) {
        let mut buffer = [0u8; 8192];
        loop {
            let client = &mut self.clients[index];
            if client.closing {
                return;
            }
            match client.stream.read(&mut buffer) {
                Ok(0) => {
                    client.closing = true;
                    client.output.clear();
                    return;
                }
                Ok(count) => client.input.extend_from_slice(&buffer[..count]),
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(_) => {
                    client.closing = true;
                    client.output.clear();
                    return;
                }
            }
        }
        while let Some(end) = self.clients[index].input.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.clients[index].input.drain(..=end).collect();
            self.request(index, &line[..line.len() - 1]);
        }
        let client = &mut self.clients[index];
        if client.input.len() > MAX_LINE {
            client.closing = true;
            client.output.clear();
        }
    }

    fn request(&mut self, index: usize, line: &[u8]) {
        if line.len() > MAX_LINE {
            self.clients[index].closing = true;
            return;
        }
        if let Err(message) = super::json_strict::check(line) {
            queue(
                &mut self.clients[index],
                &json!({"id": null, "ok": false, "error": {
                "code": "invalid_request", "message": message}}),
            );
            return;
        }
        let parsed: Option<(u64, String, serde_json::Map<String, Value>)> =
            serde_json::from_slice::<Value>(line)
                .ok()
                .and_then(|value| match value {
                    Value::Object(mut map) => {
                        let id = map.remove("id")?.as_u64().filter(|id| *id != 0)?;
                        let cmd = map.remove("cmd")?.as_str()?.to_owned();
                        Some((id, cmd, map))
                    }
                    _ => None,
                });
        let Some((id, cmd, args)) = parsed else {
            queue(
                &mut self.clients[index],
                &json!({"id": null, "ok": false, "error": {
                "code": "invalid_request", "message": "expected a JSON object with a non-zero id and cmd"}}),
            );
            return;
        };
        let now = now_s();
        if cmd == "subscribe" {
            let response = if args.is_empty() {
                json!({"id": id, "ok": true, "result": {}})
            } else {
                json!({"id": id, "ok": false, "error": {"code": "invalid_argument", "message": "subscribe takes no arguments"}})
            };
            let client = &mut self.clients[index];
            queue(client, &response);
            if args.is_empty() {
                client.subscribed = true;
                let snapshot = self.engine.snapshot(now);
                queue(&mut self.clients[index], &snapshot);
            }
            return;
        }
        let (reply, events) = self.engine.handle(&cmd, args, now);
        let response = match reply {
            Ok(result) => json!({"id": id, "ok": true, "result": result}),
            Err(failure) => {
                json!({"id": id, "ok": false, "error": {"code": failure.code, "message": failure.message}})
            }
        };
        queue(&mut self.clients[index], &response);
        self.broadcast(events);
    }

    fn broadcast(&mut self, events: Vec<Value>) {
        for event in &events {
            for client in self
                .clients
                .iter_mut()
                .filter(|c| c.subscribed && !c.closing)
            {
                queue(client, event);
            }
        }
    }

    /// Applies everything a request or tick may have changed: effects, the
    /// ring sound and vibration, the alarm timer, persistence.
    fn after_change(&mut self) -> Result<(), Box<dyn Error>> {
        for effect in self.engine.take_effects() {
            match effect {
                Effect::Chime => self.player.chime(),
                Effect::Buzz(ms) if !self.vibrating => {
                    let _ = sound::vibrate(ms);
                }
                Effect::Buzz(_) => {}
            }
        }
        let volume = self.engine.state.settings.alert_volume;
        self.player.set_ring(self.engine.ring_sound(), volume);
        self.player.ensure_open(&self.paths.alert_fifo);
        let vibrate = self.engine.ring_vibrates();
        if vibrate != self.vibrating {
            self.vibrating = vibrate;
            if vibrate {
                let _ = sound::vibrate(VIBRATE_PULSE_MS);
                arm_periodic(self.vibe_timer.as_raw_fd(), VIBRATE_PERIOD)?;
            } else {
                arm_periodic(self.vibe_timer.as_raw_fd(), Duration::ZERO)?;
                sound::stop_vibration();
            }
        }
        self.alarm_timer.arm(self.engine.next_due(), now_s())?;
        if self.engine.dirty {
            self.save()?;
        }
        for client in &mut self.clients {
            if client.output.len() > MAX_BACKLOG {
                client.closing = true;
                client.output.clear();
            }
        }
        Ok(())
    }

    fn save(&mut self) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(&self.engine.state).map_err(io::Error::other)?;
        atomic_write(&self.paths.state_dir.join("state.json"), &bytes, 0o600)?;
        let wake = self.engine.next_wake();
        if self.last_wake_written != Some(wake) {
            let text = wake.map_or_else(String::new, |w| format!("{w}\n"));
            atomic_write(
                &self.paths.state_dir.join("next-wake"),
                text.as_bytes(),
                0o644,
            )?;
            self.last_wake_written = Some(wake);
        }
        self.engine.dirty = false;
        Ok(())
    }
}

// ---- sound ----------------------------------------------------------------

#[derive(Default)]
struct Player {
    fifo: Option<File>,
    ring: Option<(Pattern, Tone)>,
    chime: Option<Tone>,
    pending: Vec<u8>,
    retry_at: Option<Instant>,
    volume: u8,
    /// Real-time pacing of what has been produced since the FIFO opened.
    clock: Option<(Instant, u64)>,
    /// When the pending chime was requested (it waits at most CHIME_PATIENCE).
    chime_since: Option<Instant>,
}

impl Player {
    fn wants_output(&self) -> bool {
        self.ring.is_some() || self.chime.is_some() || !self.pending.is_empty()
    }

    /// Frames we may still produce now without running ahead of real time.
    fn budget(&self) -> u64 {
        let Some((started, sent)) = self.clock else {
            return AHEAD_FRAMES;
        };
        let elapsed = started.elapsed().as_micros() as u64 * 48 / 1000;
        (elapsed + AHEAD_FRAMES).saturating_sub(sent)
    }

    /// Whether poll should wait for the FIFO to become writable.
    fn can_write(&self) -> bool {
        !self.pending.is_empty() || (self.wants_output() && self.budget() >= REFILL_FRAMES)
    }

    fn refill_due(&self) -> bool {
        self.fifo.is_some()
            && self.pending.is_empty()
            && self.wants_output()
            && self.budget() >= REFILL_FRAMES
    }

    /// When poll must wake: the next refill, or the next FIFO open retry.
    fn wake_at(&self) -> Option<Instant> {
        if !self.wants_output() {
            return None;
        }
        if self.fifo.is_none() {
            return self.retry_at;
        }
        if !self.pending.is_empty() {
            return None;
        }
        let budget = self.budget();
        let missing = REFILL_FRAMES.saturating_sub(budget);
        Some(Instant::now() + Duration::from_micros(missing * 1000 / 48))
    }

    fn chime(&mut self) {
        if self.ring.is_none() {
            self.chime = Tone::new(Pattern::Chime, self.volume.max(1));
            self.chime_since = Some(Instant::now());
        }
    }

    fn set_ring(&mut self, sound: Option<super::model::Sound>, volume: u8) {
        self.volume = volume;
        let wanted = sound.map(Pattern::Ring);
        if self.ring.as_ref().map(|(p, _)| *p) == wanted {
            return;
        }
        self.ring = wanted.and_then(|p| Tone::new(p, volume).map(|tone| (p, tone)));
        if self.ring.is_some() {
            self.chime = None;
        } else {
            // Stop now: drop what we hold and close, so the daemon's queued
            // audio ends within the pipe's ~0.3 s.
            self.pending.clear();
            self.fifo = None;
            self.clock = None;
        }
    }

    /// Opens the alert FIFO when there is something to play. O_NONBLOCK makes
    /// the open fail (ENXIO) instead of hanging when moon-audiod is not
    /// reading; ringing then stays vibration-only and retries every 250 ms.
    fn ensure_open(&mut self, path: &Path) {
        if !self.wants_output() {
            self.fifo = None;
            self.clock = None;
            self.retry_at = None;
            return;
        }
        if self.fifo.is_some() || self.retry_at.is_some_and(|at| Instant::now() < at) {
            return;
        }
        match OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
        {
            Ok(file) => {
                self.fifo = Some(file);
                self.retry_at = None;
                self.clock = Some((Instant::now(), 0));
                self.pump();
            }
            Err(_) => {
                self.retry_at = Some(Instant::now() + FIFO_RETRY);
                // A one-shot chime is worth a short wait, not a long one.
                if self
                    .chime_since
                    .is_some_and(|since| since.elapsed() >= CHIME_PATIENCE)
                {
                    self.chime = None;
                    self.chime_since = None;
                    self.pending.clear();
                }
            }
        }
    }

    /// Fills the pipe as far as it takes without blocking.
    fn pump(&mut self) {
        if self.fifo.is_none() {
            return;
        }
        loop {
            if self.pending.is_empty() {
                if !self.wants_output() {
                    // Done playing: close so moon-audiod sees the end.
                    self.fifo = None;
                    self.clock = None;
                    return;
                }
                let frames = self.budget().min(RENDER_FRAMES as u64) as usize;
                if frames < REFILL_FRAMES as usize {
                    return;
                }
                if let Some((_, tone)) = self.ring.as_mut() {
                    tone.render(frames, &mut self.pending);
                } else if let Some(tone) = self.chime.as_mut() {
                    tone.render(frames, &mut self.pending);
                    if tone.finished() {
                        self.chime = None;
                    }
                }
                if let Some((_, sent)) = self.clock.as_mut() {
                    *sent += (self.pending.len() / 4) as u64;
                }
                if self.pending.is_empty() {
                    continue;
                }
            }
            let Some(fifo) = self.fifo.as_mut() else {
                return;
            };
            match fifo.write(&self.pending) {
                Ok(count) => {
                    self.pending.drain(..count);
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => return,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(_) => {
                    // Reader went away (moon-audiod restarted): reopen later.
                    self.fifo = None;
                    self.clock = None;
                    self.pending.clear();
                    self.retry_at = Some(Instant::now() + FIFO_RETRY);
                    return;
                }
            }
        }
    }
}

// ---- timers ---------------------------------------------------------------

enum TimerRead {
    Expired,
    ClockChanged,
    Spurious,
}

struct AlarmTimer {
    fd: OwnedFd,
    clock_name: &'static str,
}

impl AlarmTimer {
    /// CLOCK_REALTIME_ALARM wakes a suspended system through the RTC; it
    /// needs CAP_WAKE_ALARM and RTC alarm support, else plain REALTIME.
    fn new() -> io::Result<Self> {
        match timerfd(libc::CLOCK_REALTIME_ALARM) {
            Ok(fd) => Ok(Self {
                fd,
                clock_name: "realtime-alarm",
            }),
            Err(_) => Ok(Self {
                fd: timerfd(libc::CLOCK_REALTIME)?,
                clock_name: "realtime",
            }),
        }
    }

    /// Absolute, cancelled when the wall clock is set, so an NTP step makes
    /// the daemon reschedule instead of firing late or early. With nothing
    /// due it still waits a year, to keep noticing clock changes.
    fn arm(&self, due: Option<i64>, now: i64) -> io::Result<()> {
        let at = due.unwrap_or(now + 365 * 86400).max(now - 1).max(1);
        let spec = libc::itimerspec {
            it_interval: libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
            it_value: libc::timespec {
                tv_sec: at as libc::time_t,
                tv_nsec: 0,
            },
        };
        // SAFETY: valid fd and a fully initialized itimerspec.
        let rc = unsafe {
            libc::timerfd_settime(
                self.fd.as_raw_fd(),
                libc::TFD_TIMER_ABSTIME | libc::TFD_TIMER_CANCEL_ON_SET,
                &spec,
                std::ptr::null_mut(),
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn read(&self) -> TimerRead {
        let mut expirations = 0u64;
        // SAFETY: reads 8 bytes into a u64.
        let rc = unsafe {
            libc::read(
                self.fd.as_raw_fd(),
                (&mut expirations as *mut u64).cast(),
                std::mem::size_of::<u64>(),
            )
        };
        if rc == 8 {
            TimerRead::Expired
        } else if io::Error::last_os_error().raw_os_error() == Some(libc::ECANCELED) {
            TimerRead::ClockChanged
        } else {
            TimerRead::Spurious
        }
    }
}

fn timerfd(clock: libc::clockid_t) -> io::Result<OwnedFd> {
    // SAFETY: plain syscall; the returned fd is owned below.
    let fd = unsafe { libc::timerfd_create(clock, libc::TFD_NONBLOCK | libc::TFD_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is a fresh descriptor owned by nobody else.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn arm_periodic(fd: RawFd, period: Duration) -> io::Result<()> {
    let value = libc::timespec {
        tv_sec: period.as_secs() as libc::time_t,
        tv_nsec: period.subsec_nanos() as libc::c_long,
    };
    let spec = libc::itimerspec {
        it_interval: value,
        it_value: value,
    };
    // SAFETY: valid fd and itimerspec.
    if unsafe { libc::timerfd_settime(fd, 0, &spec, std::ptr::null_mut()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn drain_timerfd(fd: RawFd) {
    let mut value = 0u64;
    // SAFETY: reads 8 bytes into a u64; errors (EAGAIN) are ignored.
    unsafe {
        libc::read(fd, (&mut value as *mut u64).cast(), 8);
    }
}

// ---- sockets and files ----------------------------------------------------

fn bind(paths: &Paths, group: Option<u32>) -> io::Result<UnixListener> {
    fs::create_dir_all(&paths.run_dir)?;
    chown(&paths.run_dir, group)?;
    fs::set_permissions(&paths.run_dir, fs::Permissions::from_mode(0o750))?;
    match fs::remove_file(&paths.socket) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(&paths.socket)?;
    chown(&paths.socket, group)?;
    fs::set_permissions(&paths.socket, fs::Permissions::from_mode(0o660))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

fn chown(path: &Path, group: Option<u32>) -> io::Result<()> {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        return Ok(());
    }
    std::os::unix::fs::chown(path, Some(0), Some(group.unwrap_or(0)))
}

fn peer_allowed(stream: &UnixStream, group: Option<u32>) -> bool {
    let mut cred = libc::ucred {
        pid: 0,
        uid: u32::MAX,
        gid: u32::MAX,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: cred is valid writable storage of `length` bytes.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    if rc != 0 {
        return false;
    }
    // SAFETY: geteuid has no preconditions.
    if cred.uid == 0 || cred.uid == unsafe { libc::geteuid() } {
        return true;
    }
    let Some(group) = group else {
        return false;
    };
    if cred.gid == group {
        return true;
    }
    fs::read_to_string(format!("/proc/{}/status", cred.pid))
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("Groups:"))
                .map(|groups| groups.split_whitespace().any(|g| g.parse() == Ok(group)))
        })
        .unwrap_or(false)
}

fn group_id(path: &Path, name: &str) -> Option<u32> {
    fs::read_to_string(path).ok()?.lines().find_map(|line| {
        let mut fields = line.split(':');
        (fields.next() == Some(name))
            .then(|| fields.nth(1)?.parse().ok())
            .flatten()
    })
}

fn queue(client: &mut Client, value: &Value) {
    if client.closing {
        return;
    }
    if let Ok(mut line) = serde_json::to_vec(value) {
        line.push(b'\n');
        client.output.extend(line);
    }
    flush(client);
}

fn flush(client: &mut Client) {
    while !client.output.is_empty() {
        let (front, _) = client.output.as_slices();
        match client.stream.write(front) {
            Ok(0) => break,
            Ok(count) => {
                client.output.drain(..count);
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(_) => {
                client.closing = true;
                client.output.clear();
                break;
            }
        }
    }
}

fn pollfd(fd: RawFd, events: libc::c_short) -> libc::pollfd {
    libc::pollfd {
        fd,
        events,
        revents: 0,
    }
}

fn load_state(dir: &Path) -> State {
    let path = dir.join("state.json");
    match fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(state) => state,
            Err(error) => {
                let aside = dir.join(format!("state.json.bad-{}", now_s()));
                eprintln!("state unreadable ({error}); kept as {}", aside.display());
                let _ = fs::rename(&path, aside);
                State::default()
            }
        },
        Err(_) => State::default(),
    }
}

fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let temporary = path.with_extension("new");
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))?;
    fs::rename(&temporary, path)
}

/// Uses Moon's configured time zone for alarm wall-clock times.
fn set_timezone(config: &Path) {
    let zone = fs::read(config)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|value| value.get("timezone")?.as_str().map(str::to_owned))
        .filter(|zone| !zone.is_empty() && zone.len() <= 64 && zone.is_ascii());
    if let Some(zone) = zone {
        // SAFETY: single-threaded at startup; nothing reads the environment
        // concurrently.
        unsafe { std::env::set_var("TZ", zone) };
    }
    unsafe extern "C" {
        fn tzset();
    }
    // SAFETY: tzset only reads TZ.
    unsafe { tzset() };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_lookup() {
        let dir = std::env::temp_dir().join(format!("notifyd-group-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("group");
        fs::write(&file, "root:x:0:\nmoon-notify:x:1801:alice\n").unwrap();
        assert_eq!(group_id(&file, "moon-notify"), Some(1801));
        assert_eq!(group_id(&file, "nobody"), None);
        let _ = fs::remove_dir_all(dir);
    }
}
