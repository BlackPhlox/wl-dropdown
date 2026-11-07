//! `wl-dropdown`: the first invocation starts the terminal; later ones are
//! remote controls over a per-user socket.

mod app;
#[cfg(feature = "settings-ui")]
mod settings_ui;

use std::process::ExitCode;

use wl_dropdown::cli::{self, Command, Invocation};
use wl_dropdown::ipc;
use wl_dropdown::paths::APP_NAME;

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let cwd = std::env::current_dir().unwrap_or_default();
    let (command, request) = match cli::parse(&args, &cwd) {
        Invocation::Help => {
            println!("{}", cli::usage());
            return ExitCode::SUCCESS;
        }
        Invocation::Version => {
            println!("{APP_NAME} {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Invocation::Error { message, code } => {
            eprintln!("{message}");
            return ExitCode::from(code as u8);
        }
        Invocation::Command(c) => (Some(c), c.as_str().as_bytes().to_vec()),
        Invocation::Exec(req) => (None, req),
    };

    // ── Client path: no Wayland connection, deliberately ────────────
    // If a terminal is already running this invocation is only a remote
    // control. It must not need a display: `toggle` over ssh or from a
    // systemd unit should still work, and it is faster this way.
    if command != Some(Command::Settings) {
        if ipc::send(&request) {
            return ExitCode::SUCCESS;
        }
        if command.is_some_and(Command::is_remote_only) {
            return ExitCode::SUCCESS; // nothing running: nothing to hide or reload
        }
    }

    // ── From here we are the process that owns a window ─────────────
    let result = if command == Some(Command::Settings) {
        run_settings()
    } else {
        app::run(&request)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{APP_NAME}: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(feature = "settings-ui")]
fn run_settings() -> Result<(), String> {
    settings_ui::run()
}

#[cfg(not(feature = "settings-ui"))]
fn run_settings() -> Result<(), String> {
    Err(format!(
        "built without the settings window; edit ~/.config/{APP_NAME}/{APP_NAME}.conf and run `{APP_NAME} reload`"
    ))
}
