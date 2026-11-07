//! The persisted configuration, shared by the terminal and the settings window.
//!
//! Three files, each with one owner:
//!
//! | file              | owner                                              |
//! |-------------------|----------------------------------------------------|
//! | `wl-dropdown.conf`| the user's overrides — the only file ever written  |
//! | `theme.conf`      | a desktop theme engine, refreshed with `reload`    |
//! | `defaults.conf`   | read-only baseline, e.g. from the home-manager module |
//!
//! Lookup is user → theme → baseline → built-in. Putting the user first is
//! the point: choosing a colour by hand deliberately stops that key following
//! the desktop palette, and clearing the override makes it follow again.
//!
//! A file can also name a colour scheme (`colorScheme`, a Konsole/Yakuake
//! `.colorscheme`). The scheme's colours slot in directly beneath the file
//! that named it, as if pasted into that file under its own keys: picked in
//! the user file it beats the desktop theme, declared in the baseline it does
//! not, and either way a colour set by hand in the same file still wins.
//!
//! Setting a value updates memory immediately, so the UI stays live, but the
//! disk write is debounced and flushed as a single atomic write of every
//! pending key. A slider emits continuously while dragged, so per-key writes
//! would mean hundreds of file writes per drag, and a partial write would let
//! the terminal observe an incoherent set (new width, old height). After a
//! flush the running terminal is told over the control socket.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::color::{self, Rgb};
use crate::colorscheme;
use crate::ini::{IniFile, Value};
use crate::paths;

// Bounds live here rather than in the UI, so a hand-edited file cannot
// produce an unusable window either.
pub const WIDTH_RANGE: (f64, f64) = (0.2, 1.0);
pub const HEIGHT_RANGE: (f64, f64) = (0.15, 1.0);
pub const FONT_SIZE_RANGE: (f64, f64) = (4.0, 72.0);
pub const MAX_CORNER_RADIUS: i32 = 64;
pub const MAX_ANIMATION_MS: i32 = 2000;

/// Long enough to swallow a slider drag, short enough to feel immediate.
pub const FLUSH_DELAY: Duration = Duration::from_millis(300);

pub const KEY_WIDTH: &str = "widthPercent";
pub const KEY_HEIGHT: &str = "heightPercent";
pub const KEY_FONT_FAMILY: &str = "fontFamily";
pub const KEY_FONT_SIZE: &str = "fontSize";
pub const KEY_SHELL: &str = "shellProgram";
pub const KEY_FOREGROUND: &str = "foreground";
pub const KEY_BACKGROUND: &str = "background";
pub const KEY_OPACITY: &str = "backgroundOpacity";
pub const KEY_RADIUS: &str = "cornerRadius";
pub const KEY_ANIMATION: &str = "animationMs";
pub const KEY_SCHEME: &str = "colorScheme";
pub const KEY_PREV_TAB: &str = "previousTabKeys";
pub const KEY_NEXT_TAB: &str = "nextTabKeys";
pub const KEY_MOVE_LEFT: &str = "moveTabLeftKeys";
pub const KEY_MOVE_RIGHT: &str = "moveTabRightKeys";

/// Every resolved setting. Built-in values are [`Values::default`].
#[derive(Clone, Debug, PartialEq)]
pub struct Values {
    pub width_percent: f64,
    pub height_percent: f64,
    pub font_family: String,
    pub font_size: f64,
    /// Empty means the shell from `passwd`.
    pub shell_program: String,
    pub foreground: Rgb,
    pub background: Rgb,
    pub background_opacity: f64,
    pub corner_radius: i32,
    pub animation_ms: i32,
    pub color_scheme: String,
    /// `None` keeps the terminal's own colour for that index.
    pub ansi_colors: [Option<Rgb>; 16],
    /// Comma-separated key combinations; empty leaves the action unbound.
    pub previous_tab_keys: String,
    pub next_tab_keys: String,
    pub move_tab_left_keys: String,
    pub move_tab_right_keys: String,
}

