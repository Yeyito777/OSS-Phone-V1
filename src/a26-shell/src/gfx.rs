//! Moon's software rasterizer.
//!
//! Every Moon surface is drawn into a client-side XRGB8888 `Canvas` and then
//! uploaded by `present::Presenter`, which sends only the pixels that changed.
//! Shapes are anti-aliased analytically (signed distance for rounded
//! rectangles and circles, exact area coverage for paths), so the shell renders
//! crisply on the A26's ~450 ppi panel without any X11 drawing extension.

/// Packed `0x00RRGGBB`, matching the A26 Xorg depth-24 little-endian layout.
pub type Rgb = u32;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl RectF {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    pub fn center(self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    pub fn inset(self, amount: f32) -> Self {
        Self::new(
            self.x + amount,
            self.y + amount,
            (self.w - 2.0 * amount).max(0.0),
            (self.h - 2.0 * amount).max(0.0),
        )
    }

    pub fn lerp(self, other: Self, t: f32) -> Self {
        Self::new(
            lerp(self.x, other.x, t),
            lerp(self.y, other.y, t),
            lerp(self.w, other.w, t),
            lerp(self.h, other.h, t),
        )
    }
}

pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Mixes two colors; `t` = 0 returns `a`.
pub fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    blend(a, b, alpha_256(t))
}

#[inline]
fn alpha_256(alpha: f32) -> u32 {
    (alpha.clamp(0.0, 1.0) * 256.0 + 0.5) as u32
}

/// Source-over blend of an opaque `src` with coverage `alpha` (0..=256).
#[inline]
pub fn blend(dst: u32, src: u32, alpha: u32) -> u32 {
    if alpha >= 256 {
        return src;
    }
    let inverse = 256 - alpha;
    let rb = (((src & 0x00ff_00ff) * alpha + (dst & 0x00ff_00ff) * inverse) >> 8) & 0x00ff_00ff;
    let g = (((src & 0x0000_ff00) * alpha + (dst & 0x0000_ff00) * inverse) >> 8) & 0x0000_ff00;
    rb | g
}

/// A premultiplied ARGB image (alpha in the top byte).
#[derive(Clone)]
pub struct Image {
    pub width: i32,
    pub height: i32,
    pub pixels: Vec<u32>,
}

impl Image {
    /// Builds an opaque image from BGRX bytes, the app icon interchange format.
    pub fn from_bgrx(width: i32, height: i32, bytes: &[u8]) -> Option<Self> {
        if bytes.len() != width as usize * height as usize * 4 {
            return None;
        }
        let pixels = bytes
            .chunks_exact(4)
            .map(|p| 0xff00_0000 | u32::from(p[2]) << 16 | u32::from(p[1]) << 8 | u32::from(p[0]))
            .collect();
        Some(Self {
            width,
            height,
            pixels,
        })
    }

    /// Area-averaged resample to `size`x`size`, clipped to a rounded square
    /// with an anti-aliased edge. This is how Moon gives every app icon the
    /// same continuous-corner silhouette regardless of the supplied artwork.
    pub fn icon_mask(&self, size: i32, radius: f32) -> Self {
        let mut pixels = vec![0_u32; (size * size) as usize];
        let sx = self.width as f32 / size as f32;
        let sy = self.height as f32 / size as f32;
        let half = size as f32 / 2.0;
        for y in 0..size {
            for x in 0..size {
                let x0 = x as f32 * sx;
                let y0 = y as f32 * sy;
                let (mut r, mut g, mut b, mut n) = (0_u32, 0_u32, 0_u32, 0_u32);
                let mut yy = y0 as i32;
                while (yy as f32) < y0 + sy && yy < self.height {
                    let mut xx = x0 as i32;
                    while (xx as f32) < x0 + sx && xx < self.width {
                        let p = self.pixels[(yy * self.width + xx) as usize];
                        r += p >> 16 & 0xff;
                        g += p >> 8 & 0xff;
                        b += p & 0xff;
                        n += 1;
                        xx += 1;
                    }
                    yy += 1;
                }
                let n = n.max(1);
                let coverage = coverage(sd_rrect(
                    x as f32 + 0.5,
                    y as f32 + 0.5,
                    half,
                    half,
                    half,
                    half,
                    radius,
                ));
                let a = (coverage * 255.0 + 0.5) as u32;
                let premultiply = |c: u32| (c / n * a + 127) / 255;
                pixels[(y * size + x) as usize] =
                    a << 24 | premultiply(r) << 16 | premultiply(g) << 8 | premultiply(b);
            }
        }
        Self {
            width: size,
            height: size,
            pixels,
        }
    }
}

