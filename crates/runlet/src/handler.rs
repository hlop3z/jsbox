//! HTTP handler for the `/execute` endpoint.

use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response as AxumResponse};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use tokio::runtime::Handle;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task;
use tracing::warn;
use uuid::Uuid;

use fabric_wire::wire::WireInit;
use fabric_wire::{
    AmqMetric, AuthMetric, BackendMetrics, DbMetric, Egress, MailMetric, MeteredEgress,
    MongoMetric, RedisMetric,
};
use runlet_core::config::EngineConfig;
use runlet_core::engine::{EngineError, ExecOutcome, Gate};
use runlet_core::errors::{ErrorCategory, ErrorDebug, ErrorEnvelope, ErrorOwner, ErrorSource};
use runlet_core::host::{CapabilitySet, ExecMetrics, Invocation, LogicHost, Outcome};
use runlet_core::http::HttpMetric;
use runlet_core::metrics::{Capability, Metrics};
use runlet_core::partition::PartitionLimiter;
use runlet_core::registry::ScriptRegistry;
use runlet_core::s3::{S3Config, S3Metric};
use runlet_core::sandbox;
use runlet_core::sys::SysConfig;

use crate::sidecar::{SessionError, SidecarEgress, SidecarTransport, connect_session};

/// Shared application state for the router: the logic host, the script registry, and
/// the concurrency bulkhead.
#[derive(Debug, Clone)]
pub(crate) struct AppState {
    /// The callable logic host (pool + resilience + engine limits) — runs each request.
    pub(crate) host: LogicHost,
    /// Read-only registry of scripts loaded at startup (execute-by-key). Resolved here for
    /// input-sizing/meta before the source is handed to the host as inline code.
    pub(crate) registry: Arc<ScriptRegistry>,
    /// Engine sandbox limits, used outside the host for input-size validation.
    pub(crate) engine_cfg: EngineConfig,
    /// Include `error.debug` (stack traces + raw causes) in responses.
    pub(crate) error_debug: bool,
    /// Bulkhead bounding concurrent executions: a permit is held across the blocking
    /// execution span, and acquisition fast-fails (`429 OVERLOADED`) when saturated so a
    /// slow downstream can't exhaust blocking threads / DB connections.
    pub(crate) limiter: Arc<Semaphore>,
    /// Per-partition fairness (Tier 5): caps concurrency per `X-Partition-Key`. `None` when
    /// disabled. Acquired *before* the global bulkhead so a noisy partition fast-fails on its
    /// own share (`429 PARTITION_OVERLOADED`) while global capacity stays free for others.
    pub(crate) partition_limiter: Option<PartitionLimiter>,
    /// How the box reaches the `fabricd` egress sidecar (local UDS, remote QUIC, or none). The box
    /// links no driver and holds no credentials: a request that names a driver resource in
    /// `config.io` opens a session over this transport to `fabricd`, which resolves the name against
    /// its own operator config and performs the I/O. [`SidecarTransport::None`] ⇒ driver
    /// capabilities are unavailable (`503 EGRESS_UNAVAILABLE`).
    pub(crate) transport: SidecarTransport,
    /// Process-wide observability counters, exposed at `GET /metrics`.
    pub(crate) metrics: Arc<Metrics>,
    /// Configured global bulkhead capacity, surfaced as the `_total` permit gauge.
    pub(crate) bulkhead_capacity: usize,
    /// Shared-secret bearer token gating `/execute`. `None` = no in-process auth (the
    /// operator either bound loopback or opted out via `allow_unauthenticated`).
    pub(crate) access_token: Option<Arc<str>>,
}

/// Pre-allocated `Box<RawValue>` for `{}` — used as default context.
static DEFAULT_CONTEXT: LazyLock<Box<RawValue>> =
    LazyLock::new(|| RawValue::from_string("{}".into()).unwrap_or_else(|_err| unreachable!()));

/// Pre-allocated `Box<RawValue>` for `null` — used as default envelope field.
static RAW_NULL: LazyLock<Box<RawValue>> =
    LazyLock::new(|| RawValue::from_string("null".into()).unwrap_or_else(|_err| unreachable!()));

/// Request body for script execution.
#[derive(Debug, Deserialize)]
pub(crate) struct ExecRequest {
    /// Inline JavaScript source to evaluate (exactly one of `script` / `key`).
    script: Option<String>,
    /// Registered-script key to execute (exactly one of `script` / `key`).
    key: Option<String>,
    /// Partition key for per-partition fairness (Tier 5). The `X-Partition-Key` header
    /// takes precedence over this field; both are set by the trusted caller, not the script.
    #[serde(default)]
    partition: Option<String>,
    /// Raw context passed straight to `QuickJS` — never deserialized in Rust.
    #[serde(default = "default_context")]
    context: Box<RawValue>,
    /// Per-request configuration.
    #[serde(default)]
    config: RequestConfig,
}

