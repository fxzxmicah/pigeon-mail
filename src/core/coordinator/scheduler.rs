use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::sync::{Arc, Condvar, Mutex};

pub(super) struct WriteLease {
    tracker: Arc<WriteTracker>,
}

pub(super) struct WriteTracker {
    count: Mutex<usize>,
    drained: Condvar,
    changed: Arc<dyn Fn() + Send + Sync>,
}

impl WriteTracker {
    pub(super) fn new(changed: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            count: Mutex::new(0),
            drained: Condvar::new(),
            changed: Arc::new(changed),
        }
    }

    pub(super) fn begin(self: &Arc<Self>) -> WriteLease {
        *self.count.lock().expect("write tracker lock poisoned") += 1;
        (self.changed)();
        WriteLease {
            tracker: Arc::clone(self),
        }
    }

    #[cfg(test)]
    pub(super) fn wait(&self) {
        let mut pending = self.count.lock().expect("write tracker lock poisoned");
        while *pending != 0 {
            pending = self
                .drained
                .wait(pending)
                .expect("write tracker lock poisoned");
        }
    }

    pub(super) fn count(&self) -> usize {
        *self.count.lock().expect("write tracker lock poisoned")
    }
}

impl Drop for WriteLease {
    fn drop(&mut self) {
        let mut pending = self
            .tracker
            .count
            .lock()
            .expect("write tracker lock poisoned");
        *pending = pending
            .checked_sub(1)
            .expect("write lease count must be balanced");
        if *pending == 0 {
            self.tracker.drained.notify_all();
        }
        drop(pending);
        (self.tracker.changed)();
    }
}

pub(super) struct LatestJobQueue<K, J> {
    entries: Mutex<HashMap<K, Option<J>>>,
}

impl<K, J> LatestJobQueue<K, J>
where
    K: Eq + Hash,
{
    pub(super) fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn begin(&self, key: K, job: J) -> Option<J> {
        let mut entries = self.entries.lock().expect("latest job queue lock poisoned");
        if let Some(pending) = entries.get_mut(&key) {
            *pending = Some(job);
            return None;
        }
        entries.insert(key, None);
        Some(job)
    }

    pub(super) fn begin_or_merge(
        &self,
        key: K,
        job: J,
        merge: impl FnOnce(&mut J, J),
    ) -> Option<J> {
        let mut entries = self.entries.lock().expect("latest job queue lock poisoned");
        if let Some(pending) = entries.get_mut(&key) {
            match pending {
                Some(existing) => merge(existing, job),
                None => *pending = Some(job),
            }
            return None;
        }
        entries.insert(key, None);
        Some(job)
    }

    pub(super) fn finish(&self, key: &K) -> Option<J> {
        let mut entries = self.entries.lock().expect("latest job queue lock poisoned");
        let next = entries.get_mut(key)?.take();
        if next.is_none() {
            entries.remove(key);
        }
        next
    }

    pub(super) fn cancel_pending(&self, key: &K) {
        if let Some(pending) = self
            .entries
            .lock()
            .expect("latest job queue lock poisoned")
            .get_mut(key)
        {
            *pending = None;
        }
    }

    pub(super) fn abort(&self, key: &K) {
        self.entries
            .lock()
            .expect("latest job queue lock poisoned")
            .remove(key);
    }

    #[cfg(test)]
    pub(super) fn contains_key(&self, key: &K) -> bool {
        self.entries
            .lock()
            .expect("latest job queue lock poisoned")
            .contains_key(key)
    }
}

pub(super) struct SerialJobQueue<K, J> {
    entries: Mutex<HashMap<K, VecDeque<J>>>,
}

impl<K, J> SerialJobQueue<K, J>
where
    K: Eq + Hash,
{
    pub(super) fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn begin(&self, key: K, job: J) -> Option<J> {
        let mut entries = self.entries.lock().expect("serial job queue lock poisoned");
        if let Some(pending) = entries.get_mut(&key) {
            pending.push_back(job);
            return None;
        }
        entries.insert(key, VecDeque::new());
        Some(job)
    }

    pub(super) fn finish(&self, key: &K) -> Option<J> {
        let mut entries = self.entries.lock().expect("serial job queue lock poisoned");
        let next = entries.get_mut(key)?.pop_front();
        if next.is_none() {
            entries.remove(key);
        }
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serial_queue_preserves_order_and_keeps_keys_independent() {
        let queue = SerialJobQueue::new();
        assert_eq!(queue.begin("first", 1), Some(1));
        assert_eq!(queue.begin("first", 2), None);
        assert_eq!(queue.begin("first", 3), None);
        assert_eq!(queue.begin("second", 4), Some(4));

        assert_eq!(queue.finish(&"first"), Some(2));
        assert_eq!(queue.finish(&"first"), Some(3));
        assert_eq!(queue.finish(&"first"), None);
        assert_eq!(queue.finish(&"second"), None);
        assert_eq!(queue.begin("first", 5), Some(5));
        assert_eq!(queue.finish(&"first"), None);
    }

    #[test]
    fn latest_queue_replaces_only_the_pending_job() {
        let queue = LatestJobQueue::new();
        assert_eq!(queue.begin("account", 1), Some(1));
        assert_eq!(queue.begin("account", 2), None);
        assert_eq!(queue.begin("account", 3), None);

        assert_eq!(queue.finish(&"account"), Some(3));
        assert_eq!(queue.finish(&"account"), None);
        assert_eq!(queue.begin("account", 4), Some(4));
        assert_eq!(queue.finish(&"account"), None);
    }
}
