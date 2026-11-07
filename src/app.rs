//! The Wayland frontend: owns the layer-shell surface and feeds the panel.
//!
//! Two details of the surface are load-bearing:
//!
//! - **It spans the whole usable output**, not just the terminal. A surface
//!   sized to the terminal alone is never told about clicks elsewhere, so
//!   outside-click dismissal would be impossible; the transparent rest of the
//!   surface catches them. Anchoring all four edges with an exclusive zone of
//!   0 makes the compositor size it to the output *minus* everyone else's
//!   exclusive zones, so the terminal sits flush under a bar with no
//!   configured offset, and — since a surface is clipped to itself — the
//!   panel appears to roll out from under the bar.
//! - **Keyboard interactivity is exclusive.** Under focus-follows-mouse an
//!   on-demand surface loses the keyboard the moment the pointer drifts off
//!   it. Compositor keybinds still take priority, so the toggle works.
//!
//! Hiding destroys the layer surface; showing creates a new one, which the
//! compositor places on the focused output.

use std::io::Read;
use std::os::fd::AsFd;
use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use alacritty_terminal::event::Event as TermEvent;
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState, FrameCallbackData};
use smithay_client_toolkit::data_device_manager::data_device::{DataDevice, DataDeviceHandler};
use smithay_client_toolkit::data_device_manager::data_offer::{DataOfferHandler, DragOffer};
use smithay_client_toolkit::data_device_manager::data_source::{
    CopyPasteSource, DataSourceHandler,
};
use smithay_client_toolkit::data_device_manager::{DataDeviceManagerState, WritePipe};
use smithay_client_toolkit::dispatch2::Dispatch2;
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop::channel::{self, Channel};
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{
    EventLoop, Interest, LoopHandle, Mode, PostAction,
};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::globals::{registry_queue_init, GlobalList};
use smithay_client_toolkit::reexports::client::protocol::wl_data_device_manager::DndAction;
use smithay_client_toolkit::reexports::client::protocol::{
    wl_data_device::WlDataDevice, wl_data_source::WlDataSource, wl_keyboard, wl_output, wl_pointer,
    wl_seat, wl_shm, wl_surface,
};
use smithay_client_toolkit::reexports::client::{Connection, Proxy, QueueHandle};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::registry_handlers;
use smithay_client_toolkit::seat::keyboard::{
    KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers,
};
use smithay_client_toolkit::seat::pointer::{
    CursorIcon, PointerEvent, PointerEventKind, PointerHandler, ThemeSpec, ThemedPointer,
};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use tiny_skia::Pixmap;
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};

use wl_dropdown::cli::{self, Command, Request};
use wl_dropdown::fonts::FontLibrary;
use wl_dropdown::ipc;
use wl_dropdown::keys::{Key, KeyPress, Mods};
use wl_dropdown::panel::{wheel_delta, CursorShape, Effect, Panel, PointerButton, Spawner};
use wl_dropdown::paths::{self, APP_NAME};
use wl_dropdown::render::{rgba_to_argb8888, Canvas};
use wl_dropdown::session::{EventSink, SessionId};
use wl_dropdown::settings::Settings;

type SessionMsg = (SessionId, TermEvent);

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
const TEXT_MIMES: [&str; 4] = [
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "STRING",
];

/// The surface while the dropdown is mapped.
struct Mapped {
    layer: LayerSurface,
    viewport: Option<WpViewport>,
    fractional: Option<WpFractionalScaleV1>,
    configured: bool,
    size: (u32, u32),
    frame_pending: bool,
}

struct App {
    conn: Connection,
    qh: QueueHandle<App>,
    loop_handle: LoopHandle<'static, App>,
    registry: RegistryState,
    seats: SeatState,
    outputs: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    pool: Option<SlotPool>,
    viewporter: Option<WpViewporter>,
    fractional_manager: Option<WpFractionalScaleManagerV1>,
    data_devices: DataDeviceManagerState,

    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<ThemedPointer>,
    data_device: Option<DataDevice>,
    modifiers: Modifiers,
    last_serial: u32,
    cursor: Option<CursorShape>,
    copy_source: Option<(CopyPasteSource, String)>,