/// Resolved script source — inline from the request body or shared from the registry.
#[derive(Debug)]
enum ScriptSource {
    /// Inline `script` field.
    Inline(String),
    /// Registered script resolved from `key`.
    Registered(Arc<str>),
}

impl ScriptSource {
    /// The script text.
    fn as_str(&self) -> &str {
        match self {
            Self::Inline(source) => source.as_str(),
            Self::Registered(source) => source.as_ref(),
        }
    }
}

/// Per-request configuration sent by the caller.
///
/// Driver-backed capabilities carry no connection config here: the request names logical resources
/// in [`io`](Self::io) and the box forwards those names to `fabricd`, which resolves the
/// endpoint/credentials operator-side. `http` (`allowed_hosts`) and `s3` stay
/// script-controlled/in-engine and keep their config.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RequestConfig {
    /// Allowed hosts for the `api` HTTP client.
    #[serde(default)]
    pub(crate) allowed_hosts: Vec<String>,
    /// S3 presigning config (omit to disable `s3` in JS). Stays in-engine (pure `SigV4` presign).
    #[serde(default)]
    pub(crate) s3: Option<S3Config>,
    /// `$sys` env/secrets context (omit to leave `$sys.env`/`$sys.secrets` empty).
    #[serde(default)]
    pub(crate) sys: Option<SysConfig>,
    /// Logical resources this invocation may reach, keyed by capability kind (e.g.
    /// `{"db":["orders-db"]}`). The names are sent to `fabricd`, which resolves them against its
    /// operator config; the request never carries endpoints or credentials.
    #[serde(default)]
    pub(crate) io: RequestIo,
}

/// The `config.io` allowlist: which logical resources the script may address, per capability
/// kind. The box selects the first named resource of each kind (single binding per kind) and
/// sends those names to `fabricd` in the session `WireInit`.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RequestIo {
    /// `db` logical resource names.
    #[serde(default)]
    pub(crate) db: Vec<String>,
    /// `mongo` logical resource names.
    #[serde(default)]
    pub(crate) mongo: Vec<String>,
    /// `mail` logical resource names.
    #[serde(default)]
    pub(crate) mail: Vec<String>,
    /// `redis` logical resource names.
    #[serde(default)]
    pub(crate) redis: Vec<String>,
    /// `amq` logical resource names.
    #[serde(default)]
    pub(crate) amq: Vec<String>,
    /// `auth` logical resource names.
    #[serde(default)]
    pub(crate) auth: Vec<String>,
}

/// Per-capability gates derived from `config.io` — which capability wrapper the engine injects.
/// `Copy` so it can move into the blocking task.
#[derive(Debug, Clone, Copy)]
struct CapabilityGates {
    /// Expose `db`.
    db: Gate,
    /// Expose `mongo`.
    mongo: Gate,
    /// Expose `mail`.
    mail: Gate,
    /// Expose `redis`.
    redis: Gate,
    /// Expose `amq`.
    amq: Gate,
    /// Expose `auth`.
    auth: Gate,
}

impl RequestIo {
    /// `true` if any driver capability is requested (so a `fabricd` session is needed).
    fn any(&self) -> bool {
        [
            &self.db,
            &self.mongo,
            &self.mail,
            &self.redis,
            &self.amq,
            &self.auth,
        ]
        .iter()
        .any(|names| !names.is_empty())
    }

    /// The engine-side capability gates — a kind is exposed iff the request named a resource for it.
    const fn gates(&self) -> CapabilityGates {
        CapabilityGates {
            db: Gate::from_enabled(!self.db.is_empty()),
            mongo: Gate::from_enabled(!self.mongo.is_empty()),
            mail: Gate::from_enabled(!self.mail.is_empty()),
            redis: Gate::from_enabled(!self.redis.is_empty()),
            amq: Gate::from_enabled(!self.amq.is_empty()),
            auth: Gate::from_enabled(!self.auth.is_empty()),
        }
    }

    /// The `fabricd` session-open message: the first named resource per kind + the per-execution
    /// deadline. `fabricd` resolves each name against its operator config.
    fn wire_init(&self, timeout: Duration) -> WireInit {
        WireInit {
            db: self.db.first().cloned(),
            mongo: self.mongo.first().cloned(),
            mail: self.mail.first().cloned(),
            redis: self.redis.first().cloned(),
            amq: self.amq.first().cloned(),
            auth: self.auth.first().cloned(),
            timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
            // The token (QUIC path) is attached by `connect_session` from the transport's auth
            // provider — the box-request layer never sees it.
            token: None,
        }
    }
}

/// Returns a clone of the pre-allocated default context.
fn default_context() -> Box<RawValue> {
    DEFAULT_CONTEXT.clone()
}

