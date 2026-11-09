//! Developer tool for screenshot testing. Build it with
//! `cargo run --features snapshot-tool --bin wl-dropdown-snapshot -- ...`.
//!
//!     wl-dropdown-snapshot list
//!     wl-dropdown-snapshot render [--out DIR] [--backdrop RRGGBB] [SCENE...]
//!     wl-dropdown-snapshot diff ACTUAL.png EXPECTED.png [--out DIFF.png] [--tolerance N]
//!
//! `render` draws the deterministic test scenes to PNG files, the same images
//! `cargo test` compares against `tests/snapshots/`. `diff` compares any two
//! PNGs — two renders, or two real screenshots taken with grim — and writes
//! an image with the differing pixels in red.

use std::path::PathBuf;
use std::process::ExitCode;

use wl_dropdown::scenes;
use wl_dropdown::snapshot::{compare, Image, Tolerance};

fn usage() -> ExitCode {
    eprintln!(
        "usage: wl-dropdown-snapshot list
       wl-dropdown-snapshot render [--out DIR] [--backdrop RRGGBB] [SCENE...]
       wl-dropdown-snapshot diff ACTUAL.png EXPECTED.png [--out DIFF.png] [--tolerance N]"
    );
    ExitCode::from(2)
}

fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let c = wl_dropdown::color::parse(&format!("#{}", s.trim_start_matches('#')))?;
    Some([c.r, c.g, c.b])
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else {
        return usage();
    };
    let mut rest = args[1..].iter();
    match cmd.as_str() {
        "list" => {
            for s in scenes::all() {
                println!("{}", s.name);
            }
            ExitCode::SUCCESS
        }
        "render" => {
            let mut out = PathBuf::from("target/snapshots/rendered");
            let mut backdrop = None;
            let mut names = Vec::new();
            while let Some(a) = rest.next() {
                match a.as_str() {
                    "--out" => out = PathBuf::from(rest.next().map_or(".", String::as_str)),
                    "--backdrop" => backdrop = rest.next().and_then(|v| parse_hex(v)),
                    name => names.push(name.to_owned()),
                }
            }
            let selected: Vec<_> = scenes::all()
                .into_iter()
                .filter(|s| names.is_empty() || names.iter().any(|n| n == s.name))
                .collect();
            if selected.is_empty() {
                eprintln!("no such scene; try `list`");
                return ExitCode::from(2);
            }
            for scene in selected {
                let mut img = (scene.build)().render();
                if let Some(bg) = backdrop {
                    img = img.over(bg);
                }
                let path = out.join(format!("{}.png", scene.name));
                if let Err(e) = img.save_png(&path) {
                    eprintln!("{}: {e}", path.display());
                    return ExitCode::FAILURE;
                }
                println!("{}", path.display());
            }
            ExitCode::SUCCESS
        }
        "diff" => {
            let (Some(a), Some(b)) = (rest.next(), rest.next()) else {
                return usage();
            };
            let mut out = None;
            let mut tol = Tolerance::default();
            while let Some(flag) = rest.next() {
                match flag.as_str() {
                    "--out" => out = rest.next().map(PathBuf::from),
                    "--tolerance" => {
                        tol.channel = rest
                            .next()
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(tol.channel);
                    }
                    _ => return usage(),
                }
            }
            let load = |p: &str| Image::load_png(p.as_ref()).map_err(|e| format!("{p}: {e}"));
            let (actual, expected) = match (load(a), load(b)) {
                (Ok(x), Ok(y)) => (x, y),
                (Err(e), _) | (_, Err(e)) => {
                    eprintln!("{e}");
                    return ExitCode::FAILURE;
                }
            };
            let Some(c) = compare(&actual, &expected, tol) else {
                println!(
                    "sizes differ: {}x{} vs {}x{}",
                    actual.width, actual.height, expected.width, expected.height
                );
                return ExitCode::FAILURE;
            };
            if let Some(out) = out {
                if let Err(e) = c.diff.save_png(&out) {
                    eprintln!("{}: {e}", out.display());
                }
            }
            println!(
                "{} pixels differ (max channel delta {})",
                c.differing_pixels, c.max_channel_delta
            );
            if c.differing_pixels > tol.pixels {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        _ => usage(),
    }
}
