//! Turning a cell's colour into RGB.
//!
//! The built-in palette is libvterm's, which the C++ version used, so a
//! terminal with no colours configured looks exactly as it did.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb as VtRgb};

use crate::color::Rgb;

/// libvterm's 16 ANSI colours: 0–7 normal, 8–15 bright.
pub const LIBVTERM_ANSI: [Rgb; 16] = [
    Rgb::new(0, 0, 0),
    Rgb::new(224, 0, 0),
    Rgb::new(0, 224, 0),
    Rgb::new(224, 224, 0),
    Rgb::new(0, 0, 224),
    Rgb::new(224, 0, 224),
    Rgb::new(0, 224, 224),
    Rgb::new(224, 224, 224),
    Rgb::new(128, 128, 128),
    Rgb::new(255, 64, 64),
    Rgb::new(64, 255, 64),
    Rgb::new(255, 255, 64),
    Rgb::new(64, 64, 255),
    Rgb::new(255, 64, 255),
    Rgb::new(64, 255, 255),
    Rgb::new(255, 255, 255),
];

const RAMP6: [u8; 6] = [0x00, 0x33, 0x66, 0x99, 0xcc, 0xff];
const RAMP24: [u8; 24] = [
    0x00, 0x0b, 0x16, 0x21, 0x2c, 0x37, 0x42, 0x4d, 0x58, 0x63, 0x6e, 0x79, 0x85, 0x90, 0x9b, 0xa6,
    0xb1, 0xbc, 0xc7, 0xd2, 0xdd, 0xe8, 0xf3, 0xff,
];

/// The colours a terminal draws with, before any per-program (OSC) changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Palette {
    pub foreground: Rgb,
    pub background: Rgb,
    pub ansi: [Rgb; 16],
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            foreground: Rgb::new(235, 235, 235),
            background: Rgb::new(0, 0, 0),
            ansi: LIBVTERM_ANSI,
        }
    }
}

impl Palette {
    /// Settings' colours over libvterm's: an unset ANSI entry keeps the
    /// built-in colour for that index.
    pub fn new(foreground: Rgb, background: Rgb, overrides: &[Option<Rgb>; 16]) -> Self {
        Self {
            foreground,
            background,
            ansi: std::array::from_fn(|i| overrides[i].unwrap_or(LIBVTERM_ANSI[i])),
        }
    }

    /// One of the 256 indexed colours.
    pub fn indexed(&self, index: u8, osc: Option<&Colors>) -> Rgb {
        if let Some(c) = osc.and_then(|c| c[index as usize]) {
            return from_vt(c);
        }
        match index {
            0..=15 => self.ansi[index as usize],
            16..=231 => {
                let i = index - 16;
                Rgb::new(
                    RAMP6[(i / 36) as usize],
                    RAMP6[(i / 6 % 6) as usize],
                    RAMP6[(i % 6) as usize],
                )
            }
            _ => {
                let g = RAMP24[(index - 232) as usize];
                Rgb::new(g, g, g)
            }
        }
    }

    pub fn resolve(&self, color: Color, osc: Option<&Colors>) -> Rgb {
        match color {
            Color::Spec(c) => from_vt(c),
            Color::Indexed(i) => self.indexed(i, osc),
            Color::Named(named) => self.named(named, osc),
        }
    }

    fn named(&self, named: NamedColor, osc: Option<&Colors>) -> Rgb {
        let n = named as usize;
        if n < 16 {
            return self.indexed(n as u8, osc);
        }
        if let Some(c) = osc.and_then(|c| c[n]) {
            return from_vt(c);
        }
        match named {
            NamedColor::Background => self.background,
            NamedColor::Foreground
            | NamedColor::BrightForeground
            | NamedColor::DimForeground
            | NamedColor::Cursor => self.foreground,
            // Dim variants have no colour of their own here, as in libvterm.
            _ => self.indexed(
                (n.saturating_sub(NamedColor::DimBlack as usize) % 8) as u8,
                osc,
            ),
        }
    }
}

pub fn from_vt(c: VtRgb) -> Rgb {
    Rgb::new(c.r, c.g, c.b)
}

pub fn to_vt(c: Rgb) -> VtRgb {
    VtRgb {
        r: c.r,
        g: c.g,
        b: c.b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn libvterm_ramps() {
        let p = Palette::default();
        assert_eq!(p.indexed(1, None), Rgb::new(224, 0, 0));
        assert_eq!(p.indexed(16, None), Rgb::new(0, 0, 0));
        assert_eq!(p.indexed(196, None), Rgb::new(0xff, 0, 0));
        assert_eq!(p.indexed(231, None), Rgb::new(0xff, 0xff, 0xff));
        assert_eq!(p.indexed(232, None), Rgb::new(0, 0, 0));
        assert_eq!(p.indexed(255, None), Rgb::new(0xff, 0xff, 0xff));
    }

    #[test]
    fn overrides_and_named() {
        let mut o = [None; 16];
        o[1] = Some(Rgb::new(1, 2, 3));
        let p = Palette::new(Rgb::new(9, 9, 9), Rgb::new(8, 8, 8), &o);
        assert_eq!(
            p.resolve(Color::Named(NamedColor::Red), None),
            Rgb::new(1, 2, 3)
        );
        assert_eq!(
            p.resolve(Color::Named(NamedColor::Green), None),
            LIBVTERM_ANSI[2]
        );
        assert_eq!(
            p.resolve(Color::Named(NamedColor::Foreground), None),
            Rgb::new(9, 9, 9)
        );
        assert_eq!(
            p.resolve(Color::Named(NamedColor::Background), None),
            Rgb::new(8, 8, 8)
        );
        assert_eq!(
            p.resolve(Color::Named(NamedColor::DimRed), None),
            Rgb::new(1, 2, 3)
        );
        assert_eq!(
            p.resolve(Color::Spec(VtRgb { r: 5, g: 6, b: 7 }), None),
            Rgb::new(5, 6, 7)
        );
    }

    #[test]
    fn osc_overrides_win() {
        let p = Palette::default();
        let mut c = Colors::default();
        c[4] = Some(VtRgb { r: 1, g: 1, b: 1 });
        assert_eq!(p.resolve(Color::Indexed(4), Some(&c)), Rgb::new(1, 1, 1));
    }
}
