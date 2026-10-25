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
}

impl Node {
    fn bounds(&self, width: i32, height: i32) -> Option<Damage> {
        let dst = match self {
            Node::Image { dst, .. } | Node::Fill { dst, .. } => *dst,
        };
        rect_damage(dst, width, height)
    }

    fn alpha(&self) -> f32 {
        match self {
            Node::Image { alpha, .. } | Node::Fill { alpha, .. } => *alpha,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub base: SurfaceId,
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
    /// For transparent layers: the bands that hold any visible pixel, so a
    /// mostly empty full-screen layer composites only its content.
    content: Option<Vec<Damage>>,
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
    build: Duration,
}

/// Timing of presented frames, for `MOON_PROFILE` and on-device tuning.
#[derive(Debug, Default, Clone)]
pub struct FrameStats {
    pub frames: u32,
    pub missed_vblanks: u64,
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
            "{} frames, {} missed vblanks; interval ms {}; submit-to-scanout ms {}",
            self.frames,
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
    last_damage: Option<Damage>,
    force_full: bool,
    next_direct_frame: Instant,
    profile: bool,
    /// MOON_DEBUG_FRAMES=1 logs every frame's node list.
    debug_frames: bool,
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
}

/// How long a submitted frame may go unconfirmed before Moon assumes Present
/// lost it (for example while DPMS has the CRTC off) and recovers.
const PRESENT_WATCHDOG: Duration = Duration::from_millis(120);
/// Frame interval of the timer fallback (120 Hz).
const DIRECT_FRAME_INTERVAL: Duration = Duration::from_micros(8_333);
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
                for _ in 0..2 {
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
            last_damage: None,
            force_full: true,
            next_direct_frame: Instant::now(),
            profile,
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
        }))
    }

    /// Uploads the changed parts of `canvas` into a layer.
    pub fn upload<C: Connection>(
        &mut self,
        conn: &C,
        id: SurfaceId,
        canvas: &Canvas,
    ) -> Result<(), Box<dyn Error>> {
        let Some(surface) = self.surfaces.get_mut(id.0).and_then(Option::as_mut) else {
            return Err("unknown layer".into());
        };
        let Some(uploader) = surface.uploader.as_mut() else {
            return Err("surface is not an uploaded layer".into());
        };
        let damage = uploader.upload(conn, canvas)?;
        if !surface.opaque && !damage.is_empty() {
            surface.content = Some(content_bands(canvas));
        }
        for damage in damage {
            surface.pending = union(surface.pending, Some(damage));
        }
        Ok(())
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
        }))
    }

    /// Copies what is on screen right now (every window, as composed by the
    /// server) into a snapshot surface. Issue it before restacking.
    pub fn capture_root<C: Connection>(
        &mut self,
        conn: &C,
        id: SurfaceId,
    ) -> Result<(), Box<dyn Error>> {
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
                let mut next = present.last_ust + period;
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
                    if present.last_msc > 0 && event.msc > present.last_msc {
                        let delta = event.msc - present.last_msc;
                        let interval_us = event.ust.saturating_sub(present.last_ust);
                        if delta == 1 && interval_us > 2_000 && interval_us < 40_000 {
                            // Track the panel's real refresh period.
                            present.period_us = (present.period_us * 7 + interval_us) / 8;
                        }
                        let interval_ms = interval_us as f32 / 1000.0;
                        let latency_ms = in_flight.sent.elapsed().as_secs_f32() * 1000.0;
                        if interval_ms < 100.0 && self.stats.intervals_ms.len() < 4096 {
                            self.stats.frames += 1;
                            self.stats.missed_vblanks += delta - 1;
                            self.stats.intervals_ms.push(interval_ms);
                            self.stats.latency_ms.push(latency_ms);
                        }
                        if self.profile {
                            eprintln!(
                                "present msc +{delta} interval {interval_ms:.2} ms submit-to-scanout {latency_ms:.2} ms build {:.2} ms damage {} px",
                                in_flight.build.as_secs_f64() * 1000.0,
                                in_flight.damage_area,
                            );
                        }
                    }
                    present.last_msc = event.msc;
                    present.last_ust = event.ust;
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
        let mut damage = if self.force_full || structure_changed {
            Some(full)
        } else {
            let mut damage = self.pending(frame.base);
            let last = self.last.as_ref().expect("checked above");
            for (old, new) in last.nodes.iter().zip(&frame.nodes) {
                if old != new {
                    damage = union(damage, old.bounds(w, h));
                    damage = union(damage, new.bounds(w, h));
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
                    .and_then(|d| new.bounds(w, h).and_then(|b| intersect(d, b)))
                } else {
                    new.bounds(w, h)
                };
                damage = union(damage, changed);
            }
            damage
        };
        damage = damage.and_then(|d| intersect(d, full));

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
        // A buffer last drew the frame before the previous one, so it also
        // needs the previous frame's damage.
        let redraw = match buffer_index {
            Some(index) if self.buffers[index].stale => Some(full),
            Some(_) => union(damage, self.last_damage),
            None => damage,
        };
        let Some(redraw) = redraw.and_then(|d| intersect(d, full)) else {
            self.last = Some(frame);
            self.last_damage = None;
            return Ok(false);
        };

        if self.debug_frames {
            eprintln!("frame redraw {redraw:?} base {:?}", frame.base);
            for node in &frame.nodes {
                eprintln!("  {node:?}");
            }
        }
        conn.render_set_picture_clip_rectangles(
            target,
            0,
            0,
            &[Rectangle {
                x: redraw.x0 as i16,
                y: redraw.y0 as i16,
                width: (redraw.x1 - redraw.x0) as u16,
                height: (redraw.y1 - redraw.y0) as u16,
            }],
        )?;
        self.draw_base(conn, frame.base, target, redraw, None)?;
        for index in 0..frame.nodes.len() {
            let node = frame.nodes[index];
            let Some(bounds) = node.bounds(w, h).and_then(|b| intersect(b, redraw)) else {
                continue;
            };
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
            };
            if radius >= 1.0 {
                self.repair_corners(conn, &frame, index, target, redraw)?;
            }
        }

        // Surfaces used by this frame have now drawn their uploads.
        let used: Vec<SurfaceId> = std::iter::once(frame.base)
            .chain(frame.nodes.iter().filter_map(|node| match node {
                Node::Image { surface, .. } => Some(*surface),
                Node::Fill { .. } => None,
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
                conn.xfixes_set_region(
                    present.region,
                    &[Rectangle {
                        x: redraw.x0 as i16,
                        y: redraw.y0 as i16,
                        width: (redraw.x1 - redraw.x0) as u16,
                        height: (redraw.y1 - redraw.y0) as u16,
                    }],
                )?;
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
                        present.last_msc + 1
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
                present.in_flight = Some(InFlight {
                    serial: present.serial,
                    buffer: index,
                    sent: Instant::now(),
                    damage_area: area(Some(redraw)),
                    build,
                });
            }
            _ => {
                self.next_direct_frame = Instant::now() + DIRECT_FRAME_INTERVAL;
                if self.profile {
                    eprintln!(
                        "direct frame build {:.2} ms damage {} px",
                        build.as_secs_f64() * 1000.0,
                        area(Some(redraw))
                    );
                }
            }
        }
        conn.flush()?;
        self.force_full = false;
        self.last = Some(frame);
        self.last_damage = damage;
        Ok(true)
    }

    fn content(&self, id: SurfaceId) -> Option<&Vec<Damage>> {
        self.surfaces
            .get(id.0)
            .and_then(Option::as_ref)
            .and_then(|surface| surface.content.as_ref())
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
                let Some(picture) = self.set_transform(conn, surface, transform, bilinear)? else {
                    return Ok(());
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
                // An unscaled transparent layer only needs its content bands.
                let bands: Vec<Damage> = match (integral, self.content(surface)) {
                    (true, Some(bands)) => {
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
                    conn.render_fill_rectangles(
                        PictOp::OVER,
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
        self.draw_base(conn, frame.base, target, area, mask)?;
        for below in &frame.nodes[..index] {
            if let Some(below_area) = below
                .bounds(i32::from(self.width), i32::from(self.height))
                .and_then(|b| intersect(b, area))
            {
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

/// Row bands of a premultiplied canvas that contain visible pixels, with the
/// horizontal extent of each; nearby rows merge into one band.
fn content_bands(canvas: &Canvas) -> Vec<Damage> {
    const MERGE_GAP: i32 = 48;
    let mut bands: Vec<Damage> = Vec::new();
    for y in 0..canvas.height {
        let row = &canvas.pixels[(y * canvas.width) as usize..((y + 1) * canvas.width) as usize];
        let Some(first) = row.iter().position(|p| p >> 24 != 0) else {
            continue;
        };
        let last = row.iter().rposition(|p| p >> 24 != 0).unwrap_or(first);
        let (x0, x1) = (first as i32, last as i32 + 1);
        match bands.last_mut() {
            Some(band) if y - band.y1 <= MERGE_GAP => {
                band.x0 = band.x0.min(x0);
                band.x1 = band.x1.max(x1);
                band.y1 = y + 1;
            }
            _ => bands.push(Damage {
                x0,
                y0: y,
                x1,
                y1: y + 1,
            }),
        }
    }
    bands
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
    fn content_bands_skip_empty_rows_and_merge_near_ones() {
        let mut canvas = Canvas::new(100, 400);
        canvas.fill_rect(10, 10, 20, 10, 0xffffff, 1.0);
        canvas.fill_rect(40, 40, 20, 10, 0xffffff, 1.0);
        canvas.fill_rect(5, 300, 10, 10, 0xffffff, 0.5);
        let bands = content_bands(&canvas);
        assert_eq!(
            bands,
            vec![
                Damage {
                    x0: 10,
                    y0: 10,
                    x1: 60,
                    y1: 50
                },
                Damage {
                    x0: 5,
                    y0: 300,
                    x1: 15,
                    y1: 310
                },
            ]
        );
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
            intervals_ms: vec![8.3, 8.4, 16.7],
            latency_ms: vec![9.0, 9.5, 10.0],
        };
        let summary = stats.summary();
        assert!(summary.contains("3 frames, 1 missed"));
        assert!(summary.contains("max 16.70"));
    }
}
