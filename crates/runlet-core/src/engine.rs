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

use std::fmt::Display;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rquickjs::module::{Declared, Evaluated};
use rquickjs::{Context, Ctx, Function, Module, Object, Runtime, Value as JsValue, WriteOptions};
use serde::Deserialize;
use serde_json::Value;
use serde_json::value::RawValue;

#[cfg(feature = "amq")]
use crate::amq;
#[cfg(feature = "auth")]
use crate::auth;
use crate::bytecode::{self, BytecodeCache};
#[cfg(feature = "db")]
use crate::db;
use crate::decimal;
use crate::egress::Egress;
use crate::errors::{self, ErrorCategory, ErrorDebug, ErrorEnvelope, ErrorOwner, ErrorSource};
#[cfg(feature = "http")]
use crate::http::{self, HttpMetric};
#[cfg(feature = "redis")]
use crate::kv;
#[cfg(feature = "mail")]
use crate::mail;
use crate::modules;
#[cfg(feature = "mongo")]
use crate::mongo;
#[cfg(feature = "s3")]
use crate::s3::{self, S3Config, S3Metric};
// The metric collector apparatus is needed only by the in-engine capabilities (`http`/`s3`); the
// driver-backed capabilities surface their metrics from the egress adapter, not the engine.
#[cfg(any(feature = "http", feature = "s3"))]
use crate::sandbox::{self, Collector};
use crate::sys::{self, SysConfig};

/// The `json()` bridge — loaded from `src/js/bridge.js` at compile time.
const JSON_BRIDGE: &str = include_str!("js/bridge.js");

/// Human-safe message for a missing `handler`.
const HANDLER_MISSING_MSG: &str = "script must define a `handler(context)` function";
/// Human-safe message for an out-of-memory abort.
const MEMORY_MSG: &str = "memory limit exceeded";

/// Determinism sanitizer — loaded from `src/js/determinism.js` at compile time. Run after
/// `sanitize_globals` under [`Profile::Deterministic`] to neutralize nondeterministic
/// surfaces (`Math.random`, `Date.now`, zero-arg `new Date()`, `$sys.date.now`,
/// `$sys.crypto.uuid`).
const DETERMINISM_SANITIZER: &str = include_str!("js/determinism.js");

/// The generic `io.call` egress wrapper — loaded from `src/js/io.js` at compile
/// time. `eval`'d after `__io` is registered, only when a [`Egress`] is wired and the
/// profile is `Full` (the seam is I/O).
const IO_WRAPPER: &str = include_str!("js/io.js");

/// Capability-injection + determinism profile for an execution.
///
/// A **runtime** injection decision (not a compile-time feature) so a single process can
/// run both tiers — see the consuming spec's "logic plane".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// The full jsbox capability set (per-request, opt-in) plus `emit`. The post-commit /
    /// action tier — essentially jsbox's existing behavior.
    Full,
    /// No I/O capabilities are injected (`db`/`api`/`mongo`/`mail`/`s3`/`redis`/`amq`/
    /// `auth` are all withheld) and nondeterminism is neutralized on top of the existing
    /// `eval`/`Proxy` removal. Only the pure `$`/`$sys` helpers, `emit`, and a
    /// consumer-supplied read-of-declared-dependencies hook are available. The
    /// in-transaction logic tier.
    Deterministic,
}

/// Whether a driver-backed capability's wrapper is injected for an invocation.
///
/// A two-variant enum rather than a `bool` so the capability structs don't accumulate a wall of
/// bools (`clippy::struct_excessive_bools`). Encodes presence only — the connection and
/// credentials live in the wired [`Egress`] port, resolved operator-side from a logical resource
/// name, never crossing the engine boundary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Gate {
    /// Capability withheld (the default).
    #[default]
    Off,
    /// Capability exposed — its JS wrapper is injected.
    On,
}

impl Gate {
    /// `On` when `enabled`, else `Off`.
    #[must_use]
    pub const fn from_enabled(enabled: bool) -> Self {
        if enabled { Self::On } else { Self::Off }
    }

    /// Whether the capability is exposed.
    #[must_use]
    pub const fn is_on(self) -> bool {
        matches!(self, Self::On)
    }
}

/// Consumer-supplied "read a declared dependency" hook — the deterministic-profile seam.
///
/// The core stays domain-agnostic: it knows nothing about what is being read. Receives the
/// JSON-encoded argument the script passed to `read(arg)` and returns the JSON-encoded
/// value, or an `Err(message)` the JS wrapper re-throws as a script error. `Send + Sync`
/// because the pooled runtime is shared across threads (the `parallel` feature).
pub type ReadHook = dyn Fn(&str) -> Result<String, String> + Send + Sync;

