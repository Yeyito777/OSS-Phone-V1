//! Anti-aliased text using the embedded Inter typeface (SIL OFL 1.1, see
//! `assets/fonts/OFL.txt`), with fallbacks for everything Inter lacks so a
//! message never shows a '?': Noto Sans (Latin extended, Greek, Cyrillic,
//! Vietnamese), Noto Sans Symbols 2, Symbols and Math, and Noto Color Emoji (colour
//! bitmaps; ZWJ sequences, skin tones, keycaps and flags through its
//! ligatures). The fallbacks are files in `FONT_DIR` (`MOON_FONTS` overrides
//! it), memory-mapped the first time a character needs them, so they cost
//! nothing until then. A character no font has draws as an empty box.
//! Glyphs are rasterized once per weight/size/character and cached for the
//! life of the process. An emoji's bitmap (a PNG in the font, decoded and
//! scaled) is made on an emoji thread from the moment its glyph is first
//! wanted, so text measured before it is drawn draws it ready; drawn before
//! that thread made it, it is made in place, as before (the event loop
//! never waits on that thread). Decoded PNGs are kept a while
//! (`STRIKE_PIXELS`), so an emoji at another size is only scaled. The same
//! emoji in another weight shares its bitmap. Ported from a26-system's copy
//! (db38e0e1); the pixels are the same as before to the bit.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};

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
    /// An emoji: its bitmap (premultiplied ARGB, drawn as is) is made on
    /// the emoji thread once the glyph is first wanted (a layout measures
    /// text before it draws it), or here if it is drawn before that.
    Emoji(Rc<RefCell<Bitmap>>),
}

enum Bitmap {
    Asked(Arc<Job>),
    Made(Option<Made>),
}

/// An emoji's bitmap and where it goes from the pen (x) and baseline (y).
struct Made {
    x: i32,
    y: i32,
    image: Image,
}

/// An emoji bitmap asked of the emoji thread.
struct Job {
    size: f32,
    ids: Vec<GlyphId>,
    asked: std::time::Instant,
    state: Mutex<JobState>,
}

enum JobState {
    Waiting,
    Making,
    Done(Option<Made>),
    /// Taken (or made by the thread that draws it).
    Gone,
}

/// Emoji bitmaps asked of the emoji thread and not yet made, at most;
/// past it they are made when drawn.
const JOBS: usize = 64;
/// How long the emoji thread leaves a bitmap after it was asked for: one
/// drawn in the frame that measured it is made there, once, not twice.
const JOB_AFTER: std::time::Duration = std::time::Duration::from_millis(12);
static WAITING: AtomicUsize = AtomicUsize::new(0);
static EMOJI_THREAD: OnceLock<Option<mpsc::Sender<Arc<Job>>>> = OnceLock::new();

