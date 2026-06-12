//! Process-wide observability counters, rendered as Prometheus text at `GET /metrics`.
//!
//! Dependency-free: a flat set of atomic counters incremented on each request's terminal
//! outcome, plus point-in-time gauges (bulkhead permits, breaker trips) read at scrape
//! time. `Relaxed` ordering is fine — these are monotonic counters with no cross-counter
//! invariant a reader depends on. Shared via `Arc` in `AppState`.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::engine::EngineError;

/// All process-wide counters. One instance lives in `AppState`, shared across requests.
#[derive(Debug, Default)]
pub(crate) struct Metrics {
    /// Executions that returned a handler result.
    success: AtomicU64,
    /// Executions that failed with a script/syntax/handler error.
    script_error: AtomicU64,
    /// Executions where a capability call (db/api/…) threw.
    capability_error: AtomicU64,
    /// Executions killed by the wall-clock timeout.
    timeout: AtomicU64,
    /// Executions aborted by the memory cap.
    memory_limit: AtomicU64,
    /// Executions whose handler returned a non-`{data,error}` envelope.
    malformed_response: AtomicU64,
    /// Executions that hit an internal (our-fault) error or task panic.
    internal_error: AtomicU64,
    /// Requests rejected before execution (bad body, key/script violation, oversized input).
    rejections: AtomicU64,
    /// Requests shed by the global bulkhead (`429 OVERLOADED`).
    overload_global: AtomicU64,
    /// Requests shed by a partition's fairness cap (`429 PARTITION_OVERLOADED`).
    overload_partition: AtomicU64,
}

impl Metrics {
    /// Records a successful execution.
    pub(crate) fn record_success(&self) {
        let _ = self.success.fetch_add(1, Ordering::Relaxed);
    }

    /// Records an execution that ended in a classified [`EngineError`], bucketing it by kind.
    pub(crate) fn record_engine_error(&self, err: &EngineError) {
        let counter = match *err {
            EngineError::Syntax(_)
            | EngineError::HandlerNotDefined
            | EngineError::Script { .. } => &self.script_error,
            EngineError::Capability(_) => &self.capability_error,
            EngineError::Timeout { .. } => &self.timeout,
            EngineError::MemoryLimit => &self.memory_limit,
            EngineError::Malformed(_) => &self.malformed_response,
            EngineError::Internal(_) => &self.internal_error,
        };
        let _ = counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a request rejected before execution (validation / routing).
    pub(crate) fn record_rejection(&self) {
        let _ = self.rejections.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a request shed by the global bulkhead.
    pub(crate) fn record_overload_global(&self) {
        let _ = self.overload_global.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a request shed by a partition's fairness cap.
    pub(crate) fn record_overload_partition(&self) {
        let _ = self.overload_partition.fetch_add(1, Ordering::Relaxed);
    }

    /// Renders the Prometheus text exposition (v0.0.4). `bulkhead_available` /
    /// `bulkhead_total` are the live semaphore permits; `breaker_trips` is the cumulative
    /// circuit-breaker open count (both read at scrape time, not stored here).
    pub(crate) fn render(
        &self,
        bulkhead_available: usize,
        bulkhead_total: usize,
        breaker_trips: u64,
    ) -> String {
        let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        format!(
            "# HELP jsbox_executions_total Executions by terminal outcome.\n\
             # TYPE jsbox_executions_total counter\n\
             jsbox_executions_total{{outcome=\"success\"}} {success}\n\
             jsbox_executions_total{{outcome=\"script_error\"}} {script_error}\n\
             jsbox_executions_total{{outcome=\"capability_error\"}} {capability_error}\n\
             jsbox_executions_total{{outcome=\"timeout\"}} {timeout}\n\
             jsbox_executions_total{{outcome=\"memory_limit\"}} {memory_limit}\n\
             jsbox_executions_total{{outcome=\"malformed_response\"}} {malformed_response}\n\
             jsbox_executions_total{{outcome=\"internal_error\"}} {internal_error}\n\
             # HELP jsbox_rejections_total Requests rejected before execution (bad body, routing, oversized).\n\
             # TYPE jsbox_rejections_total counter\n\
             jsbox_rejections_total {rejections}\n\
             # HELP jsbox_overload_total Requests shed by a concurrency limit.\n\
             # TYPE jsbox_overload_total counter\n\
             jsbox_overload_total{{scope=\"global\"}} {overload_global}\n\
             jsbox_overload_total{{scope=\"partition\"}} {overload_partition}\n\
             # HELP jsbox_db_breaker_trips_total Cumulative db circuit-breaker open transitions.\n\
             # TYPE jsbox_db_breaker_trips_total counter\n\
             jsbox_db_breaker_trips_total {breaker_trips}\n\
             # HELP jsbox_bulkhead_permits_available Free global bulkhead permits right now.\n\
             # TYPE jsbox_bulkhead_permits_available gauge\n\
             jsbox_bulkhead_permits_available {bulkhead_available}\n\
             # HELP jsbox_bulkhead_permits_total Configured global bulkhead capacity.\n\
             # TYPE jsbox_bulkhead_permits_total gauge\n\
             jsbox_bulkhead_permits_total {bulkhead_total}\n",
            success = load(&self.success),
            script_error = load(&self.script_error),
            capability_error = load(&self.capability_error),
            timeout = load(&self.timeout),
            memory_limit = load(&self.memory_limit),
            malformed_response = load(&self.malformed_response),
            internal_error = load(&self.internal_error),
            rejections = load(&self.rejections),
            overload_global = load(&self.overload_global),
            overload_partition = load(&self.overload_partition),
        )
    }
}

#[cfg(test)]
mod tests {
    //! Counter increments and exposition formatting.

    use super::Metrics;
    use crate::engine::EngineError;

    /// A fresh registry renders all-zero counters with the expected metric lines.
    #[test]
    fn renders_zeroed_registry() {
        let metrics = Metrics::default();
        let text = metrics.render(6, 6, 0);
        assert!(
            text.contains("jsbox_executions_total{outcome=\"success\"} 0"),
            "success line present and zeroed"
        );
        assert!(
            text.contains("jsbox_bulkhead_permits_total 6"),
            "bulkhead capacity gauge present"
        );
    }

    /// Outcome counters increment independently and surface in the exposition.
    #[test]
    fn counts_outcomes() {
        let metrics = Metrics::default();
        metrics.record_success();
        metrics.record_success();
        metrics.record_engine_error(&EngineError::Timeout { limit_ms: 100 });
        metrics.record_engine_error(&EngineError::MemoryLimit);
        metrics.record_rejection();
        metrics.record_overload_partition();
        let text = metrics.render(5, 6, 2);
        assert!(text.contains("jsbox_executions_total{outcome=\"success\"} 2"));
        assert!(text.contains("jsbox_executions_total{outcome=\"timeout\"} 1"));
        assert!(text.contains("jsbox_executions_total{outcome=\"memory_limit\"} 1"));
        assert!(text.contains("jsbox_rejections_total 1"));
        assert!(text.contains("jsbox_overload_total{scope=\"partition\"} 1"));
        assert!(text.contains("jsbox_db_breaker_trips_total 2"));
        assert!(text.contains("jsbox_bulkhead_permits_available 5"));
    }
}
