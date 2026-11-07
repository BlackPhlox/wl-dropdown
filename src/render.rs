//! A small software canvas over a tiny-skia pixmap.
//!
//! Callers work in logical pixels; the canvas multiplies by the output scale
//! so the buffer maps 1:1 to device pixels. Nothing is ever resampled, which
//! is what keeps text sharp at fractional scales — the problem the C++
//! version had to work around in its scene-graph node.

use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Rect, Transform};

use crate::color::Rgb;
use crate::fonts::{Fonts, Style};

/// A colour with alpha, straight (not premultiplied).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: f32,
}

impl Rgba {
    pub const TRANSPARENT: Rgba = Rgba {
        r: 0,
        g: 0,
        b: 0,
        a: 0.0,
    };

    pub const fn new(r: u8, g: u8, b: u8, a: f32) -> Self {
        Self { r, g, b, a }
    }

    pub const fn opaque(c: Rgb) -> Self {
        Self {
            r: c.r,
            g: c.g,
            b: c.b,
            a: 1.0,
        }
    }

    pub const fn with_alpha(c: Rgb, a: f32) -> Self {
        Self {
            r: c.r,
            g: c.g,
            b: c.b,
            a,
        }
    }

    /// White at the given alpha, as the QML's `Qt.rgba(1, 1, 1, a)`.
    pub const fn white(a: f32) -> Self {
        Self {
            r: 255,
            g: 255,
            b: 255,
            a,
        }
    }

    pub const fn grey(v: u8) -> Self {
        Self {
            r: v,
            g: v,
            b: v,
            a: 1.0,
        }
    }

    fn paint(self) -> Paint<'static> {
        let mut p = Paint::default();
        p.set_color_rgba8(
            self.r,
            self.g,
            self.b,
            (self.a.clamp(0.0, 1.0) * 255.0).round() as u8,
        );
        p.anti_alias = true;
        p
    }
}

/// Per-corner radii, clockwise from top-left.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Radii {
    pub top_left: f64,
    pub top_right: f64,
    pub bottom_right: f64,
    pub bottom_left: f64,
}

impl Radii {
    pub fn all(r: f64) -> Self {
        Self {
            top_left: r,
            top_right: r,
            bottom_right: r,
            bottom_left: r,
        }
    }
}

pub struct Canvas<'a> {
    pub pixmap: &'a mut Pixmap,
    pub scale: f64,
    /// Logical offset added to every coordinate.
    origin: (f64, f64),
    /// Logical clip rectangle, if any.
    clip: Option<(f64, f64, f64, f64)>,
}

impl<'a> Canvas<'a> {
    pub fn new(pixmap: &'a mut Pixmap, scale: f64) -> Self {
        Self {
            pixmap,
            scale,
            origin: (0.0, 0.0),
            clip: None,
        }
    }

    /// A canvas whose pixmap holds only part of the surface: `origin` is
    /// added to every logical coordinate, e.g. minus the part's top-left.
    pub fn with_origin(pixmap: &'a mut Pixmap, scale: f64, origin: (f64, f64)) -> Self {
        Self {
            pixmap,
            scale,
            origin,
            clip: None,
        }
    }

    pub fn clear(&mut self) {
        self.pixmap.fill(tiny_skia::Color::TRANSPARENT);
    }

    /// Runs `f` with the origin moved by `(dx, dy)`.
    pub fn translated<R>(&mut self, dx: f64, dy: f64, f: impl FnOnce(&mut Self) -> R) -> R {
        let saved = self.origin;
        self.origin = (saved.0 + dx, saved.1 + dy);
        let r = f(self);
        self.origin = saved;
        r
    }

    /// Runs `f` clipped to a logical rectangle (relative to the origin).
    pub fn clipped<R>(
        &mut self,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let saved = self.clip;
        let (ax, ay) = (x + self.origin.0, y + self.origin.1);
        let new = match saved {
            Some((cx, cy, cw, ch)) => {
                let l = ax.max(cx);
                let t = ay.max(cy);
                let r = (ax + w).min(cx + cw);
                let b = (ay + h).min(cy + ch);
                (l, t, (r - l).max(0.0), (b - t).max(0.0))
            }
            None => (ax, ay, w, h),
        };
        self.clip = Some(new);
        let r = f(self);
        self.clip = saved;
        r
    }

