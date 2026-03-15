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
}

/// Returns a clone of the pre-allocated default context.
fn default_context() -> Box<RawValue> {
    DEFAULT_CONTEXT.clone()
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
        }
    }

    /// Attaches HTTP and DB metrics to this metadata.
    fn with_metrics(mut self, http_requests: Vec<HttpMetric>, db_requests: Vec<DbMetric>) -> Self {
        self.http_requests = http_requests;
        self.db_requests = db_requests;
        self
    }
}

/// Full response: JS-produced `{data, errors}` as borrowed `RawValue` + Rust meta.
#[derive(Debug, Serialize)]
struct Response<'a> {
    /// The data field from the JS handler (borrowed, never copied).
    data: &'a RawValue,
    /// The errors field from the JS handler (borrowed, never copied).
    errors: &'a RawValue,
    /// Metadata computed by Rust.
    meta: Meta,
}

/// Infrastructure error response (runtime failures, syntax errors).
#[derive(Debug, Serialize)]
struct InfraResponse {
    /// Always null on infra failure.
    data: Option<()>,
    /// The error detail.
    errors: InfraErrorDetail,
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
    /// Raw errors from JS (zero-copy borrow).
    #[serde(default = "raw_null_ref", borrow)]
    errors: &'a RawValue,
}

/// Returns a reference to the pre-allocated `null` raw value.
fn raw_null_ref() -> &'static RawValue {
    &RAW_NULL
}

/// Executes a JS `handler(context)` and returns `{data, errors, meta}` JSON.
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

    let start = Instant::now();

    let result = task::spawn_blocking(move || -> Result<ExecResult, Box<dyn Error + Send + Sync>> {
        let runtime = js_pool.acquire()?;
        let res = engine::run(&ExecParams {
            runtime: &runtime,
            script: &script,
            context_json: &context_json,
            timeout: engine_cfg.timeout(),
            allowed_hosts: &allowed_hosts,
            db_config: db_config.as_ref(),
            max_ops: engine_cfg.max_ops,
        });
        js_pool.release(runtime);
        res
    })
    .await;

    let exec_time_us = start.elapsed().as_micros();

    // Extract metrics from the result (or empty if it failed).
    let (engine_result, http_requests, db_requests) = match result {
        Ok(Ok(exec)) => (Ok(exec.js_json), exec.http_metrics, exec.db_metrics),
        Ok(Err(err)) => (Err(err), Vec::new(), Vec::new()),
        Err(join_err) => {
            return infra_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("task panicked: {join_err}"),
                Meta::new(script_bytes, context_bytes, exec_time_us),
            );
        }
    };

    let meta = Meta::new(script_bytes, context_bytes, exec_time_us)
        .with_metrics(http_requests, db_requests);

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
                Json(Response { data: env.data, errors: env.errors, meta }),
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
            errors: InfraErrorDetail { message },
            meta,
        }),
    )
        .into_response()
}
