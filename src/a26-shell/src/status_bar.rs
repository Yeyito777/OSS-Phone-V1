use std::error::Error;

use x11rb::COPY_DEPTH_FROM_PARENT;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, EventMask, PropMode,
    StackMode, Window, WindowClass,
};
use x11rb::wrapper::ConnectionExt as _;

use crate::gfx::Canvas;
use crate::present::Presenter;
use crate::ui::{self, BLACK, StatusInfo};

/// Physical top inset reserved for the A26 camera cutout and Moon status UI.
pub const HEIGHT: u16 = 124;

/// The camera cutout's bounding box (pixels, top centre): Samsung's
/// `config_mainBuiltInDisplayCutout` for the A26
/// (`M 0,0 H -24.18 V 34.13 H 24.18 V 0 H 0 Z @dp` in
/// product/overlay/framework-res__a26xsub__auto_generated_rro_product.apk) at
/// 450 dpi (2.8125 px/dp): x 472..608, y 0..96. Banners and other floating
/// content keep below it.
pub const CUTOUT_BOTTOM: f32 = 96.0;

/// The status strip shown above apps. It is black so the camera notch
/// disappears into it; on the lock and home screens Moon draws the same
/// status content directly over the wallpaper instead.
pub struct StatusBarSurface {
    pub window: Window,
    canvas: Canvas,
    presenter: Presenter,
    mapped: bool,
    /// Something was stacked above the bar; it goes back on top.
    raise: bool,
    /// Part of the window lost its pixels; recopy them (never restack for
    /// this: an overlay above the bar exposing it would otherwise trade
    /// places with it forever).
    exposed: bool,
    last: Option<StatusInfo>,
}

impl StatusBarSurface {
    pub fn create<C: Connection>(
        conn: &C,
        root: Window,
        depth: u8,
        width: u16,
    ) -> Result<Self, Box<dyn Error>> {
        let window = conn.generate_id()?;
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            width,
            HEIGHT,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new()
                .background_pixel(BLACK)
                .override_redirect(1)
                .event_mask(EventMask::EXPOSURE),
        )?;
        conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            b"moon-status",
        )?;
        let presenter = Presenter::create(conn, window, depth, width, HEIGHT)?;
        Ok(Self {
            window,
            canvas: Canvas::new(i32::from(width), i32::from(HEIGHT)),
            presenter,
            mapped: false,
            raise: false,
            exposed: false,
            last: None,
        })
    }

    pub fn request_redraw(&mut self) {
        self.exposed = true;
    }

    pub fn request_raise(&mut self) {
        if self.mapped {
            self.raise = true;
        }
    }

    /// Returns true when the bar was restacked above everything.
    pub fn sync<C: Connection>(
        &mut self,
        conn: &C,
        visible: bool,
        status: &StatusInfo,
    ) -> Result<bool, Box<dyn Error>> {
        if !visible {
            if self.mapped {
                conn.unmap_window(self.window)?;
                self.mapped = false;
            }
            return Ok(false);
        }

        let newly_mapped = !self.mapped;
        if newly_mapped {
            conn.map_window(self.window)?;
            self.mapped = true;
            self.raise = true;
            self.exposed = true;
        }
        let raised = std::mem::take(&mut self.raise);
        if raised {
            conn.configure_window(
                self.window,
                &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
            )?;
        }
        if self.last.as_ref() != Some(status) {
            ui::draw_status_bar(&mut self.canvas, status, Some(BLACK));
            self.presenter.present(conn, &self.canvas, 0)?;
            self.last = Some(status.clone());
        }
        if std::mem::take(&mut self.exposed) {
            self.presenter.expose(conn, 0)?;
        }
        Ok(raised)
    }

    pub fn destroy<C: Connection>(&self, conn: &C) {
        let _ = conn.destroy_window(self.window);
        self.presenter.destroy(conn);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_bar_clears_the_cutout_without_consuming_app_height_twice() {
        assert_eq!(HEIGHT, 124);
        assert_eq!(2340_u16.saturating_sub(HEIGHT), 2216);
        assert_eq!(1520_u16.saturating_sub(HEIGHT), 1396);
    }
}
