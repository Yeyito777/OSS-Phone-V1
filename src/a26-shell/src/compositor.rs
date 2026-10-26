//! Server-side composition of Moon's shell window with XRender, presented at
//! vblank through the Present extension.
//!
//! The A26's Xorg has no 2D acceleration (modesetting, `AccelMethod none`,
//! a shadow framebuffer, no page flips). Composing every frame on the client
//! and pushing 10 MB through the X socket cost 13-27 ms per frame. Here the
//! client only describes a frame: a base layer plus a few nodes (scaled
//! layers, app snapshots, icons, solid cards). Xorg executes them with
//! pixman's optimized (NEON on the phone) paths, and only inside the damaged
//! rectangle. Static content is drawn once by Moon's CPU renderer and
//! uploaded once into server pixmaps (`create_layer`/`upload`); app content
//! comes from `capture_root` snapshots.
//!
//! Frames are rendered into one of two buffers and handed to Present, which
//! copies the damaged region to the window at the next vblank. A new frame is
//! started when the previous one completes, so animation runs locked to the
//! panel's refresh. Without Present (or if completions stop arriving), frames
//! are drawn straight into the window on a timer.
//!
//! Every request is one-way; the few capability queries happen in `main.rs`
//! before the event loop starts.

use std::error::Error;
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::present::{self, ConnectionExt as _};
use x11rb::protocol::render::{self, ConnectionExt as _, CreatePictureAux, PictOp, Transform};
use x11rb::protocol::xfixes::ConnectionExt as _;
use x11rb::protocol::xproto::{
    ConnectionExt as _, CreateGCAux, Gcontext, ImageFormat, Pixmap, Rectangle, SubwindowMode,
    Window,
};

use crate::gfx::{Canvas, RectF, Rgb};
use crate::present::{Damage, Uploader};

/// Picture formats resolved at startup.
#[derive(Debug, Clone, Copy)]
pub struct Formats {
    pub rgb: render::Pictformat,
    pub argb: render::Pictformat,
    pub a8: render::Pictformat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SurfaceId(usize);

/// One drawing step of a frame, composited over the base layer in order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Node {
    /// `src` of a surface, scaled into `dst`. `radius` rounds the corners;
    /// `smooth` selects bilinear filtering when scaled.
    Image {
        surface: SurfaceId,
        src: RectF,
        dst: RectF,
        alpha: f32,
        radius: f32,
        smooth: bool,
    },
    Fill {
        dst: RectF,
        color: Rgb,
        alpha: f32,
        radius: f32,
    },
    /// A full-screen background whose every row comes from one of several
    /// opaque surfaces (`Compositor::set_rows`); `generation` changes
    /// whenever any row does.
    Rows { generation: u32 },
}

/// Appends screen row `y` to a list of row runs, extending the last run
/// when adjacent.
fn push_row(runs: &mut Vec<Damage>, width: i32, y: i32) {
    match runs.last_mut() {
        Some(last) if last.y1 == y => last.y1 = y + 1,
        _ => runs.push(Damage {
            x0: 0,
            y0: y,
            x1: width,
            y1: y + 1,
        }),
    }
}

impl Node {
    fn bounds(&self, width: i32, height: i32) -> Option<Damage> {
        let dst = match self {
            Node::Image { dst, .. } | Node::Fill { dst, .. } => *dst,
            Node::Rows { .. } => RectF::new(0.0, 0.0, width as f32, height as f32),
        };
        rect_damage(dst, width, height)
    }

    fn alpha(&self) -> f32 {
        match self {
            Node::Image { alpha, .. } | Node::Fill { alpha, .. } => *alpha,
            Node::Rows { .. } => 1.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// Drawn first everywhere; None when nodes tile the whole screen.
    pub base: Option<SurfaceId>,
    pub nodes: Vec<Node>,
}

fn rect_damage(rect: RectF, width: i32, height: i32) -> Option<Damage> {
    let damage = Damage {
        x0: (rect.x.floor() as i32 - 1).max(0),
        y0: (rect.y.floor() as i32 - 1).max(0),
        x1: ((rect.x + rect.w).ceil() as i32 + 1).min(width),
        y1: ((rect.y + rect.h).ceil() as i32 + 1).min(height),
    };
    (damage.x0 < damage.x1 && damage.y0 < damage.y1).then_some(damage)
}

fn intersect(a: Damage, b: Damage) -> Option<Damage> {
    let d = Damage {
        x0: a.x0.max(b.x0),
        y0: a.y0.max(b.y0),
        x1: a.x1.min(b.x1),
        y1: a.y1.min(b.y1),
    };
    (d.x0 < d.x1 && d.y0 < d.y1).then_some(d)
}

fn union(a: Option<Damage>, b: Option<Damage>) -> Option<Damage> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.union(b)),
        (a, None) => a,
        (None, b) => b,
    }
}

fn rectangle(d: Damage) -> Rectangle {
    Rectangle {
        x: d.x0 as i16,
        y: d.y0 as i16,
        width: (d.x1 - d.x0) as u16,
        height: (d.y1 - d.y0) as u16,
    }
}

fn area(damage: Option<Damage>) -> i64 {
    damage.map_or(0, |d| i64::from(d.x1 - d.x0) * i64::from(d.y1 - d.y0))
}

fn fixed(value: f32) -> i32 {
    (f64::from(value) * 65536.0).round() as i32
}

/// Premultiplied 16-bit Render color.
fn render_color(color: Rgb, alpha: f32) -> render::Color {
    let a = alpha.clamp(0.0, 1.0);
    let channel = |shift: u32| ((((color >> shift) & 0xff) as f32 / 255.0) * a * 65535.0) as u16;
    render::Color {
        red: channel(16),
        green: channel(8),
        blue: channel(0),
        alpha: (a * 65535.0) as u16,
    }
}

struct Surface {
    pixmap: Pixmap,
    picture: render::Picture,
    width: u16,
    height: u16,
    uploader: Option<Uploader>,
    /// Damage uploaded since the last frame that used this surface.
    pending: Option<Damage>,
    transform: [i32; 4],
    bilinear: bool,
    /// No alpha channel (XRGB): drawn with SRC when fully opaque.
    opaque: bool,
    /// Edge pixels extend outward (RepeatPad), which keeps scaled draws on
    /// pixman's fast bilinear paths.
    padded: bool,
    /// For transparent layers: the 32-pixel tiles (merged into runs) that
    /// hold any visible pixel, and their bounding box, so a mostly empty
    /// full-screen layer composites only its content.
    content: Option<Vec<Damage>>,
    content_bounds: Option<Damage>,
    /// The tiles grouped into a few horizontal bands (each with its own
    /// horizontal extent), for compositing a many-tile layer cheaply.
    content_bands: Vec<Damage>,
    /// A copy pre-scaled to (w, h) with bilinear filtering, used instead of
    /// scaling every frame when a card rests at that size (app switcher).
    scaled: Option<(u16, u16, Pixmap, render::Picture)>,
}

struct Buffer {
    pixmap: Pixmap,
    picture: render::Picture,
    idle: bool,
    /// Holds no valid frame yet (fresh, or after an expose).
    stale: bool,
}

struct InFlight {
    serial: u32,
    buffer: usize,
    sent: Instant,
    damage_area: i64,
    /// Of which outside changed background rows.
    foreground_area: i64,
    build: Duration,
    /// Submitted right after the previous frame completed: only then is a
    /// late completion a missed vblank (otherwise Moon simply had nothing
    /// new to show).
    back_to_back: bool,
    /// Refreshes after the previous completion this frame targeted.
    target_delta: u64,
}

/// Timing of presented frames, for `MOON_PROFILE` and on-device tuning.
#[derive(Debug, Default, Clone)]
pub struct FrameStats {
    pub frames: u32,
    pub missed_vblanks: u64,
    /// Frames paced at 60 Hz because they did not fit a 120 Hz refresh.
    pub half_rate_frames: u32,
    pub intervals_ms: Vec<f32>,
    pub latency_ms: Vec<f32>,
}

impl FrameStats {
    pub fn summary(&self) -> String {
        let stat = |values: &[f32]| {
            if values.is_empty() {
                return "-".to_owned();
            }
            let mut sorted = values.to_vec();
            sorted.sort_by(f32::total_cmp);
            let mean = sorted.iter().sum::<f32>() / sorted.len() as f32;
            let p95 = sorted[((sorted.len() as f32 * 0.95) as usize).min(sorted.len() - 1)];
            format!(
                "mean {mean:.2} p95 {p95:.2} max {:.2}",
                sorted[sorted.len() - 1]
            )
        };
        format!(
            "{} frames ({} paced at 60 Hz), {} missed vblanks; back-to-back interval ms {}; submit-to-scanout ms {}",
            self.frames,
            self.half_rate_frames,
            self.missed_vblanks,
            stat(&self.intervals_ms),
            stat(&self.latency_ms),
        )
    }
}