/// Parameters for a single script execution. Built by the [`crate::host::LogicHost`] from
/// a public `Invocation`; internal to the core.
pub(crate) struct ExecParams<'a> {
    /// The pooled runtime.
    pub(crate) runtime: &'a Runtime,
    /// Shared compiled-bytecode cache (parse/compile reuse for the ES-module path).
    /// `None` = always recompile (e.g. a consumer that opts out).
    pub(crate) bytecode_cache: Option<&'a BytecodeCache>,
    /// Partition/tenant namespace mixed into the bytecode cache key, so identical source from
    /// different tenants does not share an entry. `None` = global (no namespace).
    pub(crate) cache_namespace: Option<&'a str>,
    /// JS script source.
    pub(crate) script: &'a str,
    /// Context JSON string.
    pub(crate) context_json: &'a str,
    /// Execution timeout.
    pub(crate) timeout: Duration,
    /// Capability + determinism profile (gates I/O injection and the determinism sanitizer).
    pub(crate) profile: Profile,
    /// Allowed HTTP hosts (empty = disabled).
    #[cfg(feature = "http")]
    pub(crate) allowed_hosts: &'a [String],
    /// Whether the `db` capability wrapper is injected (the logical-resource gate). The
    /// connection + credentials live in the wired [`Egress`] port, not here — so this is
    /// just an on/off flag, no config crosses the engine boundary.
    #[cfg(feature = "db")]
    pub(crate) db_enabled: Gate,
    /// Whether the `mongo` capability wrapper is injected (see `db_enabled`).
    #[cfg(feature = "mongo")]
    pub(crate) mongo_enabled: Gate,
    /// Whether the `mail` capability wrapper is injected (see `db_enabled`).
    #[cfg(feature = "mail")]
    pub(crate) mail_enabled: Gate,
    /// S3 config (None = disabled). Stays in-engine (pure `SigV4` presign, no driver), so unlike
    /// the driver-backed capabilities it still carries its config across the boundary.
    #[cfg(feature = "s3")]
    pub(crate) s3_config: Option<&'a S3Config>,
    /// Whether the `redis` capability wrapper is injected (see `db_enabled`).
    #[cfg(feature = "redis")]
    pub(crate) redis_enabled: Gate,
    /// Whether the `amq` capability wrapper is injected (see `db_enabled`).
    #[cfg(feature = "amq")]
    pub(crate) amq_enabled: Gate,
    /// Whether the `auth` capability wrapper is injected (see `db_enabled`).
    #[cfg(feature = "auth")]
    pub(crate) auth_enabled: Gate,
    /// `$sys` env/secrets context (None = no env/secrets injected).
    pub(crate) sys_config: Option<&'a SysConfig>,
    /// Read-of-declared-dependencies hook (the deterministic-profile seam). `None` = no
    /// `read` global is injected.
    pub(crate) read_hook: Option<Arc<ReadHook>>,
    /// I/O egress seam (the `io.call` global). `None` = no `io` global is injected.
    /// Withheld under [`Profile::Deterministic`] (it performs I/O). Not feature-gated.
    pub(crate) egress: Option<Arc<dyn Egress>>,
    /// Max operations per execution (also caps the number of `emit` effects).
    pub(crate) max_ops: usize,
    /// Max bytes the handler may return (`0` = off, bounded only by `memory_limit`).
    pub(crate) max_output_size: usize,
    /// Debug mode: relax the SSRF private-IP block (`api`/`s3`) for local testing.
    #[cfg(any(feature = "http", feature = "s3"))]
    pub(crate) allow_private_targets: bool,
    /// Whether the `api` client honors an `allowed_hosts: ["*"]` wildcard. Resolved in the
    /// handler as `allow_wildcard_hosts && !debug` — a wildcard is never honored in the
    /// SSRF-relaxed debug mode.
    #[cfg(feature = "http")]
    pub(crate) wildcard_hosts_allowed: bool,
}

/// Result of a script execution: the outcome, the declarative `emit` effects, and the
/// drained per-capability metrics. Internal to the core; the host maps it to a public
/// `Outcome`.
pub(crate) struct ExecResult {
    /// Success envelope or a classified error.
    pub(crate) outcome: ExecOutcome,
    /// Declarative effects appended via `emit(value)`, in call order (opaque JSON to the
    /// core — the consumer interprets them).
    pub(crate) effects: Vec<Box<RawValue>>,
    /// HTTP requests made during execution (in-engine capability).
    #[cfg(feature = "http")]
    pub(crate) http_metrics: Vec<HttpMetric>,
    /// S3 presign operations made during execution (in-engine capability).
    #[cfg(feature = "s3")]
    pub(crate) s3_metrics: Vec<S3Metric>,
    // The driver-backed capabilities (`db`/`mongo`/`mail`/`redis`/`amq`/`auth`) no longer report
    // metrics through the engine: they run in the wired egress adapter, which the consumer drains
    // directly (see `fabric_backends::BackendSet`). So the engine carries only the in-engine
    // capabilities' metrics here.
}

/// What the handler produced: a success envelope or a system error.
#[derive(Debug)]
pub enum ExecOutcome {
    /// Handler returned — the JS-produced `{"data": ..., "error": ...}` string.
    Success(String),
    /// A classified system error (runtime / script / capability).
    Error(EngineError),
}

/// A classified engine-level error, ready for the handler to assemble into a response.
#[derive(Debug)]
pub enum EngineError {
    /// `eval` of the script failed to parse.
    Syntax(String),
    /// A `CodeRef::Registered` key resolved to no script in the registry. (The HTTP front
    /// resolves keys itself and never produces this; it is for non-HTTP consumers that pass
    /// a key straight to the host.)
    ScriptNotFound(String),
    /// An ES-module handler `import`ed a specifier that isn't a registered module.
    ModuleNotFound(String),
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
    /// The handler's returned JSON exceeded `max_output_size`.
    OutputTooLarge {
        /// Actual size produced.
        size: usize,
        /// Configured ceiling.
        limit: usize,
    },
    /// Our fault: context creation, capability injection, or a task panic.
    Internal(String),
    /// The host is shutting down and no longer accepts new executions (see
    /// [`crate::host::LogicHost::shutdown`]). Retryable — typically against another replica.
    ShuttingDown,
    /// Uncaught `throw` from the handler (an explicit `throw` or a script bug).
    Script {
        /// JS error message.
        message: String,
        /// JS stack trace, when available.
        stack: Option<String>,
    },
    /// A capability's native call failed and its wrapper threw a tagged error. Boxed: it
    /// is by far the largest variant (it carries `details`/`raw`/`stack`), so boxing keeps
    /// `EngineError` small in the common (non-capability) paths.
    Capability(Box<CapabilityErr>),
}

