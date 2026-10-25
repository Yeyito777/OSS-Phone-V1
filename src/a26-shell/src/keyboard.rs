use std::error::Error;

use serde::Serialize;
use x11rb::connection::Connection;
use x11rb::protocol::shape::{ConnectionExt as _, SK, SO};
use x11rb::protocol::xproto::{ClipOrdering, Rectangle};
use x11rb::protocol::xproto::{
    ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, EventMask, InputFocus,
    KEY_PRESS_EVENT, KEY_RELEASE_EVENT, Keycode, Keysym, PropMode, StackMode, Window, WindowClass,
};
use x11rb::protocol::xtest::{self, ConnectionExt as _};
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, CURRENT_TIME};

use crate::gfx::{Canvas, Path, RectF, Rgb};
use crate::present::Presenter;
use crate::text::{self, Weight};
use crate::ui::{BLUE, WHITE};

// iOS dark-appearance keyboard colors.
const KEYBOARD_BG: Rgb = 0x1c1c1e;
const LETTER_KEY: Rgb = 0x5a5a5e;
const FUNCTION_KEY: Rgb = 0x38383b;
const PRESSED_LETTER_KEY: Rgb = 0x8a8a8f;
const KEY_RADIUS: f32 = 15.0;
/// Callouts are a step lighter than the keys so they read above the rows.
const POPUP_KEY: Rgb = 0x707075;

// Apple does not publish fixed software-keyboard frames. These dimensions use
// the standard 390-point iPhone portrait keyboard as a proportional reference
// and scale to Moon's 1080-pixel-wide display. The final 180 physical pixels
// remain Moon's app-close start zone, analogous to the iPhone bottom safe area.
pub const KEYBOARD_HEIGHT: u16 = 820;
pub const CLOSE_START_ZONE_HEIGHT: u16 = 180;
const REFERENCE_KEY_AREA_HEIGHT: u16 = KEYBOARD_HEIGHT - CLOSE_START_ZONE_HEIGHT;
const REFERENCE_SCREEN_WIDTH: u16 = 1080;
const ROW_Y: [u16; 4] = [18, 166, 314, 462];
const ROW_HEIGHT: [u16; 4] = [132, 132, 132, 160];
const KEY_GAP: u16 = 10;

const XK_BACK_SPACE: Keysym = 0xff08;
const XK_RETURN: Keysym = 0xff0d;
const XK_SHIFT_L: Keysym = 0xffe1;
const XK_SHIFT_R: Keysym = 0xffe2;
const XK_MODE_SWITCH: Keysym = 0xff7e;
const XK_ISO_LEVEL3_SHIFT: Keysym = 0xfe03;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyboardPurpose {
    Text,
    Url,
    Search,
    Password,
    Number,
}

