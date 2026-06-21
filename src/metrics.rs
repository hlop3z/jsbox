//! Process-wide observability counters, rendered as Prometheus text at `GET /metrics`.
//!
//! Dependency-free: a flat set of atomic counters incremented on each request's terminal
//! outcome, plus point-in-time gauges (bulkhead permits, breaker trips) read at scrape
//! time. `Relaxed` ordering is fine — these are monotonic counters with no cross-counter
//! invariant a reader depends on. Shared via `Arc` in `AppState`.

use core::array::from_fn;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::engine::EngineError;

/// Upper bounds (inclusive, microseconds) of the execution-latency histogram buckets.
/// Integer micros are compared directly — no float conversion on the hot path. The
/// matching `le` labels (seconds, Prometheus convention) are in [`LATENCY_BUCKET_LABELS`].
const LATENCY_BUCKETS_US: [u64; 12] = [
    1_000, 5_000, 10_000, 25_000, 50_000, 100_000, 250_000, 500_000, 1_000_000, 2_500_000,
    5_000_000, 10_000_000,
];

/// `le` labels (seconds) for [`LATENCY_BUCKETS_US`], in the same order.
const LATENCY_BUCKET_LABELS: [&str; 12] = [
    "0.001", "0.005", "0.01", "0.025", "0.05", "0.1", "0.25", "0.5", "1", "2.5", "5", "10",
];

/// A fixed-bucket latency histogram (Prometheus `_bucket`/`_sum`/`_count`). Lock-free:
/// each observation increments one bucket plus the running sum/count. Buckets are stored
/// non-cumulatively and accumulated at render time.
#[derive(Debug)]
struct LatencyHistogram {
    /// Per-bucket observation counts, aligned with [`LATENCY_BUCKETS_US`].
    buckets: [AtomicU64; 12],
    /// Sum of all observed durations, in microseconds (rendered as seconds).
    sum_us: AtomicU64,
    /// Total number of observations (the implicit `+Inf` bucket).
    count: AtomicU64,
}

impl Default for LatencyHistogram {
    fn default() -> Self {
        Self {
            buckets: from_fn(|_idx| AtomicU64::new(0)),
            sum_us: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }
}

impl LatencyHistogram {
    /// Records one observation: bumps the first bucket whose bound it falls within (an
    /// over-`10s` outlier lands only in `+Inf`/`count`), plus the sum and count.
    fn observe(&self, micros: u64) {
        for (bound, counter) in LATENCY_BUCKETS_US.iter().zip(self.buckets.iter()) {
            if micros <= *bound {
                let _ = counter.fetch_add(1, Ordering::Relaxed);
                break;
            }
        }
        let _ = self.count.fetch_add(1, Ordering::Relaxed);
        let _ = self.sum_us.fetch_add(micros, Ordering::Relaxed);
    }

    /// Renders only the series lines (no `# HELP`/`# TYPE`) for metric `name`, with `extra`
    /// labels (e.g. `capability="db"`, or empty) carried onto every `_bucket`/`_sum`/
    /// `_count`. Buckets are accumulated here into the cumulative Prometheus form.
    fn render_series(&self, name: &str, extra: &str) -> String {
        let total = self.count.load(Ordering::Relaxed);
        let mut cumulative: u64 = 0;
        let mut lines: Vec<String> = Vec::new();
        for (label, counter) in LATENCY_BUCKET_LABELS.iter().zip(self.buckets.iter()) {
            cumulative = cumulative.saturating_add(counter.load(Ordering::Relaxed));
            lines.push(format!(
                "{name}_bucket{} {cumulative}",
                bucket_labels(extra, label)
            ));
        }
        lines.push(format!(
            "{name}_bucket{} {total}",
            bucket_labels(extra, "+Inf")
        ));
        let suffix = if extra.is_empty() {
            String::new()
        } else {
            format!("{{{extra}}}")
        };
        let sum_us = self.sum_us.load(Ordering::Relaxed);
        // Seconds, formatted from integer micros (no float): whole.frac, 6-digit fraction.
        let whole = sum_us / 1_000_000;
        let frac = sum_us % 1_000_000;
        lines.push(format!("{name}_sum{suffix} {whole}.{frac:06}"));
        lines.push(format!("{name}_count{suffix} {total}"));
        lines.join("\n")
    }

