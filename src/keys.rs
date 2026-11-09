//! Keyboard: what a key press is, how shortcuts are spelled and matched, and
//! the bytes a key sends to the program in the terminal.
//!
//! The encoding reproduces libvterm's (`vterm_keyboard_key` and
//! `vterm_keyboard_unichar`), which the C++ version used, so programs see
//! exactly the same input as before.

/// A key, independent of the keyboard layout machinery that produced it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// A character key. Letters are always lowercase here; Shift is in the
    /// modifiers.
    Char(char),
    Enter,
    Tab,
    /// Shift+Tab as some layouts report it (`ISO_Left_Tab`).
    Backtab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Insert,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
    /// A modifier on its own (Shift, Ctrl, …). Never sent to the terminal.
    Modifier,
    Other,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[allow(clippy::struct_excessive_bools, reason = "independent modifier keys")]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
}

impl Mods {
    pub const NONE: Mods = Mods {
        shift: false,
        ctrl: false,
        alt: false,
        meta: false,
    };
    pub const SHIFT: Mods = Mods {
        shift: true,
        ..Mods::NONE
    };
    pub const CTRL: Mods = Mods {
        ctrl: true,
        ..Mods::NONE
    };
    pub const ALT: Mods = Mods {
        alt: true,
        ..Mods::NONE
    };

    pub fn ctrl_shift() -> Self {
        Mods {
            ctrl: true,
            shift: true,
            ..Mods::NONE
        }
    }

    /// libvterm's modifier bits: Shift 1, Alt 2, Ctrl 4.
    pub fn vterm_bits(self) -> u8 {
        u8::from(self.shift) | u8::from(self.alt) << 1 | u8::from(self.ctrl) << 2
    }
}

/// One key press as delivered to the terminal widget.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyPress {
    pub key: Key,
    pub mods: Mods,
    /// The text the key produces. With Ctrl held this is the *base*
    /// character, not a control code: libvterm applies Ctrl itself.
    pub text: String,
}

impl KeyPress {
    pub fn new(key: Key, mods: Mods, text: impl Into<String>) -> Self {
        Self {
            key,
            mods,
            text: text.into(),
        }
    }

    /// The key and modifiers in the form shortcuts are matched in.
    pub fn combo(&self) -> KeyCombo {
        let mut mods = self.mods;
        let mut key = self.key;
        // Shift+Tab can arrive as Backtab; fold it back so "Ctrl+Shift+Tab" in
        // the configuration means what it says.
        if key == Key::Backtab {
            key = Key::Tab;
            mods.shift = true;
        }
        if let Key::Char(c) = key {
            key = Key::Char(c.to_ascii_lowercase());
        }
        KeyCombo { key, mods }
    }
}

// ── Shortcuts ───────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyCombo {
    pub key: Key,
    pub mods: Mods,
}

fn named_key(name: &str) -> Option<Key> {
    let lower = name.to_ascii_lowercase();
    Some(match lower.as_str() {
        "left" => Key::Left,
        "right" => Key::Right,
        "up" => Key::Up,
        "down" => Key::Down,
        "tab" => Key::Tab,
        "backtab" => Key::Backtab,
        "pgup" | "pageup" | "page up" => Key::PageUp,
        "pgdown" | "pagedown" | "page down" | "pgdn" => Key::PageDown,
        "home" => Key::Home,
        "end" => Key::End,
        "ins" | "insert" => Key::Insert,
        "del" | "delete" => Key::Delete,
        "esc" | "escape" => Key::Escape,
        "return" | "enter" => Key::Enter,
        "backspace" => Key::Backspace,
        "space" => Key::Char(' '),
        _ => {
            if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                if (1..=35).contains(&n) {
                    return Some(Key::F(n));
                }
            }
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if !c.is_whitespace() => Key::Char(c.to_ascii_lowercase()),
                _ => return None,
            }
        }
    })
}

/// Parses one key combination in Qt's portable spelling — `Ctrl+Shift+Tab`,
/// `Alt+Right`, `Meta+1`, `Ctrl+PgUp`. Case-insensitive.
pub fn parse_combo(text: &str) -> Option<KeyCombo> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    // "Ctrl++" names the plus key; split from the right so it survives.
    let (mods_part, key_part) = match text.strip_suffix("++") {
        Some(rest) => (rest, "+"),
        None => match text.rfind('+') {
            Some(i) => (&text[..i], &text[i + 1..]),
            None => ("", text),
        },
    };
    let mut mods = Mods::NONE;
    for m in mods_part.split('+').filter(|m| !m.trim().is_empty()) {
        match m.trim().to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods.ctrl = true,
            "shift" => mods.shift = true,
            "alt" => mods.alt = true,
            "meta" | "super" | "logo" => mods.meta = true,
            _ => return None,
        }
    }
    let mut key = named_key(key_part.trim())?;
    if key == Key::Backtab {
        key = Key::Tab;
        mods.shift = true;
    }
    Some(KeyCombo { key, mods })
}

