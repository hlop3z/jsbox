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

use crate::breaker::CircuitBreaker;
use crate::bytecode::BytecodeCacheStats;
use crate::config::EngineConfig;
use crate::egress::Egress;
use crate::engine::{self, EngineError, ExecOutcome, ExecParams, Profile, ReadHook};
#[cfg(feature = "http")]
use crate::http::HttpMetric;
use crate::pool::{JsPool, PoolStats};
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
    /// Tokio handle, retained for `LogicHost::new` API stability.
    ///
    /// Vestigial: every async driver (`db`, `mongo`) now runs in the consumer's `Egress`
    /// adapter, which carries its own handle (`Handle::current()` on the request thread), so the
    /// engine no longer drives `block_on` and never reads this. Kept on the constructor pending
    /// the step-4/5 cleanup — see `docs/design/resource-egress.md`.
    #[expect(
        dead_code,
        reason = "async drivers moved to the Egress adapter; kept on new() for API stability"
    )]
    handle: Handle,
    /// `db` circuit breaker (Tier 3), shared across invocations. `None` = disabled.
    ///
    /// Vestigial on the host: `db` connections now happen in the consumer's `Egress` adapter
    /// (the binary's handler holds the breaker via `AppState` and passes it there), so the
    /// engine no longer reads this. Retained on [`LogicHost::new`] for API stability pending the
    /// step-4/5 cleanup — see `docs/design/resource-egress.md`.
    #[expect(
        dead_code,
        reason = "db connect/breaker moved to the Egress adapter; kept on new() for API stability"
    )]
    db_breaker: Option<Arc<CircuitBreaker>>,
    /// Engine sandbox limits (timeout, `max_ops`, output cap, wildcard policy, …).
    limits: EngineConfig,
    /// Relax the SSRF private-IP block (`api`/`s3`) — local-dev only.
    #[cfg_attr(
        not(any(feature = "http", feature = "s3")),
        expect(
            dead_code,
            reason = "the SSRF relax flag only applies to http/s3 targets"
        )
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
/// Driver-backed capabilities (`db`/`mongo`/`mail`/`redis`/`amq`/`auth`) are plain enable
/// flags — their connection/credentials live in the wired [`Egress`] port, resolved
/// operator-side from a logical resource name. `http`/`s3`/`sys` still carry their config
/// here. Under [`Profile::Deterministic`] every I/O capability is withheld regardless (the
/// boundary is enforced by the engine, not trusted here).
#[derive(Debug, Clone, Copy)]
pub struct CapabilitySet<'a> {
    /// Allowed hosts for the `api` HTTP client (empty = `api` disabled).
    #[cfg(feature = "http")]
    pub allowed_hosts: &'a [String],
    /// Whether to expose `db`. The connection + credentials live in the wired [`Egress`] port
    /// (resolved operator-side from a logical resource name), never in the request — so this is
    /// just an enable flag, no config crosses the engine boundary.
    #[cfg(feature = "db")]
    pub db: engine::Gate,
    /// Whether to expose `mongo` (see [`db`](Self::db)).
    #[cfg(feature = "mongo")]
    pub mongo: engine::Gate,
    /// Whether to expose `mail` (see [`db`](Self::db)).
    #[cfg(feature = "mail")]
    pub mail: engine::Gate,
    /// `s3` (object storage) config. Stays in-engine (pure `SigV4` presign, no driver), so it
    /// still carries its config here unlike the driver-backed capabilities.
    #[cfg(feature = "s3")]
    pub s3: Option<&'a S3Config>,
    /// Whether to expose `redis` (see [`db`](Self::db)).
    #[cfg(feature = "redis")]
    pub redis: engine::Gate,
    /// Whether to expose `amq` (see [`db`](Self::db)).
    #[cfg(feature = "amq")]
    pub amq: engine::Gate,
    /// Whether to expose `auth` (see [`db`](Self::db)).
    #[cfg(feature = "auth")]
    pub auth: engine::Gate,
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
        db: engine::Gate::Off,
        #[cfg(feature = "mongo")]
        mongo: engine::Gate::Off,
        #[cfg(feature = "mail")]
        mail: engine::Gate::Off,
        #[cfg(feature = "s3")]
        s3: None,
        #[cfg(feature = "redis")]
        redis: engine::Gate::Off,
        #[cfg(feature = "amq")]
        amq: engine::Gate::Off,
        #[cfg(feature = "auth")]
        auth: engine::Gate::Off,
        sys: None,
    };
}

/// One execution request.
///
/// `#[non_exhaustive]`: construct via [`Invocation::inline`] / [`Invocation::registered`] and
/// the builder setters, never a struct literal. This makes additive fields (like the
/// `egress` port) backward-compatible for external consumers (see
/// `crates/runlet-core/CONSUMER_NOTES.md` item #2).
#[non_exhaustive]
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
    /// Optional I/O egress seam (the `io.call` global). `None` = no `io` global.
    /// Withheld under [`Profile::Deterministic`] (it performs I/O). The HTTP front passes
    /// `None`; a sidecar-backed consumer wires its egress here.
    pub egress: Option<Arc<dyn Egress>>,
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
            .field("egress", &self.egress.as_ref().map(|_res| "<egress>"))
            .field("cache_namespace", &self.cache_namespace)
            .finish()
    }
}

