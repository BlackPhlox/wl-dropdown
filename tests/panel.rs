//! Behaviour of the dropdown's UI, driven through the same entry points the
//! Wayland frontend uses, with headless terminals.

// Positions and fractions here are computed exactly, from whole numbers.
#![allow(clippy::float_cmp)]

use std::time::Duration;

use alacritty_terminal::event::Event;
use wl_dropdown::keys::{Key, Mods};
use wl_dropdown::panel::{CursorShape, Effect, MenuItem, PointerButton, Target};
use wl_dropdown::scenes::{test_values, Stage};
use wl_dropdown::settings::Values;
use wl_dropdown::tabs::TabSession;

fn stage() -> Stage {
    let mut st = Stage::new(
        Values {
            height_percent: 0.5,
            ..test_values()
        },
        1200,
        600,
        1.0,
    );
    st.feed(b"hello\r\n");
    st
}

fn centre(r: wl_dropdown::panel::RectF) -> (f64, f64) {
    (r.x + r.w / 2.0, r.y + r.h / 2.0)
}

#[test]
fn opens_mapped_and_rolled_down() {
    let mut st = stage();
    assert!(st.panel.controller().opened());
    assert!(st.panel.controller().mapped());
    let now = st.now;
    let l = st.panel.layout(now);
    assert_eq!(l.panel.y, 0.0);
    assert_eq!(l.panel.w, 720.0);
    assert_eq!(l.panel.h, 300.0);
    assert_eq!(l.panel.x, 240.0, "centred");
}

#[test]
fn clicking_outside_hides_after_the_animation() {
    let mut st = stage();
    st.click(PointerButton::Left, 5.0, 590.0);
    assert!(!st.panel.controller().opened());
    assert!(st.panel.controller().mapped());
    st.advance(Duration::from_millis(250));
    assert!(!st.panel.controller().mapped());
}

#[test]
fn clicking_inert_parts_of_the_panel_does_not_hide() {
    let mut st = stage();
    let now = st.now;
    let l = st.panel.layout(now);
    // The top margin above the terminal.
    let (x, y) = (l.term.x + 50.0, l.panel.y + 2.0);
    assert_eq!(st.panel.hit(x, y, now), Target::Panel);
    st.click(PointerButton::Left, x, y);
    assert!(st.panel.controller().opened());
}

#[test]
fn hit_targets() {
    let mut st = stage();
    let now = st.now;
    let l = st.panel.layout(now);
    assert_eq!(
        st.panel.hit(centre(l.term).0, centre(l.term).1, now),
        Target::Terminal
    );
    assert_eq!(
        st.panel.hit(centre(l.gear).0, centre(l.gear).1, now),
        Target::Gear
    );
    assert_eq!(
        st.panel.hit(centre(l.plus).0, centre(l.plus).1, now),
        Target::Plus
    );
    assert_eq!(
        st.panel.hit(l.tabs[0].x + 10.0, centre(l.tabs[0]).1, now),
        Target::Tab(0)
    );
    let p = l.panel;
    assert_eq!(
        st.panel.hit(p.x + 1.0, p.y + 50.0, now),
        Target::Resize {
            width: true,
            height: false,
            left: true
        }
    );
    assert_eq!(
        st.panel.hit(p.right() - 1.0, p.y + 50.0, now),
        Target::Resize {
            width: true,
            height: false,
            left: false
        }
    );
    assert_eq!(
        st.panel.hit(p.x + 300.0, p.bottom() - 1.0, now),
        Target::Resize {
            width: false,
            height: true,
            left: false
        }
    );
    assert_eq!(
        st.panel.hit(p.x + 2.0, p.bottom() - 2.0, now),
        Target::Resize {
            width: true,
            height: true,
            left: true
        }
    );
    assert_eq!(st.panel.hit(p.x - 10.0, 10.0, now), Target::Backdrop);
}