/// Parses a comma-separated list of combinations. Unparseable entries are
/// dropped rather than allowed to swallow some unrelated key.
pub fn parse_combos(text: &str) -> Vec<KeyCombo> {
    text.split(',').filter_map(parse_combo).collect()
}

/// The configurable tab actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabAction {
    Previous,
    Next,
    MoveLeft,
    MoveRight,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TabShortcuts {
    pub previous: Vec<KeyCombo>,
    pub next: Vec<KeyCombo>,
    pub move_left: Vec<KeyCombo>,
    pub move_right: Vec<KeyCombo>,
}

impl TabShortcuts {
    pub fn parse(previous: &str, next: &str, move_left: &str, move_right: &str) -> Self {
        Self {
            previous: parse_combos(previous),
            next: parse_combos(next),
            move_left: parse_combos(move_left),
            move_right: parse_combos(move_right),
        }
    }

    pub fn action_for(&self, press: &KeyPress) -> Option<TabAction> {
        let combo = press.combo();
        [
            (&self.previous, TabAction::Previous),
            (&self.next, TabAction::Next),
            (&self.move_left, TabAction::MoveLeft),
            (&self.move_right, TabAction::MoveRight),
        ]
        .into_iter()
        .find(|(list, _)| list.contains(&combo))
        .map(|(_, action)| action)
    }
}

/// The fixed shortcuts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Builtin {
    Copy,
    Paste,
    NewTab,
    CloseTab,
    RenameTab,
}

pub fn builtin_for(press: &KeyPress) -> Option<Builtin> {
    let m = press.mods;
    if m.ctrl && m.shift {
        if let Key::Char(c) = press.combo().key {
            return match c {
                'c' => Some(Builtin::Copy),
                'v' => Some(Builtin::Paste),
                't' | 'n' => Some(Builtin::NewTab),
                'w' => Some(Builtin::CloseTab),
                'e' => Some(Builtin::RenameTab),
                _ => None,
            };
        }
    }
    if m.shift && press.key == Key::Insert {
        return Some(Builtin::Paste);
    }
    None
}

// ── Encoding (libvterm parity) ──────────────────────────────────────

/// Terminal modes that change what keys send.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyModes {
    /// DECCKM: cursor keys send SS3 instead of CSI.
    pub app_cursor: bool,
    /// LNM: Enter sends CR LF.
    pub newline: bool,
}

const CSI: &str = "\x1b[";
const SS3: &str = "\x1bO";

enum Code {
    Literal(u8),
    Enter,
    Tab,
    CsiCursor(char),
    CsiNum(u32),
    Ss3(char),
}

fn code_for(key: Key) -> Option<Code> {
    Some(match key {
        Key::Enter => Code::Enter,
        Key::Tab | Key::Backtab => Code::Tab,
        Key::Backspace => Code::Literal(0x7f),
        Key::Escape => Code::Literal(0x1b),
        Key::Up => Code::CsiCursor('A'),
        Key::Down => Code::CsiCursor('B'),
        Key::Right => Code::CsiCursor('C'),
        Key::Left => Code::CsiCursor('D'),
        Key::Home => Code::CsiCursor('H'),
        Key::End => Code::CsiCursor('F'),
        Key::Insert => Code::CsiNum(2),
        Key::Delete => Code::CsiNum(3),
        Key::PageUp => Code::CsiNum(5),
        Key::PageDown => Code::CsiNum(6),
        Key::F(n) => match n {
            1 => Code::Ss3('P'),
            2 => Code::Ss3('Q'),
            3 => Code::Ss3('R'),
            4 => Code::Ss3('S'),
            5 => Code::CsiNum(15),
            6 => Code::CsiNum(17),
            7 => Code::CsiNum(18),
            8 => Code::CsiNum(19),
            9 => Code::CsiNum(20),
            10 => Code::CsiNum(21),
            11 => Code::CsiNum(23),
            12 => Code::CsiNum(24),
            _ => return None,
        },
        _ => return None,
    })
}

