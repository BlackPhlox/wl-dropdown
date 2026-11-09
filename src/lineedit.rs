//! The one-line editor used to rename a tab in place.

use crate::keys::{Key, KeyPress};

pub const MAX_LEN: usize = 64;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineEdit {
    chars: Vec<char>,
    cursor: usize,
    /// The other end of the selection; equal to `cursor` when there is none.
    anchor: usize,
}

/// What a key did to the editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditResult {
    Continue,
    Accept,
    Cancel,
}

impl LineEdit {
    /// Opens holding `text`, all selected, so typing replaces it.
    pub fn new(text: &str) -> Self {
        let chars: Vec<char> = text.chars().take(MAX_LEN).collect();
        let end = chars.len();
        Self {
            chars,
            cursor: end,
            anchor: 0,
        }
    }

    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The selected range of characters, if any.
    pub fn selection(&self) -> Option<(usize, usize)> {
        (self.cursor != self.anchor)
            .then(|| (self.cursor.min(self.anchor), self.cursor.max(self.anchor)))
    }

    fn delete_selection(&mut self) -> bool {
        let Some((a, b)) = self.selection() else {
            return false;
        };
        self.chars.drain(a..b);
        self.cursor = a;
        self.anchor = a;
        true
    }

    pub fn insert(&mut self, text: &str) {
        self.delete_selection();
        for c in text.chars().filter(|c| !c.is_control()) {
            if self.chars.len() >= MAX_LEN {
                break;
            }
            self.chars.insert(self.cursor, c);
            self.cursor += 1;
        }
        self.anchor = self.cursor;
    }

    fn move_to(&mut self, pos: usize, extend: bool) {
        self.cursor = pos.min(self.chars.len());
        if !extend {
            self.anchor = self.cursor;
        }
    }

    pub fn key(&mut self, press: &KeyPress) -> EditResult {
        let shift = press.mods.shift;
        match press.key {
            Key::Enter => return EditResult::Accept,
            Key::Escape => return EditResult::Cancel,
            Key::Backspace => {
                if !self.delete_selection() && self.cursor > 0 {
                    self.cursor -= 1;
                    self.chars.remove(self.cursor);
                    self.anchor = self.cursor;
                }
            }
            Key::Delete => {
                if !self.delete_selection() && self.cursor < self.chars.len() {
                    self.chars.remove(self.cursor);
                }
            }
            Key::Left => {
                let to = match (self.selection(), shift) {
                    (Some((a, _)), false) => a,
                    _ => self.cursor.saturating_sub(1),
                };
                self.move_to(to, shift);
            }
            Key::Right => {
                let to = match (self.selection(), shift) {
                    (Some((_, b)), false) => b,
                    _ => self.cursor + 1,
                };
                self.move_to(to, shift);
            }
            Key::Home => self.move_to(0, shift),
            Key::End => self.move_to(self.chars.len(), shift),
            Key::Char('a') if press.mods.ctrl => {
                self.anchor = 0;
                self.cursor = self.chars.len();
            }
            _ if !press.mods.ctrl && !press.mods.alt && !press.text.is_empty() => {
                let text = press.text.clone();
                self.insert(&text);
            }
            _ => {}
        }
        EditResult::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::Mods;

    fn press(key: Key, mods: Mods, text: &str) -> KeyPress {
        KeyPress::new(key, mods, text)
    }

    fn typed(e: &mut LineEdit, s: &str) {
        for c in s.chars() {
            e.key(&press(Key::Char(c), Mods::NONE, &c.to_string()));
        }
    }

    #[test]
    fn opens_all_selected_and_typing_replaces() {
        let mut e = LineEdit::new("Shell");
        assert_eq!(e.selection(), Some((0, 5)));
        typed(&mut e, "build");
        assert_eq!(e.text(), "build");
    }

    #[test]
    fn editing_keys() {
        let mut e = LineEdit::new("abc");
        e.key(&press(Key::End, Mods::NONE, ""));
        e.key(&press(Key::Backspace, Mods::NONE, ""));
        assert_eq!(e.text(), "ab");
        e.key(&press(Key::Home, Mods::NONE, ""));
        e.key(&press(Key::Delete, Mods::NONE, ""));
        assert_eq!(e.text(), "b");
        typed(&mut e, "x");
        assert_eq!(e.text(), "xb");
        e.key(&press(Key::Right, Mods::SHIFT, ""));
        assert_eq!(e.selection(), Some((1, 2)));
        e.key(&press(Key::Backspace, Mods::NONE, ""));
        assert_eq!(e.text(), "x");
        e.key(&press(Key::Char('a'), Mods::CTRL, "a"));
        assert_eq!(e.selection(), Some((0, 1)));
    }

    #[test]
    fn accept_cancel_and_limits() {
        let mut e = LineEdit::new("");
        assert_eq!(
            e.key(&press(Key::Enter, Mods::NONE, "\r")),
            EditResult::Accept
        );
        assert_eq!(
            e.key(&press(Key::Escape, Mods::NONE, "")),
            EditResult::Cancel
        );
        e.insert(&"y".repeat(100));
        assert_eq!(e.text().chars().count(), MAX_LEN);
        let mut e = LineEdit::new("");
        e.insert("a\tb\u{7}");
        assert_eq!(e.text(), "ab");
    }
}