#[test]
fn cursor_shapes_follow_the_pointer() {
    let mut st = stage();
    let now = st.now;
    let l = st.panel.layout(now);
    let (x, y) = centre(l.term);
    st.panel.pointer_motion(x, y, Mods::NONE, now);
    assert_eq!(st.panel.cursor_shape(now), CursorShape::Text);
    st.panel
        .pointer_motion(l.panel.x + 1.0, l.panel.y + 50.0, Mods::NONE, now);
    assert_eq!(st.panel.cursor_shape(now), CursorShape::EwResize);
    st.panel.pointer_motion(
        l.panel.right() - 2.0,
        l.panel.bottom() - 2.0,
        Mods::NONE,
        now,
    );
    assert_eq!(st.panel.cursor_shape(now), CursorShape::NwseResize);
    st.panel.pointer_motion(5.0, 590.0, Mods::NONE, now);
    assert_eq!(st.panel.cursor_shape(now), CursorShape::Default);
}

#[test]
fn tab_buttons() {
    let mut st = stage();
    let now = st.now;
    let plus = centre(st.panel.layout(now).plus);
    st.click(PointerButton::Left, plus.0, plus.1);
    st.click(PointerButton::Left, plus.0, plus.1);
    assert_eq!(
        st.panel.tabs().len(),
        2,
        "the second click hit where + used to be"
    );
    let plus = centre(st.panel.layout(now).plus);
    st.click(PointerButton::Left, plus.0, plus.1);
    assert_eq!(st.panel.tabs().len(), 3);
    assert_eq!(st.panel.tabs().active_index(), 2);

    let l = st.panel.layout(now);
    st.click(PointerButton::Left, l.tabs[0].x + 8.0, centre(l.tabs[0]).1);
    assert_eq!(st.panel.tabs().active_index(), 0);

    let close = centre(l.closes[1].expect("close buttons with several tabs"));
    st.click(PointerButton::Left, close.0, close.1);
    assert_eq!(st.panel.tabs().len(), 2);
    assert_eq!(st.panel.tabs().active_index(), 0);

    // A single tab has no close button.
    let l = st.panel.layout(now);
    let close = centre(l.closes[1].unwrap());
    st.click(PointerButton::Left, close.0, close.1);
    assert!(st.panel.layout(now).closes[0].is_none());
}

#[test]
fn rename_by_right_click_and_keyboard() {
    let mut st = stage();
    let now = st.now;
    let l = st.panel.layout(now);
    st.click(PointerButton::Right, l.tabs[0].x + 8.0, centre(l.tabs[0]).1);
    assert_eq!(st.panel.renaming(), Some(0));
    for c in "logs".chars() {
        st.key(Key::Char(c), Mods::NONE, &c.to_string());
    }
    st.key(Key::Enter, Mods::NONE, "\r");
    assert_eq!(st.panel.renaming(), None);
    assert_eq!(st.panel.tabs().label(0), "1: logs");

    // Escape cancels.
    st.key(Key::Char('e'), Mods::ctrl_shift(), "E");
    assert_eq!(st.panel.renaming(), Some(0));
    st.key(Key::Char('x'), Mods::NONE, "x");
    st.key(Key::Escape, Mods::NONE, "");
    assert_eq!(st.panel.tabs().label(0), "1: logs");

    // Clicking the terminal keeps the edit, as losing focus does.
    st.key(Key::Char('e'), Mods::ctrl_shift(), "E");
    st.key(Key::Char('y'), Mods::NONE, "y");
    let t = centre(st.panel.layout(now).term);
    st.click(PointerButton::Left, t.0, t.1);
    assert_eq!(st.panel.tabs().label(0), "1: y");

    // An empty name follows the program's title again.
    st.feed(b"\x1b]0;make\x07");
    st.key(Key::Char('e'), Mods::ctrl_shift(), "E");
    st.key(Key::Backspace, Mods::NONE, "");
    st.key(Key::Enter, Mods::NONE, "");
    assert_eq!(st.panel.tabs().label(0), "1: make");
}

#[test]
fn opening_a_tab_drops_an_open_editor() {
    let mut st = stage();
    st.panel.start_rename(0);
    st.key(Key::Char('t'), Mods::ctrl_shift(), "T");
    assert_eq!(
        st.panel.renaming(),
        Some(0),
        "keys go to the editor while it is open"
    );
    let now = st.now;
    st.panel.new_tab(now);
    assert_eq!(st.panel.renaming(), None);
}

#[test]
fn gear_opens_settings_and_hides() {
    let mut st = stage();
    let now = st.now;
    let g = centre(st.panel.layout(now).gear);
    st.click(PointerButton::Left, g.0, g.1);
    assert_eq!(st.panel.take_effects(), vec![Effect::OpenSettings]);
    assert!(!st.panel.controller().opened());
}