/// Metadata computed by Rust.
#[derive(Debug, Serialize)]
struct Meta {
    /// Correlation ID — also logged server-side with the raw cause, so support can grep
    /// one ID across the mesh. Present on every response (success and error).
    trace_id: String,
    /// Registered-script key, echoed back when the request executed by key.
    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<String>,
    /// Partition key, echoed back when one was supplied (Tier 5 observability).
    #[serde(skip_serializing_if = "Option::is_none")]
    partition: Option<String>,
    /// Size of the script in bytes.
    script_bytes: usize,
    /// Size of the context payload in bytes.
    context_bytes: usize,
    /// Total input size in bytes (script + context).
    total_input_bytes: usize,
    /// Execution time in microseconds.
    exec_time_us: u128,
    /// HTTP requests made by the script.
    http_requests: Vec<HttpMetric>,
    /// Database operations made by the script.
    db_requests: Vec<DbMetric>,
    /// Mongo operations made by the script.
    mongo_requests: Vec<MongoMetric>,
    /// Mail operations made by the script.
    mail_requests: Vec<MailMetric>,
    /// S3 presign operations made by the script.
    s3_requests: Vec<S3Metric>,
    /// Redis operations made by the script.
    redis_requests: Vec<RedisMetric>,
    /// `RabbitMQ` operations made by the script.
    amq_requests: Vec<AmqMetric>,
    /// Auth operations made by the script.
    auth_requests: Vec<AuthMetric>,
}

impl Meta {
    /// Creates a new `Meta` with the given correlation ID, sizes, and empty metrics.
    const fn new(
        trace_id: String,
        script_bytes: usize,
        context_bytes: usize,
        exec_time_us: u128,
    ) -> Self {
        Self {
            trace_id,
            key: None,
            partition: None,
            script_bytes,
            context_bytes,
            total_input_bytes: script_bytes.saturating_add(context_bytes),
            exec_time_us,
            http_requests: Vec::new(),
            db_requests: Vec::new(),
            mongo_requests: Vec::new(),
            mail_requests: Vec::new(),
            s3_requests: Vec::new(),
            redis_requests: Vec::new(),
            amq_requests: Vec::new(),
            auth_requests: Vec::new(),
        }
    }

    /// Attaches the registered-script key (echoed back on key-mode requests).
    fn with_key(mut self, key: Option<String>) -> Self {
        self.key = key;
        self
    }

    /// Attaches the partition key (echoed back when supplied).
    fn with_partition(mut self, partition: Option<String>) -> Self {
        self.partition = partition;
        self
    }

    /// Attaches the per-capability metrics: `http`/`s3` from the engine outcome, the
    /// driver-backed capabilities from the egress adapter.
    fn with_metrics(mut self, metrics: ExecMetrics, backend: BackendMetrics) -> Self {
        self.http_requests = metrics.http;
        self.s3_requests = metrics.s3;
        self.db_requests = backend.db;
        self.mongo_requests = backend.mongo;
        self.mail_requests = backend.mail;
        self.redis_requests = backend.redis;
        self.amq_requests = backend.amq;
        self.auth_requests = backend.auth;
        self
    }
}

/// Success response: JS-produced `{data, error}` as borrowed `RawValue` + Rust meta.
#[derive(Debug, Serialize)]
struct Response<'a> {
    /// The data field from the JS handler (borrowed, never copied).
    data: &'a RawValue,
    /// The error field from the JS handler (borrowed, never copied; D1 passthrough).
    error: &'a RawValue,
    /// Metadata computed by Rust.
    meta: Meta,
}

/// System-error response: `data` is `null`, `error` is the structured envelope.
#[derive(Debug, Serialize)]
struct SystemErrorResponse {
    /// Always `null` on a system error.
    data: Option<()>,
    /// The structured error envelope.
    error: ErrorEnvelope,
    /// Metadata computed by Rust.
    meta: Meta,
}

/// Envelope parsed from the JS response — borrows from the source string.
#[derive(Deserialize)]
struct Envelope<'a> {
    /// Raw data from JS (zero-copy borrow).
    #[serde(default = "raw_null_ref", borrow)]
    data: &'a RawValue,
    /// Raw error from JS (zero-copy borrow).
    #[serde(default = "raw_null_ref", borrow)]
    error: &'a RawValue,
}

/// Returns a reference to the pre-allocated `null` raw value.
fn raw_null_ref() -> &'static RawValue {
    &RAW_NULL
}

