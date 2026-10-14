//! A small, thread-safe, sharded LRU cache.
//!
//! Used for decoded evidence structures that are expensive to rebuild and
//! accessed from many ingest threads (decompressed image chunks, MFT
//! records, directory indexes). Sharding keeps lock contention low; each
//! shard is a tiny move-to-front list, which beats a hash map at the small
//! per-shard capacities used here.

use std::collections::VecDeque;
use std::hash::{BuildHasher, Hash};
use std::sync::Mutex;

/// Sharded LRU cache mapping `K` to cheaply clonable values (`Arc<…>`).
#[derive(Debug)]
pub struct ShardedLru<K, V> {
    shards: Vec<Mutex<VecDeque<(K, V)>>>,
    per_shard: usize,
    hasher: std::hash::RandomState,
}

impl<K: Hash + Eq + Clone, V: Clone> ShardedLru<K, V> {
    /// Creates a cache with `shards` shards of `per_shard` entries each.
    pub fn new(shards: usize, per_shard: usize) -> Self {
        let shards = shards.max(1);
        Self {
            shards: (0..shards)
                .map(|_| Mutex::new(VecDeque::with_capacity(per_shard)))
                .collect(),
            per_shard: per_shard.max(1),
            hasher: std::hash::RandomState::new(),
        }
    }

    fn shard(&self, key: &K) -> Option<&Mutex<VecDeque<(K, V)>>> {
        let idx = (self.hasher.hash_one(key) % self.shards.len() as u64) as usize;
        self.shards.get(idx)
    }

    /// Returns a cached value, marking it most recently used.
    pub fn get(&self, key: &K) -> Option<V> {
        let mut shard = self
            .shard(key)?
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let pos = shard.iter().position(|(k, _)| k == key)?;
        let entry = shard.remove(pos)?;
        let value = entry.1.clone();
        shard.push_front(entry);
        Some(value)
    }

    /// Inserts a value, evicting the least recently used entry of its shard.
    pub fn insert(&self, key: K, value: V) {
        let Some(shard) = self.shard(&key) else {
            return;
        };
        let mut shard = shard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(pos) = shard.iter().position(|(k, _)| *k == key) {
            shard.remove(pos);
        }
        if shard.len() >= self.per_shard {
            shard.pop_back();
        }
        shard.push_front((key, value));
    }

    /// Returns the cached value or computes, caches and returns it.
    ///
    /// The computation runs without holding a lock, so concurrent misses for
    /// the same key may compute twice; the result is identical either way.
    pub fn get_or_try_insert<E>(&self, key: &K, f: impl FnOnce() -> Result<V, E>) -> Result<V, E> {
        if let Some(v) = self.get(key) {
            return Ok(v);
        }
        let v = f()?;
        self.insert(key.clone(), v.clone());
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn evicts_least_recently_used() {
        let c = ShardedLru::new(1, 2);
        c.insert(1, "a");
        c.insert(2, "b");
        assert_eq!(c.get(&1), Some("a")); // 1 is now most recent
        c.insert(3, "c"); // evicts 2
        assert_eq!(c.get(&2), None);
        assert_eq!(c.get(&1), Some("a"));
        assert_eq!(c.get(&3), Some("c"));
    }

    #[test]
    fn reinsert_replaces() {
        let c = ShardedLru::new(4, 4);
        c.insert("k", 1);
        c.insert("k", 2);
        assert_eq!(c.get(&"k"), Some(2));
    }

    #[test]
    fn get_or_try_insert_computes_once_when_cached() {
        let c: ShardedLru<u32, Arc<str>> = ShardedLru::new(8, 8);
        let calls = AtomicUsize::new(0);
        for _ in 0..3 {
            let v = c
                .get_or_try_insert(&7, || {
                    calls.fetch_add(1, Ordering::Relaxed);
                    Ok::<_, ()>(Arc::from("seven"))
                })
                .unwrap();
            assert_eq!(&*v, "seven");
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(
            c.get_or_try_insert(&8, || Err::<Arc<str>, _>("boom"))
                .is_err()
        );
        assert_eq!(c.get(&8), None);
    }

    #[test]
    fn concurrent_use() {
        let c = Arc::new(ShardedLru::new(16, 4));
        std::thread::scope(|s| {
            for t in 0..8 {
                let c = Arc::clone(&c);
                s.spawn(move || {
                    for i in 0..1000u64 {
                        c.insert(i % 97, t);
                        let _ = c.get(&(i % 89));
                    }
                });
            }
        });
    }
}