/// A capability error read off a thrown JS error's `__jsbox` tag (or built for an
/// inject-time connection failure). Fields are private — only `into_envelope` reads them.
#[derive(Debug)]
pub struct CapabilityErr {
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

    // Per-invocation `emit` buffer: native `__emit` appends JSON strings here; drained into
    // `ExecResult.effects` after execution. Opaque to the core (the consumer interprets it).
    let effects: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    // In-engine capability metric collectors (`http`/`s3` only); the driver-backed capabilities
    // record into the egress adapter, drained by the consumer.
    #[cfg(feature = "http")]
    let mut http_collector: Option<Collector<HttpMetric>> = None;
    #[cfg(feature = "s3")]
    let mut s3_collector: Option<Collector<S3Metric>> = None;

    let js_result = ctx.with(|qctx| -> Result<ExecOutcome, EngineError> {
        inject_bridge(&qctx).map_err(EngineError::internal)?;
        decimal::inject_decimal(&qctx).map_err(EngineError::internal)?;
        sys::inject_sys(&qctx, params.sys_config).map_err(EngineError::internal)?;
        inject_emit(&qctx, &effects, params.max_ops).map_err(EngineError::internal)?;
        if let Some(hook) = &params.read_hook {
            inject_read(&qctx, Arc::clone(hook)).map_err(EngineError::internal)?;
        }
        // The egress is I/O, so it is gated to `Profile::Full` exactly like the capabilities —
        // the boundary is enforced here, never trusted to the caller's `Invocation`.
        if params.profile == Profile::Full
            && let Some(egress) = &params.egress
        {
            inject_egress(&qctx, Arc::clone(egress), params.max_ops)
                .map_err(EngineError::internal)?;
        }
        #[cfg(feature = "_io")]
        inject_apis(
            &qctx,
            params,
            #[cfg(feature = "http")]
            &mut http_collector,
            #[cfg(feature = "s3")]
            &mut s3_collector,
        )?;
        let handler = match resolve_handler(&qctx, params) {
            Ok(func) => func,
            Err(outcome) => return Ok(outcome),
        };
        invoke_handler(
            &qctx,
            &handler,
            params.context_json,
            &timed_out,
            params.timeout,
        )
    });

    // Cleanup: clear interrupt handler so pooled runtime is clean.
    params.runtime.set_interrupt_handler(None);

    let outcome = enforce_output_cap(
        js_result.unwrap_or_else(ExecOutcome::Error),
        params.max_output_size,
    );

    Ok(ExecResult {
        outcome,
        effects: drain_effects(&effects),
        #[cfg(feature = "http")]
        http_metrics: sandbox::drain(http_collector.as_ref()),
        #[cfg(feature = "s3")]
        s3_metrics: sandbox::drain(s3_collector.as_ref()),
    })
}

