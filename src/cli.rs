//! Command-line handling, done by hand and before anything touches Wayland.
//!
//! A remote-control invocation (`wl-dropdown toggle` with a terminal already
//! running) must reach the socket without connecting to a display at all: it
//! is faster, and it works where there is no usable display (ssh, systemd
//! units, cron).

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;

use crate::paths::APP_NAME;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Toggle,
    Show,
    Hide,
    Reload,
    Settings,
}

impl Command {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "toggle" => Self::Toggle,
            "show" => Self::Show,
            "hide" => Self::Hide,
            "reload" => Self::Reload,
            "settings" => Self::Settings,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Toggle => "toggle",
            Self::Show => "show",
            Self::Hide => "hide",
            Self::Reload => "reload",
            Self::Settings => "settings",
        }
    }

    /// Commands that only mean something to a running terminal. Without this
    /// distinction, falling through would *start* one — so a theme engine's
    /// post-hook firing on a wallpaper change would open a terminal on the
    /// user's desktop.
    pub fn is_remote_only(self) -> bool {
        matches!(self, Self::Hide | Self::Reload)
    }
}

/// What an invocation asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invocation {
    Help,
    Version,
    Command(Command),
    /// `-e program args...`: the request is already encoded for the socket.
    Exec(Vec<u8>),
    Error {
        message: String,
        code: i32,
    },
}

/// Parses `args` (without argv[0]). `cwd` is where an `-e` command will run.
pub fn parse(args: &[OsString], cwd: &Path) -> Invocation {
    // `-e` ends our own arguments: everything after it is the command to run,
    // its flags included.
    let exec_at = args.iter().position(|a| a == "-e");
    let own = &args[..exec_at.unwrap_or(args.len())];
    let has = |short: &str, long: &str| own.iter().any(|a| a == short || a == long);

    if has("-h", "--help") {
        return Invocation::Help;
    }
    if has("-v", "--version") {
        return Invocation::Version;
    }
    if let Some(at) = exec_at {
        let program = &args[at + 1..];
        if program.is_empty() {
            return Invocation::Error {
                message: format!("{APP_NAME}: -e needs a program to run"),
                code: 2,
            };
        }
        return Invocation::Exec(exec_request(cwd, program));
    }

    let command = own.iter().find(|a| !a.as_bytes().starts_with(b"-"));
    match command {
        None => Invocation::Command(Command::Toggle),
        Some(c) => match c.to_str().and_then(Command::parse) {
            Some(cmd) => Invocation::Command(cmd),
            None => Invocation::Error {
                message: format!(
                    "{APP_NAME}: unknown command '{}' (expected toggle, show, hide, reload or settings)",
                    c.to_string_lossy()
                ),
                code: 2,
            },
        },
    }
}

/// The socket message for `-e`: "exec", the caller's directory, then the
/// command's arguments, NUL-separated so that arguments keep their spaces. A
/// single argument is a command line for the shell, which is what a launcher
/// passing `-e "htop -d 5"` expects; several are run as given.
pub fn exec_request(cwd: &Path, program: &[OsString]) -> Vec<u8> {
    let mut out = b"exec".to_vec();
    let mut push = |part: &[u8]| {
        out.push(0);
        out.extend_from_slice(part);
    };
    push(cwd.as_os_str().as_bytes());
    if program.len() == 1 {
        push(b"/bin/sh");
        push(b"-c");
    }
    for arg in program {
        push(arg.as_bytes());
    }
    out
}

/// A request as the running terminal receives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Command(Command),
    Exec { cwd: OsString, argv: Vec<OsString> },
    Unknown,
}

pub fn decode_request(bytes: &[u8]) -> Request {
    let parts: Vec<&[u8]> = bytes.split(|&b| b == 0).collect();
    if parts.len() > 2 && parts[0] == b"exec" {
        let os = |p: &[u8]| OsString::from_vec(p.to_vec());
        return Request::Exec {
            cwd: os(parts[1]),
            argv: parts[2..].iter().map(|p| os(p)).collect(),
        };
    }
    std::str::from_utf8(bytes)
        .ok()
        .map(str::trim)
        .and_then(Command::parse)
        .map_or(Request::Unknown, Request::Command)
}

