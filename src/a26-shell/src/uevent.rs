//! The kernel's power-supply uevents: plugging a charger in or out wakes
//! Moon at once (the charging card, the bolt) instead of at the next status
//! read, without polling. The charger's interrupt also wakes a suspended
//! phone, so this works with the screen off.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

pub struct PowerSupplyEvents {
    socket: OwnedFd,
}

impl PowerSupplyEvents {
    pub fn open() -> io::Result<Self> {
        // SAFETY: plain socket(2); the result is checked before use.
        let fd = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_DGRAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                libc::NETLINK_KOBJECT_UEVENT,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fd is a fresh descriptor owned by nothing else.
        let socket = unsafe { OwnedFd::from_raw_fd(fd) };
        // SAFETY: sockaddr_nl is plain data; zeroed is a valid value.
        let mut address: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        address.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        // The kernel's broadcast group.
        address.nl_groups = 1;
        // SAFETY: address is a valid sockaddr_nl for its full size.
        let bound = unsafe {
            libc::bind(
                socket.as_raw_fd(),
                (&raw const address).cast::<libc::sockaddr>(),
                std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            )
        };
        if bound < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { socket })
    }

    pub fn raw_fd(&self) -> RawFd {
        self.socket.as_raw_fd()
    }

    /// Reads every pending uevent; true when one was a power supply's.
    pub fn drain(&self) -> bool {
        let mut buffer = [0_u8; 4096];
        let mut power_supply = false;
        loop {
            // SAFETY: buffer is valid for writes of its full length.
            let count = unsafe {
                libc::recv(
                    self.socket.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    0,
                )
            };
            if count <= 0 {
                return power_supply;
            }
            power_supply |= is_power_supply(&buffer[..count as usize]);
        }
    }
}

/// A uevent is `action@devpath` and NUL-separated `KEY=value` fields.
fn is_power_supply(message: &[u8]) -> bool {
    message
        .split(|&byte| byte == 0)
        .any(|field| field == b"SUBSYSTEM=power_supply")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_supply_uevents_are_recognized() {
        let charger = b"change@/devices/platform/battery/power_supply/battery\0ACTION=change\0SUBSYSTEM=power_supply\0POWER_SUPPLY_STATUS=Charging\0";
        assert!(is_power_supply(charger));
        let other =
            b"change@/devices/virtual/thermal/thermal_zone0\0ACTION=change\0SUBSYSTEM=thermal\0";
        assert!(!is_power_supply(other));
    }
}
