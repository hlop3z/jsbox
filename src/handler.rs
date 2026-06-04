//! HTTP handler for the `/execute` endpoint.

use std::error::Error;
use std::sync::LazyLock;
use std::time::Instant;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response as AxumResponse};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use tokio::task;

use crate::db::{DbConfig, DbMetric};
use crate::engine;
use crate::sandbox;
use crate::engine::{ExecParams, ExecResult};
use crate::http::HttpMetric;
use crate::mail::{MailConfig, MailMetric};
use crate::s3::{S3Config, S3Metric};
use crate::pool::JsPool;

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
}

/// Metadata computed by Rust.
#[derive(Debug, Serialize)]
struct Meta {
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
}

impl Meta {
    /// Creates a new `Meta` with the given sizes and empty metrics.
    const fn new(script_bytes: usize, context_bytes: usize, exec_time_us: u128) -> Self {
        Self {
            script_bytes,
            context_bytes,
            total_input_bytes: script_bytes.saturating_add(context_bytes),
            exec_time_us,
            http_requests: Vec::new(),
            db_requests: Vec::new(),
            mail_requests: Vec::new(),
            s3_requests: Vec::new(),
        }
    }

    /// Attaches HTTP, DB, mail, and S3 metrics to this metadata.
    fn with_metrics(mut self, metrics: ExecMetrics) -> Self {
        self.http_requests = metrics.http;
        self.db_requests = metrics.db;
        self.mail_requests = metrics.mail;
        self.s3_requests = metrics.s3;
        self
    }
}

/// Full response: JS-produced `{data, error}` as borrowed `RawValue` + Rust meta.
#[derive(Debug, Serialize)]
struct Response<'a> {
    /// The data field from the JS handler (borrowed, never copied).
    data: &'a RawValue,
    /// The error field from the JS handler (borrowed, never copied).
    error: &'a RawValue,
    /// Metadata computed by Rust.
    meta: Meta,
}

/// Infrastructure error response (runtime failures, syntax errors).
#[derive(Debug, Serialize)]
struct InfraResponse {
    /// Always null on infra failure.
    data: Option<()>,
    /// The error detail.
    error: InfraErrorDetail,
    /// Metadata computed by Rust.
    meta: Meta,
}

/// Detail for infrastructure errors.
#[derive(Debug, Serialize)]
struct InfraErrorDetail {
    /// The error message.
    message: String,
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

    // Early validation — reject oversized inputs before spawning a task.
    let engine_cfg = js_pool.engine_config().clone();

    if let Err(msg) = sandbox::validate_input_sizes(
        script_bytes, context_bytes,
        engine_cfg.max_script_size, engine_cfg.max_context_size,
    ) {
        return infra_error(
            StatusCode::BAD_REQUEST,
            msg,
            Meta::new(script_bytes, context_bytes, 0),
        );
    }

    let script = req.script;
    let context_json: String = req.context.get().into();
    let allowed_hosts = req.config.allowed_hosts;
    let db_config = req.config.db;
    let mail_config = req.config.mail;
    let s3_config = req.config.s3;

    let start = Instant::now();
    let allow_private_targets = js_pool.debug();

    let result = task::spawn_blocking(move || -> Result<ExecResult, Box<dyn Error + Send + Sync>> {
        let runtime = js_pool.acquire()?;
        let res = engine::run(&ExecParams {
            runtime: &runtime,
            script: &script,
            context_json: &context_json,
            timeout: engine_cfg.timeout(),
            allowed_hosts: &allowed_hosts,
            db_config: db_config.as_ref(),
            mail_config: mail_config.as_ref(),
            s3_config: s3_config.as_ref(),
            max_ops: engine_cfg.max_ops,
            allow_private_targets,
        });
        js_pool.release(runtime);
        res
    })
    .await;

    let exec_time_us = start.elapsed().as_micros();

    // Extract metrics from the result (or empty if it failed).
    let (engine_result, metrics) = match result {
        Ok(Ok(exec)) => (
            Ok(exec.js_json),
            ExecMetrics {
                http: exec.http_metrics,
                db: exec.db_metrics,
                mail: exec.mail_metrics,
                s3: exec.s3_metrics,
            },
        ),
        Ok(Err(err)) => (Err(err), ExecMetrics::default()),
        Err(join_err) => {
            return infra_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("task panicked: {join_err}"),
                Meta::new(script_bytes, context_bytes, exec_time_us),
            );
        }
    };

    let meta = Meta::new(script_bytes, context_bytes, exec_time_us).with_metrics(metrics);

    build_response(&engine_result, meta)
}

/// Builds the final HTTP response from the engine result.
fn build_response(
    engine_result: &Result<String, Box<dyn Error + Send + Sync>>,
    meta: Meta,
) -> AxumResponse {
    match engine_result {
        Ok(js_json) => match serde_json::from_str::<Envelope<'_>>(js_json) {
            Ok(env) => (
                StatusCode::OK,
                Json(Response { data: env.data, error: env.error, meta }),
            )
                .into_response(),
            Err(parse_err) => infra_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("malformed handler response: {parse_err}"),
                meta,
            ),
        },
        Err(err) => infra_error(StatusCode::UNPROCESSABLE_ENTITY, err.to_string(), meta),
    }
}

/// Builds an infrastructure error response.
fn infra_error(status: StatusCode, message: String, meta: Meta) -> AxumResponse {
    (
        status,
        Json(InfraResponse {
            data: None,
            error: InfraErrorDetail { message },
            meta,
        }),
    )
        .into_response()
}