impl Default for Values {
    fn default() -> Self {
        Self {
            width_percent: 0.6,
            height_percent: 0.3,
            font_family: "Hack".into(),
            font_size: 10.5,
            shell_program: String::new(),
            foreground: Rgb::new(0xeb, 0xeb, 0xeb),
            background: Rgb::new(0, 0, 0),
            background_opacity: 0.92,
            corner_radius: 8,
            animation_ms: 180,
            color_scheme: String::new(),
            ansi_colors: [None; 16],
            // Yakuake's tab bindings, with Ctrl+Tab kept for anyone used to it.
            previous_tab_keys: "Shift+Left, Ctrl+Shift+Tab".into(),
            next_tab_keys: "Shift+Right, Ctrl+Tab".into(),
            move_tab_left_keys: "Ctrl+Shift+Left".into(),
            move_tab_right_keys: "Ctrl+Shift+Right".into(),
        }
    }
}

/// Where a key's current value comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    User,
    Theme,
    Baseline,
    Scheme,
    Builtin,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::User => "user",
            Source::Theme => "theme",
            Source::Baseline => "baseline",
            Source::Scheme => "scheme",
            Source::Builtin => "builtin",
        }
    }
}

/// One rung of the lookup: a settings file, or the colour scheme a file names.
enum Layer<'a> {
    File(Source, &'a IniFile),
    Scheme(&'a HashMap<String, String>),
}

impl Layer<'_> {
    fn source(&self) -> Source {
        match self {
            Layer::File(s, _) => *s,
            Layer::Scheme(_) => Source::Scheme,
        }
    }
    fn contains(&self, key: &str) -> bool {
        match self {
            Layer::File(_, f) => f.contains(key),
            Layer::Scheme(m) => m.contains_key(key),
        }
    }
    fn value(&self, key: &str) -> Option<Value> {
        match self {
            Layer::File(_, f) => f.get(key),
            Layer::Scheme(m) => m.get(key).cloned().map(Value::Str),
        }
    }
    // A layer only counts if it holds a *usable* value for the key; an
    // unparseable entry falls through to the next layer instead of winning.
    fn f64(&self, key: &str) -> Option<f64> {
        self.value(key)?.to_f64()
    }
    fn i32(&self, key: &str) -> Option<i32> {
        self.value(key)?.to_i32()
    }
    fn colour(&self, key: &str) -> Option<Rgb> {
        color::parse(&self.value(key)?.to_text())
    }
}

fn is_colour_key(key: &str) -> bool {
    key == KEY_FOREGROUND
        || key == KEY_BACKGROUND
        || key
            .strip_prefix("color")
            .is_some_and(|n| (0..16).any(|i| n == i.to_string()))
}

pub fn ansi_key(index: usize) -> String {
    format!("color{index}")
}

pub struct Settings {
    user: IniFile,
    theme: IniFile,
    baseline: IniFile,
    baseline_path: Option<PathBuf>,
    scheme_dirs: Option<Vec<PathBuf>>,
    scheme: HashMap<String, String>,
    /// Index into the file layers of the file naming the scheme.
    scheme_owner: Option<usize>,
    values: Values,
    dirty: BTreeSet<&'static str>,
    dirty_since: Option<Instant>,
    /// Called after every write; tells a running terminal to re-read.
    on_write: Box<dyn FnMut() + Send>,
}

impl Settings {
    /// The configuration from the standard locations.
    pub fn load() -> Self {
        Self::from_files(
            paths::user_file(),
            paths::theme_file(),
            paths::defaults_file(),
        )
    }

    pub fn from_files(user: PathBuf, theme: PathBuf, baseline: PathBuf) -> Self {
        let mut s = Self {
            user: IniFile::load(user),
            theme: IniFile::load(theme),
            baseline: IniFile::load(&baseline),
            baseline_path: Some(baseline),
            scheme_dirs: None,
            scheme: HashMap::new(),
            scheme_owner: None,
            values: Values::default(),
            dirty: BTreeSet::new(),
            dirty_since: None,
            on_write: Box::new(|| {
                crate::ipc::send(b"reload");
            }),
        };
        s.resolve_all();
        s
    }

    /// Replaces what happens after a write (by default, `reload` over IPC).
    pub fn set_on_write(&mut self, f: impl FnMut() + Send + 'static) {
        self.on_write = Box::new(f);
    }

