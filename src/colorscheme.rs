//! Konsole `.colorscheme` files — what Yakuake uses too — translated into
//! wl-dropdown's keys.
//!
//! They are `KConfig` INI, but read by hand: stock schemes put comments after
//! group names (`[Color0] # Black`), which a strict INI reader rejects.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::color::{self, Rgb};
use crate::paths;

/// Where `colorScheme` names are looked up, in order: our own directory, then
/// `konsole/` under each XDG data directory, which is where Konsole and
/// Yakuake keep theirs.
pub fn dirs() -> Vec<PathBuf> {
    let mut dirs = vec![paths::config_dir().join("colorschemes")];
    dirs.extend(paths::data_dirs().into_iter().map(|d| d.join("konsole")));
    dirs
}

/// The file a `colorScheme` value refers to. A value containing a slash is a
/// path (a leading `~/` is expanded); anything else is a name to search for.
pub fn locate(name: &str) -> Option<PathBuf> {
    locate_in(name, &dirs())
}

pub fn locate_in(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    let mut n = name.trim().to_owned();
    if n.is_empty() {
        return None;
    }
    if let Some(rest) = n.strip_prefix("~/") {
        n = paths::home_dir().join(rest).to_string_lossy().into_owned();
    }
    if n.contains('/') {
        let p = PathBuf::from(&n);
        return p.is_file().then_some(p);
    }
    if !n.ends_with(".colorscheme") {
        n.push_str(".colorscheme");
    }
    dirs.iter().map(|d| d.join(&n)).find(|p| p.is_file())
}

/// Names of every scheme found, deduplicated and sorted case-insensitively.
pub fn available() -> Vec<String> {
    available_in(&dirs())
}

pub fn available_in(dirs: &[PathBuf]) -> Vec<String> {
    let mut names: Vec<String> = dirs
        .iter()
        .filter_map(|d| fs::read_dir(d).ok())
        .flatten()
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| {
            e.file_name()
                .to_str()?
                .strip_suffix(".colorscheme")
                .map(str::to_owned)
        })
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup();
    names
}

/// Konsole writes `Color=r,g,b` (optionally `,a`); `KConfig` also accepts
/// colour names and `#rrggbb`, so take those too.
fn parse_scheme_colour(value: &str) -> Option<Rgb> {
    let parts: Vec<&str> = value.split(',').collect();
    if parts.len() < 3 {
        return color::parse(value);
    }
    let channel = |s: &str| {
        s.trim()
            .parse::<i32>()
            .ok()
            .filter(|v| (0..=255).contains(v))
            .map(|v| v as u8)
    };
    Some(Rgb::new(
        channel(parts[0])?,
        channel(parts[1])?,
        channel(parts[2])?,
    ))
}

/// The key a scheme group sets. Konsole's `ColorN`/`ColorNIntense` are the
/// normal and bright halves of the ANSI palette. Faint variants, the intense
/// and faint foreground and background, and colour randomisation have nothing
/// to map to and are ignored.
fn scheme_key(group: &str) -> Option<String> {
    match group {
        "Foreground" => return Some("foreground".into()),
        "Background" => return Some("background".into()),
        _ => {}
    }
    let rest = group.strip_prefix("Color")?;
    let mut chars = rest.chars();
    let digit = chars.next()?.to_digit(10).filter(|d| *d <= 7)?;
    match chars.as_str() {
        "" => Some(format!("color{digit}")),
        "Intense" => Some(format!("color{}", digit + 8)),
        _ => None,
    }
}

/// Reads a scheme into wl-dropdown's keys (`foreground`, `background`,
/// `color0`…`color15`, `backgroundOpacity`). Values are left as text:
/// resolution parses and validates every layer alike.
pub fn read(path: &Path) -> HashMap<String, String> {
    fs::read_to_string(path)
        .map(|t| parse(&t))
        .unwrap_or_default()
}

pub fn parse(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut group = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            group = match line.find(']') {
                Some(close) if close > 0 => line[1..close].trim().to_owned(),
                _ => String::new(),
            };
            continue;
        }
        let Some(eq) = line.find('=') else { continue };
        let (key, value) = (line[..eq].trim(), line[eq + 1..].trim());

        if group == "General" && key == "Opacity" {
            out.insert("backgroundOpacity".to_owned(), value.to_owned());
            continue;
        }
        if key != "Color" {
            continue;
        }
        if let (Some(target), Some(colour)) = (scheme_key(&group), parse_scheme_colour(value)) {
            out.insert(target, colour.to_hex());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
[Background]
Color=11,14,20

[BackgroundIntense]
Color=1,2,3

[Color0] # Black
Color=1,6,14

[Color1Intense]
Color=240,113,120,255

[Color7Faint]
Color=9,9,9

[Color3]
Color=#ffcc66

[Foreground]
Color=191,189,182

[General]
Description=Ayu
Opacity=0.85
Blur=true
";

    #[test]
    fn maps_konsole_groups() {
        let m = parse(SAMPLE);
        assert_eq!(m["background"], "#0b0e14");
        assert_eq!(m["foreground"], "#bfbdb6");
        assert_eq!(m["color0"], "#01060e");
        assert_eq!(m["color9"], "#f07178");
        assert_eq!(m["color3"], "#ffcc66");
        assert_eq!(m["backgroundOpacity"], "0.85");
        // Faint and intense-background variants have no equivalent.
        assert_eq!(m.len(), 6, "{m:?}");
    }

    #[test]
    fn rejects_out_of_range_channels() {
        let m = parse("[Color2]\nColor=300,0,0\n");
        assert_eq!(m, HashMap::new());
    }

    #[test]
    fn locate_by_name_and_path() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        fs::write(a.path().join("ayu.colorscheme"), SAMPLE).unwrap();
        fs::write(b.path().join("ayu.colorscheme"), "").unwrap();
        fs::write(b.path().join("Breeze.colorscheme"), "").unwrap();
        let dirs = vec![a.path().to_path_buf(), b.path().to_path_buf()];

        assert_eq!(
            locate_in("ayu", &dirs),
            Some(a.path().join("ayu.colorscheme"))
        );
        assert_eq!(
            locate_in("ayu.colorscheme", &dirs),
            Some(a.path().join("ayu.colorscheme"))
        );
        assert_eq!(
            locate_in("Breeze", &dirs),
            Some(b.path().join("Breeze.colorscheme"))
        );
        assert_eq!(locate_in("missing", &dirs), None);
        assert_eq!(locate_in("  ", &dirs), None);

        let abs = a.path().join("ayu.colorscheme");
        assert_eq!(locate_in(abs.to_str().unwrap(), &[]), Some(abs.clone()));
        assert_eq!(locate_in("/nope/x.colorscheme", &dirs), None);

        assert_eq!(
            available_in(&dirs),
            vec!["ayu".to_owned(), "Breeze".to_owned()]
        );
    }
}