#[inline]
fn coverage(signed_distance: f32) -> f32 {
    (0.5 - signed_distance).clamp(0.0, 1.0)
}

#[inline]
fn sd_rrect(px: f32, py: f32, cx: f32, cy: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let qx = (px - cx).abs() - (hw - r);
    let qy = (py - cy).abs() - (hh - r);
    let ox = qx.max(0.0);
    let oy = qy.max(0.0);
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r
}

pub struct Canvas {
    pub width: i32,
    pub height: i32,
    pub pixels: Vec<u32>,
    /// Drawing is limited to `x0..x1`, `y0..y1`.
    clip: (i32, i32, i32, i32),
}

impl Canvas {
    pub fn new(width: i32, height: i32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; (width.max(0) * height.max(0)) as usize],
            clip: (0, 0, width, height),
        }
    }

    /// Restricts subsequent drawing to a rectangle (in pixels).
    pub fn set_clip(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        self.clip = (
            x0.clamp(0, self.width),
            y0.clamp(0, self.height),
            x1.clamp(0, self.width),
            y1.clamp(0, self.height),
        );
    }

    pub fn reset_clip(&mut self) {
        self.clip = (0, 0, self.width, self.height);
    }

    #[inline]
    fn visible(&self, x: i32, y: i32) -> bool {
        x >= self.clip.0 && y >= self.clip.1 && x < self.clip.2 && y < self.clip.3
    }

    pub fn clear(&mut self, color: Rgb) {
        self.pixels.fill(color);
    }

    pub fn copy_from(&mut self, other: &Canvas) {
        if other.pixels.len() == self.pixels.len() {
            self.pixels.copy_from_slice(&other.pixels);
        }
    }

    /// Copies rows `y0..y1` from another canvas of the same width.
    pub fn copy_rows_from(&mut self, other: &Canvas, y0: i32, y1: i32) {
        if other.width != self.width {
            return;
        }
        let y0 = y0.clamp(0, self.height.min(other.height));
        let y1 = y1.clamp(y0, self.height.min(other.height));
        let a = (y0 * self.width) as usize;
        let b = (y1 * self.width) as usize;
        self.pixels[a..b].copy_from_slice(&other.pixels[a..b]);
    }

    #[inline]
    fn put(&mut self, x: i32, y: i32, color: Rgb, alpha: u32) {
        if alpha == 0 || !self.visible(x, y) {
            return;
        }
        let index = (y * self.width + x) as usize;
        self.pixels[index] = blend(self.pixels[index], color, alpha);
    }

    #[inline]
    fn span(&mut self, x0: i32, x1: i32, y: i32, color: Rgb, alpha: u32) {
        if alpha == 0 || y < self.clip.1 || y >= self.clip.3 {
            return;
        }
        let x0 = x0.max(self.clip.0);
        let x1 = x1.min(self.clip.2);
        if x0 >= x1 {
            return;
        }
        let row = (y * self.width) as usize;
        let slice = &mut self.pixels[row + x0 as usize..row + x1 as usize];
        if alpha >= 256 {
            slice.fill(color);
        } else {
            for pixel in slice {
                *pixel = blend(*pixel, color, alpha);
            }
        }
    }

    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: Rgb, alpha: f32) {
        let a = alpha_256(alpha);
        for row in y.max(0)..(y + h).min(self.height) {
            self.span(x, x + w, row, color, a);
        }
    }

    /// Anti-aliased filled rounded rectangle; a radius of half the shorter
    /// side produces a pill or circle.
    pub fn fill_rrect(&mut self, rect: RectF, radius: f32, color: Rgb, alpha: f32) {
        if rect.w <= 0.0 || rect.h <= 0.0 || alpha <= 0.0 {
            return;
        }
        let hw = rect.w / 2.0;
        let hh = rect.h / 2.0;
        let r = radius.clamp(0.0, hw.min(hh));
        let (cx, cy) = rect.center();
        let y0 = (rect.y.floor() as i32).max(0);
        let y1 = ((rect.y + rect.h).ceil() as i32).min(self.height);
        let x0 = (rect.x.floor() as i32).max(0);
        let x1 = ((rect.x + rect.w).ceil() as i32).min(self.width);
        // Pixels whose centers are at least one pixel inside the straight
        // edges share the coverage of the center column. Away from the
        // corners the edges are vertical, so only the outermost two pixels
        // on each side need the full distance evaluation.
        let corner_x0 = ((cx - hw + r + 1.0) - 0.5).ceil() as i32;
        let corner_x1 = ((cx + hw - r - 1.0) - 0.5).floor() as i32 + 1;
        let straight_x0 = ((cx - hw + 1.0) - 0.5).ceil() as i32;
        let straight_x1 = ((cx + hw - 1.0) - 0.5).floor() as i32 + 1;
        for y in y0..y1 {
            let py = y as f32 + 0.5;
            let center = coverage(sd_rrect(cx, py, cx, cy, hw, hh, r));
            let straight_row = (py - cy).abs() <= hh - r;
            let (inner_x0, inner_x1) = if straight_row {
                (straight_x0, straight_x1)
            } else {
                (corner_x0, corner_x1)
            };
            let (ix0, ix1) = if inner_x0 < inner_x1 && (straight_row || hw - r >= 1.0) {
                (inner_x0.max(x0), inner_x1.min(x1))
            } else {
                (x1, x1)
            };
            for x in x0..ix0.min(x1) {
                let c = coverage(sd_rrect(x as f32 + 0.5, py, cx, cy, hw, hh, r));
                self.put(x, y, color, alpha_256(c * alpha));
            }
            if ix0 < ix1 {
                self.span(ix0, ix1, y, color, alpha_256(center * alpha));
            }
            for x in ix1.max(x0)..x1 {
                let c = coverage(sd_rrect(x as f32 + 0.5, py, cx, cy, hw, hh, r));
                self.put(x, y, color, alpha_256(c * alpha));
            }
        }
    }

    /// Anti-aliased rounded-rectangle outline of the given stroke width,
    /// drawn inside `rect`.
    pub fn stroke_rrect(&mut self, rect: RectF, radius: f32, width: f32, color: Rgb, alpha: f32) {
        let hw = rect.w / 2.0;
        let hh = rect.h / 2.0;
        let r = radius.clamp(0.0, hw.min(hh));
        let ir = (r - width).max(0.0);
        let (cx, cy) = rect.center();
        let y0 = (rect.y.floor() as i32).max(0);
        let y1 = ((rect.y + rect.h).ceil() as i32).min(self.height);
        let x0 = (rect.x.floor() as i32).max(0);
        let x1 = ((rect.x + rect.w).ceil() as i32).min(self.width);
        for y in y0..y1 {
            let py = y as f32 + 0.5;
            for x in x0..x1 {
                let px = x as f32 + 0.5;
                let outer = coverage(sd_rrect(px, py, cx, cy, hw, hh, r));
                if outer <= 0.0 {
                    continue;
                }
                let inner = coverage(sd_rrect(px, py, cx, cy, hw - width, hh - width, ir));
                self.put(x, y, color, alpha_256((outer - inner).max(0.0) * alpha));
            }
        }
    }

    pub fn fill_circle(&mut self, cx: f32, cy: f32, radius: f32, color: Rgb, alpha: f32) {
        self.fill_rrect(
            RectF::new(cx - radius, cy - radius, radius * 2.0, radius * 2.0),
            radius,
            color,
            alpha,
        );
    }

    pub fn stroke_circle(
        &mut self,
        cx: f32,
        cy: f32,
        radius: f32,
        width: f32,
        color: Rgb,
        alpha: f32,
    ) {
        self.stroke_rrect(
            RectF::new(cx - radius, cy - radius, radius * 2.0, radius * 2.0),
            radius,
            width,
            color,
            alpha,
        );
    }

    /// A soft drop shadow for a rounded rectangle: coverage falls off over
    /// `blur` pixels outside the shape.
    pub fn shadow_rrect(&mut self, rect: RectF, radius: f32, blur: f32, color: Rgb, alpha: f32) {
        let hw = rect.w / 2.0;
        let hh = rect.h / 2.0;
        let r = radius.clamp(0.0, hw.min(hh));
        let (cx, cy) = rect.center();
        let y0 = ((rect.y - blur).floor() as i32).max(0);
        let y1 = ((rect.y + rect.h + blur).ceil() as i32).min(self.height);
        let x0 = ((rect.x - blur).floor() as i32).max(0);
        let x1 = ((rect.x + rect.w + blur).ceil() as i32).min(self.width);
        for y in y0..y1 {
            let py = y as f32 + 0.5;
            for x in x0..x1 {
                let d = sd_rrect(x as f32 + 0.5, py, cx, cy, hw, hh, r);
                if d >= blur {
                    continue;
                }
                let t = ((d + blur) / (2.0 * blur)).clamp(0.0, 1.0);
                let falloff = 1.0 - t * t * (3.0 - 2.0 * t);
                self.put(x, y, color, alpha_256(falloff * alpha));
            }
        }
    }

    /// Anti-aliased arc of a ring between angles `start` and `end` (radians,
    /// 0 = 12 o'clock, clockwise) with round caps.
    #[allow(clippy::too_many_arguments)]
    pub fn ring_arc(
        &mut self,
        cx: f32,
        cy: f32,
        radius: f32,
        width: f32,
        start: f32,
        end: f32,
        color: Rgb,
        alpha: f32,
    ) {
        let half = width / 2.0;
        let full = (end - start).abs() >= std::f32::consts::TAU - 1e-3;
        let cap = |angle: f32| (cx + radius * angle.sin(), cy - radius * angle.cos());
        let (sx, sy) = cap(start);
        let (ex, ey) = cap(end);
        let extent = radius + half + 1.0;
        let y0 = ((cy - extent).floor() as i32).max(0);
        let y1 = ((cy + extent).ceil() as i32).min(self.height);
        let x0 = ((cx - extent).floor() as i32).max(0);
        let x1 = ((cx + extent).ceil() as i32).min(self.width);
        for y in y0..y1 {
            let py = y as f32 + 0.5;
            for x in x0..x1 {
                let px = x as f32 + 0.5;
                let dx = px - cx;
                let dy = py - cy;
                let distance = (dx * dx + dy * dy).sqrt();
                let ring = (distance - radius).abs() - half;
                let mut d = if full {
                    ring
                } else {
                    let mut angle = dx.atan2(-dy);
                    if angle < start {
                        angle += std::f32::consts::TAU;
                    }
                    if angle <= end { ring } else { f32::MAX }
                };
                if !full {
                    let to_start = ((px - sx).powi(2) + (py - sy).powi(2)).sqrt() - half;
                    let to_end = ((px - ex).powi(2) + (py - ey).powi(2)).sqrt() - half;
                    d = d.min(to_start).min(to_end);
                }
                if d < 0.5 {
                    self.put(x, y, color, alpha_256(coverage(d) * alpha));
                }
            }
        }
    }

    /// Composites a premultiplied image at an integer position.
    pub fn draw_image(&mut self, image: &Image, x: i32, y: i32, alpha: f32) {
        let a = alpha_256(alpha);
        for iy in 0..image.height {
            let ty = y + iy;
            if ty < self.clip.1 || ty >= self.clip.3 {
                continue;
            }
            for ix in 0..image.width {
                let tx = x + ix;
                if tx < self.clip.0 || tx >= self.clip.2 {
                    continue;
                }
                let src = image.pixels[(iy * image.width + ix) as usize];
                let index = (ty * self.width + tx) as usize;
                self.pixels[index] = over(self.pixels[index], src, a);
            }
        }
    }

    /// Composites a premultiplied image scaled into `dst` with bilinear
    /// filtering (used by launch/close zoom animations).
    pub fn draw_image_scaled(&mut self, image: &Image, dst: RectF, alpha: f32) {
        if dst.w < 1.0 || dst.h < 1.0 {
            return;
        }
        let a = alpha_256(alpha);
        let y0 = (dst.y.floor() as i32).max(self.clip.1);
        let y1 = ((dst.y + dst.h).ceil() as i32).min(self.clip.3);
        let x0 = (dst.x.floor() as i32).max(self.clip.0);
        let x1 = ((dst.x + dst.w).ceil() as i32).min(self.clip.2);
        let sx = image.width as f32 / dst.w;
        let sy = image.height as f32 / dst.h;
        // Per-column sample positions are shared by every row.
        let columns: Vec<(usize, usize, u32)> = (x0..x1)
            .map(|x| {
                let u = ((x as f32 + 0.5 - dst.x) * sx - 0.5).clamp(0.0, (image.width - 1) as f32);
                let ux = u as usize;
                let ux1 = (ux + 1).min(image.width as usize - 1);
                (ux, ux1, ((u - ux as f32) * 256.0) as u32)
            })
            .collect();
        let stride = image.width as usize;
        for y in y0..y1 {
            let v = ((y as f32 + 0.5 - dst.y) * sy - 0.5).clamp(0.0, (image.height - 1) as f32);
            let vy = v as usize;
            let vy1 = (vy + 1).min(image.height as usize - 1);
            let fy = ((v - vy as f32) * 256.0) as u32;
            let top = &image.pixels[vy * stride..vy * stride + stride];
            let bottom = &image.pixels[vy1 * stride..vy1 * stride + stride];
            let row = (y * self.width) as usize;
            for (offset, &(ux, ux1, fx)) in columns.iter().enumerate() {
                let upper = lerp_pixel(top[ux], top[ux1], fx);
                let lower = lerp_pixel(bottom[ux], bottom[ux1], fx);
                let src = lerp_pixel(upper, lower, fy);
                let index = row + x0 as usize + offset;
                self.pixels[index] = over(self.pixels[index], src, a);
            }
        }
    }

    /// Blends an 8-bit coverage mask (a rasterized glyph or path).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_mask(
        &mut self,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        mask: &[u8],
        color: Rgb,
        alpha: f32,
    ) {
        let a = alpha_256(alpha);
        for my in 0..height {
            let ty = y + my;
            if ty < self.clip.1 || ty >= self.clip.3 {
                continue;
            }
            let row = (ty * self.width) as usize;
            for mx in 0..width {
                let tx = x + mx;
                if tx < self.clip.0 || tx >= self.clip.2 {
                    continue;
                }
                let m = u32::from(mask[(my * width + mx) as usize]);
                if m == 0 {
                    continue;
                }
                let coverage = ((m + (m >> 7)) * a) >> 8;
                let index = row + tx as usize;
                self.pixels[index] = blend(self.pixels[index], color, coverage);
            }
        }
    }

    pub fn fill_path(&mut self, path: &Path, color: Rgb, alpha: f32) {
        if let Some((x, y, w, h, mask)) = path.rasterize() {
            self.draw_mask(x, y, w, h, &mask, color, alpha);
        }
    }

    /// Fills the whole canvas from a per-row color function (gradients).
    pub fn fill_rows(&mut self, color_at_row: impl Fn(i32) -> Rgb) {
        for y in 0..self.height {
            let color = color_at_row(y);
            self.span(0, self.width, y, color, 256);
        }
    }
}

