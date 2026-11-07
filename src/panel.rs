//! The dropdown itself: layout, input handling, animation and drawing.
//!
//! This is everything the QML `Panel` and the C++ `TextRender` item did, with
//! no Wayland in it: the frontend feeds it the surface size, pointer and key
//! events and the time, and asks it to draw into a pixel buffer. That is what
//! makes the whole UI testable, down to screenshots.
//!
//! Geometry follows the QML: the surface spans the usable output and is
//! transparent except for the panel, which is centred, sized as fractions of
//! the surface, and rolls down from above the surface's top edge.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::event::Event;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Point, Side};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::TermMode;

use crate::color::Rgb;
use crate::controller::Controller;
use crate::fonts::{pt_to_px, CellMetrics, FontLibrary, Fonts, Style};
use crate::keys::{self, Builtin, KeyPress, Mods, TabAction, TabShortcuts};
use crate::lineedit::{EditResult, LineEdit};
use crate::mouse::Button;
use crate::palette::{self, Palette};
use crate::render::{Canvas, Radii, Rgba};
use crate::session::{EventSink, GridSize, Session, SessionId, SpawnRequest};
use crate::settings::{Values, HEIGHT_RANGE, WIDTH_RANGE};
use crate::tabs::Tabs;

// ── Layout constants, from Panel.qml ────────────────────────────────

const TERM_TOP: f64 = 6.0;
const TERM_LEFT: f64 = 6.0;
const TERM_RIGHT_GAP: f64 = 2.0;
/// `TextRender`'s own inset: cells start 2px in, and 4px are kept free.
const CELL_MARGIN: f64 = 2.0;
const SCROLLBAR_WIDTH: f64 = 12.0;
const THUMB_WIDTH: f64 = 6.0;
const THUMB_MIN: f64 = 24.0;
const TABBAR_HEIGHT: f64 = 28.0;
const TABBAR_BOTTOM: f64 = 6.0;
const TAB_HEIGHT: f64 = 22.0;
const TAB_SPACING: f64 = 2.0;
const TAB_LABEL_PX: f64 = 11.0;
const TAB_CLOSE_W: f64 = 12.0;
const TAB_PAD: f64 = 20.0;
const RENAME_MIN_W: f64 = 80.0;
const BUTTON_W: f64 = 28.0;
const GEAR_RIGHT: f64 = 10.0;
const FLICK_LEFT: f64 = 10.0;
const FLICK_RIGHT: f64 = 8.0;
const RESIZE_LEFT: f64 = 5.0;
const RESIZE_RIGHT: f64 = 3.0;
const RESIZE_BOTTOM: f64 = 5.0;
const RESIZE_CORNER: f64 = 10.0;

const MENU_WIDTH: f64 = 190.0;
const MENU_ITEM_H: f64 = 24.0;
const MENU_PAD: f64 = 4.0;
const MENU_TEXT_PX: f64 = 12.0;

const SELECTION: Rgba = Rgba::new(0x88, 0xc0, 0xd0, 0.4);
const CURSOR_ALPHA: f32 = 180.0 / 255.0;
const BLINK: Duration = Duration::from_millis(500);
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(150);
const AUTOSCROLL: Duration = Duration::from_millis(50);
const SCROLLBAR_ANIM: Duration = Duration::from_millis(150);
/// One wheel notch in Qt's angle units, and the units per scrolled line.
const WHEEL_PER_LINE: f64 = 40.0;

// ── Geometry ────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RectF {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl RectF {
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }
    pub fn contains(&self, px: f64, py: f64) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
    pub fn right(&self) -> f64 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }
}

/// Everything's position for one frame, in surface coordinates.
#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub panel: RectF,
    pub term: RectF,
    pub scrollbar: RectF,
    pub thumb: Option<RectF>,
    pub tabbar: RectF,
    pub gear: RectF,
    /// The scrolling strip of tabs; tabs and "+" are clipped to it.
    pub flick: RectF,
    pub tabs: Vec<RectF>,
    pub closes: Vec<Option<RectF>>,
    pub plus: RectF,
    pub content_width: f64,
}

// ── Animation ───────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
enum Ease {
    Linear,
    OutCubic,
}

#[derive(Clone, Copy, Debug)]
struct Anim {
    from: f64,
    to: f64,
    start: Option<Instant>,
    duration: Duration,
    ease: Ease,
}

impl Anim {
    fn at(v: f64, ease: Ease) -> Self {
        Self {
            from: v,
            to: v,
            start: None,
            duration: Duration::ZERO,
            ease,
        }
    }

    fn value(&self, now: Instant) -> f64 {
        let Some(start) = self.start else {
            return self.to;
        };
        if self.duration.is_zero() {
            return self.to;
        }
        let t = (now.saturating_duration_since(start).as_secs_f64() / self.duration.as_secs_f64())
            .clamp(0.0, 1.0);
        let e = match self.ease {
            Ease::Linear => t,
            Ease::OutCubic => 1.0 - (1.0 - t).powi(3),
        };
        self.from + (self.to - self.from) * e
    }

    fn running(&self, now: Instant) -> bool {
        self.start.is_some_and(|s| now < s + self.duration)
    }

    /// Animates from wherever it is now, as a QML Behavior does.
    #[allow(
        clippy::float_cmp,
        reason = "targets are constants, compared to skip restarts"
    )]
    fn retarget(&mut self, to: f64, now: Instant, duration: Duration) {
        if to == self.to {
            return;
        }
        self.from = self.value(now);
        self.to = to;
        self.start = Some(now);
        self.duration = duration;
    }
}

// ── Interaction state ───────────────────────────────────────────────

/// What lies under the pointer, topmost first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Backdrop,
    /// Inside the panel but on nothing interactive.
    Panel,
    Terminal,
    Scrollbar,
    Tab(usize),
    TabClose(usize),
    TabEditor(usize),
    Plus,
    Gear,
    /// An entry of the open context menu.
    MenuItem(MenuItem),
    /// The context menu's own padding.
    Menu,
    Resize {
        width: bool,
        height: bool,
        left: bool,
    },
}

/// The terminal's right-click menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuItem {
    Copy,
    Paste,
}

impl MenuItem {
    pub const ALL: [MenuItem; 2] = [MenuItem::Copy, MenuItem::Paste];

    fn label(self) -> &'static str {
        match self {
            MenuItem::Copy => "Copy",
            MenuItem::Paste => "Paste",
        }
    }

    fn shortcut(self) -> &'static str {
        match self {
            MenuItem::Copy => "Ctrl+Shift+C",
            MenuItem::Paste => "Ctrl+Shift+V",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerButton {
    Left,
    Middle,
    Right,
}

impl PointerButton {
    fn report(self) -> Button {
        match self {
            PointerButton::Left => Button::Left,
            PointerButton::Middle => Button::Middle,
            PointerButton::Right => Button::Right,
        }
    }
    fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Grab {
    /// A press that becomes a click if released over the same target.
    Click(Target, PointerButton),
    Resize {
        width: bool,
        height: bool,
    },
    Scroll {
        start_y: f64,
        start_ratio: f64,
    },
    Select,
    /// Buttons pressed and reported to a program tracking the mouse.
    Report(u8),
}

/// The pointer shape the frontend should show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Default,
    Text,
    EwResize,
    NsResize,
    NeswResize,
    NwseResize,
}

/// Things only the frontend can do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Put text on the clipboard.
    Copy(String),
    /// Read the clipboard and hand it to [`Panel::paste`].
    Paste,
    /// Start the settings window.
    OpenSettings,
}

struct Rename {
    index: usize,
    editor: LineEdit,
}

/// How new sessions run their program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spawner {
    Pty,
    /// No program; tests feed output directly.
    Headless,
}

pub struct Panel {
    values: Values,
    palette: Palette,
    shortcuts: TabShortcuts,
    library: Arc<FontLibrary>,
    fonts: Fonts,
    metrics: CellMetrics,

    spawner: Spawner,
    sink: EventSink,
    next_id: SessionId,
    tabs: Tabs<Session>,