impl KeyboardPurpose {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "text" => Some(Self::Text),
            "url" => Some(Self::Url),
            "search" => Some(Self::Search),
            "password" => Some(Self::Password),
            "number" => Some(Self::Number),
            _ => None,
        }
    }

    fn submit_label(self) -> &'static str {
        match self {
            Self::Url => "GO",
            Self::Search => "SEARCH",
            Self::Text => "RETURN",
            Self::Password | Self::Number => "DONE",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyboardLayout {
    Letters,
    Numbers,
    Symbols,
    NumberPad,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PublicKeyboardState {
    pub visible: bool,
    pub purpose: Option<KeyboardPurpose>,
    pub shift: bool,
    pub layout: KeyboardLayout,
}

#[derive(Debug)]
pub struct KeyboardState {
    visible: bool,
    purpose: Option<KeyboardPurpose>,
    shift: bool,
    layout: KeyboardLayout,
    pressed_key_indices: Vec<usize>,
    redraw: bool,
    raise_pending: bool,
}

impl Default for KeyboardState {
    fn default() -> Self {
        Self {
            visible: false,
            purpose: None,
            shift: false,
            layout: KeyboardLayout::Letters,
            pressed_key_indices: Vec::new(),
            redraw: false,
            raise_pending: false,
        }
    }
}

impl KeyboardState {
    pub fn show(&mut self, purpose: KeyboardPurpose) {
        self.visible = true;
        self.purpose = Some(purpose);
        self.shift = false;
        self.layout = if purpose == KeyboardPurpose::Number {
            KeyboardLayout::NumberPad
        } else {
            KeyboardLayout::Letters
        };
        self.pressed_key_indices.clear();
        self.redraw = true;
        self.raise_pending = true;
    }

    pub fn hide(&mut self) -> bool {
        let was_visible = self.visible;
        self.visible = false;
        self.purpose = None;
        self.shift = false;
        self.layout = KeyboardLayout::Letters;
        self.pressed_key_indices.clear();
        self.redraw = false;
        self.raise_pending = false;
        was_visible
    }

    pub fn public(&self) -> PublicKeyboardState {
        PublicKeyboardState {
            visible: self.visible,
            purpose: self.purpose,
            shift: self.shift,
            layout: self.layout,
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn purpose(&self) -> Option<KeyboardPurpose> {
        self.purpose
    }

    pub fn layout(&self) -> KeyboardLayout {
        self.layout
    }

    pub fn shift(&self) -> bool {
        self.shift
    }

    pub fn press_key_index(&mut self, index: usize) {
        self.pressed_key_indices.push(index);
        self.request_redraw();
    }

    pub fn release_key_index(&mut self, index: usize) {
        if let Some(position) = self
            .pressed_key_indices
            .iter()
            .position(|pressed| *pressed == index)
        {
            self.pressed_key_indices.remove(position);
            self.request_redraw();
        }
    }

    /// The most recently pressed key still held down.
    pub fn latest_pressed(&self) -> Option<usize> {
        self.pressed_key_indices.last().copied()
    }

    pub fn is_key_pressed(&self, index: usize) -> bool {
        self.pressed_key_indices.contains(&index)
    }

    pub fn needs_redraw(&self) -> bool {
        self.redraw
    }

    pub fn clear_redraw(&mut self) {
        self.redraw = false;
    }

    pub fn request_redraw(&mut self) {
        if self.visible {
            self.redraw = true;
        }
    }

    pub fn request_raise(&mut self) {
        if self.visible {
            self.raise_pending = true;
        }
    }

    pub fn raise_pending(&self) -> bool {
        self.raise_pending
    }

    pub fn clear_raise(&mut self) {
        self.raise_pending = false;
    }

    /// Apply one key without retaining any text. The returned input exists only
    /// long enough for the caller to emit one XTEST key sequence.
    pub fn activate(&mut self, action: KeyAction) -> KeyboardEffect {
        if !self.visible {
            return KeyboardEffect::None;
        }
        match action {
            KeyAction::Character(mut character) => {
                if self.shift && character.is_ascii_alphabetic() {
                    character.make_ascii_uppercase();
                }
                if self.shift {
                    self.shift = false;
                    self.redraw = true;
                }
                KeyboardEffect::Inject(KeyboardInput::Character(character))
            }
            KeyAction::Shift if self.layout == KeyboardLayout::Letters => {
                self.shift = !self.shift;
                self.redraw = true;
                KeyboardEffect::None
            }
            KeyAction::Shift => KeyboardEffect::None,
            KeyAction::SwitchLayout(layout)
                if self.purpose != Some(KeyboardPurpose::Number)
                    && layout != KeyboardLayout::NumberPad =>
            {
                self.layout = layout;
                self.shift = false;
                self.redraw = true;
                KeyboardEffect::None
            }
            KeyAction::SwitchLayout(_) => KeyboardEffect::None,
            KeyAction::Space => KeyboardEffect::Inject(KeyboardInput::Character(' ')),
            KeyAction::Backspace => KeyboardEffect::Inject(KeyboardInput::Backspace),
            KeyAction::Enter => {
                self.hide();
                KeyboardEffect::Inject(KeyboardInput::Enter)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    Character(char),
    Shift,
    SwitchLayout(KeyboardLayout),
    Space,
    Backspace,
    Enter,
}

impl KeyAction {
    pub fn is_repeatable(self) -> bool {
        matches!(self, Self::Character(_) | Self::Space | Self::Backspace)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum KeyboardInput {
    Character(char),
    Backspace,
    Enter,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum KeyboardEffect {
    None,
    Inject(KeyboardInput),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyRect {
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
}

impl KeyRect {
    fn contains(self, x: i16, y: i16) -> bool {
        let x = i32::from(x);
        let y = i32::from(y);
        x >= i32::from(self.x)
            && x < i32::from(self.x) + i32::from(self.width)
            && y >= i32::from(self.y)
            && y < i32::from(self.y) + i32::from(self.height)
    }

    fn rect_f(self) -> RectF {
        RectF::new(
            f32::from(self.x),
            f32::from(self.y),
            f32::from(self.width),
            f32::from(self.height),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyboardKey {
    pub rect: KeyRect,
    pub label: &'static str,
    pub action: KeyAction,
}

#[derive(Debug, Clone, Copy)]
pub struct KeyboardGeometry {
    pub screen_width: u16,
    pub window_y: i16,
    pub window_height: u16,
    pub key_area_height: u16,
}

impl KeyboardGeometry {
    pub fn new(screen_width: u16, screen_height: u16) -> Self {
        let window_height = screen_height.min(KEYBOARD_HEIGHT);
        let safe_height = window_height.min(CLOSE_START_ZONE_HEIGHT);
        Self {
            screen_width,
            window_y: screen_height.saturating_sub(window_height) as i16,
            window_height,
            key_area_height: window_height.saturating_sub(safe_height),
        }
    }

    pub fn owns_touch(&self, state: &KeyboardState, root_x: i16, root_y: i16) -> bool {
        if !state.is_visible() || root_x < 0 || i32::from(root_x) >= i32::from(self.screen_width) {
            return false;
        }
        let local_y = i32::from(root_y) - i32::from(self.window_y);
        local_y >= 0 && local_y < i32::from(self.key_area_height)
    }

    pub fn app_height(&self) -> u16 {
        self.window_y.max(1) as u16
    }

    pub fn action_at(&self, state: &KeyboardState, root_x: i16, root_y: i16) -> Option<KeyAction> {
        if !self.owns_touch(state, root_x, root_y) {
            return None;
        }
        self.keys(state)
            .get(self.key_index_at(state, root_x, root_y)?)
            .map(|key| key.action)
    }

    pub fn key_index_at(&self, state: &KeyboardState, root_x: i16, root_y: i16) -> Option<usize> {
        if !self.owns_touch(state, root_x, root_y) {
            return None;
        }
        let local_y = root_y.saturating_sub(self.window_y);
        self.keys(state)
            .iter()
            .position(|key| key.rect.contains(root_x, local_y))
    }

    pub fn keys(&self, state: &KeyboardState) -> Vec<KeyboardKey> {
        let mut keys = Vec::with_capacity(48);
        match state.layout() {
            KeyboardLayout::Letters => self.add_letter_keys(state, &mut keys),
            KeyboardLayout::Numbers => self.add_number_keys(state, &mut keys),
            KeyboardLayout::Symbols => self.add_symbol_keys(state, &mut keys),
            KeyboardLayout::NumberPad => self.add_number_pad_keys(&mut keys),
        }
        keys
    }

    fn add_letter_keys(&self, state: &KeyboardState, keys: &mut Vec<KeyboardKey>) {
        self.add_row(
            keys,
            0,
            16,
            &[
                ("Q", KeyAction::Character('q'), 10),
                ("W", KeyAction::Character('w'), 10),
                ("E", KeyAction::Character('e'), 10),
                ("R", KeyAction::Character('r'), 10),
                ("T", KeyAction::Character('t'), 10),
                ("Y", KeyAction::Character('y'), 10),
                ("U", KeyAction::Character('u'), 10),
                ("I", KeyAction::Character('i'), 10),
                ("O", KeyAction::Character('o'), 10),
                ("P", KeyAction::Character('p'), 10),
            ],
        );
        self.add_row(
            keys,
            1,
            64,
            &[
                ("A", KeyAction::Character('a'), 10),
                ("S", KeyAction::Character('s'), 10),
                ("D", KeyAction::Character('d'), 10),
                ("F", KeyAction::Character('f'), 10),
                ("G", KeyAction::Character('g'), 10),
                ("H", KeyAction::Character('h'), 10),
                ("J", KeyAction::Character('j'), 10),
                ("K", KeyAction::Character('k'), 10),
                ("L", KeyAction::Character('l'), 10),
            ],
        );
        self.add_row(
            keys,
            2,
            16,
            &[
                ("SHIFT", KeyAction::Shift, 13),
                ("Z", KeyAction::Character('z'), 10),
                ("X", KeyAction::Character('x'), 10),
                ("C", KeyAction::Character('c'), 10),
                ("V", KeyAction::Character('v'), 10),
                ("B", KeyAction::Character('b'), 10),
                ("N", KeyAction::Character('n'), 10),
                ("M", KeyAction::Character('m'), 10),
                ("DEL", KeyAction::Backspace, 13),
            ],
        );
        self.add_bottom_row(state, keys, KeyboardLayout::Numbers, "123");
    }

    fn add_number_keys(&self, state: &KeyboardState, keys: &mut Vec<KeyboardKey>) {
        self.add_row(
            keys,
            0,
            16,
            &[
                ("1", KeyAction::Character('1'), 10),
                ("2", KeyAction::Character('2'), 10),
                ("3", KeyAction::Character('3'), 10),
                ("4", KeyAction::Character('4'), 10),
                ("5", KeyAction::Character('5'), 10),
                ("6", KeyAction::Character('6'), 10),
                ("7", KeyAction::Character('7'), 10),
                ("8", KeyAction::Character('8'), 10),
                ("9", KeyAction::Character('9'), 10),
                ("0", KeyAction::Character('0'), 10),
            ],
        );
        self.add_row(
            keys,
            1,
            16,
            &[
                ("-", KeyAction::Character('-'), 10),
                ("/", KeyAction::Character('/'), 10),
                (":", KeyAction::Character(':'), 10),
                (";", KeyAction::Character(';'), 10),
                ("(", KeyAction::Character('('), 10),
                (")", KeyAction::Character(')'), 10),
                ("$", KeyAction::Character('$'), 10),
                ("&", KeyAction::Character('&'), 10),
                ("@", KeyAction::Character('@'), 10),
                ("\"", KeyAction::Character('"'), 10),
            ],
        );
        self.add_row(
            keys,
            2,
            16,
            &[
                ("#+=", KeyAction::SwitchLayout(KeyboardLayout::Symbols), 16),
                (".", KeyAction::Character('.'), 10),
                (",", KeyAction::Character(','), 10),
                ("?", KeyAction::Character('?'), 10),
                ("!", KeyAction::Character('!'), 10),
                ("'", KeyAction::Character('\''), 10),
                ("DEL", KeyAction::Backspace, 16),
            ],
        );
        self.add_bottom_row(state, keys, KeyboardLayout::Letters, "ABC");
    }

    fn add_symbol_keys(&self, state: &KeyboardState, keys: &mut Vec<KeyboardKey>) {
        self.add_row(
            keys,
            0,
            16,
            &[
                ("[", KeyAction::Character('['), 10),
                ("]", KeyAction::Character(']'), 10),
                ("{", KeyAction::Character('{'), 10),
                ("}", KeyAction::Character('}'), 10),
                ("#", KeyAction::Character('#'), 10),
                ("%", KeyAction::Character('%'), 10),
                ("^", KeyAction::Character('^'), 10),
                ("*", KeyAction::Character('*'), 10),
                ("+", KeyAction::Character('+'), 10),
                ("=", KeyAction::Character('='), 10),
            ],
        );
        self.add_row(
            keys,
            1,
            64,
            &[
                ("_", KeyAction::Character('_'), 10),
                ("\\", KeyAction::Character('\\'), 10),
                ("|", KeyAction::Character('|'), 10),
                ("~", KeyAction::Character('~'), 10),
                ("<", KeyAction::Character('<'), 10),
                (">", KeyAction::Character('>'), 10),
                ("$", KeyAction::Character('$'), 10),
                ("&", KeyAction::Character('&'), 10),
                ("@", KeyAction::Character('@'), 10),
            ],
        );
        self.add_row(
            keys,
            2,
            16,
            &[
                ("123", KeyAction::SwitchLayout(KeyboardLayout::Numbers), 16),
                (".", KeyAction::Character('.'), 10),
                (",", KeyAction::Character(','), 10),
                ("?", KeyAction::Character('?'), 10),
                ("!", KeyAction::Character('!'), 10),
                ("'", KeyAction::Character('\''), 10),
                ("DEL", KeyAction::Backspace, 16),
            ],
        );
        self.add_bottom_row(state, keys, KeyboardLayout::Letters, "ABC");
    }

    fn add_number_pad_keys(&self, keys: &mut Vec<KeyboardKey>) {
        for (row, definitions) in [
            [
                ("1", KeyAction::Character('1'), 10),
                ("2", KeyAction::Character('2'), 10),
                ("3", KeyAction::Character('3'), 10),
            ],
            [
                ("4", KeyAction::Character('4'), 10),
                ("5", KeyAction::Character('5'), 10),
                ("6", KeyAction::Character('6'), 10),
            ],
            [
                ("7", KeyAction::Character('7'), 10),
                ("8", KeyAction::Character('8'), 10),
                ("9", KeyAction::Character('9'), 10),
            ],
            [
                (".", KeyAction::Character('.'), 10),
                ("0", KeyAction::Character('0'), 10),
                ("DEL", KeyAction::Backspace, 10),
            ],
        ]
        .into_iter()
        .enumerate()
        {
            self.add_row(keys, row, 132, &definitions);
        }
    }

    fn add_bottom_row(
        &self,
        state: &KeyboardState,
        keys: &mut Vec<KeyboardKey>,
        switch_to: KeyboardLayout,
        switch_label: &'static str,
    ) {
        let submit = state
            .purpose()
            .unwrap_or(KeyboardPurpose::Text)
            .submit_label();
        if state.purpose() == Some(KeyboardPurpose::Url) {
            self.add_row(
                keys,
                3,
                16,
                &[
                    (switch_label, KeyAction::SwitchLayout(switch_to), 18),
                    ("SPACE", KeyAction::Space, 48),
                    (".", KeyAction::Character('.'), 12),
                    (submit, KeyAction::Enter, 22),
                ],
            );
        } else {
            self.add_row(
                keys,
                3,
                16,
                &[
                    (switch_label, KeyAction::SwitchLayout(switch_to), 20),
                    ("SPACE", KeyAction::Space, 60),
                    (submit, KeyAction::Enter, 20),
                ],
            );
        }
    }

    fn add_row(
        &self,
        output: &mut Vec<KeyboardKey>,
        row: usize,
        reference_margin: u16,
        definitions: &[(&'static str, KeyAction, u16)],
    ) {
        if definitions.is_empty() || self.key_area_height == 0 {
            return;
        }
        let scale_y = |value: u16| {
            u32::from(value) * u32::from(self.key_area_height)
                / u32::from(REFERENCE_KEY_AREA_HEIGHT)
        };
        let y = scale_y(ROW_Y[row]) as i16;
        let height = scale_y(ROW_HEIGHT[row]) as u16;
        let scale_x = |value: u16| {
            u32::from(value) * u32::from(self.screen_width) / u32::from(REFERENCE_SCREEN_WIDTH)
        };
        let margin = (scale_x(reference_margin) as u16).min(self.screen_width / 3);
        let gap = (scale_x(KEY_GAP) as u16).max(1);
        let gaps = gap.saturating_mul(definitions.len().saturating_sub(1) as u16);
        let available = self
            .screen_width
            .saturating_sub(margin.saturating_mul(2))
            .saturating_sub(gaps);
        let total_weight = definitions
            .iter()
            .map(|definition| u32::from(definition.2))
            .sum::<u32>()
            .max(1);
        let mut x = margin;
        let mut remaining_width = available;
        let mut remaining_weight = total_weight;
        for (index, (label, action, weight)) in definitions.iter().copied().enumerate() {
            let width = if index + 1 == definitions.len() {
                remaining_width
            } else {
                (u32::from(remaining_width) * u32::from(weight) / remaining_weight) as u16
            };
            output.push(KeyboardKey {
                rect: KeyRect {
                    x: x as i16,
                    y,
                    width,
                    height,
                },
                label,
                action,
            });
            x = x.saturating_add(width).saturating_add(gap);
            remaining_width = remaining_width.saturating_sub(width);
            remaining_weight = remaining_weight.saturating_sub(u32::from(weight));
        }
    }
}

pub struct KeyboardSurface {
    pub window: Window,
    canvas: Canvas,
    presenter: Presenter,
    geometry: KeyboardGeometry,
    mapped: bool,
    popup: Option<KeyPopup>,
}

/// The enlarged key callout shown above a held character key. It is a
/// separate shaped window because top-row callouts rise above the keyboard
/// over the app.
pub struct KeyPopup {
    pub window: Window,
    canvas: Canvas,
    presenter: Presenter,
    shown: Option<(i16, i16, String)>,
}

const POPUP_MAX_WIDTH: u16 = 240;
const POPUP_MAX_HEIGHT: u16 = 360;
const POPUP_SIDE: f32 = 26.0;
const POPUP_GAP: f32 = 12.0;

impl KeyPopup {
    fn create<C: Connection>(conn: &C, root: Window, depth: u8) -> Result<Self, Box<dyn Error>> {
        let window = conn.generate_id()?;
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            POPUP_MAX_WIDTH,
            POPUP_MAX_HEIGHT,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new()
                .background_pixel(POPUP_KEY)
                .override_redirect(1)
                .event_mask(EventMask::EXPOSURE),
        )?;
        Ok(Self {
            window,
            canvas: Canvas::new(i32::from(POPUP_MAX_WIDTH), i32::from(POPUP_MAX_HEIGHT)),
            presenter: Presenter::create(conn, window, depth, POPUP_MAX_WIDTH, POPUP_MAX_HEIGHT)?,
            shown: None,
        })
    }

    fn hide<C: Connection>(&mut self, conn: &C) -> Result<(), Box<dyn Error>> {
        if self.shown.take().is_some() {
            conn.unmap_window(self.window)?;
        }
        Ok(())
    }

    /// Shows the callout for `key`, whose rectangle is in root coordinates.
    fn show<C: Connection>(
        &mut self,
        conn: &C,
        key: RectF,
        label: &str,
        screen_width: f32,
    ) -> Result<(), Box<dyn Error>> {
        let id = (key.x as i16, key.y as i16, label.to_owned());
        if self.shown.as_ref() == Some(&id) {
            return Ok(());
        }
        let bubble_w = (key.w + 2.0 * POPUP_SIDE).min(f32::from(POPUP_MAX_WIDTH));
        let bubble_h = (key.h * 1.12).round();
        let height = (bubble_h + POPUP_GAP + key.h).min(f32::from(POPUP_MAX_HEIGHT));
        let x0 = (key.x - POPUP_SIDE).clamp(0.0, (screen_width - bubble_w).max(0.0));
        let y0 = key.y + key.h - height;
        let local_key = RectF::new(key.x - x0, height - key.h, key.w, key.h);
        let mut path = Path::new();
        path.rounded_rect(RectF::new(0.0, 0.0, bubble_w, bubble_h), 24.0);
        path.polygon(&[
            (0.0, bubble_h - 26.0),
            (bubble_w, bubble_h - 26.0),
            (local_key.x + local_key.w, local_key.y + 14.0),
            (local_key.x, local_key.y + 14.0),
        ]);
        path.rounded_rect(local_key, KEY_RADIUS);
        self.canvas.clear(POPUP_KEY);
        text::draw_centered(
            &mut self.canvas,
            label,
            bubble_w / 2.0,
            if label.chars().all(|c| c.is_ascii_lowercase()) {
                bubble_h / 2.0 + 30.0
            } else {
                text::centered_baseline(bubble_h / 2.0 + 4.0, Weight::Regular, 96.0)
            },
            Weight::Regular,
            96.0,
            WHITE,
            1.0,
        );
        let spans = path
            .rasterize()
            .map(|(ox, oy, w, h, mask)| mask_spans(ox, oy, w, h, &mask))
            .unwrap_or_default();
        conn.configure_window(
            self.window,
            &ConfigureWindowAux::new()
                .x(x0.round() as i32)
                .y(y0.round() as i32)
                .width(bubble_w.round() as u32)
                .height(height.round() as u32)
                .stack_mode(StackMode::ABOVE),
        )?;
        conn.shape_rectangles(
            SO::SET,
            SK::BOUNDING,
            ClipOrdering::Y_SORTED,
            self.window,
            0,
            0,
            &spans,
        )?;
        self.presenter.present(conn, &self.canvas, 0)?;
        if self.shown.is_none() {
            conn.map_window(self.window)?;
        }
        self.presenter.expose(conn, 0)?;
        self.shown = Some(id);
        Ok(())
    }

    fn destroy<C: Connection>(&self, conn: &C) {
        let _ = conn.destroy_window(self.window);
        self.presenter.destroy(conn);
    }
}

/// Row spans of the pixels at least half covered by a mask.
fn mask_spans(ox: i32, oy: i32, width: i32, height: i32, mask: &[u8]) -> Vec<Rectangle> {
    let mut spans = Vec::new();
    for y in 0..height {
        let row = &mask[(y * width) as usize..((y + 1) * width) as usize];
        let mut x = 0;
        while x < width {
            if row[x as usize] < 128 {
                x += 1;
                continue;
            }
            let start = x;
            while x < width && row[x as usize] >= 128 {
                x += 1;
            }
            spans.push(Rectangle {
                x: (ox + start) as i16,
                y: (oy + y) as i16,
                width: (x - start) as u16,
                height: 1,
            });
        }
    }
    spans
}

impl KeyboardSurface {
    pub fn create<C: Connection>(
        conn: &C,
        root: Window,
        root_depth: u8,
        geometry: KeyboardGeometry,
        shape_available: bool,
    ) -> Result<Self, Box<dyn Error>> {
        let popup = if shape_available {
            Some(KeyPopup::create(conn, root, root_depth)?)
        } else {
            None
        };
        let window = conn.generate_id()?;
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            geometry.window_y,
            geometry.screen_width,
            geometry.window_height,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new()
                .background_pixel(KEYBOARD_BG)
                .override_redirect(1)
                .event_mask(EventMask::EXPOSURE | EventMask::STRUCTURE_NOTIFY),
        )?;
        conn.change_property8(
            PropMode::REPLACE,
            window,
            x11rb::protocol::xproto::AtomEnum::WM_NAME,
            x11rb::protocol::xproto::AtomEnum::STRING,
            b"moon-keyboard",
        )?;
        let presenter = Presenter::create(
            conn,
            window,
            root_depth,
            geometry.screen_width,
            geometry.window_height,
        )?;
        Ok(Self {
            window,
            canvas: Canvas::new(
                i32::from(geometry.screen_width),
                i32::from(geometry.window_height),
            ),
            presenter,
            geometry,
            mapped: false,
            popup,
        })
    }

    pub fn popup_window(&self) -> Option<Window> {
        self.popup.as_ref().map(|popup| popup.window)
    }

    fn sync_popup<C: Connection>(
        &mut self,
        conn: &C,
        state: &KeyboardState,
    ) -> Result<(), Box<dyn Error>> {
        let Some(popup) = self.popup.as_mut() else {
            return Ok(());
        };
        let keys = self.geometry.keys(state);
        let letter_width = keys
            .iter()
            .find(|key| matches!(key.action, KeyAction::Character(c) if c.is_ascii_alphanumeric()))
            .map(|key| key.rect.width)
            .unwrap_or(0);
        let target = state
            .latest_pressed()
            .filter(|_| self.mapped && state.layout() != KeyboardLayout::NumberPad)
            .and_then(|index| keys.get(index))
            .filter(|key| {
                matches!(key.action, KeyAction::Character(_))
                    && u32::from(key.rect.width) * 4 <= u32::from(letter_width) * 5
            });
        match target {
            Some(key) => {
                let mut rect = key.rect.rect_f();
                rect.y += f32::from(self.geometry.window_y);
                let label = display_label(key, state);
                popup.show(conn, rect, &label, f32::from(self.geometry.screen_width))
            }
            None => popup.hide(conn),
        }
    }

    pub fn sync<C: Connection>(
        &mut self,
        conn: &C,
        state: &mut KeyboardState,
        active_app: Option<Window>,
    ) -> Result<(), Box<dyn Error>> {
        let should_map = state.is_visible() && active_app.is_some();
        if !should_map {
            if let Some(popup) = self.popup.as_mut() {
                popup.hide(conn)?;
            }
            if self.mapped {
                conn.unmap_window(self.window)?;
                self.mapped = false;
                if let Some(window) = active_app {
                    conn.set_input_focus(InputFocus::PARENT, window, CURRENT_TIME)?;
                }
            }
            return Ok(());
        }

        if state.needs_redraw() || !self.mapped {
            self.render(state);
            self.presenter.present(conn, &self.canvas, 0)?;
            // Exposures (mapping, restacking) are repaired from the pixmap.
            self.presenter.expose(conn, 0)?;
            conn.flush()?;
            state.clear_redraw();
        }
        if !self.mapped {
            conn.configure_window(
                self.window,
                &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
            )?;
            conn.map_window(self.window)?;
            self.mapped = true;
            state.clear_raise();
        } else if state.raise_pending() {
            conn.configure_window(
                self.window,
                &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
            )?;
            if let Some(popup) = self.popup.as_ref().filter(|popup| popup.shown.is_some()) {
                conn.configure_window(
                    popup.window,
                    &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
                )?;
            }
            state.clear_raise();
        }
        self.sync_popup(conn, state)
    }

    pub fn destroy<C: Connection>(&self, conn: &C) {
        if let Some(popup) = &self.popup {
            popup.destroy(conn);
        }
        let _ = conn.destroy_window(self.window);
        self.presenter.destroy(conn);
    }

    fn render(&mut self, state: &KeyboardState) {
        let canvas = &mut self.canvas;
        canvas.clear(KEYBOARD_BG);
        for (index, key) in self.geometry.keys(state).into_iter().enumerate() {
            draw_key(canvas, &key, state, state.is_key_pressed(index));
        }
        // Home indicator in the key-free bottom zone, where the global
        // swipe-up gesture starts.
        let width = f32::from(self.geometry.screen_width);
        let height = f32::from(self.geometry.window_height);
        canvas.fill_rrect(
            RectF::new(width / 2.0 - 185.0, height - 36.0, 370.0, 14.0),
            7.0,
            WHITE,
            0.9,
        );
    }
}

fn key_style(key: &KeyboardKey, state: &KeyboardState) -> (Rgb, bool) {
    let purpose = state.purpose().unwrap_or(KeyboardPurpose::Text);
    match key.action {
        KeyAction::Character(_) | KeyAction::Space => (LETTER_KEY, true),
        KeyAction::Enter if purpose != KeyboardPurpose::Text => (BLUE, false),
        KeyAction::Shift if state.shift() => (WHITE, false),
        _ if state.layout() == KeyboardLayout::NumberPad => (LETTER_KEY, true),
        _ => (FUNCTION_KEY, false),
    }
}

fn draw_key(canvas: &mut Canvas, key: &KeyboardKey, state: &KeyboardState, pressed: bool) {
    let rect = key.rect.rect_f();
    let (base, is_letter) = key_style(key, state);
    let fill = match (pressed, is_letter, base) {
        (true, true, _) => PRESSED_LETTER_KEY,
        (true, false, BLUE) => 0x0060df,
        (true, false, WHITE) => 0xc7c7cc,
        (true, false, _) => LETTER_KEY,
        (false, _, color) => color,
    };
    canvas.fill_rrect(
        RectF::new(rect.x, rect.y + 3.0, rect.w, rect.h),
        KEY_RADIUS,
        0x000000,
        0.55,
    );
    canvas.fill_rrect(rect, KEY_RADIUS, fill, 1.0);
    let (cx, cy) = rect.center();
    let ink = if fill == WHITE || fill == 0xc7c7cc {
        0x000000
    } else {
        WHITE
    };
    match key.action {
        KeyAction::Shift => draw_shift(canvas, cx, cy, state.shift(), ink),
        KeyAction::Backspace => draw_delete(canvas, cx, cy, ink),
        _ => {
            let label = display_label(key, state);
            let single = label.chars().count() == 1;
            let size = if single {
                if label.chars().all(|c| c.is_ascii_alphabetic()) {
                    66.0
                } else {
                    62.0
                }
            } else {
                44.0
            };
            // Lowercase letters center on their x-height, capitals and
            // symbols on the cap height.
            let lowercase = single && label.chars().all(|c| c.is_ascii_lowercase());
            let baseline = if lowercase {
                cy + size * 0.27
            } else {
                text::centered_baseline(cy, Weight::Regular, size)
            };
            text::draw_centered(
                canvas,
                &label,
                cx,
                baseline,
                Weight::Regular,
                size,
                ink,
                1.0,
            );
        }
    }
}

fn display_label(key: &KeyboardKey, state: &KeyboardState) -> String {
    match key.action {
        KeyAction::Character(c) if c.is_ascii_alphabetic() => {
            if state.shift() {
                c.to_ascii_uppercase().to_string()
            } else {
                c.to_string()
            }
        }
        KeyAction::Space => "space".into(),
        KeyAction::Enter => key.label.to_ascii_lowercase(),
        _ => key.label.to_owned(),
    }
}

fn draw_shift(canvas: &mut Canvas, cx: f32, cy: f32, active: bool, ink: Rgb) {
    let points = [
        (cx, cy - 27.0),
        (cx + 26.0, cy + 1.0),
        (cx + 12.0, cy + 1.0),
        (cx + 12.0, cy + 24.0),
        (cx - 12.0, cy + 24.0),
        (cx - 12.0, cy + 1.0),
        (cx - 26.0, cy + 1.0),
    ];
    let mut path = Path::new();
    if active {
        path.polygon(&points);
    } else {
        let mut outline = points.to_vec();
        outline.push(points[0]);
        path.stroke_polyline(&outline, 4.5);
    }
    canvas.fill_path(&path, ink, 1.0);
}

fn draw_delete(canvas: &mut Canvas, cx: f32, cy: f32, ink: Rgb) {
    let outline = [
        (cx - 34.0, cy),
        (cx - 15.0, cy - 22.0),
        (cx + 32.0, cy - 22.0),
        (cx + 32.0, cy + 22.0),
        (cx - 15.0, cy + 22.0),
        (cx - 34.0, cy),
    ];
    let mut path = Path::new();
    path.stroke_polyline(&outline, 4.5);
    let (x, y, r) = (cx + 9.0, cy, 9.0);
    path.stroke_segment((x - r, y - r), (x + r, y + r), 4.5);
    path.stroke_segment((x - r, y + r), (x + r, y - r), 4.5);
    canvas.fill_path(&path, ink, 1.0);
}

pub struct XtestInjector {
    root: Window,
    mapping: Option<ServerKeyboardMapping>,
}

impl XtestInjector {
    pub fn query<C: Connection>(
        conn: &C,
        root: Window,
        min_keycode: Keycode,
        max_keycode: Keycode,
    ) -> Result<Self, Box<dyn Error>> {
        let mapping = if conn
            .extension_information(xtest::X11_EXTENSION_NAME)?
            .is_some()
        {
            let version = conn.xtest_get_version(2, 2)?.reply()?;
            eprintln!(
                "XTEST keyboard injection ready version={}.{}",
                version.major_version, version.minor_version
            );
            Some(ServerKeyboardMapping::query(
                conn,
                min_keycode,
                max_keycode,
            )?)
        } else {
            eprintln!("XTEST unavailable; on-screen keyboard input is disabled");
            None
        };
        Ok(Self { root, mapping })
    }

    pub fn inject<C: Connection>(
        &self,
        conn: &C,
        input: KeyboardInput,
        primary_app: Window,
        focus_within_active_app: bool,
    ) -> Result<(), Box<dyn Error>> {
        let mapping = self.mapping.as_ref().ok_or("XTEST is unavailable")?;
        if primary_app == 0 || !focus_within_active_app {
            return Err("X focus is outside the active app".into());
        }
        // Never perform a synchronous X11 round trip from the input event loop.
        // Moon's long-lived rendering connection can issue more than one 16-bit
        // X11 sequence epoch of unchecked drawing/XTEST requests. A later
        // synchronous focus query can then wait forever if the wrapped reply is
        // associated with the wrong epoch. That exact wait previously froze the
        // entire shell while a user was typing.
        //
        // Focus provenance is maintained from FocusIn/FocusOut events selected
        // on Moon's managed app windows. Never move focus here: CEF descendants
        // and application transients must retain their exact editable target.
        // Queue the complete sequence without checked cookies. Any protocol error
        // remains asynchronous and is handled by the ordinary X event loop; input
        // can no longer block shell IPC, touch, power, or volume handling.
        let keysym = match input {
            KeyboardInput::Character(character) if character.is_ascii() => character as Keysym,
            KeyboardInput::Character(_) => {
                return Err("non-ASCII keyboard input is unsupported".into());
            }
            KeyboardInput::Backspace => XK_BACK_SPACE,
            KeyboardInput::Enter => XK_RETURN,
        };
        let resolved = mapping
            .resolve(keysym)
            .ok_or("keysym is absent from the X server keyboard mapping")?;

        let mut modifiers = [None, None];
        if resolved.level3 {
            modifiers[0] = mapping.level3_keycode;
        }
        if resolved.shift {
            modifiers[1] = mapping.shift_keycode;
        }
        if (resolved.level3 && modifiers[0].is_none()) || (resolved.shift && modifiers[1].is_none())
        {
            return Err("required modifier is absent from the X server mapping".into());
        }

        let modifiers: Vec<Keycode> = modifiers.into_iter().flatten().collect();
        for keycode in modifiers.iter().copied() {
            self.fake_key_unchecked(conn, KEY_PRESS_EVENT, keycode)?;
        }
        self.fake_key_unchecked(conn, KEY_PRESS_EVENT, resolved.keycode)?;
        self.fake_key_unchecked(conn, KEY_RELEASE_EVENT, resolved.keycode)?;
        for keycode in modifiers.iter().copied().rev() {
            self.fake_key_unchecked(conn, KEY_RELEASE_EVENT, keycode)?;
        }
        conn.flush()?;
        Ok(())
    }

    /// Emit a bounded repeat batch without a checked round trip or artificial
    /// key-release delay for every event. Focus provenance is still validated
    /// once for the complete batch. Asynchronous X errors are handled by the
    /// shell's ordinary X event loop.
    pub fn inject_repeats<C: Connection>(
        &self,
        conn: &C,
        input: KeyboardInput,
        primary_app: Window,
        focus_within_active_app: bool,
        count: usize,
    ) -> Result<(), Box<dyn Error>> {
        if count == 0 {
            return Ok(());
        }
        let mapping = self.mapping.as_ref().ok_or("XTEST is unavailable")?;
        if primary_app == 0 || !focus_within_active_app {
            return Err("X focus is outside the active app".into());
        }
        let keysym = match input {
            KeyboardInput::Character(character) if character.is_ascii() => character as Keysym,
            KeyboardInput::Character(_) => {
                return Err("non-ASCII keyboard input is unsupported".into());
            }
            KeyboardInput::Backspace => XK_BACK_SPACE,
            KeyboardInput::Enter => XK_RETURN,
        };
        let resolved = mapping
            .resolve(keysym)
            .ok_or("keysym is absent from the X server keyboard mapping")?;

        let mut modifiers = [None, None];
        if resolved.level3 {
            modifiers[0] = mapping.level3_keycode;
        }
        if resolved.shift {
            modifiers[1] = mapping.shift_keycode;
        }
        if (resolved.level3 && modifiers[0].is_none()) || (resolved.shift && modifiers[1].is_none())
        {
            return Err("required modifier is absent from the X server mapping".into());
        }
        let modifiers: Vec<Keycode> = modifiers.into_iter().flatten().collect();
        for _ in 0..count {
            for keycode in modifiers.iter().copied() {
                self.fake_key_unchecked(conn, KEY_PRESS_EVENT, keycode)?;
            }
            self.fake_key_unchecked(conn, KEY_PRESS_EVENT, resolved.keycode)?;
            self.fake_key_unchecked(conn, KEY_RELEASE_EVENT, resolved.keycode)?;
            for keycode in modifiers.iter().copied().rev() {
                self.fake_key_unchecked(conn, KEY_RELEASE_EVENT, keycode)?;
            }
        }
        conn.flush()?;
        Ok(())
    }

    fn fake_key_unchecked<C: Connection>(
        &self,
        conn: &C,
        event_type: u8,
        keycode: Keycode,
    ) -> Result<(), Box<dyn Error>> {
        let _ = conn.xtest_fake_input(event_type, keycode, CURRENT_TIME, self.root, 0, 0, 0)?;
        Ok(())
    }
}

struct ServerKeyboardMapping {
    min_keycode: Keycode,
    keysyms_per_keycode: usize,
    keysyms: Vec<Keysym>,
    shift_keycode: Option<Keycode>,
    level3_keycode: Option<Keycode>,
}

impl ServerKeyboardMapping {
    fn query<C: Connection>(
        conn: &C,
        min_keycode: Keycode,
        max_keycode: Keycode,
    ) -> Result<Self, Box<dyn Error>> {
        let count = u16::from(max_keycode)
            .saturating_sub(u16::from(min_keycode))
            .saturating_add(1);
        let count = u8::try_from(count).map_err(|_| "invalid X keyboard keycode range")?;
        let keyboard = conn.get_keyboard_mapping(min_keycode, count)?.reply()?;
        let modifiers = conn.get_modifier_mapping()?.reply()?;
        let mut mapping = Self {
            min_keycode,
            keysyms_per_keycode: usize::from(keyboard.keysyms_per_keycode),
            keysyms: keyboard.keysyms,
            shift_keycode: None,
            level3_keycode: None,
        };

        let keycodes_per_modifier = modifiers.keycodes.len() / 8;
        let shift_candidates = modifiers.keycodes.iter().take(keycodes_per_modifier);
        mapping.shift_keycode = shift_candidates
            .clone()
            .copied()
            .find(|keycode| {
                *keycode != 0 && mapping.keycode_has_any(*keycode, &[XK_SHIFT_L, XK_SHIFT_R])
            })
            .or_else(|| shift_candidates.copied().find(|keycode| *keycode != 0));
        mapping.level3_keycode = modifiers.keycodes.iter().copied().find(|keycode| {
            *keycode != 0
                && mapping.keycode_has_any(*keycode, &[XK_MODE_SWITCH, XK_ISO_LEVEL3_SHIFT])
        });
        Ok(mapping)
    }

    fn resolve(&self, wanted: Keysym) -> Option<ResolvedKey> {
        let mut best: Option<(u8, ResolvedKey)> = None;
        if self.keysyms_per_keycode == 0 {
            return None;
        }
        for (key_index, symbols) in self.keysyms.chunks(self.keysyms_per_keycode).enumerate() {
            let keycode = u16::from(self.min_keycode).saturating_add(key_index as u16);
            let Ok(keycode) = u8::try_from(keycode) else {
                continue;
            };
            for (level, keysym) in symbols.iter().copied().take(4).enumerate() {
                if keysym != wanted {
                    continue;
                }
                let resolved = ResolvedKey {
                    keycode,
                    shift: level % 2 == 1,
                    level3: level >= 2,
                };
                let score = u8::from(resolved.shift) + 2 * u8::from(resolved.level3);
                if best.is_none_or(|(best_score, _)| score < best_score) {
                    best = Some((score, resolved));
                }
            }
        }
        best.map(|(_, resolved)| resolved)
    }

    fn keycode_has_any(&self, keycode: Keycode, wanted: &[Keysym]) -> bool {
        let Some(index) = usize::from(keycode)
            .checked_sub(usize::from(self.min_keycode))
            .and_then(|index| index.checked_mul(self.keysyms_per_keycode))
        else {
            return false;
        };
        self.keysyms
            .get(index..index.saturating_add(self.keysyms_per_keycode))
            .is_some_and(|symbols| symbols.iter().any(|symbol| wanted.contains(symbol)))
    }
}

#[derive(Clone, Copy)]
struct ResolvedKey {
    keycode: Keycode,
    shift: bool,
    level3: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn visible(purpose: KeyboardPurpose) -> KeyboardState {
        let mut state = KeyboardState::default();
        state.show(purpose);
        state
    }

    #[test]
    fn hit_testing_uses_the_rendered_key_rectangles() {
        let geometry = KeyboardGeometry::new(1080, 2340);
        let state = visible(KeyboardPurpose::Url);
        for key in geometry.keys(&state) {
            let root_x = key.rect.x + (key.rect.width / 2) as i16;
            let root_y = geometry.window_y + key.rect.y + (key.rect.height / 2) as i16;
            assert_eq!(geometry.action_at(&state, root_x, root_y), Some(key.action));
        }
    }

    #[test]
    fn close_start_zone_contains_no_keys_and_is_not_consumed() {
        let geometry = KeyboardGeometry::new(1080, 2340);
        let state = visible(KeyboardPurpose::Text);
        let close_zone_y = 2340 - CLOSE_START_ZONE_HEIGHT as i16;
        assert!(
            geometry
                .keys(&state)
                .iter()
                .all(|key| i32::from(geometry.window_y)
                    + i32::from(key.rect.y)
                    + i32::from(key.rect.height)
                    <= i32::from(close_zone_y))
        );
        assert!(!geometry.owns_touch(&state, 540, close_zone_y));
        assert_eq!(geometry.action_at(&state, 540, 2339), None);
    }

    #[test]
    fn url_layout_has_contextual_submit_and_required_punctuation() {
        let geometry = KeyboardGeometry::new(1080, 2340);
        let mut state = visible(KeyboardPurpose::Url);
        let mut keys = geometry.keys(&state);
        assert!(
            keys.iter()
                .any(|key| key.label == "GO" && key.action == KeyAction::Enter)
        );
        assert!(
            keys.iter()
                .any(|key| key.action == KeyAction::Character('.'))
        );
        state.activate(KeyAction::SwitchLayout(KeyboardLayout::Numbers));
        keys.extend(geometry.keys(&state));
        state.activate(KeyAction::SwitchLayout(KeyboardLayout::Symbols));
        keys.extend(geometry.keys(&state));
        for character in ['.', '/', ':', '-', '_'] {
            assert!(
                keys.iter()
                    .any(|key| key.action == KeyAction::Character(character))
            );
        }
    }

    #[test]
    fn submit_labels_match_the_requested_purpose() {
        let geometry = KeyboardGeometry::new(1080, 2340);
        for (purpose, expected) in [
            (KeyboardPurpose::Text, "RETURN"),
            (KeyboardPurpose::Url, "GO"),
            (KeyboardPurpose::Search, "SEARCH"),
            (KeyboardPurpose::Password, "DONE"),
        ] {
            let state = visible(purpose);
            assert!(
                geometry
                    .keys(&state)
                    .iter()
                    .any(|key| key.action == KeyAction::Enter && key.label == expected),
                "missing submit label for {purpose:?}"
            );
        }
    }

    #[test]
    fn shift_is_one_shot_and_number_starts_on_number_pad() {
        let mut state = visible(KeyboardPurpose::Text);
        assert!(matches!(
            state.activate(KeyAction::Shift),
            KeyboardEffect::None
        ));
        assert!(state.shift());
        assert!(matches!(
            state.activate(KeyAction::Character('a')),
            KeyboardEffect::Inject(KeyboardInput::Character('A'))
        ));
        assert!(!state.shift());

        state.show(KeyboardPurpose::Number);
        assert_eq!(state.layout(), KeyboardLayout::NumberPad);
        state.activate(KeyAction::SwitchLayout(KeyboardLayout::Letters));
        assert_eq!(state.layout(), KeyboardLayout::NumberPad);
    }

    #[test]
    fn pressed_key_state_is_private_and_requests_a_repaint() {
        let mut state = visible(KeyboardPurpose::Password);
        state.clear_redraw();
        state.press_key_index(4);
        state.press_key_index(5);
        assert!(state.is_key_pressed(4));
        assert!(state.is_key_pressed(5));
        assert!(state.needs_redraw());

        state.clear_redraw();
        state.release_key_index(4);
        assert!(!state.is_key_pressed(4));
        assert!(state.is_key_pressed(5));
        state.release_key_index(5);
        assert!(!state.is_key_pressed(5));
        assert!(state.needs_redraw());
        let public = serde_json::to_value(state.public()).unwrap();
        assert!(public.get("pressed_key_index").is_none());
    }

    #[test]
    fn only_text_producing_keys_repeat() {
        for action in [
            KeyAction::Character('a'),
            KeyAction::Space,
            KeyAction::Backspace,
        ] {
            assert!(action.is_repeatable());
        }
        for action in [
            KeyAction::Shift,
            KeyAction::SwitchLayout(KeyboardLayout::Numbers),
            KeyAction::Enter,
        ] {
            assert!(!action.is_repeatable());
        }
    }

    #[test]
    fn letter_rows_follow_iphone_us_portrait_stagger() {
        let geometry = KeyboardGeometry::new(1080, 2340);
        let state = visible(KeyboardPurpose::Text);
        let keys = geometry.keys(&state);
        let rows: Vec<Vec<&KeyboardKey>> = ROW_Y
            .iter()
            .map(|y| keys.iter().filter(|key| key.rect.y == *y as i16).collect())
            .collect();

        assert_eq!(
            rows[0].iter().map(|key| key.label).collect::<String>(),
            "QWERTYUIOP"
        );
        assert_eq!(
            rows[1].iter().map(|key| key.label).collect::<String>(),
            "ASDFGHJKL"
        );
        assert_eq!(rows[2].len(), 9);
        assert_eq!(rows[2][0].label, "SHIFT");
        assert_eq!(rows[2][8].label, "DEL");
        assert_eq!(
            rows[3].iter().map(|key| key.label).collect::<Vec<_>>(),
            ["123", "SPACE", "RETURN"]
        );
        assert!(rows[1][0].rect.x > rows[0][0].rect.x);
        assert!(rows[2][0].rect.width > rows[2][1].rect.width);
        assert!(rows[2][8].rect.width > rows[2][7].rect.width);
    }

    #[test]
    fn number_and_symbol_pages_follow_iphone_key_order() {
        let geometry = KeyboardGeometry::new(1080, 2340);
        let mut state = visible(KeyboardPurpose::Text);
        state.activate(KeyAction::SwitchLayout(KeyboardLayout::Numbers));
        let numbers = geometry.keys(&state);
        assert_eq!(
            numbers
                .iter()
                .take(10)
                .map(|key| key.label)
                .collect::<String>(),
            "1234567890"
        );
        assert!(numbers.iter().any(|key| {
            key.label == "#+=" && key.action == KeyAction::SwitchLayout(KeyboardLayout::Symbols)
        }));

        state.activate(KeyAction::SwitchLayout(KeyboardLayout::Symbols));
        let symbols = geometry.keys(&state);
        assert_eq!(
            symbols
                .iter()
                .take(10)
                .map(|key| key.label)
                .collect::<String>(),
            "[]{}#%^*+="
        );
        assert!(symbols.iter().any(|key| {
            key.label == "123" && key.action == KeyAction::SwitchLayout(KeyboardLayout::Numbers)
        }));
    }

    #[test]
    fn server_mapping_selects_keycodes_and_shift_by_keysym_column() {
        let mapping = ServerKeyboardMapping {
            min_keycode: 8,
            keysyms_per_keycode: 2,
            keysyms: vec!['a' as Keysym, 'A' as Keysym, '1' as Keysym, '!' as Keysym],
            shift_keycode: Some(50),
            level3_keycode: None,
        };
        let lower = mapping.resolve('a' as Keysym).unwrap();
        assert_eq!(lower.keycode, 8);
        assert!(!lower.shift);
        let upper = mapping.resolve('A' as Keysym).unwrap();
        assert_eq!(upper.keycode, 8);
        assert!(upper.shift);
        let symbol = mapping.resolve('!' as Keysym).unwrap();
        assert_eq!(symbol.keycode, 9);
        assert!(symbol.shift);
    }
}