#[test]
fn resizing_by_the_edges_is_clamped_and_not_persisted() {
    let mut st = stage();
    let now = st.now;
    let p = st.panel.layout(now).panel;
    st.panel.pointer_press(
        PointerButton::Left,
        p.right() - 2.0,
        p.bottom() - 2.0,
        Mods::NONE,
        now,
    );
    st.panel.pointer_motion(1100.0, 450.0, Mods::NONE, now);
    let (w, h) = st.panel.size_percent();
    assert!((w - 2.0 * 500.0 / 1200.0).abs() < 1e-9, "{w}");
    assert!((h - 0.75).abs() < 1e-9, "{h}");
    st.panel.pointer_motion(600.0, 1.0, Mods::NONE, now);
    assert_eq!(
        st.panel.size_percent(),
        (0.2, 0.15),
        "clamped to what settings accept"
    );
    st.panel
        .pointer_release(PointerButton::Left, 600.0, 1.0, Mods::NONE, now);
    assert_eq!(st.panel.values().width_percent, 0.6, "settings untouched");

    // A changed setting replaces the dragged size.
    let v = Values {
        width_percent: 0.9,
        ..st.panel.values().clone()
    };
    st.panel.apply_settings(v, now);
    assert_eq!(st.panel.size_percent().0, 0.9);
}

#[test]
fn shortcuts_switch_move_and_close_tabs() {
    let mut st = stage();
    for _ in 0..2 {
        st.key(Key::Char('t'), Mods::ctrl_shift(), "T");
    }
    assert_eq!(st.panel.tabs().len(), 3);
    assert_eq!(st.panel.tabs().active_index(), 2);
    st.key(Key::Right, Mods::SHIFT, "");
    assert_eq!(st.panel.tabs().active_index(), 0, "switching wraps");
    st.key(Key::Backtab, Mods::ctrl_shift(), "");
    assert_eq!(st.panel.tabs().active_index(), 2);
    let id = st.panel.tabs().active().id();
    st.key(Key::Left, Mods::ctrl_shift(), "");
    assert_eq!(st.panel.tabs().active_index(), 1);
    assert_eq!(
        st.panel.tabs().active().id(),
        id,
        "moving keeps the tab active"
    );
    st.key(Key::Char('w'), Mods::ctrl_shift(), "W");
    assert_eq!(st.panel.tabs().len(), 2);
    st.key(Key::Char('w'), Mods::ctrl_shift(), "W");
    st.key(Key::Char('w'), Mods::ctrl_shift(), "W");
    assert_eq!(st.panel.tabs().len(), 1, "the last tab stays");
}

#[test]
fn unbound_shortcuts_reach_the_program() {
    let mut st = stage();
    let v = Values {
        previous_tab_keys: String::new(),
        ..st.panel.values().clone()
    };
    let now = st.now;
    st.panel.apply_settings(v, now);
    st.key(Key::Char('t'), Mods::ctrl_shift(), "T");
    st.key(Key::Left, Mods::SHIFT, "");
    assert_eq!(
        st.panel.tabs().active_index(),
        1,
        "Shift+Left no longer switches"
    );
}

#[test]
fn copy_and_paste_go_through_the_frontend() {
    let mut st = stage();
    let now = st.now;
    let l = st.panel.layout(now);
    let m = st.panel.metrics();
    let y = l.term.y + m.height * 0.5;
    st.panel.pointer_press(
        PointerButton::Left,
        l.term.x + 2.0 + 0.1,
        y,
        Mods::NONE,
        now,
    );
    st.panel.pointer_release(
        PointerButton::Left,
        l.term.x + 2.0 + m.width * 4.9,
        y,
        Mods::NONE,
        now,
    );
    st.key(Key::Char('c'), Mods::ctrl_shift(), "C");
    st.key(Key::Char('v'), Mods::ctrl_shift(), "V");
    st.key(Key::Insert, Mods::SHIFT, "");
    assert_eq!(
        st.panel.take_effects(),
        vec![Effect::Copy("hello".into()), Effect::Paste, Effect::Paste]
    );
    // Typing drops the selection.
    st.key(Key::Char('a'), Mods::NONE, "a");
    assert_eq!(st.panel.tabs().active().selected_text(), "");
}