    controller: Controller,
    surface: (f64, f64),
    /// Size as dragged by the edges. Not written back to settings: a drag
    /// lasts until restart, or until the configured size itself changes.
    width_percent: f64,
    height_percent: f64,
    slide: Anim,
    scrollbar_width: Anim,
    tab_scroll: f64,
    rename: Option<Rename>,
    /// The open context menu's top-left, relative to the panel, so it moves
    /// with the panel if that slides.
    menu: Option<(f64, f64)>,

    pointer: Option<(f64, f64)>,
    grab: Option<Grab>,
    autoscroll: Option<(i32, Instant)>,
    wheel_remainder: f64,

    pending_grid: Option<(GridSize, Instant)>,
    cursor_on: bool,
    next_blink: Instant,
    effects: Vec<Effect>,
    dirty: bool,
}

impl Panel {
    /// # Panics
    ///
    /// When `library` holds no usable font at all.
    pub fn new(
        values: Values,
        library: Arc<FontLibrary>,
        spawner: Spawner,
        sink: EventSink,
        now: Instant,
    ) -> Self {
        let fonts =
            Fonts::new(library.clone(), &values.font_family).expect("no usable font installed");
        let metrics = fonts.metrics(pt_to_px(values.font_size));
        let first = Session::new(1, sink.clone(), SpawnRequest::default());
        let mut panel = Self {
            palette: Palette::new(values.foreground, values.background, &values.ansi_colors),
            shortcuts: TabShortcuts::parse(
                &values.previous_tab_keys,
                &values.next_tab_keys,
                &values.move_tab_left_keys,
                &values.move_tab_right_keys,
            ),
            width_percent: values.width_percent,
            height_percent: values.height_percent,
            values,
            library,
            fonts,
            metrics,
            spawner,
            sink,
            next_id: 2,
            tabs: Tabs::new(first),
            controller: Controller::default(),
            surface: (0.0, 0.0),
            slide: Anim::at(0.0, Ease::OutCubic),
            scrollbar_width: Anim::at(0.0, Ease::Linear),
            tab_scroll: 0.0,
            rename: None,
            menu: None,
            pointer: None,
            grab: None,
            autoscroll: None,
            wheel_remainder: 0.0,
            pending_grid: None,
            cursor_on: true,
            next_blink: now + BLINK,
            effects: Vec::new(),
            dirty: true,
        };
        panel.sync_scrollbar(now);
        panel
    }

    // ── Accessors ───────────────────────────────────────────────────

    pub fn controller(&self) -> &Controller {
        &self.controller
    }

    pub fn tabs(&self) -> &Tabs<Session> {
        &self.tabs
    }

    pub fn tabs_mut(&mut self) -> &mut Tabs<Session> {
        self.dirty = true;
        &mut self.tabs
    }

    pub fn values(&self) -> &Values {
        &self.values
    }

    pub fn metrics(&self) -> CellMetrics {
        self.metrics
    }

    pub fn font_family(&self) -> &str {
        self.fonts.family()
    }

    pub fn renaming(&self) -> Option<usize> {
        self.rename.as_ref().map(|r| r.index)
    }

