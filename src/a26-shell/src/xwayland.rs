//! Xwayland as Moon's X server (milestone 4, opt-in with MOON_XWAYLAND).
//!
//! Without Xorg, Moon starts first: its Wayland server binds, then Moon
//! starts Xwayland on one end of a socket pair (WAYLAND_SOCKET), serves it
//! until it reports its display number (-displayfd), and only then
//! connects to that display as an X client, exactly as it connects to Xorg.
//! Xwayland runs rootful at the panel's size: everything Moon does in X
//! (its windows, the mirror, XTEST keys, the Wayland proxies) is unchanged;
//! what changes is that Moon reads the touchscreen itself and hands every
//! touch to Xwayland (`WaylandServer::xwayland_touch_down`).

use std::error::Error;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::wayland::WaylandServer;

/// How long Xwayland may take to start serving X.
const START_TIMEOUT: Duration = Duration::from_secs(15);
/// The descriptors the child finds its Wayland socket and -displayfd on.
const CHILD_WAYLAND_FD: i32 = 3;
const CHILD_DISPLAY_FD: i32 = 4;

pub struct Xwayland {
    child: Child,
}

impl Xwayland {
    /// Starts `command` (the Xwayland binary, or a wrapper taking its
    /// arguments) rootful at `size`, serves `server` until it is ready,
    /// and points DISPLAY at it.
    pub fn start(
        server: &mut WaylandServer,
        command: &str,
        size: (u16, u16),
    ) -> Result<Self, Box<dyn Error>> {
        let (ours, theirs) = UnixStream::pair()?;
        server.insert_xwayland(ours)?;
        let (display_read, display_write) = pipe()?;
        let mut child = {
            let socket = theirs.as_raw_fd();
            let display = display_write.as_raw_fd();
            let mut command = Command::new(command);
            command
                .args([
                    "-displayfd",
                    &CHILD_DISPLAY_FD.to_string(),
                    "-geometry",
                    &format!("{}x{}", size.0, size.1),
                    "-nolisten",
                    "tcp",
                    "-noreset",
                ])
                .env("WAYLAND_SOCKET", CHILD_WAYLAND_FD.to_string())
                .env_remove("WAYLAND_DISPLAY")
                .env_remove("DISPLAY")
                .stdin(Stdio::null());
            // SAFETY: only async-signal-safe calls between fork and exec.
            unsafe {
                command.pre_exec(move || {
                    // Out of the way first (either may sit on 3 or 4), then
                    // into place; dup2 clears close-on-exec on the copy.
                    let socket = libc::fcntl(socket, libc::F_DUPFD_CLOEXEC, 10);
                    let display = libc::fcntl(display, libc::F_DUPFD_CLOEXEC, 10);
                    for (from, to) in [(socket, CHILD_WAYLAND_FD), (display, CHILD_DISPLAY_FD)] {
                        if from < 0 || libc::dup2(from, to) < 0 {
                            return Err(io::Error::last_os_error());
                        }
                    }
                    Ok(())
                });
            }
            command.spawn()?
        };
        drop(theirs);
        drop(display_write);
        eprintln!("xwayland: started {command} (pid {})", child.id());

        let mut display_pipe = File::from(display_read);
        let mut text = String::new();
        let started = Instant::now();
        let display = loop {
            if let Some(status) = child.try_wait()? {
                return Err(format!("Xwayland exited during startup: {status}").into());
            }
            if started.elapsed() > START_TIMEOUT {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Xwayland did not report a display".into());
            }
            let [listen, clients] = server.poll_fds();
            let mut fds = [display_pipe.as_raw_fd(), listen, clients].map(|fd| libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            });
            // SAFETY: `fds` is a valid array of pollfd for its length.
            unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 100) };
            server.dispatch();
            if fds[0].revents != 0 {
                let mut bytes = [0_u8; 32];
                let count = display_pipe.read(&mut bytes)?;
                text.push_str(&String::from_utf8_lossy(&bytes[..count]));
                if let Some(line) = text.strip_suffix('\n') {
                    break line.trim().parse::<u32>()?;
                }
                if count == 0 {
                    return Err("Xwayland closed -displayfd without a display".into());
                }
            }
        };
        // SAFETY: Moon is still single-threaded here; apps inherit DISPLAY.
        unsafe { std::env::set_var("DISPLAY", format!(":{display}")) };
        eprintln!(
            "xwayland: display :{display} ready after {} ms",
            started.elapsed().as_millis()
        );
        Ok(Self { child })
    }

    /// Moon cannot run without its X server: the caller exits on Some.
    pub fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child.try_wait().ok().flatten()
    }
}

impl Drop for Xwayland {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: pipe2 writes two descriptors into `fds`.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both descriptors are new and owned by nothing else.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}
