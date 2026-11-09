//! Fonts: finding a family, measuring the cell grid, and rasterising glyphs.
//!
//! Sizes are given in pixels per em, as Qt's point and pixel sizes were:
//! a point size becomes `pt × 96 / 72` px. Glyphs are rasterised at device
//! pixels (logical size × output scale) so text stays sharp under fractional
//! scaling, and cached per subpixel position.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use ab_glyph::{point, Font, FontArc, FontVec, GlyphId, PxScale};

/// A point size to pixels per em, at the 96 dpi Qt assumed for Wayland.
pub fn pt_to_px(pt: f64) -> f64 {
    pt * 96.0 / 72.0
}

/// Families tried, in order, when the configured one is not installed.
pub const FALLBACK_MONOSPACE: &[&str] = &[
    "Hack",
    "DejaVu Sans Mono",
    "Noto Sans Mono",
    "Liberation Mono",
    "Source Code Pro",
    "JetBrains Mono",
    "Fira Mono",
    "Ubuntu Mono",
    "Cousine",
    "FreeMono",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
}

impl Style {
    pub const REGULAR: Style = Style {
        bold: false,
        italic: false,
    };

    fn index(self) -> usize {
        usize::from(self.bold) | usize::from(self.italic) << 1
    }
}

#[derive(Clone)]
struct Face {
    font: FontArc,
    /// The family has no bold face: thicken the regular one instead.
    fake_bold: bool,
}

/// Every installed font, scanned once.
pub struct FontLibrary {
    db: fontdb::Database,
}

impl FontLibrary {
    pub fn system() -> Self {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        Self { db }
    }

    /// A library of just these files — what the screenshot tests use, so
    /// results do not depend on what the machine has installed.
    ///
    /// # Errors
    ///
    /// When one of the files cannot be read.
    pub fn from_files(paths: &[&Path]) -> std::io::Result<Self> {
        let mut db = fontdb::Database::new();
        for p in paths {
            db.load_font_file(p)?;
        }
        Ok(Self { db })
    }

    fn load(&self, id: fontdb::ID) -> Option<FontArc> {
        self.db
            .with_face_data(id, |data, index| {
                FontVec::try_from_vec_and_index(data.to_vec(), index).ok()
            })
            .flatten()
            .map(FontArc::from)
    }

    fn has_family(&self, name: &str) -> bool {
        self.db
            .faces()
            .any(|f| f.families.iter().any(|(n, _)| n.eq_ignore_ascii_case(name)))
    }

    fn canonical_family(&self, name: &str) -> Option<String> {
        self.db
            .faces()
            .flat_map(|f| f.families.iter())
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(n, _)| n.clone())
    }

    /// The family actually used for `requested`: itself if installed, else
    /// the first installed monospace fallback, else any monospaced face.
    pub fn resolve_family(&self, requested: &str) -> Option<String> {
        if !requested.trim().is_empty() {
            if let Some(name) = self.canonical_family(requested.trim()) {
                return Some(name);
            }
        }
        FALLBACK_MONOSPACE
            .iter()
            .find(|f| self.has_family(f))
            .map(std::string::ToString::to_string)
            .or_else(|| {
                self.db
                    .faces()
                    .find(|f| f.monospaced)
                    .and_then(|f| f.families.first().map(|n| n.0.clone()))
            })
            .or_else(|| {
                self.db
                    .faces()
                    .next()
                    .and_then(|f| f.families.first().map(|n| n.0.clone()))
            })
    }

    fn query(&self, family: &str, style: Style) -> Option<(fontdb::ID, bool)> {
        let q = fontdb::Query {
            families: &[fontdb::Family::Name(family)],
            weight: if style.bold {
                fontdb::Weight::BOLD
            } else {
                fontdb::Weight::NORMAL
            },
            style: if style.italic {
                fontdb::Style::Italic
            } else {
                fontdb::Style::Normal
            },
            ..Default::default()
        };
        let id = self.db.query(&q)?;
        let info = self.db.face(id)?;
        let fake_bold = style.bold && info.weight.0 < 600;
        Some((id, fake_bold))
    }

    /// Faces that might cover characters the main family lacks.
    fn fallback_ids(&self, exclude: &str) -> Vec<fontdb::ID> {
        let mut ids: Vec<_> = self
            .db
            .faces()
            .filter(|f| f.style == fontdb::Style::Normal && f.weight.0 < 600)
            .filter(|f| !f.families.iter().any(|(n, _)| n == exclude))
            .map(|f| (f.monospaced, f.id))
            .collect();
        // Monospaced faces first: their glyphs fit the grid best.
        ids.sort_by_key(|(mono, _)| !mono);
        ids.into_iter().map(|(_, id)| id).collect()
    }
}

