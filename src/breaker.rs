//! Per-target circuit breaker (Tier 3 of `docs/design/resilience.md`).
//!
//! When a downstream target (a `db` `host:port`) fails to connect repeatedly, the breaker
//! trips **open** and fast-fails calls to that target for a cool-down window — so jsbox
//! doesn't burn `spawn_blocking` threads on the 5 s connect timeout to a dead database,
//! and the struggling target gets room to recover instead of a thundering herd. After the
//! cool-down a single request probes (**half-open**); success closes the breaker, failure
//! re-opens it.
//!
//! Keyed per target. Targets are operator-supplied in `config.db`, so the key set is
//! small and bounded. State is shared across requests behind a mutex (a `db` connect is
//! not a hot-enough path for the lock to matter).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Breaker tunables.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BreakerConfig {
    /// Consecutive connect failures that trip the breaker open.
    pub(crate) threshold: u32,
    /// How long the breaker stays open before allowing a half-open probe.
    pub(crate) cooldown: Duration,
}

/// Per-target failure/open state.
#[derive(Debug)]
struct TargetState {
    /// Consecutive connect failures since the last success.
    failures: u32,
    /// When the breaker (if open) next allows a probe; `None` = closed.
    open_until: Option<Instant>,
}

/// Per-target circuit breaker. Build via [`CircuitBreaker::new`]; `None` = disabled.
#[derive(Debug)]
pub(crate) struct CircuitBreaker {
    /// Tunables.
    config: BreakerConfig,
    /// Target key → state.
    targets: Mutex<HashMap<String, TargetState>>,
}

impl CircuitBreaker {
    /// Builds a breaker, or `None` when disabled (`threshold == 0`).
    pub(crate) fn new(config: BreakerConfig) -> Option<Self> {
        if config.threshold == 0 {
            return None;
        }
        Some(Self {
            config,
            targets: Mutex::new(HashMap::new()),
        })
    }

    /// Returns `true` if a call to `target` is allowed (closed, or a half-open probe).
    /// On the first call after the cool-down elapses, re-arms the open window so only that
    /// one request probes while others keep fast-failing until the probe resolves.
    pub(crate) fn allow(&self, target: &str) -> bool {
        let now = Instant::now();
        let Ok(mut map) = self.targets.lock() else {
            return true; // poisoned lock: fail open, never wedge the service
        };
        let Some(state) = map.get_mut(target) else {
            return true; // unknown target = closed
        };
        match state.open_until {
            Some(until) if now < until => false,
            Some(_) => {
                state.open_until = now.checked_add(self.config.cooldown);
                true
            }
            None => true,
        }
    }

    /// Records the outcome of a connect to `target`: success resets the breaker; a failure
    /// increments the count and trips the breaker open once it reaches the threshold.
    pub(crate) fn record(&self, target: &str, success: bool) {
        let Ok(mut map) = self.targets.lock() else {
            return;
        };
        let state = map.entry(target.to_owned()).or_insert(TargetState {
            failures: 0,
            open_until: None,
        });
        if success {
            state.failures = 0;
            state.open_until = None;
        } else {
            state.failures = state.failures.saturating_add(1);
            if state.failures >= self.config.threshold {
                state.open_until = Instant::now().checked_add(self.config.cooldown);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Trip / cool-down / half-open behavior.

    use super::{BreakerConfig, CircuitBreaker};
    use std::thread::sleep;
    use std::time::Duration;

    /// Builds a breaker with threshold 3 and a short cool-down for tests.
    fn breaker(cooldown_ms: u64) -> CircuitBreaker {
        CircuitBreaker::new(BreakerConfig {
            threshold: 3,
            cooldown: Duration::from_millis(cooldown_ms),
        })
        .unwrap_or_else(|| unreachable!())
    }

    /// `threshold == 0` disables the breaker entirely.
    #[test]
    fn disabled_when_zero() {
        assert!(
            CircuitBreaker::new(BreakerConfig {
                threshold: 0,
                cooldown: Duration::from_secs(1)
            })
            .is_none(),
            "zero threshold disables"
        );
    }

    /// Trips open after `threshold` consecutive failures, then fast-fails.
    #[test]
    fn trips_after_threshold() {
        let cb = breaker(10_000);
        assert!(cb.allow("db:1"), "closed initially");
        cb.record("db:1", false);
        cb.record("db:1", false);
        assert!(cb.allow("db:1"), "still closed under threshold");
        cb.record("db:1", false); // 3rd failure → open
        assert!(!cb.allow("db:1"), "open after threshold");
    }

    /// A success resets the failure count, keeping the breaker closed.
    #[test]
    fn success_resets() {
        let cb = breaker(10_000);
        cb.record("db:1", false);
        cb.record("db:1", false);
        cb.record("db:1", true); // reset
        cb.record("db:1", false);
        assert!(
            cb.allow("db:1"),
            "reset means the next failures start from zero"
        );
    }

    /// One failing target does not open the breaker for a different target.
    #[test]
    fn targets_are_independent() {
        let cb = breaker(10_000);
        for _ in 0..3 {
            cb.record("down:1", false);
        }
        assert!(!cb.allow("down:1"), "the failing target is open");
        assert!(cb.allow("healthy:1"), "a different target stays closed");
    }

    /// After the cool-down a single probe is allowed; the rest stay fast-failed.
    #[test]
    fn half_open_after_cooldown() {
        let cb = breaker(1); // ~1ms cool-down
        for _ in 0..3 {
            cb.record("db:1", false);
        }
        sleep(Duration::from_millis(5));
        assert!(cb.allow("db:1"), "first call after cool-down probes");
        assert!(
            !cb.allow("db:1"),
            "concurrent callers keep fast-failing during the probe"
        );
    }
}