#[inline]
fn over(dst: u32, src: u32, alpha: u32) -> u32 {
    // Premultiplied source-over with an additional global alpha.
    let sa = (src >> 24) * alpha >> 8;
    if sa == 0 {
        return dst;
    }
    let inverse = 256 - (sa + (sa >> 7));
    let scale = |shift: u32| {
        let s = (src >> shift & 0xff) * alpha >> 8;
        let d = dst >> shift & 0xff;
        (s + (d * inverse >> 8)).min(255) << shift
    };
    scale(16) | scale(8) | scale(0)
}

/// Interpolates two ARGB pixels; `t` is 0..=256.
#[inline]
fn lerp_pixel(a: u32, b: u32, t: u32) -> u32 {
    if a == b {
        return a;
    }
    let inverse = 256 - t;
    let even = ((a & 0x00ff_00ff) * inverse + (b & 0x00ff_00ff) * t) >> 8 & 0x00ff_00ff;
    let odd = ((a >> 8 & 0x00ff_00ff) * inverse + (b >> 8 & 0x00ff_00ff) * t) & 0xff00_ff00;
    even | odd
}

/// A polygonal path rasterized with exact area coverage (the accumulation
/// technique used by font rasterizers). Overlapping sub-paths of the same
/// winding union cleanly; reverse winding cuts holes.
#[derive(Default)]
pub struct Path {
    segments: Vec<((f32, f32), (f32, f32))>,
    start: Option<(f32, f32)>,
    current: Option<(f32, f32)>,
}