pub struct Compositor {
    window: Window,
    root: Window,
    width: u16,
    height: u16,
    formats: Formats,
    window_picture: render::Picture,
    gc: Gcontext,
    capture_gc: Gcontext,
    surfaces: Vec<Option<Surface>>,
    buffers: Vec<Buffer>,
    present: Option<PresentState>,
    alpha_masks: Vec<render::Picture>,
    corners: Vec<(u16, [render::Picture; 4])>,
    last: Option<Frame>,
    force_full: bool,
    next_direct_frame: Instant,
    profile: bool,
    /// The damage rectangles the current frame is clipped to.
    clip: Vec<Damage>,
    /// Diagnostics (`ctl profile skip MASK`): frame nodes not to draw.
    pub skip_nodes: u64,
    /// The surface each screen row of a `Node::Rows` background shows.
    rows: Vec<SurfaceId>,
    rows_generation: u32,
    /// Rows whose surface changed since the last composed frame.
    row_changes: Vec<Damage>,
    /// Pixels the last frame redrew besides changed background rows.
    pub foreground_area: i64,
    /// MOON_DEBUG_FRAMES=1 logs every frame's node list.
    pub debug_frames: bool,
    pub stats: FrameStats,
}

struct PresentState {
    region: u32,
    serial: u32,
    in_flight: Option<InFlight>,
    last_msc: u64,
    last_ust: u64,
    period_us: u64,
    lost: u32,
    last_complete_at: Option<Instant>,
    /// Vblanks per frame: 1 (120 Hz), or 2 while an animation's frames do
    /// not fit a refresh, so motion stays evenly paced instead of
    /// alternating between one and two refreshes.
    divisor: u64,
    /// Late/on-time history of the last back-to-back frames (1 = late).
    late_history: u8,
    /// Frames since entering 60 Hz pacing, and consecutive probes (single
    /// refresh targets) that made it in time.
    half_rate_count: u32,
    probe_successes: u8,
    /// Damage of the last submitted frame, which predicts the next one's.
    last_damage_area: i64,
}

/// Damage that reliably fits a 120 Hz refresh on the A26's Xorg (a frame's
/// cost is mostly copying its damaged pixels, twice).
const SURE_FIT_AREA: i64 = 450_000;

impl PresentState {
    /// Refreshes the next frame targets. At 60 Hz pacing every sixth frame
    /// probes a single refresh, to return to 120 Hz once frames fit again;
    /// so does every frame after a small one (an animation's cheap tail).
    fn next_target_delta(&self) -> u64 {
        if self.divisor == 2
            && (self.half_rate_count + 1) % 6 != 0
            && self.last_damage_area > SURE_FIT_AREA
        {
            2
        } else {
            1
        }
    }
}

/// How long a submitted frame may go unconfirmed before Moon assumes Present
/// lost it (for example while DPMS has the CRTC off) and recovers.
const PRESENT_WATCHDOG: Duration = Duration::from_millis(120);
/// Frame interval of the timer fallback (120 Hz).
const DIRECT_FRAME_INTERVAL: Duration = Duration::from_micros(8_333);
/// Above this many exact (row) rectangles a frame redraws everything.
const MAX_EXACT_RECTS: usize = 1200;
/// A layer with more content tiles than this damages its bounds instead.
const MAX_TILE_DAMAGE: usize = 1024;
/// Corner radii with pre-rendered masks.
const CORNER_RADII: std::ops::RangeInclusive<u16> = 4..=72;

impl Compositor {
    #[allow(clippy::too_many_arguments)]
    pub fn new<C: Connection>(
        conn: &C,
        window: Window,
        root: Window,
        width: u16,
        height: u16,
        formats: Formats,
        present_event: Option<u32>,
        profile: bool,
    ) -> Result<Self, Box<dyn Error>> {
        let window_picture = conn.generate_id()?;
        conn.render_create_picture(
            window_picture,
            window,
            formats.rgb,
            &CreatePictureAux::new(),
        )?;
        let gc = conn.generate_id()?;
        conn.create_gc(gc, window, &CreateGCAux::new().graphics_exposures(0))?;
        let capture_gc = conn.generate_id()?;
        conn.create_gc(
            capture_gc,
            root,
            &CreateGCAux::new()
                .graphics_exposures(0)
                .subwindow_mode(SubwindowMode::INCLUDE_INFERIORS),
        )?;

        let mut buffers = Vec::new();
        let present = match present_event {
            Some(eid) => {
                // One buffer: the next frame starts only after Present has
                // copied the previous one, so the buffer always holds the
                // last frame and each frame redraws only what changed.
                for _ in 0..1 {
                    let pixmap = conn.generate_id()?;
                    conn.create_pixmap(24, pixmap, window, width, height)?;
                    let picture = conn.generate_id()?;
                    conn.render_create_picture(
                        picture,
                        pixmap,
                        formats.rgb,
                        &CreatePictureAux::new(),
                    )?;
                    buffers.push(Buffer {
                        pixmap,
                        picture,
                        idle: true,
                        stale: true,
                    });
                }
                conn.present_select_input(
                    eid,
                    window,
                    present::EventMask::COMPLETE_NOTIFY | present::EventMask::IDLE_NOTIFY,
                )?;
                let region = conn.generate_id()?;
                conn.xfixes_create_region(region, &[])?;
                Some(PresentState {
                    region,
                    serial: 0,
                    in_flight: None,
                    last_msc: 0,
                    last_ust: 0,
                    period_us: 8_333,
                    lost: 0,
                    last_complete_at: None,
                    divisor: 1,
                    late_history: 0,
                    half_rate_count: 0,
                    probe_successes: 0,
                    last_damage_area: 0,
                })
            }
            None => None,
        };

        let mut compositor = Self {
            window,
            root,
            width,
            height,
            formats,
            window_picture,
            gc,
            capture_gc,
            surfaces: Vec::new(),
            buffers,
            present,
            alpha_masks: Vec::new(),
            corners: Vec::new(),
            last: None,
            force_full: true,
            next_direct_frame: Instant::now(),
            profile,
            skip_nodes: 0,
            rows: Vec::new(),
            rows_generation: 0,
            row_changes: Vec::new(),
            foreground_area: 0,
            clip: Vec::new(),
            debug_frames: std::env::var_os("MOON_DEBUG_FRAMES").is_some(),
            stats: FrameStats::default(),
        };
        for step in 0..=64_u32 {
            let picture = conn.generate_id()?;
            let a = (step * 65535 / 64) as u16;
            conn.render_create_solid_fill(
                picture,
                render::Color {
                    red: a,
                    green: a,
                    blue: a,
                    alpha: a,
                },
            )?;
            compositor.alpha_masks.push(picture);
        }
        for radius in CORNER_RADII.step_by(2) {
            let mut masks = [0; 4];
            for (corner, mask) in masks.iter_mut().enumerate() {
                *mask = compositor.corner_mask(conn, radius, corner as u8)?;
            }
            compositor.corners.push((radius, masks));
        }
        Ok(compositor)
    }

    pub fn set_profile(&mut self, enabled: bool) {
        self.profile = enabled;
    }

    pub fn present_enabled(&self) -> bool {
        self.present.is_some()
    }

    /// An A8 mask that is opaque outside a rounded corner of `radius` (the
    /// part of a card's bounding box that must show what is behind it).
    /// `corner`: 0 top-left, 1 top-right, 2 bottom-left, 3 bottom-right.
    fn corner_mask<C: Connection>(
        &self,
        conn: &C,
        radius: u16,
        corner: u8,
    ) -> Result<render::Picture, Box<dyn Error>> {
        let size = radius;
        let stride = usize::from(size).div_ceil(4) * 4;
        let mut data = vec![0_u8; stride * usize::from(size)];
        let r = f32::from(radius);
        for y in 0..size {
            for x in 0..size {
                // Distance from the arc center, in the top-left orientation.
                let (fx, fy) = match corner {
                    0 => (x, y),
                    1 => (size - 1 - x, y),
                    2 => (x, size - 1 - y),
                    _ => (size - 1 - x, size - 1 - y),
                };
                let dx = r - (f32::from(fx) + 0.5);
                let dy = r - (f32::from(fy) + 0.5);
                let distance = (dx * dx + dy * dy).sqrt();
                let outside = (distance - r + 0.5).clamp(0.0, 1.0);
                data[usize::from(y) * stride + usize::from(x)] = (outside * 255.0 + 0.5) as u8;
            }
        }
        let pixmap = conn.generate_id()?;
        conn.create_pixmap(8, pixmap, self.window, size, size)?;
        let gc = conn.generate_id()?;
        conn.create_gc(gc, pixmap, &CreateGCAux::new().graphics_exposures(0))?;
        conn.put_image(
            ImageFormat::Z_PIXMAP,
            pixmap,
            gc,
            size,
            size,
            0,
            0,
            0,
            8,
            &data,
        )?;
        conn.free_gc(gc)?;
        let picture = conn.generate_id()?;
        conn.render_create_picture(picture, pixmap, self.formats.a8, &CreatePictureAux::new())?;
        conn.free_pixmap(pixmap)?;
        Ok(picture)
    }

    fn insert(&mut self, surface: Surface) -> SurfaceId {
        if let Some(index) = self.surfaces.iter().position(Option::is_none) {
            self.surfaces[index] = Some(surface);
            SurfaceId(index)
        } else {
            self.surfaces.push(Some(surface));
            SurfaceId(self.surfaces.len() - 1)
        }
    }