#[test]
fn osc52_copies() {
    let mut st = stage();
    st.feed(b"\x1b]52;c;aGVsbG8=\x07");
    assert_eq!(st.panel.take_effects(), vec![Effect::Copy("hello".into())]);
}

#[test]
fn wheel_scrolls_history_but_not_the_alternate_screen() {
    let mut st = stage();
    let lines = (0..100).fold(String::new(), |s, i| s + &i.to_string() + "\r\n");
    st.feed(lines.as_bytes());
    let now = st.now;
    let t = centre(st.panel.layout(now).term);
    st.panel.wheel(t.0, t.1, 120.0, Mods::NONE, now);
    assert_eq!(
        st.panel.tabs().active().display_offset(),
        3,
        "a notch is three lines"
    );
    st.panel.wheel(t.0, t.1, -40.0, Mods::NONE, now);
    assert_eq!(st.panel.tabs().active().display_offset(), 2);

    // Typing returns to the bottom.
    st.key(Key::Char('x'), Mods::NONE, "x");
    assert_eq!(st.panel.tabs().active().display_offset(), 0);

    st.feed(b"\x1b[?1049h");
    st.panel.wheel(t.0, t.1, 120.0, Mods::NONE, now);
    assert_eq!(st.panel.tabs().active().display_offset(), 0);
}

#[test]
fn scrollbar_click_jumps_and_drag_follows() {
    let mut st = stage();
    let lines = (0..300).fold(String::new(), |s, i| s + &i.to_string() + "\r\n");
    st.feed(lines.as_bytes());
    let now = st.now;
    let l = st.panel.layout(now);
    assert!(l.scrollbar.w > 0.0 && l.thumb.is_some());
    let x = l.scrollbar.x + l.scrollbar.w / 2.0;
    // Click the top of the track: all the way back.
    st.panel
        .pointer_press(PointerButton::Left, x, l.scrollbar.y, Mods::NONE, now);
    let history = st.panel.tabs().active().history_size();
    assert_eq!(st.panel.tabs().active().display_offset(), history);
    // Drag to the bottom.
    st.panel
        .pointer_motion(x, l.scrollbar.bottom() + 100.0, Mods::NONE, now);
    assert_eq!(st.panel.tabs().active().display_offset(), 0);
    st.panel.pointer_release(
        PointerButton::Left,
        x,
        l.scrollbar.bottom(),
        Mods::NONE,
        now,
    );
}

#[test]
fn a_program_tracking_the_mouse_gets_clicks_unless_shift_is_held() {
    let mut st = stage();
    st.feed(b"\x1b[?1000h\x1b[?1006h");
    let now = st.now;
    let t = centre(st.panel.layout(now).term);
    st.panel
        .pointer_press(PointerButton::Left, t.0, t.1, Mods::NONE, now);
    st.panel
        .pointer_release(PointerButton::Left, t.0, t.1, Mods::NONE, now);
    assert_eq!(
        st.panel.tabs().active().selected_text(),
        "",
        "no selection while reporting"
    );

    // Shift still selects.
    let l = st.panel.layout(now);
    let m = st.panel.metrics();
    let y = l.term.y + m.height * 0.5;
    st.panel
        .pointer_press(PointerButton::Left, l.term.x + 2.1, y, Mods::SHIFT, now);
    st.panel.pointer_release(
        PointerButton::Left,
        l.term.x + 2.0 + m.width * 4.9,
        y,
        Mods::SHIFT,
        now,
    );
    assert_eq!(st.panel.tabs().active().selected_text(), "hello");
}

#[test]
fn the_last_programs_exit_gives_a_fresh_tab_and_hides() {
    let mut st = stage();
    let first = st.panel.tabs().active().id();
    let now = st.now;
    st.panel.handle_session_event(
        first,
        Event::ChildExit(std::os::unix::process::ExitStatusExt::from_raw(0)),
        now,
    );
    assert_eq!(st.panel.tabs().len(), 1);
    assert_ne!(st.panel.tabs().active().id(), first);
    assert!(st.panel.tabs().active().started());
    assert!(!st.panel.controller().opened());

    // With several tabs, the exited one just closes.
    st.panel.show(now);
    st.panel.new_tab(now);
    let second = st.panel.tabs().active().id();
    st.panel.handle_session_event(second, Event::Exit, now);
    assert_eq!(st.panel.tabs().len(), 1);
    assert!(st.panel.controller().opened());
    // A late event for a session already gone is ignored.
    st.panel.handle_session_event(second, Event::Exit, now);
    assert_eq!(st.panel.tabs().len(), 1);
}

