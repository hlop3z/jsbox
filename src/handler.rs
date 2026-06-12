//! HTTP handler for the `/execute` endpoint.

use std::sync::{Arc, LazyLock};
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
use crate::auth::{AuthConfig, AuthMetric};
use crate::db::{DbConfig, DbMetric};
use crate::engine::{self, EngineError, ExecOutcome, ExecParams, ExecResult};
use crate::errors::{ErrorCategory, ErrorEnvelope, ErrorOwner, ErrorSource};
use crate::http::HttpMetric;
use crate::kv::{RedisConfig, RedisMetric};
use crate::mail::{MailConfig, MailMetric};
use crate::pool::JsPool;
use crate::registry::ScriptRegistry;
use crate::s3::{S3Config, S3Metric};
use crate::sandbox;
use crate::sys::SysConfig;

/// Shared application state for the router: the runtime pool + the script registry.
#[derive(Debug, Clone)]
pub(crate) struct AppState {
    /// Pool of pre-warmed `QuickJS` runtimes.
    pub(crate) pool: JsPool,
    /// Read-only registry of scripts loaded at startup (execute-by-key).
    pub(crate) registry: Arc<ScriptRegistry>,
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
    /// Auth (OIDC/IAM) config (omit to disable `auth` in JS).
    #[serde(default)]
    pub(crate) auth: Option<AuthConfig>,
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
    /// Auth operation metrics.
    auth: Vec<AuthMetric>,
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
    /// Auth operations made by the script.
    auth_requests: Vec<AuthMetric>,
}

impl Meta {
    /// Creates a new `Meta` with the given correlation ID, sizes, and empty metrics.
    const fn new(trace_id: String, script_bytes: usize, context_bytes: usize, exec_time_us: u128) -> Self {
        Self {
            trace_id,
            key: None,
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
            auth_requests: Vec::new(),
        }
    }

    /// Attaches the registered-script key (echoed back on key-mode requests).
    fn with_key(mut self, key: Option<String>) -> Self {
        self.key = key;
        self
    }

    /// Attaches HTTP, DB, mail, S3, Redis, and `RabbitMQ` metrics to this metadata.
    fn with_metrics(mut self, metrics: ExecMetrics) -> Self {
        self.http_requests = metrics.http;
        self.db_requests = metrics.db;
        self.mail_requests = metrics.mail;
        self.s3_requests = metrics.s3;
        self.redis_requests = metrics.redis;
        self.amq_requests = metrics.amq;
        self.auth_requests = metrics.auth;
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
    State(state): State<AppState>,
    Json(req): Json<ExecRequest>,
) -> impl IntoResponse {
    let ExecRequest { script, key, context, config } = req;
    let context_bytes = context.get().len();

    let engine_cfg = state.pool.engine_config().clone();
    let error_debug = state.pool.error_debug();
    let allow_private_targets = state.pool.debug();
    let trace_id = Uuid::new_v4().to_string();

    // Resolve exactly one of `script` / `key` into the source to execute.
    let source = match resolve_script(script, key.as_deref(), &state.registry) {
        Ok(source) => source,
        Err(rejection) => {
            let (status, envelope) = *rejection;
            let meta = Meta::new(trace_id, 0, context_bytes, 0).with_key(key);
            return system_error_response(envelope, status, meta);
        }
    };
    let script_bytes = source.as_str().len();

    // Early validation — reject oversized inputs before spawning a task.
    if let Err((code, message)) = sandbox::validate_input_sizes(
        script_bytes, context_bytes,
        engine_cfg.max_script_size, engine_cfg.max_context_size,
    ) {
        let meta = Meta::new(trace_id, script_bytes, context_bytes, 0).with_key(key);
        return system_error_response(request_error(code, message), 400, meta);
    }

    let context_json: String = context.get().into();
    let allowed_hosts = config.allowed_hosts;
    let db_config = config.db;
    let mail_config = config.mail;
    let s3_config = config.s3;
    let redis_config = config.redis;
    let amq_config = config.amq;
    let auth_config = config.auth;
    let sys_config = config.sys;

    let start = Instant::now();
    let js_pool = state.pool;

    let result = task::spawn_blocking(move || -> Result<ExecResult, EngineError> {
        let runtime = js_pool.acquire().map_err(|err| EngineError::Internal(err.to_string()))?;
        let res = engine::run(&ExecParams {
            runtime: &runtime,
            script: source.as_str(),
            context_json: &context_json,
            timeout: engine_cfg.timeout(),
            allowed_hosts: &allowed_hosts,
            db_config: db_config.as_ref(),
            mail_config: mail_config.as_ref(),
            s3_config: s3_config.as_ref(),
            redis_config: redis_config.as_ref(),
            amq_config: amq_config.as_ref(),
            auth_config: auth_config.as_ref(),
            sys_config: sys_config.as_ref(),
            max_ops: engine_cfg.max_ops,
            allow_private_targets,
        });
        js_pool.release(runtime);
        res
    })
    .await;

    let exec_time_us = start.elapsed().as_micros();
    let base_meta =
        || Meta::new(trace_id.clone(), script_bytes, context_bytes, exec_time_us).with_key(key.clone());

    match result {
        Ok(Ok(exec)) => {
            let metrics = ExecMetrics {
                http: exec.http_metrics,
                db: exec.db_metrics,
                mail: exec.mail_metrics,
                s3: exec.s3_metrics,
                redis: exec.redis_metrics,
                amq: exec.amq_metrics,
                auth: exec.auth_metrics,
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
        (None, Some(requested)) => {
            registry.get(requested).map(ScriptSource::Registered).ok_or_else(|| {
                Box::new((
                    404,
                    request_error(
                        "SCRIPT_NOT_FOUND",
                        format!("no registered script for key `{requested}`"),
                    ),
                ))
            })
        }
        (Some(_), Some(_)) | (None, None) => Err(Box::new((
            400,
            request_error(
                "SCRIPT_XOR_KEY",
                "request must include exactly one of `script` or `key`".to_owned(),
            ),
        ))),
    }
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