/// Maps a [`SessionError`] (opening the `fabricd` session) to a response: a resolution failure is
/// the caller's `400`, an unreachable/absent sidecar is a retryable `503`, a protocol slip is a
/// `500`.
fn session_error_response(err: SessionError, meta: Meta) -> AxumResponse {
    match err {
        SessionError::Resolve { code, message } => {
            system_error_response(request_error(&code, message), 400, meta)
        }
        SessionError::Unavailable(message) => {
            let envelope = ErrorEnvelope::new(
                ErrorCategory::Runtime,
                ErrorSource::Engine,
                "EGRESS_UNAVAILABLE".to_owned(),
                true,
                ErrorOwner::Operator,
            )
            .with_message(message);
            system_error_response(envelope, 503, meta)
        }
        SessionError::Protocol(_raw) => {
            let envelope = ErrorEnvelope::new(
                ErrorCategory::Runtime,
                ErrorSource::Engine,
                "EGRESS_PROTOCOL".to_owned(),
                false,
                ErrorOwner::Operator,
            )
            .with_message("egress protocol error".to_owned());
            system_error_response(envelope, 500, meta)
        }
    }
}

/// Executes a JS `handler(context)` and returns `{data, error, meta}` JSON.
///
/// Takes `Result<Json<…>, JsonRejection>` rather than `Json<…>` so a malformed or
/// type-confused body is handled here as a structured `{data, error, meta}` envelope,
/// instead of axum short-circuiting with its default plain-text rejection.
#[expect(
    clippy::too_many_lines,
    reason = "linear request pipeline: auth → open egress session → admit → execute → respond"
)]
pub(crate) async fn execute(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<ExecRequest>, JsonRejection>,
) -> impl IntoResponse {
    // Auth gate (defense in depth) — reject before any body work.
    if let Some(rejected) = enforce_auth(&state, &headers) {
        return rejected;
    }

    let req = match payload {
        Ok(Json(req)) => req,
        Err(rejection) => {
            state.metrics.record_rejection();
            return malformed_request_response(&state, &rejection);
        }
    };
    let ExecRequest {
        script,
        key,
        partition: body_partition,
        context,
        config,
    } = req;
    // Partition key (Tier 5): `X-Partition-Key` header wins over the body field; caller-set.
    let partition = header_partition(&headers).or(body_partition);
    let context_bytes = context.get().len();

    let engine_cfg = state.engine_cfg;
    let error_debug = state.error_debug;
    let trace_id = Uuid::new_v4().to_string();

    // Resolve exactly one of `script` / `key` into the source to execute.
    let source = match resolve_script(script, key.as_deref(), &state.registry) {
        Ok(source) => source,
        Err(rejection) => {
            state.metrics.record_rejection();
            let (status, envelope) = *rejection;
            let meta = Meta::new(trace_id, 0, context_bytes, 0)
                .with_key(key)
                .with_partition(partition);
            return system_error_response(envelope, status, meta);
        }
    };
    let script_bytes = source.as_str().len();

    // Early validation — reject oversized inputs before spawning a task.
    if let Err((code, message)) = sandbox::validate_input_sizes(
        script_bytes,
        context_bytes,
        engine_cfg.max_script_size,
        engine_cfg.max_context_size,
    ) {
        state.metrics.record_rejection();
        let meta = Meta::new(trace_id, script_bytes, context_bytes, 0)
            .with_key(key)
            .with_partition(partition);
        return system_error_response(request_error(code, message), 400, meta);
    }

    let context_json: String = context.get().into();
    let gates = config.io.gates();

    // Open the `fabricd` egress session when any driver capability is requested. The box holds no
    // credentials: it sends the selected logical names; `fabricd` resolves them against its own
    // operator config. An unknown name (400), or an unreachable/absent sidecar (503), is rejected
    // here — before admission — exactly where the old in-box resolution used to reject.
    let session = if config.io.any() {
        match connect_session(&state.transport, &config.io.wire_init(engine_cfg.timeout())).await {
            Ok(conn) => Some(conn),
            Err(err) => {
                state.metrics.record_rejection();
                let meta = Meta::new(trace_id, script_bytes, context_bytes, 0)
                    .with_key(key)
                    .with_partition(partition);
                return session_error_response(err, meta);
            }
        }
    } else {
        None
    };

    let start = Instant::now();

    // Acquire the per-partition (Tier 5) then global bulkhead (Tier 1) permits.
    let busy_meta = base_error_meta(
        &trace_id,
        script_bytes,
        context_bytes,
        key.as_deref(),
        partition.as_deref(),
    );
    let (partition_permit, permit) = match admit(&state, partition.as_deref(), busy_meta) {
        Ok(permits) => permits,
        Err(shed) => return *shed,
    };

    // The blocking task wraps the pre-connected `fabricd` session as the egress, runs the
    // invocation, then drains the session's metrics — the UDS round-trips and the drain `block_on`,
    // which must run on the `spawn_blocking` thread, not a runtime worker. The driver metrics come
    // back with the outcome so the response `meta.<cap>_requests` is unchanged.
    let host = state.host.clone();
    let handle = Handle::current();
    let timeout = engine_cfg.timeout();
    // Namespace the bytecode cache by partition so identical source from different tenants
    // never shares an entry (no cross-tenant dedup / compile-timing leak). Cloned because the
    // closure is `move` and `partition` is still needed for `meta` after the await.
    let cache_ns = partition.clone();
    let result = task::spawn_blocking(move || -> (Result<Outcome, EngineError>, BackendMetrics) {
        let adapter =
            session.map(|conn| Arc::new(SidecarEgress::new(conn, handle.clone(), timeout)));
        let egress: Option<Arc<dyn Egress>> = adapter.as_ref().map(|metered| {
            // Upcast `Arc<SidecarEgress>` → `Arc<dyn Egress>`; the turbofish pins the source
            // type so the clone resolves before the coercion (the original `adapter` stays for
            // draining).
            let dynamic: Arc<dyn Egress> = Arc::<SidecarEgress>::clone(metered);
            dynamic
        });
        // `Invocation` is `#[non_exhaustive]`; build via the constructor + builder setters. The
        // HTTP front always runs the full-capability profile (the default) with no read-hook,
        // so only `caps`, the egress port, and the cache namespace differ from the defaults.
        let caps = CapabilitySet {
            allowed_hosts: &config.allowed_hosts,
            db: gates.db,
            mongo: gates.mongo,
            mail: gates.mail,
            s3: config.s3.as_ref(),
            redis: gates.redis,
            amq: gates.amq,
            auth: gates.auth,
            sys: config.sys.as_ref(),
        };
        let mut invocation = Invocation::inline(source.as_str(), &context_json).caps(caps);
        if let Some(port) = egress {
            invocation = invocation.egress(port);
        }
        if let Some(namespace) = cache_ns.as_deref() {
            invocation = invocation.cache_namespace(namespace);
        }
        let outcome = host.run(invocation);
        let metrics =
            adapter.map_or_else(BackendMetrics::default, |metered| metered.drain_metrics());
        (outcome, metrics)
    })
    .await;

    // Execution finished — free the bulkhead + per-partition permits for the next request.
    drop(permit);
    drop(partition_permit);

    let exec_time_us = start.elapsed().as_micros();
    let base_meta = Meta::new(trace_id, script_bytes, context_bytes, exec_time_us)
        .with_key(key)
        .with_partition(partition);
    build_response(result, base_meta, error_debug, &state.metrics)
}