    /// Looks colour scheme names up in `dirs` instead of the XDG locations.
    pub fn set_scheme_dirs(&mut self, dirs: Vec<PathBuf>) {
        self.scheme_dirs = Some(dirs);
        self.resolve_all();
    }

    pub fn values(&self) -> &Values {
        &self.values
    }

    /// True when a baseline file exists, so the UI can say what reset means.
    pub fn has_provisioned_defaults(&self) -> bool {
        self.baseline_path.as_ref().is_some_and(|p| p.exists())
    }

    pub fn user_file(&self) -> Option<&std::path::Path> {
        self.user.path()
    }

    fn file_layers(&self) -> [(Source, &IniFile); 3] {
        [
            (Source::User, &self.user),
            (Source::Theme, &self.theme),
            (Source::Baseline, &self.baseline),
        ]
    }

    fn layers(&self) -> Vec<Layer<'_>> {
        let mut out = Vec::with_capacity(4);
        for (i, (src, file)) in self.file_layers().into_iter().enumerate() {
            out.push(Layer::File(src, file));
            if Some(i) == self.scheme_owner && !self.scheme.is_empty() {
                out.push(Layer::Scheme(&self.scheme));
            }
        }
        out
    }

    fn resolve_raw(&self, key: &str) -> Option<Value> {
        self.layers()
            .iter()
            .find(|l| l.contains(key))
            .and_then(|l| l.value(key))
    }
    fn resolve_f64(&self, key: &str, builtin: f64) -> f64 {
        self.layers()
            .iter()
            .find_map(|l| l.f64(key))
            .unwrap_or(builtin)
    }
    fn resolve_i32(&self, key: &str, builtin: i32) -> i32 {
        self.layers()
            .iter()
            .find_map(|l| l.i32(key))
            .unwrap_or(builtin)
    }
    fn resolve_colour(&self, key: &str) -> Option<Rgb> {
        self.layers().iter().find_map(|l| l.colour(key))
    }
    fn resolve_text(&self, key: &str, builtin: &str) -> String {
        // An unquoted "a, b" reads back as a list; join it again so every
        // layer yields the same comma-separated form.
        self.resolve_raw(key)
            .map_or_else(|| builtin.to_owned(), |v| v.to_text())
    }

    fn resolve_all(&mut self) {
        // The scheme first, since where it sits in the lookup depends on which
        // file named it. Found by presence: an empty value in the user file is
        // how "no scheme" overrides one declared lower down.
        let named = self
            .file_layers()
            .into_iter()
            .enumerate()
            .find_map(|(i, (_, file))| {
                Some((i, file.get(KEY_SCHEME)?.to_text().trim().to_owned()))
            });
        self.scheme_owner = named.as_ref().map(|(i, _)| *i);
        self.values.color_scheme = named.map(|(_, name)| name).unwrap_or_default();
        self.scheme.clear();
        if !self.values.color_scheme.is_empty() {
            let found = match &self.scheme_dirs {
                Some(dirs) => colorscheme::locate_in(&self.values.color_scheme, dirs),
                None => colorscheme::locate(&self.values.color_scheme),
            };
            match found {
                Some(path) => self.scheme = colorscheme::read(&path),
                None => log::warn!("colour scheme \"{}\" not found", self.values.color_scheme),
            }
        }

        let d = Values::default();
        let clamp = |v: f64, (lo, hi): (f64, f64)| v.clamp(lo, hi);
        let v = Values {
            width_percent: clamp(self.resolve_f64(KEY_WIDTH, d.width_percent), WIDTH_RANGE),
            height_percent: clamp(self.resolve_f64(KEY_HEIGHT, d.height_percent), HEIGHT_RANGE),
            font_family: self.resolve_text(KEY_FONT_FAMILY, &d.font_family),
            font_size: clamp(
                self.resolve_f64(KEY_FONT_SIZE, d.font_size),
                FONT_SIZE_RANGE,
            ),
            // Empty is meaningful here (the passwd shell), so taken as written.
            shell_program: self.resolve_text(KEY_SHELL, ""),
            foreground: self.resolve_colour(KEY_FOREGROUND).unwrap_or(d.foreground),
            background: self.resolve_colour(KEY_BACKGROUND).unwrap_or(d.background),
            background_opacity: clamp(
                self.resolve_f64(KEY_OPACITY, d.background_opacity),
                (0.0, 1.0),
            ),
            corner_radius: self
                .resolve_i32(KEY_RADIUS, d.corner_radius)
                .clamp(0, MAX_CORNER_RADIUS),
            animation_ms: self
                .resolve_i32(KEY_ANIMATION, d.animation_ms)
                .clamp(0, MAX_ANIMATION_MS),
            color_scheme: std::mem::take(&mut self.values.color_scheme),
            ansi_colors: std::array::from_fn(|i| self.resolve_colour(&ansi_key(i))),
            // Empty means unbound, so like the shell these are taken as written.
            previous_tab_keys: self.resolve_text(KEY_PREV_TAB, &d.previous_tab_keys),
            next_tab_keys: self.resolve_text(KEY_NEXT_TAB, &d.next_tab_keys),
            move_tab_left_keys: self.resolve_text(KEY_MOVE_LEFT, &d.move_tab_left_keys),
            move_tab_right_keys: self.resolve_text(KEY_MOVE_RIGHT, &d.move_tab_right_keys),
        };
        self.values = v;
    }

    /// Re-reads every layer from disk. The terminal calls this when told a
    /// file changed; every layer, not just the two that usually move, since a
    /// rebuild can replace the baseline under a running terminal.
    pub fn reload(&mut self) {
        if !self.dirty.is_empty() {
            self.flush();
        }
        self.user.reload();
        self.theme.reload();
        self.baseline.reload();
        self.resolve_all();
    }

    /// Which layer a key's current value comes from. A key set but not yet
    /// flushed counts as the user's. Colour keys report the layer that
    /// actually supplied a *valid* colour, not merely one that mentions them.
    pub fn source_of(&self, key: &str) -> Source {
        if self.dirty.contains(key) {
            return Source::User;
        }
        let colour = is_colour_key(key);
        self.layers()
            .iter()
            .find(|l| {
                if colour {
                    l.colour(key).is_some()
                } else {
                    l.contains(key)
                }
            })
            .map_or(Source::Builtin, Layer::source)
    }

    /// Drops this key's user override so it follows the lower layers again.
    /// This is how "stop customising, follow the desktop theme" is expressed;
    /// there is no separate follow/don't-follow flag to keep in sync.
    pub fn clear_override(&mut self, key: &str) {
        let pending = self.dirty.iter().position(|k| *k == key).is_some();
        if !pending && !self.user.contains(key) {
            return;
        }
        self.dirty.retain(|k| *k != key); // a pending write for this key is now moot
        self.flush(); // anything else pending must not be lost by the re-read
        self.user.remove(key);
        self.write_user();
        self.reload();
    }

    /// Drops every user override, falling back to the baseline and then to
    /// the built-in values. Clearing rather than writing built-ins back is
    /// what makes reset mean "return to the declared baseline" when there is
    /// one.
    pub fn reset_to_defaults(&mut self) {
        self.dirty.clear();
        self.dirty_since = None;
        self.user.clear();
        self.write_user();
        self.reload();
    }

    // ── Debounced writes ────────────────────────────────────────────

    fn mark_dirty(&mut self, key: &'static str) {
        self.dirty.insert(key);
        // Restarts: the write lands after the last change.
        self.dirty_since = Some(Instant::now());
    }

    /// When the pending write is due, if one is pending.
    pub fn flush_deadline(&self) -> Option<Instant> {
        self.dirty_since.map(|t| t + FLUSH_DELAY)
    }

    pub fn flush_if_due(&mut self, now: Instant) {
        if self.flush_deadline().is_some_and(|d| d <= now) {
            self.flush();
        }
    }

    pub fn has_pending_writes(&self) -> bool {
        !self.dirty.is_empty()
    }

    fn encoded(&self, key: &str) -> String {
        let v = &self.values;
        match key {
            KEY_WIDTH => v.width_percent.to_string(),
            KEY_HEIGHT => v.height_percent.to_string(),
            KEY_FONT_FAMILY => v.font_family.clone(),
            KEY_FONT_SIZE => v.font_size.to_string(),
            KEY_SHELL => v.shell_program.clone(),
            KEY_FOREGROUND => v.foreground.to_hex(),
            KEY_BACKGROUND => v.background.to_hex(),
            KEY_OPACITY => v.background_opacity.to_string(),
            KEY_RADIUS => v.corner_radius.to_string(),
            KEY_ANIMATION => v.animation_ms.to_string(),
            KEY_SCHEME => v.color_scheme.clone(),
            KEY_PREV_TAB => v.previous_tab_keys.clone(),
            KEY_NEXT_TAB => v.next_tab_keys.clone(),
            KEY_MOVE_LEFT => v.move_tab_left_keys.clone(),
            KEY_MOVE_RIGHT => v.move_tab_right_keys.clone(),
            _ => unreachable!("not a settable key: {key}"),
        }
    }

    /// One coherent write of everything pending.
    pub fn flush(&mut self) {
        self.dirty_since = None;
        if self.dirty.is_empty() {
            return;
        }
        for key in std::mem::take(&mut self.dirty) {
            let value = self.encoded(key);
            self.user.set(key, &value);
        }
        self.write_user();
    }

    fn write_user(&mut self) {
        if let Err(err) = self.user.save() {
            log::error!(
                "cannot write {}: {err}",
                self.user
                    .path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            );
        }
        (self.on_write)();
    }

    // ── Setters ─────────────────────────────────────────────────────
    // Each clamps or normalises, and only marks the key dirty if the value
    // actually moved.

    fn set_f64(
        &mut self,
        key: &'static str,
        v: f64,
        range: (f64, f64),
        get: fn(&mut Values) -> &mut f64,
    ) {
        if !v.is_finite() {
            return;
        }
        let v = v.clamp(range.0, range.1);
        let slot = get(&mut self.values);
        if *slot != v {
            *slot = v;
            self.mark_dirty(key);
        }
    }

    fn set_i32(&mut self, key: &'static str, v: i32, max: i32, get: fn(&mut Values) -> &mut i32) {
        let v = v.clamp(0, max);
        let slot = get(&mut self.values);
        if *slot != v {
            *slot = v;
            self.mark_dirty(key);
        }
    }

    fn set_string(&mut self, key: &'static str, v: String, get: fn(&mut Values) -> &mut String) {
        let slot = get(&mut self.values);
        if *slot != v {
            *slot = v;
            self.mark_dirty(key);
        }
    }

    pub fn set_width_percent(&mut self, v: f64) {
        self.set_f64(KEY_WIDTH, v, WIDTH_RANGE, |s| &mut s.width_percent);
    }
    pub fn set_height_percent(&mut self, v: f64) {
        self.set_f64(KEY_HEIGHT, v, HEIGHT_RANGE, |s| &mut s.height_percent);
    }
    pub fn set_font_size(&mut self, v: f64) {
        self.set_f64(KEY_FONT_SIZE, v, FONT_SIZE_RANGE, |s| &mut s.font_size);
    }
    pub fn set_background_opacity(&mut self, v: f64) {
        self.set_f64(KEY_OPACITY, v, (0.0, 1.0), |s| &mut s.background_opacity);
    }
    pub fn set_corner_radius(&mut self, v: i32) {
        self.set_i32(KEY_RADIUS, v, MAX_CORNER_RADIUS, |s| &mut s.corner_radius);
    }
    pub fn set_animation_ms(&mut self, v: i32) {
        self.set_i32(KEY_ANIMATION, v, MAX_ANIMATION_MS, |s| &mut s.animation_ms);
    }
    /// An empty family is ignored.
    pub fn set_font_family(&mut self, v: &str) {
        if !v.is_empty() {
            self.set_string(KEY_FONT_FAMILY, v.to_owned(), |s| &mut s.font_family);
        }
    }
    pub fn set_shell_program(&mut self, v: &str) {
        self.set_string(KEY_SHELL, v.to_owned(), |s| &mut s.shell_program);
    }
    /// An invalid colour is ignored.
    pub fn set_foreground(&mut self, v: &str) {
        if let Some(c) = color::parse(v) {
            if self.values.foreground != c {
                self.values.foreground = c;
                self.mark_dirty(KEY_FOREGROUND);
            }
        }
    }
    pub fn set_background(&mut self, v: &str) {
        if let Some(c) = color::parse(v) {
            if self.values.background != c {
                self.values.background = c;
                self.mark_dirty(KEY_BACKGROUND);
            }
        }
    }
    pub fn set_previous_tab_keys(&mut self, v: &str) {
        self.set_string(KEY_PREV_TAB, v.trim().to_owned(), |s| {
            &mut s.previous_tab_keys
        });
    }
    pub fn set_next_tab_keys(&mut self, v: &str) {
        self.set_string(KEY_NEXT_TAB, v.trim().to_owned(), |s| &mut s.next_tab_keys);
    }
    pub fn set_move_tab_left_keys(&mut self, v: &str) {
        self.set_string(KEY_MOVE_LEFT, v.trim().to_owned(), |s| {
            &mut s.move_tab_left_keys
        });
    }
    pub fn set_move_tab_right_keys(&mut self, v: &str) {
        self.set_string(KEY_MOVE_RIGHT, v.trim().to_owned(), |s| {
            &mut s.move_tab_right_keys
        });
    }

    /// Unlike every other key this one moves a whole layer, so it cannot be
    /// applied in memory alone: it is written at once, along with anything
    /// else pending, and everything is resolved again.
    pub fn set_color_scheme(&mut self, v: &str) {
        let name = v.trim().to_owned();
        if self.values.color_scheme == name {
            return;
        }
        self.values.color_scheme = name;
        self.mark_dirty(KEY_SCHEME);
        self.flush();
        self.reload();
    }
}

