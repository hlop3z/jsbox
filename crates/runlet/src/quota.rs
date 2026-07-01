//! Per-tenant, plan-gated usage quota (section 6 of the multitenant-trust change).
//!
//! Modeled on the nexus `routing-rs/plan.rs` shape (`PlanLimits` / `DomainLimit` / `QuotaExceeded`,
//! "at-or-above", fail-closed): a data-driven `plan → limit` table caps a tenant's in-flight usage.
//! The engine ([`PlanLimits`]) is pure and independently tested; [`TenantQuota`] adds the per-tenant
//! in-flight accounting keyed on the **trusted** tenant id (never a caller-asserted value).
//!
//! Fail-closed by construction: an unknown/absent plan resolves to the most restrictive configured
//! limit, and an empty table denies every request — a misconfiguration can never grant unbounded
//! usage.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use serde::Deserialize;

/// One plan's limit. Today a single dimension — the maximum concurrent in-flight executions a tenant
/// on this plan may have. (Kept as a struct so further dimensions can be added without touching call
/// sites.)
#[derive(Debug, Clone, Copy, Deserialize)]
pub(crate) struct PlanLimit {
    /// Maximum concurrent in-flight executions per tenant. `0` denies outright (at-or-above).
    pub(crate) max_concurrent: u64,
}

/// The result of a quota check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum QuotaDecision {
    /// Under the limit — the request may proceed.
    Allowed,
    /// At or above the limit — refuse, with the structured over-limit detail.
    Exceeded(QuotaExceeded),
}

/// Structured over-limit detail returned to the caller (plan, limit, current usage).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QuotaExceeded {
    /// The plan the decision was made under (`"unknown"` when the tenant's plan wasn't configured).
    pub(crate) plan: String,
    /// The limit that was hit.
    pub(crate) limit: u64,
    /// The tenant's usage at decision time.
    pub(crate) usage: u64,
}

/// The pure `plan → limit` gate. Resolves a plan name to its limit and decides at-or-above.
#[derive(Debug, Clone)]
pub(crate) struct PlanLimits {
    /// Plan name → limit.
    plans: HashMap<String, PlanLimit>,
}

impl PlanLimits {
    /// Builds the gate from the configured table.
    pub(crate) const fn new(plans: HashMap<String, PlanLimit>) -> Self {
        Self { plans }
    }

    /// The most restrictive configured limit (minimum `max_concurrent`), or a deny-all (`0`) when no
    /// plans are configured — the fail-closed default for an unknown/unconfigured plan.
    fn most_restrictive(&self) -> u64 {
        self.plans
            .values()
            .map(|limit| limit.max_concurrent)
            .min()
            .unwrap_or(0)
    }

    /// Resolves the effective `max_concurrent` for `plan`: the plan's own limit if configured, else
    /// the most restrictive configured limit (fail-closed).
    fn limit_for(&self, plan: Option<&str>) -> u64 {
        plan.and_then(|name| self.plans.get(name))
            .map_or_else(|| self.most_restrictive(), |limit| limit.max_concurrent)
    }

    /// Decides whether a request at `usage` in-flight may proceed under `plan` (at-or-above refuses).
    pub(crate) fn check(&self, plan: Option<&str>, usage: u64) -> QuotaDecision {
        let limit = self.limit_for(plan);
        if usage >= limit {
            QuotaDecision::Exceeded(QuotaExceeded {
                plan: plan.unwrap_or("unknown").to_owned(),
                limit,
                usage,
            })
        } else {
            QuotaDecision::Allowed
        }
    }
}

/// Per-tenant in-flight quota accounting: the pure gate plus a live in-flight counter per trusted
/// tenant id. Cheap to clone (all `Arc`-backed) so it can live in the shared app state.
#[derive(Debug, Clone)]
pub(crate) struct TenantQuota {
    /// The plan-limit gate.
    limits: PlanLimits,
    /// Trusted tenant id → current in-flight count. Bounded by the live tenant set (a finished
    /// request decrements back to zero and the entry is removed).
    inflight: Arc<Mutex<HashMap<String, u64>>>,
}

