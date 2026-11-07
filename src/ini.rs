//! The INI dialect the configuration files are written in.
//!
//! Compatible with what Qt's `QSettings` (`IniFormat`) wrote and read in the C++
//! version, and with what Nix's `lib.generators.toINI` produces:
//!
//! - keys of interest live under `[General]`;
//! - whole-line comments start with `;` or `#`;
//! - a value in double quotes is a single string, with `\"`, `\\`, `\n`, `\t`
//!   escapes;
//! - an unquoted value containing top-level commas is a *list*, which is how a
//!   hand-written `nextTabKeys=Shift+Right, Ctrl+Tab` reads.
//!
//! Files are rewritten atomically (temporary file, then rename), so a reader
//! sees either the old file or the new one, never a mixture.

use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const GENERAL: &str = "General";

/// A value as read from a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Str(String),
    List(Vec<String>),
}

impl Value {
    /// The value as one string; a list is joined with ", ".
    pub fn to_text(&self) -> String {
        match self {
            Value::Str(s) => s.clone(),
            Value::List(items) => items.join(", "),
        }
    }

    /// A number, if the value is a single parseable one. An empty value is
    /// *not* zero: a half-rendered template leaves `backgroundOpacity=`, and
    /// reading that as 0.0 would make the terminal vanish.
    pub fn to_f64(&self) -> Option<f64> {
        match self {
            Value::Str(s) => s.trim().parse::<f64>().ok().filter(|v| v.is_finite()),
            Value::List(_) => None,
        }
    }

    pub fn to_i32(&self) -> Option<i32> {
        match self {
            Value::Str(s) => s.trim().parse::<i32>().ok(),
            Value::List(_) => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Section {
    name: String,
    entries: Vec<(String, String)>, // raw, unparsed values
}

/// One INI file, held in memory.
#[derive(Clone, Debug, Default)]
pub struct IniFile {
    path: Option<PathBuf>,
    sections: Vec<Section>,
}

impl IniFile {
    /// Loads `path`; a missing or unreadable file is simply empty.
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let text = fs::read_to_string(&path).unwrap_or_default();
        let mut file = Self::parse(&text);
        file.path = Some(path);
        file
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Re-reads from disk, discarding anything in memory.
    pub fn reload(&mut self) {
        if let Some(path) = self.path.clone() {
            *self = Self::load(path);
        }
    }

    pub fn parse(text: &str) -> Self {
        let mut sections: Vec<Section> = Vec::new();
        let mut current = GENERAL.to_owned();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix('[') {
                if let Some(end) = rest.find(']') {
                    rest[..end].trim().clone_into(&mut current);
                }
                continue;
            }
            let Some(eq) = line.find('=') else { continue };
            let key = line[..eq].trim().to_owned();
            let value = line[eq + 1..].trim().to_owned();
            if key.is_empty() {
                continue;
            }
            let index = sections
                .iter()
                .position(|s| s.name == current)
                .unwrap_or_else(|| {
                    sections.push(Section {
                        name: current.clone(),
                        entries: Vec::new(),
                    });
                    sections.len() - 1
                });
            let section = &mut sections[index];
            match section.entries.iter_mut().find(|(k, _)| *k == key) {
                Some(entry) => entry.1 = value, // later duplicates win
                None => section.entries.push((key, value)),
            }
        }
        Self {
            path: None,
            sections,
        }
    }

    fn general(&self) -> Option<&Section> {
        self.sections.iter().find(|s| s.name == GENERAL)
    }

    fn general_mut(&mut self) -> &mut Section {
        if let Some(i) = self.sections.iter().position(|s| s.name == GENERAL) {
            return &mut self.sections[i];
        }
        self.sections.insert(
            0,
            Section {
                name: GENERAL.to_owned(),
                entries: Vec::new(),
            },
        );
        &mut self.sections[0]
    }

    pub fn contains(&self, key: &str) -> bool {
        self.general()
            .is_some_and(|s| s.entries.iter().any(|(k, _)| k == key))
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        let raw = self
            .general()?
            .entries
            .iter()
            .find(|(k, _)| k == key)?
            .1
            .as_str();
        Some(parse_value(raw))
    }

    pub fn keys(&self) -> Vec<String> {
        self.general()
            .map(|s| s.entries.iter().map(|(k, _)| k.clone()).collect())
            .unwrap_or_default()
    }

    pub fn set(&mut self, key: &str, value: &str) {
        let encoded = encode_value(value);
        let section = self.general_mut();
        match section.entries.iter_mut().find(|(k, _)| k == key) {
            Some(entry) => entry.1 = encoded,
            None => section.entries.push((key.to_owned(), encoded)),
        }
    }

    pub fn remove(&mut self, key: &str) {
        if let Some(i) = self.sections.iter().position(|s| s.name == GENERAL) {
            self.sections[i].entries.retain(|(k, _)| k != key);
        }
    }

    /// Drops every key in every section.
    pub fn clear(&mut self) {
        self.sections.clear();
    }

    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for section in self.sections.iter().filter(|s| !s.entries.is_empty()) {
            if !out.is_empty() {
                out.push('\n');
            }
            let _ = writeln!(out, "[{}]", section.name);
            for (k, v) in &section.entries {
                let _ = writeln!(out, "{k}={v}");
            }
        }
        out
    }

    /// Writes the file atomically, creating its directory if needed.
    ///
    /// # Errors
    ///
    /// When the file has no path, or the directory, temporary file or rename
    /// cannot be written.
    pub fn save(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "INI file has no path",
            ));
        };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(self.to_text().as_bytes())?;
            f.sync_all()?;
        }
        fs::rename(&tmp, path)
    }
}

