//! Built-in wallpapers: a vertical base gradient lit by soft glows, drawn
//! procedurally at any size (the System app draws its thumbnails with the
//! same table; docs/settings.md lists it). The panel is AMOLED, so dark
//! wallpapers cost less power: "black" lights no pixel at all.
//!
//! Dynamic wallpapers add layers drawn once and moved by the phone's tilt
//! (tilt.rs), each by its own amount, so the picture has depth: "orbit"
//! (stars and a moon), "ridge" (mountain ranges at dusk), and every saved
//! picture (`saved:NAME`, a PNG in `SAVED_DIR`, which slides under the
//! screen like a window onto it).

use crate::gfx::{self, Canvas};

pub struct Glow {
    /// Center as fractions of the width and height.
    pub x: f32,
    pub y: f32,
    /// Gaussian radius as a fraction of the width.
    pub radius: f32,
    pub color: (f32, f32, f32),
    pub strength: f32,
}

pub struct Wallpaper {
    pub id: &'static str,
    /// Base color at the top and the bottom.
    pub top: (f32, f32, f32),
    pub bottom: (f32, f32, f32),
    pub glows: &'static [Glow],
}

pub const WALLPAPERS: [Wallpaper; 9] = [
    Wallpaper {
        id: "moon",
        top: (10.0, 12.0, 30.0),
        bottom: (4.0, 5.0, 16.0),
        glows: &[
            Glow {
                x: 0.86,
                y: 0.10,
                radius: 0.62,
                color: (94.0, 92.0, 230.0),
                strength: 0.62,
            },
            Glow {
                x: 0.06,
                y: 0.58,
                radius: 0.64,
                color: (10.0, 110.0, 230.0),
                strength: 0.42,
            },
            Glow {
                x: 0.78,
                y: 0.98,
                radius: 0.56,
                color: (150.0, 70.0, 210.0),
                strength: 0.40,
            },
        ],
    },
    Wallpaper {
        id: "black",
        top: (0.0, 0.0, 0.0),
        bottom: (0.0, 0.0, 0.0),
        glows: &[],
    },
    Wallpaper {
        id: "graphite",
        top: (7.0, 7.0, 9.0),
        bottom: (0.0, 0.0, 0.0),
        glows: &[
            Glow {
                x: 0.80,
                y: 0.06,
                radius: 0.55,
                color: (70.0, 74.0, 86.0),
                strength: 0.45,
            },
            Glow {
                x: 0.10,
                y: 0.92,
                radius: 0.50,
                color: (40.0, 44.0, 56.0),
                strength: 0.35,
            },
        ],
    },
    Wallpaper {
        id: "dusk",
        top: (26.0, 10.0, 28.0),
        bottom: (8.0, 4.0, 14.0),
        glows: &[
            Glow {
                x: 0.10,
                y: 0.08,
                radius: 0.60,
                color: (255.0, 128.0, 70.0),
                strength: 0.55,
            },
            Glow {
                x: 0.92,
                y: 0.46,
                radius: 0.58,
                color: (214.0, 64.0, 140.0),
                strength: 0.45,
            },
            Glow {
                x: 0.30,
                y: 0.98,
                radius: 0.60,
                color: (110.0, 50.0, 190.0),
                strength: 0.45,
            },
        ],
    },
    Wallpaper {
        id: "ocean",
        top: (2.0, 16.0, 26.0),
        bottom: (1.0, 6.0, 14.0),
        glows: &[
            Glow {
                x: 0.15,
                y: 0.12,
                radius: 0.60,
                color: (20.0, 176.0, 190.0),
                strength: 0.50,
            },
            Glow {
                x: 0.90,
                y: 0.55,
                radius: 0.62,
                color: (14.0, 90.0, 220.0),
                strength: 0.48,
            },
            Glow {
                x: 0.40,
                y: 1.00,
                radius: 0.55,
                color: (40.0, 210.0, 200.0),
                strength: 0.30,
            },
        ],
    },
    Wallpaper {
        id: "forest",
        top: (4.0, 16.0, 12.0),
        bottom: (2.0, 7.0, 6.0),
        glows: &[
            Glow {
                x: 0.85,
                y: 0.10,
                radius: 0.60,
                color: (60.0, 180.0, 110.0),
                strength: 0.50,
            },
            Glow {
                x: 0.08,
                y: 0.55,
                radius: 0.60,
                color: (130.0, 180.0, 60.0),
                strength: 0.35,
            },
            Glow {
                x: 0.70,
                y: 0.98,
                radius: 0.56,
                color: (20.0, 140.0, 140.0),
                strength: 0.40,
            },
        ],
    },
    // Dynamic: the moon and the stars float over it at their own depths and
    // move as the phone tilts (`build`, tilt.rs).
    Wallpaper {
        id: "orbit",
        top: (6.0, 7.0, 22.0),
        bottom: (1.0, 1.0, 6.0),
        glows: &[
            Glow {
                x: 0.72,
                y: 0.22,
                radius: 0.55,
                color: (70.0, 66.0, 190.0),
                strength: 0.50,
            },
            Glow {
                x: 0.04,
                y: 0.70,
                radius: 0.60,
                color: (12.0, 70.0, 170.0),
                strength: 0.30,
            },
            Glow {
                x: 0.85,
                y: 1.00,
                radius: 0.50,
                color: (120.0, 50.0, 170.0),
                strength: 0.28,
            },
        ],
    },
    // Dynamic: a cut gem whose facets catch a spectrum that slides
    // beneath them as the phone tilts (`prism_layers`).
    Wallpaper {
        id: "prism",
        top: (9.0, 9.0, 14.0),
        bottom: (3.0, 3.0, 6.0),
        glows: &[Glow {
            x: 0.5,
            y: 0.53,
            radius: 0.40,
            color: (90.0, 70.0, 160.0),
            strength: 0.35,
        }],
    },
    // Dynamic: a dusk sky over mountain ranges, nearer ones moving more
    // (`ridge_layers`).
    Wallpaper {
        id: "ridge",
        top: (10.0, 8.0, 34.0),
        bottom: (92.0, 40.0, 72.0),
        glows: &[
            Glow {
                x: 0.64,
                y: 0.60,
                radius: 0.34,
                color: (255.0, 150.0, 90.0),
                strength: 0.80,
            },
            Glow {
                x: 0.20,
                y: 0.40,
                radius: 0.60,
                color: (96.0, 40.0, 150.0),
                strength: 0.35,
            },
        ],
    },
];