impl Path {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn move_to(&mut self, x: f32, y: f32) {
        self.close();
        self.start = Some((x, y));
        self.current = Some((x, y));
    }

    pub fn line_to(&mut self, x: f32, y: f32) {
        if let Some(current) = self.current {
            self.segments.push((current, (x, y)));
        } else {
            self.start = Some((x, y));
        }
        self.current = Some((x, y));
    }

    pub fn close(&mut self) {
        if let (Some(start), Some(current)) = (self.start, self.current)
            && start != current
        {
            self.segments.push((current, start));
        }
        self.current = self.start;
    }

    /// Appends a circular arc (angles in radians, 0 = +x, counter-clockwise
    /// on screen means negative y).
    pub fn arc(&mut self, cx: f32, cy: f32, r: f32, from: f32, to: f32) {
        let steps = ((to - from).abs() * r.max(1.0) / 3.0)
            .ceil()
            .clamp(4.0, 256.0) as usize;
        for step in 0..=steps {
            let angle = from + (to - from) * step as f32 / steps as f32;
            self.line_to(cx + r * angle.cos(), cy + r * angle.sin());
        }
    }

    pub fn polygon(&mut self, points: &[(f32, f32)]) {
        if let Some((first, rest)) = points.split_first() {
            self.move_to(first.0, first.1);
            for point in rest {
                self.line_to(point.0, point.1);
            }
            self.close();
        }
    }