    surface: Option<Mapped>,
    /// Integer scale from the compositor, used without fractional scaling.
    int_scale: i32,
    /// Preferred scale ×120, from `wp_fractional_scale_v1`.
    fractional_scale: Option<u32>,

    settings: Settings,
    panel: Panel,
    exit: bool,
}

/// User data for protocol objects with no events worth handling.
struct NoEvents;
/// User data for the fractional scale object.
struct FractionalScale;

impl Dispatch2<WpViewporter, App> for NoEvents {
    fn event(
        &self,
        _: &mut App,
        _: &WpViewporter,
        _: <WpViewporter as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}
impl Dispatch2<WpViewport, App> for NoEvents {
    fn event(
        &self,
        _: &mut App,
        _: &WpViewport,
        _: <WpViewport as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}
impl Dispatch2<WpFractionalScaleManagerV1, App> for NoEvents {
    fn event(
        &self,
        _: &mut App,
        _: &WpFractionalScaleManagerV1,
        _: <WpFractionalScaleManagerV1 as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}
impl Dispatch2<WpFractionalScaleV1, App> for FractionalScale {
    fn event(
        &self,
        app: &mut App,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            if app.fractional_scale != Some(scale) {
                app.fractional_scale = Some(scale);
                app.panel.mark_dirty();
            }
        }
    }
}

fn bind_optional<I: Proxy + 'static, U: Dispatch2<I, App> + Send + Sync + 'static>(
    globals: &GlobalList,
    qh: &QueueHandle<App>,
    data: U,
) -> Option<I> {
    globals.bind::<I, App, U>(qh, 1..=1, data).ok()
}

/// Runs the terminal: the process that owns the window. `request` is what
/// the invocation that started it asked for.
pub fn run(request: &[u8]) -> Result<(), String> {
    let conn =
        Connection::connect_to_env().map_err(|e| format!("cannot connect to Wayland: {e}"))?;
    let (globals, queue) =
        registry_queue_init::<App>(&conn).map_err(|e| format!("Wayland registry: {e}"))?;
    let qh = queue.handle();
    let mut event_loop: EventLoop<App> = EventLoop::try_new().map_err(|e| e.to_string())?;
    let loop_handle = event_loop.handle();
    WaylandSource::new(conn.clone(), queue)
        .insert(loop_handle.clone())
        .map_err(|e| e.to_string())?;

    let compositor =
        CompositorState::bind(&globals, &qh).map_err(|_| "wl_compositor is not available")?;
    let layer_shell = LayerShell::bind(&globals, &qh)
        .map_err(|_| "the compositor does not support wlr-layer-shell, which wl-dropdown needs")?;
    let shm = Shm::bind(&globals, &qh).map_err(|_| "wl_shm is not available")?;
    let data_devices = DataDeviceManagerState::bind(&globals, &qh)
        .map_err(|_| "wl_data_device_manager is not available")?;

    // Session events arrive from reader threads.
    let (tx, rx): (channel::Sender<SessionMsg>, Channel<SessionMsg>) = channel::channel();
    let tx = Mutex::new(tx);
    let sink: EventSink = Arc::new(move |id, e| {
        let _ = tx.lock().unwrap().send((id, e));
    });
    event_loop
        .handle()
        .insert_source(rx, |event, (), app: &mut App| {
            if let channel::Event::Msg((id, e)) = event {
                let now = Instant::now();
                app.panel.handle_session_event(id, e, now);
            }
        })
        .map_err(|e| e.to_string())?;

    // The control socket. Safe to replace: we only get here after failing to
    // reach a live instance.
    let listener: UnixListener = ipc::listen(&paths::socket_path())
        .map_err(|e| format!("cannot listen on {}: {e}", paths::socket_path().display()))?;
    event_loop
        .handle()
        .insert_source(
            Generic::new(listener, Interest::READ, Mode::Level),
            |_, listener, app: &mut App| {
                for req in ipc::drain(listener) {
                    app.handle_request(&req);
                }
                Ok(PostAction::Continue)
            },
        )
        .map_err(|e| e.to_string())?;

    let settings = Settings::load();
    let now = Instant::now();
    let library = Arc::new(FontLibrary::system());
    let panel = Panel::new(settings.values().clone(), library, Spawner::Pty, sink, now);

    let mut app = App {
        conn: conn.clone(),
        qh: qh.clone(),
        loop_handle: loop_handle.clone(),
        registry: RegistryState::new(&globals),
        seats: SeatState::new(&globals, &qh),
        outputs: OutputState::new(&globals, &qh),
        viewporter: bind_optional(&globals, &qh, NoEvents),
        fractional_manager: bind_optional(&globals, &qh, NoEvents),
        compositor,
        layer_shell,
        shm,
        pool: None,
        data_devices,
        keyboard: None,
        pointer: None,
        data_device: None,
        modifiers: Modifiers::default(),
        last_serial: 0,
        cursor: None,
        copy_source: None,
        surface: None,
        int_scale: 1,
        fractional_scale: None,
        settings,
        panel,
        exit: false,
    };

    // Whatever started us: show, toggle (which shows) or exec.
    app.handle_request(request);

    while !app.exit {
        let now = Instant::now();
        let timeout = app
            .panel
            .next_deadline()
            .map(|d| d.saturating_duration_since(now));
        event_loop
            .dispatch(timeout, &mut app)
            .map_err(|e| e.to_string())?;
        app.after_dispatch();
    }
    let _ = std::fs::remove_file(paths::socket_path());
    Ok(())
}

fn keysym_to_key(sym: Keysym) -> Key {
    match sym {
        Keysym::Return | Keysym::KP_Enter => Key::Enter,
        Keysym::Tab | Keysym::KP_Tab => Key::Tab,
        Keysym::ISO_Left_Tab => Key::Backtab,
        Keysym::BackSpace => Key::Backspace,
        Keysym::Escape => Key::Escape,
        Keysym::Up | Keysym::KP_Up => Key::Up,
        Keysym::Down | Keysym::KP_Down => Key::Down,
        Keysym::Left | Keysym::KP_Left => Key::Left,
        Keysym::Right | Keysym::KP_Right => Key::Right,
        Keysym::Insert | Keysym::KP_Insert => Key::Insert,
        Keysym::Delete | Keysym::KP_Delete => Key::Delete,
        Keysym::Home | Keysym::KP_Home => Key::Home,
        Keysym::End | Keysym::KP_End => Key::End,
        Keysym::Page_Up | Keysym::KP_Page_Up => Key::PageUp,
        Keysym::Page_Down | Keysym::KP_Page_Down => Key::PageDown,
        Keysym::Shift_L
        | Keysym::Shift_R
        | Keysym::Control_L
        | Keysym::Control_R
        | Keysym::Alt_L
        | Keysym::Alt_R
        | Keysym::Meta_L
        | Keysym::Meta_R
        | Keysym::Super_L
        | Keysym::Super_R
        | Keysym::Hyper_L
        | Keysym::Hyper_R
        | Keysym::ISO_Level3_Shift
        | Keysym::ISO_Level5_Shift
        | Keysym::Caps_Lock
        | Keysym::Num_Lock => Key::Modifier,
        _ => {
            let raw = sym.raw();
            if (Keysym::F1.raw()..=Keysym::F35.raw()).contains(&raw) {
                return Key::F((raw - Keysym::F1.raw() + 1) as u8);
            }
            match sym.key_char() {
                Some(c) if !c.is_control() => Key::Char(c.to_lowercase().next().unwrap_or(c)),
                _ => Key::Other,
            }
        }
    }
}

impl App {
    fn mods(&self) -> Mods {
        Mods {
            shift: self.modifiers.shift,
            ctrl: self.modifiers.ctrl,
            alt: self.modifiers.alt,
            meta: self.modifiers.logo,
        }
    }

