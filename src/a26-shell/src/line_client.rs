//! A client for Moon's JSON-lines daemons (`moon-notifyd`,
//! `moon-telephonyd`): one UTF-8 JSON object per line over a Unix stream
//! socket, requests `{"id", "cmd", …}` answered in order, events
//! (`{"event", …}`) after `subscribe`. The socket is non-blocking and polled
//! with the rest of Moon's event loop; a missing or stalled daemon never
//! holds up the UI. After every (re)connect the client subscribes.

use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// The protocols' line limit; a longer line from the daemon is a protocol
/// error and drops the connection.
const MAX_LINE: usize = 16 * 1024;
/// Commands Moon could not hand to the socket yet; more means the daemon has
/// stopped reading.
const MAX_QUEUED_BYTES: usize = 64 * 1024;
const RETRY_FIRST: Duration = Duration::from_millis(500);
const RETRY_MAX: Duration = Duration::from_secs(5);

/// A line from the daemon: an event, or the failure of one of Moon's
/// requests (Moon acts on events; successful answers are dropped).
#[derive(Debug)]
pub enum Line {
    Event(Value),
    Failed { cmd: String, code: String },
}

pub struct LineClient {
    /// The daemon's name, for logs.
    name: &'static str,
    path: PathBuf,
    stream: Option<UnixStream>,
    input: Vec<u8>,
    output: Vec<u8>,
    next_id: u64,
    /// Requests awaiting their response: id, command, deadline.
    pending: Vec<(u64, String, Instant)>,
    next_attempt: Instant,
    retry: Duration,
    logged_unavailable: bool,
    /// How long a request may wait for its answer before the connection is
    /// considered lost and rebuilt.
    timeout: Duration,
}

impl LineClient {
    pub fn new(name: &'static str, path: impl AsRef<Path>, timeout: Duration) -> Self {
        Self {
            name,
            path: path.as_ref().to_path_buf(),
            stream: None,
            input: Vec::new(),
            output: Vec::new(),
            next_id: 1,
            pending: Vec::new(),
            next_attempt: Instant::now(),
            retry: RETRY_FIRST,
            logged_unavailable: false,
            timeout,
        }
    }

    pub fn connected(&self) -> bool {
        self.stream.is_some()
    }

    /// The descriptor to poll, and whether Moon has output waiting for it.
    pub fn poll_fd(&self) -> Option<(RawFd, bool)> {
        self.stream
            .as_ref()
            .map(|stream| (stream.as_raw_fd(), !self.output.is_empty()))
    }

    /// When the loop must wake for this client (a reconnect attempt or a
    /// request deadline).
    pub fn next_deadline(&self) -> Option<Instant> {
        match &self.stream {
            None => Some(self.next_attempt),
            Some(_) => self.pending.iter().map(|(_, _, at)| *at).min(),
        }
    }

    /// Queues a command (`cmd` plus its arguments), answered within the
    /// client's timeout.
    pub fn send(&mut self, cmd: &str, arguments: Value) {
        self.send_waiting(cmd, arguments, self.timeout);
    }