impl Drop for Settings {
    fn drop(&mut self) {
        // Never lose an edit to a debounce window still in flight.
        if !self.dirty.is_empty() {
            self.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct Fixture {
        dir: tempfile::TempDir,
        writes: Arc<AtomicUsize>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                dir: tempfile::tempdir().unwrap(),
                writes: Arc::new(AtomicUsize::new(0)),
            }
        }
        fn path(&self, name: &str) -> PathBuf {
            self.dir.path().join(name)
        }
        fn write(&self, name: &str, text: &str) {
            fs::write(self.path(name), text).unwrap();
        }
        fn settings(&self) -> Settings {
            let mut s = Settings::from_files(
                self.path("user.conf"),
                self.path("theme.conf"),
                self.path("defaults.conf"),
            );
            let w = self.writes.clone();
            s.set_on_write(move || {
                w.fetch_add(1, Ordering::SeqCst);
            });
            s.set_scheme_dirs(vec![self.path("schemes")]);
            s
        }
    }

    #[test]
    fn builtin_defaults() {
        let f = Fixture::new();
        let s = f.settings();
        assert_eq!(s.values(), &Values::default());
        assert_eq!(s.source_of(KEY_FOREGROUND), Source::Builtin);
        assert!(!s.has_provisioned_defaults());
    }