/// Cell metrics in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellMetrics {
    pub width: f64,
    pub height: f64,
    pub ascent: f64,
    pub descent: f64,
    pub underline_offset: f64,
    pub underline_thickness: f64,
}

/// Coverage for one rasterised glyph, positioned relative to the pen.
pub struct GlyphBitmap {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub coverage: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    face: u16,
    glyph: u16,
    size_milli: u32,
    subpixel: u8,
    fake_bold: bool,
}

/// One family in four styles, plus fallbacks for missing characters.
pub struct Fonts {
    family: String,
    faces: [Face; 4],
    library: Arc<FontLibrary>,
    fallback_ids: Vec<fontdb::ID>,
    /// Fallback faces loaded so far, indexed from 4 in glyph keys.
    fallbacks: Vec<Face>,
    fallback_for: HashMap<char, Option<u16>>,
    cache: HashMap<GlyphKey, Option<Arc<GlyphBitmap>>>,
}

const SUBPIXEL_STEPS: f32 = 4.0;

impl Fonts {
    /// The family `requested` (or its fallback) from `library`. Fails only
    /// when the library holds no usable font at all.
    pub fn new(library: Arc<FontLibrary>, requested: &str) -> Option<Self> {
        let family = library.resolve_family(requested)?;
        let regular_id = library.query(&family, Style::REGULAR)?.0;
        let regular = Face {
            font: library.load(regular_id)?,
            fake_bold: false,
        };
        let load = |style: Style| -> Face {
            match library
                .query(&family, style)
                .and_then(|(id, fake)| Some((library.load(id)?, fake)))
            {
                Some((font, fake_bold)) => Face { font, fake_bold },
                None => Face {
                    font: regular.font.clone(),
                    fake_bold: style.bold,
                },
            }
        };
        let faces = [
            regular.clone(),
            load(Style {
                bold: true,
                italic: false,
            }),
            load(Style {
                bold: false,
                italic: true,
            }),
            load(Style {
                bold: true,
                italic: true,
            }),
        ];
        let fallback_ids = library.fallback_ids(&family);
        Some(Self {
            family,
            faces,
            library,
            fallback_ids,
            fallbacks: Vec::new(),
            fallback_for: HashMap::new(),
            cache: HashMap::new(),
        })
    }

    pub fn family(&self) -> &str {
        &self.family
    }

    fn scale_for(font: &FontArc, px_em: f64) -> PxScale {
        let upem = font.units_per_em().unwrap_or(1000.0);
        PxScale::from((px_em as f32) * font.height_unscaled() / upem)
    }

    /// Cell metrics for the regular face at `px_em`.
    pub fn metrics(&self, px_em: f64) -> CellMetrics {
        let font = &self.faces[0].font;
        let upem = f64::from(font.units_per_em().unwrap_or(1000.0));
        let k = px_em / upem;
        let ascent = f64::from(font.ascent_unscaled()) * k;
        let descent = f64::from(-font.descent_unscaled()) * k;
        let space = font.glyph_id(' ');
        let mut width = f64::from(font.h_advance_unscaled(space)) * k;
        if width <= 0.0 {
            width = f64::from(font.h_advance_unscaled(font.glyph_id('M'))) * k;
        }
        CellMetrics {
            width,
            height: ascent + descent,
            ascent,
            descent,
            underline_offset: (descent * 0.4).max(1.0),
            underline_thickness: (px_em / 14.0).max(1.0),
        }
    }

    fn face(&self, index: u16) -> &Face {
        let i = index as usize;
        if i < 4 {
            &self.faces[i]
        } else {
            &self.fallbacks[i - 4]
        }
    }

    /// The face (index) and glyph to draw `c` with.
    fn lookup(&mut self, c: char, style: Style) -> Option<(u16, GlyphId)> {
        let si = style.index() as u16;
        let g = self.faces[si as usize].font.glyph_id(c);
        if g.0 != 0 {
            return Some((si, g));
        }
        // Another style of the family may have it.
        let g = self.faces[0].font.glyph_id(c);
        if g.0 != 0 {
            return Some((0, g));
        }
        if c.is_control() || c == ' ' {
            return None;
        }
        let face = if let Some(f) = self.fallback_for.get(&c) {
            *f
        } else {
            let found = self.find_fallback(c);
            self.fallback_for.insert(c, found);
            found
        }?;
        Some((face, self.face(face).font.glyph_id(c)))
    }