pub fn find(id: &str) -> &'static Wallpaper {
    WALLPAPERS
        .iter()
        .find(|w| w.id == id)
        .unwrap_or(&WALLPAPERS[0])
}

pub fn exists(id: &str) -> bool {
    WALLPAPERS.iter().any(|w| w.id == id) || saved_path(id).is_some_and(|path| path.is_file())
}

/// The user's saved wallpapers: PNGs, ideally `SAVED_W` x `SAVED_H` (the
/// screen with `SAVED_MARGIN` to spare on every side; others are scaled to
/// cover it). `moon-wallpaper-add` converts any picture.
pub const SAVED_DIR: &str = "/var/lib/a26-shell/wallpapers";
pub const SAVED_PREFIX: &str = "saved:";
const SAVED_MARGIN: i32 = 40;

/// A saved wallpaper's file: `saved:NAME` with NAME a .png file name.
fn saved_path(id: &str) -> Option<std::path::PathBuf> {
    let name = id.strip_prefix(SAVED_PREFIX)?;
    (!name.is_empty()
        && !name.starts_with('.')
        && !name.contains('/')
        && name.to_ascii_lowercase().ends_with(".png"))
    .then(|| std::path::Path::new(SAVED_DIR).join(name))
}

/// Draws a wallpaper at `width` x `height`.
pub fn render(id: &str, width: i32, height: i32) -> Canvas {
    let spec = find(id);
    let w = width as f32;
    let mut canvas = Canvas::new(width, height);
    if spec.glows.is_empty() && spec.top == spec.bottom {
        let (r, g, b) = spec.top;
        canvas.clear((r as u32) << 16 | (g as u32) << 8 | b as u32);
        return canvas;
    }
    let columns: Vec<Vec<f32>> = spec
        .glows
        .iter()
        .map(|glow| {
            let sigma = glow.radius * w;
            (0..width)
                .map(|x| {
                    let d = x as f32 - glow.x * w;
                    (-(d * d) / (2.0 * sigma * sigma)).exp()
                })
                .collect()
        })
        .collect();
    for y in 0..height {
        let t = y as f32 / height as f32;
        let base = (
            gfx::lerp(spec.top.0, spec.bottom.0, t),
            gfx::lerp(spec.top.1, spec.bottom.1, t),
            gfx::lerp(spec.top.2, spec.bottom.2, t),
        );
        let rows: Vec<f32> = spec
            .glows
            .iter()
            .map(|glow| {
                let sigma = glow.radius * w;
                let d = y as f32 - glow.y * height as f32;
                (-(d * d) / (2.0 * sigma * sigma)).exp() * glow.strength
            })
            .collect();
        for x in 0..width {
            let (mut r, mut g, mut b) = base;
            for (index, glow) in spec.glows.iter().enumerate() {
                let k = rows[index] * columns[index][x as usize];
                r += glow.color.0 * k;
                g += glow.color.1 * k;
                b += glow.color.2 * k;
            }
            // Ordered-noise dither of +/- half a code value (none on black).
            let hash = (x as u32).wrapping_mul(0x9e37_79b1) ^ (y as u32).wrapping_mul(0x85eb_ca77);
            let noise = ((hash >> 13) & 0xff) as f32 / 255.0 - 0.5;
            let channel = |v: f32| {
                if v <= 0.0 {
                    0
                } else {
                    (v + noise).clamp(0.0, 255.0) as u32
                }
            };
            canvas.pixels[(y * width + x) as usize] =
                channel(r) << 16 | channel(g) << 8 | channel(b);
        }
    }
    canvas
}

