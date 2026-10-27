//! moon-wl-demo: a native Wayland app for testing Moon's Wayland server.
//!
//! Full-screen xdg toplevel on wl_shm. Three text fields (text, password,
//! URL) use text-input-v3, so tapping one opens Moon's keyboard with the
//! right layout; touches leave fading dots; the "animate" button runs a
//! full-screen animation paced by frame callbacks and logs its frame rate.
//! Everything it receives is logged to stderr (except password text).

use std::fs::File;
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::time::Instant;

use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_keyboard, wl_registry, wl_seat, wl_shm,
    wl_shm_pool, wl_surface, wl_touch,
};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::wp::text_input::zv3::client::{
    zwp_text_input_manager_v3, zwp_text_input_v3,
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

const FONT: &[u8] = include_bytes!("../../a26-shell/assets/fonts/Inter-Regular.ttf");

#[derive(Clone, Copy, PartialEq)]
enum Field {
    Text,
    Password,
    Url,
}

/// Box mode: the moving box's size in buffer pixels.
const BOX_SIZE: usize = 240;
const FIELDS: [(Field, &str); 3] = [
    (Field::Text, "Text"),
    (Field::Password, "Password"),
    (Field::Url, "URL"),
];

struct Buffer {
    buffer: wl_buffer::WlBuffer,
    offset: usize,
    busy: bool,
    /// The content generation painted into it (box mode repaints only the
    /// band while this is current).
    generation: u64,
}

struct App {
    running: bool,
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    wm: Option<xdg_wm_base::XdgWmBase>,
    seat: Option<wl_seat::WlSeat>,
    text_manager: Option<zwp_text_input_manager_v3::ZwpTextInputManagerV3>,
    text_input: Option<zwp_text_input_v3::ZwpTextInputV3>,
    surface: Option<wl_surface::WlSurface>,
    /// Buffer pixels; the toplevel's size in surface units times `scale`.
    width: i32,
    height: i32,
    logical: (i32, i32),
    /// The compositor's preferred buffer scale (wl_surface v6).
    scale: i32,
    configured: bool,
    memory: Option<memmap::Map>,
    pool: Option<wl_shm_pool::WlShmPool>,
    buffers: Vec<Buffer>,
    font: Option<fontdue::Font>,
    dots: Vec<(f64, f64, Instant)>,
    touches: Vec<(i32, f64, f64)>,
    focused_field: Option<Field>,
    texts: [String; 3],
    pending_text: String,
    pending_delete: usize,
    animate: bool,
    phase: f32,
    frame_pending: bool,
    dirty: bool,
    last_frame_ms: Option<u32>,
    intervals: Vec<u32>,
    stats_at: Instant,
    /// MOON_DEMO_DRAW=box: the animation is a box moving in one band, and
    /// only that band is repainted and damaged (a cheap client, to measure
    /// the display path rather than the drawing).
    box_draw: bool,
    generation: u64,
    paint_ms: Vec<f32>,
}

mod memmap {
    use std::os::fd::{AsRawFd, OwnedFd};

    pub struct Map {
        pub fd: OwnedFd,
        pub ptr: *mut u8,
        pub len: usize,
    }

    impl Map {
        pub fn new(fd: OwnedFd, len: usize) -> std::io::Result<Self> {
            // SAFETY: a shared mapping of a memfd we just sized to `len`.
            let ptr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    len,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    fd.as_raw_fd(),
                    0,
                )
            };
            if ptr == libc::MAP_FAILED {
                return Err(std::io::Error::last_os_error());
            }
            Ok(Self {
                fd,
                ptr: ptr.cast(),
                len,
            })
        }

        pub fn pixels(&mut self, offset: usize, count: usize) -> &mut [u32] {
            assert!(offset + count * 4 <= self.len);
            // SAFETY: inside the mapping, 4-byte aligned (offsets are page
            // multiples), and only this process writes while it owns it.
            unsafe { std::slice::from_raw_parts_mut(self.ptr.add(offset).cast(), count) }
        }
    }

    impl Drop for Map {
        fn drop(&mut self) {
            // SAFETY: unmaps exactly what new() mapped.
            unsafe { libc::munmap(self.ptr.cast(), self.len) };
        }
    }
}