    fn scale(&self) -> f64 {
        match (self.fractional_scale, &self.viewporter) {
            (Some(s), Some(_)) => f64::from(s) / 120.0,
            _ => f64::from(self.int_scale),
        }
    }

    fn handle_request(&mut self, bytes: &[u8]) {
        let now = Instant::now();
        match cli::decode_request(bytes) {
            Request::Exec { cwd, argv } => {
                let cwd = (!cwd.is_empty()).then(|| cwd.into());
                self.panel.execute(argv, cwd, now);
            }
            Request::Command(Command::Reload) => {
                // Sent by the settings window after it writes, and by theme
                // engines after they render theme.conf.
                self.settings.reload();
                self.panel
                    .apply_settings(self.settings.values().clone(), now);
            }
            Request::Command(Command::Show) => self.panel.show(now),
            Request::Command(Command::Hide) => self.panel.hide(now),
            Request::Command(Command::Toggle) => self.panel.toggle(now),
            Request::Command(Command::Settings) => self.panel.open_settings(now),
            Request::Unknown => log::warn!(
                "ignoring unknown request {:?}",
                String::from_utf8_lossy(bytes)
            ),
        }
    }

    fn after_dispatch(&mut self) {
        let now = Instant::now();
        self.panel.tick(now);
        for effect in self.panel.take_effects() {
            match effect {
                Effect::Copy(text) => self.copy(text),
                Effect::Paste => self.paste(),
                Effect::OpenSettings => spawn_settings(),
            }
        }
        // Map or unmap to match the controller.
        match (self.panel.controller().mapped(), self.surface.is_some()) {
            (true, false) => self.map(),
            (false, true) => self.unmap(),
            _ => {}
        }
        self.update_cursor(now);
        self.maybe_draw(now);
    }