    #[test]
    fn layer_precedence_user_theme_baseline() {
        let f = Fixture::new();
        f.write(
            "defaults.conf",
            "[General]\nforeground=#111111\nbackground=#222222\nfontSize=12\n",
        );
        f.write("theme.conf", "[General]\nforeground=#333333\n");
        f.write("user.conf", "[General]\nfontSize=14\n");
        let s = f.settings();
        assert_eq!(s.values().foreground.to_hex(), "#333333");
        assert_eq!(s.values().background.to_hex(), "#222222");
        assert_eq!(s.values().font_size, 14.0);
        assert_eq!(s.source_of(KEY_FOREGROUND), Source::Theme);
        assert_eq!(s.source_of(KEY_BACKGROUND), Source::Baseline);
        assert_eq!(s.source_of(KEY_FONT_SIZE), Source::User);
        assert!(s.has_provisioned_defaults());
    }

    #[test]
    fn unusable_values_fall_through() {
        let f = Fixture::new();
        // A half-rendered template: empty and garbage entries.
        f.write(
            "theme.conf",
            "[General]\nbackgroundOpacity=\nforeground={{colors.fg}}\ncornerRadius=abc\n",
        );
        f.write(
            "defaults.conf",
            "[General]\nbackgroundOpacity=0.5\nforeground=#123456\n",
        );
        let s = f.settings();
        assert_eq!(s.values().background_opacity, 0.5);
        assert_eq!(s.values().foreground.to_hex(), "#123456");
        assert_eq!(s.values().corner_radius, 8);
        // The tag names the layer that actually supplied the colour.
        assert_eq!(s.source_of(KEY_FOREGROUND), Source::Baseline);
    }