    /// Renders the full histogram family (with `# HELP help` / `# TYPE`) for metric `name`.
    fn render(&self, name: &str, help: &str) -> String {
        format!(
            "# HELP {name} {help}\n# TYPE {name} histogram\n{series}\n",
            series = self.render_series(name, ""),
        )
    }
}

/// Builds the `{...le="<le>"}` label set for a bucket line, merging optional `extra`
/// labels (e.g. `capability="db"`) ahead of the `le` label.
fn bucket_labels(extra: &str, le: &str) -> String {
    if extra.is_empty() {
        format!("{{le=\"{le}\"}}")
    } else {
        format!("{{{extra},le=\"{le}\"}}")
    }
}

/// A metered capability, used to route a per-op latency observation to the right
/// histogram. Mirrors the `meta.<cap>_requests` families.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Capability {
    /// `db` (Postgres-family).
    Db,
    /// `mongo` (document database).
    Mongo,
    /// `api` (outbound HTTP).
    Http,
    /// `mail` (SMTP).
    Mail,
    /// `s3` (object-store presign/usage).
    S3,
    /// `redis`.
    Redis,
    /// `amq` (`RabbitMQ`).
    Amq,
    /// `auth` (OIDC/IAM).
    Auth,
}

/// Per-capability operation-latency histograms, one per [`Capability`]. Rendered as a
/// single Prometheus family keyed by a `capability` label.
#[derive(Debug, Default)]
struct CapabilityLatencies {
    /// `db` op latency.
    db: LatencyHistogram,
    /// `mongo` op latency.
    mongo: LatencyHistogram,
    /// `api` request latency.
    http: LatencyHistogram,
    /// `mail` op latency.
    mail: LatencyHistogram,
    /// `s3` op latency.
    s3: LatencyHistogram,
    /// `redis` op latency.
    redis: LatencyHistogram,
    /// `amq` op latency.
    amq: LatencyHistogram,
    /// `auth` op latency.
    auth: LatencyHistogram,
}

impl CapabilityLatencies {
    /// The histogram for `cap`.
    const fn histogram(&self, cap: Capability) -> &LatencyHistogram {
        match cap {
            Capability::Db => &self.db,
            Capability::Mongo => &self.mongo,
            Capability::Http => &self.http,
            Capability::Mail => &self.mail,
            Capability::S3 => &self.s3,
            Capability::Redis => &self.redis,
            Capability::Amq => &self.amq,
            Capability::Auth => &self.auth,
        }
    }

    /// Renders the single `jsbox_capability_op_duration_seconds` family: one `# HELP`/
    /// `# TYPE` header then every capability's series carrying a `capability="…"` label.
    fn render(&self) -> String {
        let name = "jsbox_capability_op_duration_seconds";
        let series: Vec<String> = [
            ("db", &self.db),
            ("mongo", &self.mongo),
            ("http", &self.http),
            ("mail", &self.mail),
            ("s3", &self.s3),
            ("redis", &self.redis),
            ("amq", &self.amq),
            ("auth", &self.auth),
        ]
        .into_iter()
        .map(|(label, hist)| hist.render_series(name, &format!("capability=\"{label}\"")))
        .collect();
        format!(
            "# HELP {name} Per-capability operation latency.\n# TYPE {name} histogram\n{body}\n",
            body = series.join("\n"),
        )
    }
}

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
    /// Wall-clock latency of executions that actually ran (excludes shed/rejected requests).
    exec_latency: LatencyHistogram,
    /// Per-capability op latency (which downstream is slow, not just total execution time).
    cap_latency: CapabilityLatencies,
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
            | EngineError::ModuleNotFound(_)
            | EngineError::HandlerNotDefined
            | EngineError::Script { .. } => &self.script_error,
            EngineError::Capability(_) => &self.capability_error,
            EngineError::Timeout { .. } => &self.timeout,
            EngineError::MemoryLimit => &self.memory_limit,
            // Both are the handler producing an unusable response (bad shape / over the size
            // cap) — bucket them together rather than adding a near-duplicate series.
            EngineError::Malformed(_) | EngineError::OutputTooLarge { .. } => {
                &self.malformed_response
            }
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

    /// Observes the wall-clock latency (microseconds) of an execution that ran. Clamps a
    /// (practically impossible) `u128` overflow to `u64::MAX` rather than wrapping.
    pub(crate) fn observe_execution(&self, micros: u128) {
        self.exec_latency
            .observe(u64::try_from(micros).unwrap_or(u64::MAX));
    }

    /// Observes one capability operation's latency (microseconds) in `cap`'s histogram.
    pub(crate) fn observe_op(&self, cap: Capability, micros: u128) {
        self.cap_latency
            .histogram(cap)
            .observe(u64::try_from(micros).unwrap_or(u64::MAX));
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
             jsbox_bulkhead_permits_total {bulkhead_total}\n\
             {latency}{cap_latency}",
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
            latency = self.exec_latency.render(
                "jsbox_execution_duration_seconds",
                "Execution wall-clock latency."
            ),
            cap_latency = self.cap_latency.render(),
        )
    }
}

