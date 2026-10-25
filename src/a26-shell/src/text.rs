//! Anti-aliased text using the embedded Inter typeface (SIL OFL 1.1, see
//! `assets/fonts/OFL.txt`). Glyph coverage masks are rasterized once per
//! weight/size/character and cached for the life of the process.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use fontdue::{Font, FontSettings};

use crate::gfx::{Canvas, Rgb};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Weight {
    Regular,
    Medium,
    SemiBold,
    Bold,
    /// Optical "Display" cuts are drawn tighter for large sizes.
    DisplayLight,
    DisplayMedium,
    DisplaySemiBold,
}

const WEIGHTS: [Weight; 7] = [
    Weight::Regular,
    Weight::Medium,
    Weight::SemiBold,
    Weight::Bold,
    Weight::DisplayLight,
    Weight::DisplayMedium,
    Weight::DisplaySemiBold,
];

fn font_bytes(weight: Weight) -> &'static [u8] {
    match weight {
        Weight::Regular => include_bytes!("../assets/fonts/Inter-Regular.ttf"),
        Weight::Medium => include_bytes!("../assets/fonts/Inter-Medium.ttf"),
        Weight::SemiBold => include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
        Weight::Bold => include_bytes!("../assets/fonts/Inter-Bold.ttf"),
        Weight::DisplayLight => include_bytes!("../assets/fonts/InterDisplay-Light.ttf"),
        Weight::DisplayMedium => include_bytes!("../assets/fonts/InterDisplay-Medium.ttf"),
        Weight::DisplaySemiBold => include_bytes!("../assets/fonts/InterDisplay-SemiBold.ttf"),
    }
}

struct Glyph {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    advance: f32,
    mask: Vec<u8>,
}

struct Typefaces {
    fonts: Vec<Font>,
    glyphs: HashMap<(Weight, u16, char), Rc<Glyph>>,
    gamma: [u8; 256],
}

thread_local! {
    static TYPEFACES: RefCell<Typefaces> = RefCell::new(Typefaces::load());
}

impl Typefaces {
    fn load() -> Self {
        let fonts = WEIGHTS
            .iter()
            .map(|weight| {
                Font::from_bytes(font_bytes(*weight), FontSettings::default())
                    .expect("embedded Inter font is valid")
            })
            .collect();
        // Light text on Moon's dark surfaces reads slightly thin with linear
        // coverage; a mild curve restores the designed stroke weight.
        let mut gamma = [0_u8; 256];
        for (index, value) in gamma.iter_mut().enumerate() {
            *value = ((index as f32 / 255.0).powf(0.84) * 255.0 + 0.5) as u8;
        }
        Self {
            fonts,
            glyphs: HashMap::new(),
            gamma,
        }
    }

    fn glyph(&mut self, weight: Weight, size: f32, character: char) -> Rc<Glyph> {
        let key = (weight, (size * 2.0).round() as u16, character);
        if let Some(glyph) = self.glyphs.get(&key) {
            return glyph.clone();
        }
        let index = WEIGHTS.iter().position(|w| *w == weight).unwrap_or(0);
        let font = &self.fonts[index];
        let character = if font.lookup_glyph_index(character) == 0 && character != ' ' {
            '?'
        } else {
            character
        };
        let (metrics, mut mask) = font.rasterize(character, f32::from(key.1) / 2.0);
        for value in &mut mask {
            *value = self.gamma[usize::from(*value)];
        }
        let glyph = Rc::new(Glyph {
            x: metrics.xmin,
            y: -(metrics.height as i32) - metrics.ymin,
            width: metrics.width as i32,
            height: metrics.height as i32,
            advance: metrics.advance_width,
            mask,
        });
        self.glyphs.insert(key, glyph.clone());
        glyph
    }
}

/// Typographic advance width of `text`, including `tracking` between glyphs.
pub fn measure_tracked(text: &str, weight: Weight, size: f32, tracking: f32) -> f32 {
    TYPEFACES.with(|cell| {
        let mut faces = cell.borrow_mut();
        let mut width = 0.0;
        let mut count = 0;
        for character in text.chars() {
            width += faces.glyph(weight, size, character).advance;
            count += 1;
        }
        width + tracking * (count.max(1) - 1) as f32
    })
}

