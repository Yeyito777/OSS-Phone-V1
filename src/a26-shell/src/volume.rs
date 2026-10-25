use std::error::Error;

use x11rb::COPY_DEPTH_FROM_PARENT;
use x11rb::connection::Connection;
use x11rb::protocol::shape::{ConnectionExt as _, SK, SO};
use x11rb::protocol::xproto::{
    ClipOrdering, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, EventMask, Rectangle,
    StackMode, Window, WindowClass,
};

use crate::gfx::{Canvas, RectF};
use crate::present::Presenter;
use crate::ui::{self, BLACK, VOLUME_HUD};

/// The volume HUD shown above apps. The window is shaped to the HUD's
/// capsule so the app stays visible around it.
pub struct VolumeSurface {
    pub window: Window,
    canvas: Canvas,
    presenter: Presenter,
    mapped: bool,
    redraw: bool,
    last_volume: Option<u8>,
}

impl VolumeSurface {
    pub fn create<C: Connection>(
        conn: &C,
        root: Window,
        depth: u8,
        shape_available: bool,
    ) -> Result<Self, Box<dyn Error>> {
        let (width, height) = (VOLUME_HUD.w as u16, VOLUME_HUD.h as u16);
        let window = conn.generate_id()?;
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
            root,
            VOLUME_HUD.x as i16,
            VOLUME_HUD.y as i16,
            width,
            height,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new()
                .background_pixel(BLACK)
                .override_redirect(1)
                .event_mask(EventMask::EXPOSURE),
        )?;
        if shape_available {
            let spans = capsule_spans(width, height);
            conn.shape_rectangles(
                SO::SET,
                SK::BOUNDING,
                ClipOrdering::Y_SORTED,
                window,
                0,
                0,
                &spans,
            )?;
        }
        let presenter = Presenter::create(conn, window, depth, width, height)?;
        Ok(Self {
            window,
            canvas: Canvas::new(i32::from(width), i32::from(height)),
            presenter,
            mapped: false,
            redraw: true,
            last_volume: None,
        })
    }

    pub fn request_redraw(&mut self) {
        self.redraw = true;
    }

    pub fn sync<C: Connection>(
        &mut self,
        conn: &C,
        visible: bool,
        volume: u8,
    ) -> Result<(), Box<dyn Error>> {
        if !visible {
            if self.mapped {
                conn.unmap_window(self.window)?;
                self.mapped = false;
            }
            return Ok(());
        }

        let newly_mapped = !self.mapped;
        if newly_mapped {
            conn.map_window(self.window)?;
            self.mapped = true;
            self.redraw = true;
        }
        let needs_present = self.redraw || self.last_volume != Some(volume);
        if newly_mapped || needs_present {
            conn.configure_window(
                self.window,
                &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
            )?;
        }
        if needs_present {
            self.canvas.clear(BLACK);
            let rect = RectF::new(0.0, 0.0, VOLUME_HUD.w, VOLUME_HUD.h);
            ui::draw_volume_hud(&mut self.canvas, rect, volume);
            self.presenter.present(conn, &self.canvas, 0)?;
            self.presenter.expose(conn, 0)?;
            self.redraw = false;
            self.last_volume = Some(volume);
        }
        Ok(())
    }

    pub fn destroy<C: Connection>(&self, conn: &C) {
        let _ = conn.destroy_window(self.window);
        self.presenter.destroy(conn);
    }
}

/// Row spans covering a capsule of the given size (pixels whose centers lie
/// inside it), for the window's bounding shape.
fn capsule_spans(width: u16, height: u16) -> Vec<Rectangle> {
    let radius = f32::from(width) / 2.0;
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
            let half = (radius * radius - dy * dy).max(0.0).sqrt();
            let x0 = (radius - half).round() as i16;
            let x1 = (radius + half).round() as i16;
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
    fn capsule_shape_is_full_width_in_the_middle_and_narrow_at_the_ends() {
        let spans = capsule_spans(112, 392);
        assert_eq!(spans.len(), 392);
        assert_eq!(spans[196].width, 112);
        assert!(spans[0].width < 40);
        assert_eq!(spans[0].x as u16 * 2 + spans[0].width, 112);
    }
}