/// Wallpapers with layers that move with the phone's tilt.
pub fn is_dynamic(id: &str) -> bool {
    matches!(id, "orbit" | "ridge" | "prism") || id.starts_with(SAVED_PREFIX)
}

/// One moving layer of a dynamic wallpaper: at rest its top-left corner is
/// at (`x`, `y`) on screen, and a full tilt (tilt.rs, +/- 1) shifts it by
/// `travel` pixels. A canvas pixel covers `scale` screen pixels (smooth
/// content is drawn smaller and scaled up). Layers are listed back to
/// front.
pub struct Layer {
    pub canvas: Canvas,
    pub x: f32,
    pub y: f32,
    pub travel: f32,
    pub scale: f32,
}

/// A wallpaper at `width` x `height`: its still picture (shown where
/// nothing moves: the CPU compositor, the darkened backgrounds) and its
/// moving layers (none for a still wallpaper).
pub fn build(id: &str, width: i32, height: i32) -> (Canvas, Vec<Layer>) {
    if id.starts_with(SAVED_PREFIX) {
        return match load_saved(id, width, height) {
            Some(picture) => {
                let mut still = Canvas::new(width, height);
                for y in 0..height {
                    let row = ((y + SAVED_MARGIN) * picture.width + SAVED_MARGIN) as usize;
                    still.pixels[(y * width) as usize..((y + 1) * width) as usize]
                        .copy_from_slice(&picture.pixels[row..row + width as usize]);
                }
                let margin = SAVED_MARGIN as f32;
                let layer = Layer {
                    canvas: picture,
                    x: -margin,
                    y: -margin,
                    travel: margin * 0.8,
                    scale: 1.0,
                };
                (still, vec![layer])
            }
            None => (render("moon", width, height), Vec::new()),
        };
    }
    let mut still = render(id, width, height);
    let layers = match id {
        "orbit" => {
            let margin = 40;
            let mut stars = Canvas::new(width + 2 * margin, height + 2 * margin);
            stars.pixels.fill(0);
            draw_stars(&mut stars, 0.16, 1.0, width as f32 / 1080.0);
            let moon = width * 16 / 27;
            vec![
                Layer {
                    canvas: stars,
                    x: -margin as f32,
                    y: -margin as f32,
                    travel: margin as f32 * 0.8,
                    scale: 1.0,
                },
                Layer {
                    canvas: sky_moon(moon),
                    x: width as f32 * 0.713 - moon as f32 / 2.0,
                    y: height as f32 * 0.504 - moon as f32 / 2.0,
                    travel: width as f32 / 12.0,
                    scale: 1.0,
                },
            ]
        }
        "ridge" => {
            draw_stars(&mut still, 0.10, 0.42, width as f32 / 1080.0);
            ridge_layers(width, height)
        }
        "prism" => prism_layers(width, height),
        _ => Vec::new(),
    };
    (still, layers)
}

/// Draws `layers` at rest over `still`: the whole picture, for where
/// nothing moves (the CPU compositor).
pub fn flatten(still: &mut Canvas, layers: &[Layer]) {
    for layer in layers {
        let (x0, y0) = (layer.x.round() as i32, layer.y.round() as i32);
        let (w, h) = (layer.canvas.width, layer.canvas.height);
        let (sw, sh) = (
            (w as f32 * layer.scale).round() as i32,
            (h as f32 * layer.scale).round() as i32,
        );
        for y in y0.max(0)..(y0 + sh).min(still.height) {
            let v = (((y - y0) as f32 / layer.scale) as i32).min(h - 1);
            for x in x0.max(0)..(x0 + sw).min(still.width) {
                let u = (((x - x0) as f32 / layer.scale) as i32).min(w - 1);
                let src = layer.canvas.pixels[(v * w + u) as usize];
                let under = 255 - (src >> 24);
                let dst = &mut still.pixels[(y * still.width + x) as usize];
                let channel = |shift: u32| {
                    ((src >> shift) & 0xff) + (((*dst >> shift) & 0xff) * under + 127) / 255
                };
                *dst = 0xff00_0000
                    | channel(16).min(255) << 16
                    | channel(8).min(255) << 8
                    | channel(0).min(255);
            }
        }
    }
}

