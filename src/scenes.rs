//! Deterministic scenes for screenshot tests.
//!
//! Each scene builds a [`Panel`] with headless sessions, feeds them fixed
//! terminal output, and renders at a fixed size and scale with the bundled
//! test fonts. Nothing depends on the machine, the clock or a compositor, so
//! the same scene renders to the same pixels everywhere.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::Event;
use std::time::{Duration, Instant};

use tiny_skia::Pixmap;

use crate::color::Rgb;
use crate::fonts::FontLibrary;
use crate::keys::{Key, KeyPress, Mods};
use crate::panel::{Panel, PointerButton, Spawner};
use crate::render::Canvas;
use crate::session::{EventSink, SessionId};
use crate::settings::Values;
use crate::snapshot::Image;

pub const TEST_FONT_FAMILY: &str = "DejaVu Sans Mono";

/// The bundled fonts, so rendering does not depend on what is installed.
pub fn test_fonts() -> Arc<FontLibrary> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fonts");
    let files = [
        "DejaVuSansMono.ttf",
        "DejaVuSansMono-Bold.ttf",
        "DejaVuSansMono-Oblique.ttf",
        "DejaVuSansMono-BoldOblique.ttf",
    ]
    .map(|f| dir.join(f));
    let refs: Vec<&std::path::Path> = files.iter().map(|p| p.as_path()).collect();
    Arc::new(FontLibrary::from_files(&refs).expect("test fonts are missing from tests/fonts"))
}

pub fn test_values() -> Values {
    Values {
        font_family: TEST_FONT_FAMILY.into(),
        ..Values::default()
    }
}

/// A scene under construction: a panel and a clock that only moves when
/// told to.
pub struct Stage {
    pub panel: Panel,
    events: Arc<Mutex<Vec<(SessionId, Event)>>>,
    pub now: Instant,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
}

impl Stage {
    pub fn new(values: Values, width: u32, height: u32, scale: f64) -> Self {
        Self::with_fonts(values, width, height, scale, test_fonts())
    }

    pub fn with_fonts(
        values: Values,
        width: u32,
        height: u32,
        scale: f64,
        fonts: Arc<FontLibrary>,
    ) -> Self {
        let now = Instant::now();
        // Session events (titles, mostly) are queued and delivered when the
        // stage feeds output, as the frontend's event loop would.
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink: EventSink = {
            let events = events.clone();
            Arc::new(move |id, e| events.lock().unwrap().push((id, e)))
        };
        let mut panel = Panel::new(values, fonts, Spawner::Headless, sink, now);
        panel.set_surface_size(width as f64, height as f64, now);
        panel.show(now);
        let mut stage = Self {
            panel,
            events,
            now,
            width,
            height,
            scale,
        };
        // Past the resize debounce, so the first terminal starts.
        stage.advance(Duration::from_millis(200));
        stage.panel.finish_animations();
        stage
    }

    /// Moves the clock on, firing timers. Steps stay under the cursor blink
    /// interval, so the blink phase is a function of total time only.
    pub fn advance(&mut self, by: Duration) {
        self.now += by;
        self.panel.tick(self.now);
    }

    /// Program output for the active tab. Layout that depends on it (the
    /// scrollbar appearing) settles at once, without moving the clock.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.panel.tabs_mut().active_mut().feed(bytes);
        let id = self.panel.tabs().active().id();
        self.panel.handle_session_event(id, Event::Wakeup, self.now);
        self.deliver();
        self.panel.settle(self.now);
    }

    fn deliver(&mut self) {
        let pending = std::mem::take(&mut *self.events.lock().unwrap());
        for (id, e) in pending {
            self.panel.handle_session_event(id, e, self.now);
        }
    }

    pub fn key(&mut self, key: Key, mods: Mods, text: &str) {
        let now = self.now;
        self.panel.key(&KeyPress::new(key, mods, text), now);
    }

    pub fn click(&mut self, button: PointerButton, x: f64, y: f64) {
        let now = self.now;
        self.panel.pointer_press(button, x, y, Mods::NONE, now);
        self.panel.pointer_release(button, x, y, Mods::NONE, now);
    }

    pub fn render(&mut self) -> Image {
        let w = (self.width as f64 * self.scale).round() as u32;
        let h = (self.height as f64 * self.scale).round() as u32;
        let mut pixmap = Pixmap::new(w, h).expect("non-empty scene");
        let now = self.now;
        let mut canvas = Canvas::new(&mut pixmap, self.scale);
        self.panel.render(&mut canvas, now);
        Image::from_pixmap(&pixmap)
    }
}

