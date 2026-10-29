//! The system clipboard: X11's CLIPBOARD selection, kept by Moon.
//!
//! Apps copy the way X apps do (they take CLIPBOARD: the Browser's Copy) or
//! with `clipboard set` on the control socket (apps without selection code
//! of their own). Either way Moon takes the text at once, while the app that
//! copied is still in front, and from then on owns CLIPBOARD itself: the text
//! outlives that app being frozen in the background or closed, and every X
//! app's own Paste, the keyboard's paste key and `clipboard get` are answered
//! by Moon. Text only: a copy with no text form (an image) stays with its
//! app, and the paste key has nothing to type.
//!
//! All of it runs on a thread with its own X connection, which waits for
//! replies and events as X clients do; Moon's event loop and its connection
//! never wait (scripts/a26-shell/build.sh's round-trip guard skips this file
//! for that reason). The text is never logged.

use std::error::Error;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xfixes::{ConnectionExt as _, SelectionEventMask};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, EXPOSE_EVENT, EventMask, ExposeEvent,
    PropMode, Property, SELECTION_NOTIFY_EVENT, SelectionNotifyEvent, SelectionRequestEvent,
    Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, CURRENT_TIME, NONE};

/// The most text the clipboard holds (bytes of UTF-8); one property write
/// carries it (Xorg's BIG-REQUESTS limit is 16 MiB).
pub const MAX_TEXT: usize = 1 << 20;
/// An app that does not answer a conversion by then keeps its copy.
const FETCH_TIMEOUT: Duration = Duration::from_secs(2);

/// Moon's handle on the clipboard thread.
pub struct Clipboard {
    shared: Arc<Mutex<Shared>>,
    wake: UnixStream,
}

#[derive(Default)]
struct Shared {
    text: Option<Arc<str>>,
    /// `clipboard set` happened: the thread takes CLIPBOARD for `text`.
    own: bool,
}

impl Clipboard {
    /// Starts the clipboard thread on its own connection to `DISPLAY`.
    /// `keyboard_window` (Moon's keyboard) gets an Expose whenever the
    /// clipboard gains or loses text, so the paste key repaints.
    pub fn start(keyboard_window: Window) -> Result<Self, Box<dyn Error>> {
        let (conn, screen) = RustConnection::connect(None)?;
        let root = conn.setup().roots[screen].root;
        let (wake, wake_rx) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        wake_rx.set_nonblocking(true)?;
        let shared = Arc::new(Mutex::new(Shared::default()));
        let mut worker = Worker::new(conn, root, keyboard_window, shared.clone())?;
        std::thread::Builder::new()
            .name("clipboard".into())
            .spawn(move || {
                if let Err(error) = worker.run(&wake_rx) {
                    eprintln!("clipboard: stopped: {error}");
                }
            })?;
        Ok(Self { shared, wake })
    }

    /// The clipboard's text, if it holds any.
    pub fn text(&self) -> Option<Arc<str>> {
        lock(&self.shared).text.clone()
    }

    pub fn has_text(&self) -> bool {
        lock(&self.shared).text.is_some()
    }

    /// `clipboard set`: the text is the clipboard's now (Moon takes
    /// CLIPBOARD for it). Longer than `MAX_TEXT` is refused.
    pub fn set(&self, text: &str) -> Result<(), &'static str> {
        if text.len() > MAX_TEXT {
            return Err("clipboard text is over 1 MiB");
        }
        {
            let mut shared = lock(&self.shared);
            shared.text = (!text.is_empty()).then(|| Arc::from(text));
            shared.own = true;
        }
        use std::io::Write as _;
        // A full socket already has a wake-up pending.
        let _ = (&self.wake).write(&[1]);
        Ok(())
    }
}