impl TenantQuota {
    /// Builds the accountant from the configured plan table.
    pub(crate) fn new(plans: HashMap<String, PlanLimit>) -> Self {
        Self {
            limits: PlanLimits::new(plans),
            inflight: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Admits one execution for `tenant` on `plan`: on [`QuotaDecision::Allowed`] the tenant's
    /// in-flight count is incremented and a [`QuotaGuard`] (which decrements on drop) is returned;
    /// otherwise the structured [`QuotaExceeded`] is returned and nothing is counted.
    ///
    /// # Errors
    ///
    /// Returns the [`QuotaExceeded`] detail when the tenant is at or above its plan limit.
    pub(crate) fn admit(
        &self,
        tenant: &str,
        plan: Option<&str>,
    ) -> Result<QuotaGuard, QuotaExceeded> {
        let mut guard = self.inflight.lock().unwrap_or_else(PoisonError::into_inner);
        let usage = guard.get(tenant).copied().unwrap_or(0);
        match self.limits.check(plan, usage) {
            QuotaDecision::Allowed => {
                let _ = guard.insert(tenant.to_owned(), usage.saturating_add(1));
                drop(guard);
                Ok(QuotaGuard {
                    inflight: Arc::clone(&self.inflight),
                    tenant: tenant.to_owned(),
                })
            }
            QuotaDecision::Exceeded(detail) => {
                drop(guard);
                Err(detail)
            }
        }
    }
}

/// Releases one unit of a tenant's in-flight quota when dropped (RAII, mirroring the bulkhead
/// permit). Held across the execution span.
#[derive(Debug)]
pub(crate) struct QuotaGuard {
    /// Shared in-flight table to decrement.
    inflight: Arc<Mutex<HashMap<String, u64>>>,
    /// The tenant whose count to release.
    tenant: String,
}

impl Drop for QuotaGuard {
    fn drop(&mut self) {
        let mut guard = self.inflight.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = guard.get_mut(&self.tenant) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                let _ = guard.remove(&self.tenant);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! The pure gate matrix (within/at/over, unknown → most restrictive, empty → deny) and the
    //! in-flight accounting (increment, release-on-drop, over-limit refusal).

    use super::{PlanLimit, PlanLimits, QuotaDecision, TenantQuota};
    use std::collections::HashMap;

    /// A plan table `{name: max_concurrent}`.
    fn table(entries: &[(&str, u64)]) -> HashMap<String, PlanLimit> {
        entries
            .iter()
            .map(|(name, max)| {
                (
                    (*name).to_owned(),
                    PlanLimit {
                        max_concurrent: *max,
                    },
                )
            })
            .collect()
    }

    /// Within the limit is allowed; at or above it is refused with the structured detail.
    #[test]
    fn within_at_and_over_limit() {
        let gate = PlanLimits::new(table(&[("free", 2), ("pro", 10)]));
        assert_eq!(gate.check(Some("pro"), 0), QuotaDecision::Allowed, "under");
        assert_eq!(gate.check(Some("pro"), 9), QuotaDecision::Allowed, "under");
        match gate.check(Some("pro"), 10) {
            QuotaDecision::Exceeded(detail) => {
                assert_eq!(detail.plan, "pro");
                assert_eq!(detail.limit, 10);
                assert_eq!(detail.usage, 10);
            }
            QuotaDecision::Allowed => unreachable!("at the limit must refuse"),
        }
        assert!(
            matches!(gate.check(Some("free"), 2), QuotaDecision::Exceeded(_)),
            "at the free limit refuses"
        );
    }

    /// An unknown plan inherits the most restrictive configured limit.
    #[test]
    fn unknown_plan_is_most_restrictive() {
        let gate = PlanLimits::new(table(&[("free", 2), ("pro", 10)]));
        // most restrictive = free's 2. Usage 2 must refuse; usage 1 must pass.
        assert_eq!(gate.check(Some("mystery"), 1), QuotaDecision::Allowed);
        match gate.check(None, 2) {
            QuotaDecision::Exceeded(detail) => assert_eq!(detail.limit, 2, "min limit applied"),
            QuotaDecision::Allowed => unreachable!("unknown plan must use the min limit"),
        }
    }

    /// An empty table denies every request (fail-closed).
    #[test]
    fn empty_table_denies() {
        let gate = PlanLimits::new(HashMap::new());
        assert!(
            matches!(gate.check(Some("pro"), 0), QuotaDecision::Exceeded(_)),
            "empty config denies even the first request"
        );
    }

    /// Accounting: a tenant fills its plan slots, is refused at the cap, and a released guard frees a
    /// slot again.
    #[test]
    fn accounting_increments_and_releases() {
        let quota = TenantQuota::new(table(&[("free", 2)]));
        let g1 = quota.admit("ws_a", Some("free")).ok();
        let g2 = quota.admit("ws_a", Some("free")).ok();
        assert!(g1.is_some() && g2.is_some(), "two slots admitted");
        assert!(quota.admit("ws_a", Some("free")).is_err(), "third refused");
        // A different tenant is unaffected.
        assert!(
            quota.admit("ws_b", Some("free")).is_ok(),
            "other tenant free"
        );
        drop(g1);
        assert!(
            quota.admit("ws_a", Some("free")).is_ok(),
            "a released slot is reusable"
        );
        drop(g2);
    }
}