/// `GET /metrics` — Prometheus text exposition of the process-wide counters and live
/// gauges (bulkhead permits read off the semaphore).
pub(crate) async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    let available = state.limiter.available_permits();
    // The db circuit breaker moved to `fabricd` (it owns the driver connections now); the box
    // reports zero trips, keeping the `jsbox_db_breaker_trips_total` series for scrape compatibility.
    let trips = 0_u64;
    let cache = state.host.bytecode_cache_stats();
    let body = state
        .metrics
        .render(available, state.bulkhead_capacity, trips, cache);
    (
        StatusCode::OK,
        [(CONTENT_TYPE, "text/plain; version=0.0.4")],
        body,
    )
}

/// Turns the `spawn_blocking` result into the final HTTP response, attaching metrics to
/// `meta` on success and classifying the error otherwise.
fn build_response(
    result: Result<(Result<Outcome, EngineError>, BackendMetrics), task::JoinError>,
    base_meta: Meta,
    error_debug: bool,
    metrics: &Metrics,
) -> AxumResponse {
    // Record latency for every execution that ran (shed/rejected requests return earlier).
    metrics.observe_execution(base_meta.exec_time_us);
    match result {
        Ok((Ok(exec), backend)) => {
            record_capability_latencies(metrics, &exec.metrics, &backend);
            // `exec.effects` (the declarative `emit` buffer) is intentionally ignored by the
            // HTTP front — it is for non-HTTP consumers of `runlet-core`.
            let meta = base_meta.with_metrics(exec.metrics, backend);
            match exec.result {
                ExecOutcome::Success(js_json) => {
                    metrics.record_success();
                    success_response(&js_json, meta, error_debug)
                }
                ExecOutcome::Error(engine_err) => {
                    metrics.record_engine_error(&engine_err);
                    engine_error_response(engine_err, meta, error_debug)
                }
            }
        }
        Ok((Err(engine_err), _backend)) => {
            metrics.record_engine_error(&engine_err);
            engine_error_response(engine_err, base_meta, error_debug)
        }
        Err(join_err) => {
            let engine_err = EngineError::Internal(format!("task panicked: {join_err}"));
            metrics.record_engine_error(&engine_err);
            engine_error_response(engine_err, base_meta, error_debug)
        }
    }
}