    fn find_fallback(&mut self, c: char) -> Option<u16> {
        if let Some(i) = self
            .fallbacks
            .iter()
            .position(|f| f.font.glyph_id(c).0 != 0)
        {
            return Some(4 + i as u16);
        }
        let library = self.library.clone();
        let mut i = 0;
        while i < self.fallback_ids.len() {
            let id = self.fallback_ids[i];
            let covers = library
                .db
                .with_face_data(id, |data, index| {
                    ab_glyph::FontRef::try_from_slice_and_index(data, index)
                        .is_ok_and(|f| f.glyph_id(c).0 != 0)
                })
                .unwrap_or(false);
            if covers {
                self.fallback_ids.remove(i);
                let font = library.load(id)?;
                self.fallbacks.push(Face {
                    font,
                    fake_bold: false,
                });
                return Some(4 + self.fallbacks.len() as u16 - 1);
            }
            i += 1;
        }
        None
    }

    /// Horizontal advance of `text` at `px_em`, in the same pixels.
    pub fn measure(&mut self, text: &str, px_em: f64) -> f64 {
        let mut w = 0.0;
        for c in text.chars() {
            if let Some((fi, g)) = self.lookup(c, Style::REGULAR) {
                let font = &self.face(fi).font;
                let upem = f64::from(font.units_per_em().unwrap_or(1000.0));
                w += f64::from(font.h_advance_unscaled(g)) * px_em / upem;
            }
        }
        w
    }

    /// Rasterises `c` for a pen at `x` (fractional) on an integer baseline.
    /// Returns the bitmap and the integer pen x it is relative to.
    pub fn glyph(
        &mut self,
        c: char,
        style: Style,
        px_em: f64,
        x: f64,
    ) -> Option<(Arc<GlyphBitmap>, i32)> {
        let (face, glyph) = self.lookup(c, style)?;
        let pen = x.floor();
        let subpixel = (((x - pen) as f32) * SUBPIXEL_STEPS).round() as u8;
        let (pen, subpixel) = if f32::from(subpixel) >= SUBPIXEL_STEPS {
            (pen + 1.0, 0)
        } else {
            (pen, subpixel)
        };
        let fake_bold = self.face(face).fake_bold && face < 4 && style.bold;
        let key = GlyphKey {
            face,
            glyph: glyph.0,
            size_milli: (px_em * 1000.0) as u32,
            subpixel,
            fake_bold,
        };
        if let Some(cached) = self.cache.get(&key) {
            return cached.clone().map(|b| (b, pen as i32));
        }
        let bitmap = self
            .rasterise(
                face,
                glyph,
                px_em,
                f32::from(subpixel) / SUBPIXEL_STEPS,
                fake_bold,
            )
            .map(Arc::new);
        self.cache.insert(key, bitmap.clone());
        bitmap.map(|b| (b, pen as i32))
    }

    fn rasterise(
        &self,
        face: u16,
        glyph: GlyphId,
        px_em: f64,
        frac_x: f32,
        fake_bold: bool,
    ) -> Option<GlyphBitmap> {
        let font = &self.face(face).font;
        let scale = Self::scale_for(font, px_em);
        let outlined =
            font.outline_glyph(glyph.with_scale_and_position(scale, point(frac_x, 0.0)))?;
        let bounds = outlined.px_bounds();
        let (w, h) = (bounds.width() as u32, bounds.height() as u32);
        if w == 0 || h == 0 {
            return None;
        }
        let extra = u32::from(fake_bold);
        let stride = w + extra;
        let mut coverage = vec![0u8; (stride * h) as usize];
        outlined.draw(|x, y, c| {
            let v = (c.clamp(0.0, 1.0) * 255.0).round() as u8;
            coverage[(y * stride + x) as usize] = v;
        });
        if fake_bold {
            // Smear one pixel right: the usual synthetic emboldening.
            for row in coverage.chunks_mut(stride as usize) {
                for x in (1..row.len()).rev() {
                    row[x] = row[x].max(row[x - 1]);
                }
            }
        }
        Some(GlyphBitmap {
            left: bounds.min.x as i32,
            top: bounds.min.y as i32,
            width: stride,
            height: h,
            coverage,
        })
    }
}