    /// A surface whose content is drawn on the client and uploaded with
    /// `upload`. Transparent layers hold premultiplied ARGB.
    pub fn create_layer<C: Connection>(
        &mut self,
        conn: &C,
        width: u16,
        height: u16,
        transparent: bool,
    ) -> Result<SurfaceId, Box<dyn Error>> {
        let depth = if transparent { 32 } else { 24 };
        let uploader = Uploader::create(conn, self.window, depth, width, height)?;
        let picture = conn.generate_id()?;
        let format = if transparent {
            self.formats.argb
        } else {
            self.formats.rgb
        };
        conn.render_create_picture(picture, uploader.pixmap, format, &CreatePictureAux::new())?;
        Ok(self.insert(Surface {
            pixmap: uploader.pixmap,
            picture,
            width,
            height,
            uploader: Some(uploader),
            pending: None,
            transform: [0x10000, 0, 0, 0x10000],
            bilinear: false,
            opaque: !transparent,
            padded: false,
            content: None,
            content_bounds: None,
            content_bands: Vec::new(),
            scaled: None,
        }))
    }

    /// Uploads the changed parts of `canvas` into a layer.
    pub fn upload<C: Connection>(
        &mut self,
        conn: &C,
        id: SurfaceId,
        canvas: &Canvas,
    ) -> Result<Vec<Damage>, Box<dyn Error>> {
        let Some(surface) = self.surfaces.get_mut(id.0).and_then(Option::as_mut) else {
            return Err("unknown layer".into());
        };
        let Some(uploader) = surface.uploader.as_mut() else {
            return Err("surface is not an uploaded layer".into());
        };
        let damage = uploader.upload(conn, canvas)?;
        if !surface.opaque && !damage.is_empty() {
            let tiles = content_tiles(canvas);
            surface.content_bounds = tiles.iter().copied().reduce(Damage::union);
            surface.content_bands = components_of(&tiles);
            surface.content = Some(tiles);
        }
        for damage in &damage {
            surface.pending = union(surface.pending, Some(*damage));
        }
        Ok(damage)
    }

    /// A full-screen surface for an app snapshot (see `capture_root`).
    pub fn create_snapshot<C: Connection>(
        &mut self,
        conn: &C,
    ) -> Result<SurfaceId, Box<dyn Error>> {
        let pixmap = conn.generate_id()?;
        conn.create_pixmap(24, pixmap, self.window, self.width, self.height)?;
        let picture = conn.generate_id()?;
        conn.render_create_picture(picture, pixmap, self.formats.rgb, &CreatePictureAux::new())?;
        Ok(self.insert(Surface {
            pixmap,
            picture,
            width: self.width,
            height: self.height,
            uploader: None,
            pending: None,
            transform: [0x10000, 0, 0, 0x10000],
            bilinear: false,
            opaque: true,
            padded: false,
            content: None,
            content_bounds: None,
            content_bands: Vec::new(),
            scaled: None,
        }))
    }

    /// Copies what is on screen right now (every window, as composed by the
    /// server) into a snapshot surface. Issue it before restacking.
    pub fn capture_root<C: Connection>(
        &mut self,
        conn: &C,
        id: SurfaceId,
    ) -> Result<(), Box<dyn Error>> {
        self.drop_prescaled(conn, id);
        let Some(surface) = self.surfaces.get_mut(id.0).and_then(Option::as_mut) else {
            return Err("unknown snapshot".into());
        };
        conn.copy_area(
            self.root,
            surface.pixmap,
            self.capture_gc,
            0,
            0,
            0,
            0,
            surface.width,
            surface.height,
        )?;
        surface.pending = Some(Damage {
            x0: 0,
            y0: 0,
            x1: i32::from(surface.width),
            y1: i32::from(surface.height),
        });
        Ok(())
    }

    pub fn free<C: Connection>(&mut self, conn: &C, id: SurfaceId) {
        self.drop_prescaled(conn, id);
        if let Some(surface) = self.surfaces.get_mut(id.0).and_then(Option::take) {
            let _ = conn.render_free_picture(surface.picture);
            match surface.uploader {
                Some(uploader) => uploader.destroy(conn),
                None => {
                    let _ = conn.free_pixmap(surface.pixmap);
                }
            }
        }
        self.force_full = true;
    }

    /// The window lost its contents (expose); the next frame redraws and
    /// presents everything.
    pub fn invalidate(&mut self) {
        self.force_full = true;
        for buffer in &mut self.buffers {
            buffer.stale = true;
        }
    }

    /// Whether a new frame may be submitted now. With Present this waits for
    /// the previous frame to reach the screen (one frame per vblank).
    pub fn ready(&mut self, now: Instant) -> bool {
        match self.present.as_mut() {
            Some(present) => match &present.in_flight {
                None => true,
                Some(in_flight) if now.duration_since(in_flight.sent) >= PRESENT_WATCHDOG => {
                    present.lost += 1;
                    let buffer = in_flight.buffer;
                    present.in_flight = None;
                    self.buffers[buffer].idle = true;
                    if present.lost >= 3 {
                        eprintln!("compositor: Present stopped completing; drawing directly");
                        self.disable_present();
                    }
                    true
                }
                Some(_) => false,
            },
            None => now >= self.next_direct_frame,
        }
    }

    fn disable_present(&mut self) {
        self.present = None;
        self.force_full = true;
    }

    /// When the loop should wake to submit or recover the next frame.
    pub fn next_deadline(&self) -> Option<Instant> {
        match &self.present {
            Some(present) => present
                .in_flight
                .as_ref()
                .map(|in_flight| in_flight.sent + PRESENT_WATCHDOG),
            None => Some(self.next_direct_frame),
        }
    }

    /// Best estimate of when the next submitted frame reaches the panel; the
    /// time every animation should be sampled at.
    pub fn next_display_time(&self, now: Instant) -> Instant {
        match &self.present {
            Some(present) if present.last_ust > 0 => {
                let now_us = monotonic_micros();
                let period = present.period_us.max(1);
                // The first vblank after now (frames are submitted right
                // after a completion, so this is normally last + 1 period).
                let mut next = present.last_ust + period * present.next_target_delta();
                while next + 500 < now_us {
                    next += period;
                }
                now + Duration::from_micros(next.saturating_sub(now_us))
            }
            Some(_) => now + DIRECT_FRAME_INTERVAL,
            None => now,
        }
    }

    pub fn frame_in_flight(&self) -> bool {
        self.present
            .as_ref()
            .is_some_and(|present| present.in_flight.is_some())
    }

