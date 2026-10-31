//! Built-in wallpapers: a vertical base gradient lit by soft glows, drawn
//! procedurally at any size (the System app draws its thumbnails with the
//! same table; docs/settings.md lists it). The panel is AMOLED, so dark
//! wallpapers cost less power: "black" lights no pixel at all.

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

pub const WALLPAPERS: [Wallpaper; 7] = [
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
    // move as the phone tilts (`sky_stars`, `sky_moon`, tilt.rs).
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
];

pub fn find(id: &str) -> &'static Wallpaper {
    WALLPAPERS
        .iter()
        .find(|w| w.id == id)
        .unwrap_or(&WALLPAPERS[0])
}

pub fn exists(id: &str) -> bool {
    WALLPAPERS.iter().any(|w| w.id == id)
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
    id == "orbit"
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

/// The far layer of "orbit": stars on transparency, `width` x `height`.
pub fn sky_stars(width: i32, height: i32) -> Canvas {
    let mut canvas = Canvas::new(width, height);
    canvas.pixels.fill(0);
    const CELL: i32 = 22;
    for cy in 0..(height + CELL - 1) / CELL {
        for cx in 0..(width + CELL - 1) / CELL {
            let h = hash(cx as u32, cy as u32, 0x51a7);
            if unit(h) > 0.16 {
                continue;
            }
            let sx = (cx * CELL) as f32 + unit(hash(cx as u32, cy as u32, 1)) * CELL as f32;
            let sy = (cy * CELL) as f32 + unit(hash(cx as u32, cy as u32, 2)) * CELL as f32;
            let m = unit(hash(cx as u32, cy as u32, 3));
            // Most stars faint, a few bright.
            let bright = 0.18 + 0.82 * m * m * m;
            let size = 0.55 + 1.1 * bright;
            let tint = unit(hash(cx as u32, cy as u32, 4));
            let color = if tint < 0.2 {
                (255.0, 214.0, 170.0)
            } else if tint < 0.55 {
                (180.0, 200.0, 255.0)
            } else {
                (235.0, 238.0, 255.0)
            };
            let reach = (size * 3.0).ceil() as i32 + if bright > 0.7 { 9 } else { 0 };
            for y in (sy as i32 - reach).max(0)..(sy as i32 + reach + 1).min(height) {
                for x in (sx as i32 - reach).max(0)..(sx as i32 + reach + 1).min(width) {
                    let dx = x as f32 + 0.5 - sx;
                    let dy = y as f32 + 0.5 - sy;
                    let mut k = (-(dx * dx + dy * dy) / (2.0 * size * size)).exp();
                    if bright > 0.7 {
                        // A faint four-point glint on the brightest.
                        let spike =
                            |a: f32, b: f32| (-(a * a) / 0.8).exp() * (-b.abs() / 3.5).exp();
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
    canvas
}

/// The near layer of "orbit": a lit crescent moon with its halo, on
/// transparency, `size` x `size` (the moon's radius is a quarter of it).
pub fn sky_moon(size: i32) -> Canvas {
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
        assert_eq!(sky_stars(90, 120).pixels.len(), 90 * 120);
    }
}
