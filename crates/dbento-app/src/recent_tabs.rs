//! The tabs in most-recently-used order, and the Ctrl+Tab walk through them.

/// Tab ids, most recent first, with the position of a walk in progress.
#[derive(Debug, Clone)]
pub struct Recent<T> {
    order: Vec<T>,
    walk: Option<usize>,
}

impl<T> Default for Recent<T> {
    fn default() -> Self {
        Recent {
            order: Vec::new(),
            walk: None,
        }
    }
}

impl<T: Copy + Eq> Recent<T> {
    /// Queues `id` as the least recent.
    pub fn add(&mut self, id: T) {
        if !self.order.contains(&id) {
            self.order.push(id);
        }
    }

    /// Ends any walk and makes `id` the most recent.
    pub fn touch(&mut self, id: T) {
        self.walk = None;
        self.order.retain(|o| *o != id);
        self.order.insert(0, id);
    }

    /// Ends any walk and forgets `id`.
    pub fn remove(&mut self, id: T) {
        self.walk = None;
        self.order.retain(|o| *o != id);
    }

    /// One walk step, back toward older tabs or forward toward newer ones, wrapping at either
    /// end. A walk starts on the most recent tab, so its first step back is the previous tab
    /// and its first step forward the oldest. With fewer than two tabs there is no walk.
    pub fn step(&mut self, back: bool) -> Option<T> {
        let n = self.order.len();
        if n < 2 {
            return None;
        }
        let at = self.walk.unwrap_or(0);
        let next = if back { (at + 1) % n } else { (at + n - 1) % n };
        self.walk = Some(next);
        Some(self.order[next])
    }

    /// Ends a walk, making the tab it stopped on the most recent; false with no walk.
    pub fn commit(&mut self) -> bool {
        let Some(at) = self.walk.take() else {
            return false;
        };
        let id = self.order.remove(at);
        self.order.insert(0, id);
        true
    }

    /// Whether a walk is in progress.
    pub fn walking(&self) -> bool {
        self.walk.is_some()
    }

    /// The ids, most recent first.
    pub fn order(&self) -> &[T] {
        &self.order
    }
}
