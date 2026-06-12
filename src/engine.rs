//! `QuickJS` execution engine — hardened sandbox for `handler(context)`.
//!
//! Uses `ctx.json_parse()` / `Function::call()` for direct C FFI data exchange.
//!
//! Sandbox: memory + stack limits, execution timeout, `eval()`/`Proxy` removed,
//! fresh context per request.
//!
//! On failure the engine **classifies** the outcome into a typed [`EngineError`]
//! (see `docs/99-errors.md`): a handler throw is inspected *structurally* via
//! `ctx.catch()` — a `__jsbox` tag ⇒ a capability error, otherwise a script error —
//! and the timeout signal (which JS cannot see) is folded in here. Out-of-memory is
//! caught earlier, when an oversized context fails to parse.

use std::error::Error;
use std::fmt::Display;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rquickjs::{Context, Ctx, Function, Object, Runtime, Value as JsValue};
use serde::Deserialize;
use serde_json::Value;
use tokio::runtime::Handle;

use crate::amq;
use crate::amq::{AmqConfig, AmqMetric};
use crate::auth;
use crate::auth::{AuthConfig, AuthMetric};
use crate::db;
use crate::db::{DbConfig, DbMetric};
use crate::decimal;
use crate::errors::{ErrorCategory, ErrorDebug, ErrorEnvelope, ErrorOwner, ErrorSource, Fault};
use crate::http;
use crate::http::HttpMetric;
use crate::kv;
use crate::kv::{RedisConfig, RedisMetric};
use crate::mail;
use crate::mail::{MailConfig, MailMetric};
use crate::s3;
use crate::s3::{S3Config, S3Metric};
use crate::sandbox::{self, Collector};
use crate::sys::{self, SysConfig};

/// The `json()` bridge — loaded from `src/js/bridge.js` at compile time.
const JSON_BRIDGE: &str = include_str!("js/bridge.js");

/// Human-safe message for a missing `handler`.
const HANDLER_MISSING_MSG: &str = "script must define a `handler(context)` function";
/// Human-safe message for an out-of-memory abort.
const MEMORY_MSG: &str = "memory limit exceeded";

/// Parameters for a single script execution.
pub(crate) struct ExecParams<'a> {
    /// The pooled runtime.
    pub(crate) runtime: &'a Runtime,
    /// Tokio runtime handle — drives async capability I/O (e.g. `db`) from this blocking
    /// thread via `block_on` (Tier 2, see `docs/design/resilience.md`).
    pub(crate) tokio_handle: &'a Handle,
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
    /// Mail config (None = disabled).
    pub(crate) mail_config: Option<&'a MailConfig>,
    /// S3 config (None = disabled).
    pub(crate) s3_config: Option<&'a S3Config>,
    /// Redis config (None = disabled).
    pub(crate) redis_config: Option<&'a RedisConfig>,
    /// `RabbitMQ` config (None = disabled).
    pub(crate) amq_config: Option<&'a AmqConfig>,
    /// Auth (OIDC/IAM) config (None = disabled).
    pub(crate) auth_config: Option<&'a AuthConfig>,
    /// `$sys` env/secrets context (None = no env/secrets injected).
    pub(crate) sys_config: Option<&'a SysConfig>,
    /// Max operations per execution.
    pub(crate) max_ops: usize,
    /// Debug mode: relax the SSRF private-IP block (`api`/`s3`) for local testing.
    pub(crate) allow_private_targets: bool,
}

/// Result of a script execution: the outcome plus the drained per-capability metrics.
pub(crate) struct ExecResult {
    /// Success envelope or a classified error.
    pub(crate) outcome: ExecOutcome,
    /// HTTP requests made during execution.
    pub(crate) http_metrics: Vec<HttpMetric>,
    /// DB operations made during execution.
    pub(crate) db_metrics: Vec<DbMetric>,
    /// Mail operations made during execution.
    pub(crate) mail_metrics: Vec<MailMetric>,
    /// S3 operations made during execution.
    pub(crate) s3_metrics: Vec<S3Metric>,
    /// Redis operations made during execution.
    pub(crate) redis_metrics: Vec<RedisMetric>,
    /// `RabbitMQ` operations made during execution.
    pub(crate) amq_metrics: Vec<AmqMetric>,
    /// Auth operations made during execution.
    pub(crate) auth_metrics: Vec<AuthMetric>,
}