fn main() {
    let connection = Connection::connect_to_env().expect("no Wayland display (WAYLAND_DISPLAY)");
    let mut queue = connection.new_event_queue();
    let qh = queue.handle();
    connection.display().get_registry(&qh, ());
    let mut app = App {
        running: true,
        compositor: None,
        shm: None,
        wm: None,
        seat: None,
        text_manager: None,
        text_input: None,
        surface: None,
        width: 0,
        height: 0,
        logical: (0, 0),
        scale: 1,
        configured: false,
        memory: None,
        pool: None,
        buffers: Vec::new(),
        font: fontdue::Font::from_bytes(FONT, fontdue::FontSettings::default()).ok(),
        dots: Vec::new(),
        touches: Vec::new(),
        focused_field: None,
        texts: Default::default(),
        pending_text: String::new(),
        pending_delete: 0,
        animate: std::env::args().any(|arg| arg == "--animate"),
        phase: 0.0,
        frame_pending: false,
        dirty: true,
        last_frame_ms: None,
        intervals: Vec::new(),
        stats_at: Instant::now(),
        box_draw: std::env::var("MOON_DEMO_DRAW").is_ok_and(|draw| draw == "box"),
        generation: 0,
        paint_ms: Vec::new(),
    };
    queue.roundtrip(&mut app).expect("registry roundtrip");
    let (Some(compositor), Some(wm)) = (app.compositor.clone(), app.wm.clone()) else {
        eprintln!("demo: wl_compositor or xdg_wm_base missing");
        std::process::exit(1);
    };
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("Moon Wayland demo".into());
    toplevel.set_app_id("moon-wl-demo".into());
    surface.commit();
    app.surface = Some(surface);
    if let (Some(manager), Some(seat)) = (&app.text_manager, &app.seat) {
        app.text_input = Some(manager.get_text_input(seat, &qh, ()));
    }
    eprintln!("demo: connected, waiting for the first configure");
    while app.running {
        if queue.blocking_dispatch(&mut app).is_err() {
            break;
        }
        app.redraw(&qh);
    }
}

impl App {
    /// Buffer size from the configured size and the scale.
    fn resize(&mut self) {
        if self.logical.0 > 0 {
            self.width = self.logical.0 * self.scale;
            self.height = self.logical.1 * self.scale;
            self.dirty = true;
        }
    }

    fn field_rect(&self, index: usize) -> (i32, i32, i32, i32) {
        (60, 80 + index as i32 * 190, self.width - 120, 140)
    }

    fn button_rect(&self) -> (i32, i32, i32, i32) {
        (60, 80 + 3 * 190, self.width - 120, 140)
    }

    fn hit(rect: (i32, i32, i32, i32), x: f64, y: f64) -> bool {
        let (rx, ry, rw, rh) = rect;
        x >= f64::from(rx) && x < f64::from(rx + rw) && y >= f64::from(ry) && y < f64::from(ry + rh)
    }

    fn ensure_buffers(&mut self, qh: &QueueHandle<Self>) {
        let size = (self.width * self.height * 4) as usize;
        let slot = size.div_ceil(4096) * 4096;
        if self.memory.as_ref().is_some_and(|memory| memory.len == slot * 2) {
            return;
        }
        for buffer in self.buffers.drain(..) {
            buffer.buffer.destroy();
        }
        if let Some(pool) = self.pool.take() {
            pool.destroy();
        }
        let name = std::ffi::CString::new("moon-wl-demo").unwrap();
        // SAFETY: a fresh memfd; ownership moves into OwnedFd.
        let fd = unsafe { OwnedFd::from_raw_fd(libc::memfd_create(
                name.as_ptr(),
                // MOON_DEMO_NO_SEAL=1 makes an unsealable pool (to test how
                // the compositor handles memory that may shrink).
                if std::env::var_os("MOON_DEMO_NO_SEAL").is_some() {
                    libc::MFD_CLOEXEC
                } else {
                    libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING
                },
            )) };
        let file = File::from(fd);
        file.set_len((slot * 2) as u64).expect("size the pool");
        let fd = OwnedFd::from(file);
        let pool = self
            .shm
            .as_ref()
            .expect("wl_shm")
            .create_pool(fd.as_fd(), (slot * 2) as i32, qh, ());
        for index in 0..2 {
            let buffer = pool.create_buffer(
                (index * slot) as i32,
                self.width,
                self.height,
                self.width * 4,
                wl_shm::Format::Xrgb8888,
                qh,
                index,
            );
            self.buffers.push(Buffer {
                buffer,
                offset: index * slot,
                busy: false,
                generation: u64::MAX,
            });
        }
        self.memory = Some(memmap::Map::new(fd, slot * 2).expect("map the pool"));
        self.pool = Some(pool);
    }