/// Enforces the output-size ceiling on a successful result: a handler JSON larger than
/// `max_output_size` (when non-zero) is turned into an [`EngineError::OutputTooLarge`] so a
/// script can't return a `memory_limit`-sized blob. Errors and the disabled case (`0`) pass
/// through untouched.
fn enforce_output_cap(outcome: ExecOutcome, max_output_size: usize) -> ExecOutcome {
    if max_output_size == 0 {
        return outcome;
    }
    if let ExecOutcome::Success(json) = &outcome {
        let size = json.len();
        if size > max_output_size {
            return ExecOutcome::Error(EngineError::OutputTooLarge {
                size,
                limit: max_output_size,
            });
        }
    }
    outcome
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

/// Injects the per-request capabilities (subject to the profile).
///
/// The in-engine capabilities (`http`/`s3`) build their client and return a metric collector
/// (captured into the `*_collector` slots). The driver-backed capabilities (`db`/`mongo`/`mail`/
/// `redis`/`amq`/`auth`) inject only their JS wrapper — every call routes through the wired
/// [`Egress`] port (injected before this), whose adapter owns the connection, resilience, and
/// metrics; the engine never sees their credentials or records their ops.
#[cfg(feature = "_io")]
fn inject_apis(
    qctx: &Ctx<'_>,
    params: &ExecParams<'_>,
    #[cfg(feature = "http")] http_collector: &mut Option<Collector<HttpMetric>>,
    #[cfg(feature = "s3")] s3_collector: &mut Option<Collector<S3Metric>>,
) -> Result<(), EngineError> {
    // Profile enforcement: the deterministic tier gets **no** I/O capability, regardless of
    // what configs an `Invocation` carries — the boundary is enforced here, not trusted to
    // the author (only `$`/`$sys`, `emit`, and the read-hook remain, injected elsewhere).
    if params.profile != Profile::Full {
        return Ok(());
    }
    #[cfg(feature = "http")]
    if !params.allowed_hosts.is_empty() {
        *http_collector = Some(
            http::inject_api(
                qctx,
                params.allowed_hosts,
                params.max_ops,
                params.allow_private_targets,
                params.wildcard_hosts_allowed,
            )
            .map_err(EngineError::internal)?,
        );
    }
    // Each driver-backed capability below routes through the wired [`Egress`] port: inject only
    // its JS wrapper (which calls `io.call("<cap>", …)`); the connection, deps, and metrics live
    // in the adapter, not the engine. The presence gate is the logical-resource flag — no config
    // crosses the engine boundary.
    #[cfg(feature = "db")]
    if params.db_enabled.is_on() {
        db::inject_wrapper(qctx).map_err(EngineError::internal)?;
    }
    #[cfg(feature = "mongo")]
    if params.mongo_enabled.is_on() {
        mongo::inject_wrapper(qctx).map_err(EngineError::internal)?;
    }
    #[cfg(feature = "mail")]
    if params.mail_enabled.is_on() {
        mail::inject_wrapper(qctx).map_err(EngineError::internal)?;
    }
    #[cfg(feature = "s3")]
    if let Some(s3_cfg) = params.s3_config {
        *s3_collector = Some(
            s3::inject_s3(qctx, s3_cfg, params.max_ops, params.allow_private_targets)
                .map_err(EngineError::internal)?,
        );
    }
    #[cfg(feature = "redis")]
    if params.redis_enabled.is_on() {
        kv::inject_wrapper(qctx).map_err(EngineError::internal)?;
    }
    #[cfg(feature = "amq")]
    if params.amq_enabled.is_on() {
        amq::inject_wrapper(qctx).map_err(EngineError::internal)?;
    }
    #[cfg(feature = "auth")]
    if params.auth_enabled.is_on() {
        auth::inject_wrapper(qctx).map_err(EngineError::internal)?;
    }
    Ok(())
}

/// Evaluates the user script.
fn eval_script(qctx: &Ctx<'_>, script: &str) -> Result<(), rquickjs::Error> {
    let result: JsValue<'_> = qctx.eval(script)?;
    drop(result);
    Ok(())
}

/// Removes `eval` and `Proxy` before the handler runs.
///
/// This is isolation hardening, **not** a dynamic-code block: `new Function("…")()` and the
/// `AsyncFunction`/`GeneratorFunction` constructors still compile strings, and that is fine —
/// the script is already arbitrary code, and the real boundary is `QuickJS` having no host
/// access (no fs/net/process). `eval` is removed to trim a historically bug-prone surface and
/// `Proxy` to deny exotic-object traps over the injected capability/`$sys` globals. Do not
/// rely on their absence for any policy that depends on the script *not* generating code.
fn sanitize_globals(qctx: &Ctx<'_>) -> Result<(), rquickjs::Error> {
    let globals = qctx.globals();
    globals.remove("eval")?;
    globals.remove("Proxy")?;
    Ok(())
}

/// Evaluates the user source (ES module or classic script), sanitizes globals, and returns
/// the handler function. On failure returns the classified error outcome to short-circuit:
/// a syntax/import error, or `HANDLER_NOT_DEFINED` when no handler is exported/defined.
///
/// Module vs script is detected by a top-level `export` ([`is_es_module`]); the handler
/// body runs *after* `sanitize_globals` either way, so the two modes share the same
/// `eval`/`Proxy`-removed execution environment.
fn resolve_handler<'js>(
    qctx: &Ctx<'js>,
    params: &ExecParams<'_>,
) -> Result<Function<'js>, ExecOutcome> {
    let (script, profile) = (params.script, params.profile);
    if is_es_module(script) {
        let module = eval_module(qctx, params).map_err(ExecOutcome::Error)?;
        harden(qctx, profile).map_err(|err| ExecOutcome::Error(EngineError::internal(err)))?;
        module_handler(&module).ok_or(ExecOutcome::Error(EngineError::HandlerNotDefined))
    } else {
        eval_script(qctx, script).map_err(|_err| ExecOutcome::Error(classify_eval_error(qctx)))?;
        harden(qctx, profile).map_err(|err| ExecOutcome::Error(EngineError::internal(err)))?;
        qctx.globals()
            .get::<_, Function<'js>>("handler")
            .map_err(|_err| ExecOutcome::Error(EngineError::HandlerNotDefined))
    }
}

/// Hardens the execution environment after the user source is evaluated, before the handler
/// runs: removes `eval`/`Proxy` (always) and, under [`Profile::Deterministic`], neutralizes
/// nondeterministic surfaces on top.
fn harden(qctx: &Ctx<'_>, profile: Profile) -> Result<(), rquickjs::Error> {
    sanitize_globals(qctx)?;
    if profile == Profile::Deterministic {
        sanitize_determinism(qctx)?;
    }
    Ok(())
}

/// Neutralizes nondeterminism for [`Profile::Deterministic`]: overrides `Math.random`,
/// `Date.now`, zero-arg `new Date()`, `$sys.date.now`, and `$sys.crypto.uuid` to throw
/// (see `js/determinism.js`). Runs after [`sanitize_globals`].
fn sanitize_determinism(qctx: &Ctx<'_>) -> Result<(), rquickjs::Error> {
    let sanitized: JsValue<'_> = qctx.eval(DETERMINISM_SANITIZER)?;
    drop(sanitized);
    Ok(())
}