/// A saved PNG, scaled to cover the screen and its margins.
fn load_saved(id: &str, width: i32, height: i32) -> Option<Canvas> {
    let data = std::fs::read(saved_path(id)?).ok()?;
    let (w, h, pixels) = crate::text::decode_png(&data)?;
    let (to_w, to_h) = (width + 2 * SAVED_MARGIN, height + 2 * SAVED_MARGIN);
    let mut canvas = Canvas::new(to_w, to_h);
    if (w as i32, h as i32) == (to_w, to_h) {
        canvas.pixels = pixels;
    } else {
        // Cover: the larger scale, centred; bilinear.
        let scale = (to_w as f32 / w as f32).max(to_h as f32 / h as f32);
        let (ox, oy) = (
            (w as f32 - to_w as f32 / scale) / 2.0,
            (h as f32 - to_h as f32 / scale) / 2.0,
        );
        let at = |x: i32, y: i32| {
            pixels[(y.clamp(0, h as i32 - 1) as u32 * w + x.clamp(0, w as i32 - 1) as u32) as usize]
        };
        for y in 0..to_h {
            let sy = oy + (y as f32 + 0.5) / scale - 0.5;
            let (y0, fy) = (sy.floor() as i32, sy - sy.floor());
            for x in 0..to_w {
                let sx = ox + (x as f32 + 0.5) / scale - 0.5;
                let (x0, fx) = (sx.floor() as i32, sx - sx.floor());
                let corners = [
                    at(x0, y0),
                    at(x0 + 1, y0),
                    at(x0, y0 + 1),
                    at(x0 + 1, y0 + 1),
                ];
                let weights = [
                    (1.0 - fx) * (1.0 - fy),
                    fx * (1.0 - fy),
                    (1.0 - fx) * fy,
                    fx * fy,
                ];
                let channel = |shift: u32| {
                    let v: f32 = corners
                        .iter()
                        .zip(weights)
                        .map(|(p, k)| ((p >> shift) & 0xff) as f32 * k)
                        .sum();
                    (v + 0.5) as u32
                };
                canvas.pixels[(y * to_w + x) as usize] =
                    0xff00_0000 | channel(16) << 16 | channel(8) << 8 | channel(0);
            }
        }
    }
    // Wallpapers are opaque: flatten any transparency onto black.
    for pixel in &mut canvas.pixels {
        *pixel |= 0xff00_0000;
    }
    Some(canvas)
}

fn hash(x: u32, y: u32, seed: u32) -> u32 {
    let mut h = x.wrapping_mul(0x9e37_79b1) ^ y.wrapping_mul(0x85eb_ca77) ^ seed;
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297a_2d39);
    h ^ (h >> 15)
}

fn unit(h: u32) -> f32 {
    (h & 0xff_ffff) as f32 / 16_777_216.0
}

/// Adds premultiplied light (0..255 per channel, `a` its coverage) to a
/// premultiplied ARGB pixel.
fn add_light(pixel: &mut u32, r: f32, g: f32, b: f32, a: f32) {
    let channel =
        |p: u32, shift: u32, v: f32| (((p >> shift) & 0xff) as f32 + v).clamp(0.0, 255.0) as u32;
    let p = *pixel;
    *pixel = channel(p, 24, a * 255.0) << 24
        | channel(p, 16, r) << 16
        | channel(p, 8, g) << 8
        | channel(p, 0, b);
}