    #[test]
    fn values_are_clamped() {
        let f = Fixture::new();
        f.write(
            "user.conf",
            "widthPercent=5\nheightPercent=0\nfontSize=1\nbackgroundOpacity=-2\ncornerRadius=999\nanimationMs=-5\n",
        );
        let v = f.settings().values().clone();
        assert_eq!(v.width_percent, 1.0);
        assert_eq!(v.height_percent, 0.15);
        assert_eq!(v.font_size, 4.0);
        assert_eq!(v.background_opacity, 0.0);
        assert_eq!(v.corner_radius, 64);
        assert_eq!(v.animation_ms, 0);
    }

    #[test]
    fn tab_keys_from_lists_and_empty_unbinds() {
        let f = Fixture::new();
        f.write(
            "user.conf",
            "nextTabKeys=Ctrl+PgDown, Alt+Right\nmoveTabLeftKeys=\n",
        );
        let v = f.settings().values().clone();
        assert_eq!(v.next_tab_keys, "Ctrl+PgDown, Alt+Right");
        assert_eq!(v.move_tab_left_keys, "");
        assert_eq!(v.previous_tab_keys, "Shift+Left, Ctrl+Shift+Tab");
    }

    fn write_scheme(f: &Fixture) {
        fs::create_dir_all(f.path("schemes")).unwrap();
        fs::write(
            f.path("schemes/ayu.colorscheme"),
            "[Foreground]\nColor=1,1,1\n[Background]\nColor=2,2,2\n[Color1]\nColor=3,3,3\n[General]\nOpacity=0.7\n",
        )
        .unwrap();
    }

