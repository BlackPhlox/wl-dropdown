//! Window lifecycle: whether the dropdown is open, and when to unmap it.
//!
//! Hiding is deferred so the close animation can play: `request_hide` clears
//! `opened` (the panel animates back up) and the surface is unmapped once the
//! animation's duration has elapsed. The delay is owned here rather than
//! driven by a callback from the animation, because a callback only fires if
//! the animation actually runs — and a missed one would leave the dropdown
//! stuck open. Timing it here has one code path and cannot get stuck.

use std::time::{Duration, Instant};

/// Extra time so the last animation frame is presented before the surface
/// goes away.
pub const HIDE_MARGIN: Duration = Duration::from_millis(30);

#[derive(Debug, Default)]
pub struct Controller {
    /// Intent, not visibility: true from the moment we start opening until
    /// the moment we start closing. The panel animates on this.
    opened: bool,
    /// Whether the surface should exist.
    mapped: bool,
    hide_at: Option<Instant>,
}

impl Controller {
    pub fn opened(&self) -> bool {
        self.opened
    }

    pub fn mapped(&self) -> bool {
        self.mapped
    }

    /// When the pending unmap is due.
    pub fn deadline(&self) -> Option<Instant> {
        self.hide_at
    }

    pub fn show(&mut self) {
        self.hide_at = None; // reopening mid-close must not be unmapped later
        self.mapped = true;
        self.opened = true;
    }

    pub fn request_hide(&mut self, now: Instant, animation_ms: i32) {
        if !self.opened {
            self.hide_at = None;
            self.mapped = false;
            return;
        }
        self.opened = false;
        self.hide_at = Some(now + Duration::from_millis(animation_ms.max(0) as u64) + HIDE_MARGIN);
    }

    /// Visibility is not consulted: `opened` is the authority, and the two
    /// differ for as long as the close animation is playing.
    pub fn toggle(&mut self, now: Instant, animation_ms: i32) {
        if self.opened {
            self.request_hide(now, animation_ms);
        } else {
            self.show();
        }
    }

    /// The compositor took the surface away (an output vanished, a config
    /// reload). It can be shown again later.
    pub fn surface_lost(&mut self) {
        self.opened = false;
        self.mapped = false;
        self.hide_at = None;
    }

    /// Advances time; unmaps once the close animation is over.
    pub fn tick(&mut self, now: Instant) {
        if self.hide_at.is_some_and(|t| t <= now) {
            self.hide_at = None;
            self.mapped = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_opens_then_closes_after_animation() {
        let t0 = Instant::now();
        let mut c = Controller::default();
        c.toggle(t0, 180);
        assert!(c.opened() && c.mapped());

        c.toggle(t0, 180);
        assert!(!c.opened());
        assert!(c.mapped(), "stays mapped while the close animation plays");
        c.tick(t0 + Duration::from_millis(209));
        assert!(c.mapped());
        c.tick(t0 + Duration::from_millis(210));
        assert!(!c.mapped());
    }

    #[test]
    fn reopening_mid_close_cancels_the_unmap() {
        let t0 = Instant::now();
        let mut c = Controller::default();
        c.show();
        c.request_hide(t0, 100);
        c.show();
        c.tick(t0 + Duration::from_secs(5));
        assert!(c.opened() && c.mapped());
    }

    #[test]
    fn hiding_when_closed_unmaps_at_once() {
        let t0 = Instant::now();
        let mut c = Controller::default();
        c.show();
        c.request_hide(t0, 100);
        // A second hide during the animation: already closing, unmap now.
        c.request_hide(t0, 100);
        assert!(!c.mapped());
        assert_eq!(c.deadline(), None);
    }

    #[test]
    fn zero_animation_still_waits_for_the_margin() {
        let t0 = Instant::now();
        let mut c = Controller::default();
        c.show();
        c.request_hide(t0, 0);
        assert_eq!(c.deadline(), Some(t0 + HIDE_MARGIN));
    }

    #[test]
    fn surface_lost_resets() {
        let mut c = Controller::default();
        c.show();
        c.surface_lost();
        assert!(!c.opened() && !c.mapped());
        c.toggle(Instant::now(), 10);
        assert!(c.opened(), "a lost surface can be shown again");
    }
}