pub fn usage() -> String {
    format!(
        "Yakuake-style dropdown terminal on wlr-layer-shell.

Usage: {APP_NAME} [command]
       {APP_NAME} -e <program> [args...]

Commands:
  toggle     show the terminal, or hide it if visible (default)
  show       show the terminal
  hide       hide the terminal
  reload     re-read configuration, including theme.conf
  settings   open the settings window

Options:
  -e <program>   run program in a new tab and show the terminal; the
                 rest of the line is its arguments
  -h, --help     show this help
  -v, --version  show version information

The first invocation starts the terminal; later ones are forwarded to it
over a socket in $XDG_RUNTIME_DIR."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn run(list: &[&str]) -> Invocation {
        parse(&args(list), Path::new("/work"))
    }

    #[test]
    fn default_is_toggle() {
        assert_eq!(run(&[]), Invocation::Command(Command::Toggle));
    }

    #[test]
    fn named_commands() {
        for name in ["toggle", "show", "hide", "reload", "settings"] {
            assert_eq!(
                run(&[name]),
                Invocation::Command(Command::parse(name).unwrap())
            );
        }
    }

    #[test]
    fn unknown_flags_are_skipped_when_finding_the_command() {
        assert_eq!(
            run(&["--frobnicate", "show"]),
            Invocation::Command(Command::Show)
        );
    }

    #[test]
    fn unknown_command_is_an_error_with_code_2() {
        match run(&["explode"]) {
            Invocation::Error { message, code } => {
                assert_eq!(code, 2);
                assert!(message.contains("unknown command 'explode'"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn help_and_version() {
        assert_eq!(run(&["-h"]), Invocation::Help);
        assert_eq!(run(&["show", "--help"]), Invocation::Help);
        assert_eq!(run(&["-v"]), Invocation::Version);
        assert_eq!(run(&["--version"]), Invocation::Version);
    }

    #[test]
    fn flags_after_dash_e_belong_to_the_program() {
        // `--help` here is htop's, not ours.
        let inv = run(&["-e", "htop", "--help"]);
        assert_eq!(inv, Invocation::Exec(b"exec\0/work\0htop\0--help".to_vec()));
    }

    #[test]
    fn dash_e_without_program_is_an_error() {
        assert!(matches!(run(&["-e"]), Invocation::Error { code: 2, .. }));
    }

    #[test]
    fn single_exec_argument_goes_through_the_shell() {
        let inv = run(&["-e", "htop -d 5"]);
        assert_eq!(
            inv,
            Invocation::Exec(b"exec\0/work\0/bin/sh\0-c\0htop -d 5".to_vec())
        );
    }

    #[test]
    fn remote_only_commands() {
        assert!(Command::Hide.is_remote_only());
        assert!(Command::Reload.is_remote_only());
        assert!(!Command::Toggle.is_remote_only());
        assert!(!Command::Show.is_remote_only());
        assert!(!Command::Settings.is_remote_only());
    }

    #[test]
    fn decode_roundtrips_exec_and_commands() {
        let req = exec_request(Path::new("/tmp/a b"), &args(&["vim", "my file.txt"]));
        assert_eq!(
            decode_request(&req),
            Request::Exec {
                cwd: "/tmp/a b".into(),
                argv: args(&["vim", "my file.txt"])
            }
        );
        assert_eq!(decode_request(b"show\n"), Request::Command(Command::Show));
        assert_eq!(decode_request(b"reload"), Request::Command(Command::Reload));
        assert_eq!(decode_request(b"nonsense"), Request::Unknown);
        // "exec" with no argv is not an exec request.
        assert_eq!(decode_request(b"exec\0/tmp"), Request::Unknown);
    }
}