/// What the handler produced: a success envelope or a system error.
#[derive(Debug)]
pub(crate) enum ExecOutcome {
    /// Handler returned — the JS-produced `{"data": ..., "error": ...}` string.
    Success(String),
    /// A classified system error (runtime / script / capability).
    Error(EngineError),
}

/// A classified engine-level error, ready for the handler to assemble into a response.
#[derive(Debug)]
pub(crate) enum EngineError {
    /// `eval` of the script failed to parse.
    Syntax(String),
    /// Script defines no `handler(context)`.
    HandlerNotDefined,
    /// Wall-clock limit hit (detected via the interrupt flag).
    Timeout {
        /// Configured limit, for the message.
        limit_ms: u128,
    },
    /// Memory cap exceeded (best-effort: thrown error named `InternalError`).
    MemoryLimit,
    /// `handler` returned something that isn't a `{data,error}` envelope.
    Malformed(String),
    /// Our fault: context creation, capability injection, or a task panic.
    Internal(String),
    /// Uncaught `throw` from the handler (an explicit `throw` or a script bug).
    Script {
        /// JS error message.
        message: String,
        /// JS stack trace, when available.
        stack: Option<String>,
    },
    /// A capability's native call failed and its wrapper threw a tagged error.
    Capability(CapabilityErr),
}

/// A capability error read off a thrown JS error's `__jsbox` tag (or built for an
/// inject-time connection failure). Fields are private — only `into_envelope` reads them.
#[derive(Debug)]
pub(crate) struct CapabilityErr {
    /// Originating capability.
    source: ErrorSource,
    /// Stable machine code (set in Rust, round-tripped through the tag).
    code: String,
    /// Retry hint.
    retryable: bool,
    /// Who should act on the error.
    owner: ErrorOwner,
    /// Raw driver cause — surfaced gated, in `debug.raw`.
    raw: Option<String>,
    /// JS stack trace, when available.
    stack: Option<String>,
    /// Structured, ungated machine context (e.g. `{sqlstate}` / `{http_status}`).
    details: Option<Value>,
}

/// The `__jsbox` tag deserialized from a thrown capability error (read in one
/// `json_stringify` + parse rather than field-by-field).
#[derive(Debug, Deserialize)]
struct CapabilityTag {
    /// Raw driver cause.
    #[serde(default)]
    error: Option<String>,
    /// Stable machine code.
    code: String,
    /// Retry hint.
    #[serde(default)]
    retryable: bool,
    /// Originating capability (lowercase, parsed via [`ErrorSource::parse`]).
    source: String,
    /// Responsible owner (lowercase, parsed via [`ErrorOwner::parse`]).
    #[serde(default)]
    owner: Option<String>,
    /// Structured machine context.
    #[serde(default)]
    details: Option<Value>,
}

