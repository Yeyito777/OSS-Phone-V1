//! Where a dma-heap wl_shm buffer's time goes on the phone (the demo painted
//! 3.8 ms a frame into dma-heap buffers against 2.0 ms into a memfd, and
//! Moon's cache flush took 0.8 ms). For a full-screen buffer (4352-byte
//! rows, 2217 rows) in a memfd, the `system` heap and `system-uncached`:
//!
//! - paint: every row filled, as the demo paints (write bandwidth);
//! - read: every pixel summed (uncached memory reads ~30x slower);
//! - each DMA_BUF_IOCTL_SYNC variant Moon could use to clean the caches,
//!   and the paint that follows it (an invalidate may slow the next one);
//! - whether the kernel has a partial sync (Android's
//!   DMA_BUF_IOCTL_SYNC_PARTIAL) and what a damage-sized one costs;
//! - the mapping's VmFlags from /proc/self/smaps.
//!
//!     heapbench [ROUNDS]
//!
//! Read-only for the system: it allocates and frees its own buffers.

use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::Instant;

const STRIDE: usize = 4352;
const ROWS: usize = 2217;
const LEN: usize = STRIDE * ROWS;
const DMA_HEAP_IOCTL_ALLOC: libc::c_ulong = 0xc018_4800;
const DMA_BUF_IOCTL_SYNC: libc::c_ulong = 0x4008_6200;
const SYNC_READ: u64 = 1;
const SYNC_WRITE: u64 = 2;
const SYNC_END: u64 = 4;

fn heap_alloc(heap: &str) -> io::Result<OwnedFd> {
    #[repr(C)]
    struct Allocation {
        len: u64,
        fd: u32,
        fd_flags: u32,
        heap_flags: u64,
    }
    let file = File::open(format!("/dev/dma_heap/{heap}"))?;
    let mut allocation = Allocation {
        len: LEN.div_ceil(4096) as u64 * 4096,
        fd: 0,
        fd_flags: (libc::O_RDWR | libc::O_CLOEXEC) as u32,
        heap_flags: 0,
    };
    // SAFETY: DMA_HEAP_IOCTL_ALLOC fills `allocation.fd` on success.
    if unsafe { libc::ioctl(file.as_raw_fd(), DMA_HEAP_IOCTL_ALLOC as _, &mut allocation) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a descriptor the kernel just returned.
    Ok(unsafe { OwnedFd::from_raw_fd(allocation.fd as i32) })
}

fn memfd() -> io::Result<OwnedFd> {
    let name = std::ffi::CString::new("heapbench").unwrap();
    // SAFETY: a fresh memfd.
    let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a descriptor we own.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    File::from(fd.try_clone()?).set_len(LEN as u64)?;
    Ok(fd)
}

fn map(fd: RawFd) -> io::Result<&'static mut [u32]> {
    // SAFETY: a shared mapping of LEN bytes of the descriptor; leaked for
    // the process's life (a benchmark).
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            LEN,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: LEN bytes just mapped, u32-aligned (page-aligned).
    Ok(unsafe { std::slice::from_raw_parts_mut(ptr.cast(), LEN / 4) })
}

fn sync(fd: RawFd, flags: u64) -> bool {
    // SAFETY: DMA_BUF_IOCTL_SYNC reads one u64.
    unsafe { libc::ioctl(fd, DMA_BUF_IOCTL_SYNC as _, &flags) == 0 }
}

/// _IOW('b', nr, 16 bytes): Android's DMA_BUF_IOCTL_SYNC_PARTIAL is one of
/// these (struct { u64 flags; u32 offset; u32 len; }).
fn partial_request(nr: u64) -> libc::c_ulong {
    ((1u64 << 30) | (16 << 16) | ((b'b' as u64) << 8) | nr) as libc::c_ulong
}

