//! Mouse reports for programs that enable tracking (htop, vim, mc, …).
//!
//! Reproduces libvterm's `vterm_mouse_move` / `vterm_mouse_button`, including
//! its state: which buttons are held and the last reported cell, so motion is
//! only reported when the program's mode asks for it and only when the cell
//! actually changes.

use crate::keys::Mods;

/// Which reports the program asked for, and in what encoding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MouseMode {
    /// Any tracking at all (1000, 1002 or 1003).
    pub enabled: bool,
    /// 1002: motion while a button is held.
    pub drag: bool,
    /// 1003: all motion.
    pub motion: bool,
    pub sgr: bool,
    pub utf8: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left = 1,
    Middle = 2,
    Right = 3,
    WheelUp = 4,
    WheelDown = 5,
}

#[derive(Clone, Debug, Default)]
pub struct MouseReporter {
    buttons: u8,
    col: i32,
    row: i32,
}

impl MouseReporter {
    pub fn new() -> Self {
        // An impossible cell, so the first move always registers.
        Self {
            buttons: 0,
            col: -1,
            row: -1,
        }
    }

    /// Whether any button the reporter knows about is held.
    pub fn buttons_held(&self) -> bool {
        self.buttons != 0
    }

    /// The pointer is over `col`, `row` (0-based).
    pub fn motion(&mut self, col: i32, row: i32, mods: Mods, mode: MouseMode) -> Option<String> {
        if col == self.col && row == self.row {
            return None;
        }
        self.col = col;
        self.row = row;
        if (mode.drag && self.buttons != 0) || mode.motion {
            let button = if self.buttons & 1 != 0 {
                1
            } else if self.buttons & 2 != 0 {
                2
            } else if self.buttons & 4 != 0 {
                3
            } else {
                4
            };
            return Some(output(button - 1 + 0x20, true, mods, col, row, mode));
        }
        None
    }

    /// A button changed state at the last reported cell.
    pub fn button(
        &mut self,
        button: Button,
        pressed: bool,
        mods: Mods,
        mode: MouseMode,
    ) -> Option<String> {
        let b = button as u8;
        let old = self.buttons;
        if b <= 3 {
            let bit = 1 << (b - 1);
            if pressed {
                self.buttons |= bit;
            } else {
                self.buttons &= !bit;
            }
        }
        // Wheel buttons have no release.
        if self.buttons == old && b < 4 {
            return None;
        }
        if !mode.enabled {
            return None;
        }
        let code = if b < 4 {
            b as u32 - 1
        } else {
            b as u32 - 4 + 0x40
        };
        Some(output(
            code,
            pressed,
            mods,
            self.col.max(0),
            self.row.max(0),
            mode,
        ))
    }
}

fn utf8_coord(out: &mut String, v: u32) {
    if let Some(c) = char::from_u32(v) {
        out.push(c);
    }
}

fn output(code: u32, pressed: bool, mods: Mods, col: i32, row: i32, mode: MouseMode) -> String {
    let modifiers = (mods.vterm_bits() as u32) << 2;
    let (col, row) = (col.max(0) as u32, row.max(0) as u32);
    if mode.sgr {
        return format!(
            "\x1b[<{};{};{}{}",
            code | modifiers,
            col + 1,
            row + 1,
            if pressed { 'M' } else { 'm' }
        );
    }
    let code = if pressed { code } else { 3 };
    if mode.utf8 {
        let mut s = String::from("\x1b[M");
        utf8_coord(&mut s, (code | modifiers) + 0x20);
        utf8_coord(&mut s, col + 0x21);
        utf8_coord(&mut s, row + 0x21);
        return s;
    }
    // X10: one byte per value, clamped. Bytes above 0x7f are not valid UTF-8
    // on their own, so they are carried as Latin-1 code points and written out
    // as single bytes by `to_bytes`.
    let col = col.min(0xff - 0x21);
    let row = row.min(0xff - 0x21);
    let mut s = String::from("\x1b[M");
    for v in [(code | modifiers) + 0x20, col + 0x21, row + 0x21] {
        s.push(char::from_u32(v).unwrap());
    }
    s
}

