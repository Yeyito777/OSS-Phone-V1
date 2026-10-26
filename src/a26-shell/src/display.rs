//! Types shared by Moon's brightness backend and its in-shell controls
//! (Control Centre). The socket interface is docs/display-brightness.md.
// Stubs on main until the brightness backend and Control Centre land.
#![allow(dead_code)]

/// A brightness change requested from inside Moon. The backend drains
/// `ShellState::display_requests` every loop and applies each request exactly
/// like the control-socket commands `brightness set N` and
/// `brightness auto on|off`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayRequest {
    /// The user's brightness, 0..=100 (re-anchors the curve when auto is on).
    Set(u8),
    /// Automatic brightness on or off.
    Auto(bool),
}

/// What in-shell controls show; the backend refreshes
/// `ShellState::display_view` every loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayView {
    /// The level shown now, 0..=100.
    pub brightness: u8,
    pub auto: bool,
    /// False when no light sensor was found (hide the auto toggle).
    pub sensor_available: bool,
}