fn unescape_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Splits on top-level commas, honouring quoted segments.
fn parse_value(raw: &str) -> Value {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut quoted_any = false;
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                quoted_any = true;
            }
            '\\' if in_quotes => {
                current.push('\\');
                if let Some(n) = chars.next() {
                    current.push(n);
                }
            }
            ',' if !in_quotes => {
                items.push((std::mem::take(&mut current), quoted_any));
                quoted_any = false;
            }
            _ => current.push(c),
        }
    }
    items.push((current, quoted_any));

    let finish = |(s, quoted): (String, bool)| {
        if quoted {
            unescape_quoted(&s)
        } else {
            s.trim().to_owned()
        }
    };
    if items.len() == 1 {
        Value::Str(finish(items.pop().unwrap()))
    } else {
        Value::List(items.into_iter().map(finish).collect())
    }
}

/// Quotes a value when reading it back unquoted would change it.
fn encode_value(value: &str) -> String {
    let needs_quotes = value
        .chars()
        .any(|c| matches!(c, ',' | '"' | '\\' | ';' | '\n' | '\t' | '\r'))
        || value != value.trim();
    if !needs_quotes {
        return value.to_owned();
    }
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_general_and_ignores_comments() {
        let f = IniFile::parse("; comment\n# also\n[General]\nfoo = bar \n[Other]\nfoo=nope\n");
        assert_eq!(f.get("foo"), Some(Value::Str("bar".into())));
        assert!(!f.contains("missing"));
    }

    #[test]
    fn keys_before_any_section_are_general() {
        let f = IniFile::parse("width=0.5\n");
        assert_eq!(f.get("width").unwrap().to_f64(), Some(0.5));
    }

    #[test]
    fn unquoted_commas_make_a_list() {
        let f =
            IniFile::parse("[General]\nk=Shift+Left, Ctrl+Shift+Tab\nq=\"Shift+Left, Ctrl+Tab\"\n");
        assert_eq!(
            f.get("k"),
            Some(Value::List(vec![
                "Shift+Left".into(),
                "Ctrl+Shift+Tab".into()
            ]))
        );
        assert_eq!(f.get("k").unwrap().to_text(), "Shift+Left, Ctrl+Shift+Tab");
        assert_eq!(f.get("q"), Some(Value::Str("Shift+Left, Ctrl+Tab".into())));
    }

    #[test]
    fn empty_value_is_present_but_not_a_number() {
        let f = IniFile::parse("[General]\nbackgroundOpacity=\n");
        assert!(f.contains("backgroundOpacity"));
        assert_eq!(f.get("backgroundOpacity").unwrap().to_f64(), None);
        assert_eq!(f.get("backgroundOpacity").unwrap().to_text(), "");
    }

    #[test]
    fn integers_are_strict() {
        let f = IniFile::parse("a=180\nb=180.5\nc=x\n");
        assert_eq!(f.get("a").unwrap().to_i32(), Some(180));
        assert_eq!(f.get("b").unwrap().to_i32(), None);
        assert_eq!(f.get("c").unwrap().to_i32(), None);
    }

    #[test]
    fn set_roundtrips_awkward_strings() {
        let mut f = IniFile::default();
        for v in [
            "plain",
            "a, b",
            "say \"hi\"",
            " padded ",
            "",
            "#ebebeb",
            "back\\slash",
            "semi;colon",
        ] {
            f.set("k", v);
            let reread = IniFile::parse(&f.to_text());
            assert_eq!(
                reread.get("k").unwrap().to_text(),
                v,
                "value {v:?} written as {}",
                f.to_text()
            );
        }
    }

    #[test]
    fn save_is_atomic_and_preserves_other_sections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/x.conf");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[Other]\nkeep=1\n[General]\na=1\n").unwrap();
        let mut f = IniFile::load(&path);
        f.set("b", "2");
        f.remove("a");
        f.save().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[Other]\nkeep=1"));
        assert!(text.contains("b=2"));
        assert!(!text.contains("a=1"));
        // No temporary files left behind.
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }
}