/// Report strings are text except for X10 coordinates above 0x7f, which must
/// go out as raw bytes. Converts a report to what is written to the pty.
pub fn to_bytes(report: &str, mode: MouseMode) -> Vec<u8> {
    if mode.sgr || mode.utf8 {
        report.as_bytes().to_vec()
    } else {
        report.chars().map(|c| c as u32 as u8).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLICK: MouseMode = MouseMode {
        enabled: true,
        drag: false,
        motion: false,
        sgr: false,
        utf8: false,
    };
    const SGR: MouseMode = MouseMode { sgr: true, ..CLICK };

    #[test]
    fn x10_click() {
        let mut m = MouseReporter::new();
        assert_eq!(
            m.motion(4, 2, Mods::NONE, CLICK),
            None,
            "no motion reports in click mode"
        );
        assert_eq!(
            m.button(Button::Left, true, Mods::NONE, CLICK).unwrap(),
            "\x1b[M %#"
        );
        assert_eq!(
            m.button(Button::Left, false, Mods::NONE, CLICK).unwrap(),
            "\x1b[M#%#"
        );
    }

    #[test]
    fn sgr_click_with_modifiers() {
        let mut m = MouseReporter::new();
        m.motion(9, 0, Mods::NONE, SGR);
        let ctrl = Mods::CTRL;
        assert_eq!(
            m.button(Button::Right, true, ctrl, SGR).unwrap(),
            "\x1b[<18;10;1M"
        );
        assert_eq!(
            m.button(Button::Right, false, ctrl, SGR).unwrap(),
            "\x1b[<18;10;1m"
        );
    }

    #[test]
    fn repeated_press_is_not_reported_twice() {
        let mut m = MouseReporter::new();
        m.motion(0, 0, Mods::NONE, SGR);
        assert!(m.button(Button::Left, true, Mods::NONE, SGR).is_some());
        assert!(m.button(Button::Left, true, Mods::NONE, SGR).is_none());
    }

    #[test]
    fn drag_reports_only_while_held_and_on_cell_change() {
        let mode = MouseMode { drag: true, ..SGR };
        let mut m = MouseReporter::new();
        assert_eq!(m.motion(1, 1, Mods::NONE, mode), None);
        m.button(Button::Left, true, Mods::NONE, mode);
        assert_eq!(m.motion(2, 1, Mods::NONE, mode).unwrap(), "\x1b[<32;3;2M");
        assert_eq!(m.motion(2, 1, Mods::NONE, mode), None, "same cell");
        m.button(Button::Left, false, Mods::NONE, mode);
        assert_eq!(m.motion(3, 1, Mods::NONE, mode), None);
    }

    #[test]
    fn any_motion_mode_reports_without_buttons() {
        let mode = MouseMode {
            motion: true,
            ..SGR
        };
        let mut m = MouseReporter::new();
        assert_eq!(m.motion(0, 0, Mods::NONE, mode).unwrap(), "\x1b[<35;1;1M");
    }

    #[test]
    fn wheel_has_no_release_and_uses_64_65() {
        let mut m = MouseReporter::new();
        m.motion(0, 0, Mods::NONE, SGR);
        assert_eq!(
            m.button(Button::WheelUp, true, Mods::NONE, SGR).unwrap(),
            "\x1b[<64;1;1M"
        );
        assert_eq!(
            m.button(Button::WheelDown, true, Mods::NONE, SGR).unwrap(),
            "\x1b[<65;1;1M"
        );
    }

    #[test]
    fn disabled_mode_reports_nothing_but_tracks_buttons() {
        let mut m = MouseReporter::new();
        assert_eq!(
            m.button(Button::Left, true, Mods::NONE, MouseMode::default()),
            None
        );
        assert!(m.buttons_held());
    }

    #[test]
    fn x10_clamps_and_emits_raw_bytes() {
        let mut m = MouseReporter::new();
        m.motion(500, 300, Mods::NONE, CLICK);
        let r = m.button(Button::Left, true, Mods::NONE, CLICK).unwrap();
        assert_eq!(
            to_bytes(&r, CLICK),
            vec![0x1b, b'[', b'M', 0x20, 0xff, 0xff]
        );
    }

    #[test]
    fn utf8_mode_encodes_large_coordinates() {
        let mode = MouseMode {
            utf8: true,
            ..CLICK
        };
        let mut m = MouseReporter::new();
        m.motion(300, 0, Mods::NONE, mode);
        let r = m.button(Button::Left, true, Mods::NONE, mode).unwrap();
        assert_eq!(
            r,
            format!("\x1b[M {}!", char::from_u32(300 + 0x21).unwrap())
        );
    }
}
