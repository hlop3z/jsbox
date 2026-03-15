//! `QuickJS` execution engine — hardened sandbox for `handler(context)`.
//!
//! Uses `ctx.json_parse()` / `Function::call()` for direct C FFI data exchange.
//!
//! Sandbox: memory + stack limits, execution timeout, `eval()`/`Proxy` removed,
//! fresh context per request.

use std::error::Error;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rquickjs::{Context, Function, Object, Runtime, Value as JsValue};

use crate::db;
use crate::db::{DbConfig, DbMetric};
use crate::http;
use crate::http::HttpMetric;
use crate::sandbox::{self, Collector};

/// The `json()` bridge — loaded from `src/js/bridge.js` at compile time.
const JSON_BRIDGE: &str = include_str!("js/bridge.js");

/// Parameters for a single script execution.
pub(crate) struct ExecParams<'a> {
    /// The pooled runtime.
    pub(crate) runtime: &'a Runtime,
    /// JS script source.
    pub(crate) script: &'a str,
    /// Context JSON string.
    pub(crate) context_json: &'a str,
    /// Execution timeout.
    pub(crate) timeout: Duration,
    /// Allowed HTTP hosts (empty = disabled).
    pub(crate) allowed_hosts: &'a [String],
    /// Database config (None = disabled).
    pub(crate) db_config: Option<&'a DbConfig>,
    /// Max operations per execution.
    pub(crate) max_ops: usize,
}

/// Result of a script execution.
pub(crate) struct ExecResult {
    /// The JS-produced `{"data": ..., "errors": ...}` JSON string.
    pub(crate) js_json: String,
    /// HTTP requests made during execution.
    pub(crate) http_metrics: Vec<HttpMetric>,
    /// DB operations made during execution.
    pub(crate) db_metrics: Vec<DbMetric>,
}

/// Runs the script in a sandboxed context.
///
/// # Errors
///
/// Returns an error for infrastructure failures.
pub(crate) fn run(params: &ExecParams<'_>) -> Result<ExecResult, Box<dyn Error + Send + Sync>> {
    let timed_out = setup_timeout(params.runtime, params.timeout);

    let ctx = Context::full(params.runtime)?;

    let mut http_collector: Option<Collector<HttpMetric>> = None;
    let mut db_collector: Option<Collector<DbMetric>> = None;

    let js_result = ctx.with(|qctx| -> Result<String, Box<dyn Error + Send + Sync>> {
        inject_bridge(&qctx)?;
        inject_apis(&qctx, params, &mut http_collector, &mut db_collector)?;
        eval_script(&qctx, params.script)?;
        sanitize_globals(&qctx)?;
        call_handler(&qctx, params.context_json, &timed_out, params.timeout)
    });

    // Cleanup: clear interrupt handler so pooled runtime is clean.
    params.runtime.set_interrupt_handler(None);

    Ok(ExecResult {
        js_json: js_result?,
        http_metrics: sandbox::drain(http_collector.as_ref()),
        db_metrics: sandbox::drain(db_collector.as_ref()),
    })
}

// -- Setup helpers ----------------------------------------------------------

/// Configures the timeout interrupt handler. Returns the shared flag.
fn setup_timeout(runtime: &Runtime, timeout: Duration) -> Arc<AtomicBool> {
    let timed_out = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&timed_out);
    let start = Instant::now();
    runtime.set_interrupt_handler(Some(Box::new(move || {
        let exceeded = start.elapsed() > timeout;
        if exceeded {
            flag.store(true, Ordering::Relaxed);
        }
        exceeded
    })));
    timed_out
}

/// Injects the `json(data, errors)` bridge function.
fn inject_bridge(qctx: &rquickjs::Ctx<'_>) -> Result<(), Box<dyn Error + Send + Sync>> {
    let bridge: JsValue<'_> = qctx.eval(JSON_BRIDGE)?;
    drop(bridge);
    Ok(())
}

/// Injects HTTP and DB APIs if configured.
fn inject_apis(
    qctx: &rquickjs::Ctx<'_>,
    params: &ExecParams<'_>,
    http_collector: &mut Option<Collector<HttpMetric>>,
    db_collector: &mut Option<Collector<DbMetric>>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    if !params.allowed_hosts.is_empty() {
        *http_collector = Some(http::inject_api(qctx, params.allowed_hosts, params.max_ops)?);
    }
    if let Some(db_cfg) = params.db_config {
        *db_collector = Some(db::inject_db(qctx, db_cfg, params.max_ops)?);
    }
    Ok(())
}

/// Evaluates the user script.
fn eval_script(qctx: &rquickjs::Ctx<'_>, script: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
    let result: JsValue<'_> = qctx.eval(script)?;
    drop(result);
    Ok(())
}

/// Removes dangerous globals before handler runs.
fn sanitize_globals(qctx: &rquickjs::Ctx<'_>) -> Result<(), Box<dyn Error + Send + Sync>> {
    let globals = qctx.globals();
    globals.remove("eval")?;
    globals.remove("Proxy")?;
    Ok(())
}

/// Calls the user's `handler(context)` function and extracts the result.
fn call_handler(
    qctx: &rquickjs::Ctx<'_>,
    context_json: &str,
    timed_out: &AtomicBool,
    timeout: Duration,
) -> Result<String, Box<dyn Error + Send + Sync>> {
    let parsed_ctx: JsValue<'_> = qctx.json_parse(context_json)?;

    let handler: Function<'_> = qctx
        .globals()
        .get("handler")
        .map_err(|_err| -> Box<dyn Error + Send + Sync> {
            "script must define a `handler(context)` function".into()
        })?;

    let call_result: JsValue<'_> = match handler.call::<_, JsValue<'_>>((parsed_ctx,)) {
        Ok(val) => val,
        Err(err) => {
            let msg = if timed_out.load(Ordering::Relaxed) {
                format!("execution timed out ({}ms limit)", timeout.as_millis())
            } else {
                err.to_string()
            };
            return build_error_envelope(qctx, &msg);
        }
    };

    extract_json_string(qctx, call_result)
}

/// Builds an error envelope via native rquickjs objects.
fn build_error_envelope(
    qctx: &rquickjs::Ctx<'_>,
    message: &str,
) -> Result<String, Box<dyn Error + Send + Sync>> {
    let json_fn: Function<'_> = qctx.globals().get("json")?;
    let err_obj = Object::new(qctx.clone())?;
    let js_msg = rquickjs::String::from_str(qctx.clone(), message)?;
    err_obj.set("message", js_msg)?;
    let null = JsValue::new_null(qctx.clone());
    let result: JsValue<'_> = json_fn.call::<_, JsValue<'_>>((null, err_obj))?;
    extract_json_string(qctx, result)
}

/// Extracts a JSON string from a JS value — single copy across FFI.
fn extract_json_string<'js>(
    qctx: &rquickjs::Ctx<'js>,
    result: JsValue<'js>,
) -> Result<String, Box<dyn Error + Send + Sync>> {
    if let Some(js_str) = result.as_string() {
        return Ok(js_str.to_string()?);
    }
    let stringified = qctx.json_stringify(result)?;
    match stringified {
        Some(js_str) => Ok(js_str.to_string()?),
        None => Ok("{\"data\":null,\"errors\":null}".into()),
    }
}