    fn map(&mut self) {
        let surface = self.compositor.create_surface(&self.qh);
        let viewport = self
            .viewporter
            .as_ref()
            .map(|v| v.get_viewport(&surface, &self.qh, NoEvents));
        let fractional = self
            .fractional_manager
            .as_ref()
            .map(|m| m.get_fractional_scale(&surface, &self.qh, FractionalScale));
        // No output: the compositor picks the focused one.
        let layer = self.layer_shell.create_layer_surface(
            &self.qh,
            surface,
            Layer::Overlay,
            Some(APP_NAME),
            None,
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        layer.set_size(0, 0);
        layer.commit();
        self.surface = Some(Mapped {
            layer,
            viewport,
            fractional,
            configured: false,
            size: (0, 0),
            frame_pending: false,
        });
    }

    fn unmap(&mut self) {
        if let Some(m) = self.surface.take() {
            if let Some(v) = m.viewport {
                v.destroy();
            }
            if let Some(f) = m.fractional {
                f.destroy();
            }
            drop(m.layer);
        }
        self.fractional_scale = None;
        self.cursor = None;
        self.panel.pointer_leave();
    }

    fn update_cursor(&mut self, now: Instant) {
        let shape = self.panel.cursor_shape(now);
        if self.cursor == Some(shape) {
            return;
        }
        let Some(pointer) = &self.pointer else { return };
        let icon = match shape {
            CursorShape::Default => CursorIcon::Default,
            CursorShape::Text => CursorIcon::Text,
            CursorShape::EwResize => CursorIcon::EwResize,
            CursorShape::NsResize => CursorIcon::NsResize,
            CursorShape::NeswResize => CursorIcon::NeswResize,
            CursorShape::NwseResize => CursorIcon::NwseResize,
        };
        if pointer.set_cursor(&self.conn, icon).is_ok() {
            self.cursor = Some(shape);
        }
    }

    fn maybe_draw(&mut self, now: Instant) {
        let Some(m) = &self.surface else { return };
        if !m.configured || m.frame_pending {
            return;
        }
        if self.panel.dirty() || self.panel.animating(now) {
            self.draw(now);
        }
    }

    fn draw(&mut self, now: Instant) {
        let scale = self.scale();
        let Some(m) = self.surface.as_mut() else {
            return;
        };
        let (lw, lh) = m.size;
        if lw == 0 || lh == 0 {
            return;
        }
        let bw = (f64::from(lw) * scale).round() as u32;
        let bh = (f64::from(lh) * scale).round() as u32;
        let stride = bw as i32 * 4;
        let pool = match &mut self.pool {
            Some(p) => p,
            None => match SlotPool::new((stride * bh as i32) as usize, &self.shm) {
                Ok(p) => self.pool.insert(p),
                Err(e) => {
                    log::error!("cannot allocate a shared-memory pool: {e}");
                    return;
                }
            },
        };
        let Ok((buffer, canvas)) =
            pool.create_buffer(bw as i32, bh as i32, stride, wl_shm::Format::Argb8888)
        else {
            log::error!("cannot allocate a {bw}x{bh} buffer");
            return;
        };

        // Only the panel has content; render just its bounding box and copy
        // it into an otherwise transparent buffer.
        canvas.fill(0);
        let layout = self.panel.layout(now);
        let p = layout.panel;
        let x0 = ((p.x * scale).floor().max(0.0)) as u32;
        let y0 = ((p.y * scale).floor().max(0.0)) as u32;
        let x1 = ((p.right() * scale).ceil() as u32).min(bw);
        let y1 = ((p.bottom() * scale).ceil().max(0.0) as u32).min(bh);
        if x1 > x0 && y1 > y0 {
            if let Some(mut pixmap) = Pixmap::new(x1 - x0, y1 - y0) {
                {
                    let mut c = Canvas::with_origin(
                        &mut pixmap,
                        scale,
                        (-f64::from(x0) / scale, -f64::from(y0) / scale),
                    );
                    self.panel.render(&mut c, now);
                }
                let row = ((x1 - x0) * 4) as usize;
                for (i, src) in pixmap.data().chunks_exact(row).enumerate() {
                    let off = ((y0 as usize + i) * bw as usize + x0 as usize) * 4;
                    rgba_to_argb8888(src, &mut canvas[off..off + row]);
                }
            }
        } else {
            self.panel.render_nothing();
        }

        let surface = m.layer.wl_surface();
        if let Some(v) = &m.viewport {
            v.set_destination(lw as i32, lh as i32);
            surface.set_buffer_scale(1);
        } else {
            surface.set_buffer_scale(self.int_scale);
        }
        surface.damage_buffer(0, 0, bw as i32, bh as i32);
        surface.frame(&self.qh, FrameCallbackData(surface.clone()));
        m.frame_pending = true;
        if buffer.attach_to(surface).is_err() {
            log::error!("cannot attach buffer");
        }
        m.layer.commit();
    }

    fn copy(&mut self, text: String) {
        let Some(device) = &self.data_device else {
            return;
        };
        let source = self
            .data_devices
            .create_copy_paste_source(&self.qh, TEXT_MIMES);
        source.set_selection(device, self.last_serial);
        self.copy_source = Some((source, text));
    }

    fn paste(&mut self) {
        // Our own selection needs no round trip through the compositor.
        if let Some((_, text)) = &self.copy_source {
            let text = text.clone();
            self.panel.paste(&text);
            return;
        }
        let Some(offer) = self
            .data_device
            .as_ref()
            .and_then(|d| d.data().selection_offer())
        else {
            return;
        };
        let Some(mime) = offer.with_mime_types(|m| {
            TEXT_MIMES
                .iter()
                .find(|t| m.iter().any(|x| x == *t))
                .map(std::string::ToString::to_string)
        }) else {
            return;
        };
        let pipe = match offer.receive(mime) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("cannot read the clipboard: {e}");
                return;
            }
        };
        // Read on a thread: the source may be slow, and must never stall the
        // terminal.
        let Ok(fd) = pipe.as_fd().try_clone_to_owned() else {
            return;
        };
        drop(pipe);
        let (tx, rx) = channel::channel::<String>();
        let _ = self
            .loop_handle
            .insert_source(rx, |event, (), app: &mut App| {
                if let channel::Event::Msg(text) = event {
                    app.panel.paste(&text);
                }
            });
        std::thread::spawn(move || {
            let mut file = std::fs::File::from(fd);
            let mut buf = Vec::new();
            if file.read_to_end(&mut buf).is_ok() {
                let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
            }
        });
        let _ = self.conn.flush();
    }
}

/// `wl-dropdown settings` as a separate process: `LayerShell` surfaces and an
/// ordinary decorated window do not belong in one client, and the dropdown
/// would cover the window anyway.
fn spawn_settings() {
    let exe = std::env::current_exe().unwrap_or_else(|_| APP_NAME.into());
    match std::process::Command::new(exe).arg("settings").spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => log::error!("cannot start the settings window: {e}"),
    }
}