/// `vterm_keyboard_key`.
pub fn encode_key(key: Key, mods: Mods, modes: KeyModes) -> Option<String> {
    let mut mods = mods;
    if key == Key::Backtab {
        mods.shift = true;
    }
    if key == Key::Backspace {
        // Shift+Backspace is still Backspace.
        mods.shift = false;
    }
    let m = u32::from(mods.vterm_bits());
    let literal = |c: u8| -> String {
        if mods.shift || mods.ctrl {
            format!("{CSI}{};{}u", c, m + 1)
        } else if mods.alt {
            format!("\x1b{}", c as char)
        } else {
            (c as char).to_string()
        }
    };
    let csi = |c: char| {
        if m == 0 {
            format!("{CSI}{c}")
        } else {
            format!("{CSI}1;{}{c}", m + 1)
        }
    };
    let ss3 = |c: char| if m == 0 { format!("{SS3}{c}") } else { csi(c) };

    Some(match code_for(key)? {
        Code::Literal(c) => literal(c),
        Code::Tab => {
            // Shift-Tab is CSI Z but plain Tab is 0x09.
            if m == 1 {
                format!("{CSI}Z")
            } else if mods.shift {
                format!("{CSI}1;{}Z", m + 1)
            } else {
                literal(b'\t')
            }
        }
        Code::Enter => {
            if modes.newline {
                "\r\n".to_owned()
            } else {
                literal(b'\r')
            }
        }
        Code::Ss3(c) => ss3(c),
        Code::CsiNum(n) => {
            if m == 0 {
                format!("{CSI}{n}~")
            } else {
                format!("{CSI}{n};{}~", m + 1)
            }
        }
        Code::CsiCursor(c) => {
            if modes.app_cursor {
                ss3(c)
            } else {
                csi(c)
            }
        }
    })
}

/// `vterm_keyboard_unichar`.
pub fn encode_char(c: char, mods: Mods) -> String {
    let mut mods = mods;
    // Shift is never important for Unicode characters apart from Space.
    if c != ' ' {
        mods.shift = false;
    }
    let m = u32::from(mods.vterm_bits());
    if m == 0 {
        return c.to_string();
    }
    let needs_csi_u = match c {
        // Ctrl- letters that cannot be represented otherwise.
        'i' | 'j' | 'm' | '[' => true,
        '\\' | ']' | '^' | '_' => false,
        ' ' => mods.shift,
        _ => !c.is_ascii_lowercase(),
    };
    // Alt alone can be an ESC prefix; anything else requires CSI u.
    if needs_csi_u && (mods.shift || mods.ctrl) {
        return format!("{CSI}{};{}u", c as u32, m + 1);
    }
    let mut out = String::new();
    if mods.alt {
        out.push('\x1b');
    }
    if mods.ctrl {
        out.push(char::from_u32(c as u32 & 0x1f).unwrap_or(c));
    } else {
        out.push(c);
    }
    out
}

