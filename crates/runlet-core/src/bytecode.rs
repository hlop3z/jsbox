//! Compiled-`QuickJS`-bytecode cache — always on, autonomously size-gated.
//!
//! The engine re-parses and re-compiles a handler's source on **every** invocation (a fresh
//! `Context` per request — see `engine.rs`). For a hot script that is identical work repeated
//! thousands of times a second. This cache stores the `JS_WriteObject` bytecode keyed by a
//! collision-safe hash of the source, so later invocations skip the parse and `Module::load`
//! the bytecode straight into the fresh context instead.
//!
//! **Autonomous admission (by size).** Caching only pays off above a few KB of *source code*
//! (parse cost scales with code volume; below ~1 KB it is lost in the per-call context-create +
//! eval baseline). So the cache stores bytecode only for scripts at or above
//! [`DEFAULT_MIN_SOURCE_BYTES`]: tiny handlers compile fresh every time (no wasted memory, and
//! the `unsafe` load path is never exercised for them), large/bundled handlers are cached. The
//! decision needs no configuration — it is read from the source length the engine already has.
//!
//! **Keying.** The key is the SHA-256 of the source bytes (`sha2` is already a core
//! dependency). A 256-bit digest makes a key collision — which would silently load the *wrong*
//! bytecode — infeasible, unlike a 64-bit hash. The engine version is implicit: this is an
//! **in-process** cache, never read by a different `QuickJS` build, so no version needs to be
//! mixed in. That also bounds the `unsafe` in `Module::load`: the bytes are always self-produced
//! this process and never cross a trust or version boundary. Do **not** persist or share these
//! bytes across processes — that would break the invariant.
//!
//! **Bounding.** Backed by `moka` (`TinyLFU` eviction, size-bounded), because the distinct-script
//! set across many tenants is effectively unbounded; `TinyLFU` keeps the hot entries under churn.

use core::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use moka::sync::Cache;
use sha2::{Digest, Sha256};

/// Default source-size floor (bytes) at or above which a script's bytecode is cached.
///
/// Chosen from the measured declare-vs-load curve: below ~1 KB the saving is in the noise,
/// ~2 KB gives a clear net win, and it climbs steeply for bundled handlers. Tune by constructing
/// the cache with a different floor.
pub const DEFAULT_MIN_SOURCE_BYTES: usize = 2048;

/// SHA-256 digest of `(namespace, source)` — the cache key.
pub(crate) type SourceHash = [u8; 32];

/// Computes the cache key for a script source under a partition `namespace` (empty = global).
///
/// The namespace is **length-prefixed** before the source so the hash is injective — two
/// different `(namespace, source)` splits can never collide (e.g. `("ab","c")` vs `("a","bc")`).
/// Namespacing by the caller's partition key means two tenants who submit byte-identical source
/// get **separate** cache entries: no cross-tenant dedup, and so no compile-timing side channel
/// revealing that another tenant ran the same script. Per-tenant reuse — the actual hot path —
/// is unaffected.
pub(crate) fn digest(namespace: &[u8], source: &[u8]) -> SourceHash {
    let ns_len = u64::try_from(namespace.len()).unwrap_or(u64::MAX);
    let mut hasher = Sha256::new();
    hasher.update(ns_len.to_le_bytes());
    hasher.update(namespace);
    hasher.update(source);
    hasher.finalize().into()
}

/// A point-in-time snapshot of cache activity, for observability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BytecodeCacheStats {
    /// Approximate number of cached scripts currently resident.
    pub entries: u64,
    /// Loads served from cached bytecode (the parse was skipped).
    pub hits: u64,
    /// Lookups that missed and had to compile from source.
    pub misses: u64,
    /// Misses whose freshly-compiled bytecode was admitted (passed the size gate).
    pub stored: u64,
}

/// Shared inner state — held behind an `Arc` so every [`BytecodeCache`] clone (the pool hands
/// out references via cloned `JsPool`s) shares the same entries and counters.
struct Inner {
    /// Source-hash → serialized module bytecode.
    cache: Cache<SourceHash, Arc<[u8]>>,
    /// Source-size floor (bytes) for admission — see [`BytecodeCache::should_store`].
    min_source_bytes: usize,
    /// Loads served from cache.
    hits: AtomicU64,
    /// Lookups that had to compile.
    misses: AtomicU64,
    /// Freshly-compiled entries admitted to the cache.
    stored: AtomicU64,
}

/// A bounded, concurrent cache of compiled `QuickJS` module bytecode.
///
/// Cheap to [`Clone`] (the inner state is `Arc`-shared), so the runtime pool can hold one and
/// hand a reference to every execution. Values are `Arc<[u8]>` so a cache hit clones a pointer,
/// not the bytecode.
#[derive(Clone)]
pub struct BytecodeCache {
    /// Shared cache + admission floor + counters.
    inner: Arc<Inner>,
}