// ── Handlers ────────────────────────────────────────────────────────

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        factor: i32,
    ) {
        if self.int_scale != factor {
            self.int_scale = factor;
            self.panel.mark_dirty();
        }
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        if let Some(m) = &mut self.surface {
            m.frame_pending = false;
        }
    }
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for App {
    /// The compositor took the surface away (an output vanished, a config
    /// reload). The daemon stays: the next toggle makes a new one.
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.surface = None;
        self.panel.surface_lost(Instant::now());
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let now = Instant::now();
        let Some(m) = &mut self.surface else { return };
        let (w, h) = configure.new_size;
        m.size = (w.max(1), h.max(1));
        m.configured = true;
        m.frame_pending = false;
        self.panel
            .set_surface_size(f64::from(m.size.0), f64::from(m.size.1), now);
        self.panel.mark_dirty();
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }
    fn new_seat(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        if self.data_device.is_none() {
            self.data_device = Some(self.data_devices.get_data_device(qh, &seat));
        }
    }
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            let repeat = Box::new(
                |app: &mut App, _: &wl_keyboard::WlKeyboard, event: KeyEvent| app.key(event),
            );
            match self.seats.get_keyboard_with_repeat(
                qh,
                &seat,
                None,
                self.loop_handle.clone(),
                repeat,
            ) {
                Ok(k) => self.keyboard = Some(k),
                Err(e) => log::warn!("no keyboard: {e}"),
            }
        }
        if capability == Capability::Pointer && self.pointer.is_none() {
            let surface = self.compositor.create_surface(qh);
            match self.seats.get_pointer_with_theme::<App, ()>(
                qh,
                &seat,
                self.shm.wl_shm(),
                surface,
                ThemeSpec::default(),
            ) {
                Ok(p) => self.pointer = Some(p),
                Err(e) => log::warn!("no pointer: {e}"),
            }
        }
        if self.data_device.is_none() {
            self.data_device = Some(self.data_devices.get_data_device(qh, &seat));
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard {
            if let Some(k) = self.keyboard.take() {
                k.release();
            }
        }
        if capability == Capability::Pointer {
            if let Some(p) = self.pointer.take() {
                p.pointer().release();
            }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl App {
    fn key(&mut self, event: KeyEvent) {
        let key = keysym_to_key(event.keysym);
        let mods = self.mods();
        // With Ctrl held, the text is the base character, not the control
        // code xkb produced: the encoder applies Ctrl itself.
        let text = if mods.ctrl {
            event
                .keysym
                .key_char()
                .filter(|c| !c.is_control())
                .map(String::from)
                .unwrap_or_default()
        } else {
            event.utf8.unwrap_or_default()
        };
        self.panel
            .key(&KeyPress::new(key, mods, text), Instant::now());
    }
}

impl KeyboardHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        serial: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
        self.last_serial = serial;
    }
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
    }
    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        serial: u32,
        event: KeyEvent,
    ) {
        self.last_serial = serial;
        self.key(event);
    }
    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }
    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }
    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        modifiers: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
        self.modifiers = modifiers;
    }
}

impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        let now = Instant::now();
        let mods = self.mods();
        for event in events {
            let Some(m) = &self.surface else { return };
            if &event.surface != m.layer.wl_surface() {
                continue;
            }
            let (x, y) = event.position;
            let button = |b: u32| match b {
                BTN_LEFT => Some(PointerButton::Left),
                BTN_RIGHT => Some(PointerButton::Right),
                BTN_MIDDLE => Some(PointerButton::Middle),
                _ => None,
            };
            match event.kind {
                PointerEventKind::Enter { .. } => {
                    self.cursor = None; // the compositor reset it
                    self.panel.pointer_motion(x, y, mods, now);
                }
                PointerEventKind::Leave { .. } => self.panel.pointer_leave(),
                PointerEventKind::Motion { .. } => self.panel.pointer_motion(x, y, mods, now),
                PointerEventKind::Press {
                    button: b, serial, ..
                } => {
                    self.last_serial = serial;
                    if let Some(b) = button(b) {
                        self.panel.pointer_press(b, x, y, mods, now);
                    }
                }
                PointerEventKind::Release { button: b, .. } => {
                    if let Some(b) = button(b) {
                        self.panel.pointer_release(b, x, y, mods, now);
                    }
                }
                PointerEventKind::Axis { vertical, .. } => {
                    let delta =
                        wheel_delta(vertical.value120, vertical.discrete, vertical.absolute);
                    self.panel.wheel(x, y, delta, mods, now);
                }
            }
        }
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl DataDeviceHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataDevice,
        _: f64,
        _: f64,
        _: &wl_surface::WlSurface,
    ) {
    }
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}
    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
}

