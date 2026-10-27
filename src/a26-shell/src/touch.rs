//! The touchscreen read directly from evdev (multi-touch protocol B), for
//! when Moon routes touches itself instead of receiving XInput 2 raw touch
//! events from Xorg (milestone 3 of the Wayland plan: without Xorg, nobody
//! else reads the touchscreen for Moon).
//!
//! Like the key readers in `input.rs` the device is never grabbed and read
//! nonblocking; each `SYN_REPORT` turns slot changes into begin/update/end
//! contacts in screen pixels.

use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind, Read};
use std::mem::size_of;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

const EV_SYN: u16 = 0x00;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0;
const SYN_DROPPED: u16 = 3;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;
const SLOTS: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Contact {
    /// A finger touched down: (id, x, y) in screen pixels.
    Begin(u32, i16, i16),
    Update(u32, i16, i16),
    End(u32),
}

#[derive(Debug, Clone, Copy, Default)]
struct Slot {
    /// The kernel's tracking id, -1 (none) when no finger is on the slot.
    tracking: i32,
    x: i32,
    y: i32,
    /// The contact Moon knows (begun and not ended).
    active: bool,
    moved: bool,
}

#[derive(Debug, Clone, Copy)]
struct Axis {
    minimum: i32,
    maximum: i32,
}

impl Axis {
    fn to_screen(self, value: i32, size: u16) -> i16 {
        let span = i64::from(self.maximum - self.minimum).max(1);
        let scaled = (i64::from(value - self.minimum) * i64::from(size)) / (span + 1);
        scaled.clamp(0, i64::from(size.saturating_sub(1))) as i16
    }
}

pub struct Touchscreen {
    file: File,
    path: PathBuf,
    pending: Vec<u8>,
    slots: [Slot; SLOTS],
    slot: usize,
    axes: (Axis, Axis),
    screen: (u16, u16),
}

/// `EVIOCGABS(axis)`: _IOR('E', 0x40 + axis, struct input_absinfo).
fn eviocgabs(axis: u16) -> libc::c_ulong {
    (2 << 30)
        | ((size_of::<[i32; 6]>() as libc::c_ulong) << 16)
        | (0x45 << 8)
        | (0x40 + axis as libc::c_ulong)
}

/// `EVIOCGNAME(len)`: _IOC(_IOC_READ, 'E', 0x06, len).
fn eviocgname(len: usize) -> libc::c_ulong {
    (2 << 30) | ((len as libc::c_ulong) << 16) | (0x45 << 8) | 0x06
}

fn abs_info(file: &File, axis: u16) -> io::Result<Axis> {
    let mut info = [0_i32; 6];
    // SAFETY: EVIOCGABS fills one input_absinfo (six i32) for a valid fd.
    let result = unsafe { libc::ioctl(file.as_raw_fd(), eviocgabs(axis) as _, info.as_mut_ptr()) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Axis {
        minimum: info[1],
        maximum: info[2],
    })
}

fn device_name(file: &File) -> Option<String> {
    let mut name = [0_u8; 128];
    // SAFETY: EVIOCGNAME writes at most `name.len()` bytes.
    let length = unsafe {
        libc::ioctl(
            file.as_raw_fd(),
            eviocgname(name.len()) as _,
            name.as_mut_ptr(),
        )
    };
    if length <= 0 {
        return None;
    }
    let end = name.iter().position(|&b| b == 0).unwrap_or(length as usize);
    Some(String::from_utf8_lossy(&name[..end]).into_owned())
}

