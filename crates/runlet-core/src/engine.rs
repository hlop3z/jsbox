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

// `std::error::Error` only appears in the `db`/`redis` inject-error mappers' signatures.
#[cfg(any(feature = "db", feature = "redis"))]
use std::error::Error;
use std::fmt::Display;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rquickjs::module::Evaluated;
use rquickjs::{Context, Ctx, Function, Module, Object, Runtime, Value as JsValue};
use serde::Deserialize;
use serde_json::Value;
use serde_json::value::RawValue;
// The tokio handle only drives the async `db`/`mongo` drivers; the breaker only guards `db`.
#[cfg(any(feature = "db", feature = "mongo"))]
use tokio::runtime::Handle;

#[cfg(feature = "amq")]
use crate::amq::{self, AmqConfig, AmqMetric};
#[cfg(feature = "auth")]
use crate::auth::{self, AuthConfig, AuthMetric};
#[cfg(feature = "db")]
use crate::breaker::CircuitBreaker;
#[cfg(feature = "db")]
use crate::db::{self, DbConfig, DbMetric};
use crate::decimal;
use crate::errors::{ErrorCategory, ErrorDebug, ErrorEnvelope, ErrorOwner, ErrorSource};
// `Fault` only reaches the engine through the `db`/`redis` inject-error mappers.
#[cfg(any(feature = "db", feature = "redis"))]
use crate::errors::Fault;
#[cfg(feature = "http")]
use crate::http::{self, HttpMetric};
#[cfg(feature = "redis")]
use crate::kv::{self, RedisConfig, RedisMetric};
#[cfg(feature = "mail")]
use crate::mail::{self, MailConfig, MailMetric};
use crate::modules;
#[cfg(feature = "mongo")]
use crate::mongo::{self, MongoConfig, MongoMetric};
#[cfg(feature = "s3")]
use crate::s3::{self, S3Config, S3Metric};
#[cfg(feature = "_io")]
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

