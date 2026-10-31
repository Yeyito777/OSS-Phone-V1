use std::error::Error;
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat};

/// Asks Moon to make the X root show what the panel does: a Wayland app
/// shown from its own buffers is copied into its X window first. Best
/// effort: an older Moon, or none, leaves the root as it is.
fn sync_with_moon() {
    let socket = std::env::var_os("A26_SHELL_SOCKET")
        .unwrap_or_else(|| "/run/a26-shell/control.sock".into());
    let Ok(mut stream) = UnixStream::connect(socket) else {
        return;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
    if stream.write_all(b"screen sync\n").is_ok() {
        let _ = stream.shutdown(std::net::Shutdown::Write);
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let output = std::env::args()
        .nth(1)
        .ok_or("usage: a26-shellshot OUTPUT.ppm")?;
    sync_with_moon();
    let (conn, screen_number) = x11rb::connect(None)?;
    let screen = &conn.setup().roots[screen_number];
    // GetImage on the root drawable captures the visible screen including child
    // windows. Capturing the topmost child alone breaks as soon as a small
    // override-redirect surface (keyboard or volume OSD) is present.
    let window = screen.root;
    let geometry = conn.get_geometry(window)?.reply()?;
    let image = conn
        .get_image(
            ImageFormat::Z_PIXMAP,
            window,
            0,
            0,
            geometry.width,
            geometry.height,
            u32::MAX,
        )?
        .reply()?;

    let expected = usize::from(geometry.width) * usize::from(geometry.height) * 4;
    if image.data.len() != expected {
        return Err(format!(
            "expected 32-bit XRGB data ({expected} bytes), received {}",
            image.data.len()
        )
        .into());
    }

    let mut writer = BufWriter::new(File::create(output)?);
    write!(writer, "P6\n{} {}\n255\n", geometry.width, geometry.height)?;
    for bytes in image.data.chunks_exact(4) {
        // The exact A26 Xorg target is little-endian depth-24 XRGB8888.
        writer.write_all(&[bytes[2], bytes[1], bytes[0]])?;
    }
    writer.flush()?;
    Ok(())
}
