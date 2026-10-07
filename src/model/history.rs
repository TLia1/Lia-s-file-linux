//! Back/forward navigation history, independent of GTK.

#[derive(Debug, Clone)]
pub struct History<T> {
    back: Vec<T>,
    forward: Vec<T>,
}

impl<T> Default for History<T> {
    fn default() -> Self {
        Self {
            back: Vec::new(),
            forward: Vec::new(),
        }
    }
}

impl<T> History<T> {
    /// Records that we're leaving `current` for a new location.
    /// Clears the forward stack, like a browser.
    pub fn visit(&mut self, current: T) {
        self.back.push(current);
        self.forward.clear();
    }

    /// Returns the previous location, remembering `current` for "forward".
    pub fn go_back(&mut self, current: T) -> Option<T> {
        let previous = self.back.pop()?;
        self.forward.push(current);
        Some(previous)
    }

    /// Returns the next location, remembering `current` for "back".
    pub fn go_forward(&mut self, current: T) -> Option<T> {
        let next = self.forward.pop()?;
        self.back.push(current);
        Some(next)
    }

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn back_and_forward() {
        let mut h = History::default();
        h.visit("a");
        h.visit("b");
        // now at "c"
        assert_eq!(h.go_back("c"), Some("b"));
        assert_eq!(h.go_back("b"), Some("a"));
        assert_eq!(h.go_back("a"), None);
        assert_eq!(h.go_forward("a"), Some("b"));
        assert_eq!(h.go_forward("b"), Some("c"));
        assert_eq!(h.go_forward("c"), None);
        assert!(h.can_go_back());
        assert!(!h.can_go_forward());
    }

    #[test]
    fn visiting_clears_forward() {
        let mut h = History::default();
        h.visit("a");
        assert_eq!(h.go_back("b"), Some("a"));
        assert!(h.can_go_forward());
        h.visit("a");
        assert!(!h.can_go_forward());
        assert_eq!(h.go_back("d"), Some("a"));
    }
}
