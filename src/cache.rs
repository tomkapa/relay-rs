//! Generic bounded-TTL cache.
//!
//! Cheap-clone by construction — the cache holds an internal `Arc`, so
//! cloning the handle costs one atomic increment and every clone shares
//! the same underlying state. Callers no longer need to wrap it in an
//! external `Arc<...>`.
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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::clock::SharedClock;

#[derive(Debug, Clone)]
struct Entry<V> {
    value: V,
    fetched_at: Instant,
}

/// Internal state held behind the [`BoundedTtlCache`]'s `Arc`. The
/// outer struct exists so we can derive `Clone` cheaply and so the
/// label / cap / ttl fields stay accessible without taking the lock.
#[derive(Debug)]
struct Inner<K, V> {
    state: Mutex<HashMap<K, Entry<V>>>,
    cap: usize,
    ttl: Duration,
    clock: SharedClock,
    label: &'static str,
}

/// Bounded TTL cache.
///
/// Cheap-clone — the handle itself is an `Arc<...>` so cloning shares
/// the underlying state. The value type must be `Clone`-cheap
/// (typically an `Arc<T>` or a reference-counted newtype) since
/// `get_or_load` returns owned values.
pub struct BoundedTtlCache<K, V> {
    inner: Arc<Inner<K, V>>,
}

impl<K, V> Clone for BoundedTtlCache<K, V> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
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
            inner: Arc::new(Inner {
                state: Mutex::new(HashMap::new()),
                cap,
                ttl,
                clock,
                label,
            }),
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
        let now = self.inner.clock.now();
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
        if now.saturating_duration_since(entry.fetched_at) >= self.inner.ttl {
            return None;
        }
        Some(entry.value.clone())
    }

    fn insert(&self, key: K, value: V, now: Instant) {
        let mut cache = self.lock();
        if cache.len() >= self.inner.cap && !cache.contains_key(&key) {
            evict_one(&mut cache, now, self.inner.ttl);
            assert!(
                cache.len() < self.inner.cap,
                "invariant: {} eviction made room",
                self.inner.label
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
            .state
            .lock()
            .unwrap_or_else(|_| panic!("invariant: {} mutex never poisoned", self.inner.label))
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
            .field("label", &self.inner.label)
            .field("cap", &self.inner.cap)
            .field("ttl", &self.inner.ttl)
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

    fn cache(cap: usize, ttl_secs: u64) -> (BoundedTtlCache<u64, u64>, Arc<TestClock>) {
        let clock = Arc::new(TestClock::new());
        let shared: SharedClock = clock.clone();
        let c = BoundedTtlCache::new(cap, Duration::from_secs(ttl_secs), shared, "test");
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

    #[tokio::test]
    async fn clones_share_state() {
        let (c, _clock) = cache(8, 60);
        let _: u64 = c
            .get_or_load(1u64, || async { Ok::<_, Infallible>(11u64) })
            .await
            .expect("ok");
        let cloned = c.clone();
        let v: u64 = cloned
            .get_or_load(1u64, || async { Ok::<_, Infallible>(22u64) })
            .await
            .expect("ok");
        // Same key resolved through the clone returns the original value
        // — proves the inner Arc state is shared.
        assert_eq!(v, 11u64);
    }
}
