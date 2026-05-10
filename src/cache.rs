//! Generic bounded-TTL cache.
//!
//! Hot path: one mutex round-trip + one HashMap lookup. On miss the lock
//! is released, the supplied loader runs without contention, then the
//! lock is re-acquired to insert. Cache size is bounded; eviction
//! prefers the oldest expired entry, falling back to the absolute
//! oldest. Two concurrent loaders for the same key may both run — the
//! second insert wins; this is acceptable here because every consumer
//! either serialises per-key already (the agent worker pool holds one
//! lease per session, and the agent registry has a single owner) or
//! treats the redundant compute as free.
//!
//! Hand-rolled rather than pulling in `moka` / `lru` (CLAUDE.md §8 — zero-dep
//! bias). The cache is the only one in the binary; reaching for a crate
//! would be premature.

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::hash::Hash;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::clock::SharedClock;

#[derive(Debug, Clone)]
struct Entry<V> {
    value: V,
    fetched_at: Instant,
}

/// Bounded TTL cache. Cheap-clone is not supported on purpose — share
/// via `Arc<BoundedTtlCache<...>>` if multiple owners are needed.
///
/// The value type must be `Clone`-cheap (typically an `Arc<T>` or a
/// reference-counted newtype) since `get_or_load` returns owned values.
pub struct BoundedTtlCache<K, V> {
    inner: Mutex<HashMap<K, Entry<V>>>,
    cap: usize,
    ttl: Duration,
    clock: SharedClock,
    label: &'static str,
}

impl<K, V> BoundedTtlCache<K, V>
where
    K: Eq + Hash + Copy,
    V: Clone,
{
    /// Build a cache with `cap` entries and `ttl` lifetime per entry.
    /// `label` rides on assertion messages so a poisoned-mutex panic
    /// names the offending cache.
    #[must_use]
    pub fn new(cap: usize, ttl: Duration, clock: SharedClock, label: &'static str) -> Self {
        // §6: zero-cap or zero-TTL would silently disable the cache.
        assert!(cap > 0, "invariant: {label} cap must be > 0");
        assert!(!ttl.is_zero(), "invariant: {label} ttl must be > 0");
        Self {
            inner: Mutex::new(HashMap::new()),
            cap,
            ttl,
            clock,
            label,
        }
    }

    /// Return the cached value for `key`, calling `load` to produce one
    /// on miss or expiry. The lock is released before `load` runs so a
    /// slow loader does not block other workers.
    pub async fn get_or_load<F, Fut, E>(&self, key: K, load: F) -> Result<V, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<V, E>>,
    {
        let now = self.clock.now();
        if let Some(value) = self.lookup_fresh(key, now) {
            return Ok(value);
        }
        let value = load().await?;
        self.insert(key, value.clone(), now);
        Ok(value)
    }

    fn lookup_fresh(&self, key: K, now: Instant) -> Option<V> {
        let cache = self.lock();
        let entry = cache.get(&key)?;
        if now.saturating_duration_since(entry.fetched_at) >= self.ttl {
            return None;
        }
        Some(entry.value.clone())
    }

    fn insert(&self, key: K, value: V, now: Instant) {
        let mut cache = self.lock();
        if cache.len() >= self.cap && !cache.contains_key(&key) {
            evict_one(&mut cache, now, self.ttl);
            assert!(
                cache.len() < self.cap,
                "invariant: {} eviction made room",
                self.label
            );
        }
        cache.insert(
            key,
            Entry {
                value,
                fetched_at: now,
            },
        );
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<K, Entry<V>>> {
        self.inner
            .lock()
            .unwrap_or_else(|_| panic!("invariant: {} mutex never poisoned", self.label))
    }

    /// Test/inspection: number of entries currently held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }
}

impl<K, V> fmt::Debug for BoundedTtlCache<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BoundedTtlCache")
            .field("label", &self.label)
            .field("cap", &self.cap)
            .field("ttl", &self.ttl)
            .finish_non_exhaustive()
    }
}

/// Evict the oldest expired entry, falling back to the absolute oldest
/// when nothing has expired. Bounded scan over `cap` entries.
fn evict_one<K, V>(cache: &mut HashMap<K, Entry<V>>, now: Instant, ttl: Duration)
where
    K: Eq + Hash + Copy,
{
    let mut oldest_expired: Option<K> = None;
    let mut oldest_overall: Option<(K, Instant)> = None;
    for (k, entry) in cache.iter() {
        if now.saturating_duration_since(entry.fetched_at) >= ttl {
            oldest_expired = Some(*k);
            break;
        }
        match oldest_overall {
            None => oldest_overall = Some((*k, entry.fetched_at)),
            Some((_, ts)) if entry.fetched_at < ts => {
                oldest_overall = Some((*k, entry.fetched_at));
            }
            _ => {}
        }
    }
    let victim = oldest_expired.or_else(|| oldest_overall.map(|(k, _)| k));
    if let Some(k) = victim {
        cache.remove(&k);
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::clock::TestClock;

    use super::*;

    fn cache(cap: usize, ttl_secs: u64) -> (Arc<BoundedTtlCache<u64, u64>>, Arc<TestClock>) {
        let clock = Arc::new(TestClock::new());
        let shared: SharedClock = clock.clone();
        let c = Arc::new(BoundedTtlCache::new(
            cap,
            Duration::from_secs(ttl_secs),
            shared,
            "test",
        ));
        (c, clock)
    }

    #[tokio::test]
    async fn get_or_load_runs_loader_once_within_ttl() {
        let (c, _clock) = cache(8, 60);
        let calls = Arc::new(AtomicUsize::new(0));
        for _ in 0..3 {
            let calls = calls.clone();
            let _: u64 = c
                .get_or_load(7u64, || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, Infallible>(42u64)
                })
                .await
                .expect("ok");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn ttl_expiry_re_runs_loader() {
        let (c, clock) = cache(8, 60);
        let calls = Arc::new(AtomicUsize::new(0));
        for _ in 0..2 {
            let calls = calls.clone();
            let _: u64 = c
                .get_or_load(7u64, || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, Infallible>(42u64)
                })
                .await
                .expect("ok");
            clock.advance(Duration::from_secs(61));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn cap_evicts_when_full() {
        let (c, _clock) = cache(2, 60);
        for k in 0..3u64 {
            let _: u64 = c
                .get_or_load(k, || async move { Ok::<_, Infallible>(k) })
                .await
                .expect("ok");
        }
        assert!(c.len() <= 2, "stayed under cap, got {}", c.len());
    }

    #[derive(Debug, PartialEq)]
    struct Boom;

    #[tokio::test]
    async fn loader_error_is_not_cached() {
        let (c, _clock) = cache(2, 60);
        let first: Result<u64, Boom> = c.get_or_load(1u64, || async { Err(Boom) }).await;
        assert!(first.is_err());

        let second: Result<u64, Boom> =
            c.get_or_load(1u64, || async { Ok::<_, Boom>(99u64) }).await;
        assert_eq!(second, Ok(99u64));
    }
}