/// Feeds every per-op duration from a finished execution into its capability's latency
/// histogram, so `/metrics` can show which downstream is slow, not just total exec time.
/// `http`/`s3` come from the engine outcome; the driver-backed capabilities from the adapter.
fn record_capability_latencies(metrics: &Metrics, exec: &ExecMetrics, backend: &BackendMetrics) {
    for metric in &backend.db {
        metrics.observe_op(Capability::Db, metric.duration_us());
    }
    for metric in &backend.mongo {
        metrics.observe_op(Capability::Mongo, metric.duration_us());
    }
    for metric in &exec.http {
        metrics.observe_op(Capability::Http, metric.duration_us());
    }
    for metric in &backend.mail {
        metrics.observe_op(Capability::Mail, metric.duration_us());
    }
    for metric in &exec.s3 {
        metrics.observe_op(Capability::S3, metric.duration_us());
    }
    for metric in &backend.redis {
        metrics.observe_op(Capability::Redis, metric.duration_us());
    }
    for metric in &backend.amq {
        metrics.observe_op(Capability::Amq, metric.duration_us());
    }
    for metric in &backend.auth {
        metrics.observe_op(Capability::Auth, metric.duration_us());
    }
}

/// Resolves the script source for a request: exactly one of `script` / `key` must be
/// present; a `key` is looked up in the registry.
///
/// # Errors
///
/// Returns the HTTP status + envelope for the violation (boxed — the happy path
/// shouldn't carry the envelope's size): 400 `SCRIPT_XOR_KEY` when not exactly one of
/// the two is present, 404 `SCRIPT_NOT_FOUND` for an unknown key.
fn resolve_script(
    script: Option<String>,
    key: Option<&str>,
    registry: &ScriptRegistry,
) -> Result<ScriptSource, Box<(u16, ErrorEnvelope)>> {
    match (script, key) {
        (Some(source), None) => Ok(ScriptSource::Inline(source)),
        (None, Some(requested)) => registry
            .get(requested)
            .map(ScriptSource::Registered)
            .ok_or_else(|| {
                Box::new((
                    404,
                    request_error(
                        "SCRIPT_NOT_FOUND",
                        format!("no registered script for key `{requested}`"),
                    ),
                ))
            }),
        (Some(_), Some(_)) | (None, None) => Err(Box::new((
            400,
            request_error(
                "SCRIPT_XOR_KEY",
                "request must include exactly one of `script` or `key`".to_owned(),
            ),
        ))),
    }
}

/// Builds the structured response for a request body that failed to parse or extract
/// (bad JSON, wrong field types, oversized body). Returns the same `{data, error, meta}`
/// envelope as every other error path — never axum's default plain-text rejection — so
/// a client that always parses the envelope never has to special-case malformed input.
/// The rejection's own text is surfaced only in gated `debug.raw`.
fn malformed_request_response(state: &AppState, rejection: &JsonRejection) -> AxumResponse {
    let trace_id = Uuid::new_v4().to_string();
    let base = request_error(
        "MALFORMED_REQUEST",
        "request body is not valid for /execute".to_owned(),
    );
    let envelope = if state.error_debug {
        base.with_debug(ErrorDebug {
            stack: None,
            raw: Some(rejection.body_text()),
        })
    } else {
        base
    };
    system_error_response(envelope, 400, Meta::new(trace_id, 0, 0, 0))
}

/// Builds the `429 OVERLOADED` response when the bulkhead is saturated: a runtime-category
/// envelope, retryable, owned by the operator (capacity, not the caller's request).
fn overloaded_response(meta: Meta) -> AxumResponse {
    let envelope = ErrorEnvelope::new(
        ErrorCategory::Runtime,
        ErrorSource::Engine,
        "OVERLOADED".to_owned(),
        true,
        ErrorOwner::Operator,
    )
    .with_message("server at capacity, retry shortly".to_owned());
    system_error_response(envelope, 429, meta)
}

/// Builds the `429 PARTITION_OVERLOADED` response (Tier 5): this partition exceeded its
/// concurrency share while global capacity may remain — the caller (that partition) should
/// back off, so it's owned by the caller, retryable.
fn partition_overloaded_response(meta: Meta) -> AxumResponse {
    let envelope = ErrorEnvelope::new(
        ErrorCategory::Runtime,
        ErrorSource::Engine,
        "PARTITION_OVERLOADED".to_owned(),
        true,
        ErrorOwner::Caller,
    )
    .with_message("partition concurrency limit reached, retry shortly".to_owned());
    system_error_response(envelope, 429, meta)
}

/// Outcome of acquiring the per-partition (Tier 5) + global bulkhead (Tier 1) permits.
enum Admission {
    /// Both granted — hold for the execution. `partition_permit` is `None` when no partition
    /// was supplied or fairness is disabled.
    Granted {
        /// Per-partition permit (Tier 5).
        partition_permit: Option<OwnedSemaphorePermit>,
        /// Global bulkhead permit (Tier 1).
        global: OwnedSemaphorePermit,
    },
    /// The partition exceeded its per-partition share (`429 PARTITION_OVERLOADED`).
    PartitionBusy,
    /// The global bulkhead is saturated (`429 OVERLOADED`).
    GlobalBusy,
}