fn lock(shared: &Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
    shared
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct Atoms {
    clipboard: Atom,
    targets: Atom,
    utf8: Atom,
    text: Atom,
    plain_utf8: Atom,
    plain: Atom,
    incr: Atom,
    /// Where conversions are delivered on Moon's window.
    property: Atom,
}

/// A conversion of another app's copy, in flight.
struct Fetch {
    /// The copy's own time: Moon takes CLIPBOARD as of it, so a newer copy
    /// made meanwhile is never overwritten.
    timestamp: u32,
    target: Atom,
    started: Instant,
    /// INCR: the chunks so far.
    chunks: Option<Vec<u8>>,
}

struct Worker {
    conn: RustConnection,
    window: Window,
    keyboard_window: Window,
    atoms: Atoms,
    shared: Arc<Mutex<Shared>>,
    fetch: Option<Fetch>,
}

impl Worker {
    fn new(
        conn: RustConnection,
        root: Window,
        keyboard_window: Window,
        shared: Arc<Mutex<Shared>>,
    ) -> Result<Self, Box<dyn Error>> {
        let intern = |name: &[u8]| -> Result<Atom, Box<dyn Error>> {
            Ok(conn.intern_atom(false, name)?.reply()?.atom)
        };
        let atoms = Atoms {
            clipboard: intern(b"CLIPBOARD")?,
            targets: intern(b"TARGETS")?,
            utf8: intern(b"UTF8_STRING")?,
            text: intern(b"TEXT")?,
            plain_utf8: intern(b"text/plain;charset=utf-8")?,
            plain: intern(b"text/plain")?,
            incr: intern(b"INCR")?,
            property: intern(b"MOON_CLIPBOARD")?,
        };
        let window = conn.generate_id()?;
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
            root,
            -1,
            -1,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            0,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )?;
        conn.xfixes_query_version(5, 0)?.reply()?;
        conn.xfixes_select_selection_input(
            window,
            atoms.clipboard,
            SelectionEventMask::SET_SELECTION_OWNER
                | SelectionEventMask::SELECTION_WINDOW_DESTROY
                | SelectionEventMask::SELECTION_CLIENT_CLOSE,
        )?;
        conn.flush()?;
        Ok(Self {
            conn,
            window,
            keyboard_window,
            atoms,
            shared,
            fetch: None,
        })
    }

    fn run(&mut self, wake: &UnixStream) -> Result<(), Box<dyn Error>> {
        loop {
            while let Some(event) = self.conn.poll_for_event()? {
                self.handle(event)?;
            }
            if std::mem::take(&mut lock(&self.shared).own) {
                self.conn
                    .set_selection_owner(self.window, self.atoms.clipboard, CURRENT_TIME)?;
                // A conversion still in flight is older than this text.
                self.fetch = None;
            }
            if self
                .fetch
                .as_ref()
                .is_some_and(|fetch| fetch.started.elapsed() > FETCH_TIMEOUT)
            {
                self.give_up("the copying app did not answer");
            }
            self.conn.flush()?;
            let mut descriptors = [
                libc::pollfd {
                    fd: self.conn.stream().as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: wake.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let timeout = if self.fetch.is_some() { 250 } else { -1 };
            // SAFETY: two initialized pollfd values that outlive the call.
            let result = unsafe { libc::poll(descriptors.as_mut_ptr(), 2, timeout) };
            if result < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    return Err(error.into());
                }
            }
            if descriptors[1].revents != 0 {
                let mut drain = [0_u8; 64];
                use std::io::Read as _;
                while matches!((&*wake).read(&mut drain), Ok(n) if n > 0) {}
            }
        }
    }

    fn handle(&mut self, event: Event) -> Result<(), Box<dyn Error>> {
        match event {
            Event::XfixesSelectionNotify(event) if event.selection == self.atoms.clipboard => {
                if event.owner == self.window {
                    return Ok(());
                }
                if event.owner == NONE {
                    // The app that copied is gone before Moon took its text.
                    self.give_up("");
                    return Ok(());
                }
                // Another app copied: convert it to text while it is awake.
                self.request(event.selection_timestamp, self.atoms.utf8)?;
            }
            Event::SelectionNotify(event)
                if event.requestor == self.window && event.selection == self.atoms.clipboard =>
            {
                let Some(fetch) = self.fetch.as_ref() else {
                    return Ok(());
                };
                if event.property == NONE {
                    // No UTF-8 form: try Latin-1, then give up (no text).
                    if fetch.target == self.atoms.utf8 {
                        self.request(fetch.timestamp, AtomEnum::STRING.into())?;
                    } else {
                        self.give_up("");
                    }
                    return Ok(());
                }
                let reply = self
                    .conn
                    .get_property(
                        true,
                        self.window,
                        self.atoms.property,
                        AtomEnum::ANY,
                        0,
                        (MAX_TEXT / 4 + 1) as u32,
                    )?
                    .reply()?;
                if reply.type_ == self.atoms.incr {
                    // Deleting the property asked for the first chunk.
                    if let Some(fetch) = self.fetch.as_mut() {
                        fetch.chunks = Some(Vec::new());
                    }
                } else {
                    self.finish(reply.value, reply.type_);
                }
            }
            Event::PropertyNotify(event)
                if event.window == self.window
                    && event.atom == self.atoms.property
                    && event.state == Property::NEW_VALUE =>
            {
                if self
                    .fetch
                    .as_ref()
                    .is_none_or(|fetch| fetch.chunks.is_none())
                {
                    return Ok(());
                }
                let reply = self
                    .conn
                    .get_property(
                        true,
                        self.window,
                        self.atoms.property,
                        AtomEnum::ANY,
                        0,
                        (MAX_TEXT / 4 + 1) as u32,
                    )?
                    .reply()?;
                let Some(fetch) = self.fetch.as_mut() else {
                    return Ok(());
                };
                let chunks = fetch.chunks.get_or_insert_with(Vec::new);
                if reply.value.is_empty() {
                    let bytes = std::mem::take(chunks);
                    self.finish(bytes, reply.type_);
                } else if chunks.len() + reply.value.len() > MAX_TEXT {
                    self.give_up("a copy over 1 MiB");
                } else {
                    chunks.extend_from_slice(&reply.value);
                    fetch.started = Instant::now();
                }
            }
            Event::SelectionRequest(request) if request.owner == self.window => {
                self.serve(&request)?;
            }
            // Lost to a newer copy: its XFixes notice brings the new text.
            Event::SelectionClear(_) => {}
            _ => {}
        }
        Ok(())
    }

    fn request(&mut self, timestamp: u32, target: Atom) -> Result<(), Box<dyn Error>> {
        self.conn.convert_selection(
            self.window,
            self.atoms.clipboard,
            target,
            self.atoms.property,
            timestamp,
        )?;
        self.fetch = Some(Fetch {
            timestamp,
            target,
            started: Instant::now(),
            chunks: None,
        });
        Ok(())
    }

    /// The latest copy has no text Moon can take (none, an image, too long,
    /// no answer): it stays with its app, and the paste key has nothing.
    fn give_up(&mut self, why: &str) {
        if !why.is_empty() {
            eprintln!("clipboard: {why}; the copy stays with its app");
        }
        self.fetch = None;
        self.set_text(None);
    }

    /// The copy's text arrived: it is the clipboard's, and Moon owns it.
    fn finish(&mut self, bytes: Vec<u8>, kind: Atom) {
        if bytes.len() > MAX_TEXT {
            self.give_up("a copy over 1 MiB");
            return;
        }
        let Some(fetch) = self.fetch.take() else {
            return;
        };
        let text = decode(bytes, kind == u32::from(AtomEnum::STRING));
        if text.is_empty() {
            self.set_text(None);
            return;
        }
        self.set_text(Some(Arc::from(text)));
        let _ = self
            .conn
            .set_selection_owner(self.window, self.atoms.clipboard, fetch.timestamp);
    }

    fn set_text(&mut self, text: Option<Arc<str>>) {
        let changed = {
            let mut shared = lock(&self.shared);
            let changed = shared.text.is_some() != text.is_some();
            shared.text = text;
            changed
        };
        if changed {
            // Moon repaints the paste key (lit or dimmed).
            let expose = ExposeEvent {
                response_type: EXPOSE_EVENT,
                sequence: 0,
                window: self.keyboard_window,
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                count: 0,
            };
            let _ = self
                .conn
                .send_event(false, self.keyboard_window, EventMask::NO_EVENT, expose);
        }
    }

    /// Another app pastes: the text, in the form it asked for.
    fn serve(&self, request: &SelectionRequestEvent) -> Result<(), Box<dyn Error>> {
        let atoms = &self.atoms;
        // Obsolete clients name no property: the target is the property.
        let property = if request.property == NONE {
            request.target
        } else {
            request.property
        };
        let text = lock(&self.shared).text.clone();
        let served = match (text, served_as(atoms, request.target)) {
            (Some(_), Some(Form::Targets)) => {
                let targets = [
                    atoms.targets,
                    atoms.utf8,
                    atoms.plain_utf8,
                    atoms.text,
                    AtomEnum::STRING.into(),
                    atoms.plain,
                ];
                self.conn.change_property32(
                    PropMode::REPLACE,
                    request.requestor,
                    property,
                    AtomEnum::ATOM,
                    &targets,
                )?;
                true
            }
            (Some(text), Some(Form::Utf8)) => {
                self.conn.change_property8(
                    PropMode::REPLACE,
                    request.requestor,
                    property,
                    atoms.utf8,
                    text.as_bytes(),
                )?;
                true
            }
            (Some(text), Some(Form::Latin1)) => {
                self.conn.change_property8(
                    PropMode::REPLACE,
                    request.requestor,
                    property,
                    AtomEnum::STRING,
                    &latin1(&text),
                )?;
                true
            }
            _ => false,
        };
        let notify = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: request.time,
            requestor: request.requestor,
            selection: request.selection,
            target: request.target,
            property: if served { property } else { NONE },
        };
        self.conn
            .send_event(false, request.requestor, EventMask::NO_EVENT, notify)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    Targets,
    Utf8,
    Latin1,
}

