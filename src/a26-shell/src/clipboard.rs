//! The system clipboard: X11's CLIPBOARD selection, kept by Moon.
//!
//! Apps copy the way X apps do (they take CLIPBOARD: the Browser's Copy) or
//! with `clipboard set` on the control socket (apps without selection code
//! of their own). Either way Moon takes the text at once, while the app that
//! copied is still in front, and from then on owns CLIPBOARD itself: the text
//! outlives that app being frozen in the background or closed, and every X
//! app's own Paste, the keyboard's paste key and `clipboard get` are answered
//! by Moon. A copy's text and its image (PNG, JPEG, GIF or WebP; an app
//! copies one with `clipboard set image/png BASE64`) are both kept; a copy
//! with neither stays with its app. The paste key types text only.
//!
//! All of it runs on a thread with its own X connection, which waits for
//! replies and events as X clients do; Moon's event loop and its connection
//! never wait (scripts/a26-shell/build.sh's round-trip guard skips this file
//! for that reason). Neither the text nor the image is ever logged.

use std::error::Error;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use x11rb::connection::{Connection, RequestConnection as _};
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
/// The largest image the clipboard holds (bytes, as encoded); one property
/// write carries it too.
pub const MAX_IMAGE: usize = 8 << 20;
/// An app that does not answer a conversion by then keeps its copy.
const FETCH_TIMEOUT: Duration = Duration::from_secs(2);

/// Moon's handle on the clipboard thread (and its image threads' copies).
#[derive(Clone)]
pub struct Clipboard {
    shared: Arc<Mutex<Shared>>,
    wake: Arc<UnixStream>,
}

#[derive(Default)]
struct Shared {
    text: Option<Arc<str>>,
    image: Option<ClipImage>,
    /// `clipboard set` happened: the thread takes CLIPBOARD for what it set.
    own: bool,
}

/// The image formats the clipboard keeps, in the order Moon prefers them
/// when a copy offers several.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Png,
    Jpeg,
    Gif,
    Webp,
}

impl ImageKind {
    pub const ALL: [Self; 4] = [Self::Png, Self::Jpeg, Self::Gif, Self::Webp];

    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
        }
    }

    pub fn parse(mime: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.mime() == mime)
    }

    fn index(self) -> usize {
        self as usize
    }

    /// Whether `bytes` begin as an image of this kind does.
    fn matches(self, bytes: &[u8]) -> bool {
        match self {
            Self::Png => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
            Self::Jpeg => bytes.starts_with(&[0xff, 0xd8, 0xff]),
            Self::Gif => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
            Self::Webp => bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        }
    }
}

/// An image on the clipboard: its format and its encoded bytes.
#[derive(Debug, Clone)]
pub struct ClipImage {
    pub kind: ImageKind,
    pub bytes: Arc<[u8]>,
}

impl ClipImage {
    /// An image as an app hands it over: at most `MAX_IMAGE`, and its bytes
    /// must be of the kind it says.
    pub fn new(kind: ImageKind, bytes: Vec<u8>) -> Result<Self, &'static str> {
        if bytes.len() > MAX_IMAGE {
            return Err("clipboard image is over 8 MiB");
        }
        if !kind.matches(&bytes) {
            return Err("clipboard image data is not of its type");
        }
        Ok(Self {
            kind,
            bytes: Arc::from(bytes),
        })
    }
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
        Ok(Self {
            shared,
            wake: Arc::new(wake),
        })
    }

    /// The clipboard's text, if it holds any.
    pub fn text(&self) -> Option<Arc<str>> {
        lock(&self.shared).text.clone()
    }

    pub fn has_text(&self) -> bool {
        lock(&self.shared).text.is_some()
    }

    /// The clipboard's image, if it holds one.
    pub fn image(&self) -> Option<ClipImage> {
        lock(&self.shared).image.clone()
    }

    /// `clipboard set`: the text is the clipboard's now, in place of
    /// whatever it held (Moon takes CLIPBOARD for it). Longer than
    /// `MAX_TEXT` is refused.
    pub fn set(&self, text: &str) -> Result<(), &'static str> {
        if text.len() > MAX_TEXT {
            return Err("clipboard text is over 1 MiB");
        }
        self.own((!text.is_empty()).then(|| Arc::from(text)), None);
        Ok(())
    }

    /// `clipboard set image/png BASE64`: the image is the clipboard's now,
    /// with no text.
    pub fn set_image(&self, image: ClipImage) {
        self.own(None, Some(image));
    }

    fn own(&self, text: Option<Arc<str>>, image: Option<ClipImage>) {
        {
            let mut shared = lock(&self.shared);
            shared.text = text;
            shared.image = image;
            shared.own = true;
        }
        use std::io::Write as _;
        // A full socket already has a wake-up pending.
        let _ = (&*self.wake).write(&[1]);
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
    /// The image formats' MIME types, by `ImageKind`.
    images: [Atom; 4],
    /// Where conversions are delivered on Moon's window: text (and a copy's
    /// TARGETS), and images, each in its own property so a transfer given
    /// up on never mixes into the next.
    property: Atom,
    image_property: Atom,
}