/// Injects the `emit(value)` host function: it `JSON.stringify`s the value and appends it
/// to the per-invocation `effects` buffer (surfaced as `Outcome.effects`). The number of
/// effects is capped at `max_ops` so a handler can't grow the buffer without bound. The
/// value is opaque to the core — the consumer interprets it ("logic proposes, the engine
/// disposes").
fn inject_emit(
    qctx: &Ctx<'_>,
    effects: &Arc<Mutex<Vec<String>>>,
    max_ops: usize,
) -> Result<(), rquickjs::Error> {
    let buffer = Arc::clone(effects);
    let emit_fn = Function::new(qctx.clone(), move |value: String| -> String {
        match buffer.lock() {
            Ok(buf) if buf.len() >= max_ops => {
                format!("too many emit() calls: limit is {max_ops} per execution")
            }
            Ok(mut buf) => {
                buf.push(value);
                String::new()
            }
            Err(_poisoned) => "emit buffer unavailable".to_owned(),
        }
    })?
    .with_name("__emit")?;
    qctx.globals().set("__emit", emit_fn)?;
    // `emit(v)` stringifies and forwards; a non-empty return is an error the wrapper throws.
    let wrapper: JsValue<'_> = qctx.eval(
        "globalThis.emit = function (value) { \
           var err = __emit(JSON.stringify(value === undefined ? null : value)); \
           if (err) throw new Error(err); \
         };",
    )?;
    drop(wrapper);
    Ok(())
}

/// Injects the consumer-supplied `read(arg)` host function (the deterministic-profile seam).
/// `read` stringifies its argument, calls the hook, and `JSON.parse`s the returned value;
/// an `Err` from the hook is re-thrown as a script error. The core stays domain-agnostic —
/// it neither defines nor inspects what is read.
fn inject_read(qctx: &Ctx<'_>, hook: Arc<ReadHook>) -> Result<(), rquickjs::Error> {
    let read_fn = Function::new(qctx.clone(), move |arg: String| -> String {
        match hook(&arg) {
            // `value_json` is raw JSON from the consumer's hook, spliced verbatim.
            Ok(value_json) => format!("{{\"value\":{value_json}}}"),
            Err(message) => {
                let escaped = serde_json::to_string(&message)
                    .unwrap_or_else(|_err| "\"read hook error\"".to_owned());
                format!("{{\"__readError\":{escaped}}}")
            }
        }
    })?
    .with_name("__read")?;
    qctx.globals().set("__read", read_fn)?;
    let wrapper: JsValue<'_> = qctx.eval(
        "globalThis.read = function (arg) { \
           var res = __read(JSON.stringify(arg === undefined ? null : arg)); \
           var parsed = JSON.parse(res); \
           if (parsed && parsed.__readError) throw new Error(parsed.__readError); \
           return parsed.value; \
         };",
    )?;
    drop(wrapper);
    Ok(())
}

/// Injects the consumer-supplied `io.call(name, action, payload)` egress global.
///
/// The native `__io` forwards `(name, action, payload_json)` to the [`Egress`] hook and
/// returns either the JSON result verbatim or a `__jsbox` tagged error; the JS wrapper
/// (`js/io.js`) throws on the latter so the engine classifies it as a capability error
/// exactly like a built-in capability. Calls are capped at `max_ops` per execution (a shared
/// counter, mirroring `emit`), so the egress can't be used to bypass the op budget.
fn inject_egress(
    qctx: &Ctx<'_>,
    egress: Arc<dyn Egress>,
    max_ops: usize,
) -> Result<(), rquickjs::Error> {
    let used = Arc::new(AtomicUsize::new(0));
    let egress_fn = Function::new(
        qctx.clone(),
        move |name: String, action: String, payload: String| -> String {
            if used.load(Ordering::Relaxed) >= max_ops {
                let message = format!("too many operations: limit is {max_ops} per execution");
                return errors::dynamic_fault_json(&errors::DynamicFault {
                    error: &message,
                    code: "IO_OP_LIMIT",
                    retryable: false,
                    owner: ErrorOwner::Developer,
                    source: "engine",
                    details: None,
                });
            }
            let _prev = used.fetch_add(1, Ordering::Relaxed);
            match egress.call(&name, &action, &payload) {
                Ok(json) => json,
                Err(err) => err.to_tag_json(),
            }
        },
    )?
    .with_name("__io")?;
    qctx.globals().set("__io", egress_fn)?;
    let wrapper: JsValue<'_> = qctx.eval(IO_WRAPPER)?;
    drop(wrapper);
    Ok(())
}

/// Drains the per-invocation `emit` buffer into validated `RawValue` effects. Each entry was
/// produced by `JSON.stringify`, so it parses; a (theoretically impossible) parse failure is
/// dropped rather than aborting the whole outcome.
fn drain_effects(effects: &Arc<Mutex<Vec<String>>>) -> Vec<Box<RawValue>> {
    let Ok(buf) = effects.lock() else {
        return Vec::new();
    };
    buf.iter()
        .filter_map(|json| RawValue::from_string(json.clone()).ok())
        .collect()
}

/// Best-effort detection of ES-module source by a top-level `export` — the syntax a
/// handler-module must use to export its handler. A miss is self-correcting: script-mode
/// on a real module fails to parse (a syntax error), module-mode on a plain script finds
/// no exported handler (`HANDLER_NOT_DEFINED`) — never a silent wrong result.
fn is_es_module(script: &str) -> bool {
    script.lines().any(|line| {
        line.trim_start()
            .strip_prefix("export")
            .is_some_and(|rest| rest.starts_with([' ', '\t', '{', '*']))
    })
}