/// Runs the script in a sandboxed context.
///
/// # Errors
///
/// Returns [`EngineError::Internal`] only for a failure so early there is no outcome to
/// carry (context creation). Every in-execution failure is an [`ExecOutcome::Error`].
pub(crate) fn run(params: &ExecParams<'_>) -> Result<ExecResult, EngineError> {
    let timed_out = setup_timeout(params.runtime, params.timeout);

    let ctx = Context::full(params.runtime).map_err(EngineError::internal)?;

    let mut http_collector: Option<Collector<HttpMetric>> = None;
    let mut db_collector: Option<Collector<DbMetric>> = None;
    let mut mail_collector: Option<Collector<MailMetric>> = None;
    let mut s3_collector: Option<Collector<S3Metric>> = None;
    let mut redis_collector: Option<Collector<RedisMetric>> = None;
    let mut amq_collector: Option<Collector<AmqMetric>> = None;
    let mut auth_collector: Option<Collector<AuthMetric>> = None;

    let mut collectors = Collectors {
        http: &mut http_collector,
        db: &mut db_collector,
        mail: &mut mail_collector,
        s3: &mut s3_collector,
        redis: &mut redis_collector,
        amq: &mut amq_collector,
        auth: &mut auth_collector,
    };

    let js_result = ctx.with(|qctx| -> Result<ExecOutcome, EngineError> {
        inject_bridge(&qctx).map_err(EngineError::internal)?;
        decimal::inject_decimal(&qctx).map_err(EngineError::internal)?;
        sys::inject_sys(&qctx, params.sys_config).map_err(EngineError::internal)?;
        inject_apis(&qctx, params, &mut collectors)?;
        if eval_script(&qctx, params.script).is_err() {
            return Ok(ExecOutcome::Error(classify_eval_error(&qctx)));
        }
        sanitize_globals(&qctx).map_err(EngineError::internal)?;
        call_handler(&qctx, params.context_json, &timed_out, params.timeout)
    });

    // Cleanup: clear interrupt handler so pooled runtime is clean.
    params.runtime.set_interrupt_handler(None);

    let outcome = js_result.unwrap_or_else(ExecOutcome::Error);

    Ok(ExecResult {
        outcome,
        http_metrics: sandbox::drain(http_collector.as_ref()),
        db_metrics: sandbox::drain(db_collector.as_ref()),
        mail_metrics: sandbox::drain(mail_collector.as_ref()),
        s3_metrics: sandbox::drain(s3_collector.as_ref()),
        redis_metrics: sandbox::drain(redis_collector.as_ref()),
        amq_metrics: sandbox::drain(amq_collector.as_ref()),
        auth_metrics: sandbox::drain(auth_collector.as_ref()),
    })
}

/// Mutable references to the per-capability metric collectors.
///
/// Grouped into one struct so [`inject_apis`] stays within the argument-count
/// limit as capabilities are added.
struct Collectors<'a> {
    /// HTTP metrics collector slot.
    http: &'a mut Option<Collector<HttpMetric>>,
    /// DB metrics collector slot.
    db: &'a mut Option<Collector<DbMetric>>,
    /// Mail metrics collector slot.
    mail: &'a mut Option<Collector<MailMetric>>,
    /// S3 metrics collector slot.
    s3: &'a mut Option<Collector<S3Metric>>,
    /// Redis metrics collector slot.
    redis: &'a mut Option<Collector<RedisMetric>>,
    /// `RabbitMQ` metrics collector slot.
    amq: &'a mut Option<Collector<AmqMetric>>,
    /// Auth metrics collector slot.
    auth: &'a mut Option<Collector<AuthMetric>>,
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

/// Injects the `json(data, error)` bridge function.
fn inject_bridge(qctx: &Ctx<'_>) -> Result<(), rquickjs::Error> {
    let bridge: JsValue<'_> = qctx.eval(JSON_BRIDGE)?;
    drop(bridge);
    Ok(())
}

/// Injects the HTTP, DB, mail, and S3 APIs if configured.
///
/// A `db` connection failure here (inject time) is mapped to a retryable
/// `capability/db/DB_CONNECTION` instead of `runtime/INTERNAL`, so a dead database
/// reads as a downstream-dependency outage (200) rather than a server fault (500).
fn inject_apis(
    qctx: &Ctx<'_>,
    params: &ExecParams<'_>,
    collectors: &mut Collectors<'_>,
) -> Result<(), EngineError> {
    if !params.allowed_hosts.is_empty() {
        *collectors.http = Some(
            http::inject_api(
                qctx,
                params.allowed_hosts,
                params.max_ops,
                params.allow_private_targets,
            )
            .map_err(EngineError::internal)?,
        );
    }
    if let Some(db_cfg) = params.db_config {
        *collectors.db = Some(
            db::inject_db(
                qctx,
                db_cfg,
                params.tokio_handle,
                params.timeout,
                params.max_ops,
            )
            .map_err(map_db_inject_error)?,
        );
    }
    if let Some(mail_cfg) = params.mail_config {
        *collectors.mail =
            Some(mail::inject_mail(qctx, mail_cfg, params.max_ops).map_err(EngineError::internal)?);
    }
    if let Some(s3_cfg) = params.s3_config {
        *collectors.s3 = Some(
            s3::inject_s3(qctx, s3_cfg, params.max_ops, params.allow_private_targets)
                .map_err(EngineError::internal)?,
        );
    }
    if let Some(redis_cfg) = params.redis_config {
        *collectors.redis = Some(
            kv::inject_redis(qctx, redis_cfg, params.max_ops).map_err(map_redis_inject_error)?,
        );
    }
    if let Some(amq_cfg) = params.amq_config {
        *collectors.amq =
            Some(amq::inject_amq(qctx, amq_cfg, params.max_ops).map_err(EngineError::internal)?);
    }
    if let Some(auth_cfg) = params.auth_config {
        *collectors.auth =
            Some(auth::inject_auth(qctx, auth_cfg, params.max_ops).map_err(EngineError::internal)?);
    }
    Ok(())
}