    /// Adds a stroked line segment with round caps.
    pub fn stroke_segment(&mut self, a: (f32, f32), b: (f32, f32), width: f32) {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let length = (dx * dx + dy * dy).sqrt().max(1e-3);
        let (nx, ny) = (-dy / length * width / 2.0, dx / length * width / 2.0);
        self.polygon(&[
            (a.0 + nx, a.1 + ny),
            (b.0 + nx, b.1 + ny),
            (b.0 - nx, b.1 - ny),
            (a.0 - nx, a.1 - ny),
        ]);
        for (x, y) in [a, b] {
            self.circle(x, y, width / 2.0);
        }
    }

    pub fn stroke_polyline(&mut self, points: &[(f32, f32)], width: f32) {
        for pair in points.windows(2) {
            self.stroke_segment(pair[0], pair[1], width);
        }
    }

    pub fn circle(&mut self, cx: f32, cy: f32, r: f32) {
        // Same (clockwise in screen space) winding as `polygon` users.
        let steps = (r * 1.5).ceil().clamp(12.0, 256.0) as usize;
        self.move_to(cx + r, cy);
        for step in 1..steps {
            let angle = std::f32::consts::TAU * step as f32 / steps as f32;
            self.line_to(cx + r * angle.cos(), cy + r * angle.sin());
        }
        self.close();
    }

