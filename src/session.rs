//! One terminal: an alacritty_terminal screen model, optionally driven by a
//! program on a pty.
//!
//! A session is created before it is started, because the size of the
//! terminal is not known until the window has been laid out; `-e` can still
//! replace what the first, not-yet-started session will run.
//!
//! Sessions can also be *headless*: no pty, bytes fed in directly. That is
//! what the screenshot tests render.

use std::borrow::Cow;
use std::ffi::OsString;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::sync::Arc;

use alacritty_terminal::event::{Event, EventListener, Notify, OnResize, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, Msg, Notifier};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{self, Term, TermMode};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::Processor;

use crate::keys::{self, KeyModes, KeyPress};
use crate::mouse::{self, Button, MouseMode, MouseReporter};
use crate::tabs::TabSession;

/// Matches the C++ version's scrollback cap.
pub const SCROLLBACK_LINES: usize = 5000;

pub type SessionId = u64;

/// Where session events go: the frontend's event loop, or nowhere in tests.
pub type EventSink = Arc<dyn Fn(SessionId, Event) + Send + Sync>;

#[derive(Clone)]
pub struct Proxy {
    id: SessionId,
    sink: EventSink,
}

impl EventListener for Proxy {
    fn send_event(&self, event: Event) {
        (self.sink)(self.id, event);
    }
}

/// A sink that drops everything.
pub fn null_sink() -> EventSink {
    Arc::new(|_, _| {})
}

/// What a session runs. An empty `argv` means the shell.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpawnRequest {
    pub argv: Vec<OsString>,
    pub cwd: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridSize {
    pub cols: usize,
    pub rows: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// The program to run: a `-e` command line as given; otherwise the
/// configured shell split on spaces; otherwise the login shell from passwd.
pub fn command_line(request: &SpawnRequest, shell_program: &str) -> Option<(String, Vec<String>)> {
    let parts: Vec<String> = if !request.argv.is_empty() {
        request
            .argv
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    } else if !shell_program.trim().is_empty() {
        shell_program
            .split_whitespace()
            .map(str::to_owned)
            .collect()
    } else {
        vec![login_shell()?, "--login".to_owned()]
    };
    let mut it = parts.into_iter();
    Some((it.next()?, it.collect()))
}

fn login_shell() -> Option<String> {
    // SAFETY: getpwuid returns a pointer into static storage or null; we copy
    // the string out immediately and touch nothing else.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() || (*pw).pw_shell.is_null() {
            return std::env::var("SHELL").ok();
        }
        let shell = std::ffi::CStr::from_ptr((*pw).pw_shell)
            .to_string_lossy()
            .into_owned();
        (!shell.is_empty()).then_some(shell)
    }
}

pub struct Session {
    id: SessionId,
    sink: EventSink,
    term: Arc<FairMutex<Term<Proxy>>>,
    pty: Option<Notifier>,
    request: SpawnRequest,
    started: bool,
    /// For headless sessions, which parse their input here.
    parser: Processor,
    size: GridSize,
    cell_px: (u16, u16),
    title: String,
    custom_title: String,
    mouse: MouseReporter,
}

impl Session {
    pub fn new(id: SessionId, sink: EventSink, request: SpawnRequest) -> Self {
        let size = GridSize { cols: 80, rows: 24 };
        let config = term::Config {
            scrolling_history: SCROLLBACK_LINES,
            ..Default::default()
        };
        let term = Term::new(
            config,
            &size,
            Proxy {
                id,
                sink: sink.clone(),
            },
        );
        Self {
            id,
            sink,
            term: Arc::new(FairMutex::new(term)),
            pty: None,
            request,
            started: false,
            parser: Processor::new(),
            size,
            cell_px: (8, 16),
            title: String::new(),
            custom_title: String::new(),
            mouse: MouseReporter::new(),
        }
    }

    pub fn id(&self) -> SessionId {
        self.id
    }

    pub fn started(&self) -> bool {
        self.started
    }