/// Adds stars to `canvas`: about `density` of 22 px cells hold one, and
/// they fade out down to `depth` of its height. `scale`: the screen's width
/// over 1080 (the System app's thumbnails draw them smaller).
fn draw_stars(canvas: &mut Canvas, density: f32, depth: f32, scale: f32) {
    let (width, height) = (canvas.width, canvas.height);
    let cell = ((22.0 * scale).round() as i32).max(3);
    for cy in 0..(height + cell - 1) / cell {
        let fade = 1.0 - (cy * cell) as f32 / (height as f32 * depth);
        if fade <= 0.0 {
            break;
        }
        for cx in 0..(width + cell - 1) / cell {
            let h = hash(cx as u32, cy as u32, 0x51a7);
            if unit(h) > density {
                continue;
            }
            let sx = (cx * cell) as f32 + unit(hash(cx as u32, cy as u32, 1)) * cell as f32;
            let sy = (cy * cell) as f32 + unit(hash(cx as u32, cy as u32, 2)) * cell as f32;
            let m = unit(hash(cx as u32, cy as u32, 3));
            // Most stars faint, a few bright.
            let bright = (0.18 + 0.82 * m * m * m) * fade.min(1.0);
            let size = ((0.55 + 1.1 * bright) * scale).max(0.45);
            let tint = unit(hash(cx as u32, cy as u32, 4));
            let color = if tint < 0.2 {
                (255.0, 214.0, 170.0)
            } else if tint < 0.55 {
                (180.0, 200.0, 255.0)
            } else {
                (235.0, 238.0, 255.0)
            };
            let glint = bright > 0.7;
            let reach = (size * 3.0).ceil() as i32
                + if glint {
                    (9.0 * scale).ceil() as i32
                } else {
                    0
                };
            for y in (sy as i32 - reach).max(0)..(sy as i32 + reach + 1).min(height) {
                for x in (sx as i32 - reach).max(0)..(sx as i32 + reach + 1).min(width) {
                    let dx = x as f32 + 0.5 - sx;
                    let dy = y as f32 + 0.5 - sy;
                    let mut k = (-(dx * dx + dy * dy) / (2.0 * size * size)).exp();
                    if glint {
                        // A faint four-point glint on the brightest.
                        let spike = |a: f32, b: f32| {
                            (-(a * a) / (0.8 * scale * scale)).exp()
                                * (-b.abs() / (3.5 * scale)).exp()
                        };
                        k += 0.35 * (spike(dx, dy) + spike(dy, dx));
                    }
                    let a = (k * bright).min(1.0);
                    if a < 0.004 {
                        continue;
                    }
                    let pixel = &mut canvas.pixels[(y * width + x) as usize];
                    add_light(pixel, color.0 * a, color.1 * a, color.2 * a, a);
                }
            }
        }
    }
}

/// Smooth 1-D value noise in 0..1 at `x` (in lattice units).
fn noise(x: f32, seed: u32) -> f32 {
    let i = x.floor();
    let f = x - i;
    let f = f * f * (3.0 - 2.0 * f);
    let a = unit(hash(i as i32 as u32, 0, seed));
    let b = unit(hash((i as i32 + 1) as u32, 0, seed));
    a + (b - a) * f
}

/// "ridge"'s mountain ranges, far to near: each a silhouette whose valleys
/// fill with haze, a rim of sunset light on its crest, and more travel the
/// nearer it is.
fn ridge_layers(width: i32, height: i32) -> Vec<Layer> {
    let (w, h) = (width as f32, height as f32);
    // (crest line, height, colour, haze, travel) per range.
    #[rustfmt::skip]
    let ranges: [(f32, f32, (f32, f32, f32), (f32, f32, f32), f32); 4] = [
        (0.590, 0.085, (128.0, 64.0, 100.0), (196.0, 110.0, 126.0), 0.010),
        (0.650, 0.110, (82.0, 40.0, 84.0), (150.0, 78.0, 112.0), 0.022),
        (0.730, 0.130, (44.0, 22.0, 58.0), (98.0, 50.0, 90.0), 0.040),
        (0.835, 0.150, (14.0, 8.0, 24.0), (44.0, 22.0, 52.0), 0.065),
    ];
    let sun_x = 0.64 * w;
    let mut layers = Vec::new();
    for (index, (crest, rise, color, haze, travel)) in ranges.into_iter().enumerate() {
        let seed = 0x7a3 + index as u32 * 31;
        let margin = (travel * h).ceil() as i32 + 2;
        let wavelength = w * (0.55 - 0.08 * index as f32);
        let ridge = |x: f32| {
            let mut v = 0.0;
            let mut amplitude = 0.55;
            let mut frequency = 1.0 / wavelength;
            for octave in 0..6 {
                v += amplitude * noise(x * frequency, seed + octave);
                amplitude *= 0.5;
                frequency *= 2.1;
            }
            // Sharper peaks than smooth noise gives.
            let v = ((v - 0.22) / 0.62).clamp(0.0, 1.0);
            crest * h - rise * h * v.powf(1.4)
        };
        let profile: Vec<f32> = (0..width + 2 * margin)
            .map(|x| ridge((x - margin) as f32))
            .collect();
        let top = profile.iter().fold(f32::MAX, |a, &b| a.min(b)).floor() as i32 - 2;
        let (lw, lh) = (width + 2 * margin, height + margin - top);
        let mut canvas = Canvas::new(lw, lh);
        let fog_depth = 0.16 * h;
        for x in 0..lw {
            let crest_y = profile[x as usize];
            let sx = (x - margin) as f32;
            // The crest catches the light nearest the sun.
            let sunward =
                (-((sx - sun_x) / (0.24 * w)).powi(2)).exp() * (1.0 - 0.22 * index as f32);
            for y in 0..lh {
                let sy = (y + top) as f32;
                let coverage = (sy + 0.5 - crest_y).clamp(0.0, 1.0);
                let pixel = &mut canvas.pixels[(y * lw + x) as usize];
                if coverage <= 0.0 {
                    *pixel = 0;
                    continue;
                }
                let depth = sy - crest_y;
                let fog = (depth / fog_depth).clamp(0.0, 1.0);
                let fog = fog * fog * 0.55;
                let rim = (-depth / (2.5 * w / 1080.0)).exp() * sunward * 0.45;
                let mix = |c: f32, f: f32, sun: f32| c + (f - c) * fog + sun * rim;
                let (r, g, b) = (
                    mix(color.0, haze.0, 255.0),
                    mix(color.1, haze.1, 170.0),
                    mix(color.2, haze.2, 120.0),
                );
                let noise_px = (unit(hash(x as u32, y as u32, seed)) - 0.5) * coverage;
                let channel = |v: f32| ((v * coverage + noise_px).clamp(0.0, 255.0)) as u32;
                *pixel = ((coverage * 255.0) as u32) << 24
                    | channel(r) << 16
                    | channel(g) << 8
                    | channel(b);
            }
        }
        layers.push(Layer {
            canvas,
            x: -margin as f32,
            y: top as f32,
            travel: travel * h,
            scale: 1.0,
        });
    }
    layers
}