#[cfg(test)]
mod tests {
    //! Counter increments and exposition formatting.

    use super::{Capability, Metrics};
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

    /// Latency observations land in the right cumulative buckets and sum/count totals.
    #[test]
    fn histogram_buckets_are_cumulative() {
        let metrics = Metrics::default();
        metrics.observe_execution(3_000); // 3ms  -> le="0.005" and up
        metrics.observe_execution(80_000); // 80ms -> le="0.1" and up
        metrics.observe_execution(50_000_000); // 50s -> only +Inf
        let text = metrics.render(1, 1, 0);
        // le="0.005" has counted the 3ms observation only.
        assert!(
            text.contains("jsbox_execution_duration_seconds_bucket{le=\"0.005\"} 1"),
            "3ms in the 5ms bucket"
        );
        // le="0.1" is cumulative: the 3ms + 80ms observations.
        assert!(
            text.contains("jsbox_execution_duration_seconds_bucket{le=\"0.1\"} 2"),
            "cumulative through 100ms"
        );
        // +Inf and count include the 50s outlier; sum = 0.003+0.08+50 = 50.083s.
        assert!(text.contains("jsbox_execution_duration_seconds_bucket{le=\"+Inf\"} 3"));
        assert!(text.contains("jsbox_execution_duration_seconds_count 3"));
        assert!(
            text.contains("jsbox_execution_duration_seconds_sum 50.083000"),
            "sum rendered as seconds from integer micros"
        );
    }

    /// Per-capability op observations render as a single labeled histogram family.
    #[test]
    fn capability_latency_is_labeled() {
        let metrics = Metrics::default();
        metrics.observe_op(Capability::Db, 4_000); // 4ms
        metrics.observe_op(Capability::Db, 4_000);
        metrics.observe_op(Capability::Http, 120_000); // 120ms
        let text = metrics.render(1, 1, 0);
        // One HELP/TYPE header for the whole family, series carry a `capability` label.
        assert_eq!(
            text.matches("# TYPE jsbox_capability_op_duration_seconds histogram")
                .count(),
            1,
            "exactly one TYPE line for the family"
        );
        assert!(text.contains(
            "jsbox_capability_op_duration_seconds_bucket{capability=\"db\",le=\"0.005\"} 2"
        ));
        assert!(text.contains("jsbox_capability_op_duration_seconds_count{capability=\"db\"} 2"));
        assert!(text.contains(
            "jsbox_capability_op_duration_seconds_bucket{capability=\"http\",le=\"0.1\"} 0"
        ));
        assert!(text.contains(
            "jsbox_capability_op_duration_seconds_bucket{capability=\"http\",le=\"0.25\"} 1"
        ));
        // Untouched capabilities still emit a (zeroed) series.
        assert!(text.contains("jsbox_capability_op_duration_seconds_count{capability=\"auth\"} 0"));
    }
}