    fn mask(&self) -> Option<tiny_skia::Mask> {
        let (x, y, w, h) = self.clip?;
        let mut mask = tiny_skia::Mask::new(self.pixmap.width(), self.pixmap.height())?;
        let s = self.scale as f32;
        if let Some(rect) = Rect::from_xywh(x as f32 * s, y as f32 * s, w as f32 * s, h as f32 * s)
        {
            mask.fill_path(
                &PathBuilder::from_rect(rect),
                FillRule::Winding,
                false,
                Transform::identity(),
            );
        }
        Some(mask)
    }

    /// Device-pixel bounds of a logical rect (clip applied), for direct
    /// pixel loops.
    fn device_clip(&self) -> (i32, i32, i32, i32) {
        let (pw, ph) = (self.pixmap.width() as i32, self.pixmap.height() as i32);
        match self.clip {
            Some((x, y, w, h)) => {
                let s = self.scale;
                (
                    ((x * s).floor() as i32).max(0),
                    ((y * s).floor() as i32).max(0),
                    (((x + w) * s).ceil() as i32).min(pw),
                    (((y + h) * s).ceil() as i32).min(ph),
                )
            }
            None => (0, 0, pw, ph),
        }
    }

    pub fn fill_rect(&mut self, x: f64, y: f64, w: f64, h: f64, color: Rgba) {
        if color.a <= 0.0 || w <= 0.0 || h <= 0.0 {
            return;
        }
        let s = self.scale;
        let (mut l, mut t) = (x + self.origin.0, y + self.origin.1);
        let (mut r, mut b) = (l + w, t + h);
        // Rectangles clip geometrically; a mask per cell would be costly.
        if let Some((cx, cy, cw, ch)) = self.clip {
            l = l.max(cx);
            t = t.max(cy);
            r = r.min(cx + cw);
            b = b.min(cy + ch);
        }
        if r <= l || b <= t {
            return;
        }
        let Some(rect) = Rect::from_ltrb(
            (l * s) as f32,
            (t * s) as f32,
            (r * s) as f32,
            (b * s) as f32,
        ) else {
            return;
        };
        self.pixmap
            .fill_rect(rect, &color.paint(), Transform::identity(), None);
    }

    /// A rectangle snapped outward to whole device pixels: cell backgrounds,
    /// so adjacent cells meet without anti-aliased seams.
    pub fn fill_rect_snapped(&mut self, x: f64, y: f64, w: f64, h: f64, color: Rgba) {
        let s = self.scale;
        let (ax, ay) = (x + self.origin.0, y + self.origin.1);
        let l = (ax * s).round();
        let t = (ay * s).round();
        let r = ((ax + w) * s).round();
        let b = ((ay + h) * s).round();
        let saved = self.origin;
        self.origin = (0.0, 0.0);
        self.fill_rect(l / s, t / s, (r - l) / s, (b - t) / s, color);
        self.origin = saved;
    }

    pub fn fill_round_rect(&mut self, x: f64, y: f64, w: f64, h: f64, radii: Radii, color: Rgba) {
        if color.a <= 0.0 || w <= 0.0 || h <= 0.0 {
            return;
        }
        let s = self.scale;
        let (x, y) = ((x + self.origin.0) * s, (y + self.origin.1) * s);
        let (w, h) = (w * s, h * s);
        let lim = (w.min(h) / 2.0).max(0.0);
        let r = |v: f64| (v * s).clamp(0.0, lim) as f32;
        let (tl, tr, br, bl) = (
            r(radii.top_left),
            r(radii.top_right),
            r(radii.bottom_right),
            r(radii.bottom_left),
        );
        let (x, y, w, h) = (x as f32, y as f32, w as f32, h as f32);
        // Cubic approximation of a quarter circle.
        const K: f32 = 0.552_284_8;
        let mut pb = PathBuilder::new();
        pb.move_to(x + tl, y);
        pb.line_to(x + w - tr, y);
        if tr > 0.0 {
            pb.cubic_to(
                x + w - tr * (1.0 - K),
                y,
                x + w,
                y + tr * (1.0 - K),
                x + w,
                y + tr,
            );
        }
        pb.line_to(x + w, y + h - br);
        if br > 0.0 {
            pb.cubic_to(
                x + w,
                y + h - br * (1.0 - K),
                x + w - br * (1.0 - K),
                y + h,
                x + w - br,
                y + h,
            );
        }
        pb.line_to(x + bl, y + h);
        if bl > 0.0 {
            pb.cubic_to(
                x + bl * (1.0 - K),
                y + h,
                x,
                y + h - bl * (1.0 - K),
                x,
                y + h - bl,
            );
        }
        pb.line_to(x, y + tl);
        if tl > 0.0 {
            pb.cubic_to(x, y + tl * (1.0 - K), x + tl * (1.0 - K), y, x + tl, y);
        }
        pb.close();
        let Some(path) = pb.finish() else { return };
        let mask = self.mask();
        self.pixmap.fill_path(
            &path,
            &color.paint(),
            FillRule::Winding,
            Transform::identity(),
            mask.as_ref(),
        );
    }

