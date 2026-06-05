//! HTTP handler for the `/execute` endpoint.

use std::sync::LazyLock;
use std::time::Instant;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response as AxumResponse};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use tokio::task;
use tracing::warn;
use uuid::Uuid;

use crate::amq::{AmqConfig, AmqMetric};
use crate::db::{DbConfig, DbMetric};
use crate::engine::{self, EngineError, ExecOutcome, ExecParams, ExecResult};
use crate::errors::{ErrorCategory, ErrorEnvelope, ErrorOwner, ErrorSource};
use crate::http::HttpMetric;
use crate::kv::{RedisConfig, RedisMetric};
use crate::mail::{MailConfig, MailMetric};
use crate::pool::JsPool;
use crate::s3::{S3Config, S3Metric};
use crate::sandbox;
use crate::sys::SysConfig;

/// Pre-allocated `Box<RawValue>` for `{}` — used as default context.
static DEFAULT_CONTEXT: LazyLock<Box<RawValue>> =
    LazyLock::new(|| RawValue::from_string("{}".into()).unwrap_or_else(|_err| unreachable!()));

/// Pre-allocated `Box<RawValue>` for `null` — used as default envelope field.
static RAW_NULL: LazyLock<Box<RawValue>> =
    LazyLock::new(|| RawValue::from_string("null".into()).unwrap_or_else(|_err| unreachable!()));

/// Request body for script execution.
#[derive(Debug, Deserialize)]
pub(crate) struct ExecRequest {
    /// The JavaScript source code to evaluate.
    script: String,
    /// Raw context passed straight to `QuickJS` — never deserialized in Rust.
    #[serde(default = "default_context")]
    context: Box<RawValue>,
    /// Per-request configuration.
    #[serde(default)]
    config: RequestConfig,
}

/// Per-request configuration sent by the caller.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RequestConfig {
    /// Allowed hosts for the `api` HTTP client.
    #[serde(default)]
    pub(crate) allowed_hosts: Vec<String>,
    /// Database connection config (omit to disable `db` in JS).
    #[serde(default)]
    pub(crate) db: Option<DbConfig>,
    /// Mail/SMTP config (omit to disable `mail` in JS).
    #[serde(default)]
    pub(crate) mail: Option<MailConfig>,
    /// S3 presigning config (omit to disable `s3` in JS).
    #[serde(default)]
    pub(crate) s3: Option<S3Config>,
    /// Redis config (omit to disable `redis` in JS).
    #[serde(default)]
    pub(crate) redis: Option<RedisConfig>,
    /// `RabbitMQ` config (omit to disable `amq` in JS).
    #[serde(default)]
    pub(crate) amq: Option<AmqConfig>,
    /// `$sys` env/secrets context (omit to leave `$sys.env`/`$sys.secrets` empty).
    #[serde(default)]
    pub(crate) sys: Option<SysConfig>,
}

/// Returns a clone of the pre-allocated default context.
fn default_context() -> Box<RawValue> {
    DEFAULT_CONTEXT.clone()
}

/// Per-capability metrics drained from one execution.
#[derive(Debug, Default)]
struct ExecMetrics {
    /// HTTP request metrics.
    http: Vec<HttpMetric>,
    /// DB operation metrics.
    db: Vec<DbMetric>,
    /// Mail operation metrics.
    mail: Vec<MailMetric>,
    /// S3 presign metrics.
    s3: Vec<S3Metric>,
    /// Redis operation metrics.
    redis: Vec<RedisMetric>,
    /// `RabbitMQ` operation metrics.
    amq: Vec<AmqMetric>,
}

/// Metadata computed by Rust.
#[derive(Debug, Serialize)]
struct Meta {
    /// Correlation ID — also logged server-side with the raw cause, so support can grep
    /// one ID across the mesh. Present on every response (success and error).
    trace_id: String,
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
    /// Mail operations made by the script.
    mail_requests: Vec<MailMetric>,
    /// S3 presign operations made by the script.
    s3_requests: Vec<S3Metric>,
    /// Redis operations made by the script.
    redis_requests: Vec<RedisMetric>,
    /// `RabbitMQ` operations made by the script.
    amq_requests: Vec<AmqMetric>,
}

