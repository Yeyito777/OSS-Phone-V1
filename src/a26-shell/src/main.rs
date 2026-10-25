mod apps;
mod audio;
mod config;
mod font;
mod freezer;
mod input;
mod ipc;
mod keyboard;
mod lease;
mod memory;
mod model;
mod status;
mod status_bar;
mod suspend;
mod ui;
mod volume;

use std::env;
use std::error::Error;
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::path::Path;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use apps::{AppRegistry, RegistryUpdate, WindowVisibility};
use audio::AudioVolume;
use config::Config;
use input::{Backlight, PowerKey, TouchscreenPower, VolumeKey, VolumeKeys};
use ipc::{Command, IpcServer};
use keyboard::{KeyboardEffect, KeyboardGeometry, KeyboardSurface, XtestInjector};
use model::{AppId, PointerGesture, ShellState, View};
use status_bar::StatusBarSurface;
use suspend::{PersistedSuspendState, PlatformSuspend, SuspendAction, SuspendCoordinator};
use ui::{KeypadAction, Renderer};
use volume::VolumeSurface;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xfixes::ConnectionExt as _;
use x11rb::protocol::xinput::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    AtomEnum, BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, Blanking, ChangeWindowAttributesAux,
    ConfigureWindowAux, ConnectionExt as _, CreateGCAux, CreateWindowAux, EventMask, Exposures,
    InputFocus, MOTION_NOTIFY_EVENT, NotifyDetail, PropMode, StackMode, WindowClass,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, CURRENT_TIME};

const DEVICE_STATUS_INTERVAL: Duration = Duration::from_secs(5);
const MEDIA_ACTIVITY_INTERVAL: Duration = Duration::from_millis(250);
const MEDIA_ACTIVITY_MAX_AGE: Duration = Duration::from_secs(2);
const MEMORY_STATUS_INTERVAL: Duration = Duration::from_secs(5);
const FALLBACK_PROCESS_STATUS_INTERVAL: Duration = Duration::from_millis(250);
const LAUNCH_ANIMATION_INTERVAL: Duration = Duration::from_millis(180);
const MAX_REPEAT_CATCH_UP: usize = 8;
const STALE_POINTER_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy)]
struct KeyRepeatTiming {
    delay: Duration,
    interval: Duration,
}

impl KeyRepeatTiming {
    fn from_config(config: &Config) -> Self {
        Self {
            delay: Duration::from_millis(config.keyboard_repeat_delay_ms),
            interval: Duration::from_nanos(
                1_000_000_000_u64 / u64::from(config.keyboard_repeat_rate_hz),
            ),
        }
    }
}

#[derive(Default)]
struct RawTouchTracker {
    touch_id: Option<u32>,
    x: i16,
    y: i16,
    keyboard_contacts: Vec<(u32, PointerGesture)>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let config_path = parse_config_path()?;
    let config = Config::load(&config_path)?;
    let key_repeat_timing = KeyRepeatTiming::from_config(&config);
    let (conn, screen_number) = RustConnection::connect(None)?;
    let screen = &conn.setup().roots[screen_number];
    let root = screen.root;
    let width = screen.width_in_pixels;
    let height = screen.height_in_pixels;

    let root_mask = EventMask::SUBSTRUCTURE_REDIRECT
        | EventMask::SUBSTRUCTURE_NOTIFY
        | EventMask::STRUCTURE_NOTIFY
        | EventMask::PROPERTY_CHANGE;
    conn.change_window_attributes(
        root,
        &ChangeWindowAttributesAux::new().event_mask(root_mask),
    )?
    .check()
    .map_err(|error| format!("cannot become window manager (is another WM running?): {error}"))?;

    let xi_version = conn.xinput_xi_query_version(2, 2)?.reply()?;
    if (xi_version.major_version, xi_version.minor_version) < (2, 2) {
        return Err(format!(
            "XInput 2.2 required; server negotiated {}.{}",
            xi_version.major_version, xi_version.minor_version
        )
        .into());
    }
    conn.xfixes_query_version(4, 0)?.reply()?;
    let key_injector = XtestInjector::query(
        &conn,
        root,
        conn.setup().min_keycode,
        conn.setup().max_keycode,
    )?;
    // A direct PMIC power-key read is invisible to the X server. Disable Xorg's
    // independent blanking/DPMS timers so it cannot blank while our shell still
    // believes the screen is awake (volume keys previously appeared to be the
    // only reliable wake because they happened to be X events).
    conn.set_screen_saver(0, 0, Blanking::DEFAULT, Exposures::DEFAULT)?
        .check()?;
    let raw_touch_mask = xinput::XIEventMask::RAW_TOUCH_BEGIN
        | xinput::XIEventMask::RAW_TOUCH_UPDATE
        | xinput::XIEventMask::RAW_TOUCH_END;
    conn.xinput_xi_select_events(
        root,
        &[xinput::EventMask {
            deviceid: xinput::Device::ALL.into(),
            mask: vec![raw_touch_mask],
        }],
    )?
    .check()?;

    let shell_window = conn.generate_id()?;
    let window_aux = CreateWindowAux::new()
        .background_pixel(0x0b1020)
        .override_redirect(1)
        .event_mask(
            EventMask::EXPOSURE
                | EventMask::BUTTON_PRESS
                | EventMask::BUTTON_RELEASE
                | EventMask::POINTER_MOTION
                | EventMask::KEY_PRESS
                | EventMask::STRUCTURE_NOTIFY,
        );
    conn.create_window(
        COPY_DEPTH_FROM_PARENT,
        shell_window,
        root,
        0,
        0,
        width,
        height,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &window_aux,
    )?;
    conn.change_property8(
        PropMode::REPLACE,
        root,
        AtomEnum::WM_NAME,
        AtomEnum::STRING,
        b"moon",
    )?;
    conn.change_property8(
        PropMode::REPLACE,
        shell_window,
        AtomEnum::WM_NAME,
        AtomEnum::STRING,
        b"moon-shell",
    )?;

    let shell_back_buffer = conn.generate_id()?;
    conn.create_pixmap(
        screen.root_depth,
        shell_back_buffer,
        shell_window,
        width,
        height,
    )?;
    let gc = conn.generate_id()?;
    conn.create_gc(gc, shell_window, &CreateGCAux::new().graphics_exposures(0))?;
    let keyboard_geometry = KeyboardGeometry::new(width, height);
    let mut keyboard_surface =
        KeyboardSurface::create(&conn, root, screen.root_depth, keyboard_geometry)?;
    let mut volume_surface = VolumeSurface::create(&conn, root, screen.root_depth)?;
    let mut status_bar_surface = StatusBarSurface::create(&conn, root, screen.root_depth, width)?;
    conn.map_window(shell_window)?;
    conn.xfixes_hide_cursor(shell_window)?.check()?;
    conn.xfixes_hide_cursor(keyboard_surface.window)?.check()?;
    conn.xfixes_hide_cursor(volume_surface.window)?.check()?;
    conn.xfixes_hide_cursor(status_bar_surface.window)?
        .check()?;
    raise_shell(&conn, shell_window)?;
    conn.set_input_focus(InputFocus::PARENT, shell_window, CURRENT_TIME)?;
    conn.flush()?;

