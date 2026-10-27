//! Anti-aliased text using the embedded Inter typeface (SIL OFL 1.1, see
//! `assets/fonts/OFL.txt`), with fallbacks for everything Inter lacks so a
//! message never shows a '?': Noto Sans (Latin extended, Greek, Cyrillic,
//! Vietnamese), Noto Sans Symbols 2, Symbols and Math, and Noto Color Emoji (colour
//! bitmaps; ZWJ sequences, skin tones, keycaps and flags through its
//! ligatures). The fallbacks are files in `FONT_DIR` (`MOON_FONTS` overrides
//! it), memory-mapped the first time a character needs them, so they cost
//! nothing until then. A character no font has draws as an empty box.
//! Glyphs are rasterized once per weight/size/character and cached for the
//! life of the process.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use fontdue::{Font, FontSettings};
use ttf_parser::{Face, GlyphId, OutlineBuilder, RasterImageFormat};

use crate::gfx::{Canvas, Image, Path, Rgb};

pub const FONT_DIR: &str = "/opt/a26-shell/share/fonts";

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

/// The fallback files, in the order they are tried.
const NOTO_REGULAR: usize = 0;
const NOTO_SEMIBOLD: usize = 1;
const SYMBOLS_2: usize = 2;
const SYMBOLS: usize = 3;
const MATH: usize = 4;
const EMOJI: usize = 5;
const FALLBACK_FILES: [&str; 6] = [
    "NotoSans-Regular.ttf",
    "NotoSans-SemiBold.ttf",
    "NotoSansSymbols2-Regular.ttf",
    "NotoSansSymbols-Regular.ttf",
    "NotoSansMath-Regular.ttf",
    "NotoColorEmoji.ttf",
];

enum Shape {
    /// Coverage, drawn in the text's colour.
    Mask(Vec<u8>),
    /// Premultiplied ARGB (emoji), drawn as is.
    Color(Image),
}

struct Glyph {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    advance: f32,
    shape: Shape,
}

impl Glyph {
    fn empty(advance: f32) -> Self {
        Self {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            advance,
            shape: Shape::Mask(Vec::new()),
        }
    }
}

/// What one glyph stands for: a character, or an emoji sequence.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Cluster {
    Char(char),
    Emoji(Box<[char]>),
}

enum Fallback {
    Unloaded,
    Missing,
    Loaded(Box<Face<'static>>),
}

struct Typefaces {
    fonts: Vec<Font>,
    fallbacks: Vec<Fallback>,
    glyphs: HashMap<(Weight, u16, Cluster), Rc<Glyph>>,
    gamma: [u8; 256],
}

thread_local! {
    static TYPEFACES: RefCell<Typefaces> = RefCell::new(Typefaces::load());
}

fn font_dir() -> std::path::PathBuf {
    std::env::var_os("MOON_FONTS")
        .map(Into::into)
        .unwrap_or_else(|| FONT_DIR.into())
}

/// Maps a font file read-only for the life of the process; its pages load
/// only as glyphs are used.
fn map_file(path: &std::path::Path) -> Option<&'static [u8]> {
    use std::os::fd::AsRawFd;
    let file = std::fs::File::open(path).ok()?;
    let length = usize::try_from(file.metadata().ok()?.len()).ok()?;
    if length == 0 {
        return None;
    }
    // SAFETY: a private read-only mapping of a regular file, never unmapped,
    // so the slice stays valid for 'static. The fonts are installed files
    // nobody truncates while Moon runs.
    unsafe {
        let address = libc::mmap(
            std::ptr::null_mut(),
            length,
            libc::PROT_READ,
            libc::MAP_PRIVATE,
            file.as_raw_fd(),
            0,
        );
        if address == libc::MAP_FAILED {
            return None;
        }
        Some(std::slice::from_raw_parts(address.cast::<u8>(), length))
    }
}

/// Characters that take no room of their own (joiners, selectors, tags).
fn invisible(character: char) -> bool {
    matches!(
        u32::from(character),
        0..=0x1f
            | 0x7f..=0x9f
            | 0x200b..=0x200f
            | 0x2028..=0x202e
            | 0x2060..=0x2064
            | 0xfe00..=0xfe0f
            | 0xfeff
            | 0xe0000..=0xe007f
    )
}

