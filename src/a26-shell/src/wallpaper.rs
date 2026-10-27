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

pub const WALLPAPERS: [Wallpaper; 6] = [
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
    }
}