/// Maps a `db` inject failure: a connection failure → retryable capability error;
/// an engine-setup failure → internal.
fn map_db_inject_error(err: Box<dyn Error + Send + Sync>) -> EngineError {
    if db::is_connect_error(err.as_ref()) {
        EngineError::capability_inject(ErrorSource::Db, db::DB_CONNECTION_FAULT, err.to_string())
    } else {
        EngineError::internal(err)
    }
}

/// Maps a `redis` inject failure: a connection failure → retryable capability error;
/// an engine-setup failure → internal.
fn map_redis_inject_error(err: Box<dyn Error + Send + Sync>) -> EngineError {
    if kv::is_connect_error(err.as_ref()) {
        EngineError::capability_inject(
            ErrorSource::Redis,
            kv::REDIS_CONNECTION_FAULT,
            err.to_string(),
        )
    } else {
        EngineError::internal(err)
    }
}

/// Evaluates the user script.
fn eval_script(qctx: &Ctx<'_>, script: &str) -> Result<(), rquickjs::Error> {
    let result: JsValue<'_> = qctx.eval(script)?;
    drop(result);
    Ok(())
}

/// Removes dangerous globals before handler runs.
fn sanitize_globals(qctx: &Ctx<'_>) -> Result<(), rquickjs::Error> {
    let globals = qctx.globals();
    globals.remove("eval")?;
    globals.remove("Proxy")?;
    Ok(())
}

// -- Handler invocation + classification ------------------------------------

/// Calls the user's `handler(context)` and classifies the outcome.
fn call_handler(
    qctx: &Ctx<'_>,
    context_json: &str,
    timed_out: &AtomicBool,
    timeout: Duration,
) -> Result<ExecOutcome, EngineError> {
    // The context is already syntactically valid JSON (validated as `RawValue` at the
    // HTTP layer), so the only realistic `json_parse` failure is the object graph
    // exceeding the sandbox memory limit. Surface it as a clean `MemoryLimit` (422)
    // rather than an `Internal` (500) server fault. The config invariant
    // (`max_context_size <= memory_limit / 4`) keeps this path unreachable in practice.
    let parsed_ctx: JsValue<'_> = match qctx.json_parse(context_json) {
        Ok(value) => value,
        Err(_parse_err) => {
            drop(qctx.catch()); // consume the pending exception before returning
            return Ok(ExecOutcome::Error(EngineError::MemoryLimit));
        }
    };

    let handler = match qctx.globals().get::<_, Function<'_>>("handler") {
        Ok(func) => func,
        Err(_err) => return Ok(ExecOutcome::Error(EngineError::HandlerNotDefined)),
    };

    match handler.call::<_, JsValue<'_>>((parsed_ctx,)) {
        Ok(value) => {
            let json = extract_json_string(qctx, value).map_err(EngineError::internal)?;
            Ok(ExecOutcome::Success(json))
        }
        Err(_call_err) => Ok(ExecOutcome::Error(classify_throw(qctx, timed_out, timeout))),
    }
}

/// Classifies a failed `eval` (syntax / top-level error) using the pending exception.
fn classify_eval_error(qctx: &Ctx<'_>) -> EngineError {
    let caught = qctx.catch();
    let message = caught
        .as_object()
        .and_then(|obj| read_str_prop(obj, "message"))
        .unwrap_or_else(|| "syntax error".to_owned());
    EngineError::Syntax(message)
}