#[test]
fn titles_label_tabs() {
    let mut st = stage();
    st.feed(b"\x1b]2;~/src\x07");
    assert_eq!(st.panel.tabs().label(0), "1: ~/src");
    assert_eq!(st.panel.tabs().active().title(), "~/src");
}

#[test]
fn execute_runs_in_a_new_tab_once_started() {
    let mut st = stage();
    let now = st.now;
    st.panel.hide(now);
    st.panel
        .execute(vec!["htop".into()], Some("/tmp".into()), now);
    assert_eq!(st.panel.tabs().len(), 2);
    assert!(st.panel.controller().opened(), "exec shows the terminal");
}

#[test]
fn many_tabs_scroll_the_active_one_into_view() {
    let mut st = stage();
    for _ in 0..15 {
        st.key(Key::Char('t'), Mods::ctrl_shift(), "T");
    }
    let now = st.now;
    let l = st.panel.layout(now);
    let active = l.tabs[st.panel.tabs().active_index()];
    assert!(
        active.right() <= l.flick.right() + 0.5,
        "{active:?} vs {:?}",
        l.flick
    );
    assert!(l.tabs[0].x < l.flick.x, "the first tab has scrolled off");

    // The wheel over the tab strip scrolls it back.
    let (x, y) = centre(active);
    st.panel.wheel(x, y, 10_000.0, Mods::NONE, now);
    let l = st.panel.layout(now);
    assert_eq!(l.tabs[0].x, l.flick.x);
}

#[test]
fn font_size_changes_the_grid() {
    let mut st = stage();
    let before = st.panel.tabs().active().size();
    let v = Values {
        font_size: 20.0,
        ..st.panel.values().clone()
    };
    let now = st.now;
    st.panel.apply_settings(v, now);
    st.panel.settle(now);
    let after = st.panel.tabs().active().size();
    assert!(
        after.cols < before.cols && after.rows < before.rows,
        "{before:?} -> {after:?}"
    );
}

#[test]
fn the_toggle_animation_moves_the_panel() {
    let mut st = stage();
    let now = st.now;
    st.panel.toggle(now);
    let h = st.panel.layout(now).panel.h;
    st.advance(Duration::from_millis(90));
    let mid = st.panel.layout(st.now).panel.y;
    assert!(mid < 0.0 && mid > -h, "{mid}");
    st.advance(Duration::from_millis(200));
    assert_eq!(st.panel.layout(st.now).panel.y, -h);
    assert!(!st.panel.controller().mapped());
    let now = st.now;
    st.panel.toggle(now);
    assert!(st.panel.animating(now));
}

fn select_hello(st: &mut Stage) {
    let now = st.now;
    let l = st.panel.layout(now);
    let m = st.panel.metrics();
    let y = l.term.y + m.height * 0.5;
    st.panel
        .pointer_press(PointerButton::Left, l.term.x + 2.1, y, Mods::NONE, now);
    st.panel.pointer_release(
        PointerButton::Left,
        l.term.x + 2.0 + m.width * 4.9,
        y,
        Mods::NONE,
        now,
    );
}

fn right_click_terminal(st: &mut Stage, mods: Mods) -> (f64, f64) {
    let now = st.now;
    let t = centre(st.panel.layout(now).term);
    st.panel
        .pointer_press(PointerButton::Right, t.0, t.1, mods, now);
    st.panel
        .pointer_release(PointerButton::Right, t.0, t.1, mods, now);
    t
}

fn click_menu_item(st: &mut Stage, item: MenuItem) {
    let now = st.now;
    let menu = st.panel.menu_rect(now).expect("menu is open");
    let index = MenuItem::ALL.iter().position(|i| *i == item).unwrap();
    let (x, y) = (menu.x + 20.0, menu.y + 4.0 + 24.0 * index as f64 + 12.0);
    assert_eq!(st.panel.hit(x, y, now), Target::MenuItem(item));
    st.click(PointerButton::Left, x, y);
}