impl DataOfferHandler for App {
    fn source_actions(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
    fn selected_action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
}

impl DataSourceHandler for App {
    fn accept_mime(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: Option<String>,
    ) {
    }
    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        source: &WlDataSource,
        _: String,
        pipe: WritePipe,
    ) {
        let Some((s, text)) = &self.copy_source else {
            return;
        };
        if s.inner() != source {
            return;
        }
        let bytes = text.clone().into_bytes();
        let fd = std::os::fd::OwnedFd::from(pipe);
        // Written on a thread so a slow reader cannot stall the terminal.
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = std::fs::File::from(fd).write_all(&bytes);
        });
    }
    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        if self
            .copy_source
            .as_ref()
            .is_some_and(|(s, _)| s.inner() == source)
        {
            self.copy_source = None;
        }
        source.destroy();
    }
    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

smithay_client_toolkit::delegate_registry!(App);

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_dispatch2!(App);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keysyms() {
        assert_eq!(keysym_to_key(Keysym::Return), Key::Enter);
        assert_eq!(keysym_to_key(Keysym::ISO_Left_Tab), Key::Backtab);
        assert_eq!(keysym_to_key(Keysym::F12), Key::F(12));
        assert_eq!(keysym_to_key(Keysym::A), Key::Char('a'));
        assert_eq!(keysym_to_key(Keysym::a), Key::Char('a'));
        assert_eq!(keysym_to_key(Keysym::space), Key::Char(' '));
        assert_eq!(keysym_to_key(Keysym::Shift_L), Key::Modifier);
        assert_eq!(keysym_to_key(Keysym::KP_Page_Up), Key::PageUp);
    }
}
