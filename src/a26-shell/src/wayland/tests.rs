//! The Wayland server against a real client, in one thread: the client
//! writes, the server dispatches, the client reads what came back.

use std::io::Write as _;
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::sync::{Arc, Mutex};

use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_keyboard, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface,
    wl_touch,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols::wp::text_input::zv3::client::{
    zwp_text_input_manager_v3, zwp_text_input_v3,
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

use super::{Event, WaylandServer};
use crate::keyboard::{KeyboardInput, KeyboardPurpose};

#[derive(Default)]
struct Client {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    wm: Option<xdg_wm_base::XdgWmBase>,
    seat: Option<wl_seat::WlSeat>,
    text: Option<zwp_text_input_manager_v3::ZwpTextInputManagerV3>,
    configures: Vec<(i32, i32, Vec<u32>)>,
    surface_configures: Vec<u32>,
    released: usize,
    touches: Vec<String>,
    keys: Vec<u32>,
    committed: Arc<Mutex<String>>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Client {
    fn event(
        c: &mut Self,
        r: &wl_registry::WlRegistry,
        e: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = e
        {
            match interface.as_str() {
                "wl_compositor" => c.compositor = Some(r.bind(name, version, qh, ())),
                "wl_shm" => c.shm = Some(r.bind(name, 1, qh, ())),
                "xdg_wm_base" => c.wm = Some(r.bind(name, version, qh, ())),
                "wl_seat" => c.seat = Some(r.bind(name, version, qh, ())),
                "zwp_text_input_manager_v3" => c.text = Some(r.bind(name, 1, qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for Client {
    fn event(
        c: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        e: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure {
            width,
            height,
            states,
        } = e
        {
            let states = states
                .chunks_exact(4)
                .map(|b| u32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
            c.configures.push((width, height, states));
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for Client {
    fn event(
        c: &mut Self,
        _: &xdg_surface::XdgSurface,
        e: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = e {
            c.surface_configures.push(serial);
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for Client {
    fn event(
        c: &mut Self,
        _: &wl_buffer::WlBuffer,
        e: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = e {
            c.released += 1;
        }
    }
}

impl Dispatch<wl_touch::WlTouch, ()> for Client {
    fn event(
        c: &mut Self,
        _: &wl_touch::WlTouch,
        e: wl_touch::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            wl_touch::Event::Down { id, x, y, .. } => c.touches.push(format!("down {id} {x} {y}")),
            wl_touch::Event::Up { id, .. } => c.touches.push(format!("up {id}")),
            wl_touch::Event::Cancel => c.touches.push("cancel".into()),
            _ => {}
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Client {
    fn event(
        c: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        e: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Key {
            key,
            state: wayland_client::WEnum::Value(wl_keyboard::KeyState::Pressed),
            ..
        } = e
        {
            c.keys.push(key);
        }
    }
}

impl Dispatch<zwp_text_input_v3::ZwpTextInputV3, ()> for Client {
    fn event(
        c: &mut Self,
        _: &zwp_text_input_v3::ZwpTextInputV3,
        e: zwp_text_input_v3::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_text_input_v3::Event::CommitString { text } = e {
            c.committed
                .lock()
                .unwrap()
                .push_str(&text.unwrap_or_default());
        }
    }
}

delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore wl_shm::WlShm);
delegate_noop!(Client: ignore wl_shm_pool::WlShmPool);
delegate_noop!(Client: ignore wl_seat::WlSeat);
delegate_noop!(Client: ignore xdg_wm_base::XdgWmBase);
delegate_noop!(Client: ignore zwp_text_input_manager_v3::ZwpTextInputManagerV3);

struct Harness {
    server: WaylandServer,
    queue: EventQueue<Client>,
    connection: Connection,
    client: Client,
    _dir: std::path::PathBuf,
}

impl Harness {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("moon-wl-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let server =
            WaylandServer::bind(&dir, "wayland-0", (1080, 2340), (1080, 2216), 120_000, 1).unwrap();
        let stream = std::os::unix::net::UnixStream::connect(dir.join("wayland-0")).unwrap();
        let connection = Connection::from_socket(stream).unwrap();
        let queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        let mut harness = Self {
            server,
            queue,
            connection,
            client: Client::default(),
            _dir: dir,
        };
        harness.pump();
        harness
    }

    /// Client requests to the server and the server's answers back.
    fn pump(&mut self) {
        for _ in 0..3 {
            self.connection.flush().unwrap();
            self.server.dispatch();
            if let Some(guard) = self.connection.prepare_read() {
                let fd = guard.connection_fd().as_fd().try_clone_to_owned().unwrap();
                let mut poll = libc::pollfd {
                    fd: std::os::fd::AsRawFd::as_raw_fd(&fd),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one pollfd.
                if unsafe { libc::poll(&mut poll, 1, 20) } > 0 {
                    guard.read().unwrap();
                }
            }
            self.queue.dispatch_pending(&mut self.client).unwrap();
        }
    }
}

fn memfd(size: usize) -> OwnedFd {
    let name = std::ffi::CString::new("test").unwrap();
    // SAFETY: a fresh memfd.
    let fd = unsafe { OwnedFd::from_raw_fd(libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC)) };
    let mut file = std::fs::File::from(fd);
    file.write_all(&vec![0u8; size]).unwrap();
    OwnedFd::from(file)
}

#[test]
fn toplevel_configure_commit_release_touch_and_text() {
    let mut h = Harness::new("toplevel");
    let qh = h.queue.handle();
    let surface = h
        .client
        .compositor
        .as_ref()
        .unwrap()
        .create_surface(&qh, ());
    let xdg = h
        .client
        .wm
        .as_ref()
        .unwrap()
        .get_xdg_surface(&surface, &qh, ());
    let _toplevel = xdg.get_toplevel(&qh, ());
    surface.commit();
    h.pump();
    // The first commit gets the app area, fullscreen.
    assert_eq!(
        h.client.configures.first().map(|c| (c.0, c.1)),
        Some((1080, 2216))
    );
    assert!(h.client.configures[0].2.contains(&2));
    let events = h.server.take_events();
    let surface_key = match events.as_slice() {
        [Event::ToplevelCreated { surface, pid, .. }] => {
            assert_eq!(*pid, Some(std::process::id()));
            *surface
        }
        other => panic!("unexpected {other:?}"),
    };
    let serial = *h.client.surface_configures.last().unwrap();
    xdg.ack_configure(serial);

    // A buffer: the first commit shows all of it, and the buffer comes back
    // when Moon says the copy is done.
    let (w, h_px) = (1080, 2216);
    let fd = memfd((w * h_px * 4) as usize);
    let pool = h
        .client
        .shm
        .as_ref()
        .unwrap()
        .create_pool(fd.as_fd(), w * h_px * 4, &qh, ());
    let buffer = pool.create_buffer(0, w, h_px, w * 4, wl_shm::Format::Xrgb8888, &qh, ());
    surface.attach(Some(&buffer), 0, 0);
    surface.damage_buffer(0, 0, 10, 10);
    surface.commit();
    h.pump();
    let resource = match h.server.take_events().pop() {
        Some(Event::Commit {
            surface,
            resource,
            damage,
            ..
        }) => {
            assert_eq!(surface, surface_key);
            assert_eq!(damage.len(), 1);
            assert_eq!((damage[0].width, damage[0].height), (w, h_px));
            resource
        }
        other => panic!("expected a commit, got {other:?}"),
    };
    h.server.release(&resource);
    h.pump();
    assert_eq!(h.client.released, 1);

    // Touch and keyboard need the seat; text input follows keyboard focus.
    let seat = h.client.seat.clone().unwrap();
    let _touch = seat.get_touch(&qh, ());
    let _keyboard = seat.get_keyboard(&qh, ());
    let text = h
        .client
        .text
        .as_ref()
        .unwrap()
        .get_text_input(&seat, &qh, ());
    h.pump();
    let id = *h.server.state.toplevels.keys().next().unwrap();
    h.server.set_keyboard_focus(Some(id));
    h.server.touch_down(surface_key, 0, 10.0, 20.0, 1);
    h.server.touch_up(0, 2);
    h.pump();
    assert_eq!(
        h.client.touches,
        vec!["down 0 10 20".to_string(), "up 0".to_string()]
    );

    text.enable();
    text.set_content_type(
        zwp_text_input_v3::ContentHint::None,
        zwp_text_input_v3::ContentPurpose::Password,
    );
    text.commit();
    h.pump();
    match h.server.take_events().as_slice() {
        [
            Event::TextInput {
                enabled: true,
                purpose: KeyboardPurpose::Password,
                ..
            },
        ] => {}
        other => panic!("expected text input on, got {other:?}"),
    }
    assert!(h.server.key_input(KeyboardInput::Character('x'), 3));
    // Enter is a key, not text.
    assert!(h.server.key_input(KeyboardInput::Enter, 4));
    h.pump();
    assert_eq!(*h.client.committed.lock().unwrap(), "x");
    assert_eq!(h.client.keys, vec![28]);
}