fn partial(fd: RawFd, request: libc::c_ulong, flags: u64, offset: u32, len: u32) -> io::Result<()> {
    #[repr(C)]
    struct Partial {
        flags: u64,
        offset: u32,
        len: u32,
    }
    let arg = Partial { flags, offset, len };
    // SAFETY: the kernel reads 16 bytes (or refuses an unknown request).
    if unsafe { libc::ioctl(fd, request as _, &arg) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn paint(pixels: &mut [u32], round: u32) {
    for (row, line) in pixels.chunks_exact_mut(STRIDE / 4).enumerate() {
        line.fill(0xff00_0000 | (row as u32).wrapping_mul(2654435761).wrapping_add(round));
    }
}

fn read(pixels: &[u32]) -> u32 {
    pixels.iter().fold(0u32, |sum, &value| sum.wrapping_add(value))
}

/// Mean and max of `rounds` runs of `f`, in ms.
fn time(rounds: u32, mut f: impl FnMut(u32)) -> String {
    let mut total = 0.0;
    let mut max: f64 = 0.0;
    for round in 0..rounds {
        let started = Instant::now();
        f(round);
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        total += ms;
        max = max.max(ms);
    }
    format!("{:6.2} ({:6.2})", total / f64::from(rounds), max)
}

fn vm_flags(pixels: &[u32]) -> String {
    let start = pixels.as_ptr() as usize;
    let smaps = std::fs::read_to_string("/proc/self/smaps").unwrap_or_default();
    let mut inside = false;
    for line in smaps.lines() {
        if let Some((range, _)) = line.split_once(' ')
            && let Some((from, _)) = range.split_once('-')
            && let Ok(from) = usize::from_str_radix(from, 16)
        {
            inside = from == start;
            continue;
        }
        if inside && let Some(flags) = line.strip_prefix("VmFlags:") {
            return flags.trim().to_string();
        }
    }
    "?".into()
}

fn main() {
    let rounds: u32 = std::env::args().nth(1).and_then(|r| r.parse().ok()).unwrap_or(30);
    println!("{} MB buffers ({STRIDE}-byte rows x {ROWS}), {rounds} rounds, ms mean (max)", LEN / 1_000_000);
    type Make = fn() -> io::Result<OwnedFd>;
    let backings: [(&str, Make); 3] = [
        ("memfd", memfd),
        ("system", || heap_alloc("system")),
        ("system-uncached", || heap_alloc("system-uncached")),
    ];
    for (name, make) in backings {
        let fd = match make() {
            Ok(fd) => fd,
            Err(error) => {
                println!("\n{name}: unavailable ({error})");
                continue;
            }
        };
        let raw = fd.as_raw_fd();
        let pixels = match map(raw) {
            Ok(pixels) => pixels,
            Err(error) => {
                println!("\n{name}: mmap failed ({error})");
                continue;
            }
        };
        paint(pixels, 0);
        println!("\n{name}  [VmFlags: {}]", vm_flags(pixels));
        println!("  paint                         {}", time(rounds, |r| paint(pixels, r)));
        let mut sink = 0;
        println!("  read                          {}", time(rounds.min(10), |_| sink ^= read(pixels)));
        std::hint::black_box(sink);
        if name == "memfd" {
            continue;
        }
        let variants = [
            ("sync RW start+end (Moon now)", SYNC_READ | SYNC_WRITE),
            ("sync WRITE start+end", SYNC_WRITE),
        ];
        for (label, flags) in variants {
            println!(
                "  {label:<29} {}",
                time(rounds, |_| {
                    sync(raw, flags);
                    sync(raw, flags | SYNC_END);
                })
            );
            // Only the paint, each after this sync (as the client paints
            // after Moon flushed the buffer it shows).
            let (mut total, mut max) = (0.0, 0.0_f64);
            for r in 0..rounds {
                sync(raw, flags);
                sync(raw, flags | SYNC_END);
                let started = Instant::now();
                paint(pixels, r);
                let ms = started.elapsed().as_secs_f64() * 1000.0;
                total += ms;
                max = max.max(ms);
            }
            println!("    then paint                  {:6.2} ({:6.2})", total / f64::from(rounds), max);
        }
        println!("  sync WRITE end only           {}", time(rounds, |_| { sync(raw, SYNC_WRITE | SYNC_END); }));
        println!(
            "  paint inside START/END WRITE  {}",
            time(rounds, |r| {
                sync(raw, SYNC_WRITE);
                paint(pixels, r);
                sync(raw, SYNC_WRITE | SYNC_END);
            })
        );
        // Partial sync: which request number (if any) the kernel takes.
        let band = (STRIDE * 240) as u32; // a 240-row damage band
        let mut found = None;
        for nr in 2..=8 {
            let request = partial_request(nr);
            match partial(raw, request, SYNC_WRITE | SYNC_END, 0, band) {
                Ok(()) => {
                    found = Some(request);
                    println!("  partial sync: _IOW('b', {nr}, 16) works");
                    break;
                }
                Err(error) if error.raw_os_error() == Some(libc::ENOTTY) => {}
                Err(error) => println!("  partial sync: _IOW('b', {nr}, 16) -> {error}"),
            }
        }
        match found {
            Some(request) => {
                println!(
                    "  partial WRITE end, 240 rows   {}",
                    time(rounds, |_| {
                        let _ = partial(raw, request, SYNC_WRITE, 0, band);
                        let _ = partial(raw, request, SYNC_WRITE | SYNC_END, 0, band);
                    })
                );
            }
            None => println!("  partial sync: none (every request number ENOTTY)"),
        }
    }
}