/// Classifies a handler throw structurally (timeout flag → `__jsbox` tag → script),
/// without parsing message text. Out-of-memory is handled earlier, at context parse
/// (see [`call_handler`]) — a handler that over-allocates instead surfaces as a script
/// error, which correctly attributes it to the developer's code.
fn classify_throw(qctx: &Ctx<'_>, timed_out: &AtomicBool, timeout: Duration) -> EngineError {
    if timed_out.load(Ordering::Relaxed) {
        return EngineError::Timeout {
            limit_ms: timeout.as_millis(),
        };
    }

    let caught = qctx.catch();
    let Some(obj) = caught.as_object() else {
        return EngineError::Script {
            message: stringify_value(&caught),
            stack: None,
        };
    };

    let stack = read_str_prop(obj, "stack");

    if let Some(cap) = read_capability_tag(qctx, obj, stack.clone()) {
        return EngineError::Capability(cap);
    }
    let message = read_str_prop(obj, "message").unwrap_or_default();
    EngineError::Script { message, stack }
}

/// Reads a capability's `__jsbox` tag, if present and well-formed.
///
/// Stringifies the tag object once and deserializes it (cleaner than field-by-field,
/// and `details` comes back as a `serde_json::Value` for free). Returns `None` if the
/// tag is absent or names no known source → the throw is treated as a script error.
fn read_capability_tag<'js>(
    qctx: &Ctx<'js>,
    obj: &Object<'js>,
    stack: Option<String>,
) -> Option<CapabilityErr> {
    let tag_val = obj.get::<_, JsValue<'js>>("__jsbox").ok()?;
    if tag_val.is_undefined() || tag_val.is_null() {
        return None;
    }
    let stringified = qctx.json_stringify(tag_val).ok().flatten()?;
    let json = stringified.to_string().ok()?;
    let tag: CapabilityTag = serde_json::from_str(&json).ok()?;

    let source = ErrorSource::parse(&tag.source)?;
    let owner = tag
        .owner
        .as_deref()
        .and_then(ErrorOwner::parse)
        .unwrap_or(ErrorOwner::Operator);
    Some(CapabilityErr {
        source,
        code: tag.code,
        retryable: tag.retryable,
        owner,
        raw: tag.error,
        stack,
        details: tag.details,
    })
}

/// Reads a non-empty string property off a JS object.
fn read_str_prop(obj: &Object<'_>, key: &str) -> Option<String> {
    obj.get::<_, String>(key)
        .ok()
        .filter(|text| !text.is_empty())
}

/// Best-effort string for a thrown non-object value (`throw "x"` / `throw 42`).
fn stringify_value(value: &JsValue<'_>) -> String {
    value
        .as_string()
        .and_then(|js_str| js_str.to_string().ok())
        .unwrap_or_else(|| "script error".to_owned())
}

/// Extracts a JSON string from a JS value — single copy across FFI.
fn extract_json_string<'js>(
    qctx: &Ctx<'js>,
    result: JsValue<'js>,
) -> Result<String, rquickjs::Error> {
    if let Some(js_str) = result.as_string() {
        return js_str.to_string();
    }
    let stringified = qctx.json_stringify(result)?;
    stringified.map_or_else(
        || Ok("{\"data\":null,\"error\":null}".into()),
        |js_str| js_str.to_string(),
    )
}

// -- Error → envelope assembly ----------------------------------------------

/// Builds a `runtime`-category envelope (source = engine) with a safe message.
fn runtime_envelope(
    code: &str,
    retryable: bool,
    owner: ErrorOwner,
    message: String,
) -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCategory::Runtime,
        ErrorSource::Engine,
        code.to_owned(),
        retryable,
        owner,
    )
    .with_message(message)
}

/// Builds a `script`-category envelope, attaching the stack when debug is on. The
/// message is the developer's own JS error message (their code, not secret).
fn script_envelope(message: String, stack: Option<String>, error_debug: bool) -> ErrorEnvelope {
    let envelope = ErrorEnvelope::new(
        ErrorCategory::Script,
        ErrorSource::Handler,
        "SCRIPT_ERROR".to_owned(),
        false,
        ErrorOwner::Developer,
    )
    .with_message(message);
    attach_debug(envelope, stack, None, error_debug)
}