    let renderer = Renderer {
        window: shell_window,
        back_buffer: shell_back_buffer,
        gc,
        width,
        height,
        system_icon: ui::load_system_icon(),
        browser_icon: ui::load_browser_icon(),
    };
    let ipc = IpcServer::bind(&config.socket_path)?;
    let audio_volume = match AudioVolume::open("/run/moon-audio/volume") {
        Ok(control) => Some(control),
        Err(error) => {
            eprintln!("audio volume control unavailable: {error}");
            None
        }
    };
    let initial_volume = audio_volume
        .as_ref()
        .and_then(|control| match control.get() {
            Ok(volume) => Some(volume),
            Err(error) => {
                eprintln!("persisted audio volume unavailable: {error}");
                None
            }
        })
        .unwrap_or(config.initial_volume);
    let mut state = ShellState::new(config.start_locked, initial_volume);
    let mut apps = AppRegistry::from_environment();
    if let Some(control) = audio_volume.as_ref()
        && let Err(error) = control.set(state.volume)
    {
        eprintln!("initial audio volume sync failed: {error}");
    }
    let initial_status = status::DeviceStatus::read();
    state.update_device_status(
        initial_status.battery_percent,
        initial_status.battery_charging,
        initial_status.wifi_connected,
    );
    let mut next_device_status = Instant::now() + DEVICE_STATUS_INTERVAL;
    let mut next_media_activity = Instant::now();
    let mut next_memory_status = Instant::now() + MEMORY_STATUS_INTERVAL;
    let mut next_launch_animation = Instant::now();
    let mut power_key = match PowerKey::open("/dev/input/event1") {
        Ok(device) => {
            eprintln!("physical power key ready at /dev/input/event1");
            Some(device)
        }
        Err(error) => {
            eprintln!("physical power key unavailable: {error}");
            None
        }
    };
    let mut volume_keys = match VolumeKeys::open("/dev/input/event0") {
        Ok(device) => {
            eprintln!("physical volume keys ready at /dev/input/event0");
            Some(device)
        }
        Err(error) => {
            eprintln!("physical volume keys unavailable: {error}");
            None
        }
    };
    let mut backlight = match Backlight::open("/sys/class/backlight/panel/brightness") {
        Ok(device) => Some(device),
        Err(error) => {
            eprintln!("panel backlight control unavailable: {error}");
            None
        }
    };
    let touchscreen = match TouchscreenPower::open("/sys/class/sec/tsp/enabled") {
        Ok(device) => Some(device),
        Err(error) => {
            eprintln!("touchscreen power control unavailable: {error}");
            None
        }
    };
    let (platform_suspend, persisted_suspend) = match PlatformSuspend::open() {
        Ok((_, persisted)) if !config.deep_suspend => {
            eprintln!("deep suspend disabled by config; screen-off keeps Moon resident");
            (None, persisted)
        }
        Ok((port, persisted)) => {
            eprintln!("deep suspend handoff ready (DPU quiesce, mem/deep, power+RTC wake)");
            (Some(port), persisted)
        }
        Err(error) => {
            eprintln!("deep suspend handoff unavailable: {error}");
            (None, PersistedSuspendState::default())
        }
    };
    let mut suspend_coordinator =
        SuspendCoordinator::new(true, platform_suspend.is_some(), persisted_suspend);
    let mut raw_touch = RawTouchTracker::default();
    let mut app_viewport: Option<(u32, u16)> = None;
    let mut shell_inset = false;
    renderer.render(&conn, &state)?;
    if let Some(device) = touchscreen.as_ref()
        && let Err(error) = device.on()
    {
        eprintln!("initial touchscreen wake failed: {error}");
    }
    if let Some(device) = backlight.as_ref() {
        if let Err(error) = device.on() {
            eprintln!("initial panel wake failed: {error}");
        }
    }

    eprintln!(
        "a26-shell ready display={} root=0x{root:08x} window=0x{shell_window:08x} size={width}x{height} socket={} pid={}",
        env::var("DISPLAY").unwrap_or_else(|_| "(default)".into()),
        config.socket_path.display(),
        std::process::id(),
    );

    while !state.should_exit {
        while let Some(event) = conn.poll_for_event()? {
            handle_x_event(
                &conn,
                event,
                root,
                shell_window,
                keyboard_surface.window,
                volume_surface.window,
                status_bar_surface.window,
                (width, height),
                &keyboard_geometry,
                &config,
                &mut state,
                &mut apps,
                &mut raw_touch,
                &key_injector,
                &mut volume_surface,
                &mut status_bar_surface,
                key_repeat_timing,
            )?;
        }

        cancel_stale_pointer(&mut state, &mut raw_touch, Instant::now());
        if !state.keyboard.is_visible() {
            raw_touch.keyboard_contacts.clear();
        }

        if let Some(device) = power_key.as_mut() {
            match device.poll_presses() {
                Ok(count) => {
                    if count > 0 {
                        state.toggle_screen();
                    }
                }
                Err(error) => {
                    eprintln!("physical power key failed: {error}");
                    power_key = None;
                }
            }
        }

        if let Some(device) = volume_keys.as_mut() {
            match device.poll() {
                Ok(keys) => {
                    for key in keys {
                        change_volume(
                            &mut state,
                            audio_volume.as_ref(),
                            match key {
                                VolumeKey::Down => -5,
                                VolumeKey::Up => 5,
                            },
                        );
                    }
                }
                Err(error) => {
                    eprintln!("physical volume keys failed: {error}");
                    volume_keys = None;
                }
            }
        }

        for (stream, request) in ipc.accept_all() {
            match request {
                Ok(command) => {
                    let lease_result = match &command {
                        Command::LeaseAcquire(app, kind, seconds) => apps
                            .acquire_lease(*app, *kind, *seconds, Instant::now())
                            .map(|_| ()),
                        Command::LeaseRelease(app, kind) => {
                            apps.release_lease(*app, *kind, Instant::now())
                        }
                        Command::SimulateMemoryPressure => {
                            if let Some(app) = apps.evict_lru_background(Instant::now()) {
                                state.note_memory_eviction(app);
                            }
                            Ok(())
                        }
                        Command::SuspendTest(seconds) => {
                            if platform_suspend.is_none() {
                                Err("deep suspend handoff unavailable")
                            } else {
                                suspend_coordinator.arm_test_wake(Duration::from_secs(*seconds));
                                state.screen_off();
                                Ok(())
                            }
                        }
                        _ => Ok(()),
                    };
                    if let Err(error) = lease_result {
                        ipc::respond::<model::PublicState>(stream, Err(error));
                        continue;
                    }
                    apply_command(
                        &conn,
                        root,
                        command,
                        width,
                        height,
                        &keyboard_geometry,
                        &config,
                        &mut state,
                        &key_injector,
                        audio_volume.as_ref(),
                        key_repeat_timing,
                    );
                    let public =
                        state.public(width, height, apps.public(), suspend_coordinator.public());
                    ipc::respond(stream, Ok(&public));
                }
                Err(error) => ipc::respond::<model::PublicState>(stream, Err(&error)),
            }
        }

        repeat_keyboard_if_due(
            &conn,
            &mut state,
            &mut raw_touch,
            &keyboard_geometry,
            &key_injector,
            key_repeat_timing,
        );

        let now = Instant::now();
        if apps.browser_running() && now >= next_media_activity {
            if heartbeat_is_recent(
                Path::new("/run/moon-audio/browser-media-active"),
                MEDIA_ACTIVITY_MAX_AGE,
            ) {
                apps.renew_browser_media_activity(now);
            }
            next_media_activity = now + MEDIA_ACTIVITY_INTERVAL;
        } else if !apps.browser_running() {
            // Make the first check immediate after the Browser starts, without
            // polling a nonexistent heartbeat while Moon is idle.
            next_media_activity = now;
        }
        if now >= next_memory_status {
            match memory::MemorySnapshot::read() {
                Ok(snapshot) if snapshot.under_pressure() => {
                    if let Some(app) = apps.evict_lru_background(now) {
                        eprintln!(
                            "evicted {} under memory pressure available={} total={}",
                            app.display_name(),
                            snapshot.available_bytes,
                            snapshot.total_bytes
                        );
                        state.note_memory_eviction(app);
                    }
                }
                Ok(_) => {}
                Err(error) => eprintln!("memory status unavailable: {error}"),
            }
            next_memory_status = now + MEMORY_STATUS_INTERVAL;
        }

        let desired_app = AppId::from_view(state.view);
        let update = apps.reconcile(desired_app);
        apply_registry_update(
            &conn,
            shell_window,
            width,
            height,
            &mut state,
            &mut apps,
            update,
        )?;
        let desired_shell_inset = state.view.is_app();
        if desired_shell_inset != shell_inset {
            resize_shell_window(&conn, shell_window, width, height, desired_shell_inset)?;
            shell_inset = desired_shell_inset;
        }
        state.replace_managed_windows(apps.active_windows());
        if state.app_ready_to_reveal() {
            let app_window = apps.primary_active_window();
            state.finish_app_launch();
            if let Some(window) = app_window {
                fullscreen_window(&conn, window, width, height)?;
                conn.set_input_focus(InputFocus::PARENT, window, CURRENT_TIME)?;
            }
        }
        let keyboard_app_window =
            if state.screen_awake && state.view.is_app() && !state.app_launching() {
                state.managed_windows.first().copied()
            } else {
                None
            };
        let desired_viewport = keyboard_app_window.map(|window| {
            (
                window,
                if state.keyboard.is_visible() {
                    keyboard_geometry.app_height()
                } else {
                    height
                },
            )
        });
        if desired_viewport != app_viewport {
            if let Some((window, app_height)) = desired_viewport {
                resize_app_window(&conn, window, width, app_height)?;
                status_bar_surface.request_raise();
                if state.keyboard.is_visible() {
                    state.keyboard.request_raise();
                }
            }
            app_viewport = desired_viewport;
        }
        keyboard_surface.sync(&conn, &mut state.keyboard, keyboard_app_window)?;
        let volume_visible = state.screen_awake
            && state.view.is_app()
            && !state.app_launching()
            && state
                .volume_overlay_until
                .is_some_and(|deadline| deadline > Instant::now());
        volume_surface.sync(&conn, volume_visible, state.volume)?;
        if Instant::now() >= next_device_status {
            let device_status = status::DeviceStatus::read();
            state.update_device_status(
                device_status.battery_percent,
                device_status.battery_charging,
                device_status.wifi_connected,
            );
            next_device_status = Instant::now() + DEVICE_STATUS_INTERVAL;
        }
        if state.app_launching() && Instant::now() >= next_launch_animation {
            state.redraw = true;
            next_launch_animation = Instant::now() + LAUNCH_ANIMATION_INTERVAL;
        }
        state.tick();
        if state.redraw {
            if !state.view.is_app() || state.app_launching() {
                raise_shell(&conn, shell_window)?;
                renderer.render(&conn, &state)?;
                if state.app_launching() {
                    status_bar_surface.request_raise();
                }
            }
            state.redraw = false;
        }
        // Present the status surface after the loading shell. Besides keeping
        // the cutout strip out of the app viewport, this gives the bar the last
        // stacking request on every animated launch frame and prevents the two
        // override-redirect surfaces from alternately covering one another.
        let status_bar_visible = state.screen_awake && state.view.is_app();
        status_bar_surface.sync(
            &conn,
            status_bar_visible,
            state.wifi_connected,
            state.battery_percent,
            state.battery_charging,
        )?;
        // External-app MapRequest/configure operations may be the only X11
        // traffic in this state, so they cannot rely on a shell repaint to
        // flush the connection.
        conn.flush()?;
        match suspend_coordinator.next_action(state.screen_awake) {
            SuspendAction::None => {}
            SuspendAction::Sleep => {
                if !safe_to_power_down(&state, &apps) {
                    eprintln!("screen-off transaction rejected: unsafe shell state");
                    state.screen_on();
                    suspend_coordinator.fail_sleep("unsafe_shell_state");
                    raise_shell(&conn, shell_window)?;
                    renderer.render(&conn, &state)?;
                    state.redraw = false;
                    conn.flush()?;
                } else {
                    // The lock frame and every app unmap are already flushed.
                    // Only then may device-local hardware be powered down.
                    let result = power_down_display(&mut backlight, touchscreen.as_ref());
                    match result {
                        Ok(()) => suspend_coordinator.complete_sleep(),
                        Err(code) => {
                            eprintln!("screen-off transaction failed: {code}");
                            if let Some(device) = touchscreen.as_ref() {
                                let _ = device.on();
                            }
                            if let Some(device) = backlight.as_ref() {
                                let _ = device.on();
                            }
                            state.screen_on();
                            suspend_coordinator.fail_sleep(code);
                            raise_shell(&conn, shell_window)?;
                            renderer.render(&conn, &state)?;
                            state.redraw = false;
                            conn.flush()?;
                        }
                    }
                }
            }
            SuspendAction::Wake => {
                // Waking is also transactional: the complete lock frame is
                // presented while the panel is dark, before either input or
                // brightness is restored.
                raise_shell(&conn, shell_window)?;
                renderer.render(&conn, &state)?;
                state.redraw = false;
                conn.flush()?;
                match power_up_display(backlight.as_ref(), touchscreen.as_ref()) {
                    Ok(()) => suspend_coordinator.complete_wake(),
                    Err(code) => {
                        eprintln!("screen-on transaction failed: {code}");
                        suspend_coordinator.fail_wake(code);
                    }
                }
            }
        }
        let now = Instant::now();
        if suspend_coordinator.screen_is_off() {
            let inhibited = apps.has_active_leases(now);
            suspend_coordinator.set_deep_inhibited(inhibited);
            if !inhibited && let Some(port) = platform_suspend.as_ref() {
                let test_wake = suspend_coordinator.take_test_wake();
                match port.request(test_wake) {
                    Ok(()) => {
                        suspend_coordinator.begin_deep_suspend();
                        eprintln!(
                            "deep suspend handed to device supervisor{}",
                            test_wake
                                .map(|after| format!(" with {}s RTC proof wake", after.as_secs()))
                                .unwrap_or_default()
                        );
                        // The helper is now an independent Android-namespace
                        // session. Ending this X client lets it terminate every
                        // old app, quiesce DSI/DPU, suspend, and replace Xorg.
                        state.should_exit = true;
                    }
                    Err(error) => {
                        eprintln!("deep suspend handoff failed: {error}");
                        state.screen_on();
                        let _ = power_up_display(backlight.as_ref(), touchscreen.as_ref());
                        suspend_coordinator.fail_wake("deep_suspend_handoff_failed");
                    }
                }
            }
        }
        let mut deadline = next_device_status.min(next_memory_status);
        if apps.browser_running() {
            deadline = deadline.min(next_media_activity);
        }
        if state.app_launching() {
            deadline = deadline.min(next_launch_animation);
        }
        if let Some(state_deadline) = state.next_deadline(now) {
            deadline = deadline.min(state_deadline);
        }
        if let Some(lease_deadline) = apps.next_lease_deadline(now) {
            deadline = deadline.min(lease_deadline);
        }
        if let Some(input_deadline) = next_input_deadline(&state, &raw_touch) {
            deadline = deadline.min(input_deadline);
        }
        if apps.needs_process_poll() {
            deadline = deadline.min(now + FALLBACK_PROCESS_STATUS_INTERVAL);
        }
        let app_fds = apps.poll_fds();
        wait_for_activity(
            &conn,
            &ipc,
            power_key.as_ref(),
            volume_keys.as_ref(),
            &app_fds,
            deadline.saturating_duration_since(now),
        )?;
    }