    #[test]
    fn scheme_sits_beneath_the_file_that_named_it() {
        let f = Fixture::new();
        write_scheme(&f);
        // Named by the user: beats the theme, loses to the user's own colour.
        f.write("user.conf", "colorScheme=ayu\nbackground=#abcdef\n");
        f.write("theme.conf", "foreground=#999999\n");
        let s = f.settings();
        assert_eq!(s.values().foreground.to_hex(), "#010101");
        assert_eq!(s.values().background.to_hex(), "#abcdef");
        assert_eq!(s.values().ansi_colors[1].unwrap().to_hex(), "#030303");
        assert_eq!(s.values().ansi_colors[0], None);
        assert_eq!(s.values().background_opacity, 0.7);
        assert_eq!(s.source_of(KEY_FOREGROUND), Source::Scheme);
        assert_eq!(s.source_of(KEY_SCHEME), Source::User);
    }

    #[test]
    fn scheme_in_baseline_loses_to_theme_and_empty_user_value_disables_it() {
        let f = Fixture::new();
        write_scheme(&f);
        f.write("defaults.conf", "colorScheme=ayu\n");
        f.write("theme.conf", "foreground=#999999\n");
        let s = f.settings();
        assert_eq!(s.values().foreground.to_hex(), "#999999");
        assert_eq!(s.values().background.to_hex(), "#020202");
        drop(s);

        f.write("user.conf", "colorScheme=\n");
        let s = f.settings();
        assert_eq!(s.values().color_scheme, "");
        assert_eq!(s.values().background.to_hex(), "#000000");
    }