/// Attaches gated debug context (`stack` + raw cause). Omitted entirely when
/// `error_debug` is off or there is nothing to carry.
fn attach_debug(
    envelope: ErrorEnvelope,
    stack: Option<String>,
    raw: Option<String>,
    error_debug: bool,
) -> ErrorEnvelope {
    if error_debug {
        envelope.with_debug(ErrorDebug { stack, raw })
    } else {
        envelope
    }
}

/// A generic, secret-free message per capability — keeps raw driver text (which can
/// contain credentials / PII) out of the always-present `message`.
const fn capability_message(source: ErrorSource) -> &'static str {
    match source {
        ErrorSource::Db => "database request failed",
        ErrorSource::Mail => "mail delivery failed",
        ErrorSource::S3 => "object storage request failed",
        ErrorSource::Api => "upstream request failed",
        ErrorSource::Redis => "redis request failed",
        ErrorSource::Amq => "message broker request failed",
        ErrorSource::Auth => "identity request failed",
        ErrorSource::Request | ErrorSource::Engine | ErrorSource::Handler => {
            "capability request failed"
        }
    }
}

impl EngineError {
    /// Wraps any Rust-side failure as an internal error.
    fn internal<E: Display>(err: E) -> Self {
        Self::Internal(err.to_string())
    }

    /// Builds a capability error for an inject-time failure (no JS throw involved).
    fn capability_inject(source: ErrorSource, fault: Fault, raw: String) -> Self {
        Self::Capability(CapabilityErr {
            source,
            code: fault.code.to_owned(),
            retryable: fault.retryable,
            owner: fault.owner,
            raw: Some(raw),
            stack: None,
            details: None,
        })
    }

    /// HTTP status for this error per `docs/99-errors.md`.
    pub(crate) const fn http_status(&self) -> u16 {
        match self {
            Self::Internal(_) => 500,
            Self::Script { .. } | Self::Capability(_) => 200,
            Self::Syntax(_)
            | Self::HandlerNotDefined
            | Self::Timeout { .. }
            | Self::MemoryLimit
            | Self::Malformed(_) => 422,
        }
    }

    /// Assembles the structured [`ErrorEnvelope`], gating debug on `error_debug`.
    pub(crate) fn into_envelope(self, error_debug: bool) -> ErrorEnvelope {
        let dev = ErrorOwner::Developer;
        match self {
            Self::Syntax(message) => runtime_envelope("SYNTAX_ERROR", false, dev, message),
            Self::HandlerNotDefined => runtime_envelope(
                "HANDLER_NOT_DEFINED",
                false,
                dev,
                HANDLER_MISSING_MSG.to_owned(),
            ),
            Self::Timeout { limit_ms } => runtime_envelope(
                "TIMEOUT",
                false,
                dev,
                format!("execution timed out ({limit_ms}ms limit)"),
            ),
            Self::MemoryLimit => {
                runtime_envelope("MEMORY_LIMIT", false, dev, MEMORY_MSG.to_owned())
            }
            Self::Malformed(message) => runtime_envelope("MALFORMED_RESPONSE", false, dev, message),
            // Generic message + raw cause in gated debug — never leak internal infra
            // detail (hostnames, etc.) in the always-present `message`.
            Self::Internal(raw) => attach_debug(
                runtime_envelope(
                    "INTERNAL",
                    true,
                    ErrorOwner::Operator,
                    "internal error".to_owned(),
                ),
                None,
                Some(raw),
                error_debug,
            ),
            Self::Script { message, stack } => script_envelope(message, stack, error_debug),
            Self::Capability(cap) => cap.into_envelope(error_debug),
        }
    }
}

impl CapabilityErr {
    /// Assembles a `capability`-category envelope: generic message, structured details,
    /// raw cause + stack in gated debug.
    fn into_envelope(self, error_debug: bool) -> ErrorEnvelope {
        let envelope = ErrorEnvelope::new(
            ErrorCategory::Capability,
            self.source,
            self.code,
            self.retryable,
            self.owner,
        )
        .with_message(capability_message(self.source).to_owned())
        .with_details(self.details);
        attach_debug(envelope, self.stack, self.raw, error_debug)
    }
}