/// Evaluates the user source as an ES module, settling synchronously: `Promise::finish`
/// pumps the job queue to completion, and since every jsbox capability is sync FFI a module
/// never truly suspends. Imports resolve through the per-runtime registry loader. On failure
/// the pending exception is classified into a [`EngineError`] (`MODULE_NOT_FOUND` for an
/// unresolved `import`, else a syntax/top-level error).
fn eval_module<'js>(
    qctx: &Ctx<'js>,
    params: &ExecParams<'_>,
) -> Result<Module<'js, Evaluated>, EngineError> {
    try_eval_module(qctx, params).map_err(|()| classify_module_error(qctx))
}

/// The raw eval attempt; `Err(())` leaves the pending exception set for classification.
fn try_eval_module<'js>(
    qctx: &Ctx<'js>,
    params: &ExecParams<'_>,
) -> Result<Module<'js, Evaluated>, ()> {
    let declared = obtain_declared(qctx, params)?;
    let (module, promise) = declared.eval().map_err(drop)?;
    promise.finish::<()>().map_err(drop)?;
    Ok(module)
}

/// Obtains the declared (compiled-but-not-evaluated) handler module: a plain
/// `Module::declare` (parse + compile) when no bytecode cache is wired, or — with a cache
/// present — a `Module::load` of previously-compiled bytecode on a hit, and on a miss a compile
/// that is admitted to the cache only if the source clears the size floor. Either way the
/// returned module is then evaluated by the caller, so behavior (including thrown syntax/
/// top-level errors, which are classified from the pending exception) is identical to the
/// uncached path.
fn obtain_declared<'js>(
    qctx: &Ctx<'js>,
    params: &ExecParams<'_>,
) -> Result<Module<'js, Declared>, ()> {
    let Some(cache) = params.bytecode_cache else {
        return Module::declare(qctx.clone(), "handler", params.script).map_err(drop);
    };
    let namespace = params.cache_namespace.unwrap_or("");
    let key = bytecode::digest(namespace.as_bytes(), params.script.as_bytes());
    if let Some(bytecode) = cache.get(&key) {
        cache.note_hit();
        return load_bytecode(qctx, &bytecode);
    }
    cache.note_miss();
    let declared = Module::declare(qctx.clone(), "handler", params.script).map_err(drop)?;
    // Autonomous, size-based admission: cache only scripts large enough to be worth it (small
    // handlers recompile every call and never touch the `unsafe` load path). A failed
    // serialization just forgoes caching this script — it never fails the request.
    if cache.should_store(params.script.len())
        && let Ok(bytes) = declared.write(WriteOptions::default())
    {
        cache.insert(key, Arc::from(bytes.into_boxed_slice()));
    }
    Ok(declared)
}

/// Loads a handler module from previously-cached `QuickJS` bytecode.
///
/// The lone `unsafe` in the workspace: `Module::load` is `unsafe` because it trusts the bytes
/// are valid bytecode (malformed input is UB). Here the bytes are *self-produced* — they came
/// only from `Module::write` on a module this same process compiled from source, held in an
/// in-memory `BytecodeCache`, never crossing a trust boundary or a `QuickJS`-version boundary.
#[expect(
    unsafe_code,
    reason = "Module::load deserializes self-produced, in-process bytecode (see fn docs); the \
              bytes originate only from our own Module::write earlier this process"
)]
fn load_bytecode<'js>(qctx: &Ctx<'js>, bytecode: &[u8]) -> Result<Module<'js, Declared>, ()> {
    // SAFETY: `bytecode` was produced by `Module::write` on a module compiled in this process
    // and stored verbatim in the cache; it is therefore valid QuickJS bytecode for this build.
    unsafe { Module::load(qctx.clone(), bytecode) }.map_err(drop)
}

/// Classifies a module eval failure from the pending exception: the resolver's
/// [`modules::UNRESOLVED_MARKER`] in the message ⇒ `MODULE_NOT_FOUND` (a bad `import`),
/// otherwise a syntax / top-level error. Consumes the pending exception.
fn classify_module_error(qctx: &Ctx<'_>) -> EngineError {
    let caught = qctx.catch();
    let message = caught
        .as_object()
        .and_then(|obj| read_str_prop(obj, "message"));
    match message {
        Some(msg) if msg.contains(modules::UNRESOLVED_MARKER) => EngineError::ModuleNotFound(msg),
        Some(msg) => EngineError::Syntax(msg),
        None => EngineError::Syntax("syntax error".to_owned()),
    }
}

/// Reads the exported handler from an evaluated module: `export default function handler`
/// (namespace `default`) is preferred, then `export function handler` (namespace `handler`).
/// `None` if neither names an exported function.
fn module_handler<'js>(module: &Module<'js, Evaluated>) -> Option<Function<'js>> {
    let namespace = module.namespace().ok()?;
    namespace
        .get::<_, Function<'js>>("default")
        .ok()
        .or_else(|| namespace.get::<_, Function<'js>>("handler").ok())
}

// -- Handler invocation + classification ------------------------------------