    fn redraw(&mut self, qh: &QueueHandle<Self>) {
        if !self.configured || self.frame_pending || !(self.dirty || self.animate || !self.dots.is_empty())
        {
            return;
        }
        self.ensure_buffers(qh);
        let Some(index) = self.buffers.iter().position(|buffer| !buffer.busy) else {
            return;
        };
        let (width, height) = (self.width as usize, self.height as usize);
        let offset = self.buffers[index].offset;
        if self.dirty {
            self.generation += 1;
        }
        let full = !self.box_draw
            || !self.animate
            || !self.dots.is_empty()
            || self.buffers[index].generation != self.generation;
        let started = Instant::now();
        let mut canvas = std::mem::take(&mut self.memory).expect("mapped");
        let band = {
            let pixels = canvas.pixels(offset, width * height);
            if full {
                self.paint(pixels, width, height);
                (0, 0, self.width, self.height)
            } else {
                self.paint_band(pixels, width, height)
            }
        };
        self.memory = Some(canvas);
        if self.animate {
            self.paint_ms.push(started.elapsed().as_secs_f32() * 1000.0);
        }
        self.buffers[index].generation = self.generation;
        let surface = self.surface.as_ref().expect("surface");
        surface.set_buffer_scale(self.scale);
        surface.attach(Some(&self.buffers[index].buffer), 0, 0);
        surface.damage_buffer(band.0, band.1, band.2, band.3);
        surface.frame(qh, ());
        surface.commit();
        self.buffers[index].busy = true;
        self.frame_pending = true;
        self.dirty = false;
    }

    /// Box mode: the band is repainted with the box at its new place.
    /// Returns the band (x, y, width, height).
    fn paint_band(&self, pixels: &mut [u32], width: usize, height: usize) -> (i32, i32, i32, i32) {
        let (y0, size) = (height * 3 / 5, BOX_SIZE.min(height / 4));
        for y in y0..(y0 + size).min(height) {
            pixels[y * width..(y + 1) * width].fill(background(y, height, 0.0));
        }
        let travel = width.saturating_sub(size).max(1);
        let step = (self.phase / 0.08).round() as usize * 9 % (2 * travel);
        let x = if step < travel { step } else { 2 * travel - step };
        fill(pixels, width, height, (x as i32, y0 as i32, size as i32, size as i32), 0xff0a84ff);
        (0, y0 as i32, width as i32, size as i32)
    }

