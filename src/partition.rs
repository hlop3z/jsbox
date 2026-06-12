//! Per-partition concurrency fairness (Tier 5 of `docs/design/resilience.md`).
//!
//! A **per-pod** backstop: the global bulkhead (`handler.rs`) protects the instance as a
//! whole, but sheds load indiscriminately — under overload a noisy partition's flood and
//! a well-behaved partition's single request hit the same semaphore, so the good one is
//! rejected too. This caps concurrency per partition key (whatever the operator/gateway
//! keys on — tenant, API key, route…) so a noisy key can't monopolize *this pod*.
//!
//! This is NOT a global guarantee. Across an N-replica fleet the effective ceiling is
//! per-pod × N, and global per-partition fairness belongs at the gateway (which has the
//! fleet-wide view). See `docs/design/resilience.md`.
//!
//! Partition state is bounded by construction: keys are hashed into a fixed array of
//! semaphores (stochastic fairness). No per-key lifecycle/eviction, constant memory —
//! and a single pod never has more than the bulkhead's worth of *concurrent* partitions,
//! so collisions stay rare regardless of how many partitions exist overall. The trade-off
//! is that two keys can share a bucket's quota — graceful, tunable by the bucket count.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash as _, Hasher as _};
use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Fixed array of per-partition concurrency buckets. A partition key hashes to one
/// bucket; a request must hold a bucket permit (plus the global bulkhead permit) to run.
#[derive(Debug, Clone)]
pub(crate) struct PartitionLimiter {
    /// One semaphore per bucket, each with `max_concurrent_per_partition` permits.
    buckets: Arc<[Arc<Semaphore>]>,
}

impl PartitionLimiter {
    /// Builds a limiter with `count` buckets of `per_partition` permits each. Returns
    /// `None` when disabled (`per_partition == 0`) so the caller can skip the gating.
    pub(crate) fn new(count: usize, per_partition: usize) -> Option<Self> {
        if per_partition == 0 || count == 0 {
            return None;
        }
        let buckets: Arc<[Arc<Semaphore>]> = (0..count)
            .map(|_idx| Arc::new(Semaphore::new(per_partition)))
            .collect();
        Some(Self { buckets })
    }

    /// Tries to take a permit for `partition`. `Some(permit)` to proceed, `None` when the
    /// partition's bucket is saturated (caller should reject with `PARTITION_OVERLOADED`).
    pub(crate) fn try_acquire(&self, partition: &str) -> Option<OwnedSemaphorePermit> {
        self.bucket_for(partition)
            .and_then(|bucket| Arc::clone(bucket).try_acquire_owned().ok())
    }

    /// Resolves a partition key to its bucket via a stable hash (lint-safe modulo + lookup).
    fn bucket_for(&self, partition: &str) -> Option<&Arc<Semaphore>> {
        let mut hasher = DefaultHasher::new();
        partition.hash(&mut hasher);
        let idx = usize::try_from(hasher.finish())
            .unwrap_or(0)
            .checked_rem(self.buckets.len())
            .unwrap_or(0);
        self.buckets.get(idx)
    }
}

#[cfg(test)]
mod tests {
    //! Bucket math and disabled-state behavior.

    use super::PartitionLimiter;

    /// `per_partition == 0` (or zero buckets) disables the limiter entirely.
    #[test]
    fn disabled_when_zero() {
        assert!(
            PartitionLimiter::new(256, 0).is_none(),
            "zero per-partition cap disables"
        );
        assert!(
            PartitionLimiter::new(0, 4).is_none(),
            "zero buckets disables"
        );
    }

    /// A partition can hold up to its cap, then is refused until a permit is released.
    #[test]
    fn caps_a_partition_then_refuses() {
        let limiter = PartitionLimiter::new(64, 2).unwrap_or_else(|| unreachable!());
        let p1 = limiter.try_acquire("acme");
        let p2 = limiter.try_acquire("acme");
        assert!(p1.is_some() && p2.is_some(), "first two acquire");
        assert!(
            limiter.try_acquire("acme").is_none(),
            "third refused at the cap"
        );
        drop(p1);
        assert!(
            limiter.try_acquire("acme").is_some(),
            "a freed permit is reusable"
        );
        drop(p2);
    }

    /// A saturated partition does not block a different one (assuming distinct buckets).
    #[test]
    fn one_partition_does_not_starve_another() {
        let limiter = PartitionLimiter::new(256, 1).unwrap_or_else(|| unreachable!());
        let _held = limiter.try_acquire("noisy");
        assert!(
            limiter.try_acquire("noisy").is_none(),
            "noisy is at its cap"
        );
        // `quiet` hashes to a different bucket with high probability at 256 buckets.
        assert!(
            limiter.try_acquire("quiet").is_some(),
            "a different partition still proceeds"
        );
    }
}
