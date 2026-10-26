//! Panel and display-pipeline power through the X server's DPMS extension.
//!
//! Zeroing the backlight alone leaves the A26's panel, DSI link and DECON
//! refreshing at 120 Hz behind a dark screen: about 185 mA of a ~200 mA
//! screen-off drain. DPMS Off makes the Exynos DRM driver commit the CRTC
//! inactive without a modeset, so the panel sleeps and loses its regulators
//! and DECON/DSIM runtime-suspend (≈ 34 mA total). DPMS On brings all of it
//! back in place in about 150 ms. A full modeset (`xrandr --off`, a mode
//! change) is a different path that this firmware cannot undo, so nothing here
//! touches the mode.
//!
//! Requests are one-way, as Moon's long-lived connection requires; whether
//! DPMS is usable is decided once at startup in `main.rs`. In GPU mode Xorg
//! has no display; moon-compositor makes the same CRTC commit instead.

use x11rb::connection::Connection;
use x11rb::errors::ConnectionError;
use x11rb::protocol::dpms::{ConnectionExt as _, DPMSMode};

use crate::gpu::SharedLink;

pub struct DisplayPower {
    available: bool,
    gpu: Option<SharedLink>,
}

impl DisplayPower {
    pub fn new(available: bool) -> Self {
        Self {
            available,
            gpu: None,
        }
    }

    pub fn gpu(link: SharedLink) -> Self {
        Self {
            available: true,
            gpu: Some(link),
        }
    }

    pub fn describe(&self) -> &'static str {
        match (&self.gpu, self.available) {
            (Some(_), _) => "CRTC off through moon-compositor",
            (None, true) => "DPMS",
            (None, false) => "backlight only",
        }
    }

    /// Powers the panel and display pipeline down. Xorg's own DPMS timers are
    /// zeroed at startup, so enabling DPMS here only allows the forced level.
    pub fn off<C: Connection>(&self, conn: &C) -> Result<(), ConnectionError> {
        if let Some(link) = &self.gpu {
            link.borrow_mut().power(false);
            return Ok(());
        }
        if !self.available {
            return Ok(());
        }
        conn.dpms_enable()?;
        conn.dpms_force_level(DPMSMode::OFF)?;
        conn.flush()
    }

    /// Powers the panel back up and disables DPMS again, so Xorg can never
    /// blank on its own while Moon believes the screen is awake.
    pub fn on<C: Connection>(&self, conn: &C) -> Result<(), ConnectionError> {
        if let Some(link) = &self.gpu {
            link.borrow_mut().power(true);
            return Ok(());
        }
        if !self.available {
            return Ok(());
        }
        conn.dpms_enable()?;
        conn.dpms_force_level(DPMSMode::ON)?;
        conn.dpms_disable()?;
        conn.flush()
    }
}
