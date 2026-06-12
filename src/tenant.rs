//! Per-tenant concurrency fairness (Tier 5 of `docs/design/resilience.md`).
//!
//! The global bulkhead (`handler.rs`) protects the system as a whole, but sheds load
//! indiscriminately: under overload a noisy tenant's flood and a well-behaved tenant's
//! single request hit the same semaphore, so the good tenant is rejected too. This adds
//! a per-tenant concurrency cap *underneath* the global one — a tenant that exhausts its
//! own share fast-fails while global capacity stays available for others.
//!
//! Tenant state is bounded by construction: tenants are hashed into a fixed array of
//! semaphores (stochastic fairness). No per-tenant lifecycle/eviction, constant memory.
//! The trade-off is that two tenants can hash to the same bucket and share its quota —
//! graceful degradation, tunable by the bucket count.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash as _, Hasher as _};
use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Fixed array of per-tenant concurrency buckets. A tenant id hashes to one bucket; a
/// request must hold a bucket permit (in addition to the global bulkhead permit) to run.
#[derive(Debug, Clone)]
pub(crate) struct TenantLimiter {
    /// One semaphore per bucket, each with `max_concurrent_per_tenant` permits.
    buckets: Arc<[Arc<Semaphore>]>,
}

impl TenantLimiter {
    /// Builds a limiter with `buckets` buckets of `per_tenant` permits each. Returns
    /// `None` when disabled (`per_tenant == 0`) so the caller can skip per-tenant gating.
    pub(crate) fn new(count: usize, per_tenant: usize) -> Option<Self> {
        if per_tenant == 0 || count == 0 {
            return None;
        }
        let buckets: Arc<[Arc<Semaphore>]> = (0..count)
            .map(|_idx| Arc::new(Semaphore::new(per_tenant)))
            .collect();
        Some(Self { buckets })
    }

    /// Tries to take a permit for `tenant`. `Some(permit)` to proceed, `None` when the
    /// tenant's bucket is saturated (caller should reject with `TENANT_OVERLOADED`).
    pub(crate) fn try_acquire(&self, tenant: &str) -> Option<OwnedSemaphorePermit> {
        self.bucket_for(tenant)
            .and_then(|bucket| Arc::clone(bucket).try_acquire_owned().ok())
    }

    /// Resolves a tenant id to its bucket via a stable hash (lint-safe modulo + lookup).
    fn bucket_for(&self, tenant: &str) -> Option<&Arc<Semaphore>> {
        let mut hasher = DefaultHasher::new();
        tenant.hash(&mut hasher);
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

    use super::TenantLimiter;

    /// `per_tenant == 0` (or zero buckets) disables the limiter entirely.
    #[test]
    fn disabled_when_zero() {
        assert!(
            TenantLimiter::new(256, 0).is_none(),
            "zero per-tenant cap disables"
        );
        assert!(TenantLimiter::new(0, 4).is_none(), "zero buckets disables");
    }

    /// A tenant can hold up to its cap, then is refused until a permit is released.
    #[test]
    fn caps_a_tenant_then_refuses() {
        let limiter = TenantLimiter::new(64, 2).unwrap_or_else(|| unreachable!());
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

    /// A saturated tenant does not block a different tenant (assuming distinct buckets).
    #[test]
    fn one_tenant_does_not_starve_another() {
        let limiter = TenantLimiter::new(256, 1).unwrap_or_else(|| unreachable!());
        let _held = limiter.try_acquire("noisy");
        assert!(
            limiter.try_acquire("noisy").is_none(),
            "noisy is at its cap"
        );
        // `quiet` hashes to a different bucket with high probability at 256 buckets.
        assert!(
            limiter.try_acquire("quiet").is_some(),
            "a different tenant still proceeds"
        );
    }
}
