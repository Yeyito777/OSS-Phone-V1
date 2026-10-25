//! Measures the X server operations Moon's compositor relies on, so the
//! frame budget can be checked on the phone's unaccelerated Xorg:
//!
//!     DISPLAY=:0 a26-renderbench [ITERATIONS]
//!
//! Every timing ends with a GetInputFocus round trip, so it includes the time
//! the server spent executing the requests. It also samples the vblank period
//! through Present NotifyMSC. This is a diagnostic tool; Moon itself never
//! waits for replies at runtime.

use std::error::Error;
use std::time::{Duration, Instant};

use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::Event;
use x11rb::protocol::present::{self, ConnectionExt as _};
use x11rb::protocol::render::{self, ConnectionExt as _, CreatePictureAux, PictOp, Transform};
use x11rb::protocol::xproto::{
    ConfigureWindowAux, ConnectionExt as _, CreateGCAux, CreateWindowAux, ImageFormat, StackMode,
    WindowClass,
};

fn fixed(value: f64) -> i32 {
    (value * 65536.0).round() as i32
}

fn scale_transform(scale: f64) -> Transform {
    // Picture transforms map destination to source coordinates.
    let inverse = 1.0 / scale;
    Transform {
        matrix11: fixed(inverse),
        matrix12: 0,
        matrix13: 0,
        matrix21: 0,
        matrix22: fixed(inverse),
        matrix23: 0,
        matrix31: 0,
        matrix32: 0,
        matrix33: fixed(1.0),
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let iterations: u32 = std::env::args()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or(30);
    let (conn, screen_number) = x11rb::connect(None)?;
    let screen = conn.setup().roots[screen_number].clone();
    let (width, height) = (screen.width_in_pixels, screen.height_in_pixels);
    let formats = conn.render_query_pict_formats()?.reply()?;
    let find = |depth: u8, alpha: bool| {
        formats
            .formats
            .iter()
            .find(|f| {
                f.depth == depth
                    && f.type_ == render::PictType::DIRECT
                    && (f.direct.alpha_mask != 0) == alpha
                    && (depth != 24 || f.direct.red_shift == 16)
            })
            .map(|f| f.id)
    };
    let rgb = find(24, false).ok_or("no x8r8g8b8 format")?;
    let argb = find(32, true).ok_or("no a8r8g8b8 format")?;
    let a8 = find(8, true).ok_or("no a8 format")?;

    let window = conn.generate_id()?;
    conn.create_window(
        screen.root_depth,
        window,
        screen.root,
        0,
        0,
        width,
        height,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new()
            .override_redirect(1)
            .background_pixel(0),
    )?;
    conn.map_window(window)?;
    conn.configure_window(
        window,
        &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
    )?;
    let window_picture = conn.generate_id()?;
    conn.render_create_picture(window_picture, window, rgb, &CreatePictureAux::new())?;

    let make = |depth: u8, format: u32| -> Result<(u32, u32), Box<dyn Error>> {
        let pixmap = conn.generate_id()?;
        conn.create_pixmap(depth, pixmap, screen.root, width, height)?;
        let picture = conn.generate_id()?;
        conn.render_create_picture(picture, pixmap, format, &CreatePictureAux::new())?;
        Ok((pixmap, picture))
    };
    let (scene, scene_picture) = make(24, rgb)?;
    let (layer, layer_picture) = make(24, rgb)?;
    let (_overlay, overlay_picture) = make(32, argb)?;
    let (_snapshot, snapshot_picture) = make(24, rgb)?;
    let gc = conn.generate_id()?;
    conn.create_gc(gc, scene, &CreateGCAux::new().graphics_exposures(0))?;

    // Give every buffer real content so no path is short-circuited.
    let rows = 32_u16;
    let stripe: Vec<u8> = (0..usize::from(width) * usize::from(rows) * 4)
        .map(|i| (i * 7 % 251) as u8)
        .collect();
    for y in (0..height).step_by(usize::from(rows)) {
        let h = rows.min(height - y);
        conn.put_image(
            ImageFormat::Z_PIXMAP,
            layer,
            gc,
            width,
            h,
            0,
            y as i16,
            0,
            24,
            &stripe[..usize::from(width) * usize::from(h) * 4],
        )?;
    }
    conn.render_composite(
        PictOp::SRC,
        layer_picture,
        0u32,
        snapshot_picture,
        0,
        0,
        0,
        0,
        0,
        0,
        width,
        height,
    )?;
    conn.render_composite(
        PictOp::SRC,
        layer_picture,
        0u32,
        overlay_picture,
        0,
        0,
        0,
        0,
        0,
        0,
        width,
        height,
    )?;
    sync(&conn)?;

    let measure = |name: &str, op: &mut dyn FnMut() -> Result<(), Box<dyn Error>>| {
        let _ = op();
        let _ = sync(&conn);
        let start = Instant::now();
        for _ in 0..iterations {
            op()?;
        }
        sync(&conn)?;
        let per = start.elapsed().as_secs_f64() * 1000.0 / f64::from(iterations);
        println!("{name:<44} {per:7.2} ms");
        Ok::<(), Box<dyn Error>>(())
    };

    measure("CopyArea full screen pixmap->pixmap", &mut || {
        conn.copy_area(layer, scene, gc, 0, 0, 0, 0, width, height)?;
        Ok(())
    })?;
    measure("CopyArea full screen pixmap->window", &mut || {
        conn.copy_area(scene, window, gc, 0, 0, 0, 0, width, height)?;
        Ok(())
    })?;
    measure("Render SRC full screen", &mut || {
        conn.render_composite(
            PictOp::SRC,
            layer_picture,
            0u32,
            scene_picture,
            0,
            0,
            0,
            0,
            0,
            0,
            width,
            height,
        )?;
        Ok(())
    })?;
    let half = conn.generate_id()?;
    conn.render_create_solid_fill(
        half,
        render::Color {
            red: 0,
            green: 0,
            blue: 0,
            alpha: 0x8000,
        },
    )?;
    measure("Render OVER solid black 50% full screen", &mut || {
        conn.render_composite(
            PictOp::OVER,
            half,
            0u32,
            scene_picture,
            0,
            0,
            0,
            0,
            0,
            0,
            width,
            height,
        )?;
        Ok(())
    })?;
    measure("Render OVER argb layer full screen", &mut || {
        conn.render_composite(
            PictOp::OVER,
            overlay_picture,
            0u32,
            scene_picture,
            0,
            0,
            0,
            0,
            0,
            0,
            width,
            height,
        )?;
        Ok(())
    })?;
    measure("Render OVER rgb layer, 50% alpha, full", &mut || {
        conn.render_composite(
            PictOp::OVER,
            layer_picture,
            half,
            scene_picture,
            0,
            0,
            0,
            0,
            0,
            0,
            width,
            height,
        )?;
        Ok(())
    })?;

    for (label, filter) in [("bilinear", &b"bilinear"[..]), ("nearest", &b"nearest"[..])] {
        conn.render_set_picture_filter(snapshot_picture, filter, &[])?;
        for scale in [0.95, 0.6, 0.3] {
            conn.render_set_picture_transform(snapshot_picture, scale_transform(scale))?;
            let w = (f64::from(width) * scale) as u16;
            let h = (f64::from(height) * scale) as u16;
            measure(&format!("Render SRC scaled {scale} {label}"), &mut || {
                conn.render_composite(
                    PictOp::SRC,
                    snapshot_picture,
                    0u32,
                    scene_picture,
                    0,
                    0,
                    0,
                    0,
                    40,
                    40,
                    w,
                    h,
                )?;
                Ok(())
            })?;
        }
    }
    // Frame-shaped operations: a scaled card with an alpha mask, a clipped
    // destination, and an A8 corner mask.
    conn.render_set_picture_filter(snapshot_picture, b"bilinear", &[])?;
    conn.render_set_picture_transform(snapshot_picture, scale_transform(0.62))?;
    let (cw, ch) = (
        (f64::from(width) * 0.62) as u16,
        (f64::from(height) * 0.62) as u16,
    );
    measure("Render OVER scaled 0.62 bilinear (no mask)", &mut || {
        conn.render_composite(
            PictOp::OVER,
            snapshot_picture,
            0u32,
            scene_picture,
            0,
            0,
            0,
            0,
            200,
            400,
            cw,
            ch,
        )?;
        Ok(())
    })?;
    measure("Render OVER scaled 0.62 bilinear, 50% mask", &mut || {
        conn.render_composite(
            PictOp::OVER,
            snapshot_picture,
            half,
            scene_picture,
            0,
            0,
            0,
            0,
            200,
            400,
            cw,
            ch,
        )?;
        Ok(())
    })?;
    conn.render_set_picture_clip_rectangles(
        scene_picture,
        0,
        0,
        &[x11rb::protocol::xproto::Rectangle {
            x: 100,
            y: 300,
            width: 900,
            height: 1700,
        }],
    )?;
    measure("Render OVER scaled 0.62 bilinear, clipped", &mut || {
        conn.render_composite(
            PictOp::OVER,
            snapshot_picture,
            0u32,
            scene_picture,
            0,
            0,
            0,
            0,
            200,
            400,
            cw,
            ch,
        )?;
        Ok(())
    })?;
    measure("Render SRC full layer, clipped", &mut || {
        conn.render_composite(
            PictOp::SRC,
            layer_picture,
            0u32,
            scene_picture,
            0,
            0,
            0,
            0,
            0,
            0,
            width,
            height,
        )?;
        Ok(())
    })?;
    measure("FillRectangles OVER 55% black, clipped", &mut || {
        conn.render_fill_rectangles(
            PictOp::OVER,
            scene_picture,
            render::Color {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 0x8ccc,
            },
            &[x11rb::protocol::xproto::Rectangle {
                x: 0,
                y: 0,
                width,
                height,
            }],
        )?;
        Ok(())
    })?;
    let mask_pixmap = conn.generate_id()?;
    conn.create_pixmap(8, mask_pixmap, screen.root, 64, 64)?;
    let mask_gc = conn.generate_id()?;
    conn.create_gc(mask_gc, mask_pixmap, &CreateGCAux::new())?;
    conn.put_image(
        ImageFormat::Z_PIXMAP,
        mask_pixmap,
        mask_gc,
        64,
        64,
        0,
        0,
        0,
        8,
        &[128_u8; 64 * 64],
    )?;
    let corner = conn.generate_id()?;
    conn.render_create_picture(corner, mask_pixmap, a8, &CreatePictureAux::new())?;
    measure("4 corners: layer through A8 mask (x10)", &mut || {
        for _ in 0..10 {
            for (x, y) in [(200, 400), (800, 400), (200, 1800), (800, 1800)] {
                conn.render_composite(
                    PictOp::OVER,
                    layer_picture,
                    corner,
                    scene_picture,
                    x,
                    y,
                    0,
                    0,
                    x,
                    y,
                    64,
                    64,
                )?;
            }
        }
        Ok(())
    })?;
    conn.render_set_picture_clip_rectangles(
        scene_picture,
        0,
        0,
        &[x11rb::protocol::xproto::Rectangle {
            x: 0,
            y: 0,
            width,
            height,
        }],
    )?;

    measure("PutImage full screen (socket upload)", &mut || {
        for y in (0..height).step_by(usize::from(rows)) {
            let h = rows.min(height - y);
            conn.put_image(
                ImageFormat::Z_PIXMAP,
                layer,
                gc,
                width,
                h,
                0,
                y as i16,
                0,
                24,
                &stripe[..usize::from(width) * usize::from(h) * 4],
            )?;
        }
        Ok(())
    })?;

    // Vblank period through Present NotifyMSC.
    if conn
        .extension_information(present::X11_EXTENSION_NAME)?
        .is_some()
    {
        conn.present_query_version(1, 2)?.reply()?;
        let eid = conn.generate_id()?;
        conn.present_select_input(eid, window, present::EventMask::COMPLETE_NOTIFY)?;
        let mut stamps = Vec::new();
        conn.present_notify_msc(window, 0, 0, 1, 0)?;
        conn.flush()?;
        let deadline = Instant::now() + Duration::from_secs(3);
        while stamps.len() < 121 && Instant::now() < deadline {
            if let Event::PresentCompleteNotify(event) = conn.wait_for_event()? {
                stamps.push((event.msc, event.ust, Instant::now()));
                conn.present_notify_msc(window, stamps.len() as u32, 0, 1, 0)?;
                conn.flush()?;
            }
        }
        if stamps.len() > 2 {
            let n = stamps.len() - 1;
            let msc_span = stamps[n].0 - stamps[0].0;
            let ust_span = stamps[n].1 - stamps[0].1;
            let wall = stamps[n].2 - stamps[0].2;
            let intervals: Vec<f64> = stamps
                .windows(2)
                .map(|pair| (pair[1].2 - pair[0].2).as_secs_f64() * 1000.0)
                .collect();
            let worst = intervals.iter().cloned().fold(0.0, f64::max);
            println!(
                "Present NotifyMSC: {n} events, msc +{msc_span}, ust period {:.3} ms, wall period {:.3} ms, worst gap {worst:.2} ms",
                ust_span as f64 / 1000.0 / msc_span.max(1) as f64,
                wall.as_secs_f64() * 1000.0 / n as f64,
            );
        } else {
            println!("Present NotifyMSC: no events");
        }
    } else {
        println!("Present: extension absent");
    }
    Ok(())
}

fn sync<C: Connection>(conn: &C) -> Result<(), Box<dyn Error>> {
    conn.get_input_focus()?.reply()?;
    Ok(())
}