    // A normal development restart must never strand the device with its
    // backlight at zero. Wake only to the freshly rendered lock screen.
    if !state.screen_awake {
        state.screen_on();
        let _ = renderer.render(&conn, &state);
        if let Some(device) = touchscreen.as_ref() {
            let _ = device.on();
        }
        if let Some(device) = backlight.as_ref() {
            let _ = device.on();
        }
    }
    apps.shutdown();
    let _ = conn.destroy_window(shell_window);
    keyboard_surface.destroy(&conn);
    volume_surface.destroy(&conn);
    status_bar_surface.destroy(&conn);
    let _ = conn.free_pixmap(shell_back_buffer);
    let _ = conn.free_gc(gc);
    let _ = conn.flush();
    eprintln!("a26-shell stopping");
    Ok(())
}

fn parse_config_path() -> Result<PathBuf, Box<dyn Error>> {
    let mut arguments = env::args().skip(1);
    let mut config = env::var_os("A26_SHELL_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/etc/a26-shell/config.json".into());
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--config" => config = arguments.next().ok_or("--config requires a path")?.into(),
            "--help" | "-h" => {
                println!("usage: a26-shell [--config PATH]");
                std::process::exit(0);
            }
            _ => return Err(format!("unknown argument: {argument}").into()),
        }
    }
    Ok(config)
}

fn heartbeat_is_recent(path: &Path, maximum_age: Duration) -> bool {
    let Ok(modified) = std::fs::metadata(path).and_then(|metadata| metadata.modified()) else {
        return false;
    };
    match std::time::SystemTime::now().duration_since(modified) {
        Ok(age) => age <= maximum_age,
        // A small wall-clock correction into the future must not interrupt an
        // already-playing stream; the bounded lease still expires without new
        // writes.
        Err(_) => true,
    }
}

fn safe_to_power_down(state: &ShellState, apps: &AppRegistry) -> bool {
    state.view == View::Locked
        && !state.screen_awake
        && !state.keyboard.is_visible()
        && state.pointer.is_none()
        && state.managed_windows.is_empty()
        && apps.active().is_none()
}

fn power_down_display(
    backlight: &mut Option<Backlight>,
    touchscreen: Option<&TouchscreenPower>,
) -> Result<(), &'static str> {
    let Some(backlight) = backlight.as_mut() else {
        return Err("backlight_unavailable");
    };
    backlight.off().map_err(|_| "backlight_sleep_failed")?;
    let Some(touchscreen) = touchscreen else {
        return Err("touchscreen_unavailable");
    };
    touchscreen.off().map_err(|_| "touchscreen_sleep_failed")
}

fn power_up_display(
    backlight: Option<&Backlight>,
    touchscreen: Option<&TouchscreenPower>,
) -> Result<(), &'static str> {
    let Some(touchscreen) = touchscreen else {
        return Err("touchscreen_unavailable");
    };
    touchscreen.on().map_err(|_| "touchscreen_wake_failed")?;
    let Some(backlight) = backlight else {
        return Err("backlight_unavailable");
    };
    backlight.on().map_err(|_| "backlight_wake_failed")
}