fn regional_indicator(character: char) -> bool {
    matches!(u32::from(character), 0x1f1e6..=0x1f1ff)
}

/// Code points that start an emoji sequence (pictographs, dingbats,
/// arrows and symbols with an emoji form, regional indicators).
fn pictographic(character: char) -> bool {
    matches!(
        u32::from(character),
        0xa9 | 0xae
            | 0x203c
            | 0x2049
            | 0x2122
            | 0x2139
            | 0x2194..=0x21aa
            | 0x231a..=0x23ff
            | 0x24c2
            | 0x25aa..=0x25fe
            | 0x2600..=0x27bf
            | 0x2934..=0x2935
            | 0x2b05..=0x2b55
            | 0x3030
            | 0x303d
            | 0x3297
            | 0x3299
            | 0x1f000..=0x1faff
    )
}

/// Splits text into what each glyph stands for. An emoji sequence is a
/// pictograph (or a keycap base) with its selectors, skin tone, tags and
/// ZWJ-joined parts, or a pair of regional indicators (a flag).
fn clusters(text: &str) -> Vec<Cluster> {
    let mut out = Vec::new();
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        let keycap = matches!(character, '0'..='9' | '#' | '*')
            && matches!(characters.peek(), Some('\u{fe0f}' | '\u{20e3}'));
        if !pictographic(character) && !keycap {
            out.push(Cluster::Char(character));
            continue;
        }
        let mut sequence = vec![character];
        if regional_indicator(character) {
            if let Some(&next) = characters.peek()
                && regional_indicator(next)
            {
                sequence.push(next);
                characters.next();
            }
        } else {
            while let Some(&next) = characters.peek() {
                match u32::from(next) {
                    0xfe0e | 0xfe0f | 0x20e3 | 0x1f3fb..=0x1f3ff | 0xe0020..=0xe007f => {
                        sequence.push(next);
                        characters.next();
                    }
                    0x200d => {
                        sequence.push(next);
                        characters.next();
                        if let Some(joined) = characters.next() {
                            sequence.push(joined);
                        }
                    }
                    _ => break,
                }
            }
        }
        // A text-style selector asks for the plain character.
        if sequence.len() == 1 || sequence[1..] == ['\u{fe0e}'] {
            out.push(Cluster::Char(character));
        } else {
            out.push(Cluster::Emoji(sequence.into_boxed_slice()));
        }
    }
    out
}

/// Flattens a TrueType/CFF outline into a `Path` in pixels, y down, with
/// the pen at the origin.
struct Outline {
    path: Path,
    scale: f32,
    last: (f32, f32),
}

impl Outline {
    fn point(&self, x: f32, y: f32) -> (f32, f32) {
        (x * self.scale, -y * self.scale)
    }
}

impl OutlineBuilder for Outline {
    fn move_to(&mut self, x: f32, y: f32) {
        let (px, py) = self.point(x, y);
        self.path.move_to(px, py);
        self.last = (px, py);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let (px, py) = self.point(x, y);
        self.path.line_to(px, py);
        self.last = (px, py);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (c, end, start) = (self.point(x1, y1), self.point(x, y), self.last);
        for step in 1..=8 {
            let t = step as f32 / 8.0;
            let u = 1.0 - t;
            self.path.line_to(
                u * u * start.0 + 2.0 * u * t * c.0 + t * t * end.0,
                u * u * start.1 + 2.0 * u * t * c.1 + t * t * end.1,
            );
        }
        self.last = end;
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (c1, c2, end, start) = (
            self.point(x1, y1),
            self.point(x2, y2),
            self.point(x, y),
            self.last,
        );
        for step in 1..=12 {
            let t = step as f32 / 12.0;
            let u = 1.0 - t;
            let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            self.path.line_to(
                a * start.0 + b * c1.0 + c * c2.0 + d * end.0,
                a * start.1 + b * c1.1 + c * c2.1 + d * end.1,
            );
        }
        self.last = end;
    }

    fn close(&mut self) {
        self.path.close();
    }
}

