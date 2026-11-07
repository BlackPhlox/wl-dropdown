//! Screenshot tests: each scene in `wl_dropdown::scenes` is rendered and
//! compared with `tests/snapshots/<scene>.png`.
//!
//! After an intended visual change, regenerate the references with
//!
//!     UPDATE_SNAPSHOTS=1 cargo test --test screenshots
//!
//! and review the changed PNGs in the diff. On a mismatch, the actual render
//! and a diff image (changed pixels in red) are written to `target/snapshots/`.

use wl_dropdown::scenes;
use wl_dropdown::snapshot::{self, Tolerance};

fn check(name: &str) {
    let scene = scenes::by_name(name).unwrap_or_else(|| panic!("no scene named {name}"));
    let image = (scene.build)().render();
    if let Err(msg) = snapshot::check(name, &image, Tolerance::default()) {
        panic!("{msg}");
    }
}

macro_rules! screenshot_tests {
    ($($name:ident),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                check(stringify!($name));
            }
        )*

        /// Every scene has a test above, so none is silently left out.
        #[test]
        fn every_scene_is_covered() {
            let covered = [$(stringify!($name)),*];
            for scene in scenes::all() {
                assert!(covered.contains(&scene.name), "scene '{}' has no screenshot test", scene.name);
            }
        }
    };
}

screenshot_tests!(
    default,
    colours,
    tabs,
    rename,
    scrollback,
    selection,
    alt_screen,
    colour_scheme,
    half_open,
    fractional_scale,
    resized,
);

#[test]
fn rendering_is_deterministic() {
    let a = (scenes::by_name("colours").unwrap().build)().render();
    let b = (scenes::by_name("colours").unwrap().build)().render();
    assert_eq!(a, b);
}

#[test]
fn a_changed_render_is_caught() {
    // The comparison has teeth: a one-colour change fails it.
    let base = (scenes::by_name("default").unwrap().build)().render();
    let mut values = scenes::test_values();
    values.background = wl_dropdown::color::Rgb::new(0x20, 0, 0);
    let mut stage = scenes::Stage::new(values, 1280, 480, 1.0);
    stage.feed(b"x");
    let changed = stage.render();
    let c = snapshot::compare(&changed, &base, Tolerance::default()).unwrap();
    assert!(c.differing_pixels > 1000, "{} pixels", c.differing_pixels);
}