pub fn measure(text: &str, weight: Weight, size: f32) -> f32 {
    measure_tracked(text, weight, size, 0.0)
}

/// Draws `text` with its alphabetic baseline at `baseline`; returns its width.
#[allow(clippy::too_many_arguments)]
pub fn draw_tracked(
    canvas: &mut Canvas,
    text: &str,
    x: f32,
    baseline: f32,
    weight: Weight,
    size: f32,
    tracking: f32,
    color: Rgb,
    alpha: f32,
) -> f32 {
    TYPEFACES.with(|cell| {
        let mut faces = cell.borrow_mut();
        let mut pen = x;
        for character in text.chars() {
            let glyph = faces.glyph(weight, size, character);
            if glyph.width > 0 && glyph.height > 0 {
                canvas.draw_mask(
                    pen.round() as i32 + glyph.x,
                    baseline.round() as i32 + glyph.y,
                    glyph.width,
                    glyph.height,
                    &glyph.mask,
                    color,
                    alpha,
                );
            }
            pen += glyph.advance + tracking;
        }
        pen - tracking - x
    })
}

#[allow(clippy::too_many_arguments)]
pub fn draw(
    canvas: &mut Canvas,
    text: &str,
    x: f32,
    baseline: f32,
    weight: Weight,
    size: f32,
    color: Rgb,
    alpha: f32,
) -> f32 {
    draw_tracked(canvas, text, x, baseline, weight, size, 0.0, color, alpha)
}

#[allow(clippy::too_many_arguments)]
pub fn draw_centered(
    canvas: &mut Canvas,
    text: &str,
    center_x: f32,
    baseline: f32,
    weight: Weight,
    size: f32,
    color: Rgb,
    alpha: f32,
) {
    let width = measure(text, weight, size);
    draw(
        canvas,
        text,
        center_x - width / 2.0,
        baseline,
        weight,
        size,
        color,
        alpha,
    );
}

#[allow(clippy::too_many_arguments)]
pub fn draw_right(
    canvas: &mut Canvas,
    text: &str,
    right_x: f32,
    baseline: f32,
    weight: Weight,
    size: f32,
    color: Rgb,
    alpha: f32,
) {
    let width = measure(text, weight, size);
    draw(
        canvas,
        text,
        right_x - width,
        baseline,
        weight,
        size,
        color,
        alpha,
    );
}

/// Height of capital letters above the baseline; used to optically center
/// labels inside controls.
pub fn cap_height(weight: Weight, size: f32) -> f32 {
    TYPEFACES.with(|cell| cell.borrow_mut().glyph(weight, size, 'H').height as f32)
}

/// Baseline that vertically centers capitals around `center_y`.
pub fn centered_baseline(center_y: f32, weight: Weight, size: f32) -> f32 {
    center_y + cap_height(weight, size) / 2.0
}

/// Shortens `text` with an ellipsis so it fits in `max_width`.
pub fn fit(text: &str, weight: Weight, size: f32, max_width: f32) -> String {
    if measure(text, weight, size) <= max_width {
        return text.to_owned();
    }
    let mut characters: Vec<char> = text.chars().collect();
    while !characters.is_empty() {
        characters.pop();
        let candidate: String = characters.iter().collect::<String>() + "…";
        if measure(&candidate, weight, size) <= max_width {
            return candidate;
        }
    }
    "…".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_fonts_measure_and_render() {
        for weight in WEIGHTS {
            assert!(measure("Moon", weight, 40.0) > 60.0);
            assert!(cap_height(weight, 40.0) > 20.0);
        }
        let mut canvas = Canvas::new(200, 80);
        let width = draw(
            &mut canvas,
            "Hi",
            10.0,
            60.0,
            Weight::SemiBold,
            48.0,
            0xffffff,
            1.0,
        );
        assert!(width > 30.0);
        assert!(canvas.pixels.iter().any(|pixel| *pixel == 0xffffff));
    }

    #[test]
    fn fit_truncates_with_an_ellipsis() {
        let long = "An extremely long network name that will not fit";
        let fitted = fit(long, Weight::Regular, 40.0, 300.0);
        assert!(fitted.ends_with('…'));
        assert!(measure(&fitted, Weight::Regular, 40.0) <= 300.0);
        assert_eq!(fit("Short", Weight::Regular, 40.0, 300.0), "Short");
    }
}
