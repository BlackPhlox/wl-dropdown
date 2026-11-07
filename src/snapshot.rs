//! Screenshot testing: render, encode to PNG, and compare against a stored
//! reference with a visual diff on mismatch.
//!
//! References live in `tests/snapshots/<name>.png`. Run the tests with
//! `UPDATE_SNAPSHOTS=1` to (re)write them after an intended visual change,
//! then review the new PNGs like any other diff. On a mismatch the actual
//! image and a diff image are written to `target/snapshots/` so the change
//! can be inspected.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tiny_skia::Pixmap;

/// How different two images may be and still match. Rasterisation can vary
/// by a step or two of anti-aliasing between platforms and library versions,
/// which is noise rather than a visual change.
#[derive(Clone, Copy, Debug)]
pub struct Tolerance {
    /// Largest per-channel difference that does not count at all.
    pub channel: u8,
    /// How many pixels may differ by more than that.
    pub pixels: usize,
}

impl Default for Tolerance {
    fn default() -> Self {
        Self {
            channel: 8,
            pixels: 0,
        }
    }
}

/// Straight (non-premultiplied) RGBA pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Image {
    pub fn from_pixmap(p: &Pixmap) -> Self {
        let mut rgba = Vec::with_capacity(p.data().len());
        for px in p.pixels() {
            let c = px.demultiply();
            rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
        }
        Self {
            width: p.width(),
            height: p.height(),
            rgba,
        }
    }

    /// From premultiplied ARGB8888 as Wayland screenshots (and our buffers)
    /// hold it.
    pub fn from_premultiplied_rgba(width: u32, height: u32, data: &[u8]) -> Self {
        let mut rgba = Vec::with_capacity(data.len());
        for px in data.as_chunks::<4>().0 {
            let a = px[3] as u32;
            let un = |c: u8| {
                (c as u32 * 255 + a / 2)
                    .checked_div(a)
                    .map_or(0, |v| v.min(255) as u8)
            };
            rgba.extend_from_slice(&[un(px[0]), un(px[1]), un(px[2]), px[3]]);
        }
        Self {
            width,
            height,
            rgba,
        }
    }

    pub fn load_png(path: &Path) -> io::Result<Self> {
        let decoder = png::Decoder::new(io::BufReader::new(fs::File::open(path)?));
        let mut reader = decoder.read_info().map_err(io::Error::other)?;
        let mut buf = vec![0; reader.output_buffer_size().unwrap_or(0)];
        let info = reader.next_frame(&mut buf).map_err(io::Error::other)?;
        buf.truncate(info.buffer_size());
        let rgba = match info.color_type {
            png::ColorType::Rgba => buf,
            png::ColorType::Rgb => buf
                .as_chunks::<3>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2], 255])
                .collect(),
            other => {
                return Err(io::Error::other(format!(
                    "unsupported PNG colour type {other:?}"
                )))
            }
        };
        Ok(Self {
            width: info.width,
            height: info.height,
            rgba,
        })
    }

    pub fn save_png(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let file = io::BufWriter::new(fs::File::create(path)?);
        let mut encoder = png::Encoder::new(file, self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::High);
        let mut writer = encoder.write_header().map_err(io::Error::other)?;
        writer
            .write_image_data(&self.rgba)
            .map_err(io::Error::other)?;
        writer.finish().map_err(io::Error::other)
    }

    /// Composites over a solid colour, so a transparent surface can be looked
    /// at as it would appear on a desktop.
    pub fn over(&self, bg: [u8; 3]) -> Self {
        let mut rgba = self.rgba.clone();
        for px in rgba.as_chunks_mut::<4>().0 {
            let a = px[3] as u32;
            for i in 0..3 {
                px[i] = ((px[i] as u32 * a + bg[i] as u32 * (255 - a) + 127) / 255) as u8;
            }
            px[3] = 255;
        }
        Self {
            width: self.width,
            height: self.height,
            rgba,
        }
    }
}

#[derive(Debug)]
pub struct Comparison {
    pub differing_pixels: usize,
    pub max_channel_delta: u8,
    /// The reference dimmed, with differing pixels in red.
    pub diff: Image,
}

