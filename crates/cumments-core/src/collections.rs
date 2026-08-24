//! Small bounded in-memory collections for caller-controlled keys.

use std::collections::{HashMap, VecDeque};

/// A hard-capped string-keyed map that evicts its least-recently-used value.
///
/// This is intended for UX preferences rather than authoritative state: an
/// eviction is harmless because callers retain a fallback lookup.
pub struct BoundedLruMap<V> {
    capacity: usize,
    values: HashMap<String, V>,
    order: VecDeque<String>,
}

impl<V> BoundedLruMap<V> {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            values: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// Returns the value and marks the key as most recently used.
    pub fn get(&mut self, key: &str) -> Option<&V> {
        if self.values.contains_key(key) {
            self.touch(key);
        }
        self.values.get(key)
    }

    /// Inserts a value, evicting the least-recently-used key at capacity.
    ///
    /// Unlike `HashMap::clear`, insertion at capacity affects only one entry;
    /// every other caller keeps its preference.
    pub fn put(&mut self, key: impl Into<String>, value: V) {
        let key = key.into();
        if !self.values.contains_key(&key) && self.values.len() >= self.capacity {
            while let Some(oldest) = self.order.pop_front() {
                if self.values.remove(&oldest).is_some() {
                    break;
                }
            }
        }
        if self.values.insert(key.clone(), value).is_none() {
            self.order.push_back(key);
        } else {
            self.touch(&key);
        }
    }

    fn touch(&mut self, key: &str) {
        if let Some(index) = self.order.iter().position(|entry| entry == key)
            && let Some(entry) = self.order.remove(index)
        {
            self.order.push_back(entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_refreshes_recency_before_eviction() {
        let mut cache = BoundedLruMap::new(2);
        cache.put("alice", "site-a");
        cache.put("bob", "site-b");
        assert_eq!(cache.get("alice"), Some(&"site-a"));

        cache.put("carol", "site-c");
        assert_eq!(cache.get("bob"), None);
        assert_eq!(cache.get("alice"), Some(&"site-a"));
        assert_eq!(cache.get("carol"), Some(&"site-c"));
    }

    #[test]
    fn replacing_an_existing_key_does_not_evict() {
        let mut cache = BoundedLruMap::new(2);
        cache.put("alice", "site-a");
        cache.put("bob", "site-b");
        cache.put("bob", "site-c");
        cache.put("carol", "site-d");

        assert_eq!(cache.get("alice"), None);
        assert_eq!(cache.get("bob"), Some(&"site-c"));
        assert_eq!(cache.get("carol"), Some(&"site-d"));
    }
}