    pub fn size(&self) -> GridSize {
        self.size
    }

    /// Replaces what a not-yet-started session will run. Returns false once
    /// it has started.
    pub fn set_request(&mut self, request: SpawnRequest) -> bool {
        if self.started {
            return false;
        }
        self.request = request;
        true
    }

    pub fn term(&self) -> &Arc<FairMutex<Term<Proxy>>> {
        &self.term
    }

    fn window_size(&self) -> WindowSize {
        WindowSize {
            num_lines: self.size.rows as u16,
            num_cols: self.size.cols as u16,
            cell_width: self.cell_px.0,
            cell_height: self.cell_px.1,
        }
    }

    /// Starts the program at `size`. A program that cannot be started is
    /// reported like one that exited at once, which is what the C++ version's
    /// fork-then-exec looked like from outside.
    pub fn start(&mut self, size: GridSize, cell_px: (u16, u16), shell_program: &str) {
        if self.started {
            return;
        }
        self.started = true;
        self.resize(size, cell_px);

        let Some((program, args)) = command_line(&self.request, shell_program) else {
            log::warn!("cannot determine the user's shell");
            self.report_exit();
            return;
        };
        let home = crate::paths::home_dir();
        // Start where `-e` was run from, else in the home directory.
        let cwd = self
            .request
            .cwd
            .clone()
            .filter(|d| d.is_dir())
            .unwrap_or(home);
        let options = tty::Options {
            shell: Some(tty::Shell::new(program.clone(), args)),
            working_directory: Some(cwd),
            drain_on_exit: true,
            env: [
                ("TERM".to_owned(), "xterm-256color".to_owned()),
                ("COLORTERM".to_owned(), "truecolor".to_owned()),
            ]
            .into_iter()
            .collect(),
        };
        let pty = match tty::new(&options, self.window_size(), self.id) {
            Ok(pty) => pty,
            Err(err) => {
                log::warn!("cannot start {program}: {err}");
                self.report_exit();
                return;
            }
        };
        let proxy = Proxy {
            id: self.id,
            sink: self.sink.clone(),
        };
        match EventLoop::new(self.term.clone(), proxy, pty, true, false) {
            Ok(event_loop) => {
                self.pty = Some(Notifier(event_loop.channel()));
                event_loop.spawn();
            }
            Err(err) => {
                log::warn!("cannot start pty reader: {err}");
                self.report_exit();
            }
        }
    }

    /// Starts without a program: input comes from [`Session::feed`].
    pub fn start_headless(&mut self, size: GridSize) {
        self.started = true;
        self.resize(size, self.cell_px);
    }

    fn report_exit(&self) {
        (self.sink)(
            self.id,
            Event::ChildExit(std::process::ExitStatus::from_raw(127 << 8)),
        );
    }

    /// Feeds program output directly (headless sessions and tests).
    pub fn feed(&mut self, bytes: &[u8]) {
        let mut term = self.term.lock();
        self.parser.advance(&mut *term, bytes);
    }

    pub fn resize(&mut self, size: GridSize, cell_px: (u16, u16)) {
        let size = GridSize {
            cols: size.cols.max(1),
            rows: size.rows.max(1),
        };
        if size == self.size && cell_px == self.cell_px {
            return;
        }
        self.size = size;
        self.cell_px = cell_px;
        self.term.lock().resize(size);
        let ws = self.window_size();
        if let Some(pty) = &mut self.pty {
            pty.on_resize(ws);
        }
    }