const PROMPT: &str =
    "\x1b[1;32mjunky\x1b[0m@\x1b[1;34mdeck\x1b[0m:\x1b[1;36m~/src/wl-dropdown\x1b[0m$ ";

fn prompt(cmd: &str) -> String {
    format!("{PROMPT}{cmd}\r\n")
}

/// A scene: a name and how to build it.
pub struct Scene {
    pub name: &'static str,
    pub build: fn() -> Stage,
}

pub fn all() -> Vec<Scene> {
    vec![
        Scene {
            name: "default",
            build: default_scene,
        },
        Scene {
            name: "colours",
            build: colours_scene,
        },
        Scene {
            name: "tabs",
            build: tabs_scene,
        },
        Scene {
            name: "rename",
            build: rename_scene,
        },
        Scene {
            name: "scrollback",
            build: scrollback_scene,
        },
        Scene {
            name: "selection",
            build: selection_scene,
        },
        Scene {
            name: "alt_screen",
            build: alt_screen_scene,
        },
        Scene {
            name: "colour_scheme",
            build: scheme_scene,
        },
        Scene {
            name: "half_open",
            build: half_open_scene,
        },
        Scene {
            name: "fractional_scale",
            build: fractional_scene,
        },
        Scene {
            name: "resized",
            build: resized_scene,
        },
        Scene {
            name: "context_menu",
            build: context_menu_scene,
        },
    ]
}

pub fn by_name(name: &str) -> Option<Scene> {
    all().into_iter().find(|s| s.name == name)
}

fn ls_output() -> String {
    let mut s = prompt("ls -la");
    let rows = [
        ("drwxr-xr-x", "4.0K", "\x1b[1;34m.\x1b[0m"),
        ("drwxr-xr-x", "4.0K", "\x1b[1;34m..\x1b[0m"),
        ("-rw-r--r--", "1.1K", "Cargo.toml"),
        ("-rw-r--r--", " 12K", "README.md"),
        ("drwxr-xr-x", "4.0K", "\x1b[1;34msrc\x1b[0m"),
        ("drwxr-xr-x", "4.0K", "\x1b[1;34mtests\x1b[0m"),
        ("-rwxr-xr-x", "2.3K", "\x1b[1;32mscreenshot.sh\x1b[0m"),
        ("-rw-r--r--", "4.8M", "\x1b[35mpreview.png\x1b[0m"),
    ];
    for (perm, size, name) in rows {
        s += &format!("{perm} 1 junky junky \x1b[33m{size}\x1b[0m Nov  7 08:54 {name}\r\n");
    }
    s += PROMPT;
    s
}

fn default_scene() -> Stage {
    let mut st = Stage::new(test_values(), 1280, 480, 1.0);
    st.feed(ls_output().as_bytes());
    st
}