/// Applies the font's ligatures (GSUB type 4) to a glyph run, lookup by
/// lookup, as shaping would for the emoji font's sequences.
fn ligate(face: &Face, mut glyphs: Vec<GlyphId>) -> Vec<GlyphId> {
    use ttf_parser::gsub::SubstitutionSubtable;
    let Some(gsub) = face.tables().gsub else {
        return glyphs;
    };
    for lookup in gsub.lookups {
        for index in 0..lookup.subtables.len() {
            let Some(SubstitutionSubtable::Ligature(subtable)) =
                lookup.subtables.get::<SubstitutionSubtable>(index)
            else {
                continue;
            };
            let mut position = 0;
            while position < glyphs.len() {
                let replaced = subtable
                    .coverage
                    .get(glyphs[position])
                    .and_then(|set| subtable.ligature_sets.get(set))
                    .and_then(|set| {
                        set.into_iter().find(|ligature| {
                            let count = usize::from(ligature.components.len());
                            position + count < glyphs.len()
                                && ligature
                                    .components
                                    .into_iter()
                                    .zip(&glyphs[position + 1..])
                                    .filter(|(a, b)| a == *b)
                                    .count()
                                    == count
                        })
                    });
                if let Some(ligature) = replaced {
                    let count = usize::from(ligature.components.len());
                    glyphs.splice(position..=position + count, [ligature.glyph]);
                }
                position += 1;
            }
        }
    }
    glyphs
}

/// Decodes a PNG strike glyph into premultiplied ARGB.
fn decode_png(data: &[u8]) -> Option<(u32, u32, Vec<u32>)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer).ok()?;
    let channels = info.color_type.samples();
    let pixels = buffer[..info.buffer_size()]
        .chunks_exact(channels)
        .map(|p| {
            let (r, g, b, a) = match channels {
                4 => (p[0], p[1], p[2], p[3]),
                3 => (p[0], p[1], p[2], 255),
                2 => (p[0], p[0], p[0], p[1]),
                _ => (p[0], p[0], p[0], 255),
            };
            let a32 = u32::from(a);
            let pre = |c: u8| (u32::from(c) * a32 + 127) / 255;
            a32 << 24 | pre(r) << 16 | pre(g) << 8 | pre(b)
        })
        .collect();
    Some((info.width, info.height, pixels))
}

