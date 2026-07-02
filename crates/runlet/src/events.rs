//! Per-tenant usage + audit events (Change C).
//!
//! One unified, versioned envelope is emitted per request — a `usage` event for every executed
//! request (billing/quota-tuning record) and an `audit` event for every request (allowed, or
//! denied-with-reason at a gate; the compliance trail). Emission is **non-blocking and fail-open**:
//! events are handed to a bounded channel via `try_send` and drained by a writer task that writes
//! one JSON line per event to stdout (a dedicated event stream a collector routes on the envelope).
//! On a full channel the event is dropped and a counter bumped — the request path never blocks.
//!
//! The [`Sink`] port + the per-event `event_id` (dedup key) are the seam a durable, billing-grade
//! outbox drops into later without changing the emission sites (design D1/D3). Identity lives here
//! in `runlet`, never in `runlet-core` (D6); tenant is an event dimension, never a metric label.

use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uuid::Uuid;

/// Schema version of the event envelope. Bump on any breaking envelope/body change so a durable
/// consumer can branch on it.
const EVENT_SCHEMA_VERSION: u32 = 1;

/// The unified event envelope. `usage` and `audit` events share these fields; the type-specific
/// payload is flattened in from [`EventBody`] (adding a `type` discriminator).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Event {
    /// Envelope schema version ([`EVENT_SCHEMA_VERSION`]). Serialized as `v`.
    #[serde(rename = "v")]
    version: u32,
    /// Unique per-event id — the idempotency/dedup key a durable outbox consumes. Serialized as
    /// `event_id`.
    #[serde(rename = "event_id")]
    id: String,
    /// Event time, Unix epoch milliseconds.
    ts: u128,
    /// Trusted tenant id this event is attributed to. `None` only when a request was rejected
    /// before any tenant was resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    tenant: Option<String>,
    /// Trusted user id (audit attribution).
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<String>,
    /// Tenant plan (quota tier).
    #[serde(skip_serializing_if = "Option::is_none")]
    plan: Option<String>,
    /// Correlation id shared with the request/trace (`meta.trace_id`).
    trace_id: String,
    /// The type-tagged payload (`type` = `usage` | `audit`).
    #[serde(flatten)]
    body: EventBody,
}

/// The type-specific event payload, internally tagged by `type`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum EventBody {
    /// One executed request's usage (billing dimensions).
    Usage(UsageBody),
    /// One request's terminal decision (compliance trail).
    Audit(AuditBody),
}

/// Per-request usage dimensions — sourced from the response `meta` the box already computes.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct UsageBody {
    /// Terminal outcome (`success` / `script_error` / `capability_error` / `timeout` / …), the
    /// same taxonomy the metrics use.
    pub(crate) outcome: String,
    /// Wall-clock execution time, microseconds.
    pub(crate) exec_time_us: u128,
    /// Total input size (script + context) in bytes.
    pub(crate) input_bytes: usize,
    /// Per-capability operation counts (`db`, `mongo`, `http`, `mail`, `s3`, `redis`, `amq`,
    /// `auth`) — the metered downstream work, including `fabricd`-drained egress.
    pub(crate) ops: CapabilityOps,
}

/// Operation counts per capability for one request.
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct CapabilityOps {
    /// `db` operations.
    pub(crate) db: usize,
    /// `mongo` operations.
    pub(crate) mongo: usize,
    /// `http` (`api`) requests.
    pub(crate) http: usize,
    /// `mail` operations.
    pub(crate) mail: usize,
    /// `s3` operations.
    pub(crate) s3: usize,
    /// `redis` operations.
    pub(crate) redis: usize,
    /// `amq` operations.
    pub(crate) amq: usize,
    /// `auth` operations.
    pub(crate) auth: usize,
}

/// Per-request decision — `allowed` when the request ran, or `denied` with a machine-readable
/// `reason` code when a gate terminated it (optionally with `detail`, e.g. quota plan/limit/usage).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AuditBody {
    /// `allowed` or `denied`.
    pub(crate) decision: &'static str,
    /// The reject reason code (the response error code), when denied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
    /// Optional decision detail (e.g. `{plan, limit, usage}` for quota; `{entitlement}` for authz).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<Value>,
}

impl Event {
    /// Builds an event with a fresh `event_id` + timestamp, attributed to the given identity.
    pub(crate) fn new(
        tenant: Option<String>,
        user: Option<String>,
        plan: Option<String>,
        trace_id: String,
        body: EventBody,
    ) -> Self {
        Self {
            version: EVENT_SCHEMA_VERSION,
            id: Uuid::new_v4().to_string(),
            ts: now_unix_millis(),
            tenant,
            user,
            plan,
            trace_id,
            body,
        }
    }
}

/// Current Unix time in milliseconds; `0` if the clock is before the epoch (never blocks/panics).
fn now_unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|delta| delta.as_millis())
        .unwrap_or(0)
}

/// A non-blocking event sink. `record` must never block or fail the request path. `Debug` is
/// required so `AppState` (which holds an `Arc<dyn Sink>`) can derive it.
pub(crate) trait Sink: Send + Sync + fmt::Debug {
    /// Hands an event to the sink. Implementations drop (never await) under backpressure.
    fn record(&self, event: Event);
}

/// The lossy, observability-grade sink: a bounded channel drained by a writer task that emits one
/// JSON line per event to stdout. A full channel drops the event and increments `dropped`.
#[derive(Debug)]
struct LogSink {
    /// Bounded sender to the writer task.
    tx: mpsc::Sender<Event>,
    /// Count of events dropped because the channel was full/closed (the backpressure signal).
    dropped: Arc<AtomicU64>,
}

