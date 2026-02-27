use std::time::{Duration, Instant};

const DEFAULT_STABILIZE_DELAY: Duration = Duration::from_millis(300);

/// Debounce filter for screen snapshots.
/// Waits for the screen to be stable (unchanged) for `delay` before committing.
pub struct Stabilizer {
    delay: Duration,
    pending_snapshot: Option<Vec<String>>,
    pending_since: Option<Instant>,
}

impl Stabilizer {
    pub fn new(delay: Duration) -> Self {
        Stabilizer {
            delay,
            pending_snapshot: None,
            pending_since: None,
        }
    }

    pub fn default_delay() -> Self {
        Self::new(DEFAULT_STABILIZE_DELAY)
    }

    /// Called when the screen changes. Resets the stability timer.
    pub fn on_change(&mut self, snapshot: Vec<String>) {
        if self.pending_snapshot.as_ref() != Some(&snapshot) {
            self.pending_snapshot = Some(snapshot);
            self.pending_since = Some(Instant::now());
        }
    }

    /// Called periodically (e.g., every 50ms). Returns the committed snapshot
    /// if the screen has been stable for the configured delay.
    pub fn try_commit(&mut self) -> Option<Vec<String>> {
        if let (Some(pending), Some(since)) = (&self.pending_snapshot, self.pending_since) {
            if since.elapsed() >= self.delay {
                let committed = pending.clone();
                self.pending_snapshot = None;
                self.pending_since = None;
                return Some(committed);
            }
        }
        None
    }

    /// Returns the pending snapshot without committing, for instant state detection.
    #[allow(dead_code)]
    pub fn pending(&self) -> Option<&Vec<String>> {
        self.pending_snapshot.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_stabilizer_no_commit_before_delay() {
        let mut stabilizer = Stabilizer::new(Duration::from_millis(100));
        stabilizer.on_change(vec!["frame1".into()]);
        assert!(stabilizer.try_commit().is_none());
    }

    #[test]
    fn test_stabilizer_commits_after_delay() {
        let mut stabilizer = Stabilizer::new(Duration::from_millis(50));
        stabilizer.on_change(vec!["stable content".into()]);
        thread::sleep(Duration::from_millis(80));
        let committed = stabilizer.try_commit();
        assert_eq!(committed, Some(vec!["stable content".to_string()]));
    }

    #[test]
    fn test_stabilizer_resets_on_change() {
        let mut stabilizer = Stabilizer::new(Duration::from_millis(100));
        stabilizer.on_change(vec!["frame1".into()]);
        thread::sleep(Duration::from_millis(60));
        stabilizer.on_change(vec!["frame2".into()]);
        thread::sleep(Duration::from_millis(60));
        // Only 60ms since last change, not yet stable
        assert!(stabilizer.try_commit().is_none());
        thread::sleep(Duration::from_millis(60));
        let committed = stabilizer.try_commit();
        assert_eq!(committed, Some(vec!["frame2".to_string()]));
    }

    #[test]
    fn test_stabilizer_no_double_commit() {
        let mut stabilizer = Stabilizer::new(Duration::from_millis(50));
        stabilizer.on_change(vec!["content".into()]);
        thread::sleep(Duration::from_millis(80));
        assert!(stabilizer.try_commit().is_some());
        assert!(stabilizer.try_commit().is_none());
    }

    #[test]
    fn test_stabilizer_pending() {
        let mut stabilizer = Stabilizer::new(Duration::from_millis(100));
        assert!(stabilizer.pending().is_none());
        stabilizer.on_change(vec!["pending".into()]);
        assert_eq!(
            stabilizer.pending(),
            Some(&vec!["pending".to_string()])
        );
    }
}