    fn rasterize(&self) -> Option<(i32, i32, i32, i32, Vec<u8>)> {
        let mut segments = self.segments.clone();
        if let (Some(start), Some(current)) = (self.start, self.current)
            && start != current
        {
            segments.push((current, start));
        }
        if segments.is_empty() {
            return None;
        }
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for (a, b) in &segments {
            for p in [a, b] {
                min_x = min_x.min(p.0);
                min_y = min_y.min(p.1);
                max_x = max_x.max(p.0);
                max_y = max_y.max(p.1);
            }
        }
        let ox = min_x.floor() as i32;
        let oy = min_y.floor() as i32;
        let w = (max_x.ceil() as i32 - ox + 2).max(1);
        let h = (max_y.ceil() as i32 - oy + 1).max(1);
        if w as i64 * h as i64 > 4_000_000 {
            return None;
        }
        let stride = (w + 2) as usize;
        let mut accumulation = vec![0.0_f32; stride * h as usize + 2];
        for (a, b) in segments {
            accumulate_line(
                &mut accumulation,
                stride,
                h as usize,
                (a.0 - ox as f32, a.1 - oy as f32),
                (b.0 - ox as f32, b.1 - oy as f32),
            );
        }
        let mut mask = vec![0_u8; (w * h) as usize];
        for y in 0..h as usize {
            let mut sum = 0.0_f32;
            for x in 0..w as usize {
                sum += accumulation[y * stride + x];
                mask[y * w as usize + x] = (sum.abs().min(1.0) * 255.0 + 0.5) as u8;
            }
        }
        Some((ox, oy, w, h, mask))
    }
}