fn next_input_deadline(state: &ShellState, raw_touch: &RawTouchTracker) -> Option<Instant> {
    state
        .pointer
        .iter()
        .chain(
            raw_touch
                .keyboard_contacts
                .iter()
                .map(|(_, pointer)| pointer),
        )
        .flat_map(|pointer| {
            [
                pointer.keyboard_next_repeat_at,
                pointer.started.checked_add(STALE_POINTER_TIMEOUT),
            ]
        })
        .flatten()
        .min()
}

fn wait_for_activity(
    conn: &RustConnection,
    ipc: &IpcServer,
    power_key: Option<&PowerKey>,
    volume_keys: Option<&VolumeKeys>,
    app_fds: &[RawFd],
    timeout: Duration,
) -> io::Result<()> {
    let mut descriptors = Vec::with_capacity(4 + app_fds.len());
    let mut add = |fd: RawFd| {
        descriptors.push(libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        });
    };
    add(conn.stream().as_raw_fd());
    add(ipc.raw_fd());
    if let Some(device) = power_key {
        add(device.raw_fd());
    }
    if let Some(device) = volume_keys {
        add(device.raw_fd());
    }
    for fd in app_fds {
        add(*fd);
    }

    let timeout_ms = duration_to_poll_timeout(timeout);
    // SAFETY: descriptors points to initialized pollfd values for its full
    // length and remains alive for the duration of poll.
    let result = unsafe {
        libc::poll(
            descriptors.as_mut_ptr(),
            descriptors.len() as libc::nfds_t,
            timeout_ms,
        )
    };
    if result < 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    Ok(())
}

fn duration_to_poll_timeout(timeout: Duration) -> i32 {
    let milliseconds = timeout.as_millis();
    let rounded_up = milliseconds + u128::from(timeout.subsec_nanos() % 1_000_000 != 0);
    rounded_up.min(i32::MAX as u128) as i32
}

#[allow(clippy::too_many_arguments)]
fn handle_x_event(
    conn: &RustConnection,
    event: Event,
    root: u32,
    shell_window: u32,
    keyboard_window: u32,
    volume_window: u32,
    status_bar_window: u32,
    dimensions: (u16, u16),
    keyboard_geometry: &KeyboardGeometry,
    config: &Config,
    state: &mut ShellState,
    apps: &mut AppRegistry,
    raw_touch: &mut RawTouchTracker,
    key_injector: &XtestInjector,
    volume_surface: &mut VolumeSurface,
    status_bar_surface: &mut StatusBarSurface,
    key_repeat_timing: KeyRepeatTiming,
) -> Result<(), Box<dyn Error>> {
    let (width, height) = dimensions;
    match event {
        Event::XinputRawTouchBegin(event) => {
            let x = raw_axis(&event, 0, width.saturating_sub(1));
            let y = raw_axis(&event, 1, height.saturating_sub(1));
            if let (Some(x), Some(y)) = (x, y) {
                if keyboard_geometry.owns_touch(&state.keyboard, x, y) {
                    if let Some(action) = begin_raw_keyboard_contact(
                        state,
                        raw_touch,
                        event.detail,
                        keyboard_geometry,
                        x,
                        y,
                        key_repeat_timing,
                    ) {
                        handle_keyboard_action(conn, state, action, key_injector);
                    }
                } else if raw_touch.touch_id.is_none() && state.pointer.is_none() {
                    // Non-keyboard app/close gestures remain deliberately
                    // single-finger even while the keyboard supports rollover.
                    raw_touch.touch_id = Some(event.detail);
                    raw_touch.x = x;
                    raw_touch.y = y;
                    if let Some(action) =
                        pointer_begin(state, keyboard_geometry, x, y, key_repeat_timing)
                    {
                        handle_keyboard_action(conn, state, action, key_injector);
                    }
                }
            }
        }
        Event::XinputRawTouchUpdate(event) => {
            if raw_touch
                .keyboard_contacts
                .iter()
                .any(|(touch_id, _)| *touch_id == event.detail)
            {
                update_raw_keyboard_contact(
                    state,
                    raw_touch,
                    event.detail,
                    keyboard_geometry,
                    raw_axis(&event, 0, width.saturating_sub(1)),
                    raw_axis(&event, 1, height.saturating_sub(1)),
                );
            } else if raw_touch.touch_id == Some(event.detail) {
                if let Some(x) = raw_axis(&event, 0, width.saturating_sub(1)) {
                    raw_touch.x = x;
                }
                if let Some(y) = raw_axis(&event, 1, height.saturating_sub(1)) {
                    raw_touch.y = y;
                }
                pointer_move(state, keyboard_geometry, raw_touch.x, raw_touch.y);
            }
        }
        Event::XinputRawTouchEnd(event) => {
            if raw_touch
                .keyboard_contacts
                .iter()
                .any(|(touch_id, _)| *touch_id == event.detail)
            {
                end_raw_keyboard_contact(
                    conn,
                    state,
                    raw_touch,
                    event.detail,
                    keyboard_geometry,
                    key_injector,
                );
            } else if raw_touch.touch_id == Some(event.detail) {
                pointer_end(
                    conn,
                    root,
                    state,
                    raw_touch.x,
                    raw_touch.y,
                    width,
                    height,
                    keyboard_geometry,
                    config,
                    key_injector,
                );
                raw_touch.touch_id = None;
            }
        }
        Event::Expose(event) if event.window == shell_window => state.redraw = true,
        Event::Expose(event) if event.window == keyboard_window => {
            state.keyboard.request_redraw();
        }
        Event::Expose(event) if event.window == volume_window => {
            volume_surface.request_redraw();
        }
        Event::Expose(event) if event.window == status_bar_window => {
            status_bar_surface.request_redraw();
        }
        Event::ButtonPress(event) if event.event == shell_window => {
            if state.pointer.is_none() && raw_touch.keyboard_contacts.is_empty() {
                if let Some(action) = pointer_begin(
                    state,
                    keyboard_geometry,
                    event.event_x,
                    event.event_y,
                    key_repeat_timing,
                ) {
                    handle_keyboard_action(conn, state, action, key_injector);
                }
            }
        }
        Event::MotionNotify(event) if event.event == shell_window => {
            pointer_move(state, keyboard_geometry, event.event_x, event.event_y);
        }
        Event::ButtonRelease(event) if event.event == shell_window => {
            pointer_end(
                conn,
                root,
                state,
                event.event_x,
                event.event_y,
                width,
                height,
                keyboard_geometry,
                config,
                key_injector,
            );
        }
        Event::MapRequest(event) => {
            if event.window != shell_window
                && event.window != keyboard_window
                && event.window != volume_window
                && event.window != status_bar_window
            {
                // A MapRequest is handled inside the current app lifecycle. Do
                // not synchronously query WM_TRANSIENT_FOR on Moon's long-lived
                // X connection: any runtime reply wait can hit the same wrapped
                // sequence ambiguity that previously froze keyboard input.
                // Known windows retain their owner; new primary/popup windows
                // belong to the app that Moon deliberately launched or resumed.
                let owner = apps.owner_of(event.window).or_else(|| apps.active());
                let registration = owner.map(|owner| apps.register_window(owner, event.window));
                let should_show = registration
                    .as_ref()
                    .is_none_or(|registration| registration.should_show);
                if registration.is_some() {
                    // Focus events on the managed top-level also report focus
                    // entering/leaving its descendants. This preserves CEF's
                    // exact page/textfield focus without a synchronous query.
                    let _ = conn.change_window_attributes(
                        event.window,
                        &ChangeWindowAttributesAux::new().event_mask(EventMask::FOCUS_CHANGE),
                    )?;
                }
                if should_show {
                    conn.map_window(event.window)?;
                    let _ = conn.xfixes_hide_cursor(event.window)?;
                }
                state.replace_managed_windows(apps.active_windows());
                let is_active = registration
                    .as_ref()
                    .is_some_and(|registration| apps.active() == Some(registration.owner));
                let is_primary = registration
                    .as_ref()
                    .is_some_and(|registration| registration.primary);
                if is_active {
                    state.last_action = "map_external_window".into();
                    state.redraw = true;
                    let app_height = if is_primary && state.keyboard.is_visible() {
                        keyboard_geometry.app_height()
                    } else {
                        height
                    };
                    fullscreen_window(conn, event.window, width, app_height)?;
                    if state.app_launching() {
                        if is_primary {
                            state.note_app_window_mapped();
                        }
                        raise_shell(conn, shell_window)?;
                    } else if is_primary {
                        conn.set_input_focus(InputFocus::PARENT, event.window, CURRENT_TIME)?;
                    }
                    state.keyboard.request_raise();
                    status_bar_surface.request_raise();
                } else {
                    raise_shell(conn, shell_window)?;
                }
            }
        }
        Event::ConfigureRequest(event) => {
            if state.view.is_app() && apps.is_active_window(event.window) {
                let is_primary = apps.primary_active_window() == Some(event.window);
                if is_primary && state.keyboard.is_visible() {
                    resize_app_window(conn, event.window, width, keyboard_geometry.app_height())?;
                } else {
                    fullscreen_window(conn, event.window, width, height)?;
                }
                state.keyboard.request_raise();
                status_bar_surface.request_raise();
                if state.app_launching() {
                    raise_shell(conn, shell_window)?;
                }
            } else {
                let aux = ConfigureWindowAux::from_configure_request(&event);
                conn.configure_window(event.window, &aux)?;
                // Browser engines create auxiliary clipboard, selection, and
                // popup windows. Their configure requests must not raise the
                // shell over the application's primary window.
                if state.view.is_app() {
                    state.keyboard.request_raise();
                    status_bar_surface.request_raise();
                } else {
                    raise_shell(conn, shell_window)?;
                }
            }
        }
        Event::MapNotify(event)
            if event.window != shell_window
                && event.window != keyboard_window
                && event.window != volume_window
                && event.window != status_bar_window =>
        {
            if apps.note_mapped(event.window) {
                state.keyboard.request_raise();
                volume_surface.request_redraw();
                status_bar_surface.request_raise();
            } else {
                conn.unmap_window(event.window)?;
                raise_shell(conn, shell_window)?;
            }
        }
        Event::ConfigureNotify(event)
            if event.window != shell_window
                && event.window != keyboard_window
                && event.window != volume_window
                && event.window != status_bar_window =>
        {
            if apps
                .owner_of(event.window)
                .is_none_or(|owner| apps.active() == Some(owner))
            {
                state.keyboard.request_raise();
                volume_surface.request_redraw();
                status_bar_surface.request_raise();
            }
        }
        Event::DestroyNotify(event) => {
            if event.window != volume_window && event.window != status_bar_window {
                apps.remove_window(event.window);
                state.replace_managed_windows(apps.active_windows());
            }
        }
        Event::UnmapNotify(event) => {
            if event.window != keyboard_window
                && event.window != volume_window
                && event.window != status_bar_window
            {
                apps.note_unmapped(event.window);
                state.replace_managed_windows(apps.active_windows());
            }
        }
        Event::FocusIn(event) if apps.is_active_window(event.event) => {
            state.set_active_app_focused(true);
        }
        Event::FocusOut(event)
            if apps.is_active_window(event.event) && event.detail != NotifyDetail::INFERIOR =>
        {
            state.set_active_app_focused(false);
        }
        Event::MappingNotify(_) => {
            // The phone uses one fixed XKB map for the entire Xorg session.
            // Avoid introducing a runtime round trip into the event loop. A
            // mapping change takes effect after the next Moon session restart.
            eprintln!("X keyboard mapping changed; refresh deferred until restart");
        }
        _ => {}
    }
    Ok(())
}