fn colours_scene() -> Stage {
    let mut st = Stage::new(
        Values {
            height_percent: 0.75,
            ..test_values()
        },
        1280,
        560,
        1.0,
    );
    let mut s = prompt("colours");
    for base in [30, 90] {
        for i in 0..8 {
            s += &format!(
                "\x1b[{}m {:>2} \x1b[0m",
                base + i + 10,
                if base == 30 { i } else { i + 8 }
            );
        }
        s += "  ";
        for i in 0..8 {
            s += &format!("\x1b[{}m ab \x1b[0m", base + i);
        }
        s += "\r\n";
    }
    for row in 0..6 {
        for i in 0..36 {
            s += &format!("\x1b[48;5;{}m ", 16 + row * 36 + i);
        }
        s += "\x1b[0m\r\n";
    }
    for i in 232..256 {
        s += &format!("\x1b[48;5;{i}m  ");
    }
    s += "\x1b[0m\r\n";
    for i in 0..64 {
        let r = i * 4;
        s += &format!("\x1b[48;2;{};{};{}m ", r, 255 - r, 128);
    }
    s += "\x1b[0m\r\n";
    s += "\x1b[1mbold\x1b[0m \x1b[3mitalic\x1b[0m \x1b[1;3mbold italic\x1b[0m \x1b[4munderline\x1b[0m \x1b[9mstrike\x1b[0m \x1b[7minverse\x1b[0m \x1b[8mhidden\x1b[0m normal\r\n";
    s += "┌──┬──┐ accents: e\u{301} n\u{303} a\u{308}\r\n";
    s += "│a │b │ arrows: ← ↑ → ↓\r\n";
    s += "└──┴──┘ blocks: ░▒▓█▀▄\r\n";
    s += PROMPT;
    st.feed(s.as_bytes());
    st
}

fn tabs_scene() -> Stage {
    let mut st = Stage::new(
        Values {
            height_percent: 0.5,
            ..test_values()
        },
        1280,
        400,
        1.0,
    );
    st.feed(b"\x1b]0;~\x07");
    st.feed(prompt("").as_bytes());
    for (i, title) in ["vim Cargo.toml", "htop", "cargo test"].iter().enumerate() {
        let now = st.now;
        st.panel.new_tab(now);
        st.feed(format!("\x1b]2;{title}\x07").as_bytes());
        st.feed(format!("tab {}\r\n", i + 2).as_bytes());
    }
    st.panel.rename_tab(3, "tests");
    let now = st.now;
    st.panel.activate_tab(1, now);
    st
}

fn rename_scene() -> Stage {
    let mut st = tabs_scene();
    st.panel.start_rename(2);
    for c in "monitor".chars() {
        st.key(Key::Char(c), Mods::NONE, &c.to_string());
    }
    st
}

fn scrollback_scene() -> Stage {
    let mut st = Stage::new(
        Values {
            height_percent: 0.5,
            ..test_values()
        },
        1280,
        400,
        1.0,
    );
    let mut s = prompt("seq 1 200");
    for i in 1..=200 {
        s += &format!("{i}\r\n");
    }
    s += PROMPT;
    st.feed(s.as_bytes());
    st.panel.tabs_mut().active_mut().scroll_lines(60);
    st.feed(b"");
    st
}

fn selection_scene() -> Stage {
    let mut st = default_scene();
    let m = st.panel.metrics();
    let now = st.now;
    let l = st.panel.layout(now);
    let x0 = l.term.x + 2.0 + m.width * 11.2;
    let y0 = l.term.y + m.height * 2.5;
    let x1 = l.term.x + 2.0 + m.width * 30.7;
    let y1 = l.term.y + m.height * 4.5;
    st.panel
        .pointer_press(PointerButton::Left, x0, y0, Mods::NONE, now);
    st.panel.pointer_motion(x1, y1, Mods::NONE, now);
    st.panel
        .pointer_release(PointerButton::Left, x1, y1, Mods::NONE, now);
    st
}

