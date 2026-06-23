//! The callable `LogicHost` port — drive the engine directly, with no HTTP assumption.
//!
//! `runlet-core`'s public entry point. A consumer builds an [`Invocation`] (inline source
//! or a registry key, a JSON context, a [`Profile`], and the [`CapabilitySet`] to inject)
//! and calls [`LogicHost::run`]; the host acquires a pooled runtime, executes, and returns
//! an [`Outcome`] (the `{data,error}` envelope, the declarative `emit` effects, and the
//! per-capability metrics). The HTTP front (`runlet`) is one consumer; a write-path / CDC /
//! action scheduler is another.
//!
//! `run` is synchronous and must be called from a blocking context (the host captures a
//! tokio [`Handle`] at construction to drive async capability I/O via `block_on`, exactly as
//! the engine requires — `QuickJS` is single-threaded and must not run on a runtime worker).

use core::fmt;
use std::sync::Arc;

use serde_json::value::RawValue;
use tokio::runtime::Handle;

#[cfg(feature = "amq")]
use crate::amq::{AmqConfig, AmqMetric};
#[cfg(feature = "auth")]
use crate::auth::{AuthConfig, AuthMetric};
use crate::breaker::CircuitBreaker;
use crate::bytecode::BytecodeCacheStats;
use crate::config::EngineConfig;
#[cfg(feature = "db")]
use crate::db::{DbConfig, DbMetric};
use crate::engine::{self, EngineError, ExecOutcome, ExecParams, Profile, ReadHook};
#[cfg(feature = "http")]
use crate::http::HttpMetric;
#[cfg(feature = "redis")]
use crate::kv::{RedisConfig, RedisMetric};
#[cfg(feature = "mail")]
use crate::mail::{MailConfig, MailMetric};
#[cfg(feature = "mongo")]
use crate::mongo::{MongoConfig, MongoMetric};
use crate::pool::JsPool;
use crate::registry::ScriptRegistry;
#[cfg(feature = "s3")]
use crate::s3::{S3Config, S3Metric};
use crate::sys::SysConfig;

/// A reusable, callable logic host.
///
/// A pooled `QuickJS` engine plus the resilience wiring, usable without any HTTP
/// involvement. Cheap to [`Clone`] (all state is `Arc`/`Copy`-backed) so it can be moved
/// into a `spawn_blocking` task per request.
#[derive(Clone, Debug)]
pub struct LogicHost {
    /// Pool of pre-warmed runtimes.
    pool: JsPool,
    /// Tokio handle driving async capability I/O from the blocking thread (`block_on`).
    /// Only consumed by the async `db`/`mongo` drivers.
    #[cfg_attr(
        not(any(feature = "db", feature = "mongo")),
        expect(dead_code, reason = "only the async db/mongo drivers use the runtime handle")
    )]
    handle: Handle,
    /// `db` circuit breaker (Tier 3), shared across invocations. `None` = disabled.
    #[cfg_attr(
        not(feature = "db"),
        expect(dead_code, reason = "the circuit breaker only guards the db capability")
    )]
    db_breaker: Option<Arc<CircuitBreaker>>,
    /// Engine sandbox limits (timeout, `max_ops`, output cap, wildcard policy, …).
    limits: EngineConfig,
    /// Relax the SSRF private-IP block (`api`/`s3`) — local-dev only.
    #[cfg_attr(
        not(any(feature = "http", feature = "s3")),
        expect(dead_code, reason = "the SSRF relax flag only applies to http/s3 targets")
    )]
    allow_private_targets: bool,
    /// Registry resolving `CodeRef::Registered` keys to source.
    registry: Arc<ScriptRegistry>,
}

/// Engine-level settings for a [`LogicHost`] (grouped so `new` stays within the
/// argument-count limit).
#[derive(Debug, Clone, Copy)]
pub struct HostSettings {
    /// Engine sandbox limits.
    pub limits: EngineConfig,
    /// Relax the SSRF private-IP block (`api`/`s3`) — local-dev only.
    pub allow_private_targets: bool,
}

/// Where a script's source comes from.
#[derive(Debug, Clone, Copy)]
pub enum CodeRef<'a> {
    /// Inline source supplied by the caller.
    Inline(&'a str),
    /// A key resolved against the host's [`ScriptRegistry`].
    Registered(&'a str),
}

/// Which capabilities to inject for an invocation (per-request, opt-in).
///
/// Each `Some` config requests that capability; under [`Profile::Deterministic`] every I/O
/// capability is withheld regardless (the boundary is enforced by the engine, not trusted
/// here).
#[derive(Debug, Clone, Copy)]
pub struct CapabilitySet<'a> {
    /// Allowed hosts for the `api` HTTP client (empty = `api` disabled).
    #[cfg(feature = "http")]
    pub allowed_hosts: &'a [String],
    /// `db` (Postgres-family) config.
    #[cfg(feature = "db")]
    pub db: Option<&'a DbConfig>,
    /// `mongo` (document DB) config.
    #[cfg(feature = "mongo")]
    pub mongo: Option<&'a MongoConfig>,
    /// `mail` (SMTP) config.
    #[cfg(feature = "mail")]
    pub mail: Option<&'a MailConfig>,
    /// `s3` (object storage) config.
    #[cfg(feature = "s3")]
    pub s3: Option<&'a S3Config>,
    /// `redis` config.
    #[cfg(feature = "redis")]
    pub redis: Option<&'a RedisConfig>,
    /// `amq` (message broker) config.
    #[cfg(feature = "amq")]
    pub amq: Option<&'a AmqConfig>,
    /// `auth` (OIDC/IAM) config.
    #[cfg(feature = "auth")]
    pub auth: Option<&'a AuthConfig>,
    /// `$sys` env/secrets context.
    pub sys: Option<&'a SysConfig>,
}