    pub fn size_percent(&self) -> (f64, f64) {
        (self.width_percent, self.height_percent)
    }

    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    /// Whether anything changed since the last [`Panel::render`].
    pub fn dirty(&self) -> bool {
        self.dirty
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Whether an animation needs frames.
    pub fn animating(&self, now: Instant) -> bool {
        self.slide.running(now) || self.scrollbar_width.running(now)
    }

    /// The earliest moment [`Panel::tick`] has something to do.
    pub fn next_deadline(&self) -> Option<Instant> {
        [
            self.controller.deadline(),
            self.pending_grid.map(|(_, t)| t),
            self.autoscroll.map(|(_, t)| t),
            self.controller.mapped().then_some(self.next_blink),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    // ── Window lifecycle ────────────────────────────────────────────

    fn sync_slide(&mut self, now: Instant) {
        let target = if self.controller.opened() { 1.0 } else { 0.0 };
        self.slide.retarget(
            target,
            now,
            Duration::from_millis(self.values.animation_ms.max(0) as u64),
        );
        self.dirty = true;
    }

    pub fn show(&mut self, now: Instant) {
        self.controller.show();
        self.sync_slide(now);
    }

    pub fn hide(&mut self, now: Instant) {
        self.menu = None;
        self.controller.request_hide(now, self.values.animation_ms);
        self.sync_slide(now);
    }

    pub fn toggle(&mut self, now: Instant) {
        self.controller.toggle(now, self.values.animation_ms);
        self.sync_slide(now);
    }

    /// The compositor took the surface away.
    pub fn surface_lost(&mut self, now: Instant) {
        self.controller.surface_lost();
        self.menu = None;
        self.slide = Anim::at(0.0, Ease::OutCubic);
        self.grab = None;
        self.sync_slide(now);
    }

    /// Opens the settings window, which cannot share the screen with an
    /// overlay that holds the keyboard, so the dropdown goes away.
    pub fn open_settings(&mut self, now: Instant) {
        self.effects.push(Effect::OpenSettings);
        self.hide(now);
    }

    /// Ends the slide at once, for screenshots.
    pub fn finish_animations(&mut self) {
        self.slide = Anim::at(self.slide.to, Ease::OutCubic);
        self.scrollbar_width = Anim::at(self.scrollbar_width.to, Ease::Linear);
        self.dirty = true;
    }

    /// Applies any pending grid size and ends animations, without waiting:
    /// for screenshots, which must not depend on timing.
    pub fn settle(&mut self, _now: Instant) {
        self.finish_animations();
        if let Some((size, _)) = self.pending_grid.take() {
            self.apply_grid(size);
        }
    }

    /// The surface's logical size, as the compositor configured it.
    pub fn set_surface_size(&mut self, w: f64, h: f64, now: Instant) {
        if (w, h) != self.surface {
            self.surface = (w, h);
            self.dirty = true;
            self.schedule_grid(now);
        }
    }

    /// Advances timers. Call when [`Panel::next_deadline`] passes, or any time.
    pub fn tick(&mut self, now: Instant) {
        let was_mapped = self.controller.mapped();
        self.controller.tick(now);
        if was_mapped != self.controller.mapped() {
            self.dirty = true;
        }
        if now >= self.next_blink {
            self.cursor_on = !self.cursor_on;
            self.next_blink = now + BLINK;
            self.dirty = true;
        }
        if let Some((size, at)) = self.pending_grid {
            if now >= at {
                self.pending_grid = None;
                self.apply_grid(size);
            }
        }
        if let Some((dir, at)) = self.autoscroll {
            if now >= at {
                self.tabs.active_mut().scroll_lines(-dir);
                if let Some(pos) = self.pointer {
                    self.update_selection(pos);
                }
                self.autoscroll = Some((dir, now + AUTOSCROLL));
                self.sync_scrollbar(now);
                self.dirty = true;
            }
        }
    }

    // ── Settings ────────────────────────────────────────────────────

    #[allow(clippy::float_cmp, reason = "any change to a configured value counts")]
    pub fn apply_settings(&mut self, values: Values, now: Instant) {
        let old = std::mem::replace(&mut self.values, values);
        let v = &self.values;
        if old.width_percent != v.width_percent {
            self.width_percent = v.width_percent;
        }
        if old.height_percent != v.height_percent {
            self.height_percent = v.height_percent;
        }
        if old.font_family != v.font_family {
            if let Some(fonts) = Fonts::new(self.library.clone(), &v.font_family) {
                self.fonts = fonts;
            }
        }
        self.metrics = self.fonts.metrics(pt_to_px(v.font_size));
        self.palette = Palette::new(v.foreground, v.background, &v.ansi_colors);
        self.shortcuts = TabShortcuts::parse(
            &v.previous_tab_keys,
            &v.next_tab_keys,
            &v.move_tab_left_keys,
            &v.move_tab_right_keys,
        );
        self.dirty = true;
        self.schedule_grid(now);
    }

    // ── Sessions ────────────────────────────────────────────────────

    fn new_session_object(&mut self, request: SpawnRequest) -> Session {
        let id = self.next_id;
        self.next_id += 1;
        let mut s = Session::new(id, self.sink.clone(), request);
        // Start immediately when the size is known.
        if let Some(size) = self.grid_size() {
            self.start(&mut s, size);
        }
        s
    }

    fn start(&self, s: &mut Session, size: GridSize) {
        match self.spawner {
            Spawner::Pty => s.start(size, self.cell_px(), &self.values.shell_program),
            Spawner::Headless => s.start_headless(size),
        }
    }

    fn cell_px(&self) -> (u16, u16) {
        (
            self.metrics.width.round() as u16,
            self.metrics.height.round() as u16,
        )
    }

    pub fn new_tab(&mut self, now: Instant) -> usize {
        self.new_tab_with(SpawnRequest::default(), now)
    }

    fn new_tab_with(&mut self, request: SpawnRequest, now: Instant) -> usize {
        let s = self.new_session_object(request);
        let i = self.tabs.push(s);
        self.after_tabs_changed(true, now);
        i
    }

    /// `-e`: runs the command in a new tab, or in the first one if its shell
    /// has not started yet — the case when that very command launched us.
    pub fn execute(&mut self, argv: Vec<OsString>, cwd: Option<PathBuf>, now: Instant) {
        let request = SpawnRequest { argv, cwd };
        if !self.tabs.active_mut().set_request(request.clone()) {
            self.new_tab_with(request, now);
        }
        self.show(now);
    }

    pub fn close_tab(&mut self, index: usize, now: Instant) {
        if self.tabs.close(index).is_some() {
            self.after_tabs_changed(true, now);
        }
    }

    pub fn activate_tab(&mut self, index: usize, now: Instant) {
        if self.tabs.set_active(index) {
            self.after_active_changed(now);
        }
    }

    pub fn rename_tab(&mut self, index: usize, name: &str) {
        if self.tabs.rename(index, name) {
            self.dirty = true;
        }
    }

    fn after_tabs_changed(&mut self, count_changed: bool, now: Instant) {
        if count_changed {
            // The tabs are renumbered, so an open editor would now belong to
            // some other tab: drop it.
            self.rename = None;
        }
        self.after_active_changed(now);
    }

    fn after_active_changed(&mut self, now: Instant) {
        self.stop_autoscroll();
        self.menu = None;
        if matches!(self.grab, Some(Grab::Select | Grab::Report(_))) {
            self.grab = None;
        }
        // Size the newly active terminal to the current area at once.
        if let Some(size) = self.grid_size() {
            let px = self.cell_px();
            let active = self.tabs.active_index();
            let spawner = self.spawner;
            let shell = self.values.shell_program.clone();
            let s = self.tabs.get_mut(active).unwrap();
            if s.started() {
                s.resize(size, px);
            } else {
                match spawner {
                    Spawner::Pty => s.start(size, px, &shell),
                    Spawner::Headless => s.start_headless(size),
                }
            }
        }
        self.ensure_tab_visible(self.tabs.active_index());
        self.sync_scrollbar(now);
        self.dirty = true;
    }

    /// An event from a session's reader thread.
    pub fn handle_session_event(&mut self, id: SessionId, event: Event, now: Instant) {
        let Some(index) = self.tabs.position(|s| s.id() == id) else {
            return;
        };
        match event {
            Event::Wakeup | Event::MouseCursorDirty | Event::CursorBlinkingChange => {
                self.sync_scrollbar(now);
                self.dirty = true;
            }
            Event::Title(t) => self.set_title(index, t),
            Event::ResetTitle => self.set_title(index, String::new()),
            Event::PtyWrite(text) => self.reply(index, text.into_bytes()),
            Event::ColorRequest(i, format) => {
                let Some(s) = self.tabs.get(index) else {
                    return;
                };
                let osc = *s.term().lock().colors();
                let rgb = match i {
                    0..=255 => self.palette.indexed(i as u8, Some(&osc)),
                    257 => osc[i].map_or(self.palette.background, palette::from_vt),
                    _ => osc[i].map_or(self.palette.foreground, palette::from_vt),
                };
                self.reply(index, format(palette::to_vt(rgb)).into_bytes());
            }
            Event::TextAreaSizeRequest(format) => {
                let Some(size) = self.tabs.get(index).map(Session::size) else {
                    return;
                };
                let (cw, ch) = self.cell_px();
                let ws = alacritty_terminal::event::WindowSize {
                    num_lines: size.rows as u16,
                    num_cols: size.cols as u16,
                    cell_width: cw,
                    cell_height: ch,
                };
                self.reply(index, format(ws).into_bytes());
            }
            Event::ClipboardStore(_, text) => self.effects.push(Effect::Copy(text)),
            // Reading the clipboard from a program (OSC 52 query) is refused:
            // anything running in a tab could otherwise read it silently. The
            // bell has no visual counterpart here either.
            Event::ClipboardLoad(..) | Event::Bell => {}
            Event::ChildExit(_) | Event::Exit => self.hangup(index, now),
        }
    }

    /// Writes a response to a program's query back to it.
    fn reply(&self, index: usize, bytes: Vec<u8>) {
        if let Some(s) = self.tabs.get(index) {
            s.write(bytes);
        }
    }

    fn set_title(&mut self, index: usize, title: String) {
        if let Some(s) = self.tabs.get_mut(index) {
            s.set_title(title);
            self.dirty = true;
        }
    }

    /// A program exited. Its tab closes, unless it is the only one: that gets
    /// a fresh shell and the dropdown is put away, as closing it would in an
    /// ordinary terminal.
    fn hangup(&mut self, index: usize, now: Instant) {
        if self.tabs.len() > 1 {
            self.close_tab(index, now);
            return;
        }
        let fresh = self.new_session_object(SpawnRequest::default());
        self.tabs.replace(index, fresh);
        self.after_tabs_changed(false, now);
        self.hide(now);
    }

    // ── Layout ──────────────────────────────────────────────────────

    fn panel_size(&self) -> (f64, f64) {
        let (w, h) = self.surface;
        (
            (w * self.width_percent).round(),
            (h * self.height_percent).round(),
        )
    }

    fn term_rect_local(&self, scrollbar_w: f64) -> RectF {
        let (pw, ph) = self.panel_size();
        let tabbar_top = ph - TABBAR_BOTTOM - TABBAR_HEIGHT;
        RectF::new(
            TERM_LEFT,
            TERM_TOP,
            pw - scrollbar_w - TERM_RIGHT_GAP - TERM_LEFT,
            tabbar_top - TERM_TOP,
        )
    }

    /// The grid the terminal area holds, or `None` while it is too small to
    /// be meaningful (tiny sizes are ignored, as during animations).
    pub fn grid_size(&self) -> Option<GridSize> {
        if self.surface.0 <= 0.0 || self.metrics.width <= 0.0 {
            return None;
        }
        let r = self.term_rect_local(self.scrollbar_width.to);
        let cols = ((r.w - 2.0 * CELL_MARGIN) / self.metrics.width)
            .floor()
            .max(1.0) as usize;
        let rows = ((r.h - 2.0 * CELL_MARGIN) / self.metrics.height)
            .floor()
            .max(1.0) as usize;
        (cols >= 4 && rows >= 2).then_some(GridSize { cols, rows })
    }

    fn schedule_grid(&mut self, now: Instant) {
        let Some(size) = self.grid_size() else { return };
        let active = self.tabs.active();
        if !active.started() || active.size() != size {
            self.pending_grid = Some((size, now + RESIZE_DEBOUNCE));
        }
    }

    fn apply_grid(&mut self, size: GridSize) {
        let px = self.cell_px();
        let spawner = self.spawner;
        let shell = self.values.shell_program.clone();
        let s = self.tabs.active_mut();
        if s.started() {
            s.resize(size, px);
        } else {
            s.resize(size, px);
            match spawner {
                Spawner::Pty => s.start(size, px, &shell),
                Spawner::Headless => s.start_headless(size),
            }
        }
        self.dirty = true;
    }

    fn panel_x(&self) -> f64 {
        ((self.surface.0 - self.panel_size().0) / 2.0).round()
    }

    pub fn layout(&mut self, now: Instant) -> Layout {
        let (pw, ph) = self.panel_size();
        let progress = self.slide.value(now);
        let px = self.panel_x();
        let py = -(1.0 - progress) * ph;
        let panel = RectF::new(px, py, pw, ph);

        let sbw = self.scrollbar_width.value(now);
        let t = self.term_rect_local(sbw);
        let term = RectF::new(px + t.x, py + t.y, t.w, t.h);
        let tabbar = RectF::new(
            px,
            py + ph - TABBAR_BOTTOM - TABBAR_HEIGHT,
            pw,
            TABBAR_HEIGHT,
        );
        let scrollbar = RectF::new(px + pw - sbw, py, sbw, tabbar.y - py);
        let thumb = self.thumb_rect(scrollbar);

        let button_y = tabbar.y + (TABBAR_HEIGHT - TAB_HEIGHT) / 2.0;
        let gear = RectF::new(
            px + pw - GEAR_RIGHT - BUTTON_W,
            button_y,
            BUTTON_W,
            TAB_HEIGHT,
        );
        let flick_x = px + FLICK_LEFT;
        let flick = RectF::new(
            flick_x,
            tabbar.y,
            (gear.x - FLICK_RIGHT - flick_x).max(0.0),
            TABBAR_HEIGHT,
        );

        let mut tabs = Vec::with_capacity(self.tabs.len());
        let mut closes = Vec::with_capacity(self.tabs.len());
        let mut x = 0.0;
        let multiple = self.tabs.len() > 1;
        for i in 0..self.tabs.len() {
            let w = self.tab_width(i);
            let r = RectF::new(flick.x - self.tab_scroll + x, button_y, w, TAB_HEIGHT);
            closes.push(
                multiple.then(|| RectF::new(r.right() - 4.0 - TAB_CLOSE_W, r.y, TAB_CLOSE_W, r.h)),
            );
            tabs.push(r);
            x += w + TAB_SPACING;
        }
        let plus = RectF::new(
            flick.x - self.tab_scroll + x,
            button_y,
            BUTTON_W,
            TAB_HEIGHT,
        );
        let content_width = x + BUTTON_W;

        Layout {
            panel,
            term,
            scrollbar,
            thumb,
            tabbar,
            gear,
            flick,
            tabs,
            closes,
            plus,
            content_width,
        }
    }

    fn tab_width(&mut self, index: usize) -> f64 {
        let label_w = match &self.rename {
            Some(r) if r.index == index => {
                (self.fonts.measure(&r.editor.text(), TAB_LABEL_PX) + 2.0).max(RENAME_MIN_W)
            }
            _ => {
                let label = self.tabs.label(index);
                self.fonts.measure(&label, TAB_LABEL_PX)
            }
        };
        label_w + TAB_CLOSE_W + TAB_PAD
    }

    fn ensure_tab_visible(&mut self, index: usize) {
        let flick_w = {
            let (pw, _) = self.panel_size();
            (pw - GEAR_RIGHT - BUTTON_W - FLICK_RIGHT - FLICK_LEFT).max(0.0)
        };
        let mut x = 0.0;
        for i in 0..index {
            x += self.tab_width(i) + TAB_SPACING;
        }
        let w = if index < self.tabs.len() {
            self.tab_width(index)
        } else {
            0.0
        };
        if x < self.tab_scroll {
            self.tab_scroll = x;
        } else if x + w > self.tab_scroll + flick_w {
            self.tab_scroll = x + w - flick_w;
        }
    }

    // ── Scrollbar ───────────────────────────────────────────────────

    /// (total lines, visible lines, position from top).
    fn scroll_state(&self) -> (f64, f64, f64) {
        let s = self.tabs.active();
        let rows = s.size().rows as f64;
        if s.alt_screen() {
            return (rows, rows, 0.0);
        }
        let history = s.history_size() as f64;
        (rows + history, rows, history - s.display_offset() as f64)
    }

    #[allow(clippy::float_cmp, reason = "the target is one of two constants")]
    fn sync_scrollbar(&mut self, now: Instant) {
        let (total, view, _) = self.scroll_state();
        let target = if total - view > 0.0 {
            SCROLLBAR_WIDTH
        } else {
            0.0
        };
        if target != self.scrollbar_width.to {
            self.scrollbar_width.retarget(target, now, SCROLLBAR_ANIM);
            self.schedule_grid(now);
        }
    }

    fn thumb_rect(&self, track: RectF) -> Option<RectF> {
        let (total, view, pos) = self.scroll_state();
        let range = total - view;
        if range <= 0.0 || track.w <= 0.0 {
            return None;
        }
        let ratio = if view > 0.0 && total > 0.0 {
            (view / total).min(1.0)
        } else {
            1.0
        };
        let h = (track.h * ratio).max(THUMB_MIN);
        let y = (track.h - h) * (pos / range);
        Some(RectF::new(
            track.x + (track.w - THUMB_WIDTH) / 2.0,
            track.y + y,
            THUMB_WIDTH,
            h,
        ))
    }

    fn scroll_to_ratio(&mut self, ratio: f64, now: Instant) {
        let (total, view, _) = self.scroll_state();
        let range = total - view;
        let pos = (ratio.clamp(0.0, 1.0) * range).round();
        let s = self.tabs.active_mut();
        let history = s.history_size() as f64;
        s.set_display_offset((history - pos).max(0.0) as usize);
        self.sync_scrollbar(now);
        self.dirty = true;
    }

    // ── Hit testing ─────────────────────────────────────────────────

    /// Whether the right-click menu is open.
    pub fn menu_open(&self) -> bool {
        self.menu.is_some()
    }

    /// The open menu's rectangle, in surface coordinates.
    pub fn menu_rect(&mut self, now: Instant) -> Option<RectF> {
        let (mx, my) = self.menu?;
        let p = self.layout(now).panel;
        let h = MENU_PAD * 2.0 + MENU_ITEM_H * MenuItem::ALL.len() as f64;
        Some(RectF::new(p.x + mx, p.y + my, MENU_WIDTH, h))
    }

    fn menu_item_rect(menu: RectF, index: usize) -> RectF {
        RectF::new(
            menu.x + MENU_PAD,
            menu.y + MENU_PAD + index as f64 * MENU_ITEM_H,
            menu.w - 2.0 * MENU_PAD,
            MENU_ITEM_H,
        )
    }

    fn menu_item_enabled(&self, item: MenuItem) -> bool {
        match item {
            MenuItem::Copy => !self.tabs.active().selected_text().is_empty(),
            MenuItem::Paste => true,
        }
    }

    /// Opens the menu at a point, kept inside the panel so it is never
    /// clipped by the surface or drawn over the backdrop.
    fn open_menu(&mut self, x: f64, y: f64, now: Instant) {
        let p = self.layout(now).panel;
        let h = MENU_PAD * 2.0 + MENU_ITEM_H * MenuItem::ALL.len() as f64;
        let mx = (x - p.x).min(p.w - MENU_WIDTH - 4.0).max(4.0);
        let my = (y - p.y).min(p.h - h - 4.0).max(4.0);
        self.menu = Some((mx, my));
        self.dirty = true;
    }

    fn activate_menu_item(&mut self, item: MenuItem) {
        if !self.menu_item_enabled(item) {
            return; // a disabled entry ignores the click and the menu stays
        }
        self.menu = None;
        match item {
            MenuItem::Copy => {
                let text = self.tabs.active().selected_text();
                self.effects.push(Effect::Copy(text));
            }
            MenuItem::Paste => self.effects.push(Effect::Paste),
        }
        self.dirty = true;
    }

    pub fn hit(&mut self, x: f64, y: f64, now: Instant) -> Target {
        // The menu sits above everything.
        if let Some(menu) = self.menu_rect(now) {
            if menu.contains(x, y) {
                return MenuItem::ALL
                    .iter()
                    .enumerate()
                    .find(|(i, _)| Self::menu_item_rect(menu, *i).contains(x, y))
                    .map_or(Target::Menu, |(_, item)| Target::MenuItem(*item));
            }
        }
        let l = self.layout(now);
        if !l.panel.contains(x, y) {
            return Target::Backdrop;
        }
        let (lx, ly) = (x - l.panel.x, y - l.panel.y);
        let (pw, ph) = (l.panel.w, l.panel.h);
        // Resize handles sit above everything; corners above edges.
        if ly >= ph - RESIZE_CORNER && lx < RESIZE_CORNER {
            return Target::Resize {
                width: true,
                height: true,
                left: true,
            };
        }
        if ly >= ph - RESIZE_CORNER && lx >= pw - RESIZE_CORNER {
            return Target::Resize {
                width: true,
                height: true,
                left: false,
            };
        }
        if ly >= ph - RESIZE_BOTTOM {
            return Target::Resize {
                width: false,
                height: true,
                left: false,
            };
        }
        if lx < RESIZE_LEFT {
            return Target::Resize {
                width: true,
                height: false,
                left: true,
            };
        }
        if lx >= pw - RESIZE_RIGHT {
            return Target::Resize {
                width: true,
                height: false,
                left: false,
            };
        }
        if l.gear.contains(x, y) {
            return Target::Gear;
        }
        if l.flick.contains(x, y) {
            for (i, r) in l.tabs.iter().enumerate() {
                if r.contains(x, y) {
                    if self.renaming() == Some(i) {
                        return Target::TabEditor(i);
                    }
                    if l.closes[i].is_some_and(|c| c.contains(x, y)) {
                        return Target::TabClose(i);
                    }
                    // The tab's own click area stops short of its close button.
                    return if x < r.right() - TAB_CLOSE_W - 4.0 {
                        Target::Tab(i)
                    } else {
                        Target::Panel
                    };
                }
            }
            if l.plus.contains(x, y) {
                return Target::Plus;
            }
        }
        if l.scrollbar.w > 0.0 && l.scrollbar.contains(x, y) {
            return Target::Scrollbar;
        }
        if l.term.contains(x, y) {
            return Target::Terminal;
        }
        Target::Panel
    }

    pub fn cursor_shape(&mut self, now: Instant) -> CursorShape {
        let target = match self.grab {
            Some(Grab::Resize { width, height }) => {
                return match (width, height) {
                    (true, false) => CursorShape::EwResize,
                    (false, true) => CursorShape::NsResize,
                    _ => CursorShape::NwseResize,
                }
            }
            Some(Grab::Select | Grab::Report(_)) => return CursorShape::Text,
            _ => match self.pointer {
                Some((x, y)) => self.hit(x, y, now),
                None => return CursorShape::Default,
            },
        };
        match target {
            Target::Terminal => CursorShape::Text,
            Target::Resize {
                width: true,
                height: false,
                ..
            } => CursorShape::EwResize,
            Target::Resize {
                width: false,
                height: true,
                ..
            } => CursorShape::NsResize,
            Target::Resize { left: true, .. } => CursorShape::NeswResize,
            Target::Resize { .. } => CursorShape::NwseResize,
            _ => CursorShape::Default,
        }
    }

    // ── Terminal coordinates ────────────────────────────────────────

    fn term_origin(&mut self, now: Instant) -> (f64, f64, RectF) {
        let l = self.layout(now);
        (l.term.x + CELL_MARGIN, l.term.y, l.term)
    }

    /// The 0-based cell under a point, clamped to the grid, for mouse
    /// reports: a report must name a cell on screen, even for a drag past an
    /// edge.
    fn report_cell(&mut self, x: f64, y: f64, now: Instant) -> (usize, usize) {
        let (ox, oy, _) = self.term_origin(now);
        let size = self.tabs.active().size();
        let col = ((x - ox) / self.metrics.width).floor().max(0.0) as usize;
        let row = ((y - oy) / self.metrics.height).floor().max(0.0) as usize;
        (col.min(size.cols - 1), row.min(size.rows - 1))
    }

    /// The grid point and side under a point, for selection. Past the top
    /// the selection runs to the start of the line; past the bottom, to its
    /// end.
    fn selection_point(&mut self, x: f64, y: f64, now: Instant) -> (Point, Side) {
        let (ox, oy, rect) = self.term_origin(now);
        let size = self.tabs.active().size();
        let s = self.tabs.active();
        if y < rect.y {
            return (s.viewport_point(0, 0), Side::Left);
        }
        if y >= rect.bottom() {
            return (s.viewport_point(size.cols - 1, size.rows - 1), Side::Right);
        }
        let fx = ((x - ox) / self.metrics.width).max(0.0);
        let col = (fx.floor() as usize).min(size.cols - 1);
        let side = if fx >= size.cols as f64 || fx.fract() >= 0.5 {
            Side::Right
        } else {
            Side::Left
        };
        let row = (((y - oy) / self.metrics.height).floor().max(0.0) as usize).min(size.rows - 1);
        (s.viewport_point(col, row), side)
    }

    fn update_selection(&mut self, pos: (f64, f64)) {
        let now = Instant::now();
        let (p, side) = self.selection_point(pos.0, pos.1, now);
        self.tabs.active_mut().update_selection(p, side);
        self.dirty = true;
    }

    fn stop_autoscroll(&mut self) {
        self.autoscroll = None;
    }

    // ── Pointer input ───────────────────────────────────────────────

    pub fn pointer_leave(&mut self) {
        self.pointer = None;
        self.dirty = true;
    }

    pub fn pointer_motion(&mut self, x: f64, y: f64, mods: Mods, now: Instant) {
        let moved = self.pointer != Some((x, y));
        self.pointer = Some((x, y));
        if moved {
            self.dirty = true; // hover colours
        }
        match self.grab {
            Some(Grab::Resize { width, height }) => {
                let (sw, sh) = self.surface;
                if width && sw > 0.0 {
                    self.width_percent =
                        (2.0 * (x - sw / 2.0).abs() / sw).clamp(WIDTH_RANGE.0, WIDTH_RANGE.1);
                }
                if height && sh > 0.0 {
                    self.height_percent = (y / sh).clamp(HEIGHT_RANGE.0, HEIGHT_RANGE.1);
                }
                self.schedule_grid(now);
                self.dirty = true;
            }
            Some(Grab::Scroll {
                start_y,
                start_ratio,
            }) => {
                let l = self.layout(now);
                if let Some(thumb) = l.thumb {
                    let track = l.scrollbar.h - thumb.h;
                    if track > 0.0 {
                        self.scroll_to_ratio(start_ratio + (y - start_y) / track, now);
                    }
                }
            }
            Some(Grab::Select) => {
                self.update_selection((x, y));
                // Auto-scroll while dragging past the top or bottom edge.
                let (_, _, rect) = self.term_origin(now);
                let dir = if y < rect.y {
                    -1
                } else {
                    i32::from(y > rect.bottom())
                };
                match (dir, self.autoscroll) {
                    (0, _) => self.autoscroll = None,
                    (d, Some((cur, _))) if d == cur => {}
                    (d, _) => self.autoscroll = Some((d, now + AUTOSCROLL)),
                }
            }
            Some(Grab::Report(_)) => {
                let (col, row) = self.report_cell(x, y, now);
                self.tabs.active_mut().mouse_move(col, row, mods);
            }
            Some(Grab::Click(..)) => {}
            None => {
                if self.tabs.active().mouse_tracking() && self.hit(x, y, now) == Target::Terminal {
                    let (col, row) = self.report_cell(x, y, now);
                    self.tabs.active_mut().mouse_move(col, row, mods);
                }
            }
        }
    }

    pub fn pointer_press(
        &mut self,
        button: PointerButton,
        x: f64,
        y: f64,
        mods: Mods,
        now: Instant,
    ) {
        self.pointer = Some((x, y));
        if let Some(Grab::Report(held)) = self.grab {
            // Another button while some are already reported.
            let (col, row) = self.report_cell(x, y, now);
            self.tabs
                .active_mut()
                .mouse_button(button.report(), true, col, row, mods);
            self.grab = Some(Grab::Report(held | button.bit()));
            return;
        }
        if self.grab.is_some() {
            return;
        }
        let target = self.hit(x, y, now);
        if self.press_with_menu_open(target, button) {
            return;
        }
        // Pressing anywhere but the editor finishes a rename, as the editor
        // losing focus does in the QML.
        if !matches!(target, Target::TabEditor(_)) {
            self.commit_rename();
        }
        self.dirty = true;
        match target {
            Target::Resize { width, height, .. } if button == PointerButton::Left => {
                self.grab = Some(Grab::Resize { width, height });
            }
            Target::Scrollbar if button == PointerButton::Left => self.press_scrollbar(y, now),
            Target::Terminal => self.press_terminal(button, x, y, mods, now),
            Target::Backdrop
            | Target::Tab(_)
            | Target::TabClose(_)
            | Target::Plus
            | Target::Gear => {
                self.grab = Some(Grab::Click(target, button));
            }
            _ => {}
        }
    }

    /// A press while the menu is open. Returns true if it was used up.
    fn press_with_menu_open(&mut self, target: Target, button: PointerButton) -> bool {
        if self.menu.is_none() {
            return false;
        }
        match target {
            Target::MenuItem(_) if button == PointerButton::Left => {
                self.grab = Some(Grab::Click(target, button));
                true
            }
            Target::MenuItem(_) | Target::Menu => true,
            // A right click elsewhere on the terminal moves the menu there;
            // any other press just closes it, and is used up doing so — it
            // does not also act on what lies beneath.
            Target::Terminal if button == PointerButton::Right => false,
            _ => {
                self.menu = None;
                self.dirty = true;
                true
            }
        }
    }

    /// A press on the scrollbar: on the thumb it starts a drag, elsewhere it
    /// jumps there first.
    fn press_scrollbar(&mut self, y: f64, now: Instant) {
        let l = self.layout(now);
        let start_ratio = match l.thumb {
            Some(t) if y >= t.y && y <= t.bottom() => {
                let (total, view, pos) = self.scroll_state();
                let range = total - view;
                if range > 0.0 {
                    pos / range
                } else {
                    0.0
                }
            }
            _ => {
                let ratio = (y - l.scrollbar.y) / l.scrollbar.h;
                self.scroll_to_ratio(ratio, now);
                ratio.clamp(0.0, 1.0)
            }
        };
        self.grab = Some(Grab::Scroll {
            start_y: y,
            start_ratio,
        });
    }

    fn press_terminal(&mut self, button: PointerButton, x: f64, y: f64, mods: Mods, now: Instant) {
        let tracking = self.tabs.active().mouse_tracking();
        // A press goes to a program tracking the mouse, unless Shift is held:
        // as in xterm, Shift still selects text for copying.
        if tracking && !mods.shift {
            self.menu = None;
            let (col, row) = self.report_cell(x, y, now);
            self.tabs
                .active_mut()
                .mouse_button(button.report(), true, col, row, mods);
            self.grab = Some(Grab::Report(button.bit()));
        } else if button == PointerButton::Left {
            let (p, side) = self.selection_point(x, y, now);
            let s = self.tabs.active_mut();
            s.clear_selection();
            s.start_selection(p, side);
            self.grab = Some(Grab::Select);
        } else if button == PointerButton::Right {
            // The selection is kept, so the menu can copy it.
            self.open_menu(x, y, now);
        }
    }

    pub fn pointer_release(
        &mut self,
        button: PointerButton,
        x: f64,
        y: f64,
        mods: Mods,
        now: Instant,
    ) {
        self.pointer = Some((x, y));
        self.dirty = true;
        match self.grab {
            Some(Grab::Report(held)) => {
                // The release follows its press, even if the program changed
                // mode meanwhile.
                if held & button.bit() != 0 {
                    let (col, row) = self.report_cell(x, y, now);
                    self.tabs
                        .active_mut()
                        .mouse_button(button.report(), false, col, row, mods);
                    let rest = held & !button.bit();
                    self.grab = (rest != 0).then_some(Grab::Report(rest));
                }
            }
            Some(Grab::Select) if button == PointerButton::Left => {
                self.update_selection((x, y));
                self.stop_autoscroll();
                self.grab = None;
            }
            Some(Grab::Resize { .. } | Grab::Scroll { .. }) if button == PointerButton::Left => {
                self.grab = None;
            }
            Some(Grab::Click(target, b)) if b == button => {
                self.grab = None;
                if self.hit(x, y, now) == target {
                    self.click(target, button, now);
                }
            }
            _ => {}
        }
    }

    fn click(&mut self, target: Target, button: PointerButton, now: Instant) {
        match (target, button) {
            (Target::Backdrop, _) => self.hide(now),
            (Target::Tab(i), PointerButton::Right) => self.start_rename(i),
            (Target::Tab(i), PointerButton::Left) => self.activate_tab(i, now),
            (Target::TabClose(i), PointerButton::Left) => self.close_tab(i, now),
            (Target::Plus, PointerButton::Left) => {
                self.new_tab(now);
            }
            (Target::Gear, PointerButton::Left) => self.open_settings(now),
            (Target::MenuItem(item), PointerButton::Left) => self.activate_menu_item(item),
            _ => {}
        }
    }

    /// `steps` in Qt angle units: 120 per notch, positive away from the
    /// user. `pixels` is set instead for touchpads.
    pub fn wheel(&mut self, x: f64, y: f64, delta: f64, mods: Mods, now: Instant) {
        if delta == 0.0 {
            return;
        }
        if self.menu.take().is_some() {
            self.dirty = true;
            return;
        }
        let target = self.hit(x, y, now);
        match target {
            Target::Tab(_) | Target::TabClose(_) | Target::Plus | Target::TabEditor(_) => {
                let l = self.layout(now);
                let max = (l.content_width - l.flick.w).max(0.0);
                self.tab_scroll = (self.tab_scroll - delta).clamp(0.0, max);
                self.dirty = true;
            }
            Target::Terminal | Target::Scrollbar => {
                if self.tabs.active().alt_screen() {
                    // No scrollback on the alternate screen, so the wheel goes
                    // to the program. Touchpad deltas accumulate so each step
                    // is a real notch.
                    self.wheel_remainder += delta;
                    let steps = (self.wheel_remainder / WHEEL_PER_LINE).trunc();
                    self.wheel_remainder -= steps * WHEEL_PER_LINE;
                    let (col, row) = self.report_cell(x, y, now);
                    self.tabs.active_mut().wheel(steps as i32, col, row, mods);
                } else {
                    self.wheel_remainder = 0.0;
                    let lines = ((delta.abs() / WHEEL_PER_LINE) as i32).max(1);
                    self.tabs
                        .active_mut()
                        .scroll_lines(if delta > 0.0 { lines } else { -lines });
                    self.sync_scrollbar(now);
                }
                self.dirty = true;
            }
            _ => {}
        }
    }

    // ── Rename ──────────────────────────────────────────────────────

    pub fn start_rename(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        let editor = LineEdit::new(&self.tabs.name(index));
        self.rename = Some(Rename { index, editor });
        self.ensure_tab_visible(index);
        self.dirty = true;
    }

    fn commit_rename(&mut self) {
        if let Some(r) = self.rename.take() {
            self.rename_tab(r.index, &r.editor.text());
            self.dirty = true;
        }
    }

    // ── Keyboard ────────────────────────────────────────────────────

    pub fn key(&mut self, press: &KeyPress, now: Instant) {
        self.dirty = true;
        // Any key closes the menu; Escape does nothing else.
        if self.menu.take().is_some() && press.key == keys::Key::Escape {
            return;
        }
        if let Some(r) = &mut self.rename {
            match r.editor.key(press) {
                EditResult::Accept => self.commit_rename(),
                EditResult::Cancel => self.rename = None,
                EditResult::Continue => {}
            }
            return;
        }
        // Configurable tab shortcuts come first, so a binding the user chose
        // wins over the fixed ones below.
        if let Some(action) = self.shortcuts.action_for(press) {
            let changed = match action {
                TabAction::Previous => self.tabs.previous(),
                TabAction::Next => self.tabs.next(),
                TabAction::MoveLeft => self.tabs.move_active_left(),
                TabAction::MoveRight => self.tabs.move_active_right(),
            };
            if changed {
                self.after_active_changed(now);
            }
            return;
        }
        match keys::builtin_for(press) {
            Some(Builtin::Copy) => {
                let text = self.tabs.active().selected_text();
                if !text.is_empty() {
                    self.effects.push(Effect::Copy(text));
                }
            }
            Some(Builtin::Paste) => self.effects.push(Effect::Paste),
            Some(Builtin::NewTab) => {
                self.new_tab(now);
            }
            Some(Builtin::CloseTab) => {
                if self.tabs.len() > 1 {
                    let i = self.tabs.active_index();
                    self.close_tab(i, now);
                }
            }
            Some(Builtin::RenameTab) => {
                let i = self.tabs.active_index();
                self.start_rename(i);
            }
            None => {
                self.tabs.active_mut().key(press);
                self.sync_scrollbar(now);
            }
        }
    }

    pub fn paste(&mut self, text: &str) {
        if let Some(r) = &mut self.rename {
            r.editor.insert(text.lines().next().unwrap_or(""));
        } else {
            self.tabs.active_mut().paste(text);
        }
        self.dirty = true;
    }

    // ── Drawing ─────────────────────────────────────────────────────

    /// Records that a frame was presented with nothing of the panel on it
    /// (it is entirely above the surface).
    pub fn render_nothing(&mut self) {
        self.dirty = false;
    }

    /// Draws the whole surface. The buffer is the surface at `canvas.scale`.
    pub fn render(&mut self, canvas: &mut Canvas, now: Instant) {
        self.dirty = false;
        canvas.clear();
        if self.surface.0 <= 0.0 {
            return;
        }
        let l = self.layout(now);
        // Snap the sliding panel to whole device pixels so text stays crisp
        // while it moves.
        let s = canvas.scale;
        let snap = |v: f64| (v * s).round() / s;
        let dy = snap(l.panel.y) - l.panel.y;

        canvas.translated(0.0, dy, |c| {
            let bg = Rgba::with_alpha(
                self.values.background,
                self.values.background_opacity as f32,
            );
            let r = f64::from(self.values.corner_radius);
            // Top corners square and flush with the top of the usable area, so
            // it reads as dropping out of the bar rather than floating.
            let radii = Radii {
                top_left: 0.0,
                top_right: 0.0,
                bottom_right: r,
                bottom_left: r,
            };
            c.fill_round_rect(l.panel.x, l.panel.y, l.panel.w, l.panel.h, radii, bg);
            self.draw_terminal(c, &l);
            self.draw_scrollbar(c, &l, now);
            self.draw_tabbar(c, &l);
            self.draw_menu(c, now);
        });
    }

    /// What the active terminal shows, copied out so the lock is not held
    /// while drawing.
    fn visible(&self, cursor_on: bool) -> Visible {
        let palette = &self.palette;
        let default_bg = palette.background;
        let s = self.tabs.active();
        let term = s.term().lock();
        let content = term.renderable_content();
        let offset = content.display_offset as i32;
        let osc = content.colors;
        let mut cells = Vec::new();
        for item in content.display_iter {
            let cell = item.cell;
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            let mut fg = palette.resolve(cell.fg, Some(osc));
            let mut bg = palette.resolve(cell.bg, Some(osc));
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let has_text = cell.c != ' ' && cell.c != '\0';
            if !has_text && bg == default_bg && !cell.flags.intersects(Flags::ALL_UNDERLINES) {
                continue;
            }
            let mut chars = vec![cell.c];
            if let Some(zw) = cell.zerowidth() {
                chars.extend_from_slice(zw);
            }
            cells.push(VisibleCell {
                col: item.point.column.0,
                row: (item.point.line.0 + offset) as usize,
                chars,
                fg,
                bg,
                flags: cell.flags,
            });
        }
        let selection = content.selection.map(|r| {
            (
                (r.start.line.0 + offset, r.start.column.0),
                (r.end.line.0 + offset, r.end.column.0),
            )
        });
        let show_cursor = term.mode().contains(TermMode::SHOW_CURSOR) && offset == 0 && cursor_on;
        let cursor =
            show_cursor.then_some((content.cursor.point.line.0, content.cursor.point.column.0));
        Visible {
            cells,
            selection,
            cursor,
            cols: term.columns(),
        }
    }

    #[allow(
        clippy::many_single_char_names,
        reason = "c, l, m: canvas, layout, metrics"
    )]
    fn draw_terminal(&mut self, c: &mut Canvas, l: &Layout) {
        let m = self.metrics;
        let px_em = pt_to_px(self.values.font_size);
        let palette = self.palette.clone();
        let default_bg = palette.background;
        let default_fg = palette.foreground;
        let cursor_on = self.cursor_on;

        let Visible {
            cells,
            selection,
            cursor,
            cols,
        } = self.visible(cursor_on);

        let origin_x = l.term.x + CELL_MARGIN;
        let origin_y = l.term.y;
        c.clipped(l.term.x, l.term.y, l.term.w, l.term.h, |c| {
            // Background pass. The default background is transparent: the
            // panel's own rectangle *is* the terminal background.
            for cell in &cells {
                if cell.bg == default_bg {
                    continue;
                }
                let span = if cell.flags.contains(Flags::WIDE_CHAR) {
                    2.0
                } else {
                    1.0
                };
                c.fill_rect_snapped(
                    origin_x + cell.col as f64 * m.width,
                    origin_y + cell.row as f64 * m.height,
                    m.width * span,
                    m.height,
                    Rgba::opaque(cell.bg),
                );
            }
            // Text pass.
            for cell in &cells {
                let x = origin_x + cell.col as f64 * m.width;
                let baseline = origin_y + cell.row as f64 * m.height + m.ascent;
                let span = if cell.flags.contains(Flags::WIDE_CHAR) {
                    2.0
                } else {
                    1.0
                };
                let style = Style {
                    bold: cell.flags.contains(Flags::BOLD),
                    italic: cell.flags.contains(Flags::ITALIC),
                };
                let color = Rgba::opaque(cell.fg);
                if !cell.flags.contains(Flags::HIDDEN) {
                    for &ch in &cell.chars {
                        if ch != ' ' && ch != '\0' {
                            c.draw_char(&mut self.fonts, ch, style, px_em, x, baseline, color);
                        }
                    }
                }
                if cell.flags.intersects(Flags::ALL_UNDERLINES) {
                    c.fill_rect_snapped(
                        x,
                        baseline + m.underline_offset,
                        m.width * span,
                        m.underline_thickness,
                        color,
                    );
                }
                if cell.flags.contains(Flags::STRIKEOUT) {
                    let y = baseline - m.ascent * 0.3;
                    c.fill_rect_snapped(x, y, m.width * span, m.underline_thickness, color);
                }
            }
            // Selection overlay.
            if let Some(((top, left), (bottom, right))) = selection {
                let rows = self.tabs.active().size().rows as i32;
                for row in top.max(0)..=bottom.min(rows - 1) {
                    let from = if row == top { left } else { 0 };
                    let to = if row == bottom { right } else { cols - 1 };
                    if to < from {
                        continue;
                    }
                    c.fill_rect(
                        origin_x + from as f64 * m.width,
                        origin_y + f64::from(row) * m.height,
                        (to - from + 1) as f64 * m.width,
                        m.height,
                        SELECTION,
                    );
                }
            }
            if let Some((row, col)) = cursor {
                c.fill_rect(
                    origin_x + col as f64 * m.width,
                    origin_y + f64::from(row) * m.height,
                    m.width,
                    m.height,
                    Rgba::with_alpha(default_fg, CURSOR_ALPHA),
                );
            }
        });
    }

    fn draw_scrollbar(&mut self, c: &mut Canvas, l: &Layout, _now: Instant) {
        let Some(thumb) = l.thumb else { return };
        let pressed = matches!(self.grab, Some(Grab::Scroll { .. }));
        let hovered = self
            .pointer
            .is_some_and(|(x, y)| l.scrollbar.contains(x, y));
        let alpha = if pressed {
            0.6
        } else if hovered {
            0.45
        } else {
            0.3
        };
        c.fill_round_rect(
            thumb.x,
            thumb.y,
            thumb.w,
            thumb.h,
            Radii::all(3.0),
            Rgba::white(alpha),
        );
    }

    /// Baseline that vertically centres a line of `px_em` text in a box.
    fn centred_baseline(&self, top: f64, height: f64, px_em: f64) -> f64 {
        let m = self.fonts.metrics(px_em);
        top + (height - m.height) / 2.0 + m.ascent
    }

    /// The in-place editor that replaces a tab's label while renaming.
    fn draw_tab_editor(&mut self, c: &mut Canvas, r: RectF, baseline: f64) {
        let Some(rename) = &self.rename else { return };
        let text = rename.editor.text();
        let selection = rename.editor.selection();
        let cursor = rename.editor.cursor();
        let chars: Vec<char> = text.chars().collect();
        let fonts = &mut self.fonts;
        let mut prefix =
            |n: usize| fonts.measure(&chars[..n].iter().collect::<String>(), TAB_LABEL_PX);
        let text_x = r.x + 8.0;
        if let Some((from, to)) = selection {
            let x0 = text_x + prefix(from);
            let x1 = text_x + prefix(to);
            c.fill_rect(x0, r.y + 4.0, x1 - x0, r.h - 8.0, Rgba::white(0.3));
        }
        let cursor_x = text_x + prefix(cursor);
        let edit_w = r.w - TAB_CLOSE_W - TAB_PAD;
        let cursor_on = self.cursor_on;
        c.clipped(text_x, r.y, edit_w + 1.0, r.h, |c| {
            c.draw_text(
                &mut self.fonts,
                &text,
                TAB_LABEL_PX,
                text_x,
                baseline,
                Rgba::grey(0xff),
            );
            if cursor_on {
                c.fill_rect(cursor_x, r.y + 4.0, 1.0, r.h - 8.0, Rgba::grey(0xff));
            }
        });
    }

    #[allow(
        clippy::many_single_char_names,
        reason = "c, l, f, p, r: canvas, layout, rectangles"
    )]
    fn draw_tabbar(&mut self, c: &mut Canvas, l: &Layout) {
        let hover = self.pointer;
        let is_hover = |r: &RectF| hover.is_some_and(|(x, y)| r.contains(x, y));

        // Settings launcher.
        let gear_hover = is_hover(&l.gear);
        let g = l.gear;
        c.fill_round_rect(
            g.x,
            g.y,
            g.w,
            g.h,
            Radii::all(4.0),
            Rgba::white(if gear_hover { 0.15 } else { 0.05 }),
        );
        draw_gear(
            c,
            g.x + g.w / 2.0,
            g.y + g.h / 2.0,
            if gear_hover {
                Rgba::grey(0xff)
            } else {
                Rgba::grey(0xaa)
            },
        );

        let f = l.flick;
        let active = self.tabs.active_index();
        let renaming = self.renaming();
        let labels = self.tabs.labels();
        let label_baseline = self.centred_baseline(
            l.tabs.first().map_or(0.0, |r| r.y),
            TAB_HEIGHT,
            TAB_LABEL_PX,
        );
        let close_baseline = self.centred_baseline(l.plus.y, TAB_HEIGHT, 10.0);
        let plus_m = self.fonts.metrics(14.0);
        c.clipped(f.x, f.y, f.w, f.h, |c| {
            for (i, r) in l.tabs.iter().enumerate() {
                let highlighted = i == active || renaming == Some(i);
                c.fill_round_rect(
                    r.x,
                    r.y,
                    r.w,
                    r.h,
                    Radii::all(4.0),
                    Rgba::white(if highlighted { 0.15 } else { 0.05 }),
                );
                let text_x = r.x + 8.0;
                if renaming == Some(i) {
                    self.draw_tab_editor(c, *r, label_baseline);
                } else {
                    let color = if i == active {
                        Rgba::grey(0xff)
                    } else {
                        Rgba::grey(0xaa)
                    };
                    c.draw_text(
                        &mut self.fonts,
                        &labels[i],
                        TAB_LABEL_PX,
                        text_x,
                        label_baseline,
                        color,
                    );
                }
                if let Some(close) = l.closes[i] {
                    c.draw_text(
                        &mut self.fonts,
                        "x",
                        10.0,
                        close.x,
                        close_baseline,
                        Rgba::grey(0x88),
                    );
                }
            }
            // "+" button.
            let p = l.plus;
            c.fill_round_rect(p.x, p.y, p.w, p.h, Radii::all(4.0), Rgba::white(0.05));
            let w = self.fonts.measure("+", 14.0);
            let baseline = p.y + (p.h - plus_m.height) / 2.0 + plus_m.ascent;
            c.draw_text(
                &mut self.fonts,
                "+",
                14.0,
                p.x + (p.w - w) / 2.0,
                baseline,
                Rgba::grey(0xaa),
            );
        });
    }
}