/// Compares two images of the same size. `None` when the sizes differ.
pub fn compare(actual: &Image, expected: &Image, tol: Tolerance) -> Option<Comparison> {
    if (actual.width, actual.height) != (expected.width, expected.height) {
        return None;
    }
    let mut differing = 0;
    let mut max_delta = 0u8;
    let mut diff = Vec::with_capacity(expected.rgba.len());
    for (a, e) in actual
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(expected.rgba.as_chunks::<4>().0)
    {
        let delta = (0..4).map(|i| a[i].abs_diff(e[i])).max().unwrap();
        max_delta = max_delta.max(delta);
        if delta > tol.channel {
            differing += 1;
            diff.extend_from_slice(&[255, 0, 0, 255]);
        } else {
            let grey =
                ((e[0] as u32 + e[1] as u32 + e[2] as u32) / 3 * e[3] as u32 / 255 / 3) as u8;
            diff.extend_from_slice(&[grey, grey, grey, 255]);
        }
    }
    Some(Comparison {
        differing_pixels: differing,
        max_channel_delta: max_delta,
        diff: Image {
            width: expected.width,
            height: expected.height,
            rgba: diff,
        },
    })
}

/// The directory references are kept in.
pub fn snapshot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")
}

/// Where mismatches are written for inspection.
pub fn output_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"))
        .join("snapshots")
}

fn update_requested() -> bool {
    std::env::var_os("UPDATE_SNAPSHOTS").is_some_and(|v| !v.is_empty() && v != "0")
}

/// Checks `image` against `tests/snapshots/<name>.png`.
///
/// Returns a description of the mismatch, or `Ok` if it matches (or was
/// written because `UPDATE_SNAPSHOTS` is set).
pub fn check(name: &str, image: &Image, tol: Tolerance) -> Result<(), String> {
    let reference = snapshot_dir().join(format!("{name}.png"));
    let out = output_dir();
    if update_requested() {
        image
            .save_png(&reference)
            .map_err(|e| format!("cannot write {}: {e}", reference.display()))?;
        return Ok(());
    }
    let actual_path = out.join(format!("{name}.actual.png"));
    let expected = match Image::load_png(&reference) {
        Ok(img) => img,
        Err(_) => {
            let _ = image.save_png(&actual_path);
            return Err(format!(
                "no reference for snapshot '{name}'; wrote {} — rerun with UPDATE_SNAPSHOTS=1 to accept it",
                actual_path.display()
            ));
        }
    };
    match compare(image, &expected, tol) {
        None => {
            let _ = image.save_png(&actual_path);
            Err(format!(
                "snapshot '{name}' changed size: {}x{} now, {}x{} in the reference; actual written to {}",
                image.width,
                image.height,
                expected.width,
                expected.height,
                actual_path.display()
            ))
        }
        Some(c) if c.differing_pixels > tol.pixels => {
            let diff_path = out.join(format!("{name}.diff.png"));
            let _ = image.save_png(&actual_path);
            let _ = c.diff.save_png(&diff_path);
            Err(format!(
                "snapshot '{name}' differs in {} pixels (max channel delta {}); see {} and {}",
                c.differing_pixels,
                c.max_channel_delta,
                actual_path.display(),
                diff_path.display()
            ))
        }
        Some(_) => {
            let _ = fs::remove_file(&actual_path);
            let _ = fs::remove_file(out.join(format!("{name}.diff.png")));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, c: [u8; 4]) -> Image {
        Image {
            width: w,
            height: h,
            rgba: c.repeat((w * h) as usize),
        }
    }

    #[test]
    fn identical_images_match() {
        let a = solid(4, 3, [10, 20, 30, 255]);
        let c = compare(&a, &a, Tolerance::default()).unwrap();
        assert_eq!(c.differing_pixels, 0);
        assert_eq!(c.max_channel_delta, 0);
    }

    #[test]
    fn small_deltas_are_tolerated_and_large_ones_counted() {
        let a = solid(2, 2, [100, 100, 100, 255]);
        let mut b = a.clone();
        b.rgba[0] = 105; // within tolerance
        b.rgba[4] = 200; // not
        let c = compare(&b, &a, Tolerance::default()).unwrap();
        assert_eq!(c.differing_pixels, 1);
        assert_eq!(c.max_channel_delta, 100);
        assert_eq!(&c.diff.rgba[4..8], &[255, 0, 0, 255]);
    }

    #[test]
    fn size_mismatch_is_none() {
        assert!(compare(
            &solid(1, 1, [0; 4]),
            &solid(2, 1, [0; 4]),
            Tolerance::default()
        )
        .is_none());
    }

    #[test]
    fn png_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.png");
        let img = Image {
            width: 2,
            height: 1,
            rgba: vec![1, 2, 3, 4, 250, 251, 252, 255],
        };
        img.save_png(&p).unwrap();
        assert_eq!(Image::load_png(&p).unwrap(), img);
    }

    #[test]
    fn unpremultiplies() {
        let img = Image::from_premultiplied_rgba(1, 1, &[64, 0, 128, 128]);
        assert_eq!(img.rgba, vec![128, 0, 255, 128]);
    }
}