/// Area-averaged downscale of premultiplied pixels (bitmap emoji are drawn
/// well below their strike size).
fn resample(width: u32, height: u32, pixels: &[u32], to_w: u32, to_h: u32) -> Vec<u32> {
    let (sx, sy) = (width as f32 / to_w as f32, height as f32 / to_h as f32);
    let mut out = vec![0; (to_w * to_h) as usize];
    for y in 0..to_h {
        let (y0, y1) = (y as f32 * sy, (y + 1) as f32 * sy);
        for x in 0..to_w {
            let (x0, x1) = (x as f32 * sx, (x + 1) as f32 * sx);
            let mut sum = [0.0_f32; 4];
            let mut total = 0.0;
            let mut yy = y0.floor() as u32;
            while (yy as f32) < y1 && yy < height {
                let wy = ((yy + 1) as f32).min(y1) - (yy as f32).max(y0);
                let mut xx = x0.floor() as u32;
                while (xx as f32) < x1 && xx < width {
                    let wx = ((xx + 1) as f32).min(x1) - (xx as f32).max(x0);
                    let w = wx * wy;
                    let p = pixels[(yy * width + xx) as usize];
                    for (channel, value) in sum.iter_mut().enumerate() {
                        *value += (p >> (24 - 8 * channel) & 0xff) as f32 * w;
                    }
                    total += w;
                    xx += 1;
                }
                yy += 1;
            }
            let total = total.max(1e-6);
            out[(y * to_w + x) as usize] = sum
                .iter()
                .enumerate()
                .map(|(channel, value)| {
                    ((value / total + 0.5) as u32).min(255) << (24 - 8 * channel)
                })
                .fold(0, |pixel, part| pixel | part);
        }
    }
    out
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
            fallbacks: FALLBACK_FILES.iter().map(|_| Fallback::Unloaded).collect(),
            glyphs: HashMap::new(),
            gamma,
        }
    }

    fn fallback(&mut self, index: usize) -> Option<&Face<'static>> {
        if let Fallback::Unloaded = self.fallbacks[index] {
            let path = font_dir().join(FALLBACK_FILES[index]);
            self.fallbacks[index] = match map_file(&path).and_then(|data| Face::parse(data, 0).ok())
            {
                Some(face) => Fallback::Loaded(Box::new(face)),
                None => {
                    eprintln!("font fallback unavailable: {}", path.display());
                    Fallback::Missing
                }
            };
        }
        match &self.fallbacks[index] {
            Fallback::Loaded(face) => Some(face),
            _ => None,
        }
    }

    fn glyph(&mut self, weight: Weight, size: f32, cluster: &Cluster) -> Rc<Glyph> {
        let key = (weight, (size * 2.0).round() as u16, cluster.clone());
        if let Some(glyph) = self.glyphs.get(&key) {
            return glyph.clone();
        }
        let size = f32::from(key.1) / 2.0;
        let glyph = Rc::new(match cluster {
            Cluster::Char(character) => self.char_glyph(weight, size, *character),
            Cluster::Emoji(sequence) => self.emoji_glyph(weight, size, sequence),
        });
        self.glyphs.insert(key, glyph.clone());
        glyph
    }

    fn inter(&self, weight: Weight) -> &Font {
        let index = WEIGHTS.iter().position(|w| *w == weight).unwrap_or(0);
        &self.fonts[index]
    }

    fn char_glyph(&mut self, weight: Weight, size: f32, character: char) -> Glyph {
        if invisible(character) {
            return Glyph::empty(0.0);
        }
        if self.inter(weight).lookup_glyph_index(character) != 0 {
            return self.inter_glyph(weight, size, character);
        }
        if character.is_whitespace() {
            return Glyph::empty(self.inter(weight).metrics(' ', size).advance_width);
        }
        let noto = match weight {
            Weight::SemiBold | Weight::Bold | Weight::DisplaySemiBold => NOTO_SEMIBOLD,
            _ => NOTO_REGULAR,
        };
        for index in [noto, SYMBOLS_2, SYMBOLS, MATH] {
            if let Some(glyph) = self.outline_glyph(index, size, character) {
                return glyph;
            }
        }
        if let Some(face) = self.fallback(EMOJI)
            && let Some(id) = face.glyph_index(character)
            && let Some(glyph) = self.color_glyph(size, &[id])
        {
            return glyph;
        }
        self.tofu(weight, size)
    }

    fn emoji_glyph(&mut self, weight: Weight, size: f32, sequence: &[char]) -> Glyph {
        let ids = self.fallback(EMOJI).and_then(|face| {
            let first = face.glyph_index(sequence[0])?;
            let mut ids = vec![first];
            ids.extend(sequence[1..].iter().filter_map(|c| face.glyph_index(*c)));
            Some(ligate(face, ids))
        });
        if let Some(glyph) = ids.and_then(|ids| self.color_glyph(size, &ids)) {
            return glyph;
        }
        // No emoji font: the parts as text, joiners and selectors dropped.
        let mut advance = 0.0;
        let mut first = None;
        for character in sequence.iter().copied().filter(|c| !invisible(*c)) {
            let glyph = self.char_glyph(weight, size, character);
            advance += glyph.advance;
            first.get_or_insert(glyph);
        }
        let mut glyph = first.unwrap_or_else(|| Glyph::empty(0.0));
        glyph.advance = advance;
        glyph
    }

    fn inter_glyph(&self, weight: Weight, size: f32, character: char) -> Glyph {
        let (metrics, mut mask) = self.inter(weight).rasterize(character, size);
        for value in &mut mask {
            *value = self.gamma[usize::from(*value)];
        }
        Glyph {
            x: metrics.xmin,
            y: -(metrics.height as i32) - metrics.ymin,
            width: metrics.width as i32,
            height: metrics.height as i32,
            advance: metrics.advance_width,
            shape: Shape::Mask(mask),
        }
    }

    fn outline_glyph(&mut self, index: usize, size: f32, character: char) -> Option<Glyph> {
        let gamma = self.gamma;
        let face = self.fallback(index)?;
        let id = face.glyph_index(character)?;
        let scale = size / f32::from(face.units_per_em());
        let advance = f32::from(face.glyph_hor_advance(id).unwrap_or(0)) * scale;
        let mut outline = Outline {
            path: Path::new(),
            scale,
            last: (0.0, 0.0),
        };
        if face.outline_glyph(id, &mut outline).is_none() {
            return Some(Glyph::empty(advance));
        }
        outline.path.close();
        let Some((x, y, width, height, mut mask)) = outline.path.rasterize() else {
            return Some(Glyph::empty(advance));
        };
        for value in &mut mask {
            *value = gamma[usize::from(*value)];
        }
        Some(Glyph {
            x,
            y,
            width,
            height,
            advance,
            shape: Shape::Mask(mask),
        })
    }

    /// Emoji glyphs side by side (a sequence the font has no single
    /// ligature for), scaled from the bitmap strike to `size`.
    fn color_glyph(&mut self, size: f32, ids: &[GlyphId]) -> Option<Glyph> {
        let face = self.fallback(EMOJI)?;
        let upem = f32::from(face.units_per_em());
        let strike = face.glyph_raster_image(ids[0], u16::MAX)?.pixels_per_em;
        let scale = size / f32::from(strike);
        let mut parts = Vec::new();
        let mut pen = 0.0;
        for id in ids {
            let advance = f32::from(face.glyph_hor_advance(*id).unwrap_or(0)) * size / upem;
            if let Some(image) = face.glyph_raster_image(*id, strike)
                && image.format == RasterImageFormat::PNG
                && let Some((w, h, pixels)) = decode_png(image.data)
            {
                let to_w = ((w as f32 * scale).round() as u32).max(1);
                let to_h = ((h as f32 * scale).round() as u32).max(1);
                let left = (pen + f32::from(image.x) * scale).round() as i32;
                let top = -((f32::from(image.y) + f32::from(image.height)) * scale).round() as i32;
                parts.push((left, top, to_w, to_h, resample(w, h, &pixels, to_w, to_h)));
            }
            pen += advance;
        }
        if parts.is_empty() {
            return None;
        }
        // One image covering every part.
        let x0 = parts.iter().map(|p| p.0).min()?;
        let y0 = parts.iter().map(|p| p.1).min()?;
        let x1 = parts.iter().map(|p| p.0 + p.2 as i32).max()?;
        let y1 = parts.iter().map(|p| p.1 + p.3 as i32).max()?;
        let (width, height) = (x1 - x0, y1 - y0);
        let mut pixels = vec![0_u32; (width * height) as usize];
        for (left, top, w, h, part) in parts {
            for row in 0..h as i32 {
                for column in 0..w as i32 {
                    let source = part[(row * w as i32 + column) as usize];
                    let index = ((top - y0 + row) * width + left - x0 + column) as usize;
                    if source >> 24 > pixels[index] >> 24 {
                        pixels[index] = source;
                    }
                }
            }
        }
        Some(Glyph {
            x: x0,
            y: y0,
            width,
            height,
            advance: pen,
            shape: Shape::Color(Image {
                width,
                height,
                pixels,
            }),
        })
    }

    /// An empty box: a character no font has.
    fn tofu(&self, weight: Weight, size: f32) -> Glyph {
        let advance = self.inter(weight).metrics('0', size).advance_width;
        let width = (size * 0.5).round().max(3.0) as i32;
        let height = (size * 0.7).round().max(4.0) as i32;
        let stroke = (size / 16.0).round().max(1.0) as i32;
        let mut mask = vec![0_u8; (width * height) as usize];
        for y in 0..height {
            for x in 0..width {
                let edge = x < stroke || y < stroke || x >= width - stroke || y >= height - stroke;
                if edge {
                    mask[(y * width + x) as usize] = 200;
                }
            }
        }
        Glyph {
            x: ((advance - width as f32) / 2.0).round() as i32,
            y: -height,
            width,
            height,
            advance,
            shape: Shape::Mask(mask),
        }
    }
}

