//! The ordered list of terminal sessions and which one is active.
//!
//! Generic over the session type so the bookkeeping — which tab becomes
//! active when one closes, how moves and renames behave — is tested without
//! spawning shells.

pub trait TabSession {
    /// The title the program set (OSC 0/2), possibly empty.
    fn title(&self) -> &str;
    /// The name the user gave the tab; empty means follow the program's title.
    fn custom_title(&self) -> &str;
    fn set_custom_title(&mut self, title: String);
}

/// There is always at least one tab.
#[derive(Debug)]
pub struct Tabs<S> {
    sessions: Vec<S>,
    active: usize,
}

impl<S: TabSession> Tabs<S> {
    pub fn new(first: S) -> Self {
        Self {
            sessions: vec![first],
            active: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn active(&self) -> &S {
        &self.sessions[self.active]
    }

    pub fn active_mut(&mut self) -> &mut S {
        &mut self.sessions[self.active]
    }

    pub fn get(&self, index: usize) -> Option<&S> {
        self.sessions.get(index)
    }

    pub fn get_mut(&mut self, index: usize) -> Option<&mut S> {
        self.sessions.get_mut(index)
    }

    pub fn iter(&self) -> impl Iterator<Item = &S> {
        self.sessions.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut S> {
        self.sessions.iter_mut()
    }

    pub fn position(&self, pred: impl Fn(&S) -> bool) -> Option<usize> {
        self.sessions.iter().position(pred)
    }

    /// Returns whether the active tab changed.
    pub fn set_active(&mut self, index: usize) -> bool {
        if index >= self.sessions.len() || index == self.active {
            return false;
        }
        self.active = index;
        true
    }

    /// Appends a tab and makes it active; returns its index.
    pub fn push(&mut self, session: S) -> usize {
        self.sessions.push(session);
        self.active = self.sessions.len() - 1;
        self.active
    }

    /// Closes a tab, unless it is the last. Keeps the same tab active if one
    /// before it closed; if the active tab itself closed, its right neighbour
    /// takes its place (the left one, if it was last).
    pub fn close(&mut self, index: usize) -> Option<S> {
        if index >= self.sessions.len() || self.sessions.len() <= 1 {
            return None;
        }
        let removed = self.sessions.remove(index);
        if index < self.active || self.active >= self.sessions.len() {
            self.active -= 1;
        }
        Some(removed)
    }

    /// Replaces a tab's session in place, e.g. the last tab's exited shell.
    pub fn replace(&mut self, index: usize, session: S) -> Option<S> {
        let slot = self.sessions.get_mut(index)?;
        Some(std::mem::replace(slot, session))
    }

    /// Moves a tab; the active tab stays the same session wherever it lands.
    pub fn move_tab(&mut self, from: usize, to: usize) -> bool {
        let len = self.sessions.len();
        if from >= len || to >= len || from == to {
            return false;
        }
        let active_was = self.active;
        let s = self.sessions.remove(from);
        self.sessions.insert(to, s);
        self.active = if active_was == from {
            to
        } else if from < active_was && to >= active_was {
            active_was - 1
        } else if from > active_was && to <= active_was {
            active_was + 1
        } else {
            active_was
        };
        true
    }

    /// Switching wraps around.
    #[allow(clippy::should_implement_trait)]
    pub fn previous(&mut self) -> bool {
        let len = self.sessions.len();
        self.set_active((self.active + len - 1) % len)
    }

    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> bool {
        let len = self.sessions.len();
        self.set_active((self.active + 1) % len)
    }

    /// Moving stops at either end instead of wrapping: a tab that jumps from
    /// first to last is easy to lose track of.
    pub fn move_active_left(&mut self) -> bool {
        self.active > 0 && self.move_tab(self.active, self.active - 1)
    }

    pub fn move_active_right(&mut self) -> bool {
        self.move_tab(self.active, self.active + 1)
    }

    /// The tab's name without its number: the user's name if one was given,
    /// otherwise the program's title, otherwise "Shell".
    pub fn name(&self, index: usize) -> String {
        let Some(s) = self.sessions.get(index) else {
            return String::new();
        };
        if !s.custom_title().is_empty() {
            s.custom_title().to_owned()
        } else if !s.title().is_empty() {
            s.title().to_owned()
        } else {
            "Shell".to_owned()
        }
    }

    /// The label shown on the tab: "1: name".
    pub fn label(&self, index: usize) -> String {
        if index >= self.sessions.len() {
            return String::new();
        }
        format!("{}: {}", index + 1, self.name(index))
    }

    pub fn labels(&self) -> Vec<String> {
        (0..self.sessions.len()).map(|i| self.label(i)).collect()
    }

    /// An empty name drops the user's name and goes back to the program's
    /// title. The editor opens holding the current name, so confirming it
    /// unchanged must not pin the program's title in place of the live one.
    pub fn rename(&mut self, index: usize, name: &str) -> bool {
        let trimmed = name.trim();
        if index >= self.sessions.len() || trimmed == self.name(index) {
            return false;
        }
        self.sessions[index].set_custom_title(trimmed.to_owned());
        true
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[derive(Debug, Default, Clone, PartialEq)]
    pub struct Fake {
        pub id: u32,
        pub title: String,
        pub custom: String,
    }

    impl TabSession for Fake {
        fn title(&self) -> &str {
            &self.title
        }
        fn custom_title(&self) -> &str {
            &self.custom
        }
        fn set_custom_title(&mut self, title: String) {
            self.custom = title;
        }
    }

    fn fake(id: u32) -> Fake {
        Fake {
            id,
            ..Default::default()
        }
    }

    fn tabs(n: u32) -> Tabs<Fake> {
        let mut t = Tabs::new(fake(0));
        for i in 1..n {
            t.push(fake(i));
        }
        t
    }

    fn ids(t: &Tabs<Fake>) -> Vec<u32> {
        t.iter().map(|s| s.id).collect()
    }

    #[test]
    fn push_activates_the_new_tab() {
        let t = tabs(3);
        assert_eq!(t.len(), 3);
        assert_eq!(t.active_index(), 2);
    }

    #[test]
    fn closing_the_last_tab_is_refused() {
        let mut t = tabs(1);
        assert!(t.close(0).is_none());
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn closing_before_the_active_keeps_it_active() {
        let mut t = tabs(4);
        t.set_active(2);
        t.close(0);
        assert_eq!(t.active().id, 2);
    }

    #[test]
    fn closing_the_active_activates_its_right_neighbour_or_left_if_last() {
        let mut t = tabs(4);
        t.set_active(1);
        t.close(1);
        assert_eq!(t.active().id, 2);
        t.set_active(2); // the last one now
        t.close(2);
        assert_eq!(t.active().id, 2, "{:?}", ids(&t));
        assert_eq!(ids(&t), vec![0, 2]);
    }

    #[test]
    fn closing_after_the_active_changes_nothing() {
        let mut t = tabs(3);
        t.set_active(0);
        t.close(2);
        assert_eq!(t.active().id, 0);
    }

    #[test]
    fn moving_keeps_the_active_session() {
        let mut t = tabs(4); // active 3
        t.set_active(1);
        assert!(t.move_tab(0, 3));
        assert_eq!(ids(&t), vec![1, 2, 3, 0]);
        assert_eq!(t.active().id, 1);
        assert!(t.move_tab(3, 0));
        assert_eq!(t.active().id, 1);
        assert!(!t.move_tab(0, 0));
        assert!(!t.move_tab(0, 9));
    }

    #[test]
    fn switching_wraps_moving_does_not() {
        let mut t = tabs(3);
        t.set_active(0);
        t.previous();
        assert_eq!(t.active_index(), 2);
        t.next();
        assert_eq!(t.active_index(), 0);

        assert!(!t.move_active_left());
        assert!(t.move_active_right());
        assert_eq!(ids(&t), vec![1, 0, 2]);
        assert_eq!(t.active_index(), 1);
        t.set_active(2);
        assert!(!t.move_active_right());
    }

    #[test]
    fn single_tab_switching_is_a_no_op() {
        let mut t = tabs(1);
        assert!(!t.next());
        assert!(!t.previous());
    }

    #[test]
    fn names_and_labels() {
        let mut t = tabs(2);
        assert_eq!(t.label(0), "1: Shell");
        t.get_mut(1).unwrap().title = "~/src".into();
        assert_eq!(
            t.labels(),
            vec!["1: Shell".to_owned(), "2: ~/src".to_owned()]
        );
        assert_eq!(t.label(5), "");
    }

    #[test]
    fn rename_semantics() {
        let mut t = tabs(1);
        t.get_mut(0).unwrap().title = "vim".into();
        // Confirming the editor unchanged does not pin the live title.
        assert!(!t.rename(0, " vim "));
        assert_eq!(t.get(0).unwrap().custom, "");
        assert!(t.rename(0, "  build  "));
        assert_eq!(t.name(0), "build");
        t.get_mut(0).unwrap().title = "make".into();
        assert_eq!(t.name(0), "build", "a custom name wins over the title");
        assert!(t.rename(0, ""));
        assert_eq!(t.name(0), "make", "an empty name follows the title again");
        assert!(!t.rename(3, "x"));
    }
}