fn raw_axis(event: &xinput::RawTouchBeginEvent, wanted: usize, maximum: u16) -> Option<i16> {
    let mut value_index = 0;
    for (word_index, mask) in event.valuator_mask.iter().copied().enumerate() {
        for bit in 0..32 {
            if mask & (1_u32 << bit) == 0 {
                continue;
            }
            let axis = word_index * 32 + bit;
            let value = event.axisvalues_raw.get(value_index)?;
            if axis == wanted {
                return Some(value.integral.clamp(0, i32::from(maximum)) as i16);
            }
            value_index += 1;
        }
    }
    None
}

fn raise_shell(conn: &RustConnection, shell_window: u32) -> Result<(), Box<dyn Error>> {
    conn.configure_window(
        shell_window,
        &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
    )?;
    Ok(())
}

fn fullscreen_window(
    conn: &RustConnection,
    window: u32,
    width: u16,
    height: u16,
) -> Result<(), Box<dyn Error>> {
    let content_height = app_content_height(height);
    conn.configure_window(
        window,
        &ConfigureWindowAux::new()
            .x(0)
            .y(i32::from(status_bar::HEIGHT))
            .width(u32::from(width))
            .height(u32::from(content_height))
            .border_width(0)
            .stack_mode(StackMode::ABOVE),
    )?;
    Ok(())
}

fn resize_app_window(
    conn: &RustConnection,
    window: u32,
    width: u16,
    height: u16,
) -> Result<(), Box<dyn Error>> {
    let content_height = app_content_height(height);
    conn.configure_window(
        window,
        &ConfigureWindowAux::new()
            .x(0)
            .y(i32::from(status_bar::HEIGHT))
            .width(u32::from(width))
            .height(u32::from(content_height))
            .border_width(0),
    )?;
    Ok(())
}

fn app_content_height(bottom: u16) -> u16 {
    bottom.saturating_sub(status_bar::HEIGHT).max(1)
}

fn resize_shell_window<C: Connection>(
    conn: &C,
    window: u32,
    width: u16,
    height: u16,
    inset: bool,
) -> Result<(), Box<dyn Error>> {
    let (top, content_height) = shell_geometry(height, inset);
    conn.configure_window(
        window,
        &ConfigureWindowAux::new()
            .x(0)
            .y(i32::from(top))
            .width(u32::from(width))
            .height(u32::from(content_height))
            .border_width(0),
    )?;
    Ok(())
}

fn shell_geometry(height: u16, inset: bool) -> (u16, u16) {
    let top = if inset { status_bar::HEIGHT } else { 0 };
    (top, height.saturating_sub(top).max(1))
}

fn apply_registry_update(
    conn: &RustConnection,
    shell_window: u32,
    width: u16,
    height: u16,
    state: &mut ShellState,
    apps: &mut AppRegistry,
    update: RegistryUpdate,
) -> Result<(), Box<dyn Error>> {
    let freeze_after_hide = update.freeze_after_hide.clone();
    for action in update.visibility {
        match action {
            WindowVisibility::Show(window) => {
                conn.map_window(window)?;
            }
            WindowVisibility::Hide(window) => {
                conn.unmap_window(window)?;
            }
        }
    }

    if !freeze_after_hide.is_empty() {
        // Submit every unmap before suspending the client process. Xorg can
        // then finish the visual transition without waiting on a frozen app.
        conn.flush()?;
        for app in freeze_after_hide {
            if let Err(error) = apps.freeze_background(app) {
                eprintln!(
                    "cannot freeze {} in background: {error}",
                    app.display_name()
                );
            }
        }
    }

    if let Some(app) = update.active_process_exited {
        state.home();
        state.last_action = format!("{}_process_exited", app.display_name().to_ascii_lowercase());
        raise_shell(conn, shell_window)?;
    } else if let Some(app) = update.active_process_failed {
        state.home();
        state.last_action = format!("{}_process_error", app.display_name().to_ascii_lowercase());
        raise_shell(conn, shell_window)?;
    } else if let Some(app) = update.resumed {
        state.note_app_resumed(app);
        if let Some(window) = apps.primary_active_window() {
            fullscreen_window(conn, window, width, height)?;
            conn.set_input_focus(InputFocus::PARENT, window, CURRENT_TIME)?;
        }
    }

    if apps.active().is_none() {
        state.set_active_app_focused(false);
        raise_shell(conn, shell_window)?;
    }
    Ok(())
}

fn pointer_begin(
    state: &mut ShellState,
    keyboard_geometry: &KeyboardGeometry,
    x: i16,
    y: i16,
    repeat_timing: KeyRepeatTiming,
) -> Option<keyboard::KeyAction> {
    if !state.screen_awake {
        return None;
    }
    if let Some(previous) = state.pointer.take()
        && previous.keyboard_pressed
        && let Some(index) = previous.keyboard_key_index
    {
        state.keyboard.release_key_index(index);
    }
    let (pointer, action) = new_pointer_gesture(state, keyboard_geometry, x, y, repeat_timing);
    if pointer.keyboard_pressed
        && let Some(index) = pointer.keyboard_key_index
    {
        state.keyboard.press_key_index(index);
    }
    state.pointer = Some(pointer);
    state.last_action = "pointer_begin".into();
    action
}