impl Atoms {
    fn image_kind(&self, target: Atom) -> Option<ImageKind> {
        ImageKind::ALL
            .into_iter()
            .find(|kind| self.images[kind.index()] == target)
    }

    fn property_for(&self, target: Atom) -> Atom {
        if self.image_kind(target).is_some() {
            self.image_property
        } else {
            self.property
        }
    }

    /// The most a conversion to `target` may bring.
    fn limit(&self, target: Atom) -> usize {
        if self.image_kind(target).is_some() {
            MAX_IMAGE
        } else {
            MAX_TEXT
        }
    }
}

/// Taking another app's copy, one conversion at a time: what it offers
/// (TARGETS), then its text, then its image.
struct Fetch {
    /// The copy's own time: Moon takes CLIPBOARD as of it, so a newer copy
    /// made meanwhile is never overwritten.
    timestamp: u32,
    /// The conversion in flight.
    target: Atom,
    started: Instant,
    /// INCR: the chunks so far.
    chunks: Option<Vec<u8>>,
    /// The conversions still to make.
    next: Vec<Atom>,
    /// What arrived so far.
    text: Option<Arc<str>>,
    image: Option<ClipImage>,
}

struct Worker {
    conn: RustConnection,
    window: Window,
    keyboard_window: Window,
    atoms: Atoms,
    /// The largest request this X server takes (BIG-REQUESTS): an image
    /// served in one property must fit.
    max_request: usize,
    shared: Arc<Mutex<Shared>>,
    fetch: Option<Fetch>,
    /// Whether the paste key was last told the clipboard has text.
    lit: bool,
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
        let mut images = [NONE; 4];
        for kind in ImageKind::ALL {
            images[kind.index()] = intern(kind.mime().as_bytes())?;
        }
        let atoms = Atoms {
            clipboard: intern(b"CLIPBOARD")?,
            targets: intern(b"TARGETS")?,
            utf8: intern(b"UTF8_STRING")?,
            text: intern(b"TEXT")?,
            plain_utf8: intern(b"text/plain;charset=utf-8")?,
            plain: intern(b"text/plain")?,
            incr: intern(b"INCR")?,
            images,
            property: intern(b"MOON_CLIPBOARD")?,
            image_property: intern(b"MOON_CLIPBOARD_IMAGE")?,
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
        let max_request = conn.maximum_request_bytes();
        conn.flush()?;
        Ok(Self {
            conn,
            window,
            keyboard_window,
            atoms,
            max_request,
            shared,
            fetch: None,
            lit: false,
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
                // A conversion still in flight is older than this content.
                self.fetch = None;
                // (Set from an image thread, Moon's loop has not seen it.)
                self.repaint_paste_key();
            }
            if self
                .fetch
                .as_ref()
                .is_some_and(|fetch| fetch.started.elapsed() > FETCH_TIMEOUT)
            {
                self.skip("the copying app did not answer")?;
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
                    // The app that copied is gone before Moon took its copy.
                    self.give_up("");
                    return Ok(());
                }
                // Another app copied: see what it offers while it is awake.
                self.fetch = Some(Fetch {
                    timestamp: event.selection_timestamp,
                    target: NONE,
                    started: Instant::now(),
                    chunks: None,
                    next: vec![self.atoms.targets],
                    text: None,
                    image: None,
                });
                self.advance()?;
            }
            Event::SelectionNotify(event)
                if event.requestor == self.window && event.selection == self.atoms.clipboard =>
            {
                // (An answer to a conversion given up on is not this one.)
                let Some(target) = self
                    .fetch
                    .as_ref()
                    .map(|fetch| fetch.target)
                    .filter(|target| *target == event.target)
                else {
                    return Ok(());
                };
                if event.property == NONE {
                    self.refused(target)?;
                    return Ok(());
                }
                let reply = self
                    .conn
                    .get_property(
                        true,
                        self.window,
                        self.atoms.property_for(target),
                        AtomEnum::ANY,
                        0,
                        (self.atoms.limit(target) / 4 + 1) as u32,
                    )?
                    .reply()?;
                if reply.type_ == self.atoms.incr {
                    // Deleting the property asked for the first chunk.
                    if let Some(fetch) = self.fetch.as_mut() {
                        fetch.chunks = Some(Vec::new());
                    }
                } else if reply.bytes_after > 0 {
                    self.skip(too_large(&self.atoms, target))?;
                } else if target == self.atoms.targets {
                    let offered: Vec<Atom> =
                        reply.value32().map(Iterator::collect).unwrap_or_default();
                    if let Some(fetch) = self.fetch.as_mut() {
                        fetch.next = plan(&self.atoms, &offered);
                    }
                    self.advance()?;
                } else {
                    self.arrived(reply.value, reply.type_)?;
                }
            }
            Event::PropertyNotify(event)
                if event.window == self.window && event.state == Property::NEW_VALUE =>
            {
                let Some(target) = self
                    .fetch
                    .as_ref()
                    .filter(|fetch| fetch.chunks.is_some())
                    .map(|fetch| fetch.target)
                    .filter(|target| self.atoms.property_for(*target) == event.atom)
                else {
                    return Ok(());
                };
                let limit = self.atoms.limit(target);
                let reply = self
                    .conn
                    .get_property(
                        true,
                        self.window,
                        event.atom,
                        AtomEnum::ANY,
                        0,
                        (limit / 4 + 1) as u32,
                    )?
                    .reply()?;
                let Some(fetch) = self.fetch.as_mut() else {
                    return Ok(());
                };
                let chunks = fetch.chunks.get_or_insert_with(Vec::new);
                if reply.value.is_empty() {
                    let bytes = std::mem::take(chunks);
                    self.arrived(bytes, reply.type_)?;
                } else if chunks.len() + reply.value.len() > limit {
                    self.skip(too_large(&self.atoms, target))?;
                } else {
                    chunks.extend_from_slice(&reply.value);
                    fetch.started = Instant::now();
                }
            }
            Event::SelectionRequest(request) if request.owner == self.window => {
                self.serve(&request)?;
            }
            // Lost to a newer copy: its XFixes notice brings the new one.
            Event::SelectionClear(_) => {}
            _ => {}
        }
        Ok(())
    }

    /// The next conversion of the copy, or, with none left, what came of
    /// it.
    fn advance(&mut self) -> Result<(), Box<dyn Error>> {
        let Some(fetch) = self.fetch.as_mut() else {
            return Ok(());
        };
        if fetch.next.is_empty() {
            let Some(fetch) = self.fetch.take() else {
                return Ok(());
            };
            self.finish(fetch);
            return Ok(());
        }
        let target = fetch.next.remove(0);
        fetch.target = target;
        fetch.started = Instant::now();
        fetch.chunks = None;
        self.conn.convert_selection(
            self.window,
            self.atoms.clipboard,
            target,
            self.atoms.property_for(target),
            fetch.timestamp,
        )?;
        Ok(())
    }

    /// The copying app has no such form after all. Without TARGETS, Moon
    /// asks for UTF-8 text (as X clients long did); without UTF-8, Latin-1.
    fn refused(&mut self, target: Atom) -> Result<(), Box<dyn Error>> {
        let fallback = if target == self.atoms.targets {
            Some(self.atoms.utf8)
        } else if target == self.atoms.utf8 {
            Some(AtomEnum::STRING.into())
        } else {
            None
        };
        if let (Some(fetch), Some(fallback)) = (self.fetch.as_mut(), fallback)
            && !fetch.next.contains(&fallback)
        {
            fetch.next.insert(0, fallback);
        }
        self.advance()
    }

    /// This form of the copy cannot be had (too large, no answer): the
    /// others still can.
    fn skip(&mut self, why: &str) -> Result<(), Box<dyn Error>> {
        eprintln!("clipboard: {why}");
        self.advance()
    }

    /// A conversion's bytes arrived: the copy's text or its image.
    fn arrived(&mut self, bytes: Vec<u8>, kind: Atom) -> Result<(), Box<dyn Error>> {
        let Some(fetch) = self.fetch.as_mut() else {
            return Ok(());
        };
        fetch.chunks = None;
        if let Some(image) = self.atoms.image_kind(fetch.target) {
            match ClipImage::new(image, bytes) {
                Ok(image) => fetch.image = Some(image),
                Err(why) => eprintln!("clipboard: {why}"),
            }
        } else if bytes.len() > MAX_TEXT {
            eprintln!("clipboard: {}", too_large(&self.atoms, fetch.target));
        } else {
            let text = decode(bytes, kind == u32::from(AtomEnum::STRING));
            fetch.text = (!text.is_empty()).then(|| Arc::from(text));
        }
        self.advance()
    }

    /// The latest copy has nothing Moon can take (no text or image, too
    /// large, no answer): it stays with its app, and the paste key has
    /// nothing.
    fn give_up(&mut self, why: &str) {
        if !why.is_empty() {
            eprintln!("clipboard: {why}; the copy stays with its app");
        }
        self.fetch = None;
        self.set_contents(None, None);
    }

    /// Every conversion is done: what arrived is the clipboard's, and Moon
    /// owns it; with nothing, the copy stays with its app.
    fn finish(&mut self, fetch: Fetch) {
        if fetch.text.is_none() && fetch.image.is_none() {
            self.set_contents(None, None);
            return;
        }
        self.set_contents(fetch.text, fetch.image);
        let _ = self
            .conn
            .set_selection_owner(self.window, self.atoms.clipboard, fetch.timestamp);
    }

    fn set_contents(&mut self, text: Option<Arc<str>>, image: Option<ClipImage>) {
        {
            let mut shared = lock(&self.shared);
            shared.text = text;
            shared.image = image;
        }
        self.repaint_paste_key();
    }

    /// The clipboard gained or lost text: Moon wakes and lights or dims the
    /// paste key.
    fn repaint_paste_key(&mut self) {
        let lit = lock(&self.shared).text.is_some();
        if lit != std::mem::replace(&mut self.lit, lit) {
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

    /// Another app pastes: the text or the image, in the form it asked for.
    fn serve(&self, request: &SelectionRequestEvent) -> Result<(), Box<dyn Error>> {
        let atoms = &self.atoms;
        // Obsolete clients name no property: the target is the property.
        let property = if request.property == NONE {
            request.target
        } else {
            request.property
        };
        let (text, image) = {
            let shared = lock(&self.shared);
            (shared.text.clone(), shared.image.clone())
        };
        let served = match (served_as(atoms, request.target), &text, &image) {
            (Some(Form::Targets), _, _) if text.is_some() || image.is_some() => {
                let mut targets = vec![atoms.targets];
                if text.is_some() {
                    targets.extend([
                        atoms.utf8,
                        atoms.plain_utf8,
                        atoms.text,
                        AtomEnum::STRING.into(),
                        atoms.plain,
                    ]);
                }
                if let Some(image) = &image {
                    targets.push(atoms.images[image.kind.index()]);
                }
                self.conn.change_property32(
                    PropMode::REPLACE,
                    request.requestor,
                    property,
                    AtomEnum::ATOM,
                    &targets,
                )?;
                true
            }
            (Some(Form::Utf8), Some(text), _) => {
                self.conn.change_property8(
                    PropMode::REPLACE,
                    request.requestor,
                    property,
                    atoms.utf8,
                    text.as_bytes(),
                )?;
                true
            }
            (Some(Form::Latin1), Some(text), _) => {
                self.conn.change_property8(
                    PropMode::REPLACE,
                    request.requestor,
                    property,
                    AtomEnum::STRING,
                    &latin1(text),
                )?;
                true
            }
            (Some(Form::Image(kind)), _, Some(image)) if image.kind == kind => {
                // One request carries it (8 MiB is well inside BIG-REQUESTS'
                // 16 MiB; a server without it gets no image).
                let fits = image.bytes.len() + 64 <= self.max_request;
                if fits {
                    self.conn.change_property8(
                        PropMode::REPLACE,
                        request.requestor,
                        property,
                        request.target,
                        &image.bytes,
                    )?;
                }
                fits
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

/// Why a form of a copy was left with its app.
fn too_large(atoms: &Atoms, target: Atom) -> &'static str {
    if atoms.image_kind(target).is_some() {
        "a copied image over 8 MiB"
    } else {
        "a copy's text over 1 MiB"
    }
}

/// The conversions to make of a copy that offers `offered` (its TARGETS):
/// its text (UTF-8 when it has it), then its image (the kind Moon prefers).
/// An empty or unreadable list is asked for UTF-8 text, as before TARGETS.
fn plan(atoms: &Atoms, offered: &[Atom]) -> Vec<Atom> {
    if offered.is_empty() {
        return vec![atoms.utf8];
    }
    let text = [
        atoms.utf8,
        atoms.plain_utf8,
        AtomEnum::STRING.into(),
        atoms.text,
        atoms.plain,
    ]
    .into_iter()
    .find(|target| offered.contains(target));
    let image = ImageKind::ALL
        .into_iter()
        .map(|kind| atoms.images[kind.index()])
        .find(|target| offered.contains(target));
    text.into_iter().chain(image).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    Targets,
    Utf8,
    Latin1,
    Image(ImageKind),
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
        atoms.image_kind(target).map(Form::Image)
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

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
/// Each byte's value in base64, or 0xff.
const BASE64_VALUES: [u8; 256] = {
    let mut values = [0xff; 256];
    let mut index = 0;
    while index < 64 {
        values[BASE64[index] as usize] = index as u8;
        index += 1;
    }
    values
};

/// Standard base64 with padding: images over the control socket. (A table
/// a group at a time: an 8 MiB image takes milliseconds on the phone, on
/// Moon's main loop.)
pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = Vec::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut groups = bytes.chunks_exact(3);
    for group in &mut groups {
        let n = u32::from(group[0]) << 16 | u32::from(group[1]) << 8 | u32::from(group[2]);
        out.extend_from_slice(&[
            BASE64[(n >> 18) as usize & 63],
            BASE64[(n >> 12) as usize & 63],
            BASE64[(n >> 6) as usize & 63],
            BASE64[n as usize & 63],
        ]);
    }
    let rest = groups.remainder();
    if !rest.is_empty() {
        let n = u32::from(rest[0]) << 16 | rest.get(1).map_or(0, |byte| u32::from(*byte) << 8);
        out.extend_from_slice(&[
            BASE64[(n >> 18) as usize & 63],
            BASE64[(n >> 12) as usize & 63],
            if rest.len() == 2 {
                BASE64[(n >> 6) as usize & 63]
            } else {
                b'='
            },
            b'=',
        ]);
    }
    String::from_utf8(out).unwrap_or_default()
}

/// Standard base64, padded or not; anything else (whitespace included) is
/// refused.
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    let body = bytes
        .strip_suffix(b"==")
        .or_else(|| bytes.strip_suffix(b"="))
        .unwrap_or(bytes);
    // Padding only where the last group is short.
    let padding = bytes.len() - body.len();
    if body.len() % 4 == 1 || (padding > 0 && body.len() % 4 + padding != 4) {
        return None;
    }
    let value = |byte: u8| u32::from(BASE64_VALUES[usize::from(byte)]);
    let mut out = Vec::with_capacity(body.len() / 4 * 3 + 2);
    let mut groups = body.chunks_exact(4);
    for group in &mut groups {
        let (a, b, c, d) = (
            value(group[0]),
            value(group[1]),
            value(group[2]),
            value(group[3]),
        );
        if (a | b | c | d) > 63 {
            return None;
        }
        let n = a << 18 | b << 12 | c << 6 | d;
        out.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8, n as u8]);
    }
    let rest = groups.remainder();
    if !rest.is_empty() {
        let mut n = 0;
        for (index, byte) in rest.iter().enumerate() {
            let value = value(*byte);
            if value > 63 {
                return None;
            }
            n |= value << (18 - 6 * index);
        }
        out.extend_from_slice(&n.to_be_bytes()[1..rest.len()]);
    }
    Some(out)
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
        let atoms = atoms();
        assert_eq!(served_as(&atoms, 101), Some(Form::Targets));
        for utf8 in [102, 103, 104] {
            assert_eq!(served_as(&atoms, utf8), Some(Form::Utf8));
        }
        assert_eq!(
            served_as(&atoms, AtomEnum::STRING.into()),
            Some(Form::Latin1)
        );
        assert_eq!(served_as(&atoms, 105), Some(Form::Latin1));
        assert_eq!(served_as(&atoms, 110), Some(Form::Image(ImageKind::Png)));
        assert_eq!(served_as(&atoms, 113), Some(Form::Image(ImageKind::Webp)));
        assert_eq!(served_as(&atoms, 999), None, "text/html and the like");
        // Images arrive in their own property, with their own limit.
        assert_eq!(atoms.property_for(111), 108);
        assert_eq!(atoms.property_for(102), 107);
        assert_eq!(atoms.limit(110), MAX_IMAGE);
        assert_eq!(atoms.limit(101), MAX_TEXT);
    }

    fn atoms() -> Atoms {
        Atoms {
            clipboard: 100,
            targets: 101,
            utf8: 102,
            text: 103,
            plain_utf8: 104,
            plain: 105,
            incr: 106,
            images: [110, 111, 112, 113],
            property: 107,
            image_property: 108,
        }
    }

    #[test]
    fn a_copy_gives_its_text_then_its_preferred_image() {
        let atoms = atoms();
        let string = u32::from(AtomEnum::STRING);
        // Chromium's text copy: UTF-8 over the rest, no image.
        assert_eq!(plan(&atoms, &[101, 999, 104, 102, string]), vec![102]);
        // Its Copy Image: PNG only (text/html is not kept).
        assert_eq!(plan(&atoms, &[101, 999, 110]), vec![110]);
        // Both, and PNG over JPEG whatever the order offered.
        assert_eq!(plan(&atoms, &[111, string, 110]), vec![string, 110]);
        assert_eq!(plan(&atoms, &[113, 112]), vec![112]);
        assert!(plan(&atoms, &[101, 999]).is_empty());
        assert_eq!(plan(&atoms, &[]), vec![102], "a TARGETS answer of nothing");
    }

    #[test]
    fn base64_round_trips() {
        for (bytes, text) in [
            (&b""[..], ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg=="),
            (b"\xff\xfe\x00", "//4A"),
        ] {
            assert_eq!(base64_encode(bytes), text);
            assert_eq!(base64_decode(text).as_deref(), Some(bytes));
        }
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(base64_decode(&base64_encode(&all)), Some(all));
        assert_eq!(base64_decode("Zm9vYg").as_deref(), Some(&b"foob"[..]));
        for bad in ["Zg=", "Zm9v=", "Z", "Zm 9v", "Zm9v\n", "Zm9-", "Zg==Zg=="] {
            assert_eq!(base64_decode(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn images_must_be_what_they_say_and_fit() {
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        assert!(ClipImage::new(ImageKind::Png, png.clone()).is_ok());
        assert!(ClipImage::new(ImageKind::Jpeg, png.clone()).is_err());
        assert!(ClipImage::new(ImageKind::Jpeg, vec![0xff, 0xd8, 0xff, 0xe0]).is_ok());
        assert!(ClipImage::new(ImageKind::Gif, b"GIF89a..".to_vec()).is_ok());
        assert!(ClipImage::new(ImageKind::Webp, b"RIFF\0\0\0\0WEBPVP8 ".to_vec()).is_ok());
        assert!(ClipImage::new(ImageKind::Webp, b"RIFF\0\0\0\0WAVE".to_vec()).is_err());
        let mut big = png;
        big.resize(MAX_IMAGE + 1, 0);
        assert!(ClipImage::new(ImageKind::Png, big).is_err());
        for kind in ImageKind::ALL {
            assert_eq!(ImageKind::parse(kind.mime()), Some(kind));
        }
        assert_eq!(ImageKind::parse("image/svg+xml"), None);
    }
}