impl Sink for LogSink {
    fn record(&self, event: Event) {
        if self.tx.try_send(event).is_err() {
            let _ = self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Owns the writer task so buffered events can be flushed on shutdown, and exposes the dropped
/// counter for the `/metrics` backpressure gauge.
pub(crate) struct EventPipeline {
    /// The background writer task (drains the channel until all senders drop).
    writer: JoinHandle<()>,
    /// Shared dropped-events counter.
    dropped: Arc<AtomicU64>,
}

impl EventPipeline {
    /// Spawns the writer task and returns the pipeline plus the [`Sink`] to place in `AppState`.
    /// `bound` is the channel capacity; beyond it events are dropped (fail-open).
    pub(crate) fn spawn(bound: usize) -> (Self, Arc<dyn Sink>) {
        let (tx, rx) = mpsc::channel(bound.max(1));
        let dropped = Arc::new(AtomicU64::new(0));
        let sink: Arc<dyn Sink> = Arc::new(LogSink {
            tx,
            dropped: Arc::clone(&dropped),
        });
        let writer = tokio::spawn(writer_loop(rx));
        (Self { writer, dropped }, sink)
    }

    /// A shared handle to the dropped-events counter, for the `/metrics` backpressure gauge
    /// (`runlet_events_dropped_total`), read live at scrape time.
    pub(crate) fn dropped_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.dropped)
    }

    /// Best-effort flush: the caller drops the last [`Sink`] (closing the channel) before calling
    /// this, so the writer drains the remainder and exits, which this awaits.
    pub(crate) async fn shutdown(self) {
        if let Err(err) = self.writer.await {
            tracing::warn!("event writer task join error: {err}");
        }
    }
}

/// Drains events and writes one JSON line each to stdout until all senders drop.
#[expect(
    clippy::print_stdout,
    reason = "the event stream IS stdout by design (D5): a distinct JSON line per event the \
              collector tails, decoupled from the tracing/OTLP path so a collector outage cannot \
              lose events"
)]
async fn writer_loop(mut rx: mpsc::Receiver<Event>) {
    while let Some(event) = rx.recv().await {
        match serde_json::to_string(&event) {
            Ok(line) => println!("{line}"),
            Err(err) => tracing::warn!("event serialize error: {err}"),
        }
    }
}

#[cfg(test)]
mod tests {
    //! Envelope serialization + the non-blocking drop-on-full contract.

    use super::{
        AtomicU64, AuditBody, CapabilityOps, Event, EventBody, LogSink, Ordering, Sink, UsageBody,
        Value, mpsc,
    };
    use std::sync::Arc;

    /// A representative `usage` event.
    fn usage_event() -> Event {
        Event::new(
            Some("ws_acme".to_owned()),
            Some("u_1".to_owned()),
            Some("pro".to_owned()),
            "trace-abc".to_owned(),
            EventBody::Usage(UsageBody {
                outcome: "success".to_owned(),
                exec_time_us: 5,
                input_bytes: 10,
                ops: CapabilityOps::default(),
            }),
        )
    }

    /// The envelope serializes with every shared field, the flattened `type`, and a unique id.
    #[test]
    fn envelope_serializes_with_fields_and_unique_id() {
        let first = usage_event();
        let second = usage_event();
        assert_ne!(
            first.id, second.id,
            "event_id is unique per event (the outbox dedup key)"
        );
        let json = serde_json::to_string(&first).unwrap_or_default();
        for field in [
            "\"v\":1",
            "\"event_id\"",
            "\"trace_id\":\"trace-abc\"",
            "\"type\":\"usage\"",
            "\"tenant\":\"ws_acme\"",
            "\"outcome\":\"success\"",
        ] {
            assert!(json.contains(field), "missing {field} in {json}");
        }
    }

    /// A denied audit event carries the reason + detail and the `audit` type.
    #[test]
    fn audit_event_serializes_reason_and_detail() {
        let event = Event::new(
            Some("ws_acme".to_owned()),
            Some("u_1".to_owned()),
            Some("free".to_owned()),
            "trace-xyz".to_owned(),
            EventBody::Audit(AuditBody {
                decision: "denied",
                reason: Some("QUOTA_EXCEEDED".to_owned()),
                detail: Some(Value::from("detail")),
            }),
        );
        let json = serde_json::to_string(&event).unwrap_or_default();
        assert!(json.contains("\"type\":\"audit\""));
        assert!(json.contains("\"decision\":\"denied\""));
        assert!(json.contains("\"reason\":\"QUOTA_EXCEEDED\""));
    }

    /// `record` never blocks: once the bounded channel is full, further events are dropped and
    /// counted rather than awaited.
    #[test]
    fn full_channel_drops_and_counts() {
        // Capacity 1, receiver held but never drained.
        let (tx, _rx) = mpsc::channel(1);
        let dropped = Arc::new(AtomicU64::new(0));
        let sink = LogSink {
            tx,
            dropped: Arc::clone(&dropped),
        };
        sink.record(usage_event()); // fills the single slot
        sink.record(usage_event()); // full → dropped
        sink.record(usage_event()); // full → dropped
        assert_eq!(
            dropped.load(Ordering::Relaxed),
            2,
            "events beyond the bound are dropped, not blocked"
        );
    }
}