    #[test]
    fn writes_are_debounced_and_coherent() {
        let f = Fixture::new();
        let mut s = f.settings();
        s.set_width_percent(0.7);
        s.set_height_percent(0.4);
        s.set_width_percent(0.75);
        assert!(s.has_pending_writes());
        assert_eq!(
            s.source_of(KEY_WIDTH),
            Source::User,
            "pending counts as user"
        );
        assert!(!f.path("user.conf").exists());
        assert_eq!(f.writes.load(Ordering::SeqCst), 0);

        let deadline = s.flush_deadline().unwrap();
        s.flush_if_due(deadline - Duration::from_millis(1));
        assert!(s.has_pending_writes());
        s.flush_if_due(deadline);
        assert!(!s.has_pending_writes());
        assert_eq!(
            f.writes.load(Ordering::SeqCst),
            1,
            "one write for the whole burst"
        );

        let text = fs::read_to_string(f.path("user.conf")).unwrap();
        assert!(text.contains("widthPercent=0.75"), "{text}");
        assert!(text.contains("heightPercent=0.4"), "{text}");
    }

    #[test]
    fn setters_ignore_invalid_and_unchanged_values() {
        let f = Fixture::new();
        let mut s = f.settings();
        s.set_foreground("not a colour");
        s.set_font_family("");
        s.set_width_percent(0.6);
        s.set_width_percent(f64::NAN);
        assert!(!s.has_pending_writes());
        s.set_foreground("#ABC");
        assert_eq!(s.values().foreground.to_hex(), "#aabbcc");
        s.set_next_tab_keys("  Ctrl+Tab  ");
        assert_eq!(s.values().next_tab_keys, "Ctrl+Tab");
    }

    #[test]
    fn clear_override_follows_lower_layers_again() {
        let f = Fixture::new();
        f.write("theme.conf", "foreground=#333333\n");
        f.write("user.conf", "foreground=#ffffff\nfontSize=20\n");
        let mut s = f.settings();
        assert_eq!(s.source_of(KEY_FOREGROUND), Source::User);
        s.clear_override(KEY_FOREGROUND);
        assert_eq!(s.values().foreground.to_hex(), "#333333");
        assert_eq!(s.source_of(KEY_FOREGROUND), Source::Theme);
        assert_eq!(s.values().font_size, 20.0, "other overrides untouched");
        assert!(!fs::read_to_string(f.path("user.conf"))
            .unwrap()
            .contains("foreground"));

        // A value set but not yet flushed can be reverted too.
        s.set_background("#010203");
        s.clear_override(KEY_BACKGROUND);
        assert_eq!(s.values().background.to_hex(), "#000000");
        assert_eq!(s.source_of(KEY_BACKGROUND), Source::Builtin);
    }

    #[test]
    fn reset_returns_to_the_baseline() {
        let f = Fixture::new();
        f.write("defaults.conf", "widthPercent=0.9\n");
        f.write("user.conf", "widthPercent=0.3\nfontFamily=Iosevka\n");
        let mut s = f.settings();
        s.set_height_percent(0.8);
        s.reset_to_defaults();
        assert_eq!(s.values().width_percent, 0.9);
        assert_eq!(s.values().height_percent, 0.3);
        assert_eq!(s.values().font_family, "Hack");
        assert!(!s.has_pending_writes());
    }

    #[test]
    fn setting_a_scheme_applies_immediately() {
        let f = Fixture::new();
        write_scheme(&f);
        let mut s = f.settings();
        s.set_width_percent(0.5); // pending, must survive the scheme's re-read
        s.set_color_scheme(" ayu ");
        assert_eq!(s.values().color_scheme, "ayu");
        assert_eq!(s.values().foreground.to_hex(), "#010101");
        assert_eq!(s.values().width_percent, 0.5);
        assert!(!s.has_pending_writes());
    }

    #[test]
    fn reload_picks_up_external_changes() {
        let f = Fixture::new();
        let mut s = f.settings();
        f.write("theme.conf", "background=#202020\n");
        s.reload();
        assert_eq!(s.values().background.to_hex(), "#202020");
    }

    #[test]
    fn drop_flushes_pending_edits() {
        let f = Fixture::new();
        {
            let mut s = f.settings();
            s.set_corner_radius(12);
        }
        assert!(fs::read_to_string(f.path("user.conf"))
            .unwrap()
            .contains("cornerRadius=12"));
    }

    #[test]
    fn colour_key_detection() {
        assert!(is_colour_key("foreground"));
        assert!(is_colour_key("color0"));
        assert!(is_colour_key("color15"));
        assert!(!is_colour_key("color16"));
        assert!(!is_colour_key("colorScheme"));
    }
}