/// The emoji thread, started the first time an emoji is wanted: it makes
/// the bitmaps asked of it with a face of its own on the same font data.
fn emoji_thread(data: &'static [u8]) -> Option<&'static mpsc::Sender<Arc<Job>>> {
    EMOJI_THREAD
        .get_or_init(|| {
            let (sender, jobs) = mpsc::channel::<Arc<Job>>();
            std::thread::Builder::new()
                .name("emoji".into())
                .spawn(move || {
                    let face = Face::parse(data, 0).ok();
                    let mut strikes = Vec::new();
                    for job in jobs {
                        WAITING.fetch_sub(1, Ordering::Relaxed);
                        let Some(face) = &face else { continue };
                        if let Some(left) = JOB_AFTER.checked_sub(job.asked.elapsed()) {
                            std::thread::sleep(left);
                        }
                        {
                            let mut state = job.state.lock().unwrap_or_else(|e| e.into_inner());
                            if !matches!(*state, JobState::Waiting) {
                                continue;
                            }
                            *state = JobState::Making;
                        }
                        let made = make_emoji(face, job.size, &job.ids, &mut strikes);
                        let mut state = job.state.lock().unwrap_or_else(|e| e.into_inner());
                        if matches!(*state, JobState::Making) {
                            *state = JobState::Done(made);
                        }
                    }
                })
                .ok()?;
            Some(sender)
        })
        .as_ref()
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
    /// Emoji bitmaps as decoded from the font, the most recently used
    /// first, up to `STRIKE_PIXELS`: an emoji wanted at another size is
    /// only scaled again.
    strikes: Vec<(GlyphId, Rc<Strike>)>,
    /// The emoji font's bytes, for the emoji thread.
    emoji_data: Option<&'static [u8]>,
    /// Emoji bitmaps by size and glyphs: the same emoji in another weight
    /// is the same bitmap. Up to `EMOJI_SHARED`.
    emoji: HashMap<(u16, Vec<GlyphId>), Rc<RefCell<Bitmap>>>,
}

/// Emoji bitmaps shared between weights, at most (past it the sharing
/// starts again; the glyphs keep theirs).
const EMOJI_SHARED: usize = 1024;

/// An emoji's bitmap at the font's strike size, premultiplied ARGB.
struct Strike {
    width: u32,
    height: u32,
    pixels: Vec<u32>,
}

/// What the decoded emoji bitmaps may hold, in pixels, on each thread that
/// makes them (the UI's and the emoji thread): Noto's are 136 by 128, so
/// about thirty emoji, 2 MB.
const STRIKE_PIXELS: usize = 1 << 19;

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

/// Decodes a PNG strike glyph into premultiplied ARGB. A palette image
/// (every Noto emoji) has its palette premultiplied once and each pixel
/// looked up; the rest go through the png crate's expansion, a pixel of
/// full or no cover kept or cleared as the arithmetic would leave it.
pub(crate) fn decode_png(data: &[u8]) -> Option<(u32, u32, Vec<u32>)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
    decoder.set_transformations(png::Transformations::IDENTITY);
    let mut reader = decoder.read_info().ok()?;
    let info = reader.info();
    if info.color_type == png::ColorType::Indexed
        && info.bit_depth == png::BitDepth::Eight
        && let Some(palette) = info.palette.as_deref()
    {
        let table = premultiplied_palette(palette, info.trns.as_deref().unwrap_or(&[]));
        let mut buffer = vec![0; reader.output_buffer_size()];
        let frame = reader.next_frame(&mut buffer).ok()?;
        let pixels = buffer[..frame.buffer_size()]
            .iter()
            .map(|&index| table[usize::from(index)])
            .collect();
        return Some((frame.width, frame.height, pixels));
    }
    let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer).ok()?;
    let channels = info.color_type.samples();
    let pixels = buffer[..info.buffer_size()]
        .chunks_exact(channels)
        .map(|p| match channels {
            4 => premultiply(p[0], p[1], p[2], p[3]),
            3 => premultiply(p[0], p[1], p[2], 255),
            2 => premultiply(p[0], p[0], p[0], p[1]),
            _ => premultiply(p[0], p[0], p[0], 255),
        })
        .collect();
    Some((info.width, info.height, pixels))
}

/// A pixel premultiplied: each colour times its cover, rounded. Full cover
/// keeps the colour and none clears it, as the arithmetic gives.
#[inline]
fn premultiply(r: u8, g: u8, b: u8, a: u8) -> u32 {
    match a {
        255 => 0xff00_0000 | u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b),
        0 => 0,
        _ => {
            let a32 = u32::from(a);
            let pre = |c: u8| (u32::from(c) * a32 + 127) / 255;
            a32 << 24 | pre(r) << 16 | pre(g) << 8 | pre(b)
        }
    }
}

/// A PNG palette and its tRNS covers as the png crate expands them,
/// premultiplied: entries past the tRNS opaque, a tRNS longer than the
/// palette ignored, indices past the palette opaque black.
fn premultiplied_palette(palette: &[u8], trns: &[u8]) -> [u32; 256] {
    let entries = palette.len() / 3;
    let trns = if trns.len() <= entries { trns } else { &[] };
    let mut table = [premultiply(0, 0, 0, 255); 256];
    for (index, entry) in table.iter_mut().enumerate().take(entries) {
        let rgb = &palette[index * 3..index * 3 + 3];
        *entry = premultiply(
            rgb[0],
            rgb[1],
            rgb[2],
            trns.get(index).copied().unwrap_or(255),
        );
    }
    table
}