    /// Writes bytes to the program.
    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let bytes = bytes.into();
        if bytes.is_empty() {
            return;
        }
        if let Some(pty) = &self.pty {
            pty.notify(bytes);
        }
    }

    pub fn mode(&self) -> TermMode {
        *self.term.lock().mode()
    }

    pub fn alt_screen(&self) -> bool {
        self.mode().contains(TermMode::ALT_SCREEN)
    }

    pub fn mouse_mode(&self) -> MouseMode {
        let m = self.mode();
        MouseMode {
            enabled: m.intersects(TermMode::MOUSE_MODE),
            drag: m.contains(TermMode::MOUSE_DRAG),
            motion: m.contains(TermMode::MOUSE_MOTION),
            sgr: m.contains(TermMode::SGR_MOUSE),
            utf8: m.contains(TermMode::UTF8_MOUSE),
        }
    }

    pub fn mouse_tracking(&self) -> bool {
        self.mouse_mode().enabled
    }

    // ── Input ───────────────────────────────────────────────────────

    /// A key for the program. Any real key returns the view to the bottom
    /// and drops the selection.
    pub fn key(&mut self, press: &KeyPress) {
        if press.key == keys::Key::Modifier {
            return;
        }
        let mode = self.mode();
        let modes = KeyModes {
            app_cursor: mode.contains(TermMode::APP_CURSOR),
            newline: mode.contains(TermMode::LINE_FEED_NEW_LINE),
        };
        self.scroll_to_bottom();
        self.clear_selection();
        if let Some(s) = keys::encode(press, modes) {
            self.write(s.into_bytes());
        }
    }

    pub fn paste(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.scroll_to_bottom();
        self.clear_selection();
        if self.mode().contains(TermMode::BRACKETED_PASTE) {
            // Strip any end marker the text carries, so it cannot end the
            // paste early and have the rest run as typed commands.
            let clean = text.replace("\x1b[201~", "");
            self.write(format!("\x1b[200~{clean}\x1b[201~").into_bytes());
        } else {
            self.write(text.as_bytes().to_vec());
        }
    }

    /// The pointer is over `col`, `row` (0-based). Reports motion to a
    /// program whose mode asks for it.
    pub fn mouse_move(&mut self, col: usize, row: usize, mods: keys::Mods) {
        let mode = self.mouse_mode();
        if let Some(r) = self.mouse.motion(col as i32, row as i32, mods, mode) {
            self.write(mouse::to_bytes(&r, mode));
        }
    }

    pub fn mouse_button(
        &mut self,
        button: Button,
        pressed: bool,
        col: usize,
        row: usize,
        mods: keys::Mods,
    ) {
        let mode = self.mouse_mode();
        let mut out = Vec::new();
        if let Some(r) = self.mouse.motion(col as i32, row as i32, mods, mode) {
            out.extend(mouse::to_bytes(&r, mode));
        }
        if let Some(r) = self.mouse.button(button, pressed, mods, mode) {
            out.extend(mouse::to_bytes(&r, mode));
        }
        self.write(out);
    }

    /// The wheel on the alternate screen, which has no scrollback: a mouse
    /// report when the program tracks the mouse, otherwise cursor keys, like
    /// xterm's alternateScroll. `steps > 0` is up.
    pub fn wheel(&mut self, steps: i32, col: usize, row: usize, mods: keys::Mods) {
        if steps == 0 {
            return;
        }
        if self.mouse_tracking() {
            for _ in 0..steps.unsigned_abs() {
                let b = if steps > 0 {
                    Button::WheelUp
                } else {
                    Button::WheelDown
                };
                self.mouse_button(b, true, col, row, mods);
            }
        } else {
            let key = if steps > 0 {
                keys::Key::Up
            } else {
                keys::Key::Down
            };
            let mode = self.mode();
            let modes = KeyModes {
                app_cursor: mode.contains(TermMode::APP_CURSOR),
                newline: false,
            };
            let seq = keys::encode_key(key, mods, modes).unwrap_or_default();
            self.write(seq.repeat(steps.unsigned_abs() as usize).into_bytes());
        }
    }

    // ── Scrollback ──────────────────────────────────────────────────

    pub fn history_size(&self) -> usize {
        self.term.lock().grid().history_size()
    }

    /// Lines scrolled back from the bottom.
    pub fn display_offset(&self) -> usize {
        self.term.lock().grid().display_offset()
    }

    /// Positive scrolls back into history. Ignored on the alternate screen.
    pub fn scroll_lines(&mut self, lines: i32) {
        if lines == 0 || self.alt_screen() {
            return;
        }
        self.term.lock().scroll_display(Scroll::Delta(lines));
    }

    pub fn scroll_to_bottom(&mut self) {
        self.term.lock().scroll_display(Scroll::Bottom);
    }

    /// Sets how far back the view is, clamped to the history.
    pub fn set_display_offset(&mut self, offset: usize) {
        if self.alt_screen() {
            return;
        }
        let mut term = self.term.lock();
        let current = term.grid().display_offset() as i32;
        let target = offset.min(term.grid().history_size()) as i32;
        term.scroll_display(Scroll::Delta(target - current));
    }

    // ── Selection ───────────────────────────────────────────────────

    /// A grid point from a viewport cell (0-based), honouring scrollback.
    pub fn viewport_point(&self, col: usize, row: usize) -> Point {
        let offset = self.display_offset();
        let col = col.min(self.size.cols.saturating_sub(1));
        let row = row.min(self.size.rows.saturating_sub(1));
        term::viewport_to_point(offset, Point::new(row, Column(col)))
    }

    pub fn start_selection(&mut self, point: Point, side: Side) {
        self.term.lock().selection = Some(Selection::new(SelectionType::Simple, point, side));
    }

    pub fn update_selection(&mut self, point: Point, side: Side) {
        if let Some(sel) = self.term.lock().selection.as_mut() {
            sel.update(point, side);
        }
    }

    pub fn clear_selection(&mut self) {
        self.term.lock().selection = None;
    }

    pub fn selected_text(&self) -> String {
        self.term.lock().selection_to_string().unwrap_or_default()
    }

    /// The topmost line of history, for clamping a drag that leaves the top.
    pub fn top_line(&self) -> Line {
        self.term.lock().grid().topmost_line()
    }

    // ── Title ───────────────────────────────────────────────────────

    pub fn set_title(&mut self, title: String) {
        self.title = title;
    }
}