impl<'a> Invocation<'a> {
    /// An invocation from inline source + JSON context, defaulting to [`Profile::Full`], no
    /// capabilities ([`CapabilitySet::NONE`]), no read-hook, no egress port, and the global
    /// (unnamespaced) bytecode cache. Refine with the builder setters.
    #[must_use]
    pub const fn inline(source: &'a str, context_json: &'a str) -> Self {
        Self::with_defaults(CodeRef::Inline(source), context_json)
    }

    /// An invocation from a registry key + JSON context, with the same defaults as
    /// [`inline`](Self::inline).
    #[must_use]
    pub const fn registered(key: &'a str, context_json: &'a str) -> Self {
        Self::with_defaults(CodeRef::Registered(key), context_json)
    }

    /// Shared constructor: all optionals at their defaults.
    #[must_use]
    const fn with_defaults(code: CodeRef<'a>, context_json: &'a str) -> Self {
        Self {
            code,
            context_json,
            profile: Profile::Full,
            caps: CapabilitySet::NONE,
            read_hook: None,
            egress: None,
            cache_namespace: None,
        }
    }

    /// Sets the capability + determinism profile.
    #[must_use]
    pub const fn profile(mut self, profile: Profile) -> Self {
        self.profile = profile;
        self
    }

    /// Sets the capabilities to inject (subject to the profile).
    #[must_use]
    pub const fn caps(mut self, caps: CapabilitySet<'a>) -> Self {
        self.caps = caps;
        self
    }

    /// Sets the read-of-declared-dependencies hook (the deterministic-profile seam).
    #[must_use]
    pub fn read_hook(mut self, hook: Arc<ReadHook>) -> Self {
        self.read_hook = Some(hook);
        self
    }

    /// Sets the I/O egress port (the `io.call` seam).
    #[must_use]
    pub fn egress(mut self, egress: Arc<dyn Egress>) -> Self {
        self.egress = Some(egress);
        self
    }

    /// Sets the bytecode-cache partition namespace (per-tenant cache isolation).
    #[must_use]
    pub const fn cache_namespace(mut self, namespace: &'a str) -> Self {
        self.cache_namespace = Some(namespace);
        self
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

/// In-engine capability metrics drained from one execution (`http`/`s3`).
///
/// The driver-backed capabilities (`db`/`mongo`/`mail`/`redis`/`amq`/`auth`) run in the wired
/// [`Egress`] adapter, not the engine, so their metrics are drained by the consumer straight from
/// that adapter (see `fabric_backends::BackendSet`) — they don't ride here.
#[derive(Debug, Default)]
// With neither in-engine capability the struct is empty, so it can (and per the lint must) be
// `Copy`; with `http` or `s3` it holds `Vec`s and cannot.
#[cfg_attr(not(any(feature = "http", feature = "s3")), derive(Clone, Copy))]
pub struct ExecMetrics {
    /// HTTP (`api`) request metrics.
    #[cfg(feature = "http")]
    pub http: Vec<HttpMetric>,
    /// `s3` operation metrics.
    #[cfg(feature = "s3")]
    pub s3: Vec<S3Metric>,
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

    /// A snapshot of runtime-pool liveness (configured size, idle, in-flight) — for
    /// operability gauges and for a consumer to drive its own graceful-drain loop.
    #[must_use]
    pub fn pool_stats(&self) -> PoolStats {
        self.pool.stats()
    }

    /// Begins graceful teardown: subsequent [`run`](Self::run) calls are rejected with
    /// [`EngineError::ShuttingDown`], and the warm runtime pool is disposed (in-flight
    /// executions finish and dispose their own runtime on release).
    ///
    /// This is a **surface-agnostic primitive**: signal handling and in-flight draining stay
    /// with the consumer. A typical sequence is `host.shutdown()`, then poll
    /// [`pool_stats`](Self::pool_stats) until `in_flight` reaches zero (bounded by the
    /// wall-clock cap) before exiting. Because each capability uses a fresh per-request connection (torn down
    /// at request end), no long-lived driver connections outlive the host. Idempotent; cheap.
    pub fn shutdown(&self) {
        self.pool.shutdown();
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
        if !self.pool.is_accepting() {
            return Err(EngineError::ShuttingDown);
        }

        let source = match inv.code {
            CodeRef::Inline(source) => ResolvedSource::Borrowed(source),
            CodeRef::Registered(key) => {
                ResolvedSource::Owned(self.registry.get(key).ok_or_else(|| {
                    EngineError::ScriptNotFound(format!("no registered script for key `{key}`"))
                })?)
            }
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
                script: source.as_str(),
                context_json: inv.context_json,
                timeout: self.limits.timeout(),
                profile: inv.profile,
                #[cfg(feature = "http")]
                allowed_hosts: inv.caps.allowed_hosts,
                #[cfg(feature = "db")]
                db_enabled: inv.caps.db,
                #[cfg(feature = "mongo")]
                mongo_enabled: inv.caps.mongo,
                #[cfg(feature = "mail")]
                mail_enabled: inv.caps.mail,
                #[cfg(feature = "s3")]
                s3_config: inv.caps.s3,
                #[cfg(feature = "redis")]
                redis_enabled: inv.caps.redis,
                #[cfg(feature = "amq")]
                amq_enabled: inv.caps.amq,
                #[cfg(feature = "auth")]
                auth_enabled: inv.caps.auth,
                sys_config: inv.caps.sys,
                read_hook: inv.read_hook,
                egress: inv.egress,
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
                #[cfg(feature = "s3")]
                s3: result.s3_metrics,
            },
        })
    }
}