impl Panel {
    fn draw_menu(&mut self, c: &mut Canvas, now: Instant) {
        let Some(menu) = self.menu_rect(now) else {
            return;
        };
        let hover = self.pointer;
        // A hairline border: the outer rectangle in the border colour, the
        // inner one in the fill.
        c.fill_round_rect(
            menu.x,
            menu.y,
            menu.w,
            menu.h,
            Radii::all(6.0),
            Rgba::white(0.14),
        );
        c.fill_round_rect(
            menu.x + 1.0,
            menu.y + 1.0,
            menu.w - 2.0,
            menu.h - 2.0,
            Radii::all(5.0),
            Rgba::new(0x1e, 0x1e, 0x22, 0.98),
        );
        for (i, item) in MenuItem::ALL.into_iter().enumerate() {
            let r = Self::menu_item_rect(menu, i);
            let enabled = self.menu_item_enabled(item);
            if enabled && hover.is_some_and(|(x, y)| r.contains(x, y)) {
                c.fill_round_rect(r.x, r.y, r.w, r.h, Radii::all(4.0), Rgba::white(0.12));
            }
            let baseline = self.centred_baseline(r.y, r.h, MENU_TEXT_PX);
            let (text, hint) = if enabled {
                (Rgba::grey(0xee), Rgba::grey(0x88))
            } else {
                (Rgba::grey(0x66), Rgba::grey(0x55))
            };
            c.draw_text(
                &mut self.fonts,
                item.label(),
                MENU_TEXT_PX,
                r.x + 8.0,
                baseline,
                text,
            );
            let hint_px = MENU_TEXT_PX - 1.0;
            let w = self.fonts.measure(item.shortcut(), hint_px);
            c.draw_text(
                &mut self.fonts,
                item.shortcut(),
                hint_px,
                r.right() - 8.0 - w,
                baseline,
                hint,
            );
        }
    }
}