impl Meta {
    /// Creates a new `Meta` with the given correlation ID, sizes, and empty metrics.
    const fn new(trace_id: String, script_bytes: usize, context_bytes: usize, exec_time_us: u128) -> Self {
        Self {
            trace_id,
            script_bytes,
            context_bytes,
            total_input_bytes: script_bytes.saturating_add(context_bytes),
            exec_time_us,
            http_requests: Vec::new(),
            db_requests: Vec::new(),
            mail_requests: Vec::new(),
            s3_requests: Vec::new(),
            redis_requests: Vec::new(),
            amq_requests: Vec::new(),
        }
    }

    /// Attaches HTTP, DB, mail, S3, Redis, and `RabbitMQ` metrics to this metadata.
    fn with_metrics(mut self, metrics: ExecMetrics) -> Self {
        self.http_requests = metrics.http;
        self.db_requests = metrics.db;
        self.mail_requests = metrics.mail;
        self.s3_requests = metrics.s3;
        self.redis_requests = metrics.redis;
        self.amq_requests = metrics.amq;
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

/// Executes a JS `handler(context)` and returns `{data, error, meta}` JSON.
pub(crate) async fn execute(
    State(js_pool): State<JsPool>,
    Json(req): Json<ExecRequest>,
) -> impl IntoResponse {
    let script_bytes = req.script.len();
    let context_bytes = req.context.get().len();

    let engine_cfg = js_pool.engine_config().clone();
    let error_debug = js_pool.error_debug();
    let trace_id = Uuid::new_v4().to_string();

    // Early validation — reject oversized inputs before spawning a task.
    if let Err((code, message)) = sandbox::validate_input_sizes(
        script_bytes, context_bytes,
        engine_cfg.max_script_size, engine_cfg.max_context_size,
    ) {
        let envelope = ErrorEnvelope::new(
            ErrorCategory::Request,
            ErrorSource::Request,
            code.to_owned(),
            false,
            ErrorOwner::Caller,
        )
        .with_message(message);
        return system_error_response(envelope, 400, Meta::new(trace_id, script_bytes, context_bytes, 0));
    }

    let script = req.script;
    let context_json: String = req.context.get().into();
    let allowed_hosts = req.config.allowed_hosts;
    let db_config = req.config.db;
    let mail_config = req.config.mail;
    let s3_config = req.config.s3;
    let redis_config = req.config.redis;
    let amq_config = req.config.amq;
    let sys_config = req.config.sys;

    let start = Instant::now();
    let allow_private_targets = js_pool.debug();

    let result = task::spawn_blocking(move || -> Result<ExecResult, EngineError> {
        let runtime = js_pool.acquire().map_err(|err| EngineError::Internal(err.to_string()))?;
        let res = engine::run(&ExecParams {
            runtime: &runtime,
            script: &script,
            context_json: &context_json,
            timeout: engine_cfg.timeout(),
            allowed_hosts: &allowed_hosts,
            db_config: db_config.as_ref(),
            mail_config: mail_config.as_ref(),
            s3_config: s3_config.as_ref(),
            redis_config: redis_config.as_ref(),
            amq_config: amq_config.as_ref(),
            sys_config: sys_config.as_ref(),
            max_ops: engine_cfg.max_ops,
            allow_private_targets,
        });
        js_pool.release(runtime);
        res
    })
    .await;

    let exec_time_us = start.elapsed().as_micros();
    let base_meta = || Meta::new(trace_id.clone(), script_bytes, context_bytes, exec_time_us);

    match result {
        Ok(Ok(exec)) => {
            let metrics = ExecMetrics {
                http: exec.http_metrics,
                db: exec.db_metrics,
                mail: exec.mail_metrics,
                s3: exec.s3_metrics,
                redis: exec.redis_metrics,
                amq: exec.amq_metrics,
            };
            let meta = base_meta().with_metrics(metrics);
            match exec.outcome {
                ExecOutcome::Success(js_json) => {
                    success_response(&js_json, meta, error_debug)
                }
                ExecOutcome::Error(engine_err) => {
                    engine_error_response(engine_err, meta, error_debug)
                }
            }
        }
        Ok(Err(engine_err)) => engine_error_response(engine_err, base_meta(), error_debug),
        Err(join_err) => engine_error_response(
            EngineError::Internal(format!("task panicked: {join_err}")),
            base_meta(),
            error_debug,
        ),
    }
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
            Json(Response { data: env.data, error: env.error, meta }),
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
    (code, Json(SystemErrorResponse { data: None, error, meta })).into_response()
}