impl CapabilitySet<'_> {
    /// An empty set — no capabilities requested (the natural default for the deterministic
    /// profile, where I/O is withheld anyway).
    pub const NONE: CapabilitySet<'static> = CapabilitySet {
        #[cfg(feature = "http")]
        allowed_hosts: &[],
        #[cfg(feature = "db")]
        db: None,
        #[cfg(feature = "mongo")]
        mongo: None,
        #[cfg(feature = "mail")]
        mail: None,
        #[cfg(feature = "s3")]
        s3: None,
        #[cfg(feature = "redis")]
        redis: None,
        #[cfg(feature = "amq")]
        amq: None,
        #[cfg(feature = "auth")]
        auth: None,
        sys: None,
    };
}

/// One execution request.
pub struct Invocation<'a> {
    /// Inline source or a registry key.
    pub code: CodeRef<'a>,
    /// Opaque JSON context, passed straight to `QuickJS` (validated as JSON by the caller).
    pub context_json: &'a str,
    /// Capability + determinism profile.
    pub profile: Profile,
    /// Capabilities to inject (subject to the profile).
    pub caps: CapabilitySet<'a>,
    /// Optional read-of-declared-dependencies hook (the deterministic-profile seam). `None`
    /// = no `read` global. The core never inspects what is read — opaque to it.
    pub read_hook: Option<Arc<ReadHook>>,
    /// Partition/tenant namespace for the bytecode cache key. Identical source under different
    /// namespaces gets separate cache entries (no cross-tenant dedup / compile-timing leak).
    /// `None` = global. Typically the caller's partition key.
    pub cache_namespace: Option<&'a str>,
}

impl fmt::Debug for Invocation<'_> {
    #[expect(
        clippy::renamed_function_params,
        reason = "descriptive name over the trait's terse `f`, matching the crate's min-ident lint"
    )]
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Invocation")
            .field("code", &self.code)
            .field("context_json", &self.context_json)
            .field("profile", &self.profile)
            .field("caps", &self.caps)
            .field("read_hook", &self.read_hook.as_ref().map(|_hook| "<hook>"))
            .field("cache_namespace", &self.cache_namespace)
            .finish()
    }
}

/// The result of an execution.
///
/// Carries the handler outcome, the declarative `emit` effects, and the drained
/// per-capability metrics. `effects` is opaque JSON the consumer interprets ("logic
/// proposes, the engine disposes"); the HTTP front simply ignores it.
#[derive(Debug)]
pub struct Outcome {
    /// Handler envelope (`{data,error}` JSON) or a classified engine error.
    pub result: ExecOutcome,
    /// Effects appended via `emit(value)`, in call order.
    pub effects: Vec<Box<RawValue>>,
    /// Per-capability operation metrics.
    pub metrics: ExecMetrics,
}

/// Per-capability metrics drained from one execution (mirrors the `meta.<cap>_requests`
/// families surfaced by the HTTP front).
#[derive(Debug, Default)]
// With no capability features the struct is empty, so it can (and per the lint must) be
// `Copy`; with any I/O capability it holds `Vec`s and cannot.
#[cfg_attr(not(feature = "_io"), derive(Clone, Copy))]
pub struct ExecMetrics {
    /// HTTP (`api`) request metrics.
    #[cfg(feature = "http")]
    pub http: Vec<HttpMetric>,
    /// `db` operation metrics.
    #[cfg(feature = "db")]
    pub db: Vec<DbMetric>,
    /// `mongo` operation metrics.
    #[cfg(feature = "mongo")]
    pub mongo: Vec<MongoMetric>,
    /// `mail` operation metrics.
    #[cfg(feature = "mail")]
    pub mail: Vec<MailMetric>,
    /// `s3` operation metrics.
    #[cfg(feature = "s3")]
    pub s3: Vec<S3Metric>,
    /// `redis` operation metrics.
    #[cfg(feature = "redis")]
    pub redis: Vec<RedisMetric>,
    /// `amq` operation metrics.
    #[cfg(feature = "amq")]
    pub amq: Vec<AmqMetric>,
    /// `auth` operation metrics.
    #[cfg(feature = "auth")]
    pub auth: Vec<AuthMetric>,
}

/// Resolved script source — borrowed (inline) or owned (a registry `Arc<str>` kept alive
/// for the duration of the call).
enum ResolvedSource<'a> {
    /// Inline source borrowed from the invocation.
    Borrowed(&'a str),
    /// Source resolved from the registry, owned for the call.
    Owned(Arc<str>),
}