/// Typographic advance width of `text`, including `tracking` between glyphs.
pub fn measure_tracked(text: &str, weight: Weight, size: f32, tracking: f32) -> f32 {
    TYPEFACES.with(|cell| {
        let mut faces = cell.borrow_mut();
        let mut width = 0.0;
        let mut count = 0;
        for cluster in clusters(text) {
            width += faces.glyph(weight, size, &cluster).advance;
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
        for cluster in clusters(text) {
            let glyph = faces.glyph(weight, size, &cluster);
            if glyph.width > 0 && glyph.height > 0 {
                let (gx, gy) = (
                    pen.round() as i32 + glyph.x,
                    baseline.round() as i32 + glyph.y,
                );
                match &glyph.shape {
                    Shape::Mask(mask) => {
                        canvas.draw_mask(gx, gy, glyph.width, glyph.height, mask, color, alpha);
                    }
                    Shape::Color(image) => canvas.draw_image(image, gx, gy, alpha),
                }
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

#[allow(clippy::too_many_arguments, dead_code)]
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
    TYPEFACES.with(|cell| {
        cell.borrow_mut()
            .glyph(weight, size, &Cluster::Char('H'))
            .height as f32
    })
}

/// Baseline that vertically centers capitals around `center_y`.
pub fn centered_baseline(center_y: f32, weight: Weight, size: f32) -> f32 {
    center_y + cap_height(weight, size) / 2.0
}

/// Shortens `text` with an ellipsis so it fits in `max_width`, never
/// splitting an emoji sequence.
#[allow(dead_code)]
pub fn fit(text: &str, weight: Weight, size: f32, max_width: f32) -> String {
    if measure(text, weight, size) <= max_width {
        return text.to_owned();
    }
    let mut parts: Vec<String> = clusters(text)
        .into_iter()
        .map(|cluster| match cluster {
            Cluster::Char(character) => character.to_string(),
            Cluster::Emoji(sequence) => sequence.iter().collect(),
        })
        .collect();
    while !parts.is_empty() {
        parts.pop();
        let candidate = parts.concat() + "…";
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
        assert!(
            canvas
                .pixels
                .iter()
                .any(|pixel| pixel & 0xffffff == 0xffffff)
        );
    }

    #[test]
    fn fit_truncates_with_an_ellipsis() {
        let long = "An extremely long network name that will not fit";
        let fitted = fit(long, Weight::Regular, 40.0, 300.0);
        assert!(fitted.ends_with('…'));
        assert!(measure(&fitted, Weight::Regular, 40.0) <= 300.0);
        assert_eq!(fit("Short", Weight::Regular, 40.0, 300.0), "Short");
    }

    #[test]
    fn emoji_sequences_are_one_cluster() {
        let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        let flag = "\u{1f1e8}\u{1f1e6}";
        let text = format!(
            "a{family}{flag}\u{2764}\u{fe0f}1\u{fe0f}\u{20e3}\u{1f44d}\u{1f3fd}\u{263a}\u{fe0e}b"
        );
        let parts = clusters(&text);
        assert_eq!(parts.len(), 8, "{parts:?}");
        assert_eq!(parts[0], Cluster::Char('a'));
        assert!(matches!(&parts[1], Cluster::Emoji(s) if s.len() == 5));
        assert!(matches!(&parts[2], Cluster::Emoji(s) if s.len() == 2));
        assert!(matches!(&parts[3], Cluster::Emoji(s) if s.len() == 2));
        assert!(matches!(&parts[4], Cluster::Emoji(s) if s.len() == 3));
        assert!(matches!(&parts[5], Cluster::Emoji(s) if s.len() == 2));
        assert_eq!(parts[6], Cluster::Char('\u{263a}'));
        assert_eq!(parts[7], Cluster::Char('b'));
        // Digits stay text unless they are keycaps.
        assert_eq!(clusters("12"), vec![Cluster::Char('1'), Cluster::Char('2')]);
    }

    #[test]
    fn missing_characters_never_draw_a_question_mark() {
        // With no fallback fonts installed, an unknown character is a box
        // as wide as a digit, and joiners take no room.
        let digit = measure("0", Weight::Regular, 40.0);
        let unknown = measure("\u{4e2d}", Weight::Regular, 40.0);
        assert!((unknown - digit).abs() < 0.5);
        assert_eq!(measure("\u{200d}\u{fe0f}", Weight::Regular, 40.0), 0.0);
        assert_eq!(
            measure("a\nb", Weight::Regular, 40.0),
            measure("ab", Weight::Regular, 40.0)
        );
    }
}