fn accumulate_line(a: &mut [f32], stride: usize, h: usize, p0: (f32, f32), p1: (f32, f32)) {
    if (p0.1 - p1.1).abs() <= f32::EPSILON {
        return;
    }
    let (dir, p0, p1) = if p0.1 < p1.1 {
        (1.0, p0, p1)
    } else {
        (-1.0, p1, p0)
    };
    let dxdy = (p1.0 - p0.0) / (p1.1 - p0.1);
    let mut x = p0.0;
    let y0 = p0.1.max(0.0) as usize;
    if p0.1 < 0.0 {
        x -= p0.1 * dxdy;
    }
    let y_end = (p1.1.ceil().max(0.0) as usize).min(h);
    let mut add = |index: usize, value: f32| {
        if let Some(cell) = a.get_mut(index) {
            *cell += value;
        }
    };
    for y in y0..y_end {
        let line = y * stride;
        let dy = ((y + 1) as f32).min(p1.1) - (y as f32).max(p0.1);
        let x_next = x + dxdy * dy;
        let d = dy * dir;
        let (x0, x1) = if x < x_next { (x, x_next) } else { (x_next, x) };
        let x0 = x0.max(0.0);
        let x1 = x1.max(0.0);
        let x0_floor = x0.floor();
        let x0i = x0_floor as usize;
        let x1_ceil = x1.ceil();
        let x1i = x1_ceil as usize;
        if x1i <= x0i + 1 {
            let xmf = 0.5 * (x + x_next) - x0_floor;
            add(line + x0i, d - d * xmf);
            add(line + x0i + 1, d * xmf);
        } else {
            let s = (x1 - x0).recip();
            let x0f = x0 - x0_floor;
            let a0 = 0.5 * s * (1.0 - x0f) * (1.0 - x0f);
            let x1f = x1 - x1_ceil + 1.0;
            let am = 0.5 * s * x1f * x1f;
            add(line + x0i, d * a0);
            if x1i == x0i + 2 {
                add(line + x0i + 1, d * (1.0 - a0 - am));
            } else {
                let a1 = s * (1.5 - x0f);
                add(line + x0i + 1, d * (a1 - a0));
                for xi in x0i + 2..x1i - 1 {
                    add(line + xi, d * s);
                }
                let a2 = a1 + (x1i - x0i - 3) as f32 * s;
                add(line + x1i - 1, d * (1.0 - a2 - am));
            }
            add(line + x1i, d * am);
        }
        x = x_next;
    }
}