    /// Fills an arbitrary path given in logical coordinates.
    pub fn fill_path(
        &mut self,
        build: impl FnOnce(&mut PathBuilder, f32, (f32, f32)),
        color: Rgba,
    ) {
        let mut pb = PathBuilder::new();
        build(
            &mut pb,
            self.scale as f32,
            (self.origin.0 as f32, self.origin.1 as f32),
        );
        let Some(path) = pb.finish() else { return };
        let mask = self.mask();
        self.pixmap.fill_path(
            &path,
            &color.paint(),
            FillRule::EvenOdd,
            Transform::identity(),
            mask.as_ref(),
        );
    }

    #[allow(clippy::too_many_arguments)]
    /// Draws one character with its pen at logical `x` on the logical
    /// baseline `y`. Returns false if no font has the character.
    pub fn draw_char(
        &mut self,
        fonts: &mut Fonts,
        c: char,
        style: Style,
        px_em: f64,
        x: f64,
        y: f64,
        color: Rgba,
    ) -> bool {
        let s = self.scale;
        let dx = (x + self.origin.0) * s;
        let baseline = ((y + self.origin.1) * s).round() as i32;
        let Some((bitmap, pen)) = fonts.glyph(c, style, px_em * s, dx) else {
            return false;
        };
        let (cl, ct, cr, cb) = self.device_clip();
        let alpha = color.a.clamp(0.0, 1.0);
        let width = self.pixmap.width() as i32;
        let data = self.pixmap.data_mut();
        for gy in 0..bitmap.height as i32 {
            let py = baseline + bitmap.top + gy;
            if py < ct || py >= cb {
                continue;
            }
            for gx in 0..bitmap.width as i32 {
                let px = pen + bitmap.left + gx;
                if px < cl || px >= cr {
                    continue;
                }
                let cov = bitmap.coverage[(gy * bitmap.width as i32 + gx) as usize];
                if cov == 0 {
                    continue;
                }
                let a = cov as f32 / 255.0 * alpha;
                let i = ((py * width + px) * 4) as usize;
                // Source-over onto premultiplied RGBA.
                let inv = 1.0 - a;
                let blend = |src: u8, dst: u8| (src as f32 * a + dst as f32 * inv).round() as u8;
                data[i] = blend(color.r, data[i]);
                data[i + 1] = blend(color.g, data[i + 1]);
                data[i + 2] = blend(color.b, data[i + 2]);
                data[i + 3] = ((a + data[i + 3] as f32 / 255.0 * inv) * 255.0).round() as u8;
            }
        }
        true
    }

    /// Draws a string with ordinary advances; returns its logical width.
    pub fn draw_text(
        &mut self,
        fonts: &mut Fonts,
        text: &str,
        px_em: f64,
        x: f64,
        y: f64,
        color: Rgba,
    ) -> f64 {
        let mut pen = x;
        let mut buf = [0u8; 4];
        for c in text.chars() {
            self.draw_char(fonts, c, Style::REGULAR, px_em, pen, y, color);
            pen += fonts.measure(c.encode_utf8(&mut buf), px_em);
        }
        pen - x
    }
}

/// Converts the canvas's premultiplied RGBA to Wayland's ARGB8888, which is
/// premultiplied BGRA in memory on little-endian machines.
pub fn rgba_to_argb8888(src: &[u8], dst: &mut [u8]) {
    let (src, _) = src.as_chunks::<4>();
    let (dst, _) = dst.as_chunks_mut::<4>();
    for (s, d) in src.iter().zip(dst.iter_mut()) {
        *d = [s[2], s[1], s[0], s[3]];
    }
}
