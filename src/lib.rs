//! wl-dropdown: a Yakuake-style dropdown terminal for Wayland.
//!
//! The library holds everything that can be exercised without a compositor —
//! configuration, IPC, input encoding, the terminal sessions and the software
//! renderer — so it can be unit- and screenshot-tested. The binary adds the
//! Wayland frontend and the settings window.

pub mod cli;
pub mod color;
pub mod colorscheme;
pub mod controller;
pub mod dnd;
pub mod fonts;
pub mod ini;
pub mod ipc;
pub mod keys;
pub mod lineedit;
pub mod mouse;
pub mod palette;
pub mod panel;
pub mod paths;
pub mod render;
pub mod scenes;
pub mod session;
pub mod settings;
pub mod snapshot;
pub mod tabs;
