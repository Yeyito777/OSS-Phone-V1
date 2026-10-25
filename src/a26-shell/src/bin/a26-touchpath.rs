//! Injects a touch path into Moon at the panel's 120 Hz, for measuring
//! gestures on the device without touching the screen:
//!
//!     a26-touchpath X0 Y0 X1 Y1 DURATION_MS [HOLD_MS]
//!
//! It sends `pointer-begin`, a `pointer-move` every 8.33 ms along a straight
//! line (ease-out, like a finger decelerating), optionally holds still at the
//! end, and sends `pointer-end`. Each command uses Moon's control socket
//! (`A26_SHELL_SOCKET`, default /run/a26-shell/control.sock).

use std::error::Error;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn send(socket: &PathBuf, command: &str) -> Result<(), Box<dyn Error>> {
    let mut stream = UnixStream::connect(socket)?;
    stream.write_all(command.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    if !response.contains("\"ok\":true") {
        return Err(format!("{command}: {response}").into());
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<f32> = std::env::args()
        .skip(1)
        .map(|a| a.parse::<f32>())
        .collect::<Result<_, _>>()
        .map_err(|_| "usage: a26-touchpath X0 Y0 X1 Y1 DURATION_MS [HOLD_MS]")?;
    if args.len() < 5 {
        return Err("usage: a26-touchpath X0 Y0 X1 Y1 DURATION_MS [HOLD_MS]".into());
    }
    let socket: PathBuf = std::env::var_os("A26_SHELL_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/run/a26-shell/control.sock".into());
    let (x0, y0, x1, y1) = (args[0], args[1], args[2], args[3]);
    let duration = Duration::from_secs_f32(args[4].max(1.0) / 1000.0);
    let hold = Duration::from_secs_f32(args.get(5).copied().unwrap_or(0.0).max(0.0) / 1000.0);
    let period = Duration::from_nanos(8_333_333);
    let point = |t: f32| {
        let e = 1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(2);
        ((x0 + (x1 - x0) * e).round(), (y0 + (y1 - y0) * e).round())
    };
    send(
        &socket,
        &format!("pointer-begin {} {}", x0.round(), y0.round()),
    )?;
    let start = Instant::now();
    let mut next = start + period;
    loop {
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        }
        let elapsed = next.duration_since(start);
        let (x, y) = point(elapsed.as_secs_f32() / duration.as_secs_f32());
        send(&socket, &format!("pointer-move {x} {y}"))?;
        if elapsed >= duration + hold {
            break;
        }
        next += period;
    }
    let (x, y) = point(1.0);
    send(&socket, &format!("pointer-end {x} {y}"))?;
    Ok(())
}
