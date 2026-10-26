//! The notification banner over an app. Moon cannot draw into an app's
//! window, so the banner floats in its own override-redirect window, shaped
//! to the card so the app stays visible around its rounded corners, and
//! moved as it drops in or slides away (the server copies its pixels; only
//! the strip it uncovers is repainted beneath it).

use std::error::Error;

use x11rb::COPY_DEPTH_FROM_PARENT;
use x11rb::connection::Connection;
use x11rb::protocol::shape::{ConnectionExt as _, SK, SO};
use x11rb::protocol::xproto::{
    ClipOrdering, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, EventMask, Rectangle,
    StackMode, Window, WindowClass,
};

use crate::alerts::{CARD_RADIUS, CARD_W, CARD_X};
use crate::gfx::Canvas;
use crate::present::Presenter;
use crate::ui::BLACK;

/// The tallest card a banner shows.
const MAX_HEIGHT: u16 = 260;

pub struct BannerSurface {
    pub window: Window,
    canvas: Canvas,
    presenter: Presenter,
    shape_available: bool,
    mapped: bool,
    /// Content and shape last presented.
    key: Option<u64>,
    height: u16,
    y: Option<i32>,
    exposed: bool,
    raise: bool,
}

impl BannerSurface {
    pub fn create<C: Connection>(
        conn: &C,
        root: Window,
        depth: u8,
        shape_available: bool,
    ) -> Result<Self, Box<dyn Error>> {
        let width = CARD_W as u16;
        let window = conn.generate_id()?;
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
            root,
            CARD_X as i16,
            -(i16::try_from(MAX_HEIGHT).unwrap_or(i16::MAX)),
            width,
            MAX_HEIGHT,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new()
                .background_pixel(BLACK)
                .override_redirect(1)
                .event_mask(EventMask::EXPOSURE),
        )?;
        let presenter = Presenter::create(conn, window, depth, width, MAX_HEIGHT)?;
        Ok(Self {
            window,
            canvas: Canvas::new(i32::from(width), i32::from(MAX_HEIGHT)),
            presenter,
            shape_available,
            mapped: false,
            key: None,
            height: 0,
            y: None,
            exposed: false,
            raise: false,
        })
    }

    pub fn request_redraw(&mut self) {
        self.exposed = true;
    }

    /// Another overlay was raised; the banner stays on top of it.
    pub fn request_raise(&mut self) {
        self.raise = true;
    }

    /// Shows the banner card (`height` tall, drawn by `draw` when `key`
    /// changes) with its top at `y`, or hides it.
    pub fn sync<C: Connection>(
        &mut self,
        conn: &C,
        shown: Option<(i32, f32, u64)>,
        draw: impl FnOnce(&mut Canvas),
    ) -> Result<(), Box<dyn Error>> {
        let Some((y, height, key)) = shown else {
            if self.mapped {
                conn.unmap_window(self.window)?;
                self.mapped = false;
                self.y = None;
            }
            return Ok(());
        };
        let height = (height.round() as u16).clamp(1, MAX_HEIGHT);
        if self.key != Some(key) || self.height != height {
            self.canvas.clear(BLACK);
            draw(&mut self.canvas);
            if self.shape_available && self.height != height {
                conn.shape_rectangles(
                    SO::SET,
                    SK::BOUNDING,
                    ClipOrdering::Y_SORTED,
                    self.window,
                    0,
                    0,
                    &rounded_spans(CARD_W as u16, height, CARD_RADIUS),
                )?;
            }
            self.presenter.present(conn, &self.canvas, 0)?;
            self.key = Some(key);
            self.height = height;
            self.exposed = true;
        }
        if self.y != Some(y) {
            conn.configure_window(
                self.window,
                &ConfigureWindowAux::new().y(y).stack_mode(StackMode::ABOVE),
            )?;
            self.y = Some(y);
        }
        if !self.mapped {
            conn.map_window(self.window)?;
            conn.configure_window(
                self.window,
                &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
            )?;
            self.mapped = true;
            self.exposed = true;
        }
        if std::mem::take(&mut self.raise) {
            conn.configure_window(
                self.window,
                &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
            )?;
        }
        if std::mem::take(&mut self.exposed) {
            self.presenter.expose(conn, 0)?;
        }
        Ok(())
    }

    pub fn destroy<C: Connection>(&self, conn: &C) {
        let _ = conn.destroy_window(self.window);
        self.presenter.destroy(conn);
    }
}

/// Row spans covering a rounded rectangle (pixels whose centers lie inside
/// it), for the window's bounding shape.
fn rounded_spans(width: u16, height: u16, radius: f32) -> Vec<Rectangle> {
    let radius = radius.min(f32::from(width.min(height)) / 2.0);
    (0..height)
        .filter_map(|y| {
            let py = f32::from(y) + 0.5;
            let dy = if py < radius {
                radius - py
            } else if py > f32::from(height) - radius {
                py - (f32::from(height) - radius)
            } else {
                0.0
            };
            let inset = radius - (radius * radius - dy * dy).max(0.0).sqrt();
            let x0 = inset.round() as i16;
            let x1 = (f32::from(width) - inset).round() as i16;
            (x1 > x0).then_some(Rectangle {
                x: x0,
                y: y as i16,
                width: (x1 - x0) as u16,
                height: 1,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounded_spans_are_full_width_in_the_middle() {
        let spans = rounded_spans(1000, 200, 50.0);
        assert_eq!(spans.len(), 200);
        assert_eq!(spans[100].width, 1000);
        assert!(spans[0].x > 20 && spans[0].width < 1000);
        assert_eq!(spans[0].x, spans[199].x);
    }
}