/// Calls the resolved `handler(context)` and classifies the outcome.
fn invoke_handler<'js>(
    qctx: &Ctx<'js>,
    handler: &Function<'js>,
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
        return EngineError::Capability(Box::new(cap));
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
        ErrorSource::Db | ErrorSource::Mongo => "database request failed",
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

    /// HTTP status for this error per `docs/99-errors.md`.
    #[must_use]
    pub const fn http_status(&self) -> u16 {
        match self {
            Self::Internal(_) => 500,
            Self::ShuttingDown => 503,
            Self::Script { .. } | Self::Capability(_) => 200,
            Self::ScriptNotFound(_) => 404,
            Self::Syntax(_)
            | Self::ModuleNotFound(_)
            | Self::HandlerNotDefined
            | Self::Timeout { .. }
            | Self::MemoryLimit
            | Self::Malformed(_)
            | Self::OutputTooLarge { .. } => 422,
        }
    }

    /// Assembles the structured [`ErrorEnvelope`], gating debug on `error_debug`.
    #[must_use]
    pub fn into_envelope(self, error_debug: bool) -> ErrorEnvelope {
        let dev = ErrorOwner::Developer;
        match self {
            Self::Syntax(message) => runtime_envelope("SYNTAX_ERROR", false, dev, message),
            // Request-category (the caller named a key that doesn't exist) — mirrors the
            // HTTP front's own `SCRIPT_NOT_FOUND` envelope so the two paths are identical.
            Self::ScriptNotFound(message) => ErrorEnvelope::new(
                ErrorCategory::Request,
                ErrorSource::Request,
                "SCRIPT_NOT_FOUND".to_owned(),
                false,
                ErrorOwner::Caller,
            )
            .with_message(message),
            Self::ModuleNotFound(message) => {
                runtime_envelope("MODULE_NOT_FOUND", false, dev, message)
            }
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
            Self::OutputTooLarge { size, limit } => runtime_envelope(
                "OUTPUT_TOO_LARGE",
                false,
                dev,
                format!("handler output too large: {size} bytes (max {limit})"),
            ),
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
            Self::ShuttingDown => runtime_envelope(
                "SHUTTING_DOWN",
                true,
                ErrorOwner::Operator,
                "service is shutting down".to_owned(),
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

#[cfg(test)]
mod tests {
    //! Verifies the wall-clock interrupt preempts a catastrophic-backtracking regex.
    //!
    //! `QuickJS`'s libregexp does not yield to the interrupt handler on its own, so this proves
    //! that a `ReDoS` pattern is still bounded by the execution timeout rather than pinning a
    //! `spawn_blocking` thread until the match completes.

    use rquickjs::{Context, Runtime, Value as JsValue};
    use std::time::{Duration, Instant};

    /// A `(a+)+$` pattern over a non-matching tail backtracks exponentially; with 30 leading
    /// `a`s it would run for several seconds uninterrupted, so prompt completion proves the
    /// interrupt aborted the match rather than letting it run to the end.
    #[test]
    fn catastrophic_regex_is_interrupted() {
        let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
        let timeout = Duration::from_millis(250);
        let start = Instant::now();
        runtime.set_interrupt_handler(Some(Box::new(move || start.elapsed() > timeout)));
        let ctx = Context::full(&runtime).unwrap_or_else(|_err| unreachable!());
        let script = format!("/(a+)+$/.test(\"{}!\")", "a".repeat(30));
        ctx.with(|qctx| {
            let res: Result<JsValue<'_>, _> = qctx.eval(script.as_bytes());
            assert!(
                res.is_err(),
                "the wall-clock interrupt must abort a catastrophic regex"
            );
        });
        runtime.set_interrupt_handler(None);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "regex was not preempted promptly (interrupt did not fire during matching)"
        );
    }
}

/// The bytecode cache must populate on the first run and produce a byte-identical result on
/// the second (the `Module::load` path), and must autonomously skip caching sources below its
/// size floor. The extra `cfg` gates to the capability-free build so `ExecParams` has no I/O
/// fields to populate; the separate `#[cfg(test)]` keeps `tests_outside_test_module` satisfied.
#[cfg(test)]
#[cfg(not(feature = "_io"))]
mod bytecode_cache_tests {
    use super::{ExecOutcome, ExecParams, Profile, run};
    use crate::bytecode::BytecodeCache;
    use rquickjs::Runtime;
    use std::time::Duration;

    /// An ES-module handler — only the module path is cached. Adds 1 to `ctx.n`.
    const SCRIPT: &str = "export default function handler(ctx) { return json(ctx.n + 1); }";

    /// Builds `ExecParams` for the minimal (no-capability) build with the cache wired in.
    fn params<'a>(runtime: &'a Runtime, cache: &'a BytecodeCache) -> ExecParams<'a> {
        ExecParams {
            runtime,
            bytecode_cache: Some(cache),
            cache_namespace: None,
            script: SCRIPT,
            context_json: "{\"n\":41}",
            timeout: Duration::from_secs(5),
            profile: Profile::Full,
            sys_config: None,
            read_hook: None,
            egress: None,
            max_ops: 64,
            max_output_size: 0,
        }
    }

    /// Extracts the success envelope; a non-success outcome fails the test.
    fn success_json(outcome: ExecOutcome) -> String {
        let ExecOutcome::Success(json) = outcome else {
            unreachable!("expected a success outcome");
        };
        json
    }

    /// With a zero floor (cache everything): the cold run compiles + stores, the warm run loads
    /// bytecode and returns a byte-identical result.
    #[test]
    fn warm_run_loads_bytecode_with_identical_result() {
        let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
        let cache = BytecodeCache::new(8, 0);

        let cold = run(&params(&runtime, &cache)).unwrap_or_else(|_err| unreachable!());
        let cold_json = success_json(cold.outcome);
        let after_cold = cache.stats();
        assert_eq!(after_cold.misses, 1, "cold run compiles (miss)");
        assert_eq!(after_cold.stored, 1, "cold run caches the compiled module");

        let warm = run(&params(&runtime, &cache)).unwrap_or_else(|_err| unreachable!());
        let warm_json = success_json(warm.outcome);
        let after_warm = cache.stats();
        assert_eq!(
            cold_json, warm_json,
            "bytecode-load result matches the compiled result"
        );
        assert!(cold_json.contains("42"), "handler computed ctx.n + 1 = 42");
        assert_eq!(after_warm.hits, 1, "warm run is a cache hit");
        assert_eq!(after_warm.stored, 1, "warm run re-uses, doesn't re-store");
        assert_eq!(after_warm.entries, 1, "exactly one entry cached");
    }

    /// Autonomy: a sub-floor source is never cached — both runs miss and nothing is stored,
    /// so the `unsafe` load path is never exercised for a tiny handler.
    #[test]
    fn small_source_is_never_cached() {
        let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
        // Floor far above the ~60-byte SCRIPT, so admission is refused.
        let cache = BytecodeCache::new(8, 4096);

        for _ in 0..2 {
            let outcome = run(&params(&runtime, &cache)).unwrap_or_else(|_err| unreachable!());
            assert!(
                success_json(outcome.outcome).contains("42"),
                "still correct"
            );
        }
        let stats = cache.stats();
        assert_eq!(
            stats.misses, 2,
            "every run recompiles (sub-floor, never cached)"
        );
        assert_eq!(stats.stored, 0, "nothing admitted below the size floor");
        assert_eq!(stats.hits, 0, "no cache hits");
        assert_eq!(stats.entries, 0, "cache stays empty");
    }
}

/// The `Egress` egress seam: a wired egress exposes `io.call`, success JSON flows
/// back to the script, a `EgressError` round-trips as a classified capability error, and the
/// seam is withheld under `Profile::Deterministic`. Gated to the capability-free build so
/// `ExecParams` has no I/O fields to populate.
#[cfg(test)]
#[cfg(not(feature = "_io"))]
mod egress_tests {
    use super::{EngineError, ExecOutcome, ExecParams, Profile, run};
    use crate::egress::{Egress, EgressError};
    use rquickjs::Runtime;
    use std::sync::Arc;
    use std::time::Duration;

    /// A stub egress: action `"fail"` returns a retryable `db` error; anything else echoes the
    /// payload back wrapped in `{"echoed": …}` (valid JSON, since the wrapper stringified it).
    struct EchoEgress;

    impl Egress for EchoEgress {
        fn call(
            &self,
            _name: &str,
            action: &str,
            payload_json: &str,
        ) -> Result<String, EgressError> {
            if action == "fail" {
                return Err(EgressError::new("db", "DB_TIMEOUT", "backend unreachable").retryable());
            }
            Ok(format!("{{\"echoed\":{payload_json}}}"))
        }
    }

    /// Builds `ExecParams` for the no-capability build with an optional egress port wired.
    fn params<'a>(
        runtime: &'a Runtime,
        script: &'a str,
        profile: Profile,
        egress: Option<Arc<dyn Egress>>,
    ) -> ExecParams<'a> {
        ExecParams {
            runtime,
            bytecode_cache: None,
            cache_namespace: None,
            script,
            context_json: "{\"n\":7}",
            timeout: Duration::from_secs(5),
            profile,
            sys_config: None,
            read_hook: None,
            egress,
            max_ops: 8,
            max_output_size: 0,
        }
    }

    /// A successful `io.call` returns the backend JSON to the script.
    #[test]
    fn resource_call_returns_backend_json() {
        let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
        let script =
            "function handler(ctx) { return json(io.call('orders', 'ping', { x: ctx.n })); }";
        let egress: Arc<dyn Egress> = Arc::new(EchoEgress);
        let exec = run(&params(&runtime, script, Profile::Full, Some(egress)))
            .unwrap_or_else(|_err| unreachable!());
        let ExecOutcome::Success(json) = exec.outcome else {
            unreachable!("expected a success outcome");
        };
        assert!(
            json.contains("echoed"),
            "backend JSON flows to the script: {json}"
        );
        assert!(
            json.contains("\"x\":7"),
            "the payload round-tripped: {json}"
        );
    }

    /// A `EgressError` round-trips through the `__jsbox` tag and classifies as a capability
    /// error (not a generic script error), preserving the `db` source.
    #[test]
    fn resource_error_classifies_as_capability() {
        let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
        let script = "function handler(ctx) { return json(io.call('orders', 'fail', {})); }";
        let egress: Arc<dyn Egress> = Arc::new(EchoEgress);
        let exec = run(&params(&runtime, script, Profile::Full, Some(egress)))
            .unwrap_or_else(|_err| unreachable!());
        assert!(
            matches!(exec.outcome, ExecOutcome::Error(EngineError::Capability(_))),
            "a EgressError must surface as a classified capability error"
        );
    }

    /// Under `Profile::Deterministic` the egress is withheld: `egress` is undefined even when
    /// one is wired (the boundary is enforced by the engine, not the caller).
    #[test]
    fn resource_withheld_under_deterministic_profile() {
        let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
        let script = "function handler() { return json(typeof egress); }";
        let egress: Arc<dyn Egress> = Arc::new(EchoEgress);
        let exec = run(&params(
            &runtime,
            script,
            Profile::Deterministic,
            Some(egress),
        ))
        .unwrap_or_else(|_err| unreachable!());
        let ExecOutcome::Success(json) = exec.outcome else {
            unreachable!("expected a success outcome");
        };
        assert!(
            json.contains("undefined"),
            "egress withheld under deterministic: {json}"
        );
    }
}