impl Touchscreen {
    /// The first `/dev/input/event*` device with this name (the A26's is
    /// `sec_touchscreen`), mapped onto a `width` x `height` screen.
    pub fn find(name: &str, width: u16, height: u16) -> io::Result<Self> {
        let mut entries: Vec<PathBuf> = std::fs::read_dir("/dev/input")?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("event"))
            })
            .collect();
        entries.sort();
        for path in entries {
            let Ok(file) = open(&path) else {
                continue;
            };
            if device_name(&file).as_deref() == Some(name) {
                return Self::from_file(file, path, width, height);
            }
        }
        Err(io::Error::new(
            ErrorKind::NotFound,
            format!("no input device named {name}"),
        ))
    }

    fn from_file(file: File, path: PathBuf, width: u16, height: u16) -> io::Result<Self> {
        let axes = (
            abs_info(&file, ABS_MT_POSITION_X)?,
            abs_info(&file, ABS_MT_POSITION_Y)?,
        );
        Ok(Self {
            file,
            path,
            pending: Vec::new(),
            slots: [Slot {
                tracking: -1,
                ..Slot::default()
            }; SLOTS],
            slot: 0,
            axes,
            screen: (width, height),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn raw_fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }

    /// Reads what the device has and returns the contacts it completed.
    pub fn poll(&mut self) -> io::Result<Vec<Contact>> {
        let mut bytes = [0_u8; 24 * 64];
        loop {
            match self.file.read(&mut bytes) {
                Ok(0) => break,
                Ok(count) => self.pending.extend_from_slice(&bytes[..count]),
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            }
        }
        let timeval_bytes = size_of::<libc::timeval>();
        let event_bytes = timeval_bytes + 8;
        let complete = self.pending.len() / event_bytes * event_bytes;
        let mut contacts = Vec::new();
        let events: Vec<(u16, u16, i32)> = self.pending[..complete]
            .chunks_exact(event_bytes)
            .map(|event| {
                let field = |at: usize| [event[at], event[at + 1]];
                (
                    u16::from_ne_bytes(field(timeval_bytes)),
                    u16::from_ne_bytes(field(timeval_bytes + 2)),
                    i32::from_ne_bytes([
                        event[timeval_bytes + 4],
                        event[timeval_bytes + 5],
                        event[timeval_bytes + 6],
                        event[timeval_bytes + 7],
                    ]),
                )
            })
            .collect();
        self.pending.drain(..complete);
        for (kind, code, value) in events {
            self.feed(kind, code, value, &mut contacts);
        }
        Ok(contacts)
    }

    fn feed(&mut self, kind: u16, code: u16, value: i32, contacts: &mut Vec<Contact>) {
        match (kind, code) {
            (EV_ABS, ABS_MT_SLOT) => {
                self.slot = usize::try_from(value).unwrap_or(0).min(SLOTS - 1);
            }
            (EV_ABS, ABS_MT_TRACKING_ID) => {
                self.slots[self.slot].tracking = value;
                self.slots[self.slot].moved = true;
            }
            (EV_ABS, ABS_MT_POSITION_X) => {
                self.slots[self.slot].x = value;
                self.slots[self.slot].moved = true;
            }
            (EV_ABS, ABS_MT_POSITION_Y) => {
                self.slots[self.slot].y = value;
                self.slots[self.slot].moved = true;
            }
            (EV_SYN, SYN_REPORT) => self.report(contacts),
            (EV_SYN, SYN_DROPPED) => {
                // Events were lost: end every contact and start over.
                for (index, slot) in self.slots.iter_mut().enumerate() {
                    if slot.active {
                        contacts.push(Contact::End(index as u32));
                    }
                    *slot = Slot {
                        tracking: -1,
                        ..Slot::default()
                    };
                }
            }
            _ => {}
        }
    }

    fn report(&mut self, contacts: &mut Vec<Contact>) {
        let (width, height) = self.screen;
        let (ax, ay) = self.axes;
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if !std::mem::take(&mut slot.moved) {
                continue;
            }
            // The slot index identifies the contact for its whole life.
            let id = index as u32;
            let (x, y) = (ax.to_screen(slot.x, width), ay.to_screen(slot.y, height));
            match (slot.active, slot.tracking >= 0) {
                (false, true) => {
                    slot.active = true;
                    contacts.push(Contact::Begin(id, x, y));
                }
                (true, true) => contacts.push(Contact::Update(id, x, y)),
                (true, false) => {
                    slot.active = false;
                    contacts.push(Contact::End(id));
                }
                (false, false) => {}
            }
        }
    }
}

fn open(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> Touchscreen {
        Touchscreen {
            file: File::open("/dev/null").unwrap(),
            path: PathBuf::from("/dev/null"),
            pending: Vec::new(),
            slots: [Slot {
                tracking: -1,
                ..Slot::default()
            }; SLOTS],
            slot: 0,
            axes: (
                Axis {
                    minimum: 0,
                    maximum: 4319,
                },
                Axis {
                    minimum: 0,
                    maximum: 9359,
                },
            ),
            screen: (1080, 2340),
        }
    }

    #[test]
    fn protocol_b_contacts_begin_move_and_end_per_slot() {
        let mut touch = screen();
        let mut out = Vec::new();
        for (kind, code, value) in [
            (EV_ABS, ABS_MT_SLOT, 0),
            (EV_ABS, ABS_MT_TRACKING_ID, 7),
            (EV_ABS, ABS_MT_POSITION_X, 2160),
            (EV_ABS, ABS_MT_POSITION_Y, 4680),
            (EV_SYN, SYN_REPORT, 0),
            (EV_ABS, ABS_MT_SLOT, 1),
            (EV_ABS, ABS_MT_TRACKING_ID, 8),
            (EV_ABS, ABS_MT_POSITION_X, 0),
            (EV_ABS, ABS_MT_POSITION_Y, 0),
            (EV_ABS, ABS_MT_SLOT, 0),
            (EV_ABS, ABS_MT_POSITION_X, 4319),
            (EV_SYN, SYN_REPORT, 0),
            (EV_ABS, ABS_MT_TRACKING_ID, -1),
            (EV_SYN, SYN_REPORT, 0),
        ] {
            touch.feed(kind, code, value, &mut out);
        }
        assert_eq!(
            out,
            vec![
                Contact::Begin(0, 540, 1170),
                Contact::Update(0, 1079, 1170),
                Contact::Begin(1, 0, 0),
                Contact::End(0),
            ]
        );
    }

    #[test]
    fn dropped_events_end_every_contact() {
        let mut touch = screen();
        let mut out = Vec::new();
        touch.feed(EV_ABS, ABS_MT_TRACKING_ID, 3, &mut out);
        touch.feed(EV_SYN, SYN_REPORT, 0, &mut out);
        touch.feed(EV_SYN, SYN_DROPPED, 0, &mut out);
        assert_eq!(out, vec![Contact::Begin(0, 0, 0), Contact::End(0)]);
    }
}
