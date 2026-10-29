use std::error::Error;

use x11rb::COPY_DEPTH_FROM_PARENT;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, EventMask, PropMode,
    StackMode, Window, WindowClass,
};
use x11rb::wrapper::ConnectionExt as _;

use crate::gfx::{Canvas, Rgb};
use crate::model::AppId;
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
/// disappears into it, unless the app in front asked for its own colour
/// (`StatusStyle`); on the lock and home screens Moon draws the same status
/// content directly over the wallpaper instead.
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
    last: Option<(StatusInfo, Option<StatusStyle>)>,
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

    /// Returns true when the bar was restacked above everything. `style` is
    /// the app in front's own colour (None: black).
    pub fn sync<C: Connection>(
        &mut self,
        conn: &C,
        visible: bool,
        status: &StatusInfo,
        style: Option<StatusStyle>,
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
        if self
            .last
            .as_ref()
            .is_none_or(|(last, last_style)| last != status || *last_style != style)
        {
            draw(&mut self.canvas, status, style);
            self.presenter.present(conn, &self.canvas, 0)?;
            self.last = Some((status.clone(), style));
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

/// The bar in an app's colour: light content is drawn over it as on black;
/// dark content is drawn light on black, then turned dark over the colour.
fn draw(canvas: &mut Canvas, status: &StatusInfo, style: Option<StatusStyle>) {
    match style {
        None => ui::draw_status_bar(canvas, status, Some(BLACK)),
        Some(style) if !style.dark_content => {
            ui::draw_status_bar(canvas, status, Some(style.background))
        }
        Some(style) => {
            ui::draw_status_bar(canvas, status, Some(BLACK));
            let rows = usize::from(HEIGHT) * canvas.width as usize;
            for pixel in canvas.pixels.iter_mut().take(rows) {
                *pixel = darkened(*pixel, style.background);
            }
        }
    }
}

/// One pixel of light-on-black content, recoloured for a light background:
/// its brightest channel is how much ink covers it; white and greys become
/// that much black, colours (the battery's green and red, the call pill)
/// keep their hue.
fn darkened(pixel: Rgb, background: Rgb) -> Rgb {
    let channel = |value: Rgb, shift: u32| (value >> shift) & 0xff;
    let (r, g, b) = (channel(pixel, 16), channel(pixel, 8), channel(pixel, 0));
    let coverage = r.max(g).max(b);
    if coverage == 0 {
        return background | 0xff00_0000;
    }
    let neutral = coverage - r.min(g).min(b) < 24;
    let mut out = 0xff00_0000;
    for (shift, value) in [(16, r), (8, g), (0, b)] {
        let ink = if neutral { 0 } else { value * 255 / coverage };
        let back = channel(background, shift);
        let mixed = (back * (255 - coverage) + ink * coverage + 127) / 255;
        out |= mixed << shift;
    }
    out
}

/// An app's own status bar (`statusbar color`): its background and whether
/// the time and glyphs over it are dark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusStyle {
    pub background: Rgb,
    pub dark_content: bool,
}

impl StatusStyle {
    /// `#RRGGBB [light|dark]`; without a choice the content is whichever
    /// reads better on the colour (dark above WCAG's 0.179 luminance, where
    /// black text has the higher contrast).
    pub fn parse(color: &str, content: Option<&str>) -> Result<Self, String> {
        let hex = color
            .strip_prefix('#')
            .filter(|hex| hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| "statusbar color requires #RRGGBB".to_string())?;
        let background = Rgb::from_str_radix(hex, 16).map_err(|error| error.to_string())?;
        let dark_content = match content {
            None => luminance(background) > 0.179,
            Some("dark") => true,
            Some("light") => false,
            Some(_) => return Err("statusbar content is light or dark".into()),
        };
        Ok(Self {
            background,
            dark_content,
        })
    }
}

/// WCAG relative luminance of an sRGB colour.
fn luminance(color: Rgb) -> f32 {
    let linear = |shift: u32| {
        let c = ((color >> shift) & 0xff) as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(16) + 0.7152 * linear(8) + 0.0722 * linear(0)
}

/// The colours apps asked for, each with the process that asked: an app
/// that exits (or restarts) has Moon's black again.
#[derive(Debug, Default, Clone)]
pub struct AppStatusStyles([Option<(u32, StatusStyle)>; AppId::COUNT]);

#[derive(Debug, Clone, serde::Serialize)]
pub struct PublicStatusStyle {
    pub color: String,
    pub content: &'static str,
}

impl AppStatusStyles {
    /// `style` None: back to black.
    pub fn set(&mut self, app: AppId, pid: u32, style: Option<StatusStyle>) {
        self.0[app.index()] = style.map(|style| (pid, style));
    }

    /// The app's colour, while `pid` is still the process that asked.
    pub fn get(&self, app: AppId, pid: Option<u32>) -> Option<StatusStyle> {
        self.0[app.index()]
            .filter(|(asked, _)| Some(*asked) == pid)
            .map(|(_, style)| style)
    }

    /// For `state`: the running apps' colours, by app.
    pub fn public(
        &self,
        pid: impl Fn(AppId) -> Option<u32>,
    ) -> std::collections::BTreeMap<&'static str, PublicStatusStyle> {
        AppId::ALL
            .into_iter()
            .filter_map(|app| {
                let style = self.get(app, pid(app))?;
                Some((
                    app.cgroup_name(),
                    PublicStatusStyle {
                        color: format!("#{:06x}", style.background),
                        content: if style.dark_content { "dark" } else { "light" },
                    },
                ))
            })
            .collect()
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

    #[test]
    fn colours_parse_and_choose_legible_content() {
        let dark = StatusStyle::parse("#00050f", None).unwrap();
        assert_eq!(dark.background, 0x00050f);
        assert!(!dark.dark_content);
        assert!(StatusStyle::parse("#FFFFFF", None).unwrap().dark_content);
        assert!(StatusStyle::parse("#f2f2f7", None).unwrap().dark_content);
        assert!(!StatusStyle::parse("#1c1c1e", None).unwrap().dark_content);
        // Either can be forced.
        assert!(
            StatusStyle::parse("#007aff", Some("dark"))
                .unwrap()
                .dark_content
        );
        assert!(
            !StatusStyle::parse("#ffffff", Some("light"))
                .unwrap()
                .dark_content
        );
        for bad in ["00050f", "#00050", "#00050fa", "#gg0000", "red"] {
            assert!(StatusStyle::parse(bad, None).is_err(), "{bad}");
        }
        assert!(StatusStyle::parse("#000000", Some("grey")).is_err());
    }

    #[test]
    fn dark_content_turns_white_ink_black_and_keeps_colours() {
        let white = 0xffffff;
        let rgb = |pixel: Rgb| pixel & 0xffffff;
        assert_eq!(rgb(darkened(0xff00_0000, white)), white);
        assert_eq!(rgb(darkened(0xffff_ffff, white)), 0x000000);
        // Half-covered antialiasing: half way to black.
        assert_eq!(rgb(darkened(0x808080, white)), 0x7f7f7f);
        // The battery's green stays green.
        let green = rgb(darkened(0x34c759, white));
        assert!(
            (green >> 8) & 0xff > 0xf0 && green >> 16 < 0x80,
            "{green:06x}"
        );
    }

    #[test]
    fn a_colour_lasts_as_long_as_the_process_that_asked() {
        let mut styles = AppStatusStyles::default();
        let style = StatusStyle::parse("#00050f", None).unwrap();
        styles.set(AppId::Exocortex, 41, Some(style));
        assert_eq!(styles.get(AppId::Exocortex, Some(41)), Some(style));
        assert_eq!(styles.get(AppId::Exocortex, Some(42)), None, "restarted");
        assert_eq!(styles.get(AppId::Exocortex, None), None, "exited");
        assert_eq!(styles.get(AppId::System, Some(41)), None);
        let public = styles.public(|app| (app == AppId::Exocortex).then_some(41));
        assert_eq!(public["exocortex"].color, "#00050f");
        assert_eq!(public["exocortex"].content, "light");
        assert!(styles.public(|_| None).is_empty());
        styles.set(AppId::Exocortex, 41, None);
        assert_eq!(styles.get(AppId::Exocortex, Some(41)), None);
    }

    #[test]
    fn bar_is_drawn_in_the_app_colour() {
        let status = StatusInfo {
            time: "9:41".into(),
            wifi_connected: true,
            battery_percent: Some(80),
            battery_charging: false,
            alarm_set: false,
            dnd: false,
            cellular: crate::telephony_client::Cellular::Hidden,
            carrier: None,
            call_pill: false,
        };
        let mut canvas = Canvas::new(1080, i32::from(HEIGHT));
        draw(&mut canvas, &status, None);
        assert_eq!(canvas.pixels[0] & 0xffffff, BLACK);
        for (color, content) in [(0x00050f, None), (0xf2f2f7, None)] {
            let style = StatusStyle::parse(&format!("#{color:06x}"), content).unwrap();
            draw(&mut canvas, &status, Some(style));
            assert_eq!(canvas.pixels[0] & 0xffffff, color);
            // The time is there, in the contrasting ink.
            let ink = if style.dark_content {
                0x000000
            } else {
                0xffffff
            };
            assert!(
                canvas.pixels.iter().any(|p| p & 0xffffff == ink),
                "{color:06x}"
            );
        }
    }
}