/// Acquires the partition (Tier 5) + global bulkhead (Tier 1) permits, recording the shed
/// and returning the ready-to-send `429` response when either limit is hit. `Ok` carries
/// the permits to hold across the execution span. `busy_meta` is consumed only on a shed.
fn admit(
    state: &AppState,
    partition: Option<&str>,
    busy_meta: Meta,
) -> Result<(Option<OwnedSemaphorePermit>, OwnedSemaphorePermit), Box<AxumResponse>> {
    match acquire_permits(state, partition) {
        Admission::Granted {
            partition_permit,
            global,
        } => Ok((partition_permit, global)),
        Admission::PartitionBusy => {
            state.metrics.record_overload_partition();
            Err(Box::new(partition_overloaded_response(busy_meta)))
        }
        Admission::GlobalBusy => {
            state.metrics.record_overload_global();
            Err(Box::new(overloaded_response(busy_meta)))
        }
    }
}

/// Acquires the per-partition permit (if a partition is supplied and fairness is on) then the
/// global bulkhead permit. Per-partition first, so a noisy partition fast-fails on its own
/// share before consuming a global slot.
fn acquire_permits(state: &AppState, partition: Option<&str>) -> Admission {
    let partition_permit = if let (Some(limiter), Some(id)) = (&state.partition_limiter, partition)
    {
        let Some(permit) = limiter.try_acquire(id) else {
            return Admission::PartitionBusy;
        };
        Some(permit)
    } else {
        None
    };
    match Arc::clone(&state.limiter).try_acquire_owned() {
        Ok(global) => Admission::Granted {
            partition_permit,
            global,
        },
        Err(_too_busy) => Admission::GlobalBusy,
    }
}

/// Builds a zero-timing `Meta` for an early error return, cloning the correlation fields
/// (which the caller still needs on the continuing path).
fn base_error_meta(
    trace_id: &str,
    script_bytes: usize,
    context_bytes: usize,
    key: Option<&str>,
    partition: Option<&str>,
) -> Meta {
    Meta::new(trace_id.to_owned(), script_bytes, context_bytes, 0)
        .with_key(key.map(str::to_owned))
        .with_partition(partition.map(str::to_owned))
}

/// Reads the partition key from the `X-Partition-Key` header (trimmed, non-empty). Takes
/// precedence over the request body's `partition` field.
fn header_partition(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-partition-key")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

/// Enforces the optional `/execute` bearer gate. Returns `Some(401)` when a token is
/// configured and the request doesn't present a matching one; `None` when auth passes or no
/// token is configured (auth handled upstream / loopback bind).
fn enforce_auth(state: &AppState, headers: &HeaderMap) -> Option<AxumResponse> {
    let expected = state.access_token.as_deref()?;
    if request_authorized(headers, expected) {
        return None;
    }
    state.metrics.record_rejection();
    Some(unauthorized_response())
}

/// Returns `true` if the request carries a valid `Authorization: Bearer <token>` matching
/// `expected`. The token is compared in constant time so a timing side-channel can't recover
/// it byte by byte.
fn request_authorized(headers: &HeaderMap, expected: &str) -> bool {
    let Some(value) = headers.get(AUTHORIZATION).and_then(|raw| raw.to_str().ok()) else {
        return false;
    };
    let Some(token) = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))
    else {
        return false;
    };
    ct_eq(token.trim().as_bytes(), expected.as_bytes())
}

/// Constant-time byte-slice equality. Length difference returns early (a token's length is
/// not the secret); equal-length inputs are compared without an early exit.
fn ct_eq(lhs: &[u8], rhs: &[u8]) -> bool {
    if lhs.len() != rhs.len() {
        return false;
    }
    let mut acc = 0_u8;
    for (left, right) in lhs.iter().zip(rhs.iter()) {
        acc |= left ^ right;
    }
    acc == 0
}

/// Builds the `401 UNAUTHORIZED` response for a missing/invalid bearer token.
fn unauthorized_response() -> AxumResponse {
    let trace_id = Uuid::new_v4().to_string();
    let envelope = request_error("UNAUTHORIZED", "missing or invalid bearer token".to_owned());
    system_error_response(envelope, 401, Meta::new(trace_id, 0, 0, 0))
}

/// Builds a `request`-category envelope (the caller's fault, never retryable).
fn request_error(code: &str, message: String) -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCategory::Request,
        ErrorSource::Request,
        code.to_owned(),
        false,
        ErrorOwner::Caller,
    )
    .with_message(message)
}

/// Builds the success response, or a `MALFORMED_RESPONSE` error if the JS envelope
/// can't be parsed.
///
/// Secrets need no output scrubbing: their plaintext never enters JS — it stays
/// Rust-side as opaque handles (see `sys.rs`), so a script can only ever return the
/// `"[secret:NAME]"` placeholder, never the value. The `{data,error}` borrow stays
/// zero-copy.
fn success_response(js_json: &str, meta: Meta, error_debug: bool) -> AxumResponse {
    match serde_json::from_str::<Envelope<'_>>(js_json) {
        Ok(env) => (
            StatusCode::OK,
            Json(Response {
                data: env.data,
                error: env.error,
                meta,
            }),
        )
            .into_response(),
        Err(parse_err) => engine_error_response(
            EngineError::Malformed(format!("malformed handler response: {parse_err}")),
            meta,
            error_debug,
        ),
    }
}