/// Cubic ease-out, the default Moon motion curve.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// A lightly under-damped spring-like ease (overshoots ~3%) for zooms.
pub fn ease_spring(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let c1 = 1.2;
    let c3 = c1 + 1.0;
    1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_endpoints_are_exact() {
        assert_eq!(blend(0x000000, 0xffffff, 256), 0xffffff);
        assert_eq!(blend(0x123456, 0xffffff, 0), 0x123456);
        assert!((0x7f..=0x80).contains(&(mix(0x000000, 0xff0000, 0.5) >> 16 & 0xff)));
    }

    #[test]
    fn rounded_rect_is_solid_inside_and_soft_at_corners() {
        let mut canvas = Canvas::new(100, 100);
        canvas.fill_rrect(RectF::new(10.0, 10.0, 80.0, 80.0), 20.0, 0xffffff, 1.0);
        assert_eq!(canvas.pixels[50 * 100 + 50], 0xffffff);
        assert_eq!(canvas.pixels[50 * 100 + 10], 0xffffff);
        assert_eq!(canvas.pixels[10 * 100 + 10], 0x000000);
        assert_eq!(canvas.pixels[50 * 100 + 9], 0x000000);
        let corner = canvas.pixels[15 * 100 + 16] & 0xff;
        assert!(corner > 0 && corner < 255, "corner pixel {corner}");
    }

    #[test]
    fn path_coverage_fills_polygons_and_unions_overlaps() {
        let mut path = Path::new();
        path.polygon(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)]);
        path.polygon(&[(5.0, 0.0), (15.0, 0.0), (15.0, 10.0), (5.0, 10.0)]);
        let (x, y, w, _h, mask) = path.rasterize().unwrap();
        assert_eq!((x, y), (0, 0));
        assert_eq!(mask[(5 * w + 7) as usize], 255);
        assert_eq!(mask[(5 * w + 2) as usize], 255);
        assert_eq!(mask[(5 * w + 16) as usize], 0);
    }

    #[test]
    fn icon_mask_rounds_corners_and_keeps_center() {
        let image = Image::from_bgrx(4, 4, &[255; 64]).unwrap();
        let icon = image.icon_mask(40, 10.0);
        assert_eq!(icon.pixels[0] >> 24, 0);
        assert_eq!(icon.pixels[20 * 40 + 20], 0xffff_ffff);
    }

    #[test]
    fn easing_curves_hit_endpoints() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert!((ease_spring(1.0) - 1.0).abs() < 1e-6);
        assert!(ease_spring(0.0).abs() < 1e-6);
    }
}