/// Area-averaged downscale of premultiplied pixels (bitmap emoji are drawn
/// well below their strike size). Each output pixel sums the source pixels
/// it covers, row by row, each times the part of it covered. It is the same
/// to the bit as working that out pixel by pixel (the test keeps that
/// version): the columns' and rows' parts are worked out once, with the
/// same f32 arithmetic; the four channels are summed side by side, each a
/// multiply then an add as before (never fused).
fn resample(width: u32, height: u32, pixels: &[u32], to_w: u32, to_h: u32) -> Vec<u32> {
    let (sx, sy) = (width as f32 / to_w as f32, height as f32 / to_h as f32);
    let columns = Spans::new(to_w, width, sx);
    let rows = Spans::new(to_h, height, sy);
    let stride = width as usize;
    // The source's bytes as f32 (exact), in memory order: blue, green, red,
    // alpha. Into a buffer kept between emoji: a strike's (278 kB) is past
    // what the allocator keeps, so a fresh one would be mapped, faulted in
    // page by page and unmapped every time.
    let mut source = SOURCE.with(|cell| cell.take());
    source.clear();
    source.extend(pixels.iter().map(|p| p.to_le_bytes().map(f32::from)));
    let mut out = vec![0; (to_w * to_h) as usize];
    for y in 0..to_h as usize {
        let (first_row, wys) = rows.get(y);
        for x in 0..to_w as usize {
            let (first_column, wxs) = columns.get(x);
            let mut sum = Lanes::ZERO;
            let mut total = 0.0_f32;
            for (row, &wy) in wys.iter().enumerate() {
                let at = (first_row + row) * stride + first_column;
                for (&wx, p) in wxs.iter().zip(&source[at..at + wxs.len()]) {
                    let w = wx * wy;
                    sum = sum.add_scaled(p, w);
                    total += w;
                }
            }
            let [b, g, r, a] = sum.get();
            let total = total.max(1e-6);
            let channel = |value: f32| ((value / total + 0.5) as u32).min(255);
            out[y * to_w as usize + x] =
                channel(a) << 24 | channel(r) << 16 | channel(g) << 8 | channel(b);
        }
    }
    // Kept for the next, unless an unusually large source made it big.
    if source.capacity() <= SOURCE_KEPT {
        SOURCE.with(|cell| cell.replace(source));
    }
    out
}

thread_local! {
    /// The scaling's f32 copy of its source, kept between calls.
    static SOURCE: RefCell<Vec<[f32; 4]>> = const { RefCell::new(Vec::new()) };
}

/// The most source pixels that copy is kept for (Noto's strikes are 136 by
/// 128): 1 MB on each thread that scales emoji.
const SOURCE_KEPT: usize = 1 << 16;

/// Four f32 channels summed side by side: `add_scaled` is `sum[i] += p[i] *
/// w` for each, a multiply and an add per lane exactly as the scalar code
/// (vector multiplies and adds round like scalar ones; nothing is fused).
#[derive(Clone, Copy)]
struct Lanes(Inner);

#[cfg(target_arch = "aarch64")]
type Inner = std::arch::aarch64::float32x4_t;
#[cfg(target_arch = "x86_64")]
type Inner = std::arch::x86_64::__m128;
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
type Inner = [f32; 4];

impl Lanes {
    const ZERO: Self = Self(
        // SAFETY: four zero f32s and the 128-bit vector have the same size,
        // and every bit pattern of either is valid.
        unsafe { std::mem::transmute::<[f32; 4], Inner>([0.0; 4]) },
    );

    /// `self + p * w`, lane by lane.
    #[inline(always)]
    fn add_scaled(self, p: &[f32; 4], w: f32) -> Self {
        #[cfg(target_arch = "aarch64")]
        // SAFETY: NEON is part of every aarch64 target; the load reads the
        // four f32s `p` points to.
        unsafe {
            use std::arch::aarch64::*;
            Self(vaddq_f32(self.0, vmulq_n_f32(vld1q_f32(p.as_ptr()), w)))
        }
        #[cfg(target_arch = "x86_64")]
        // SAFETY: SSE is part of every x86_64 target; the load reads the
        // four f32s `p` points to (unaligned).
        unsafe {
            use std::arch::x86_64::*;
            Self(_mm_add_ps(
                self.0,
                _mm_mul_ps(_mm_loadu_ps(p.as_ptr()), _mm_set1_ps(w)),
            ))
        }
        #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
        {
            let mut sum = self.0;
            for (value, channel) in sum.iter_mut().zip(p) {
                *value += channel * w;
            }
            Self(sum)
        }
    }

    fn get(self) -> [f32; 4] {
        // SAFETY: as for ZERO, the other way round.
        unsafe { std::mem::transmute::<Inner, [f32; 4]>(self.0) }
    }
}

/// For each output column (or row) of a downscale, the source columns it
/// covers: the first, and how much of each.
struct Spans {
    spans: Vec<(usize, usize, usize)>,
    weights: Vec<f32>,
}