/// Maps a classified [`EngineError`] to its envelope (debug-gated) + HTTP status, and
/// logs the full (raw) error server-side keyed by `trace_id` — so the raw cause is
/// always captured for support even when `error_debug` strips it from the response.
fn engine_error_response(err: EngineError, meta: Meta, error_debug: bool) -> AxumResponse {
    let status = err.http_status();
    warn!(trace_id = %meta.trace_id, status, error = ?err, "execute system error");
    let envelope = err.into_envelope(error_debug);
    system_error_response(envelope, status, meta)
}

/// Serializes a `{ data: null, error, meta }` response at the given status.
fn system_error_response(error: ErrorEnvelope, status: u16, meta: Meta) -> AxumResponse {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (
        code,
        Json(SystemErrorResponse {
            data: None,
            error,
            meta,
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    //! `/execute` bearer-auth gate: constant-time compare + `Authorization` header parsing.

    use super::{ct_eq, request_authorized};
    use axum::http::HeaderMap;
    use axum::http::HeaderValue;
    use axum::http::header::AUTHORIZATION;

    /// A `HeaderMap` carrying a single `Authorization` header value.
    fn with_auth(value: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        drop(headers.insert(AUTHORIZATION, HeaderValue::from_static(value)));
        headers
    }

    /// Constant-time compare is true only for byte-identical inputs (incl. equal length).
    #[test]
    fn ct_eq_matches_only_identical_bytes() {
        assert!(ct_eq(b"s3cret-token", b"s3cret-token"), "identical matches");
        assert!(
            !ct_eq(b"s3cret-token", b"s3cret-tokeX"),
            "same length, one byte off"
        );
        assert!(
            !ct_eq(b"short", b"longer-token"),
            "different length differs"
        );
        assert!(ct_eq(b"", b""), "empty equals empty");
    }

    /// A matching bearer token authorizes, case-insensitively on the scheme.
    #[test]
    fn authorized_accepts_matching_bearer() {
        assert!(
            request_authorized(&with_auth("Bearer s3cret"), "s3cret"),
            "exact match authorizes"
        );
        assert!(
            request_authorized(&with_auth("bearer s3cret"), "s3cret"),
            "lowercase scheme authorizes"
        );
    }

    /// A wrong, prefix-less, empty, or absent token is rejected.
    #[test]
    fn authorized_rejects_bad_or_missing() {
        assert!(
            !request_authorized(&with_auth("Bearer wrong"), "s3cret"),
            "wrong token rejected"
        );
        assert!(
            !request_authorized(&with_auth("s3cret"), "s3cret"),
            "missing Bearer prefix rejected"
        );
        assert!(
            !request_authorized(&HeaderMap::new(), "s3cret"),
            "absent header rejected"
        );
        assert!(
            !request_authorized(&with_auth("Bearer "), "s3cret"),
            "empty token rejected"
        );
    }
}

#[cfg(test)]
mod request_io_tests {
    //! The box-side `config.io` interpretation: which capabilities are requested, the engine
    //! gates, and the `fabricd` session-open `WireInit` (the first name per kind). Name→config
    //! resolution itself now lives in `fabricd` (`fabric_backends::resolve`).

    use super::RequestIo;

    /// A `RequestIo` naming one db resource (other kinds empty).
    fn io_db(name: &str) -> RequestIo {
        RequestIo {
            db: vec![name.to_owned()],
            ..RequestIo::default()
        }
    }

    /// `any()` is true iff some kind is named; `gates()` mirror per-kind presence.
    #[test]
    fn any_and_gates_track_named_kinds() {
        let io = io_db("orders-db");
        assert!(io.any(), "a named db means a session is needed");
        assert!(io.gates().db.is_on(), "db gate on");
        assert!(!io.gates().mongo.is_on(), "unnamed kinds stay off");

        let empty = RequestIo::default();
        assert!(!empty.any(), "no names → no session");
        assert!(!empty.gates().db.is_on());
    }

    /// `wire_init` carries the first name per kind and the deadline.
    #[test]
    fn wire_init_selects_first_name() {
        let io = RequestIo {
            db: vec!["orders-db".to_owned(), "ignored".to_owned()],
            ..RequestIo::default()
        };
        let init = io.wire_init(std::time::Duration::from_millis(1500));
        assert_eq!(init.db.as_deref(), Some("orders-db"), "first name selected");
        assert_eq!(init.mongo, None, "unnamed kinds stay None");
        assert_eq!(init.timeout_ms, 1500);
    }
}