#[test]
fn right_click_menu_copies_the_selection() {
    let mut st = stage();
    select_hello(&mut st);
    right_click_terminal(&mut st, Mods::NONE);
    assert!(st.panel.menu_open());
    assert_eq!(
        st.panel.tabs().active().selected_text(),
        "hello",
        "right click keeps the selection"
    );
    click_menu_item(&mut st, MenuItem::Copy);
    assert!(!st.panel.menu_open());
    assert_eq!(st.panel.take_effects(), vec![Effect::Copy("hello".into())]);
}

#[test]
fn right_click_menu_pastes() {
    let mut st = stage();
    right_click_terminal(&mut st, Mods::NONE);
    click_menu_item(&mut st, MenuItem::Paste);
    assert_eq!(st.panel.take_effects(), vec![Effect::Paste]);
    assert!(!st.panel.menu_open());
}

#[test]
fn copy_is_disabled_without_a_selection() {
    let mut st = stage();
    right_click_terminal(&mut st, Mods::NONE);
    click_menu_item(&mut st, MenuItem::Copy);
    assert!(st.panel.take_effects().is_empty());
    assert!(
        st.panel.menu_open(),
        "a disabled entry leaves the menu open"
    );
}

#[test]
fn the_menu_closes_without_side_effects() {
    let mut st = stage();
    right_click_terminal(&mut st, Mods::NONE);
    st.key(Key::Escape, Mods::NONE, "");
    assert!(!st.panel.menu_open());

    // A click outside closes it, and is used up doing so: the dropdown stays.
    right_click_terminal(&mut st, Mods::NONE);
    st.click(PointerButton::Left, 5.0, 590.0);
    assert!(!st.panel.menu_open());
    assert!(st.panel.controller().opened());

    // Typing closes it and still types.
    right_click_terminal(&mut st, Mods::NONE);
    st.key(Key::Char('t'), Mods::ctrl_shift(), "T");
    assert!(!st.panel.menu_open());
    assert_eq!(st.panel.tabs().len(), 2);

    // So does hiding.
    right_click_terminal(&mut st, Mods::NONE);
    let now = st.now;
    st.panel.hide(now);
    assert!(!st.panel.menu_open());
}

#[test]
fn a_second_right_click_moves_the_menu() {
    let mut st = stage();
    right_click_terminal(&mut st, Mods::NONE);
    let now = st.now;
    let first = st.panel.menu_rect(now).unwrap();
    let t = st.panel.layout(now).term;
    st.panel.pointer_press(
        PointerButton::Right,
        t.x + 10.0,
        t.y + 10.0,
        Mods::NONE,
        now,
    );
    let second = st.panel.menu_rect(now).unwrap();
    assert_ne!(first, second);
}

#[test]
fn the_menu_stays_inside_the_panel() {
    let mut st = stage();
    let now = st.now;
    let l = st.panel.layout(now);
    let (x, y) = (l.term.right() - 8.0, l.term.bottom() - 2.0);
    st.panel
        .pointer_press(PointerButton::Right, x, y, Mods::NONE, now);
    let m = st.panel.menu_rect(now).unwrap();
    assert!(
        m.right() <= l.panel.right() && m.bottom() <= l.panel.bottom(),
        "{m:?} in {:?}",
        l.panel
    );
}

#[test]
fn a_program_tracking_the_mouse_gets_right_clicks_unless_shift_is_held() {
    let mut st = stage();
    st.feed(b"\x1b[?1000h\x1b[?1006h");
    right_click_terminal(&mut st, Mods::NONE);
    assert!(!st.panel.menu_open());
    right_click_terminal(&mut st, Mods::SHIFT);
    assert!(st.panel.menu_open());
}

#[test]
fn right_click_on_a_tab_still_renames() {
    let mut st = stage();
    let now = st.now;
    let l = st.panel.layout(now);
    st.click(PointerButton::Right, l.tabs[0].x + 8.0, centre(l.tabs[0]).1);
    assert_eq!(st.panel.renaming(), Some(0));
    assert!(!st.panel.menu_open());
}