/// The form a paste target asks for (TEXT lets the owner choose: UTF-8).
fn served_as(atoms: &Atoms, target: Atom) -> Option<Form> {
    if target == atoms.targets {
        Some(Form::Targets)
    } else if [atoms.utf8, atoms.text, atoms.plain_utf8].contains(&target) {
        Some(Form::Utf8)
    } else if target == u32::from(AtomEnum::STRING) || target == atoms.plain {
        Some(Form::Latin1)
    } else {
        None
    }
}

/// A copy's bytes as text: STRING is Latin-1, anything else UTF-8.
fn decode(bytes: Vec<u8>, latin1: bool) -> String {
    if latin1 {
        bytes.into_iter().map(char::from).collect()
    } else {
        String::from_utf8(bytes)
            .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
    }
}

/// Latin-1 for STRING requestors; what it cannot carry becomes `?`.
fn latin1(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_decode_as_their_type_says() {
        assert_eq!(decode(b"caf\xc3\xa9".to_vec(), false), "café");
        assert_eq!(decode(b"caf\xe9".to_vec(), true), "café");
        // Broken UTF-8 still pastes, marked.
        assert_eq!(decode(b"a\xffb".to_vec(), false), "a\u{fffd}b");
        assert_eq!(latin1("café ✓"), b"caf\xe9 ?");
    }

    #[test]
    fn paste_targets_map_to_forms() {
        let atoms = Atoms {
            clipboard: 100,
            targets: 101,
            utf8: 102,
            text: 103,
            plain_utf8: 104,
            plain: 105,
            incr: 106,
            property: 107,
        };
        assert_eq!(served_as(&atoms, 101), Some(Form::Targets));
        for utf8 in [102, 103, 104] {
            assert_eq!(served_as(&atoms, utf8), Some(Form::Utf8));
        }
        assert_eq!(
            served_as(&atoms, AtomEnum::STRING.into()),
            Some(Form::Latin1)
        );
        assert_eq!(served_as(&atoms, 105), Some(Form::Latin1));
        assert_eq!(served_as(&atoms, 999), None, "image/png and the like");
    }
}