fn alt_screen_scene() -> Stage {
    let mut st = Stage::new(
        Values {
            height_percent: 0.5,
            ..test_values()
        },
        1280,
        400,
        1.0,
    );
    let mut s = String::from("\x1b[?1049h\x1b[H\x1b[2J");
    s += "\x1b[7m  PID USER      PRI  NI  VIRT   RES   SHR S CPU% MEM%   TIME+  Command          \x1b[0m\r\n";
    let procs = [
        (
            "1312",
            "junky",
            "20",
            "0",
            "1.2G",
            "180M",
            " 90M",
            "S",
            " 4.0",
            " 2.2",
            "0:12.31",
            "wl-dropdown",
        ),
        (
            "2201", "junky", "20", "0", " 12M", "4.1M", "3.2M", "R", " 1.3", " 0.1", "0:00.42",
            "htop",
        ),
        (
            "  88",
            "root",
            "20",
            "0",
            " 22M",
            " 11M",
            "8.0M",
            "S",
            " 0.0",
            " 0.1",
            "0:01.07",
            "systemd-journald",
        ),
    ];
    for p in procs {
        s += &format!(
            " {} {:<9} {:>3} {:>3} {:>5} {:>5} {:>5} {} {:>4} {:>4} {:>8}  \x1b[1m{}\x1b[0m\r\n",
            p.0, p.1, p.2, p.3, p.4, p.5, p.6, p.7, p.8, p.9, p.10, p.11
        );
    }
    s += "\x1b[20;1H\x1b[30;46mF1\x1b[0mHelp  \x1b[30;46mF2\x1b[0mSetup \x1b[30;46mF10\x1b[0mQuit";
    s += "\x1b[?25l";
    st.feed(s.as_bytes());
    st
}

fn scheme_scene() -> Stage {
    let mut v = test_values();
    // As a Konsole scheme would resolve: ayu-ish colours.
    v.foreground = Rgb::new(0xbf, 0xbd, 0xb6);
    v.background = Rgb::new(0x0b, 0x0e, 0x14);
    v.background_opacity = 0.85;
    v.corner_radius = 16;
    v.width_percent = 0.8;
    v.height_percent = 0.5;
    v.font_size = 12.0;
    let ayu = [
        (0x01, 0x06, 0x0e),
        (0xea, 0x6c, 0x73),
        (0x7f, 0xd9, 0x62),
        (0xf9, 0xaf, 0x4f),
        (0x53, 0xbd, 0xfa),
        (0xcd, 0xa1, 0xfa),
        (0x90, 0xe1, 0xc6),
        (0xc7, 0xc7, 0xc7),
    ];
    for (i, (r, g, b)) in ayu.into_iter().enumerate() {
        v.ansi_colors[i] = Some(Rgb::new(r, g, b));
        v.ansi_colors[i + 8] = Some(Rgb::new(
            r.saturating_add(30),
            g.saturating_add(30),
            b.saturating_add(30),
        ));
    }
    let mut st = Stage::new(v, 1280, 480, 1.0);
    st.feed(ls_output().as_bytes());
    st
}

fn half_open_scene() -> Stage {
    let mut st = default_scene();
    let now = st.now;
    st.panel.hide(now);
    // Closing with the default 180ms OutCubic: 60ms in is a little over
    // half way up.
    st.advance(Duration::from_millis(60));
    st
}

fn fractional_scene() -> Stage {
    let mut st = Stage::new(test_values(), 1024, 400, 1.25);
    st.feed(ls_output().as_bytes());
    st
}

fn resized_scene() -> Stage {
    let mut st = default_scene();
    let now = st.now;
    let l = st.panel.layout(now);
    // Drag the bottom-right corner in.
    let (x, y) = (l.panel.right() - 2.0, l.panel.bottom() - 2.0);
    st.panel
        .pointer_press(PointerButton::Left, x, y, Mods::NONE, now);
    st.panel.pointer_motion(
        st.width as f64 * 0.85,
        st.height as f64 * 0.5,
        Mods::NONE,
        now,
    );
    st.panel.pointer_release(
        PointerButton::Left,
        st.width as f64 * 0.85,
        st.height as f64 * 0.5,
        Mods::NONE,
        now,
    );
    st.advance(Duration::from_millis(200));
    st
}

fn context_menu_scene() -> Stage {
    let mut st = selection_scene();
    let now = st.now;
    let l = st.panel.layout(now);
    let m = st.panel.metrics();
    let (x, y) = (l.term.x + 2.0 + m.width * 24.0, l.term.y + m.height * 1.5);
    st.panel
        .pointer_press(PointerButton::Right, x, y, Mods::NONE, now);
    st.panel
        .pointer_release(PointerButton::Right, x, y, Mods::NONE, now);
    // Hover the first entry, so the highlight is in the picture.
    st.panel.pointer_motion(x + 20.0, y + 14.0, Mods::NONE, now);
    st
}