fn new_pointer_gesture(
    state: &ShellState,
    keyboard_geometry: &KeyboardGeometry,
    x: i16,
    y: i16,
    repeat_timing: KeyRepeatTiming,
) -> (PointerGesture, Option<keyboard::KeyAction>) {
    let keyboard_owned = keyboard_geometry.owns_touch(&state.keyboard, x, y);
    let keyboard_key_index = keyboard_geometry.key_index_at(&state.keyboard, x, y);
    let repeatable_action = keyboard_key_index
        .and_then(|index| keyboard_geometry.keys(&state.keyboard).get(index).cloned())
        .map(|key| key.action)
        .filter(|action| action.is_repeatable());
    let started = Instant::now();
    (
        PointerGesture {
            start_x: x,
            start_y: y,
            last_x: x,
            last_y: y,
            started,
            keyboard_owned,
            keyboard_key_index,
            keyboard_pressed: keyboard_owned && keyboard_key_index.is_some(),
            keyboard_initial_sent: repeatable_action.is_some(),
            keyboard_repeat_uppercase: state.keyboard.shift(),
            keyboard_next_repeat_at: repeatable_action.map(|_| started + repeat_timing.delay),
        },
        repeatable_action,
    )
}

fn pointer_move(state: &mut ShellState, keyboard_geometry: &KeyboardGeometry, x: i16, y: i16) {
    let current_key_index = keyboard_geometry.key_index_at(&state.keyboard, x, y);
    let pressed_key_index = state.pointer.as_ref().and_then(|pointer| {
        (pointer.keyboard_owned && pointer.keyboard_key_index == current_key_index)
            .then_some(pointer.keyboard_key_index)
            .flatten()
    });
    if let Some(pointer) = state.pointer.as_mut() {
        let was_pressed = pointer.keyboard_pressed;
        pointer.last_x = x;
        pointer.last_y = y;
        pointer.keyboard_pressed = pressed_key_index.is_some();
        if pressed_key_index.is_none() {
            pointer.keyboard_next_repeat_at = None;
        }
        if was_pressed != pointer.keyboard_pressed
            && let Some(index) = pointer.keyboard_key_index
        {
            if pointer.keyboard_pressed {
                state.keyboard.press_key_index(index);
            } else {
                state.keyboard.release_key_index(index);
            }
        }
    }
}

fn begin_raw_keyboard_contact(
    state: &mut ShellState,
    raw_touch: &mut RawTouchTracker,
    touch_id: u32,
    keyboard_geometry: &KeyboardGeometry,
    x: i16,
    y: i16,
    repeat_timing: KeyRepeatTiming,
) -> Option<keyboard::KeyAction> {
    if raw_touch
        .keyboard_contacts
        .iter()
        .any(|(existing, _)| *existing == touch_id)
    {
        return None;
    }
    let (pointer, action) = new_pointer_gesture(state, keyboard_geometry, x, y, repeat_timing);
    if !pointer.keyboard_owned {
        return None;
    }
    if pointer.keyboard_pressed
        && let Some(index) = pointer.keyboard_key_index
    {
        state.keyboard.press_key_index(index);
    }
    raw_touch.keyboard_contacts.push((touch_id, pointer));
    state.last_action = "keyboard_contact_begin".into();
    action
}

fn update_raw_keyboard_contact(
    state: &mut ShellState,
    raw_touch: &mut RawTouchTracker,
    touch_id: u32,
    keyboard_geometry: &KeyboardGeometry,
    x: Option<i16>,
    y: Option<i16>,
) {
    let Some(position) = raw_touch
        .keyboard_contacts
        .iter()
        .position(|(existing, _)| *existing == touch_id)
    else {
        return;
    };
    let (next_x, next_y) = {
        let pointer = &raw_touch.keyboard_contacts[position].1;
        (x.unwrap_or(pointer.last_x), y.unwrap_or(pointer.last_y))
    };
    let current_key_index = keyboard_geometry.key_index_at(&state.keyboard, next_x, next_y);
    let pointer = &mut raw_touch.keyboard_contacts[position].1;
    let should_press = pointer.keyboard_key_index == current_key_index;
    let was_pressed = pointer.keyboard_pressed;
    pointer.last_x = next_x;
    pointer.last_y = next_y;
    pointer.keyboard_pressed = should_press;
    if !should_press {
        pointer.keyboard_next_repeat_at = None;
    }
    if was_pressed != should_press
        && let Some(index) = pointer.keyboard_key_index
    {
        if should_press {
            state.keyboard.press_key_index(index);
        } else {
            state.keyboard.release_key_index(index);
        }
    }
}

fn end_raw_keyboard_contact(
    conn: &RustConnection,
    state: &mut ShellState,
    raw_touch: &mut RawTouchTracker,
    touch_id: u32,
    keyboard_geometry: &KeyboardGeometry,
    key_injector: &XtestInjector,
) {
    let Some(position) = raw_touch
        .keyboard_contacts
        .iter()
        .position(|(existing, _)| *existing == touch_id)
    else {
        return;
    };
    let (_, pointer) = raw_touch.keyboard_contacts.remove(position);
    if pointer.keyboard_pressed
        && let Some(index) = pointer.keyboard_key_index
    {
        state.keyboard.release_key_index(index);
    }
    let previous_layout = state.keyboard.layout();
    finish_keyboard_pointer(conn, state, pointer, keyboard_geometry, key_injector);
    if !state.keyboard.is_visible() || state.keyboard.layout() != previous_layout {
        for (_, contact) in raw_touch.keyboard_contacts.drain(..) {
            if contact.keyboard_pressed
                && let Some(index) = contact.keyboard_key_index
            {
                state.keyboard.release_key_index(index);
            }
        }
    }
}

fn repeat_keyboard_if_due(
    conn: &RustConnection,
    state: &mut ShellState,
    raw_touch: &mut RawTouchTracker,
    keyboard_geometry: &KeyboardGeometry,
    key_injector: &XtestInjector,
    repeat_timing: KeyRepeatTiming,
) {
    if !state.keyboard.is_visible() {
        return;
    }
    let now = Instant::now();
    let mut repeats = Vec::with_capacity(raw_touch.keyboard_contacts.len() + 1);
    if let Some(pointer) = state.pointer.as_mut()
        && let Some(repeat) = due_repeat(pointer, now, repeat_timing.interval)
    {
        repeats.push(repeat);
    }
    for (_, pointer) in &mut raw_touch.keyboard_contacts {
        if let Some(repeat) = due_repeat(pointer, now, repeat_timing.interval) {
            repeats.push(repeat);
        }
    }
    for (index, repeat_count, uppercase) in repeats {
        let Some(action) = keyboard_geometry
            .keys(&state.keyboard)
            .get(index)
            .map(|key| key.action)
            .filter(|action| action.is_repeatable())
        else {
            continue;
        };
        let input = match action {
            keyboard::KeyAction::Character(mut character) => {
                if uppercase && character.is_ascii_alphabetic() {
                    character.make_ascii_uppercase();
                }
                keyboard::KeyboardInput::Character(character)
            }
            keyboard::KeyAction::Space => keyboard::KeyboardInput::Character(' '),
            keyboard::KeyAction::Backspace => keyboard::KeyboardInput::Backspace,
            keyboard::KeyAction::Shift
            | keyboard::KeyAction::SwitchLayout(_)
            | keyboard::KeyAction::Enter => continue,
        };
        let result = state
            .managed_windows
            .first()
            .copied()
            .ok_or_else(|| "active app window is unavailable".into())
            .and_then(|primary| {
                key_injector.inject_repeats(
                    conn,
                    input,
                    primary,
                    state.active_app_focused(),
                    repeat_count,
                )
            });
        if let Err(error) = result {
            // Never include repeated character/key identity in diagnostics.
            eprintln!("keyboard repeat injection failed: {error}");
            state.hide_keyboard();
            state.pointer = None;
            raw_touch.keyboard_contacts.clear();
            raw_touch.touch_id = None;
            state.last_action = "keyboard_input_failed".into();
            return;
        }
        state.last_action = "keyboard_repeat".into();
    }
}

fn due_repeat(
    pointer: &mut PointerGesture,
    now: Instant,
    interval: Duration,
) -> Option<(usize, usize, bool)> {
    let index = pointer.keyboard_key_index?;
    if !pointer.keyboard_owned || !pointer.keyboard_pressed {
        return None;
    }
    let count = take_due_repeats(&mut pointer.keyboard_next_repeat_at, now, interval);
    (count > 0).then_some((index, count, pointer.keyboard_repeat_uppercase))
}