impl BytecodeCache {
    /// Builds a cache holding at most `max_capacity` distinct compiled scripts, admitting only
    /// sources of at least `min_source_bytes` (use `0` to cache everything — e.g. in tests).
    #[must_use]
    pub(crate) fn new(max_capacity: u64, min_source_bytes: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                cache: Cache::new(max_capacity),
                min_source_bytes,
                hits: AtomicU64::new(0),
                misses: AtomicU64::new(0),
                stored: AtomicU64::new(0),
            }),
        }
    }

    /// Whether a source of `source_len` bytes is large enough to be worth caching (the
    /// autonomous, size-based admission decision).
    #[must_use]
    pub(crate) fn should_store(&self, source_len: usize) -> bool {
        source_len >= self.inner.min_source_bytes
    }

    /// The cached bytecode for a source hash, if present (clones an `Arc`, not the bytes).
    pub(crate) fn get(&self, key: &SourceHash) -> Option<Arc<[u8]>> {
        self.inner.cache.get(key)
    }

    /// Stores freshly-compiled bytecode under its source hash (also counts it as admitted).
    pub(crate) fn insert(&self, key: SourceHash, bytecode: Arc<[u8]>) {
        let _ = self.inner.stored.fetch_add(1, Ordering::Relaxed);
        self.inner.cache.insert(key, bytecode);
    }

    /// Records a load served from cache.
    pub(crate) fn note_hit(&self) {
        let _ = self.inner.hits.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a lookup that had to compile from source.
    pub(crate) fn note_miss(&self) {
        let _ = self.inner.misses.fetch_add(1, Ordering::Relaxed);
    }

    /// A snapshot of current cache activity (entries + hit/miss/store counters).
    #[must_use]
    pub fn stats(&self) -> BytecodeCacheStats {
        self.inner.cache.run_pending_tasks();
        BytecodeCacheStats {
            entries: self.inner.cache.entry_count(),
            hits: self.inner.hits.load(Ordering::Relaxed),
            misses: self.inner.misses.load(Ordering::Relaxed),
            stored: self.inner.stored.load(Ordering::Relaxed),
        }
    }
}

impl fmt::Debug for BytecodeCache {
    #[expect(
        clippy::renamed_function_params,
        reason = "descriptive name over the trait's terse `f`, matching the crate's min-ident lint"
    )]
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BytecodeCache")
            .field("min_source_bytes", &self.inner.min_source_bytes)
            .field("stats", &self.stats())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    //! Digest stability, the size-gate decision, and a cache round-trip with counters.

    use super::{BytecodeCache, digest};
    use std::sync::Arc;

    /// The same `(namespace, source)` hashes stably; differing source or namespace diverges,
    /// and the length-prefix makes the namespace/source split unambiguous.
    #[test]
    fn digest_is_stable_namespaced_and_injective() {
        assert_eq!(digest(b"", b"handler"), digest(b"", b"handler"), "stable");
        assert_ne!(
            digest(b"", b"handler(a)"),
            digest(b"", b"handler(b)"),
            "distinct source ⇒ distinct key"
        );
        // Same source, different tenant namespace ⇒ separate cache entries.
        assert_ne!(
            digest(b"tenant-a", b"handler"),
            digest(b"tenant-b", b"handler"),
            "namespace partitions the key"
        );
        // Injective split: ("ab","c") and ("a","bc") must not collide.
        assert_ne!(
            digest(b"ab", b"c"),
            digest(b"a", b"bc"),
            "length-prefix prevents split collisions"
        );
    }

    /// The size gate admits sources at or above the floor and rejects smaller ones.
    #[test]
    fn size_gate_admits_only_large_enough_sources() {
        let cache = BytecodeCache::new(8, 1024);
        assert!(!cache.should_store(512), "below the floor is rejected");
        assert!(cache.should_store(1024), "at the floor is admitted");
        assert!(cache.should_store(4096), "above the floor is admitted");
    }

    /// A stored entry reads back and bumps `stored`; a miss is `None`.
    #[test]
    fn round_trips_and_counts() {
        let cache = BytecodeCache::new(8, 0);
        let key = digest(b"", b"export default () => 1");
        assert!(cache.get(&key).is_none(), "cold lookup misses");
        cache.insert(key, Arc::from(vec![1_u8, 2, 3].into_boxed_slice()));
        assert_eq!(
            cache.get(&key).as_deref(),
            Some([1_u8, 2, 3].as_slice()),
            "stored bytecode reads back"
        );
        assert_eq!(cache.stats().stored, 1, "insert counts as a store");
    }
}