impl TabSession for Session {
    fn title(&self) -> &str {
        &self.title
    }
    fn custom_title(&self) -> &str {
        &self.custom_title
    }
    fn set_custom_title(&mut self, title: String) {
        self.custom_title = title;
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // The reader thread owns the pty; shutting it down drops the pty,
        // which hangs the program up.
        if let Some(pty) = &self.pty {
            let _ = pty.0.send(Msg::Shutdown);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headless(cols: usize, rows: usize) -> Session {
        let mut s = Session::new(1, null_sink(), SpawnRequest::default());
        s.start_headless(GridSize { cols, rows });
        s
    }

    #[test]
    fn command_line_choices() {
        let req = SpawnRequest {
            argv: vec!["htop".into(), "-d".into(), "5".into()],
            cwd: None,
        };
        assert_eq!(
            command_line(&req, "zsh"),
            Some(("htop".into(), vec!["-d".into(), "5".into()]))
        );
        let req = SpawnRequest::default();
        assert_eq!(
            command_line(&req, " fish  -l "),
            Some(("fish".into(), vec!["-l".into()]))
        );
        let (_, args) = command_line(&req, "").unwrap();
        assert_eq!(args, vec!["--login".to_owned()]);
    }

    #[test]
    fn request_can_change_until_started() {
        let mut s = Session::new(1, null_sink(), SpawnRequest::default());
        assert!(s.set_request(SpawnRequest {
            argv: vec!["top".into()],
            cwd: None
        }));
        s.start_headless(GridSize { cols: 10, rows: 2 });
        assert!(!s.set_request(SpawnRequest::default()));
    }

    #[test]
    fn scrollback_and_selection() {
        let mut s = headless(10, 3);
        for i in 0..10 {
            s.feed(format!("line{i}\r\n").as_bytes());
        }
        assert_eq!(s.history_size(), 8);
        s.scroll_lines(3);
        assert_eq!(s.display_offset(), 3);
        s.scroll_lines(100);
        assert_eq!(s.display_offset(), 8);
        s.set_display_offset(2);
        assert_eq!(s.display_offset(), 2);

        // Visible: line6, line7, line8. Select the top row.
        let start = s.viewport_point(0, 0);
        let end = s.viewport_point(9, 0);
        s.start_selection(start, Side::Left);
        s.update_selection(end, Side::Right);
        assert_eq!(s.selected_text(), "line6");

        s.scroll_to_bottom();
        assert_eq!(s.display_offset(), 0);
        s.clear_selection();
        assert_eq!(s.selected_text(), "");
    }

    #[test]
    fn alt_screen_has_no_scrollback() {
        let mut s = headless(10, 3);
        for _ in 0..10 {
            s.feed(b"x\r\n");
        }
        s.feed(b"\x1b[?1049h");
        assert!(s.alt_screen());
        s.scroll_lines(2);
        assert_eq!(s.display_offset(), 0);
    }

    #[test]
    fn mouse_modes_follow_the_program() {
        let mut s = headless(10, 3);
        assert!(!s.mouse_tracking());
        s.feed(b"\x1b[?1002h\x1b[?1006h");
        let m = s.mouse_mode();
        assert!(m.enabled && m.drag && m.sgr && !m.motion);
        s.feed(b"\x1b[?1002l");
        assert!(!s.mouse_tracking());
    }

    #[test]
    fn titles_arrive_as_events() {
        use std::sync::Mutex;
        let got = Arc::new(Mutex::new(Vec::new()));
        let sink: EventSink = {
            let got = got.clone();
            Arc::new(move |id, e| {
                if let Event::Title(t) = e {
                    got.lock().unwrap().push((id, t));
                }
            })
        };
        let mut s = Session::new(7, sink, SpawnRequest::default());
        s.start_headless(GridSize { cols: 10, rows: 2 });
        s.feed(b"\x1b]0;hello\x07");
        assert_eq!(*got.lock().unwrap(), vec![(7, "hello".to_owned())]);
    }

    #[test]
    fn a_real_program_runs_and_exits() {
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let sink: EventSink = Arc::new(move |_, e| {
            let _ = tx.lock().unwrap().send(e);
        });
        let req = SpawnRequest {
            argv: vec!["/bin/sh".into(), "-c".into(), "printf 'hi there'".into()],
            cwd: None,
        };
        let mut s = Session::new(3, sink, req);
        s.start(GridSize { cols: 20, rows: 4 }, (8, 16), "");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut exited = false;
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(Event::ChildExit(_)) => {
                    exited = true;
                    break;
                }
                _ => continue,
            }
        }
        assert!(exited, "child exit was reported");
        // The exit is reported before the reader drains the last output, and
        // a Wakeup follows once it has.
        let first_line = || {
            let term = s.term().lock();
            (0..8)
                .map(|c| term.grid()[Line(0)][Column(c)].c)
                .collect::<String>()
        };
        while first_line() != "hi there" && std::time::Instant::now() < deadline {
            let _ = rx.recv_timeout(std::time::Duration::from_millis(50));
        }
        assert_eq!(first_line(), "hi there");
    }

    #[test]
    fn a_missing_program_reports_an_exit() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let exited = Arc::new(AtomicBool::new(false));
        let sink: EventSink = {
            let exited = exited.clone();
            Arc::new(move |_, e| {
                if matches!(e, Event::ChildExit(_)) {
                    exited.store(true, Ordering::SeqCst);
                }
            })
        };
        let req = SpawnRequest {
            argv: vec!["/definitely/not/here".into()],
            cwd: None,
        };
        let mut s = Session::new(4, sink, req);
        s.start(GridSize { cols: 20, rows: 4 }, (8, 16), "");
        assert!(exited.load(Ordering::SeqCst));
    }
}
