//! Where wl-dropdown keeps things, following the XDG base directory spec.
//!
//! Everything is resolved from the environment each time it is asked for, so
//! tests can point a whole run at a temporary directory by setting
//! `XDG_CONFIG_HOME` and friends.

use std::env;
use std::path::PathBuf;

/// The one name everything else is derived from: the binary, the config
/// directory, the socket and the layer-shell namespace.
pub const APP_NAME: &str = "wl-dropdown";

fn env_dir(var: &str) -> Option<PathBuf> {
    env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

pub fn home_dir() -> PathBuf {
    env_dir("HOME").unwrap_or_else(|| PathBuf::from("/"))
}

/// `$XDG_CONFIG_HOME`, or `~/.config`.
pub fn config_home() -> PathBuf {
    env_dir("XDG_CONFIG_HOME").unwrap_or_else(|| home_dir().join(".config"))
}

/// `$XDG_DATA_HOME` then each of `$XDG_DATA_DIRS`, in lookup order.
pub fn data_dirs() -> Vec<PathBuf> {
    let mut dirs =
        vec![env_dir("XDG_DATA_HOME").unwrap_or_else(|| home_dir().join(".local/share"))];
    let system = env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_owned());
    dirs.extend(
        system
            .split(':')
            .filter(|d| !d.is_empty())
            .map(PathBuf::from),
    );
    dirs
}

/// `~/.config/wl-dropdown`.
pub fn config_dir() -> PathBuf {
    config_home().join(APP_NAME)
}

/// The user's own overrides — the only file wl-dropdown ever writes.
pub fn user_file() -> PathBuf {
    config_dir().join(format!("{APP_NAME}.conf"))
}

/// Written by a desktop theme engine; absent unless something provisions it.
pub fn theme_file() -> PathBuf {
    config_dir().join("theme.conf")
}

/// Read-only baseline, typically a store symlink from the home-manager module.
pub fn defaults_file() -> PathBuf {
    config_dir().join("defaults.conf")
}

/// The per-user control socket. `XDG_RUNTIME_DIR` is already per-user and
/// mode 0700.
pub fn socket_path() -> PathBuf {
    env_dir("XDG_RUNTIME_DIR")
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(format!("{APP_NAME}.sock"))
}