struct VisibleCell {
    col: usize,
    row: usize,
    chars: Vec<char>,
    fg: Rgb,
    bg: Rgb,
    flags: Flags,
}

/// Selection ends are (viewport row, column), rows possibly off screen.
type SelectionSpan = ((i32, usize), (i32, usize));

struct Visible {
    cells: Vec<VisibleCell>,
    selection: Option<SelectionSpan>,
    cursor: Option<(i32, usize)>,
    cols: usize,
}

/// A small cog, drawn rather than taken from a font so it looks the same
/// whatever fonts are installed.
fn draw_gear(c: &mut Canvas, cx: f64, cy: f64, color: Rgba) {
    c.fill_path(
        |pb, s, (ox, oy)| {
            let (cx, cy) = ((cx as f32 + ox) * s, (cy as f32 + oy) * s);
            let teeth = 8;
            let (r_out, r_in, r_hole) = (6.0 * s, 4.6 * s, 2.0 * s);
            let step = std::f32::consts::TAU / teeth as f32;
            for i in 0..teeth {
                let a = i as f32 * step;
                let pts = [
                    (a - step * 0.30, r_in),
                    (a - step * 0.17, r_out),
                    (a + step * 0.17, r_out),
                    (a + step * 0.30, r_in),
                    (a + step * 0.70, r_in),
                ];
                for (j, (ang, r)) in pts.iter().enumerate() {
                    let (x, y) = (cx + r * ang.cos(), cy + r * ang.sin());
                    if i == 0 && j == 0 {
                        pb.move_to(x, y);
                    } else {
                        pb.line_to(x, y);
                    }
                }
            }
            pb.close();
            pb.push_circle(cx, cy, r_hole);
        },
        color,
    );
}

/// Converts a Wayland scroll to Qt's angle units (120 per notch, positive
/// away from the user). `value120` is preferred; otherwise the continuous
/// value is used as a pixel delta, as Qt's pixelDelta was.
pub fn wheel_delta(value120: i32, discrete: i32, absolute: f64) -> f64 {
    if value120 != 0 {
        -f64::from(value120)
    } else if discrete != 0 {
        -f64::from(discrete) * 120.0
    } else {
        -absolute
    }
}