fn cancel_stale_pointer(state: &mut ShellState, raw_touch: &mut RawTouchTracker, now: Instant) {
    let legacy_stale = state
        .pointer
        .as_ref()
        .is_some_and(|pointer| now.duration_since(pointer.started) >= STALE_POINTER_TIMEOUT);
    let mut recovered = false;
    if legacy_stale {
        if let Some(pointer) = state.pointer.take()
            && pointer.keyboard_pressed
            && let Some(index) = pointer.keyboard_key_index
        {
            state.keyboard.release_key_index(index);
        }
        raw_touch.touch_id = None;
        recovered = true;
    }
    let mut position = 0;
    while position < raw_touch.keyboard_contacts.len() {
        if now.duration_since(raw_touch.keyboard_contacts[position].1.started)
            >= STALE_POINTER_TIMEOUT
        {
            let (_, pointer) = raw_touch.keyboard_contacts.remove(position);
            if pointer.keyboard_pressed
                && let Some(index) = pointer.keyboard_key_index
            {
                state.keyboard.release_key_index(index);
            }
            recovered = true;
        } else {
            position += 1;
        }
    }
    if recovered {
        state.last_action = "pointer_timeout_recovered".into();
    }
}

fn take_due_repeats(
    next_repeat_at: &mut Option<Instant>,
    now: Instant,
    interval: Duration,
) -> usize {
    let Some(mut deadline) = *next_repeat_at else {
        return 0;
    };
    let mut count = 0;
    while now >= deadline && count < MAX_REPEAT_CATCH_UP {
        count += 1;
        deadline += interval;
    }
    if count == MAX_REPEAT_CATCH_UP && now >= deadline {
        deadline = now + interval;
    }
    *next_repeat_at = Some(deadline);
    count
}

#[allow(clippy::too_many_arguments)]
fn pointer_end(
    conn: &RustConnection,
    root: u32,
    state: &mut ShellState,
    x: i16,
    y: i16,
    width: u16,
    height: u16,
    keyboard_geometry: &KeyboardGeometry,
    config: &Config,
    key_injector: &XtestInjector,
) {
    let Some(mut pointer) = state.pointer.take() else {
        return;
    };
    pointer.last_x = x;
    pointer.last_y = y;
    if pointer.keyboard_owned {
        if pointer.keyboard_pressed
            && let Some(index) = pointer.keyboard_key_index
        {
            state.keyboard.release_key_index(index);
        }
        finish_keyboard_pointer(conn, state, pointer, keyboard_geometry, key_injector);
        return;
    }
    let dx = i32::from(pointer.last_x) - i32::from(pointer.start_x);
    let dy = i32::from(pointer.last_y) - i32::from(pointer.start_y);
    let elapsed = pointer.started.elapsed();
    let upward = -dy;
    let bottom_start = i32::from(pointer.start_y) >= i32::from(height) - 180;
    let close_swipe = state.view.is_app()
        && bottom_start
        && upward >= 350
        && dx.abs() <= 300
        && upward * 3 >= dx.abs() * 4
        && elapsed <= Duration::from_millis(1400);
    if close_swipe {
        state.home();
        state.last_action = "swipe_up_background".into();
        return;
    }
    if dx.abs() <= 35 && dy.abs() <= 35 && elapsed <= Duration::from_millis(650) {
        handle_tap(
            conn,
            root,
            state,
            x,
            y,
            width,
            keyboard_geometry,
            config,
            key_injector,
        );
    } else {
        state.last_action = "gesture_cancel".into();
    }
    state.redraw = true;
}

