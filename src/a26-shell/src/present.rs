//! Uploads a `Canvas` to an X11 window by sending only changed pixels.
//!
//! The presenter keeps a server-side pixmap that always mirrors the last
//! presented frame and a client-side copy of it. Each frame is compared in
//! horizontal bands; only the changed span of each band is sent with
//! `PutImage` into the pixmap, and then the union of the damage is copied to
//! the window in a single `CopyArea`, so the panel never scans out a
//! half-drawn frame. Every request is one-way: no reply is ever awaited, as
//! Moon's long-lived connection requires (see `X11-INPUT-DEADLOCK.md`).

use std::error::Error;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    ConnectionExt as _, CreateGCAux, Gcontext, ImageFormat, Pixmap, Window,
};

use crate::gfx::Canvas;

/// Keep each PutImage comfortably under the core 256 KiB request limit so
/// x11rb never needs BIG-REQUESTS (whose negotiation is a reply wait).
const MAX_REQUEST_BYTES: usize = 200 * 1024;
const BAND_ROWS: i32 = 16;
const CHUNK: usize = 64;

pub struct Presenter {
    window: Window,
    pixmap: Pixmap,
    gc: Gcontext,
    width: i32,
    height: i32,
    shadow: Vec<u32>,
    valid: bool,
    scratch: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Damage {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl Presenter {
    pub fn create<C: Connection>(
        conn: &C,
        window: Window,
        depth: u8,
        width: u16,
        height: u16,
    ) -> Result<Self, Box<dyn Error>> {
        let pixmap = conn.generate_id()?;
        conn.create_pixmap(depth, pixmap, window, width, height)?;
        let gc = conn.generate_id()?;
        conn.create_gc(gc, pixmap, &CreateGCAux::new().graphics_exposures(0))?;
        let width = i32::from(width);
        let height = i32::from(height);
        Ok(Self {
            window,
            pixmap,
            gc,
            width,
            height,
            shadow: vec![0; (width * height) as usize],
            valid: false,
            scratch: Vec::new(),
        })
    }

    /// Forgets what the server holds, forcing the next frame to upload fully.
    pub fn invalidate(&mut self) {
        self.valid = false;
    }

    /// Uploads `canvas` and copies the damaged areas to the window, whose
    /// origin corresponds to canvas row `window_y`. Returns the bounding box
    /// of the damage.
    ///
    /// Each 16-row band is compared in 64-pixel column chunks; runs of dirty
    /// chunks are uploaded separately, so a moving edge on either side of a
    /// large unchanged area costs only the edges.
    pub fn present<C: Connection>(
        &mut self,
        conn: &C,
        canvas: &Canvas,
        window_y: i32,
    ) -> Result<Option<Damage>, Box<dyn Error>> {
        if canvas.width != self.width || canvas.height != self.height {
            return Err("canvas size does not match presenter".into());
        }
        let width = self.width as usize;
        let chunks = width.div_ceil(CHUNK);
        let mut dirty = vec![false; chunks];
        let mut runs: Vec<(i32, i32)> = Vec::new();
        // Damage rectangles still growing downward, merged across bands.
        let mut open: Vec<Damage> = Vec::new();
        let mut closed: Vec<Damage> = Vec::new();
        let mut band_y = 0;
        while band_y < self.height {
            let band_end = (band_y + BAND_ROWS).min(self.height);
            dirty.fill(!self.valid);
            if self.valid {
                for y in band_y..band_end {
                    let start = y as usize * width;
                    let new = &canvas.pixels[start..start + width];
                    let old = &self.shadow[start..start + width];
                    if new == old {
                        continue;
                    }
                    for (chunk, flag) in dirty.iter_mut().enumerate() {
                        if *flag {
                            continue;
                        }
                        let a = chunk * CHUNK;
                        let b = (a + CHUNK).min(width);
                        *flag = new[a..b] != old[a..b];
                    }
                }
            }
            runs.clear();
            let mut chunk = 0;
            while chunk < chunks {
                if !dirty[chunk] {
                    chunk += 1;
                    continue;
                }
                let first = chunk;
                // Bridge single clean chunks to keep the request count low.
                while chunk < chunks && (dirty[chunk] || (chunk + 1 < chunks && dirty[chunk + 1])) {
                    chunk += 1;
                }
                let x0 = (first * CHUNK) as i32;
                let x1 = ((chunk * CHUNK).min(width)) as i32;
                runs.push((x0, x1));
            }
            let mut still_open = Vec::with_capacity(runs.len());
            for &(x0, x1) in &runs {
                self.upload(conn, canvas, x0, band_y, x1, band_end)?;
                if let Some(position) = open
                    .iter()
                    .position(|d| d.x0 == x0 && d.x1 == x1 && d.y1 == band_y)
                {
                    let mut grown = open.swap_remove(position);
                    grown.y1 = band_end;
                    still_open.push(grown);
                } else {
                    still_open.push(Damage {
                        x0,
                        y0: band_y,
                        x1,
                        y1: band_end,
                    });
                }
            }
            closed.append(&mut open);
            open = still_open;
            band_y = band_end;
        }
        closed.append(&mut open);
        self.valid = true;
        let mut bounds: Option<Damage> = None;
        for damage in &closed {
            self.copy_to_window(conn, *damage, window_y)?;
            bounds = Some(match bounds {
                Some(b) => Damage {
                    x0: b.x0.min(damage.x0),
                    y0: b.y0.min(damage.y0),
                    x1: b.x1.max(damage.x1),
                    y1: b.y1.max(damage.y1),
                },
                None => *damage,
            });
        }
        Ok(bounds)
    }

    /// Repaints the window from the pixmap after an Expose, without upload.
    pub fn expose<C: Connection>(&self, conn: &C, window_y: i32) -> Result<(), Box<dyn Error>> {
        if self.valid {
            self.copy_to_window(
                conn,
                Damage {
                    x0: 0,
                    y0: 0,
                    x1: self.width,
                    y1: self.height,
                },
                window_y,
            )?;
        }
        Ok(())
    }

    fn copy_to_window<C: Connection>(
        &self,
        conn: &C,
        damage: Damage,
        window_y: i32,
    ) -> Result<(), Box<dyn Error>> {
        let y0 = damage.y0.max(window_y);
        if damage.y1 <= y0 {
            return Ok(());
        }
        conn.copy_area(
            self.pixmap,
            self.window,
            self.gc,
            damage.x0 as i16,
            y0 as i16,
            damage.x0 as i16,
            (y0 - window_y) as i16,
            (damage.x1 - damage.x0) as u16,
            (damage.y1 - y0) as u16,
        )?;
        Ok(())
    }

    fn upload<C: Connection>(
        &mut self,
        conn: &C,
        canvas: &Canvas,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
    ) -> Result<(), Box<dyn Error>> {
        let span = (x1 - x0) as usize;
        let rows_per_request = (MAX_REQUEST_BYTES / (span * 4)).max(1) as i32;
        let mut y = y0;
        while y < y1 {
            let rows = rows_per_request.min(y1 - y);
            self.scratch.clear();
            for row in y..y + rows {
                let start = (row * self.width + x0) as usize;
                let pixels = &canvas.pixels[start..start + span];
                self.shadow[start..start + span].copy_from_slice(pixels);
                self.scratch.extend_from_slice(pixels);
            }
            conn.put_image(
                ImageFormat::Z_PIXMAP,
                self.pixmap,
                self.gc,
                span as u16,
                rows as u16,
                x0 as i16,
                y as i16,
                0,
                24,
                pixel_bytes(&self.scratch),
            )?;
            y += rows;
        }
        Ok(())
    }

    pub fn destroy<C: Connection>(&self, conn: &C) {
        let _ = conn.free_pixmap(self.pixmap);
        let _ = conn.free_gc(self.gc);
    }
}

/// Views XRGB pixels as the little-endian Z-pixmap bytes Xorg expects.
#[cfg(target_endian = "little")]
fn pixel_bytes(pixels: &[u32]) -> &[u8] {
    // SAFETY: u32 has no padding or invalid bit patterns, u8 has alignment 1,
    // and the byte length covers exactly the same allocation.
    unsafe { std::slice::from_raw_parts(pixels.as_ptr().cast::<u8>(), pixels.len() * 4) }
}