/// The bytes a key press sends to the program, as the C++ terminal decided
/// them: special keys by table, Ctrl+letter as its base letter, everything
/// else as its text.
pub fn encode(press: &KeyPress, modes: KeyModes) -> Option<String> {
    if press.key == Key::Modifier {
        return None;
    }
    if let Some(s) = encode_key(press.key, press.mods, modes) {
        return Some(s);
    }
    if press.mods.ctrl {
        if let Key::Char(c) = press.key {
            if c.is_ascii_alphabetic() {
                return Some(encode_char(c.to_ascii_lowercase(), press.mods));
            }
        }
    }
    if press.text.is_empty() {
        return None;
    }
    Some(
        press
            .text
            .chars()
            .map(|c| encode_char(c, press.mods))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(key: Key, mods: Mods, text: &str) -> String {
        encode(&KeyPress::new(key, mods, text), KeyModes::default()).unwrap_or_default()
    }

    #[test]
    fn plain_text_and_control_letters() {
        assert_eq!(enc(Key::Char('a'), Mods::NONE, "a"), "a");
        assert_eq!(enc(Key::Char('a'), Mods::SHIFT, "A"), "A");
        assert_eq!(enc(Key::Char('c'), Mods::CTRL, "c"), "\x03");
        assert_eq!(enc(Key::Char('c'), Mods::ctrl_shift(), "C"), "\x03");
        assert_eq!(enc(Key::Char('x'), Mods::ALT, "x"), "\x1bx");
        assert_eq!(
            enc(
                Key::Char('x'),
                Mods {
                    ctrl: true,
                    alt: true,
                    ..Mods::NONE
                },
                "x"
            ),
            "\x1b\x18"
        );
        assert_eq!(enc(Key::Char('é'), Mods::NONE, "é"), "é");
    }

    #[test]
    fn ctrl_letters_that_need_csi_u() {
        // libvterm keeps Ctrl+I/J/M distinct from Tab/LF/CR.
        assert_eq!(enc(Key::Char('i'), Mods::CTRL, "i"), "\x1b[105;5u");
        assert_eq!(enc(Key::Char('m'), Mods::CTRL, "m"), "\x1b[109;5u");
        assert_eq!(enc(Key::Char('['), Mods::CTRL, "["), "\x1b[91;5u");
        assert_eq!(enc(Key::Char(']'), Mods::CTRL, "]"), "\x1d");
        assert_eq!(enc(Key::Char('1'), Mods::CTRL, "1"), "\x1b[49;5u");
    }

    #[test]
    fn space() {
        assert_eq!(enc(Key::Char(' '), Mods::NONE, " "), " ");
        assert_eq!(enc(Key::Char(' '), Mods::CTRL, " "), "\0");
        assert_eq!(enc(Key::Char(' '), Mods::SHIFT, " "), "\x1b[32;2u");
    }

    #[test]
    fn special_keys() {
        assert_eq!(enc(Key::Enter, Mods::NONE, "\r"), "\r");
        assert_eq!(enc(Key::Enter, Mods::SHIFT, ""), "\x1b[13;2u");
        assert_eq!(enc(Key::Enter, Mods::ALT, ""), "\x1b\r");
        assert_eq!(enc(Key::Tab, Mods::NONE, "\t"), "\t");
        assert_eq!(enc(Key::Tab, Mods::SHIFT, ""), "\x1b[Z");
        assert_eq!(enc(Key::Backtab, Mods::NONE, ""), "\x1b[Z");
        assert_eq!(enc(Key::Tab, Mods::ctrl_shift(), ""), "\x1b[1;6Z");
        assert_eq!(enc(Key::Tab, Mods::CTRL, ""), "\x1b[9;5u");
        assert_eq!(enc(Key::Backspace, Mods::NONE, ""), "\x7f");
        assert_eq!(enc(Key::Backspace, Mods::SHIFT, ""), "\x7f");
        assert_eq!(enc(Key::Backspace, Mods::CTRL, ""), "\x1b[127;5u");
        assert_eq!(enc(Key::Escape, Mods::NONE, ""), "\x1b");
        assert_eq!(enc(Key::Up, Mods::NONE, ""), "\x1b[A");
        assert_eq!(enc(Key::Left, Mods::CTRL, ""), "\x1b[1;5D");
        assert_eq!(enc(Key::Home, Mods::NONE, ""), "\x1b[H");
        assert_eq!(enc(Key::End, Mods::NONE, ""), "\x1b[F");
        assert_eq!(enc(Key::Delete, Mods::NONE, ""), "\x1b[3~");
        assert_eq!(enc(Key::PageUp, Mods::SHIFT, ""), "\x1b[5;2~");
        assert_eq!(enc(Key::F(1), Mods::NONE, ""), "\x1bOP");
        assert_eq!(enc(Key::F(1), Mods::SHIFT, ""), "\x1b[1;2P");
        assert_eq!(enc(Key::F(5), Mods::NONE, ""), "\x1b[15~");
        assert_eq!(enc(Key::F(12), Mods::ALT, ""), "\x1b[24;3~");
        assert_eq!(enc(Key::Modifier, Mods::SHIFT, ""), "");
    }

    #[test]
    fn modes() {
        let app = KeyModes {
            app_cursor: true,
            newline: false,
        };
        assert_eq!(
            encode(&KeyPress::new(Key::Up, Mods::NONE, ""), app).unwrap(),
            "\x1bOA"
        );
        // With modifiers DECCKM falls back to CSI.
        assert_eq!(
            encode(&KeyPress::new(Key::Up, Mods::CTRL, ""), app).unwrap(),
            "\x1b[1;5A"
        );
        let lnm = KeyModes {
            app_cursor: false,
            newline: true,
        };
        assert_eq!(
            encode(&KeyPress::new(Key::Enter, Mods::NONE, ""), lnm).unwrap(),
            "\r\n"
        );
    }

    #[test]
    fn parse_combos_like_qt() {
        assert_eq!(
            parse_combos("Shift+Left, Ctrl+Shift+Tab"),
            vec![
                KeyCombo {
                    key: Key::Left,
                    mods: Mods::SHIFT
                },
                KeyCombo {
                    key: Key::Tab,
                    mods: Mods::ctrl_shift()
                },
            ]
        );
        assert_eq!(
            parse_combo("ctrl+pgup"),
            Some(KeyCombo {
                key: Key::PageUp,
                mods: Mods::CTRL
            })
        );
        assert_eq!(
            parse_combo("Meta+1"),
            Some(KeyCombo {
                key: Key::Char('1'),
                mods: Mods {
                    meta: true,
                    ..Mods::NONE
                }
            })
        );
        assert_eq!(
            parse_combo("Ctrl+Shift+T"),
            Some(KeyCombo {
                key: Key::Char('t'),
                mods: Mods::ctrl_shift()
            })
        );
        assert_eq!(
            parse_combo("Ctrl++"),
            Some(KeyCombo {
                key: Key::Char('+'),
                mods: Mods::CTRL
            })
        );
        assert_eq!(
            parse_combo("F11"),
            Some(KeyCombo {
                key: Key::F(11),
                mods: Mods::NONE
            })
        );
        assert_eq!(
            parse_combo("Backtab"),
            Some(KeyCombo {
                key: Key::Tab,
                mods: Mods::SHIFT
            })
        );
        assert_eq!(parse_combo("Hyper+X"), None);
        assert_eq!(parse_combo("Ctrl+Nonsense"), None);
        assert_eq!(parse_combos(""), vec![]);
        assert_eq!(parse_combos("Bogus+Key, Alt+Right").len(), 1);
    }

    #[test]
    fn tab_shortcut_matching() {
        let d = crate::settings::Values::default();
        let s = TabShortcuts::parse(
            &d.previous_tab_keys,
            &d.next_tab_keys,
            &d.move_tab_left_keys,
            &d.move_tab_right_keys,
        );
        let p = |key, mods| KeyPress::new(key, mods, "");
        assert_eq!(
            s.action_for(&p(Key::Left, Mods::SHIFT)),
            Some(TabAction::Previous)
        );
        assert_eq!(
            s.action_for(&p(Key::Right, Mods::SHIFT)),
            Some(TabAction::Next)
        );
        assert_eq!(
            s.action_for(&p(Key::Tab, Mods::CTRL)),
            Some(TabAction::Next)
        );
        // Shift+Tab may arrive as Backtab.
        assert_eq!(
            s.action_for(&p(Key::Backtab, Mods::ctrl_shift())),
            Some(TabAction::Previous)
        );
        assert_eq!(
            s.action_for(&p(Key::Backtab, Mods::CTRL)),
            Some(TabAction::Previous)
        );
        assert_eq!(
            s.action_for(&p(Key::Left, Mods::ctrl_shift())),
            Some(TabAction::MoveLeft)
        );
        assert_eq!(
            s.action_for(&p(Key::Right, Mods::ctrl_shift())),
            Some(TabAction::MoveRight)
        );
        assert_eq!(s.action_for(&p(Key::Left, Mods::NONE)), None);
        assert_eq!(s.action_for(&p(Key::Left, Mods::CTRL)), None);
    }

    #[test]
    fn builtin_shortcuts() {
        let p = |c: char, mods| KeyPress::new(Key::Char(c), mods, "");
        assert_eq!(
            builtin_for(&p('c', Mods::ctrl_shift())),
            Some(Builtin::Copy)
        );
        assert_eq!(
            builtin_for(&p('C', Mods::ctrl_shift())),
            Some(Builtin::Copy)
        );
        assert_eq!(
            builtin_for(&p('v', Mods::ctrl_shift())),
            Some(Builtin::Paste)
        );
        assert_eq!(
            builtin_for(&p('t', Mods::ctrl_shift())),
            Some(Builtin::NewTab)
        );
        assert_eq!(
            builtin_for(&p('n', Mods::ctrl_shift())),
            Some(Builtin::NewTab)
        );
        assert_eq!(
            builtin_for(&p('w', Mods::ctrl_shift())),
            Some(Builtin::CloseTab)
        );
        assert_eq!(
            builtin_for(&p('e', Mods::ctrl_shift())),
            Some(Builtin::RenameTab)
        );
        assert_eq!(builtin_for(&p('c', Mods::CTRL)), None);
        assert_eq!(
            builtin_for(&KeyPress::new(Key::Insert, Mods::SHIFT, "")),
            Some(Builtin::Paste)
        );
    }
}