impl Spans {
    fn new(count: u32, source: u32, scale: f32) -> Self {
        let mut spans = Vec::with_capacity(count as usize);
        let mut weights = Vec::new();
        for x in 0..count {
            let (x0, x1) = (x as f32 * scale, (x + 1) as f32 * scale);
            let first = x0.floor() as u32;
            let start = weights.len();
            let mut xx = first;
            while (xx as f32) < x1 && xx < source {
                weights.push(((xx + 1) as f32).min(x1) - (xx as f32).max(x0));
                xx += 1;
            }
            spans.push((first as usize, start, weights.len()));
        }
        Self { spans, weights }
    }

    fn get(&self, index: usize) -> (usize, &[f32]) {
        let (first, start, end) = self.spans[index];
        (first, &self.weights[start..end])
    }
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
            strikes: Vec::new(),
            emoji_data: None,
            emoji: HashMap::new(),
        }
    }

    fn fallback(&mut self, index: usize) -> Option<&Face<'static>> {
        if let Fallback::Unloaded = self.fallbacks[index] {
            let path = font_dir().join(FALLBACK_FILES[index]);
            let data = map_file(&path);
            if index == EMOJI {
                self.emoji_data = data;
            }
            self.fallbacks[index] = match data.and_then(|data| Face::parse(data, 0).ok()) {
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
        self.new_glyph(key)
    }

    /// A glyph not made yet (out of line: text measured and drawn again
    /// finds its glyphs made).
    #[inline(never)]
    fn new_glyph(&mut self, key: (Weight, u16, Cluster)) -> Rc<Glyph> {
        let (weight, cluster) = (key.0, &key.2);
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
    /// ligature for), scaled from the bitmap strike to `size`: their
    /// advance now, their bitmap asked of the emoji thread.
    fn color_glyph(&mut self, size: f32, ids: &[GlyphId]) -> Option<Glyph> {
        let face = self.fallback(EMOJI)?;
        let upem = f32::from(face.units_per_em());
        let strike = face.glyph_raster_image(ids[0], u16::MAX)?.pixels_per_em;
        let mut pen = 0.0;
        let mut any = false;
        for id in ids {
            pen += f32::from(face.glyph_hor_advance(*id).unwrap_or(0)) * size / upem;
            any |= face
                .glyph_raster_image(*id, strike)
                .is_some_and(|image| image.format == RasterImageFormat::PNG);
        }
        // (A PNG that then fails to decode draws nothing, where text drew
        // its parts before; the font's all decode.)
        if !any {
            return None;
        }
        let key = ((size * 2.0).round() as u16, ids.to_vec());
        if let Some(bitmap) = self.emoji.get(&key) {
            return Some(Glyph {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
                advance: pen,
                shape: Shape::Emoji(bitmap.clone()),
            });
        }
        let job = Arc::new(Job {
            size,
            ids: ids.to_vec(),
            asked: std::time::Instant::now(),
            state: Mutex::new(JobState::Waiting),
        });
        if WAITING.load(Ordering::Relaxed) < JOBS
            && let Some(thread) = self.emoji_data.and_then(emoji_thread)
        {
            WAITING.fetch_add(1, Ordering::Relaxed);
            if thread.send(job.clone()).is_err() {
                WAITING.fetch_sub(1, Ordering::Relaxed);
            }
        }
        let bitmap = Rc::new(RefCell::new(Bitmap::Asked(job)));
        if self.emoji.len() >= EMOJI_SHARED {
            self.emoji.clear();
        }
        self.emoji.insert(key, bitmap.clone());
        Some(Glyph {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            advance: pen,
            shape: Shape::Emoji(bitmap),
        })
    }

    /// An emoji's bitmap, to draw it: the emoji thread's if it is made,
    /// else made here (the UI thread does not wait for one that may run
    /// on a slower core).
    fn made(&mut self, bitmap: &RefCell<Bitmap>) {
        let job = match &*bitmap.borrow() {
            Bitmap::Made(_) => return,
            Bitmap::Asked(job) => job.clone(),
        };
        let done = {
            let mut state = job.state.lock().unwrap_or_else(|e| e.into_inner());
            match std::mem::replace(&mut *state, JobState::Gone) {
                JobState::Done(made) => Some(made),
                _ => None,
            }
        };
        let made = match done {
            Some(made) => made,
            None => {
                let Fallback::Loaded(face) = &self.fallbacks[EMOJI] else {
                    *bitmap.borrow_mut() = Bitmap::Made(None);
                    return;
                };
                make_emoji(face, job.size, &job.ids, &mut self.strikes)
            }
        };
        *bitmap.borrow_mut() = Bitmap::Made(made);
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

/// The bitmap of emoji glyphs `ids` side by side at `size`: each scaled
/// from the font's strike, one image covering them all.
fn make_emoji(
    face: &Face,
    size: f32,
    ids: &[GlyphId],
    strikes: &mut Vec<(GlyphId, Rc<Strike>)>,
) -> Option<Made> {
    let upem = f32::from(face.units_per_em());
    let strike = face.glyph_raster_image(ids[0], u16::MAX)?.pixels_per_em;
    let scale = size / f32::from(strike);
    let mut parts = Vec::new();
    let mut pen = 0.0;
    for id in ids {
        let advance = f32::from(face.glyph_hor_advance(*id).unwrap_or(0)) * size / upem;
        if let Some(image) = face.glyph_raster_image(*id, strike)
            && image.format == RasterImageFormat::PNG
            && let Some(decoded) = decoded_strike(strikes, *id, image.data)
        {
            let (w, h) = (decoded.width, decoded.height);
            let to_w = ((w as f32 * scale).round() as u32).max(1);
            let to_h = ((h as f32 * scale).round() as u32).max(1);
            let left = (pen + f32::from(image.x) * scale).round() as i32;
            let top = -((f32::from(image.y) + f32::from(image.height)) * scale).round() as i32;
            parts.push((
                left,
                top,
                to_w,
                to_h,
                resample(w, h, &decoded.pixels, to_w, to_h),
            ));
        }
        pen += advance;
    }
    if parts.is_empty() {
        return None;
    }
    // One part is the image (composing it on clear pixels would copy it: a
    // part's pixel of no cover is all zero, its colours being sums no
    // greater than its cover's).
    if let [(left, top, w, h, _)] = parts[..] {
        let pixels = parts.pop()?.4;
        return Some(Made {
            x: left,
            y: top,
            image: Image {
                width: w as i32,
                height: h as i32,
                pixels,
            },
        });
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
    Some(Made {
        x: x0,
        y: y0,
        image: Image {
            width,
            height,
            pixels,
        },
    })
}

/// Draws an emoji with the pen at (`pen`, `baseline`), its bitmap made
/// first if need be (out of line: text is mostly not emoji).
#[inline(never)]
fn draw_emoji(
    faces: &mut Typefaces,
    canvas: &mut Canvas,
    bitmap: &RefCell<Bitmap>,
    pen: f32,
    baseline: f32,
    alpha: f32,
) {
    faces.made(bitmap);
    if let Bitmap::Made(Some(made)) = &*bitmap.borrow() {
        let (x, y) = (
            pen.round() as i32 + made.x,
            baseline.round() as i32 + made.y,
        );
        if alpha >= 1.0 {
            blit(canvas, &made.image, x, y);
        } else {
            canvas.draw_image(&made.image, x, y, alpha);
        }
    }
}

/// Draws `image` (premultiplied) as Canvas::draw_image does at full alpha,
/// pixel for pixel, within the clip only: a pixel of full cover replaces
/// what is there, one of none leaves it, the rest are blended as it blends.
fn blit(canvas: &mut Canvas, image: &Image, x: i32, y: i32) {
    let (cx0, cy0, cx1, cy1) = canvas.clip();
    let (x0, x1) = (
        x.max(cx0).max(0),
        (x + image.width).min(cx1).min(canvas.width),
    );
    let (y0, y1) = (
        y.max(cy0).max(0),
        (y + image.height).min(cy1).min(canvas.height),
    );
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let (from, n) = ((x0 - x) as usize, (x1 - x0) as usize);
    let (stride, source) = (canvas.width as usize, image.width as usize);
    for ty in y0..y1 {
        let at = (ty - y) as usize * source + from;
        let src = &image.pixels[at..at + n];
        let at = ty as usize * stride + x0 as usize;
        for (d, &s) in canvas.pixels[at..at + n].iter_mut().zip(src) {
            match s >> 24 {
                0 => {}
                255 => *d = s,
                sa => {
                    let inverse = 256 - (sa + (sa >> 7));
                    let scale = |shift: u32| {
                        let channel = s >> shift & 0xff;
                        let under = *d >> shift & 0xff;
                        (channel + ((under * inverse) >> 8)).min(255) << shift
                    };
                    *d = scale(24) | scale(16) | scale(8) | scale(0);
                }
            }
        }
    }
}

/// Glyph `id`'s bitmap `data` decoded, from the cache when it was
/// decoded lately.
fn decoded_strike(
    strikes: &mut Vec<(GlyphId, Rc<Strike>)>,
    id: GlyphId,
    data: &[u8],
) -> Option<Rc<Strike>> {
    if let Some(at) = strikes.iter().position(|(cached, _)| *cached == id) {
        let entry = strikes.remove(at);
        let strike = entry.1.clone();
        strikes.insert(0, entry);
        return Some(strike);
    }
    let (width, height, pixels) = decode_png(data)?;
    let strike = Rc::new(Strike {
        width,
        height,
        pixels,
    });
    strikes.insert(0, (id, strike.clone()));
    let mut held = 0;
    strikes.retain(|(_, kept)| {
        held += kept.pixels.len();
        held <= STRIKE_PIXELS
    });
    Some(strike)
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
            if let Shape::Emoji(bitmap) = &glyph.shape {
                draw_emoji(&mut faces, canvas, bitmap, pen, baseline, alpha);
            } else if glyph.width > 0 && glyph.height > 0 {
                let (gx, gy) = (
                    pen.round() as i32 + glyph.x,
                    baseline.round() as i32 + glyph.y,
                );
                match &glyph.shape {
                    Shape::Mask(mask) => {
                        canvas.draw_mask(gx, gy, glyph.width, glyph.height, mask, color, alpha);
                    }
                    Shape::Emoji(_) => {}
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

    /// The downscale as e3add0d did it, pixel by pixel, to compare with.
    fn reference_resample(
        width: u32,
        height: u32,
        pixels: &[u32],
        to_w: u32,
        to_h: u32,
    ) -> Vec<u32> {
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

    /// The PNG decoding of e3add0d, to compare with.
    fn reference_decode_png(data: &[u8]) -> Option<(u32, u32, Vec<u32>)> {
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

    /// PNGs decode as they did: palette images with and without covers, a
    /// short and an over-long tRNS, indices past the palette, 4-bit
    /// palettes, and RGBA, RGB, grey with and without alpha, 16-bit.
    #[test]
    fn png_decodes_as_it_did() {
        let mut seed = 0x9e37_79b9_u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let (width, height) = (37_u32, 23_u32);
        let encode = |color: png::ColorType,
                      depth: png::BitDepth,
                      palette: Option<Vec<u8>>,
                      trns: Option<Vec<u8>>,
                      data: Vec<u8>| {
            let mut out = Vec::new();
            {
                let mut encoder = png::Encoder::new(&mut out, width, height);
                encoder.set_color(color);
                encoder.set_depth(depth);
                if let Some(palette) = palette {
                    encoder.set_palette(palette);
                }
                if let Some(trns) = trns {
                    encoder.set_trns(trns);
                }
                let mut writer = encoder.write_header().unwrap();
                writer.write_image_data(&data).unwrap();
            }
            out
        };
        let mut cases = Vec::new();
        let palette: Vec<u8> = (0..200 * 3).map(|_| next() as u8).collect();
        let covers: Vec<u8> = (0..200)
            .map(|i| match i % 4 {
                0 => 0,
                1 => 255,
                _ => next() as u8,
            })
            .collect();
        // Indices up to 255, past the palette's 200 entries.
        let indices: Vec<u8> = (0..width * height).map(|_| next() as u8).collect();
        for trns in [
            None,
            Some(covers.clone()),
            Some(covers[..50].to_vec()),
            Some(vec![7; 210]),
        ] {
            cases.push(encode(
                png::ColorType::Indexed,
                png::BitDepth::Eight,
                Some(palette.clone()),
                trns,
                indices.clone(),
            ));
        }
        let packed: Vec<u8> = (0..(width as usize).div_ceil(2) * height as usize)
            .map(|_| next() as u8)
            .collect();
        cases.push(encode(
            png::ColorType::Indexed,
            png::BitDepth::Four,
            Some(palette[..48].to_vec()),
            Some(covers[..10].to_vec()),
            packed,
        ));
        for (color, channels) in [
            (png::ColorType::Rgba, 4),
            (png::ColorType::Rgb, 3),
            (png::ColorType::GrayscaleAlpha, 2),
            (png::ColorType::Grayscale, 1),
        ] {
            let data: Vec<u8> = (0..width * height * channels)
                .map(|i| {
                    if channels % 2 == 0 && i % channels == channels - 1 {
                        [0, 255, next() as u8][(next() % 3) as usize]
                    } else {
                        next() as u8
                    }
                })
                .collect();
            cases.push(encode(color, png::BitDepth::Eight, None, None, data));
            let wide: Vec<u8> = (0..width * height * channels * 2)
                .map(|_| next() as u8)
                .collect();
            cases.push(encode(color, png::BitDepth::Sixteen, None, None, wide));
        }
        for (n, data) in cases.iter().enumerate() {
            assert_eq!(decode_png(data), reference_decode_png(data), "case {n}");
        }
        // And the emoji font's own, where it is installed.
        TYPEFACES.with(|cell| {
            let mut faces = cell.borrow_mut();
            let Some(face) = faces.fallback(EMOJI) else {
                return;
            };
            let mut checked = 0;
            for id in (0..face.number_of_glyphs()).step_by(7) {
                if let Some(image) = face.glyph_raster_image(GlyphId(id), u16::MAX)
                    && image.format == RasterImageFormat::PNG
                {
                    assert_eq!(
                        decode_png(image.data),
                        reference_decode_png(image.data),
                        "glyph {id}"
                    );
                    checked += 1;
                }
            }
            eprintln!("png_decodes_as_it_did: {checked} of the emoji font's PNGs compared");
        });
    }

    /// The downscale gives what it gave pixel by pixel, to the bit: every
    /// size from 1 to 160 px of Noto's 136 by 128 strikes and of odd ones,
    /// with transparent, opaque and half-covered pixels.
    #[test]
    fn resample_is_the_same_to_the_bit() {
        let mut seed = 0x2545_f491_u32;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for (width, height) in [(136, 128), (136, 136), (97, 61), (3, 200), (200, 1)] {
            let pixels: Vec<u32> = (0..width * height)
                .map(|i| {
                    let r = next();
                    // Areas of nothing, of full cover, and edges between.
                    let a = match (i / 7 + r % 5) % 4 {
                        0 => 0,
                        1 => 255,
                        _ => r >> 24,
                    };
                    let c = |v: u32| ((v & 0xff) * a) / 255;
                    a << 24 | c(r) << 16 | c(r >> 8) << 8 | c(r >> 16)
                })
                .collect();
            for to_w in (1..=80).chain((81..=160).step_by(7)) {
                let to_h = ((height as f32 * to_w as f32 / width as f32).round() as u32).max(1);
                assert_eq!(
                    resample(width, height, &pixels, to_w, to_h),
                    reference_resample(width, height, &pixels, to_w, to_h),
                    "{width}x{height} to {to_w}x{to_h}"
                );
            }
        }
    }

    /// Emoji as e3add0d drew them, to compare with: the glyphs' ids as
    /// `char_glyph` and `emoji_glyph` found them, each part decoded and
    /// scaled on the spot. (x, y, image, advance)
    fn reference_emoji(
        face: &Face,
        size: f32,
        cluster: &Cluster,
    ) -> Option<(i32, i32, Image, f32)> {
        let ids = match cluster {
            Cluster::Char(character) => vec![face.glyph_index(*character)?],
            Cluster::Emoji(sequence) => {
                let mut ids = vec![face.glyph_index(sequence[0])?];
                ids.extend(sequence[1..].iter().filter_map(|c| face.glyph_index(*c)));
                ligate(face, ids)
            }
        };
        let upem = f32::from(face.units_per_em());
        let strike = face.glyph_raster_image(ids[0], u16::MAX)?.pixels_per_em;
        let scale = size / f32::from(strike);
        let mut parts = Vec::new();
        let mut pen = 0.0;
        for id in &ids {
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
        Some((
            x0,
            y0,
            Image {
                width,
                height,
                pixels,
            },
            pen,
        ))
    }

    /// Emoji draw as they did before the port: at Moon's sizes, in four of
    /// its weights (the same emoji in another weight shares its bitmap), at
    /// whole and fractional positions, clipped or not, faded or not,
    /// measured first or drawn straight away, and drawn again.
    /// Needs the emoji font (MOON_FONTS or the phone's); without it there
    /// is nothing to compare.
    #[test]
    fn emoji_draw_as_they_did() {
        // (A lone pictograph without U+FE0F is text: Noto Sans Symbols 2.)
        let emoji = [
            "\u{1f44d}\u{fe0f}",
            "\u{1f602}\u{fe0f}",
            "\u{1f389}\u{fe0f}",
            "\u{1f525}\u{fe0f}",
            "\u{2764}\u{fe0f}",
            "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}",
            "\u{1f1e8}\u{1f1e6}",
            "\u{1f44d}\u{1f3fd}",
            "1\u{fe0f}\u{20e3}",
            "\u{1f3f3}\u{fe0f}\u{200d}\u{1f308}",
            // No ligature: two bitmaps side by side.
            "\u{1f44d}\u{200d}\u{1f525}",
        ];
        // Moon's sizes: notifications (title 44, body 42, time 36), the
        // lock screen and Control Centre (32-40, 52, 62, 74, 84), home
        // labels (33), and very small and very large.
        let sizes = [
            44.0, 42.0, 36.0, 32.0, 33.0, 34.0, 37.0, 38.0, 40.0, 52.0, 62.0, 74.0, 84.0, 44.0,
            13.0, 128.0,
        ];
        let places = [
            (10.0, 60.0),
            (10.3, 60.5),
            (10.5, 61.4),
            (10.75, 120.0),
            (-20.0, 5.0),
        ];
        let present = TYPEFACES.with(|cell| cell.borrow_mut().fallback(EMOJI).is_some());
        if !present {
            eprintln!(
                "emoji_draw_as_they_did: no emoji font in {}, nothing compared",
                font_dir().display()
            );
            return;
        }
        let mut compared = 0;
        let mut made_there = 0;
        for (round, &size) in sizes.iter().enumerate() {
            for (n, text) in emoji.iter().enumerate() {
                let cluster = clusters(text).remove(0);
                let size = (size * 2.0_f32).round() / 2.0;
                // The same emoji in each weight shares its bitmap.
                let weight = [
                    Weight::Regular,
                    Weight::SemiBold,
                    Weight::Bold,
                    Weight::DisplaySemiBold,
                ][(round + n) % 4];
                let Some((gx, gy, image, advance)) = TYPEFACES.with(|cell| {
                    let mut faces = cell.borrow_mut();
                    reference_emoji(faces.fallback(EMOJI).unwrap(), size, &cluster)
                }) else {
                    panic!("{text}: no bitmap");
                };
                // Some measured before they are drawn (as a layout does).
                if (round + n) % 2 == 0 {
                    measure(&format!("a{text}b"), weight, size);
                    // And drawn once the emoji thread made it.
                    let start = std::time::Instant::now();
                    while start.elapsed().as_secs() < 5 {
                        let done = TYPEFACES.with(|cell| {
                            let glyph = cell.borrow_mut().glyph(weight, size, &cluster);
                            match &glyph.shape {
                                Shape::Emoji(bitmap) => match &*bitmap.borrow() {
                                    Bitmap::Asked(job) => {
                                        matches!(*job.state.lock().unwrap(), JobState::Done(_))
                                    }
                                    Bitmap::Made(_) => true,
                                },
                                Shape::Mask(_) => panic!("{text}: not an emoji"),
                            }
                        });
                        if done {
                            made_there += 1;
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                }
                for again in 0..2 {
                    for (p, &(x, baseline)) in places.iter().enumerate() {
                        let alpha = if p % 2 == 0 { 1.0 } else { 0.6 };
                        let mut drawn = Canvas::new(500, 300);
                        for (i, pixel) in drawn.pixels.iter_mut().enumerate() {
                            *pixel = (i as u32).wrapping_mul(2654435761) & 0x00ff_ffff;
                        }
                        let mut expected = Canvas::new(500, 300);
                        expected.pixels.copy_from_slice(&drawn.pixels);
                        let line = format!("a{text}b");
                        let width = draw(
                            &mut drawn, &line, x, baseline, weight, size, 0xe0e0e0, alpha,
                        );
                        let mut pen = x + draw(
                            &mut expected,
                            "a",
                            x,
                            baseline,
                            weight,
                            size,
                            0xe0e0e0,
                            alpha,
                        );
                        let before = expected.pixels.clone();
                        expected.draw_image(
                            &image,
                            pen.round() as i32 + gx,
                            baseline.round() as i32 + gy,
                            alpha,
                        );
                        assert!(
                            expected.pixels != before || x < 0.0,
                            "{text} at {size}: nothing drawn"
                        );
                        pen += advance;
                        pen += draw(
                            &mut expected,
                            "b",
                            pen,
                            baseline,
                            weight,
                            size,
                            0xe0e0e0,
                            alpha,
                        );
                        assert_eq!(width, pen - x, "{text} at {size}: advance");
                        assert!(
                            drawn.pixels == expected.pixels,
                            "{text} at {size}, ({x}, {baseline}), alpha {alpha}, drawn {again}"
                        );
                        compared += 1;
                    }
                }
            }
        }
        eprintln!(
            "emoji_draw_as_they_did: {compared} drawings compared, {made_there} emoji made on the emoji thread"
        );
        assert!(made_there > 0);
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