/// HSV (hue in turns) to RGB, 0..255.
fn hsv(hue: f32, saturation: f32, value: f32) -> (f32, f32, f32) {
    let h = (hue.rem_euclid(1.0)) * 6.0;
    let c = value * saturation;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let (r, g, b) = match h as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = value - c;
    ((r + m) * 255.0, (g + m) * 255.0, (b + m) * 255.0)
}

/// "prism": a brilliant-cut gem seen from above, its facets windows of
/// different clarity in a dark glass layer that stays put, and under it a
/// spectrum of light bands and glints that slides far as the phone tilts:
/// facet by facet the gem catches colour and flashes.
fn prism_layers(width: i32, height: i32) -> Vec<Layer> {
    let (w, h) = (width as f32, height as f32);
    let unit_px = w / 1080.0;
    // The light: half resolution, scaled up (it is all smooth).
    let travel = 300.0 * unit_px;
    let margin = travel.ceil() as i32 + 4;
    let (lw, lh) = ((width + 2 * margin) / 2, (height + 2 * margin) / 2);
    let mut light = Canvas::new(lw, lh);
    let (a1, a2) = (55f32.to_radians(), (-35f32).to_radians());
    for v in 0..lh {
        for u in 0..lw {
            let x = (u * 2 - margin) as f32 / unit_px;
            let y = (v * 2 - margin) as f32 / unit_px;
            let hue = (x * a1.cos() + y * a1.sin()) / 900.0;
            let band =
                0.5 + 0.5 * (std::f32::consts::TAU * (x * a2.cos() + y * a2.sin()) / 420.0).cos();
            // Narrow bright bands with dark between: facets flash as one
            // passes beneath them.
            let value = 0.06 + 0.94 * band.powi(5);
            let (r, g, b) = hsv(hue, 0.72, value);
            light.pixels[(v * lw + u) as usize] =
                0xff00_0000 | (r as u32) << 16 | (g as u32) << 8 | b as u32;
        }
    }
    // Glints: small bright points the facets catch as they pass.
    let cell = 46;
    for cy in 0..lh / cell + 1 {
        for cx in 0..lw / cell + 1 {
            if unit(hash(cx as u32, cy as u32, 0x9117)) > 0.18 {
                continue;
            }
            let gx = (cx * cell) as f32 + unit(hash(cx as u32, cy as u32, 5)) * cell as f32;
            let gy = (cy * cell) as f32 + unit(hash(cx as u32, cy as u32, 6)) * cell as f32;
            let strength = 0.5 + 0.5 * unit(hash(cx as u32, cy as u32, 7));
            let radius = 2.0 + 2.5 * unit(hash(cx as u32, cy as u32, 8));
            let reach = (radius * 3.0) as i32;
            for y in (gy as i32 - reach).max(0)..(gy as i32 + reach).min(lh) {
                for x in (gx as i32 - reach).max(0)..(gx as i32 + reach).min(lw) {
                    let (dx, dy) = (x as f32 - gx, y as f32 - gy);
                    let k = strength * (-(dx * dx + dy * dy) / (2.0 * radius * radius)).exp();
                    add_light(
                        &mut light.pixels[(y * lw + x) as usize],
                        255.0 * k,
                        255.0 * k,
                        255.0 * k,
                        0.0,
                    );
                }
            }
        }
    }

    // The gem's facets: (polygon, clarity).
    let (cx, cy, radius) = (0.5 * w, 0.53 * h, 0.38 * w);
    let at = |r: f32, degrees: f32| {
        let a = degrees.to_radians();
        (cx + r * radius * a.cos(), cy + r * radius * a.sin())
    };
    let table: Vec<(f32, f32)> = (0..8).map(|k| at(0.42, k as f32 * 45.0 + 22.5)).collect();
    let star: Vec<(f32, f32)> = (0..8).map(|k| at(0.66, k as f32 * 45.0)).collect();
    let girdle: Vec<(f32, f32)> = (0..16).map(|j| at(1.0, j as f32 * 22.5)).collect();
    let mut facets: Vec<(Vec<(f32, f32)>, f32)> = vec![(table.clone(), 0.62)];
    for k in 0..8 {
        let even = k % 2 == 0;
        let previous = (k + 7) % 8;
        facets.push((
            vec![table[previous], table[k], star[k]],
            if even { 0.78 } else { 0.42 },
        ));
        facets.push((
            vec![table[k], star[k], girdle[2 * k + 1], star[(k + 1) % 8]],
            if even { 0.30 } else { 0.56 },
        ));
        facets.push((
            vec![star[k], girdle[2 * k], girdle[(2 * k + 15) % 16]],
            if even { 0.70 } else { 0.36 },
        ));
        facets.push((
            vec![star[k], girdle[2 * k], girdle[2 * k + 1]],
            if even { 0.40 } else { 0.74 },
        ));
    }
    // Inside a convex polygon (either winding): the distance to its nearest
    // edge, else None.
    let inside = |polygon: &[(f32, f32)], x: f32, y: f32| -> Option<f32> {
        let mut sign = 0.0_f32;
        let mut nearest = f32::MAX;
        for i in 0..polygon.len() {
            let (ax, ay) = polygon[i];
            let (bx, by) = polygon[(i + 1) % polygon.len()];
            let (ex, ey) = (bx - ax, by - ay);
            let cross = ex * (y - ay) - ey * (x - ax);
            if sign == 0.0 {
                sign = cross.signum();
            } else if cross * sign < 0.0 {
                return None;
            }
            nearest = nearest.min(cross.abs() / (ex * ex + ey * ey).sqrt());
        }
        Some(nearest)
    };
    let mut glass = Canvas::new(width, height);
    for y in 0..height {
        let t = y as f32 / h;
        let tint = (
            gfx::lerp(9.0, 3.0, t),
            gfx::lerp(9.0, 3.0, t),
            gfx::lerp(14.0, 6.0, t),
        );
        for x in 0..width {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let r = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() / radius;
            let mut clarity;
            let mut edge = 0.0;
            if r < 1.01 {
                let found = facets
                    .iter()
                    .find_map(|(polygon, clear)| inside(polygon, px, py).map(|d| (*clear, d)));
                match found {
                    Some((clear, distance)) => {
                        clarity = clear;
                        edge = (1.0 - distance / (1.4 * unit_px).max(0.9)).clamp(0.0, 1.0);
                    }
                    // Just outside the girdle's straight edges.
                    None => clarity = 0.035,
                }
            } else {
                clarity = 0.035;
            }
            if r >= 0.97 {
                // Light spilling past the girdle.
                let out = (r - 1.0).max(0.0) * radius / (70.0 * unit_px);
                clarity = clarity.max(0.035 + 0.16 * (-out * out).exp() * (r >= 1.0) as u8 as f32);
            }
            let alpha = 1.0 - clarity;
            let grain = (unit(hash(x as u32, y as u32, 0x5e)) - 0.5) * 0.8;
            let channel = |c: f32| ((c * alpha + grain).clamp(0.0, 255.0)) as u32;
            let mut pixel = ((alpha * 255.0) as u32) << 24
                | channel(tint.0) << 16
                | channel(tint.1) << 8
                | channel(tint.2);
            if edge > 0.0 {
                let a = 0.28 * edge;
                add_light(&mut pixel, 255.0 * a, 255.0 * a, 255.0 * a, a);
            }
            glass.pixels[(y * width + x) as usize] = pixel;
        }
    }
    vec![
        Layer {
            canvas: light,
            x: -margin as f32,
            y: -margin as f32,
            travel,
            scale: 2.0,
        },
        Layer {
            canvas: glass,
            x: 0.0,
            y: 0.0,
            travel: 0.0,
            scale: 1.0,
        },
    ]
}