fn finish_keyboard_pointer(
    conn: &RustConnection,
    state: &mut ShellState,
    pointer: PointerGesture,
    keyboard_geometry: &KeyboardGeometry,
    key_injector: &XtestInjector,
) {
    let dx = i32::from(pointer.last_x) - i32::from(pointer.start_x);
    let dy = i32::from(pointer.last_y) - i32::from(pointer.start_y);
    let elapsed = pointer.started.elapsed();
    let end_key_index =
        keyboard_geometry.key_index_at(&state.keyboard, pointer.last_x, pointer.last_y);
    if dx.abs() <= 60
        && dy.abs() <= 60
        && (pointer.keyboard_initial_sent || elapsed <= Duration::from_millis(850))
        && pointer.keyboard_key_index == end_key_index
    {
        if let Some(index) = pointer.keyboard_key_index {
            if !pointer.keyboard_initial_sent
                && let Some(key) = keyboard_geometry.keys(&state.keyboard).get(index)
            {
                handle_keyboard_action(conn, state, key.action, key_injector);
            }
        } else {
            state.last_action = "keyboard_tap_between_keys".into();
        }
    } else {
        state.last_action = "keyboard_gesture_cancel".into();
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_tap(
    conn: &RustConnection,
    root: u32,
    state: &mut ShellState,
    x: i16,
    y: i16,
    width: u16,
    keyboard_geometry: &KeyboardGeometry,
    config: &Config,
    key_injector: &XtestInjector,
) {
    if keyboard_geometry.owns_touch(&state.keyboard, x, y) {
        if let Some(action) = keyboard_geometry.action_at(&state.keyboard, x, y) {
            handle_keyboard_action(conn, state, action, key_injector);
        } else {
            state.last_action = "keyboard_tap_between_keys".into();
        }
        return;
    }
    match state.view {
        View::Locked => match ui::keypad_action_at(width, x, y) {
            Some(KeypadAction::Digit(digit)) => state.input_digit(digit, config),
            Some(KeypadAction::Backspace) => state.backspace_pin(),
            Some(KeypadAction::Submit) => {
                state.submit_pin(config);
            }
            None => state.last_action = "lock_tap_outside".into(),
        },
        View::Launcher => {
            if ui::system_app_at(x, y) {
                state.launch_system();
            } else if ui::browser_app_at(x, y) {
                state.launch_browser();
            } else {
                state.last_action = "launcher_tap_outside".into();
            }
        }
        View::System | View::Browser => {
            let app_name = if state.view == View::System {
                "system"
            } else {
                "browser"
            };
            let Some(_window) = state.managed_windows.first().copied() else {
                state.last_action = format!("{app_name}_tap_no_window");
                state.redraw = true;
                return;
            };
            match forward_tap(conn, root, x, y) {
                Ok(()) => state.last_action = format!("{app_name}_tap_forwarded"),
                Err(error) => {
                    eprintln!("cannot forward tap to {app_name}: {error}");
                    state.last_action = format!("{app_name}_tap_failed");
                }
            }
        }
    }
    state.redraw = true;
}

fn handle_keyboard_action(
    conn: &RustConnection,
    state: &mut ShellState,
    action: keyboard::KeyAction,
    key_injector: &XtestInjector,
) {
    match state.activate_keyboard_key(action) {
        KeyboardEffect::Inject(input) => {
            inject_keyboard_input(conn, state, input, key_injector, None);
        }
        KeyboardEffect::None => {}
    }
}

fn inject_keyboard_input(
    conn: &RustConnection,
    state: &mut ShellState,
    input: keyboard::KeyboardInput,
    key_injector: &XtestInjector,
    success_action: Option<&'static str>,
) {
    let result = state
        .managed_windows
        .first()
        .copied()
        .ok_or_else(|| "active app window is unavailable".into())
        .and_then(|primary| key_injector.inject(conn, input, primary, state.active_app_focused()));
    if let Err(error) = result {
        // Never include the character or key action in this diagnostic;
        // password input is intentionally ephemeral and non-loggable.
        eprintln!("keyboard input injection failed: {error}");
        state.hide_keyboard();
        state.last_action = "keyboard_input_failed".into();
    } else if let Some(action) = success_action {
        state.last_action = action.into();
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_command(
    conn: &RustConnection,
    root: u32,
    command: Command,
    width: u16,
    height: u16,
    keyboard_geometry: &KeyboardGeometry,
    config: &Config,
    state: &mut ShellState,
    key_injector: &XtestInjector,
    audio_volume: Option<&AudioVolume>,
    key_repeat_timing: KeyRepeatTiming,
) {
    match command {
        Command::Ping | Command::State => {}
        Command::Digit(digit) => state.input_digit(digit, config),
        Command::Backspace => state.backspace_pin(),
        Command::Submit => {
            state.submit_pin(config);
        }
        Command::Tap(x, y) => handle_tap(
            conn,
            root,
            state,
            x,
            y,
            width,
            keyboard_geometry,
            config,
            key_injector,
        ),
        Command::PointerBegin(x, y) => {
            if let Some(action) = pointer_begin(state, keyboard_geometry, x, y, key_repeat_timing) {
                handle_keyboard_action(conn, state, action, key_injector);
            }
        }
        Command::PointerMove(x, y) => pointer_move(state, keyboard_geometry, x, y),
        Command::PointerEnd(x, y) => pointer_end(
            conn,
            root,
            state,
            x,
            y,
            width,
            height,
            keyboard_geometry,
            config,
            key_injector,
        ),
        Command::Lock => state.lock(),
        Command::Home => state.home(),
        Command::LaunchSystem => state.launch_system(),
        Command::LaunchBrowser => state.launch_browser(),
        Command::KeyboardShow(purpose) => {
            state.show_keyboard(purpose);
        }
        Command::KeyboardHide => state.hide_keyboard(),
        Command::SwipeUp => {
            if state.view.is_app() {
                state.home();
                state.last_action = "swipe_up_background".into();
            }
        }
        Command::VolumeUp => change_volume(state, audio_volume, 5),
        Command::VolumeDown => change_volume(state, audio_volume, -5),
        Command::VolumeSet(value) => set_volume(state, audio_volume, value),
        Command::Power => state.toggle_screen(),
        Command::ScreenOff => state.screen_off(),
        Command::ScreenOn => state.screen_on(),
        Command::LeaseAcquire(_, _, _)
        | Command::LeaseRelease(_, _)
        | Command::SimulateMemoryPressure
        | Command::SuspendTest(_) => {}
        Command::Quit => state.should_exit = true,
    }
}

fn change_volume(state: &mut ShellState, audio_volume: Option<&AudioVolume>, delta: i8) {
    state.change_volume(delta);
    sync_audio_volume(audio_volume, state.volume);
}

fn set_volume(state: &mut ShellState, audio_volume: Option<&AudioVolume>, volume: u8) {
    state.set_volume(volume);
    sync_audio_volume(audio_volume, state.volume);
}

fn sync_audio_volume(audio_volume: Option<&AudioVolume>, volume: u8) {
    if let Some(control) = audio_volume
        && let Err(error) = control.set(volume)
    {
        eprintln!("audio volume sync failed: {error}");
    }
}

fn forward_tap(conn: &RustConnection, root: u32, x: i16, y: i16) -> Result<(), Box<dyn Error>> {
    // Send through XTEST rather than constructing an event for the managed
    // top-level. Chromium places its page surface in descendant X windows; a
    // SendEvent aimed at the top-level never reaches the renderer and cannot
    // establish page focus. XTEST performs normal server hit-testing at the
    // physical coordinate, so native app controls and embedded page surfaces
    // receive the same pointer sequence as a hardware tap.
    let _ = conn.xtest_fake_input(MOTION_NOTIFY_EVENT, 0, CURRENT_TIME, root, x, y, 0)?;
    for response_type in [BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT] {
        let _ = conn.xtest_fake_input(response_type, 1, CURRENT_TIME, root, 0, 0, 0)?;
    }
    conn.flush()?;
    Ok(())
}

#[cfg(test)]
mod touch_tests {
    use super::*;

    fn fp(value: i32) -> xinput::Fp3232 {
        xinput::Fp3232 {
            integral: value,
            frac: 0,
        }
    }

    #[test]
    fn app_layout_reserves_one_top_status_inset() {
        assert_eq!(app_content_height(2340), 2216);
        assert_eq!(app_content_height(1520), 1396);
        assert_eq!(app_content_height(0), 1);
        assert_eq!(shell_geometry(2340, true), (124, 2216));
        assert_eq!(shell_geometry(2340, false), (0, 2340));
    }

    #[test]
    fn media_heartbeat_requires_a_recent_existing_file() {
        let path = env::temp_dir().join(format!("moon-media-heartbeat-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert!(!heartbeat_is_recent(&path, Duration::from_secs(2)));
        std::fs::write(&path, b"active\n").unwrap();
        assert!(heartbeat_is_recent(&path, Duration::from_secs(2)));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn poll_timeout_rounds_up_without_overflowing() {
        assert_eq!(duration_to_poll_timeout(Duration::ZERO), 0);
        assert_eq!(duration_to_poll_timeout(Duration::from_nanos(1)), 1);
        assert_eq!(duration_to_poll_timeout(Duration::from_millis(8)), 8);
        assert_eq!(duration_to_poll_timeout(Duration::MAX), i32::MAX);
    }

    #[test]
    fn raw_touch_axes_use_physical_valuator_coordinates() {
        let event = xinput::RawTouchBeginEvent {
            valuator_mask: vec![0b1111],
            axisvalues_raw: vec![fp(820), fp(808), fp(6), fp(4)],
            ..Default::default()
        };
        assert_eq!(raw_axis(&event, 0, 1079), Some(820));
        assert_eq!(raw_axis(&event, 1, 2339), Some(808));
    }

    #[test]
    fn sparse_raw_touch_update_preserves_absent_axis() {
        let event = xinput::RawTouchBeginEvent {
            valuator_mask: vec![0b0010],
            axisvalues_raw: vec![fp(1700)],
            ..Default::default()
        };
        assert_eq!(raw_axis(&event, 0, 1079), None);
        assert_eq!(raw_axis(&event, 1, 2339), Some(1700));
    }

    #[test]
    fn keyboard_repeat_matches_workstation_xrate() {
        let config = KeyRepeatTiming {
            delay: Duration::from_millis(200),
            interval: Duration::from_nanos(1_000_000_000 / 45),
        };
        assert_eq!(config.delay, Duration::from_millis(200));
        assert_eq!(config.interval, Duration::from_nanos(22_222_222));

        let base = Instant::now();
        let mut next = Some(base + config.delay);
        assert_eq!(
            take_due_repeats(
                &mut next,
                base + Duration::from_millis(199),
                config.interval,
            ),
            0
        );
        assert_eq!(
            take_due_repeats(
                &mut next,
                base + Duration::from_millis(200),
                config.interval,
            ),
            1
        );
        assert_eq!(
            take_due_repeats(
                &mut next,
                base + Duration::from_millis(245),
                config.interval,
            ),
            2
        );
    }

    #[test]
    fn keyboard_contacts_have_independent_multitouch_rollover() {
        let geometry = KeyboardGeometry::new(1080, 2340);
        let repeat = KeyRepeatTiming {
            delay: Duration::from_millis(200),
            interval: Duration::from_nanos(1_000_000_000 / 45),
        };
        let mut state = ShellState::new(false, 50);
        state.keyboard.show(keyboard::KeyboardPurpose::Text);
        let mut raw_touch = RawTouchTracker::default();

        let first = begin_raw_keyboard_contact(
            &mut state,
            &mut raw_touch,
            11,
            &geometry,
            754,
            1752,
            repeat,
        );
        let second = begin_raw_keyboard_contact(
            &mut state,
            &mut raw_touch,
            12,
            &geometry,
            647,
            1752,
            repeat,
        );

        assert_eq!(first, Some(keyboard::KeyAction::Character('j')));
        assert_eq!(second, Some(keyboard::KeyAction::Character('h')));
        assert_eq!(raw_touch.keyboard_contacts.len(), 2);
        let first_index = raw_touch.keyboard_contacts[0].1.keyboard_key_index.unwrap();
        let second_index = raw_touch.keyboard_contacts[1].1.keyboard_key_index.unwrap();
        assert!(state.keyboard.is_key_pressed(first_index));
        assert!(state.keyboard.is_key_pressed(second_index));

        update_raw_keyboard_contact(
            &mut state,
            &mut raw_touch,
            11,
            &geometry,
            Some(10),
            Some(1752),
        );
        assert!(!state.keyboard.is_key_pressed(first_index));
        assert!(state.keyboard.is_key_pressed(second_index));
        assert!(
            raw_touch.keyboard_contacts[0]
                .1
                .keyboard_next_repeat_at
                .is_none()
        );
        assert!(
            raw_touch.keyboard_contacts[1]
                .1
                .keyboard_next_repeat_at
                .is_some()
        );
    }

    #[test]
    fn stale_touch_watchdog_releases_keyboard_and_raw_contact() {
        let now = Instant::now();
        let mut state = ShellState::new(false, 50);
        state.keyboard.show(keyboard::KeyboardPurpose::Text);
        state.keyboard.press_key_index(0);
        state.pointer = Some(PointerGesture {
            start_x: 64,
            start_y: 1604,
            last_x: 64,
            last_y: 1604,
            started: now - STALE_POINTER_TIMEOUT - Duration::from_millis(1),
            keyboard_owned: true,
            keyboard_key_index: Some(0),
            keyboard_pressed: true,
            keyboard_initial_sent: true,
            keyboard_repeat_uppercase: false,
            keyboard_next_repeat_at: Some(now),
        });
        let mut raw_touch = RawTouchTracker {
            touch_id: Some(7),
            x: 64,
            y: 1604,
            keyboard_contacts: Vec::new(),
        };

        cancel_stale_pointer(&mut state, &mut raw_touch, now);

        assert!(state.pointer.is_none());
        assert!(raw_touch.touch_id.is_none());
        assert!(!state.keyboard.is_key_pressed(0));
        assert_eq!(state.last_action, "pointer_timeout_recovered");
    }
}