impl ResolvedSource<'_> {
    /// The source text.
    fn as_str(&self) -> &str {
        match self {
            Self::Borrowed(source) => source,
            Self::Owned(source) => source,
        }
    }
}

impl LogicHost {
    /// Builds a host from a runtime pool, a tokio handle, the optional `db` breaker, the
    /// script registry, and the engine [`HostSettings`].
    #[must_use]
    pub const fn new(
        pool: JsPool,
        handle: Handle,
        db_breaker: Option<Arc<CircuitBreaker>>,
        registry: Arc<ScriptRegistry>,
        settings: HostSettings,
    ) -> Self {
        Self {
            pool,
            handle,
            db_breaker,
            limits: settings.limits,
            allow_private_targets: settings.allow_private_targets,
            registry,
        }
    }

    /// The script registry, for a consumer that resolves keys itself (e.g. to size or
    /// pre-validate the source before invoking).
    #[must_use]
    pub fn registry(&self) -> &ScriptRegistry {
        &self.registry
    }

    /// The engine sandbox limits.
    #[must_use]
    pub const fn limits(&self) -> &EngineConfig {
        &self.limits
    }

    /// A snapshot of the compiled-bytecode cache's activity (resident entries + hit/miss/store
    /// counters) — for metrics / observability of the autonomous size-gated caching.
    #[must_use]
    pub fn bytecode_cache_stats(&self) -> BytecodeCacheStats {
        self.pool.bytecode_cache().stats()
    }

    /// Executes one [`Invocation`] on a pooled runtime.
    ///
    /// # Errors
    ///
    /// Returns an [`EngineError`] for a pre-execution failure that yields no outcome: a
    /// `CodeRef::Registered` miss ([`EngineError::ScriptNotFound`]), a pool/runtime
    /// acquisition failure, or context creation ([`EngineError::Internal`]). Every
    /// in-execution failure is carried in `Outcome.result` as [`ExecOutcome::Error`].
    pub fn run(&self, inv: Invocation<'_>) -> Result<Outcome, EngineError> {
        let source = match inv.code {
            CodeRef::Inline(source) => ResolvedSource::Borrowed(source),
            CodeRef::Registered(key) => ResolvedSource::Owned(self.registry.get(key).ok_or_else(
                || EngineError::ScriptNotFound(format!("no registered script for key `{key}`")),
            )?),
        };

        let runtime = self
            .pool
            .acquire()
            .map_err(|err| EngineError::Internal(err.to_string()))?;

        // Scope `params` so its borrow of `runtime` ends before the runtime is released.
        let exec = {
            let params = ExecParams {
                runtime: &runtime,
                bytecode_cache: Some(self.pool.bytecode_cache()),
                cache_namespace: inv.cache_namespace,
                #[cfg(any(feature = "db", feature = "mongo"))]
                tokio_handle: &self.handle,
                #[cfg(feature = "db")]
                db_breaker: self.db_breaker.as_deref(),
                script: source.as_str(),
                context_json: inv.context_json,
                timeout: self.limits.timeout(),
                profile: inv.profile,
                #[cfg(feature = "http")]
                allowed_hosts: inv.caps.allowed_hosts,
                #[cfg(feature = "db")]
                db_config: inv.caps.db,
                #[cfg(feature = "mongo")]
                mongo_config: inv.caps.mongo,
                #[cfg(feature = "mail")]
                mail_config: inv.caps.mail,
                #[cfg(feature = "s3")]
                s3_config: inv.caps.s3,
                #[cfg(feature = "redis")]
                redis_config: inv.caps.redis,
                #[cfg(feature = "amq")]
                amq_config: inv.caps.amq,
                #[cfg(feature = "auth")]
                auth_config: inv.caps.auth,
                sys_config: inv.caps.sys,
                read_hook: inv.read_hook,
                max_ops: self.limits.max_ops,
                max_output_size: self.limits.max_output_size,
                #[cfg(any(feature = "http", feature = "s3"))]
                allow_private_targets: self.allow_private_targets,
                // `*` honored only as explicit opt-in, never in SSRF-relaxed debug mode.
                #[cfg(feature = "http")]
                wildcard_hosts_allowed: self.limits.allow_wildcard_hosts
                    && !self.allow_private_targets,
            };
            engine::run(&params)
        };
        self.pool.release(runtime);

        let result = exec?;
        Ok(Outcome {
            result: result.outcome,
            effects: result.effects,
            metrics: ExecMetrics {
                #[cfg(feature = "http")]
                http: result.http_metrics,
                #[cfg(feature = "db")]
                db: result.db_metrics,
                #[cfg(feature = "mongo")]
                mongo: result.mongo_metrics,
                #[cfg(feature = "mail")]
                mail: result.mail_metrics,
                #[cfg(feature = "s3")]
                s3: result.s3_metrics,
                #[cfg(feature = "redis")]
                redis: result.redis_metrics,
                #[cfg(feature = "amq")]
                amq: result.amq_metrics,
                #[cfg(feature = "auth")]
                auth: result.auth_metrics,
            },
        })
    }
}