/// Capability-injection + determinism profile for an execution.
///
/// A **runtime** injection decision (not a compile-time feature) so a single process can
/// run both tiers — see `TODO.md` / the consuming spec's "logic plane".
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
    /// Tokio runtime handle — drives async capability I/O (e.g. `db`) from this blocking
    /// thread via `block_on` (Tier 2, see `docs/design/resilience.md`).
    #[cfg(any(feature = "db", feature = "mongo"))]
    pub(crate) tokio_handle: &'a Handle,
    /// `db` circuit breaker (Tier 3). `None` = disabled.
    #[cfg(feature = "db")]
    pub(crate) db_breaker: Option<&'a CircuitBreaker>,
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
    /// Database config (None = disabled).
    #[cfg(feature = "db")]
    pub(crate) db_config: Option<&'a DbConfig>,
    /// Mongo (document DB) config (None = disabled).
    #[cfg(feature = "mongo")]
    pub(crate) mongo_config: Option<&'a MongoConfig>,
    /// Mail config (None = disabled).
    #[cfg(feature = "mail")]
    pub(crate) mail_config: Option<&'a MailConfig>,
    /// S3 config (None = disabled).
    #[cfg(feature = "s3")]
    pub(crate) s3_config: Option<&'a S3Config>,
    /// Redis config (None = disabled).
    #[cfg(feature = "redis")]
    pub(crate) redis_config: Option<&'a RedisConfig>,
    /// `RabbitMQ` config (None = disabled).
    #[cfg(feature = "amq")]
    pub(crate) amq_config: Option<&'a AmqConfig>,
    /// Auth (OIDC/IAM) config (None = disabled).
    #[cfg(feature = "auth")]
    pub(crate) auth_config: Option<&'a AuthConfig>,
    /// `$sys` env/secrets context (None = no env/secrets injected).
    pub(crate) sys_config: Option<&'a SysConfig>,
    /// Read-of-declared-dependencies hook (the deterministic-profile seam). `None` = no
    /// `read` global is injected.
    pub(crate) read_hook: Option<Arc<ReadHook>>,
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
    /// HTTP requests made during execution.
    #[cfg(feature = "http")]
    pub(crate) http_metrics: Vec<HttpMetric>,
    /// DB operations made during execution.
    #[cfg(feature = "db")]
    pub(crate) db_metrics: Vec<DbMetric>,
    /// Mongo operations made during execution.
    #[cfg(feature = "mongo")]
    pub(crate) mongo_metrics: Vec<MongoMetric>,
    /// Mail operations made during execution.
    #[cfg(feature = "mail")]
    pub(crate) mail_metrics: Vec<MailMetric>,
    /// S3 operations made during execution.
    #[cfg(feature = "s3")]
    pub(crate) s3_metrics: Vec<S3Metric>,
    /// Redis operations made during execution.
    #[cfg(feature = "redis")]
    pub(crate) redis_metrics: Vec<RedisMetric>,
    /// `RabbitMQ` operations made during execution.
    #[cfg(feature = "amq")]
    pub(crate) amq_metrics: Vec<AmqMetric>,
    /// Auth operations made during execution.
    #[cfg(feature = "auth")]
    pub(crate) auth_metrics: Vec<AuthMetric>,
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

    #[cfg(feature = "http")]
    let mut http_collector: Option<Collector<HttpMetric>> = None;
    #[cfg(feature = "db")]
    let mut db_collector: Option<Collector<DbMetric>> = None;
    #[cfg(feature = "mongo")]
    let mut mongo_collector: Option<Collector<MongoMetric>> = None;
    #[cfg(feature = "mail")]
    let mut mail_collector: Option<Collector<MailMetric>> = None;
    #[cfg(feature = "s3")]
    let mut s3_collector: Option<Collector<S3Metric>> = None;
    #[cfg(feature = "redis")]
    let mut redis_collector: Option<Collector<RedisMetric>> = None;
    #[cfg(feature = "amq")]
    let mut amq_collector: Option<Collector<AmqMetric>> = None;
    #[cfg(feature = "auth")]
    let mut auth_collector: Option<Collector<AuthMetric>> = None;

    #[cfg(feature = "_io")]
    let mut collectors = Collectors {
        #[cfg(feature = "http")]
        http: &mut http_collector,
        #[cfg(feature = "db")]
        db: &mut db_collector,
        #[cfg(feature = "mongo")]
        mongo: &mut mongo_collector,
        #[cfg(feature = "mail")]
        mail: &mut mail_collector,
        #[cfg(feature = "s3")]
        s3: &mut s3_collector,
        #[cfg(feature = "redis")]
        redis: &mut redis_collector,
        #[cfg(feature = "amq")]
        amq: &mut amq_collector,
        #[cfg(feature = "auth")]
        auth: &mut auth_collector,
    };

    let js_result = ctx.with(|qctx| -> Result<ExecOutcome, EngineError> {
        inject_bridge(&qctx).map_err(EngineError::internal)?;
        decimal::inject_decimal(&qctx).map_err(EngineError::internal)?;
        sys::inject_sys(&qctx, params.sys_config).map_err(EngineError::internal)?;
        inject_emit(&qctx, &effects, params.max_ops).map_err(EngineError::internal)?;
        if let Some(hook) = &params.read_hook {
            inject_read(&qctx, Arc::clone(hook)).map_err(EngineError::internal)?;
        }
        #[cfg(feature = "_io")]
        inject_apis(&qctx, params, &mut collectors)?;
        let handler = match resolve_handler(&qctx, params.script, params.profile) {
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
        #[cfg(feature = "db")]
        db_metrics: sandbox::drain(db_collector.as_ref()),
        #[cfg(feature = "mongo")]
        mongo_metrics: sandbox::drain(mongo_collector.as_ref()),
        #[cfg(feature = "mail")]
        mail_metrics: sandbox::drain(mail_collector.as_ref()),
        #[cfg(feature = "s3")]
        s3_metrics: sandbox::drain(s3_collector.as_ref()),
        #[cfg(feature = "redis")]
        redis_metrics: sandbox::drain(redis_collector.as_ref()),
        #[cfg(feature = "amq")]
        amq_metrics: sandbox::drain(amq_collector.as_ref()),
        #[cfg(feature = "auth")]
        auth_metrics: sandbox::drain(auth_collector.as_ref()),
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

/// Mutable references to the per-capability metric collectors.
///
/// Grouped into one struct so [`inject_apis`] stays within the argument-count
/// limit as capabilities are added. Exists only when at least one I/O capability is
/// compiled in (`feature = "_io"`).
#[cfg(feature = "_io")]
struct Collectors<'a> {
    /// HTTP metrics collector slot.
    #[cfg(feature = "http")]
    http: &'a mut Option<Collector<HttpMetric>>,
    /// DB metrics collector slot.
    #[cfg(feature = "db")]
    db: &'a mut Option<Collector<DbMetric>>,
    /// Mongo metrics collector slot.
    #[cfg(feature = "mongo")]
    mongo: &'a mut Option<Collector<MongoMetric>>,
    /// Mail metrics collector slot.
    #[cfg(feature = "mail")]
    mail: &'a mut Option<Collector<MailMetric>>,
    /// S3 metrics collector slot.
    #[cfg(feature = "s3")]
    s3: &'a mut Option<Collector<S3Metric>>,
    /// Redis metrics collector slot.
    #[cfg(feature = "redis")]
    redis: &'a mut Option<Collector<RedisMetric>>,
    /// `RabbitMQ` metrics collector slot.
    #[cfg(feature = "amq")]
    amq: &'a mut Option<Collector<AmqMetric>>,
    /// Auth metrics collector slot.
    #[cfg(feature = "auth")]
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
#[cfg(feature = "_io")]
fn inject_apis(
    qctx: &Ctx<'_>,
    params: &ExecParams<'_>,
    collectors: &mut Collectors<'_>,
) -> Result<(), EngineError> {
    // Profile enforcement: the deterministic tier gets **no** I/O capability, regardless of
    // what configs an `Invocation` carries — the boundary is enforced here, not trusted to
    // the author (only `$`/`$sys`, `emit`, and the read-hook remain, injected elsewhere).
    if params.profile != Profile::Full {
        return Ok(());
    }
    #[cfg(feature = "http")]
    if !params.allowed_hosts.is_empty() {
        *collectors.http = Some(
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
    #[cfg(feature = "db")]
    if let Some(db_cfg) = params.db_config {
        *collectors.db = Some(
            db::inject_db(
                qctx,
                db_cfg,
                &db::DbDeps {
                    handle: params.tokio_handle,
                    timeout: params.timeout,
                    breaker: params.db_breaker,
                },
                params.max_ops,
            )
            .map_err(map_db_inject_error)?,
        );
    }
    #[cfg(feature = "mongo")]
    if let Some(mongo_cfg) = params.mongo_config {
        *collectors.mongo = Some(
            mongo::inject_mongo(
                qctx,
                mongo_cfg,
                &mongo::MongoDeps {
                    handle: params.tokio_handle,
                    timeout: params.timeout,
                },
                params.max_ops,
            )
            .map_err(EngineError::internal)?,
        );
    }
    #[cfg(feature = "mail")]
    if let Some(mail_cfg) = params.mail_config {
        *collectors.mail =
            Some(mail::inject_mail(qctx, mail_cfg, params.max_ops).map_err(EngineError::internal)?);
    }
    #[cfg(feature = "s3")]
    if let Some(s3_cfg) = params.s3_config {
        *collectors.s3 = Some(
            s3::inject_s3(qctx, s3_cfg, params.max_ops, params.allow_private_targets)
                .map_err(EngineError::internal)?,
        );
    }
    #[cfg(feature = "redis")]
    if let Some(redis_cfg) = params.redis_config {
        *collectors.redis = Some(
            kv::inject_redis(qctx, redis_cfg, params.max_ops).map_err(map_redis_inject_error)?,
        );
    }
    #[cfg(feature = "amq")]
    if let Some(amq_cfg) = params.amq_config {
        *collectors.amq =
            Some(amq::inject_amq(qctx, amq_cfg, params.max_ops).map_err(EngineError::internal)?);
    }
    #[cfg(feature = "auth")]
    if let Some(auth_cfg) = params.auth_config {
        *collectors.auth =
            Some(auth::inject_auth(qctx, auth_cfg, params.max_ops).map_err(EngineError::internal)?);
    }
    Ok(())
}

/// Maps a `db` inject failure: a connection failure → retryable capability error;
/// an engine-setup failure → internal.
#[cfg(feature = "db")]
fn map_db_inject_error(err: Box<dyn Error + Send + Sync>) -> EngineError {
    if db::is_circuit_open(err.as_ref()) {
        EngineError::capability_inject(ErrorSource::Db, db::DB_CIRCUIT_OPEN_FAULT, err.to_string())
    } else if db::is_connect_error(err.as_ref()) {
        EngineError::capability_inject(ErrorSource::Db, db::DB_CONNECTION_FAULT, err.to_string())
    } else {
        EngineError::internal(err)
    }
}

/// Maps a `redis` inject failure: a connection failure → retryable capability error;
/// an engine-setup failure → internal.
#[cfg(feature = "redis")]
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
    script: &str,
    profile: Profile,
) -> Result<Function<'js>, ExecOutcome> {
    if is_es_module(script) {
        let module = eval_module(qctx, script).map_err(ExecOutcome::Error)?;
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
fn eval_module<'js>(qctx: &Ctx<'js>, script: &str) -> Result<Module<'js, Evaluated>, EngineError> {
    try_eval_module(qctx, script).map_err(|()| classify_module_error(qctx))
}

/// The raw eval attempt; `Err(())` leaves the pending exception set for classification.
fn try_eval_module<'js>(qctx: &Ctx<'js>, script: &str) -> Result<Module<'js, Evaluated>, ()> {
    let declared = Module::declare(qctx.clone(), "handler", script).map_err(drop)?;
    let (module, promise) = declared.eval().map_err(drop)?;
    promise.finish::<()>().map_err(drop)?;
    Ok(module)
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

    /// Builds a capability error for an inject-time failure (no JS throw involved).
    #[cfg(any(feature = "db", feature = "redis"))]
    fn capability_inject(source: ErrorSource, fault: Fault, raw: String) -> Self {
        Self::Capability(Box::new(CapabilityErr {
            source,
            code: fault.code.to_owned(),
            retryable: fault.retryable,
            owner: fault.owner,
            raw: Some(raw),
            stack: None,
            details: None,
        }))
    }

    /// HTTP status for this error per `docs/99-errors.md`.
    #[must_use]
    pub const fn http_status(&self) -> u16 {
        match self {
            Self::Internal(_) => 500,
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