/// "orbit"'s moon: a lit crescent moon with its halo, on
/// transparency, `size` x `size` (the moon's radius is a quarter of it).
fn sky_moon(size: i32) -> Canvas {
    let mut canvas = Canvas::new(size, size);
    let c = size as f32 / 2.0;
    let radius = size as f32 * 0.25;
    // Sunlight from the upper right and a little behind: a fat crescent.
    let light = {
        let (x, y, z) = (0.82_f32, -0.42_f32, -0.12_f32);
        let n = (x * x + y * y + z * z).sqrt();
        (x / n, y / n, z / n)
    };
    // Maria: darker, soft patches on the face.
    let maria: [(f32, f32, f32, f32); 6] = [
        (-0.30, -0.25, 0.30, 0.30),
        (0.15, -0.35, 0.22, 0.22),
        (0.32, 0.05, 0.26, 0.26),
        (-0.05, 0.30, 0.28, 0.20),
        (0.45, 0.42, 0.16, 0.18),
        (-0.48, 0.15, 0.18, 0.15),
    ];
    for y in 0..size {
        for x in 0..size {
            let dx = (x as f32 + 0.5 - c) / radius;
            let dy = (y as f32 + 0.5 - c) / radius;
            let r2 = dx * dx + dy * dy;
            let r = r2.sqrt();
            let pixel = &mut canvas.pixels[(y * size + x) as usize];
            *pixel = 0;
            // The halo, brighter on the lit side.
            let lit_side = (0.55 + 0.45 * (dx * light.0 + dy * light.1) / r.max(1e-3)).max(0.0);
            let out = (r - 1.0).max(0.0);
            let halo = 0.30 * (-out * out / 0.18).exp() * lit_side + 0.10 * (-out / 0.9).exp();
            // Gone before the layer's edge (the disc's radius is a quarter
            // of the size, so the edge is 2 radii away at the closest).
            let fade = ((1.95 - r) / 0.7).clamp(0.0, 1.0);
            let halo = halo * fade * fade;
            let coverage = ((1.0 - r) * radius + 0.5).clamp(0.0, 1.0);
            if coverage < 1.0 {
                let a = halo * (1.0 - coverage);
                add_light(pixel, 150.0 * a, 150.0 * a, 255.0 * a, a);
            }
            if coverage <= 0.0 {
                continue;
            }
            let nz = (1.0 - r2.min(1.0)).sqrt();
            let lambert = dx * light.0 + dy * light.1 + nz * light.2;
            // A soft terminator.
            let day = ((lambert + 0.08) / 0.30).clamp(0.0, 1.0);
            let day = day * day * (3.0 - 2.0 * day);
            let mut albedo = 1.0;
            for (mx, my, sx, sy) in maria {
                let ex = (dx - mx) / sx;
                let ey = (dy - my) / sy;
                albedo -= 0.30 * (-(ex * ex + ey * ey)).exp();
            }
            let grain = unit(hash(x as u32, y as u32, 9)) - 0.5;
            albedo += grain * 0.05;
            // Limb darkening on the day side, earthshine on the night side.
            let limb = 0.65 + 0.35 * nz;
            let shade = day * limb * albedo;
            let (r_, g_, b_) = (
                (236.0 * shade + 30.0 * albedo).min(255.0),
                (232.0 * shade + 32.0 * albedo).min(255.0),
                (248.0 * shade + 72.0 * albedo).min(255.0),
            );
            let a = coverage;
            // Opaque over the halo: the disc hides the stars behind it.
            let under = 1.0 - a;
            let p = *pixel;
            let mix = |shift: u32, v: f32| {
                (((p >> shift) & 0xff) as f32 * under + v * a).clamp(0.0, 255.0) as u32
            };
            *pixel = mix(24, 255.0) << 24 | mix(16, r_) << 16 | mix(8, g_) << 8 | mix(0, b_);
        }
    }
    canvas
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_wallpaper_renders_and_black_is_black() {
        for spec in &WALLPAPERS {
            let canvas = render(spec.id, 54, 117);
            assert_eq!(canvas.pixels.len(), 54 * 117);
        }
        assert!(
            render("black", 30, 60)
                .pixels
                .iter()
                .all(|p| p & 0xffffff == 0)
        );
        assert_eq!(find("nope").id, "moon");
        assert!(is_dynamic("orbit") && !is_dynamic("moon"));
        let moon = sky_moon(64);
        assert_eq!(moon.pixels[0] >> 24, 0);
        assert_eq!(moon.pixels[32 * 64 + 32] >> 24, 255);
        for id in ["orbit", "ridge", "prism"] {
            let (still, layers) = build(id, 108, 234);
            assert_eq!(still.pixels.len(), 108 * 234);
            assert!(!layers.is_empty());
        }
        assert!(build("moon", 10, 10).1.is_empty());
        // A missing saved picture shows the default.
        assert!(!exists("saved:nope.png") && !exists("saved:../x.png"));
        assert!(build("saved:nope.png", 10, 10).1.is_empty());
    }
}