    /// Queues a command that may take `timeout` to be answered (one that
    /// waits for a modem, say).
    pub fn send_waiting(&mut self, cmd: &str, mut arguments: Value, timeout: Duration) {
        if self.stream.is_none() {
            eprintln!("{} unavailable; dropped {cmd}", self.name);
            return;
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        if !arguments.is_object() {
            arguments = json!({});
        }
        arguments["id"] = json!(id);
        arguments["cmd"] = json!(cmd);
        self.output
            .extend_from_slice(arguments.to_string().as_bytes());
        self.output.push(b'\n');
        self.pending
            .push((id, cmd.to_owned(), Instant::now() + timeout));
        if self.output.len() > MAX_QUEUED_BYTES {
            self.disconnect("output backlog");
        }
    }

    /// Connects when due, flushes output, and returns everything received.
    /// Never blocks.
    pub fn service(&mut self, now: Instant) -> Vec<Line> {
        let mut incoming = Vec::new();
        if self.stream.is_none() {
            if now < self.next_attempt {
                return incoming;
            }
            match connect_nonblocking(&self.path) {
                Ok(stream) => {
                    eprintln!("{} connected", self.name);
                    self.stream = Some(stream);
                    self.retry = RETRY_FIRST;
                    self.logged_unavailable = false;
                    self.input.clear();
                    self.output.clear();
                    self.pending.clear();
                    self.send("subscribe", json!({}));
                }
                Err(error) => {
                    if !self.logged_unavailable {
                        eprintln!("{} unavailable ({error}); retrying", self.name);
                        self.logged_unavailable = true;
                    }
                    self.next_attempt = now + self.retry;
                    self.retry = (self.retry * 2).min(RETRY_MAX);
                    return incoming;
                }
            }
        }
        if let Err(error) = self.flush() {
            self.disconnect(&error.to_string());
            return incoming;
        }
        if let Err(error) = self.read_lines(&mut incoming) {
            self.disconnect(&error.to_string());
            return incoming;
        }
        if self.pending.iter().any(|(_, _, at)| now >= *at) {
            self.disconnect("request timed out");
        }
        incoming
    }

    fn flush(&mut self) -> io::Result<()> {
        let Some(stream) = self.stream.as_mut() else {
            return Ok(());
        };
        while !self.output.is_empty() {
            match stream.write(&self.output) {
                Ok(0) => return Err(io::Error::other("socket closed")),
                Ok(written) => {
                    self.output.drain(..written);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn read_lines(&mut self, incoming: &mut Vec<Line>) -> io::Result<()> {
        let Some(stream) = self.stream.as_mut() else {
            return Ok(());
        };
        let mut chunk = [0_u8; 8192];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => return Err(io::Error::other("daemon closed the connection")),
                Ok(read) => self.input.extend_from_slice(&chunk[..read]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        while let Some(end) = self.input.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.input.drain(..=end).collect();
            if let Some(item) = self.parse_line(&line[..line.len() - 1]) {
                incoming.push(item);
            }
        }
        if self.input.len() > MAX_LINE {
            return Err(io::Error::other("line too long"));
        }
        Ok(())
    }

    fn parse_line(&mut self, line: &[u8]) -> Option<Line> {
        let value: Value = match serde_json::from_slice(line) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("{} sent invalid JSON: {error}", self.name);
                return None;
            }
        };
        if value.get("event").is_some() {
            return Some(Line::Event(value));
        }
        let id = value.get("id").and_then(Value::as_u64)?;
        let position = self
            .pending
            .iter()
            .position(|(pending, _, _)| *pending == id)?;
        let (_, cmd, _) = self.pending.remove(position);
        if value.get("ok").and_then(Value::as_bool) == Some(true) {
            return None;
        }
        let code = value
            .pointer("/error/code")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        Some(Line::Failed { cmd, code })
    }

    fn disconnect(&mut self, why: &str) {
        if self.stream.take().is_some() {
            eprintln!("{} connection lost: {why}", self.name);
        }
        self.input.clear();
        self.output.clear();
        self.pending.clear();
        self.next_attempt = Instant::now() + self.retry;
    }
}

/// A non-blocking connect: a local socket either accepts at once or is
/// refused; a full backlog (`EAGAIN`) counts as unavailable.
fn connect_nonblocking(path: &Path) -> io::Result<UnixStream> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: plain socket(2) call; the descriptor is owned below.
    let fd = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is a fresh socket owned by nobody else.
    let stream = unsafe { UnixStream::from_raw_fd(fd) };
    // SAFETY: sockaddr_un is plain old data; zeroed is a valid value.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    if bytes.len() >= address.sun_path.len() {
        return Err(io::Error::other("socket path too long"));
    }
    for (slot, byte) in address.sun_path.iter_mut().zip(bytes) {
        *slot = *byte as libc::c_char;
    }
    let length = std::mem::size_of::<libc::sa_family_t>() + bytes.len() + 1;
    // SAFETY: address is initialized and length is within its size.
    let result = unsafe {
        libc::connect(
            fd,
            (&address as *const libc::sockaddr_un).cast(),
            length as libc::socklen_t,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(stream)
}