    fn paint(&mut self, pixels: &mut [u32], width: usize, height: usize) {
        let phase = self.phase;
        for y in 0..height {
            let t = y as f32 / height as f32;
            let wave = if self.animate && !self.box_draw {
                ((t * 6.0 + phase).sin() * 0.5 + 0.5) * 60.0
            } else {
                0.0
            };
            pixels[y * width..(y + 1) * width].fill(background(y, height, wave));
        }
        for (index, (field, label)) in FIELDS.iter().enumerate() {
            let rect = self.field_rect(index);
            let focused = self.focused_field == Some(*field);
            fill(pixels, width, height, rect, if focused { 0xff2c2c40 } else { 0xff1c1c24 });
            let shown = if *field == Field::Password {
                "•".repeat(self.texts[index].chars().count())
            } else {
                self.texts[index].clone()
            };
            let line = if shown.is_empty() {
                format!("{label}…")
            } else {
                shown
            };
            self.text(pixels, width, height, rect.0 + 30, rect.1 + 95, 56.0, &line, 0xffe8e8f0);
        }
        let button = self.button_rect();
        fill(
            pixels,
            width,
            height,
            button,
            if self.animate { 0xff0a84ff } else { 0xff30304a },
        );
        let label = if self.animate { "Stop animation" } else { "Animate" };
        self.text(pixels, width, height, button.0 + 30, button.1 + 95, 56.0, label, 0xffffffff);
        let now = Instant::now();
        self.dots.retain(|(_, _, at)| now.duration_since(*at).as_millis() < 600);
        for (x, y, at) in self.dots.clone() {
            let age = now.duration_since(at).as_secs_f32() / 0.6;
            let radius = 30.0 + 50.0 * age;
            let level = (255.0 * (1.0 - age)) as u32;
            disc(pixels, width, height, x as f32, y as f32, radius, 0xff000000 | level << 16 | level << 8 | 0x40);
        }
        for (_, x, y) in self.touches.clone() {
            disc(pixels, width, height, x as f32, y as f32, 40.0, 0xffffffff);
        }
        if self.box_draw && self.animate {
            self.paint_band(pixels, width, height);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn text(&self, pixels: &mut [u32], width: usize, height: usize, x: i32, baseline: i32, size: f32, text: &str, color: u32) {
        let Some(font) = &self.font else {
            return;
        };
        let mut pen = x as f32;
        for character in text.chars() {
            let (metrics, coverage) = font.rasterize(character, size);
            let gx = pen as i32 + metrics.xmin;
            let gy = baseline - metrics.height as i32 - metrics.ymin;
            for row in 0..metrics.height {
                for column in 0..metrics.width {
                    let alpha = u32::from(coverage[row * metrics.width + column]);
                    let (px, py) = (gx + column as i32, gy + row as i32);
                    if alpha == 0 || px < 0 || py < 0 || px as usize >= width || py as usize >= height {
                        continue;
                    }
                    let index = py as usize * width + px as usize;
                    pixels[index] = blend(pixels[index], color, alpha);
                }
            }
            pen += metrics.advance_width;
        }
    }

    fn touch_at(&mut self, x: f64, y: f64) {
        for (index, (field, _)) in FIELDS.iter().enumerate() {
            if Self::hit(self.field_rect(index), x, y) {
                self.focus_field(Some(*field));
                return;
            }
        }
        if Self::hit(self.button_rect(), x, y) {
            self.animate = !self.animate;
            self.intervals.clear();
            self.last_frame_ms = None;
            eprintln!("demo: animation {}", if self.animate { "on" } else { "off" });
            self.dirty = true;
            return;
        }
        self.focus_field(None);
    }

    fn focus_field(&mut self, field: Option<Field>) {
        self.focused_field = field;
        self.dirty = true;
        let Some(input) = &self.text_input else {
            return;
        };
        match field {
            Some(field) => {
                input.enable();
                input.set_content_type(
                    zwp_text_input_v3::ContentHint::None,
                    match field {
                        Field::Text => zwp_text_input_v3::ContentPurpose::Normal,
                        Field::Password => zwp_text_input_v3::ContentPurpose::Password,
                        Field::Url => zwp_text_input_v3::ContentPurpose::Url,
                    },
                );
            }
            None => input.disable(),
        }
        input.commit();
        eprintln!("demo: text input {}", if field.is_some() { "enabled" } else { "disabled" });
    }

    fn field_index(&self) -> Option<usize> {
        let field = self.focused_field?;
        FIELDS.iter().position(|(candidate, _)| *candidate == field)
    }
}

/// The background gradient's color on row `y`, brightened by `wave`.
fn background(y: usize, height: usize, wave: f32) -> u32 {
    let t = y as f32 / height as f32;
    let r = (20.0 + 40.0 * t + wave) as u32;
    let g = (24.0 + 30.0 * (1.0 - t)) as u32;
    let b = (60.0 + 90.0 * t) as u32;
    0xff00_0000 | (r.min(255) << 16) | (g.min(255) << 8) | b.min(255)
}

fn fill(pixels: &mut [u32], width: usize, height: usize, rect: (i32, i32, i32, i32), color: u32) {
    let (x, y, w, h) = rect;
    for row in y.max(0)..(y + h).min(height as i32) {
        let start = row as usize * width;
        let x0 = x.max(0) as usize;
        let x1 = ((x + w).min(width as i32)).max(0) as usize;
        if x1 > x0 {
            pixels[start + x0..start + x1].fill(color);
        }
    }
}

fn disc(pixels: &mut [u32], width: usize, height: usize, cx: f32, cy: f32, r: f32, color: u32) {
    let y0 = (cy - r).max(0.0) as usize;
    let y1 = ((cy + r) as usize).min(height.saturating_sub(1));
    let x0 = (cx - r).max(0.0) as usize;
    let x1 = ((cx + r) as usize).min(width.saturating_sub(1));
    for y in y0..=y1 {
        for x in x0..=x1 {
            let (dx, dy) = (x as f32 - cx, y as f32 - cy);
            if dx * dx + dy * dy <= r * r {
                pixels[y * width + x] = color;
            }
        }
    }
}

fn blend(under: u32, over: u32, alpha: u32) -> u32 {
    let mix = |shift: u32| {
        let a = (under >> shift) & 0xff;
        let b = (over >> shift) & 0xff;
        ((a * (255 - alpha) + b * alpha) / 255) << shift
    };
    0xff00_0000 | mix(16) | mix(8) | mix(0)
}

impl Dispatch<wl_registry::WlRegistry, ()> for App {
    fn event(
        app: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => app.compositor = Some(registry.bind(name, version.min(6), qh, ())),
                "wl_shm" => app.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => app.wm = Some(registry.bind(name, version.min(6), qh, ())),
                "wl_seat" => {
                    let seat: wl_seat::WlSeat = registry.bind(name, version.min(7), qh, ());
                    app.seat = Some(seat);
                }
                "zwp_text_input_manager_v3" => {
                    app.text_manager = Some(registry.bind(name, 1, qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for App {
    fn event(_: &mut Self, wm: &xdg_wm_base::XdgWmBase, event: xdg_wm_base::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for App {
    fn event(app: &mut Self, xdg: &xdg_surface::XdgSurface, event: xdg_surface::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg.ack_configure(serial);
            app.configured = true;
            app.dirty = true;
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for App {
    fn event(app: &mut Self, _: &xdg_toplevel::XdgToplevel, event: xdg_toplevel::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            xdg_toplevel::Event::Configure { width, height, states } => {
                let states: Vec<u32> = states
                    .chunks_exact(4)
                    .map(|chunk| u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                    .collect();
                eprintln!("demo: configure {width}x{height} states {states:?}");
                if width > 0 && height > 0 {
                    app.logical = (width, height);
                } else if app.logical.0 == 0 {
                    app.logical = (1080, 2216);
                }
                app.resize();
            }
            xdg_toplevel::Event::Close => app.running = false,
            _ => {}
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for App {
    fn event(app: &mut Self, _: &wl_callback::WlCallback, event: wl_callback::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_callback::Event::Done { callback_data } = event {
            app.frame_pending = false;
            if app.animate {
                app.phase += 0.08;
                if let Some(last) = app.last_frame_ms {
                    app.intervals.push(callback_data.wrapping_sub(last));
                }
                app.last_frame_ms = Some(callback_data);
                if app.stats_at.elapsed().as_secs() >= 2 && !app.intervals.is_empty() {
                    let mean = app.intervals.iter().sum::<u32>() as f32 / app.intervals.len() as f32;
                    let paints = app.paint_ms.len().max(1) as f32;
                    eprintln!(
                        "demo: {} frames, mean interval {:.2} ms ({:.1} fps), paint mean {:.2} ms max {:.2} ms{}",
                        app.intervals.len(),
                        mean,
                        1000.0 / mean.max(0.001),
                        app.paint_ms.iter().sum::<f32>() / paints,
                        app.paint_ms.iter().copied().fold(0.0, f32::max),
                        if app.box_draw { " (box)" } else { "" }
                    );
                    app.intervals.clear();
                    app.paint_ms.clear();
                    app.stats_at = Instant::now();
                }
            }
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, usize> for App {
    fn event(app: &mut Self, _: &wl_buffer::WlBuffer, event: wl_buffer::Event, index: &usize, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = event
            && let Some(buffer) = app.buffers.get_mut(*index)
        {
            buffer.busy = false;
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for App {
    fn event(app: &mut Self, seat: &wl_seat::WlSeat, event: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event {
            eprintln!("demo: seat capabilities {caps:?}");
            if caps.contains(wl_seat::Capability::Touch) {
                seat.get_touch(qh, ());
            }
            if caps.contains(wl_seat::Capability::Keyboard) {
                seat.get_keyboard(qh, ());
            }
            let _ = app;
        }
    }
}

impl Dispatch<wl_touch::WlTouch, ()> for App {
    fn event(app: &mut Self, _: &wl_touch::WlTouch, event: wl_touch::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_touch::Event::Down { id, x, y, .. } => {
                eprintln!("demo: touch down {id} at {x:.0},{y:.0}");
                let (x, y) = (x * f64::from(app.scale), y * f64::from(app.scale));
                app.touches.push((id, x, y));
                app.touch_at(x, y);
                app.dirty = true;
            }
            wl_touch::Event::Motion { id, x, y, .. } => {
                if let Some(touch) = app.touches.iter_mut().find(|touch| touch.0 == id) {
                    touch.1 = x * f64::from(app.scale);
                    touch.2 = y * f64::from(app.scale);
                }
                app.dirty = true;
            }
            wl_touch::Event::Up { id, .. } => {
                if let Some(position) = app.touches.iter().position(|touch| touch.0 == id) {
                    let (_, x, y) = app.touches.remove(position);
                    eprintln!("demo: touch up {id} at {x:.0},{y:.0}");
                    app.dots.push((x, y, Instant::now()));
                }
                app.dirty = true;
            }
            wl_touch::Event::Cancel => {
                eprintln!("demo: touch cancelled");
                app.touches.clear();
                app.dirty = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for App {
    fn event(app: &mut Self, _: &wl_keyboard::WlKeyboard, event: wl_keyboard::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_keyboard::Event::Keymap { size, .. } => eprintln!("demo: keymap of {size} bytes"),
            wl_keyboard::Event::Enter { .. } => eprintln!("demo: keyboard focus in"),
            wl_keyboard::Event::Leave { .. } => eprintln!("demo: keyboard focus out"),
            wl_keyboard::Event::Key { key, state: WEnum::Value(wl_keyboard::KeyState::Pressed), .. } => {
                match key {
                    28 => {
                        eprintln!("demo: key Enter");
                        app.focus_field(None);
                    }
                    14 => {
                        if let Some(index) = app.field_index() {
                            app.texts[index].pop();
                            app.dirty = true;
                        }
                    }
                    other => eprintln!("demo: key {other}"),
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<zwp_text_input_v3::ZwpTextInputV3, ()> for App {
    fn event(app: &mut Self, _: &zwp_text_input_v3::ZwpTextInputV3, event: zwp_text_input_v3::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            zwp_text_input_v3::Event::Enter { .. } => eprintln!("demo: text input entered"),
            zwp_text_input_v3::Event::Leave { .. } => eprintln!("demo: text input left"),
            zwp_text_input_v3::Event::CommitString { text } => {
                app.pending_text.push_str(&text.unwrap_or_default());
            }
            zwp_text_input_v3::Event::DeleteSurroundingText { before_length, .. } => {
                app.pending_delete += before_length as usize;
            }
            zwp_text_input_v3::Event::Done { .. } => {
                if let Some(index) = app.field_index() {
                    for _ in 0..std::mem::take(&mut app.pending_delete) {
                        app.texts[index].pop();
                    }
                    let text = std::mem::take(&mut app.pending_text);
                    app.texts[index].push_str(&text);
                    if FIELDS[index].0 == Field::Password {
                        eprintln!("demo: password field now {} characters", app.texts[index].chars().count());
                    } else {
                        eprintln!("demo: field {} = {:?}", FIELDS[index].1, app.texts[index]);
                    }
                    app.dirty = true;
                }
            }
            _ => {}
        }
    }
}

delegate_noop!(App: ignore wl_compositor::WlCompositor);
impl Dispatch<wl_surface::WlSurface, ()> for App {
    fn event(app: &mut Self, _: &wl_surface::WlSurface, event: wl_surface::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_surface::Event::PreferredBufferScale { factor } = event {
            eprintln!("demo: preferred buffer scale {factor}");
            app.scale = factor.max(1);
            app.resize();
        }
    }
}
delegate_noop!(App: ignore wl_shm::WlShm);
delegate_noop!(App: ignore wl_shm_pool::WlShmPool);
delegate_noop!(App: ignore zwp_text_input_manager_v3::ZwpTextInputManagerV3);