    /// Handles Present events; returns true when the event was Moon's.
    pub fn handle_event(&mut self, event: &Event) -> bool {
        let Some(present) = self.present.as_mut() else {
            return false;
        };
        match event {
            Event::PresentCompleteNotify(event) if event.window == self.window => {
                if event.kind != present::CompleteKind::PIXMAP {
                    return true;
                }
                if let Some(in_flight) = present
                    .in_flight
                    .take_if(|in_flight| in_flight.serial == event.serial)
                {
                    present.lost = 0;
                    // A copy-mode completion means the pixmap was read.
                    self.buffers[in_flight.buffer].idle = true;
                    if present.last_msc > 0 && event.msc > present.last_msc {
                        let delta = event.msc - present.last_msc;
                        let interval_us = event.ust.saturating_sub(present.last_ust);
                        if delta == 1 && interval_us > 2_000 && interval_us < 40_000 {
                            // Track the panel's real refresh period.
                            present.period_us = (present.period_us * 7 + interval_us) / 8;
                        }
                        let interval_ms = interval_us as f32 / 1000.0;
                        let latency_ms = in_flight.sent.elapsed().as_secs_f32() * 1000.0;
                        if self.stats.latency_ms.len() < 4096 {
                            self.stats.frames += 1;
                            self.stats.latency_ms.push(latency_ms);
                            if in_flight.back_to_back {
                                self.stats.missed_vblanks +=
                                    delta.saturating_sub(in_flight.target_delta);
                                self.stats.intervals_ms.push(interval_ms);
                                if in_flight.target_delta == 2 {
                                    self.stats.half_rate_frames += 1;
                                }
                            }
                        }
                        if self.profile {
                            eprintln!(
                                "present msc +{delta} interval {interval_ms:.2} ms submit-to-scanout {latency_ms:.2} ms build {:.2} ms damage {} px fg {} px{}",
                                in_flight.build.as_secs_f64() * 1000.0,
                                in_flight.damage_area,
                                in_flight.foreground_area,
                                if in_flight.back_to_back {
                                    ""
                                } else {
                                    " (after idle)"
                                },
                            );
                        }
                    }
                    if in_flight.back_to_back && present.last_msc > 0 {
                        let late = event.msc > present.last_msc + in_flight.target_delta;
                        if in_flight.target_delta == 1 && present.divisor == 2 {
                            // A probe at full rate while paced at 60 Hz.
                            present.probe_successes =
                                if late { 0 } else { present.probe_successes + 1 };
                            if present.probe_successes >= 2 {
                                present.divisor = 1;
                                present.late_history = 0;
                            }
                        } else if present.divisor == 1 {
                            present.late_history = (present.late_history << 1) | u8::from(late);
                            if present.late_history.count_ones() >= 3 {
                                present.divisor = 2;
                                present.half_rate_count = 0;
                                present.probe_successes = 0;
                            }
                        }
                    }
                    present.last_msc = event.msc;
                    present.last_ust = event.ust;
                    present.last_complete_at = Some(Instant::now());
                }
                true
            }
            Event::PresentIdleNotify(event) if event.window == self.window => {
                for buffer in &mut self.buffers {
                    if buffer.pixmap == event.pixmap {
                        buffer.idle = true;
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// Composes `frame` and submits it. Only the area that differs from the
    /// previous frame is redrawn and presented. Returns false when nothing
    /// needed to be drawn.
    pub fn compose<C: Connection>(
        &mut self,
        conn: &C,
        frame: Frame,
    ) -> Result<bool, Box<dyn Error>> {
        let started = Instant::now();
        let row_changes = std::mem::take(&mut self.row_changes);
        let (w, h) = (i32::from(self.width), i32::from(self.height));
        let full = Damage {
            x0: 0,
            y0: 0,
            x1: w,
            y1: h,
        };
        let structure_changed = self.last.as_ref().is_none_or(|last| {
            last.base != frame.base
                || last.nodes.len() != frame.nodes.len()
                || last
                    .nodes
                    .iter()
                    .zip(&frame.nodes)
                    .any(|(a, b)| std::mem::discriminant(a) != std::mem::discriminant(b))
        });
        // Damage is a list of rectangles: for unscaled layers, the content
        // tiles at their old and new positions. Copies and redraws then
        // touch only pixels that changed, which on this unaccelerated Xorg
        // is what a frame costs.
        let mut damage: Vec<Damage> = Vec::new();
        // Background rows, kept exact (the grid would widen them to cells).
        let mut exact: Vec<Damage> = Vec::new();
        if self.force_full || structure_changed {
            damage.push(full);
        } else {
            damage.extend(frame.base.and_then(|base| self.pending(base)));
            let last = self.last.as_ref().expect("checked above");
            for (old, new) in last.nodes.iter().zip(&frame.nodes) {
                if let (Node::Rows { .. }, Node::Rows { .. }) = (old, new) {
                    // The rows that changed copy, and those whose copy was
                    // redrawn while on screen.
                    exact.extend(row_changes.iter().copied());
                    for surface in self.row_surfaces() {
                        if let Some(pending) = self.pending(surface) {
                            exact.extend(
                                self.rows_showing(surface)
                                    .into_iter()
                                    .filter_map(|run| intersect(run, pending)),
                            );
                        }
                    }
                    continue;
                }
                if old != new {
                    self.node_damage(old, &mut damage);
                    self.node_damage(new, &mut damage);
                    continue;
                }
                let Node::Image {
                    surface,
                    src,
                    dst,
                    alpha,
                    ..
                } = *new
                else {
                    continue;
                };
                let Some(pending) = self.pending(surface) else {
                    continue;
                };
                if alpha <= 0.0 {
                    continue;
                }
                let untransformed = (src.w - dst.w).abs() < 1e-3 && (src.h - dst.h).abs() < 1e-3;
                let changed = if untransformed {
                    // Only the uploaded part of an unscaled layer changed.
                    let (dx, dy) = (
                        (dst.x - src.x).round() as i32,
                        (dst.y - src.y).round() as i32,
                    );
                    Some(Damage {
                        x0: pending.x0 + dx,
                        y0: pending.y0 + dy,
                        x1: pending.x1 + dx,
                        y1: pending.y1 + dy,
                    })
                    .and_then(|d| self.node_bounds(new).and_then(|b| intersect(d, b)))
                } else {
                    self.node_bounds(new)
                };
                damage.extend(changed);
            }
        }
        // Snap to a 32-pixel grid and coalesce: overlapping rectangles at
        // different offsets would otherwise fragment the server's clip
        // region into thousands of tiny boxes, each costing setup time.
        let mut damage = grid_cells(&damage, w, h);
        self.foreground_area = damage.iter().map(|d| area(Some(*d))).sum();
        if exact.len() > MAX_EXACT_RECTS {
            damage = vec![full];
        } else {
            damage.extend(exact);
        }

        let (target, buffer_index) = match &self.present {
            Some(_) => {
                let index = (0..self.buffers.len()).find(|i| self.buffers[*i].idle);
                let Some(index) = index else {
                    // Both buffers are still owned by the server.
                    if self.profile {
                        eprintln!("compositor: both buffers busy");
                    }
                    return Ok(false);
                };
                (self.buffers[index].picture, Some(index))
            }
            None => (self.window_picture, None),
        };
        let rects = match buffer_index {
            Some(index) if self.buffers[index].stale => vec![full],
            _ => damage,
        };
        let Some(redraw) = rects.iter().copied().reduce(Damage::union) else {
            self.last = Some(frame);
            return Ok(false);
        };
        let clip: Vec<Rectangle> = rects.iter().map(|d| rectangle(*d)).collect();
        let damage_area: i64 = rects.iter().map(|d| area(Some(*d))).sum();

        if self.debug_frames {
            eprintln!("frame redraw {redraw:?} base {:?}", frame.base);
            for node in &frame.nodes {
                eprintln!("  {node:?}");
            }
        }
        conn.render_set_picture_clip_rectangles(target, 0, 0, &clip)?;
        self.clip = rects.clone();
        // Nothing beneath the topmost opaque card needs drawing: the base
        // and earlier nodes are drawn only around its interior.
        let occluder = (0..frame.nodes.len()).rev().find_map(|index| {
            self.opaque_interior(&frame.nodes[index])
                .map(|interior| (index, interior))
        });
        let around: Vec<Damage> = match occluder {
            Some((_, interior)) => subtract(redraw, interior),
            None => vec![redraw],
        };
        for part in &around {
            if let Some(base) = frame.base {
                self.draw_base(conn, base, target, *part, None)?;
            }
        }
        for index in 0..frame.nodes.len() {
            if index < 64 && self.skip_nodes & (1 << index) != 0 {
                continue;
            }
            let node = frame.nodes[index];
            let Some(bounds) = self.node_bounds(&node).and_then(|b| intersect(b, redraw)) else {
                continue;
            };
            if occluder.is_some_and(|(top, _)| index < top) {
                for part in &around {
                    if let Some(piece) = intersect(bounds, *part) {
                        self.draw_node(conn, &node, target, piece, None)?;
                    }
                }
                continue;
            }
            self.draw_node(conn, &node, target, bounds, None)?;
            if let Node::Image {
                src, dst, alpha, ..
            } = node
                && alpha < 0.999
                && ((src.w - dst.w).abs() > 1e-3 || (src.h - dst.h).abs() > 1e-3)
            {
                // Fade a scaled image: re-cover it with what lies beneath at
                // (1 - alpha), which stays on fast paths.
                let cover = self.alpha_masks[(((1.0 - alpha) * 64.0).round() as usize).min(64)];
                self.redraw_beneath(
                    conn, &frame, index, target, bounds, cover, bounds.x0, bounds.y0,
                )?;
            }
            let radius = match node {
                Node::Image { radius, .. } | Node::Fill { radius, .. } => radius,
                Node::Rows { .. } => 0.0,
            };
            if radius >= 1.0 {
                self.repair_corners(conn, &frame, index, target, redraw)?;
            }
        }

        // Surfaces used by this frame have now drawn their uploads.
        let used: Vec<SurfaceId> = frame
            .base
            .into_iter()
            .chain(frame.nodes.iter().flat_map(|node| match node {
                Node::Image { surface, .. } => vec![*surface],
                Node::Rows { .. } => self.row_surfaces(),
                Node::Fill { .. } => Vec::new(),
            }))
            .collect();
        for id in used {
            if let Some(surface) = self.surfaces.get_mut(id.0).and_then(Option::as_mut) {
                surface.pending = None;
            }
        }

        let build = started.elapsed();
        match (self.present.as_mut(), buffer_index) {
            (Some(present), Some(index)) => {
                present.serial = present.serial.wrapping_add(1);
                let back_to_back = present
                    .last_complete_at
                    .is_some_and(|at| Instant::now().duration_since(at) < Duration::from_millis(4));
                if !back_to_back {
                    // A new animation starts at full rate again.
                    present.divisor = 1;
                    present.late_history = 0;
                }
                let target_delta = present.next_target_delta();
                if present.divisor == 2 {
                    present.half_rate_count += 1;
                }
                present.last_damage_area = damage_area;
                conn.xfixes_set_region(present.region, &clip)?;
                conn.present_pixmap(
                    self.window,
                    self.buffers[index].pixmap,
                    present.serial,
                    x11rb::NONE,
                    present.region,
                    0,
                    0,
                    x11rb::NONE,
                    x11rb::NONE,
                    x11rb::NONE,
                    present::Option::NONE.into(),
                    // The vblank after the last completed frame. If it has
                    // already passed (a late frame), the copy happens at once
                    // rather than waiting another refresh.
                    if present.last_msc > 0 {
                        present.last_msc + target_delta
                    } else {
                        0
                    },
                    0,
                    0,
                    &[],
                )?;
                let buffer = &mut self.buffers[index];
                buffer.idle = false;
                buffer.stale = false;
                let sent = Instant::now();
                present.in_flight = Some(InFlight {
                    serial: present.serial,
                    buffer: index,
                    sent,
                    damage_area,
                    foreground_area: self.foreground_area,
                    build,
                    back_to_back,
                    target_delta,
                });
            }
            _ => {
                self.next_direct_frame = Instant::now() + DIRECT_FRAME_INTERVAL;
                if self.profile {
                    eprintln!(
                        "direct frame build {:.2} ms damage {} px",
                        build.as_secs_f64() * 1000.0,
                        damage_area
                    );
                }
            }
        }
        conn.flush()?;
        self.force_full = false;
        self.last = Some(frame);
        Ok(true)
    }

    fn surface(&self, id: SurfaceId) -> Option<&Surface> {
        self.surfaces.get(id.0).and_then(Option::as_ref)
    }

    /// The node's screen bounds, shrunk to a layer's content when the layer
    /// is drawn unscaled.
    fn node_bounds(&self, node: &Node) -> Option<Damage> {
        let bounds = node.bounds(i32::from(self.width), i32::from(self.height))?;
        let Node::Image {
            surface, src, dst, ..
        } = *node
        else {
            return Some(bounds);
        };
        let unscaled = (src.w - dst.w).abs() < 1e-3 && (src.h - dst.h).abs() < 1e-3;
        match self.surface(surface).and_then(|s| s.content_bounds) {
            Some(content) if unscaled => {
                let (dx, dy) = (
                    (dst.x - src.x).round() as i32,
                    (dst.y - src.y).round() as i32,
                );
                intersect(
                    Damage {
                        x0: content.x0 + dx,
                        y0: content.y0 + dy,
                        x1: content.x1 + dx,
                        y1: content.y1 + dy,
                    },
                    bounds,
                )
            }
            _ => Some(bounds),
        }
    }

    /// Appends what a node covers: its content tiles for an unscaled layer,
    /// otherwise its bounds.
    fn node_damage(&self, node: &Node, out: &mut Vec<Damage>) {
        // An invisible node drew nothing and covers nothing.
        if let Node::Image { alpha, .. } | Node::Fill { alpha, .. } = *node
            && alpha <= 0.0
        {
            return;
        }
        let Some(bounds) = self.node_bounds(node) else {
            return;
        };
        if let Node::Image {
            surface,
            src,
            dst,
            alpha,
            ..
        } = *node
            && alpha > 0.0
            && (src.w - dst.w).abs() < 1e-3
            && (src.h - dst.h).abs() < 1e-3
            && let Some(tiles) = self.content(surface)
            && tiles.len() <= MAX_TILE_DAMAGE
        {
            let (dx, dy) = (
                (dst.x - src.x).round() as i32,
                (dst.y - src.y).round() as i32,
            );
            out.extend(tiles.iter().filter_map(|t| {
                intersect(
                    Damage {
                        x0: t.x0 + dx,
                        y0: t.y0 + dy,
                        x1: t.x1 + dx,
                        y1: t.y1 + dy,
                    },
                    bounds,
                )
            }));
            return;
        }
        out.push(bounds);
    }

    /// The whole pixels a node covers opaquely, if it does.
    fn opaque_interior(&self, node: &Node) -> Option<Damage> {
        let (dst, alpha) = match *node {
            Node::Rows { .. } => {
                if self.rows.is_empty()
                    || !self
                        .row_surfaces()
                        .into_iter()
                        .all(|id| self.surface(id).is_some_and(|s| s.opaque))
                {
                    return None;
                }
                (
                    RectF::new(0.0, 0.0, f32::from(self.width), f32::from(self.height)),
                    1.0,
                )
            }
            Node::Image {
                surface,
                dst,
                alpha,
                ..
            } => {
                if !self.surface(surface).is_some_and(|s| s.opaque) {
                    return None;
                }
                (dst, alpha)
            }
            Node::Fill { dst, alpha, .. } => (dst, alpha),
        };
        if alpha < 0.999 {
            return None;
        }
        let interior = Damage {
            x0: dst.x.ceil() as i32,
            y0: dst.y.ceil() as i32,
            x1: (dst.x + dst.w).floor() as i32,
            y1: (dst.y + dst.h).floor() as i32,
        };
        (interior.x1 - interior.x0 > 64 && interior.y1 - interior.y0 > 64).then_some(interior)
    }

    /// Renders a copy of `id` scaled to `width` x `height` (bilinear, once,
    /// server-side) so cards resting at that size draw without scaling.
    pub fn prescale<C: Connection>(
        &mut self,
        conn: &C,
        id: SurfaceId,
        width: u16,
        height: u16,
    ) -> Result<(), Box<dyn Error>> {
        let Some(surface) = self.surface(id) else {
            return Ok(());
        };
        if surface
            .scaled
            .is_some_and(|(w, h, _, _)| (w, h) == (width, height))
        {
            return Ok(());
        }
        let (source_w, source_h, opaque) = (surface.width, surface.height, surface.opaque);
        let format = if opaque {
            self.formats.rgb
        } else {
            self.formats.argb
        };
        let depth = if opaque { 24 } else { 32 };
        let pixmap = conn.generate_id()?;
        conn.create_pixmap(depth, pixmap, self.window, width, height)?;
        let picture = conn.generate_id()?;
        conn.render_create_picture(picture, pixmap, format, &CreatePictureAux::new())?;
        let sx = f32::from(source_w) / f32::from(width);
        let sy = f32::from(source_h) / f32::from(height);
        let Some(source) = self.set_transform(conn, id, [fixed(sx), 0, fixed(sy), 0], true)? else {
            return Ok(());
        };
        conn.render_composite(
            PictOp::SRC,
            source,
            x11rb::NONE,
            picture,
            0,
            0,
            0,
            0,
            0,
            0,
            width,
            height,
        )?;
        let surface = self.surfaces[id.0].as_mut().expect("checked above");
        if let Some((_, _, old_pixmap, old_picture)) =
            surface.scaled.replace((width, height, pixmap, picture))
        {
            conn.render_free_picture(old_picture)?;
            conn.free_pixmap(old_pixmap)?;
        }
        Ok(())
    }

    fn drop_prescaled<C: Connection>(&mut self, conn: &C, id: SurfaceId) {
        if let Some(surface) = self.surfaces.get_mut(id.0).and_then(Option::as_mut)
            && let Some((_, _, pixmap, picture)) = surface.scaled.take()
        {
            let _ = conn.render_free_picture(picture);
            let _ = conn.free_pixmap(pixmap);
        }
    }

    /// A server-side copy of `source` darkened by `dim` (0..1), for
    /// backgrounds that would otherwise need a full-screen blend per frame.
    pub fn create_dimmed<C: Connection>(
        &mut self,
        conn: &C,
        source: SurfaceId,
        dim: f32,
    ) -> Result<SurfaceId, Box<dyn Error>> {
        let id = self.create_snapshot(conn)?;
        self.refresh_dimmed(conn, id, source, dim)?;
        Ok(id)
    }

    pub fn refresh_dimmed<C: Connection>(
        &mut self,
        conn: &C,
        id: SurfaceId,
        source: SurfaceId,
        dim: f32,
    ) -> Result<(), Box<dyn Error>> {
        let full = Damage {
            x0: 0,
            y0: 0,
            x1: i32::from(self.width),
            y1: i32::from(self.height),
        };
        self.refresh_dimmed_regions(conn, id, source, dim, &[full])
    }

    /// Redraws `regions` of a darkened copy from its source.
    pub fn refresh_dimmed_regions<C: Connection>(
        &mut self,
        conn: &C,
        id: SurfaceId,
        source: SurfaceId,
        dim: f32,
        regions: &[Damage],
    ) -> Result<(), Box<dyn Error>> {
        let Some(source) = self.set_transform(conn, source, [0x10000, 0, 0x10000, 0], false)?
        else {
            return Ok(());
        };
        let Some(surface) = self.surfaces.get_mut(id.0).and_then(Option::as_mut) else {
            return Ok(());
        };
        let target = surface.picture;
        let bounds = Damage {
            x0: 0,
            y0: 0,
            x1: i32::from(surface.width),
            y1: i32::from(surface.height),
        };
        let regions: Vec<Damage> = regions
            .iter()
            .filter_map(|r| intersect(*r, bounds))
            .collect();
        for region in &regions {
            surface.pending = union(surface.pending, Some(*region));
        }
        let rects: Vec<Rectangle> = regions.iter().map(|r| rectangle(*r)).collect();
        for rect in &rects {
            conn.render_composite(
                PictOp::SRC,
                source,
                x11rb::NONE,
                target,
                rect.x,
                rect.y,
                0,
                0,
                rect.x,
                rect.y,
                rect.width,
                rect.height,
            )?;
        }
        conn.render_fill_rectangles(PictOp::OVER, target, render_color(0, dim), &rects)?;
        Ok(())
    }

    fn content(&self, id: SurfaceId) -> Option<&Vec<Damage>> {
        self.surfaces
            .get(id.0)
            .and_then(Option::as_ref)
            .and_then(|surface| surface.content.as_ref())
    }

    /// Sets the surface each screen row of the background shows and returns
    /// the node that draws it; only rows that changed are redrawn.
    pub fn set_rows(&mut self, rows: &[SurfaceId]) -> Node {
        let width = i32::from(self.width);
        let mut changed = false;
        if self.rows.len() != rows.len() {
            self.rows = rows.to_vec();
            self.row_changes.push(Damage {
                x0: 0,
                y0: 0,
                x1: width,
                y1: rows.len() as i32,
            });
            changed = true;
        } else {
            for (y, (old, new)) in self.rows.iter_mut().zip(rows).enumerate() {
                if old != new {
                    *old = *new;
                    push_row(&mut self.row_changes, width, y as i32);
                    changed = true;
                }
            }
        }
        if changed {
            self.rows_generation = self.rows_generation.wrapping_add(1);
        }
        Node::Rows {
            generation: self.rows_generation,
        }
    }

    /// The distinct surfaces the background rows show.
    fn row_surfaces(&self) -> Vec<SurfaceId> {
        let mut surfaces: Vec<SurfaceId> = Vec::new();
        for surface in &self.rows {
            if surfaces.last() != Some(surface) && !surfaces.contains(surface) {
                surfaces.push(*surface);
            }
        }
        surfaces
    }

    /// The background rows showing `surface`, merged into runs.
    fn rows_showing(&self, surface: SurfaceId) -> Vec<Damage> {
        let mut runs = Vec::new();
        for (y, row) in self.rows.iter().enumerate() {
            if *row == surface {
                push_row(&mut runs, i32::from(self.width), y as i32);
            }
        }
        runs
    }

    fn pending(&self, id: SurfaceId) -> Option<Damage> {
        self.surfaces
            .get(id.0)
            .and_then(Option::as_ref)
            .and_then(|surface| surface.pending)
    }

    fn set_transform<C: Connection>(
        &mut self,
        conn: &C,
        id: SurfaceId,
        transform: [i32; 4],
        bilinear: bool,
    ) -> Result<Option<render::Picture>, Box<dyn Error>> {
        let Some(surface) = self.surfaces.get_mut(id.0).and_then(Option::as_mut) else {
            return Ok(None);
        };
        if surface.transform != transform {
            conn.render_set_picture_transform(
                surface.picture,
                Transform {
                    matrix11: transform[0],
                    matrix12: 0,
                    matrix13: transform[1],
                    matrix21: 0,
                    matrix22: transform[2],
                    matrix23: transform[3],
                    matrix31: 0,
                    matrix32: 0,
                    matrix33: 0x10000,
                },
            )?;
            surface.transform = transform;
        }
        if surface.bilinear != bilinear {
            let filter: &[u8] = if bilinear { b"bilinear" } else { b"nearest" };
            conn.render_set_picture_filter(surface.picture, filter, &[])?;
            surface.bilinear = bilinear;
        }
        if transform != [0x10000, 0, 0x10000, 0] && !surface.padded {
            conn.render_change_picture(
                surface.picture,
                &render::ChangePictureAux::new().repeat(render::Repeat::PAD),
            )?;
            surface.padded = true;
        }
        Ok(Some(surface.picture))
    }

    fn draw_base<C: Connection>(
        &mut self,
        conn: &C,
        base: SurfaceId,
        target: render::Picture,
        area: Damage,
        mask: Option<(render::Picture, i32, i32)>,
    ) -> Result<(), Box<dyn Error>> {
        let Some(picture) = self.set_transform(conn, base, [0x10000, 0, 0x10000, 0], false)? else {
            return Ok(());
        };
        let (op, mask_picture, mx, my) = match mask {
            Some((m, mx, my)) => (PictOp::OVER, m, mx, my),
            None => (PictOp::SRC, x11rb::NONE, 0, 0),
        };
        conn.render_composite(
            op,
            picture,
            mask_picture,
            target,
            area.x0 as i16,
            area.y0 as i16,
            (area.x0 - mx) as i16,
            (area.y0 - my) as i16,
            area.x0 as i16,
            area.y0 as i16,
            (area.x1 - area.x0) as u16,
            (area.y1 - area.y0) as u16,
        )?;
        Ok(())
    }

    /// Draws one node limited to `area`. With `mask` (a corner mask at
    /// position mx, my) the node is drawn only where the mask is opaque.
    fn draw_node<C: Connection>(
        &mut self,
        conn: &C,
        node: &Node,
        target: render::Picture,
        area: Damage,
        mask: Option<(render::Picture, i32, i32)>,
    ) -> Result<(), Box<dyn Error>> {
        let alpha = node.alpha();
        if alpha <= 0.0 {
            return Ok(());
        }
        let alpha_mask = if alpha < 0.999 {
            Some(self.alpha_masks[((alpha * 64.0).round() as usize).min(64)])
        } else {
            None
        };
        let (w, h) = ((area.x1 - area.x0) as u16, (area.y1 - area.y0) as u16);
        match *node {
            Node::Rows { .. } => {
                let screen = RectF::new(0.0, 0.0, f32::from(self.width), f32::from(self.height));
                let image = |surface| Node::Image {
                    surface,
                    src: screen,
                    dst: screen,
                    alpha: 1.0,
                    radius: 0.0,
                    smooth: false,
                };
                // The clipped rows, grouped by the surface they show.
                let mut groups: Vec<(SurfaceId, Vec<Rectangle>)> = Vec::new();
                for clip in self.clip.iter().filter_map(|c| intersect(*c, area)) {
                    let mut y = clip.y0;
                    while y < clip.y1 {
                        let Some(&surface) = self.rows.get(y as usize) else {
                            break;
                        };
                        let mut end = y + 1;
                        while end < clip.y1 && self.rows.get(end as usize) == Some(&surface) {
                            end += 1;
                        }
                        let rect = rectangle(Damage {
                            x0: clip.x0,
                            y0: y,
                            x1: clip.x1,
                            y1: end,
                        });
                        match groups.iter_mut().find(|group| group.0 == surface) {
                            Some(group) => group.1.push(rect),
                            None => groups.push((surface, vec![rect])),
                        }
                        y = end;
                    }
                }
                if let [(surface, _)] = groups.as_slice() {
                    self.draw_node(conn, &image(*surface), target, area, mask)?;
                } else if !groups.is_empty() {
                    let restore: Vec<Rectangle> = self.clip.iter().map(|d| rectangle(*d)).collect();
                    for (surface, rects) in &groups {
                        conn.render_set_picture_clip_rectangles(target, 0, 0, rects)?;
                        self.draw_node(conn, &image(*surface), target, area, mask)?;
                    }
                    conn.render_set_picture_clip_rectangles(target, 0, 0, &restore)?;
                }
                let _ = (w, h, alpha_mask);
            }
            Node::Image {
                surface,
                src,
                dst,
                smooth,
                ..
            } => {
                // Exactly the card's pixels (its padded bounds would sample
                // outside the source and leave the fast path).
                let exact = Damage {
                    x0: dst.x.floor() as i32,
                    y0: dst.y.floor() as i32,
                    x1: (dst.x + dst.w).ceil() as i32,
                    y1: (dst.y + dst.h).ceil() as i32,
                };
                let Some(area) = intersect(area, exact) else {
                    return Ok(());
                };
                let (w, h) = ((area.x1 - area.x0) as u16, (area.y1 - area.y0) as u16);
                let opaque = self
                    .surfaces
                    .get(surface.0)
                    .and_then(Option::as_ref)
                    .is_some_and(|s| s.opaque);
                let sx = src.w / dst.w.max(0.001);
                let sy = src.h / dst.h.max(0.001);
                let integral = (sx - 1.0).abs() < 1e-4
                    && (sy - 1.0).abs() < 1e-4
                    && (dst.x - dst.x.round()).abs() < 1e-3
                    && (dst.y - dst.y.round()).abs() < 1e-3;
                let (transform, src_x, src_y, bilinear) = if integral {
                    (
                        [0x10000, 0, 0x10000, 0],
                        area.x0 + (src.x - dst.x).round() as i32,
                        area.y0 + (src.y - dst.y).round() as i32,
                        false,
                    )
                } else {
                    // Destination coordinates map to source coordinates.
                    (
                        [
                            fixed(sx),
                            fixed(src.x - dst.x * sx),
                            fixed(sy),
                            fixed(src.y - dst.y * sy),
                        ],
                        area.x0,
                        area.y0,
                        smooth,
                    )
                };
                // A card resting at a pre-scaled size draws that copy 1:1.
                let prescaled = self.surface(surface).and_then(|s| {
                    let full_source = src.x.abs() < 0.5
                        && src.y.abs() < 0.5
                        && (src.w - f32::from(s.width)).abs() < 0.5
                        && (src.h - f32::from(s.height)).abs() < 0.5;
                    s.scaled.filter(|(w, h, _, _)| {
                        full_source
                            && (dst.w - f32::from(*w)).abs() < 1.0
                            && (dst.h - f32::from(*h)).abs() < 1.0
                    })
                });
                let (picture, src_x, src_y, integral) = match prescaled {
                    Some((_, _, _, picture)) => (
                        picture,
                        area.x0 - dst.x.round() as i32,
                        area.y0 - dst.y.round() as i32,
                        true,
                    ),
                    None => {
                        let Some(picture) =
                            self.set_transform(conn, surface, transform, bilinear)?
                        else {
                            return Ok(());
                        };
                        (picture, src_x, src_y, integral)
                    }
                };
                // A scaled draw through an alpha mask leaves pixman's fast
                // paths (5x slower); fades of scaled images are done by
                // drawing them opaque and re-covering them with what is
                // behind (see `compose`).
                let alpha_mask = if integral { alpha_mask } else { None };
                let (mask_picture, mx, my) = match (mask, alpha_mask) {
                    (Some((m, mx, my)), _) => (m, area.x0 - mx, area.y0 - my),
                    (None, Some(m)) => (m, 0, 0),
                    (None, None) => (x11rb::NONE, 0, 0),
                };
                let op = if opaque && mask_picture == x11rb::NONE {
                    PictOp::SRC
                } else {
                    PictOp::OVER
                };
                // An unscaled transparent layer only needs its content tiles.
                let bands: Vec<Damage> = match (integral, self.content(surface)) {
                    // With many tiles, one composite clipped to the damage
                    // region is cheaper than a request per tile.
                    (true, Some(tiles)) => {
                        let bands = if tiles.len() <= 12 {
                            tiles
                        } else {
                            &self.surface(surface).expect("has content").content_bands
                        };
                        let (dx, dy) = (
                            (dst.x - src.x).round() as i32,
                            (dst.y - src.y).round() as i32,
                        );
                        bands
                            .iter()
                            .filter_map(|b| {
                                intersect(
                                    Damage {
                                        x0: b.x0 + dx,
                                        y0: b.y0 + dy,
                                        x1: b.x1 + dx,
                                        y1: b.y1 + dy,
                                    },
                                    area,
                                )
                            })
                            .collect()
                    }
                    _ => vec![area],
                };
                for part in bands {
                    conn.render_composite(
                        op,
                        picture,
                        mask_picture,
                        target,
                        (src_x + part.x0 - area.x0) as i16,
                        (src_y + part.y0 - area.y0) as i16,
                        (mx + part.x0 - area.x0) as i16,
                        (my + part.y0 - area.y0) as i16,
                        part.x0 as i16,
                        part.y0 as i16,
                        (part.x1 - part.x0) as u16,
                        (part.y1 - part.y0) as u16,
                    )?;
                }
                let _ = (w, h);
            }
            Node::Fill { color, .. } => match mask {
                None => {
                    // An opaque fill replaces what is beneath (cheaper).
                    let op = if alpha >= 0.999 {
                        PictOp::SRC
                    } else {
                        PictOp::OVER
                    };
                    conn.render_fill_rectangles(
                        op,
                        target,
                        render_color(color, alpha),
                        &[Rectangle {
                            x: area.x0 as i16,
                            y: area.y0 as i16,
                            width: w,
                            height: h,
                        }],
                    )?;
                }
                Some((m, mx, my)) => {
                    let solid = conn.generate_id()?;
                    conn.render_create_solid_fill(solid, render_color(color, alpha))?;
                    conn.render_composite(
                        PictOp::OVER,
                        solid,
                        m,
                        target,
                        0,
                        0,
                        (area.x0 - mx) as i16,
                        (area.y0 - my) as i16,
                        area.x0 as i16,
                        area.y0 as i16,
                        w,
                        h,
                    )?;
                    conn.render_free_picture(solid)?;
                }
            },
        }
        Ok(())
    }

    /// Rounds the corners of `frame.nodes[index]` by redrawing everything
    /// beneath it (base and earlier nodes) through corner masks.
    fn repair_corners<C: Connection>(
        &mut self,
        conn: &C,
        frame: &Frame,
        index: usize,
        target: render::Picture,
        redraw: Damage,
    ) -> Result<(), Box<dyn Error>> {
        let node = frame.nodes[index];
        let (dst, radius) = match node {
            Node::Image { dst, radius, .. } | Node::Fill { dst, radius, .. } => (dst, radius),
            Node::Rows { .. } => return Ok(()),
        };
        let max = (dst.w.min(dst.h) / 2.0).floor();
        let wanted = radius.min(max).round() as u16;
        let Some((r, masks)) = self
            .corners
            .iter()
            .min_by_key(|(r, _)| (i32::from(*r) - i32::from(wanted)).abs())
            .copied()
        else {
            return Ok(());
        };
        if wanted < 3 {
            return Ok(());
        }
        let r = i32::from(r);
        let x0 = dst.x.round() as i32;
        let y0 = dst.y.round() as i32;
        let x1 = (dst.x + dst.w).round() as i32;
        let y1 = (dst.y + dst.h).round() as i32;
        let origins = [(x0, y0), (x1 - r, y0), (x0, y1 - r), (x1 - r, y1 - r)];
        for (corner, (cx, cy)) in origins.into_iter().enumerate() {
            let square = Damage {
                x0: cx,
                y0: cy,
                x1: cx + r,
                y1: cy + r,
            };
            let Some(area) = intersect(square, redraw) else {
                continue;
            };
            self.redraw_beneath(conn, frame, index, target, area, masks[corner], cx, cy)?;
        }
        Ok(())
    }

    /// Draws the base and the nodes below `frame.nodes[index]` over `area`
    /// through `mask` (positioned at mx, my).
    #[allow(clippy::too_many_arguments)]
    fn redraw_beneath<C: Connection>(
        &mut self,
        conn: &C,
        frame: &Frame,
        index: usize,
        target: render::Picture,
        area: Damage,
        mask: render::Picture,
        mx: i32,
        my: i32,
    ) -> Result<(), Box<dyn Error>> {
        let mask = Some((mask, mx, my));
        if let Some(base) = frame.base {
            self.draw_base(conn, base, target, area, mask)?;
        }
        for below in &frame.nodes[..index] {
            if let Some(below_area) = self.node_bounds(below).and_then(|b| intersect(b, area)) {
                self.draw_node(conn, below, target, below_area, mask)?;
            }
        }
        Ok(())
    }

    pub fn destroy<C: Connection>(&mut self, conn: &C) {
        for index in 0..self.surfaces.len() {
            self.free(conn, SurfaceId(index));
        }
        for buffer in &self.buffers {
            let _ = conn.render_free_picture(buffer.picture);
            let _ = conn.free_pixmap(buffer.pixmap);
        }
        let _ = conn.render_free_picture(self.window_picture);
        let _ = conn.free_gc(self.gc);
        let _ = conn.free_gc(self.capture_gc);
    }
}

/// The 32-pixel tiles of a premultiplied canvas that hold visible pixels,
/// merged into horizontal runs and then into taller rectangles where runs
/// repeat on consecutive tile rows.
fn content_tiles(canvas: &Canvas) -> Vec<Damage> {
    const TILE: i32 = 32;
    let (w, h) = (canvas.width, canvas.height);
    let columns = ((w + TILE - 1) / TILE) as usize;
    let mut rects: Vec<Damage> = Vec::new();
    let mut open: Vec<Damage> = Vec::new();
    let mut occupied = vec![false; columns];
    let mut ty = 0;
    while ty < h {
        let y1 = (ty + TILE).min(h);
        occupied.fill(false);
        for y in ty..y1 {
            let row = &canvas.pixels[(y * w) as usize..((y + 1) * w) as usize];
            for (column, flag) in occupied.iter_mut().enumerate() {
                if *flag {
                    continue;
                }
                let x0 = column * TILE as usize;
                let x1 = (x0 + TILE as usize).min(w as usize);
                *flag = row[x0..x1].iter().any(|p| p >> 24 != 0);
            }
        }
        let mut still_open = Vec::new();
        let mut column = 0;
        while column < columns {
            if !occupied[column] {
                column += 1;
                continue;
            }
            let start = column;
            while column < columns && occupied[column] {
                column += 1;
            }
            let (x0, x1) = (start as i32 * TILE, (column as i32 * TILE).min(w));
            match open
                .iter()
                .position(|r| r.x0 == x0 && r.x1 == x1 && r.y1 == ty)
            {
                Some(position) => {
                    let mut grown = open.swap_remove(position);
                    grown.y1 = y1;
                    still_open.push(grown);
                }
                None => still_open.push(Damage { x0, y0: ty, x1, y1 }),
            }
        }
        rects.append(&mut open);
        open = still_open;
        ty = y1;
    }
    rects.append(&mut open);
    rects
}

/// The 32-pixel grid cells touched by `rects`, as non-overlapping
/// rectangles (horizontal runs merged down across rows).
fn grid_cells(rects: &[Damage], width: i32, height: i32) -> Vec<Damage> {
    const CELL: i32 = 32;
    let columns = (width + CELL - 1) / CELL;
    let rows = (height + CELL - 1) / CELL;
    let mut cells = vec![false; (columns * rows) as usize];
    for r in rects {
        let (x0, x1) = (r.x0.max(0) / CELL, (r.x1.min(width) + CELL - 1) / CELL);
        let (y0, y1) = (r.y0.max(0) / CELL, (r.y1.min(height) + CELL - 1) / CELL);
        for y in y0..y1 {
            for x in x0..x1 {
                cells[(y * columns + x) as usize] = true;
            }
        }
    }
    let mut out: Vec<Damage> = Vec::new();
    let mut open: Vec<Damage> = Vec::new();
    for row in 0..rows {
        let (top, bottom) = (row * CELL, ((row + 1) * CELL).min(height));
        let mut still_open = Vec::new();
        let mut column = 0;
        while column < columns {
            if !cells[(row * columns + column) as usize] {
                column += 1;
                continue;
            }
            let start = column;
            while column < columns && cells[(row * columns + column) as usize] {
                column += 1;
            }
            let (x0, x1) = (start * CELL, (column * CELL).min(width));
            match open
                .iter()
                .position(|r| r.x0 == x0 && r.x1 == x1 && r.y1 == top)
            {
                Some(position) => {
                    let mut grown = open.swap_remove(position);
                    grown.y1 = bottom;
                    still_open.push(grown);
                }
                None => still_open.push(Damage {
                    x0,
                    y0: top,
                    x1,
                    y1: bottom,
                }),
            }
        }
        out.append(&mut open);
        open = still_open;
    }
    out.append(&mut open);
    out
}

/// Bounding boxes of the connected groups of tiles (tiles touching or
/// within 16 pixels merge), e.g. one per keypad key; when there are many
/// groups, horizontal bands instead.
fn components_of(tiles: &[Damage]) -> Vec<Damage> {
    let mut boxes: Vec<Damage> = tiles.to_vec();
    let near = |a: &Damage, b: &Damage| {
        a.x0 <= b.x1 + 16 && b.x0 <= a.x1 + 16 && a.y0 <= b.y1 + 16 && b.y0 <= a.y1 + 16
    };
    loop {
        let mut merged = false;
        let mut i = 0;
        while i < boxes.len() {
            let mut j = i + 1;
            while j < boxes.len() {
                if near(&boxes[i], &boxes[j]) {
                    let other = boxes.swap_remove(j);
                    boxes[i] = boxes[i].union(other);
                    merged = true;
                } else {
                    j += 1;
                }
            }
            i += 1;
        }
        if !merged {
            break;
        }
    }
    if boxes.len() > 24 {
        return bands_of(tiles);
    }
    boxes
}

/// Groups tiles into horizontal bands: tiles whose rows lie within 64
/// pixels of each other merge, taking the union of their extents.
fn bands_of(tiles: &[Damage]) -> Vec<Damage> {
    let mut sorted = tiles.to_vec();
    sorted.sort_by_key(|t| t.y0);
    let mut bands: Vec<Damage> = Vec::new();
    for tile in sorted {
        match bands.last_mut() {
            Some(band) if tile.y0 <= band.y1 + 64 => *band = band.union(tile),
            _ => bands.push(tile),
        }
    }
    bands
}

/// `area` minus `hole`, as up to four rectangles.
fn subtract(area: Damage, hole: Damage) -> Vec<Damage> {
    let Some(hole) = intersect(area, hole) else {
        return vec![area];
    };
    let mut parts = Vec::with_capacity(4);
    if hole.y0 > area.y0 {
        parts.push(Damage {
            y1: hole.y0,
            ..area
        });
    }
    if hole.y1 < area.y1 {
        parts.push(Damage {
            y0: hole.y1,
            ..area
        });
    }
    if hole.x0 > area.x0 {
        parts.push(Damage {
            x0: area.x0,
            y0: hole.y0,
            x1: hole.x0,
            y1: hole.y1,
        });
    }
    if hole.x1 < area.x1 {
        parts.push(Damage {
            x0: hole.x1,
            y0: hole.y0,
            x1: area.x1,
            y1: hole.y1,
        });
    }
    parts
}

fn monotonic_micros() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: ts is a valid timespec for clock_gettime to fill.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000 + ts.tv_nsec as u64 / 1_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_bounds_are_clamped_and_padded() {
        let node = Node::Fill {
            dst: RectF::new(-10.0, 5.2, 50.0, 20.0),
            color: 0,
            alpha: 1.0,
            radius: 0.0,
        };
        assert_eq!(
            node.bounds(100, 100),
            Some(Damage {
                x0: 0,
                y0: 4,
                x1: 41,
                y1: 27
            })
        );
        let offscreen = Node::Fill {
            dst: RectF::new(200.0, 0.0, 10.0, 10.0),
            color: 0,
            alpha: 1.0,
            radius: 0.0,
        };
        assert_eq!(offscreen.bounds(100, 100), None);
    }

    #[test]
    fn content_tiles_cover_only_occupied_tiles() {
        let mut canvas = Canvas::new(200, 400);
        canvas.fill_rect(10, 10, 20, 50, 0xffffff, 1.0);
        canvas.fill_rect(150, 300, 10, 10, 0xffffff, 0.5);
        let tiles = content_tiles(&canvas);
        assert_eq!(
            tiles,
            vec![
                Damage {
                    x0: 0,
                    y0: 0,
                    x1: 32,
                    y1: 64
                },
                Damage {
                    x0: 128,
                    y0: 288,
                    x1: 160,
                    y1: 320
                },
            ]
        );
    }

    #[test]
    fn grid_cells_coalesce_overlapping_rectangles() {
        let rects = [
            Damage {
                x0: 5,
                y0: 5,
                x1: 60,
                y1: 40,
            },
            Damage {
                x0: 10,
                y0: 12,
                x1: 62,
                y1: 47,
            },
            Damage {
                x0: 200,
                y0: 0,
                x1: 201,
                y1: 1,
            },
        ];
        assert_eq!(
            sorted(grid_cells(&rects, 300, 100)),
            vec![
                Damage {
                    x0: 0,
                    y0: 0,
                    x1: 64,
                    y1: 64
                },
                Damage {
                    x0: 192,
                    y0: 0,
                    x1: 224,
                    y1: 32
                },
            ]
        );
        assert!(grid_cells(&[], 300, 100).is_empty());
    }

    fn sorted(mut rects: Vec<Damage>) -> Vec<Damage> {
        rects.sort_by_key(|d| (d.y0, d.x0));
        rects
    }

    #[test]
    fn components_box_each_separate_group() {
        let tiles = [
            Damage {
                x0: 0,
                y0: 0,
                x1: 32,
                y1: 32,
            },
            Damage {
                x0: 32,
                y0: 0,
                x1: 64,
                y1: 32,
            },
            Damage {
                x0: 400,
                y0: 0,
                x1: 432,
                y1: 32,
            },
        ];
        let mut boxes = components_of(&tiles);
        boxes.sort_by_key(|b| b.x0);
        assert_eq!(
            boxes,
            vec![
                Damage {
                    x0: 0,
                    y0: 0,
                    x1: 64,
                    y1: 32
                },
                Damage {
                    x0: 400,
                    y0: 0,
                    x1: 432,
                    y1: 32
                },
            ]
        );
    }

    #[test]
    fn bands_merge_nearby_rows_only() {
        let tiles = [
            Damage {
                x0: 0,
                y0: 0,
                x1: 32,
                y1: 32,
            },
            Damage {
                x0: 64,
                y0: 64,
                x1: 96,
                y1: 96,
            },
            Damage {
                x0: 0,
                y0: 500,
                x1: 32,
                y1: 532,
            },
        ];
        assert_eq!(
            bands_of(&tiles),
            vec![
                Damage {
                    x0: 0,
                    y0: 0,
                    x1: 96,
                    y1: 96
                },
                Damage {
                    x0: 0,
                    y0: 500,
                    x1: 32,
                    y1: 532
                },
            ]
        );
    }

    #[test]
    fn subtract_leaves_the_frame_around_a_hole() {
        let area = Damage {
            x0: 0,
            y0: 0,
            x1: 100,
            y1: 100,
        };
        let parts = subtract(
            area,
            Damage {
                x0: 20,
                y0: 30,
                x1: 80,
                y1: 90,
            },
        );
        let covered: i64 = parts
            .iter()
            .map(|d| i64::from(d.x1 - d.x0) * i64::from(d.y1 - d.y0))
            .sum();
        assert_eq!(covered, 100 * 100 - 60 * 60);
        let far = Damage {
            x0: 200,
            y0: 0,
            x1: 300,
            y1: 10,
        };
        assert_eq!(subtract(area, far), vec![area]);
        assert!(subtract(area, area).is_empty());
    }

    #[test]
    fn render_colors_are_premultiplied() {
        let color = render_color(0xff8000, 0.5);
        assert_eq!(color.alpha, 32767);
        assert_eq!(color.red, 32767);
        assert_eq!(color.green, 16448);
        assert_eq!(color.blue, 0);
    }

    #[test]
    fn frame_stats_summarize_intervals() {
        let stats = FrameStats {
            frames: 3,
            missed_vblanks: 1,
            half_rate_frames: 0,
            intervals_ms: vec![8.3, 8.4, 16.7],
            latency_ms: vec![9.0, 9.5, 10.0],
        };
        let summary = stats.summary();
        assert!(summary.contains("3 frames (0 paced at 60 Hz), 1 missed"));
        assert!(summary.contains("max 16.70"));
    }
}
