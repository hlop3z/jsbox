//! Configuration loaded from an optional `config.json` file.
//!
//! All fields have sensible defaults. If the file is missing,
//! the server starts with defaults.
//!
//! Size fields accept human-readable strings: `"8mb"`, `"256kb"`, `"1gb"`,
//! or plain numbers in bytes: `8388608`.

use std::error::Error;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::bytesize::deserialize_byte_size;

/// Empirically-measured heap cost of parsing a JSON context into `QuickJS` objects, as
/// a multiple of the JSON text size (~4×, stable across 16/32/64 MB memory limits). A
/// context larger than `memory_limit / PARSE_HEAP_FACTOR` cannot be parsed at all, so
/// this is the hard ceiling the load-time invariant enforces — keeping an oversized
/// context a clean `CONTEXT_TOO_LARGE` (400) instead of a runtime out-of-memory.
const PARSE_HEAP_FACTOR: usize = 4;

/// Divisor for the auto-derived `max_context_size` (used when it is left at `0`).
/// Parsing costs ~4× and a typical transform needs ~6× the text size, so dividing the
/// memory limit by 8 leaves headroom for real handler work, not just loading the input.
const CONTEXT_LIMIT_DIVISOR: usize = 8;

/// Multiplier for the auto-derived concurrency bulkhead (used when
/// `max_concurrent_executions` is left at `0`): `pool_size × this`. Generous enough not
/// to throttle typical I/O-bound load, but far below the `spawn_blocking` thread ceiling
/// (~512) so a slow downstream can't exhaust the runtime (see `docs/design/resilience.md`).
const AUTO_CONCURRENCY_FACTOR: usize = 16;

/// Default number of hashed partition buckets when `partition_buckets` is left at `0`.
/// Enough that distinct keys rarely collide while keeping the semaphore array small.
const DEFAULT_PARTITION_BUCKETS: usize = 256;

/// Default `db` circuit-breaker cool-down (ms) when `db_breaker_cooldown_ms` is `0`.
const DEFAULT_BREAKER_COOLDOWN_MS: u64 = 5000;

/// Top-level configuration. `Default` is derived — every field's default is its type
/// default (`false` / `None` / the nested config's own `Default`), including the
/// security-relevant `error_debug: false` (secure by default) and `access_token: None`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Config {
    /// Local-dev switch. When `true`, the SSRF private-IP block is relaxed so
    /// localhost / LAN targets (e.g. `MinIO`) work for `s3` and `api`. Never enable in
    /// production — it removes the guard against internal/local targets.
    pub(crate) debug: bool,
    /// Include `error.debug` (stack traces + raw driver causes) in responses. Default
    /// `false` (secure by default): the raw cause can carry internal hostnames / driver
    /// detail, so an operator running purely internally opts *in* to the verbosity. The
    /// `trace_id` is always present and the raw cause is always logged server-side, so
    /// support can correlate without leaking detail across the boundary. Kept separate from
    /// `debug` (which only relaxes the SSRF guard) so the two don't entangle.
    pub(crate) error_debug: bool,
    /// Server configuration.
    pub(crate) server: ServerConfig,
    /// JS engine sandbox limits.
    pub(crate) engine: EngineConfig,
    /// Directory of registered scripts (`*.js`), loaded once at startup; a script's
    /// key is its relative path without the extension (`acme/billing/pricing.js` →
    /// `acme/billing/pricing`). Omit to disable execute-by-key (`key` requests then
    /// fail with `SCRIPT_NOT_FOUND`).
    pub(crate) scripts_dir: Option<PathBuf>,
    /// Directory of injectable ES modules (`*.js` / `*.mjs`), loaded once at startup; a
    /// module's specifier is its relative path without the extension (`acme/pricing.mjs`
    /// → `acme/pricing`). A handler `import`s them by that specifier. Omit to disable
    /// `import` (any `import` of a module then fails to resolve).
    pub(crate) modules_dir: Option<PathBuf>,
    /// Shared-secret bearer token gating `/execute`. When set, a request must carry
    /// `Authorization: Bearer <token>` (constant-time compared) or it is rejected `401
    /// UNAUTHORIZED`. `/health` and `/metrics` stay open (probe/scrape paths). This is
    /// defense in depth behind the gateway, not a replacement for it — the `/execute` caller
    /// is fully trusted (it supplies credentials), so an unauthenticated reachable port is a
    /// full compromise. Omit only when auth is genuinely terminated upstream (see
    /// `allow_unauthenticated`).
    #[serde(default)]
    pub(crate) access_token: Option<String>,
    /// Explicit acknowledgement that `/execute` may run without a token on a non-loopback
    /// bind (auth handled by an upstream gateway/mesh). Default `false`: jsbox **refuses to
    /// start** on a non-loopback address when no `access_token` is set, so a misconfigured
    /// deployment fails closed instead of silently exposing an unauthenticated executor. A
    /// loopback bind never needs this.
    #[serde(default)]
    pub(crate) allow_unauthenticated: bool,
}

/// HTTP server settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub(crate) struct ServerConfig {
    /// Address to bind to.
    pub(crate) host: IpAddr,
    /// Port to listen on.
    pub(crate) port: u16,
}

/// JS engine sandbox limits.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub(crate) struct EngineConfig {
    /// Maximum memory a script can allocate (e.g. `"8mb"`, `"16mb"`, or bytes).
    #[serde(deserialize_with = "deserialize_byte_size")]
    pub(crate) memory_limit: usize,
    /// Maximum native stack size (e.g. `"256kb"`, `"512kb"`, or bytes).
    #[serde(deserialize_with = "deserialize_byte_size")]
    pub(crate) max_stack_size: usize,
    /// Maximum execution time in milliseconds.
    pub(crate) timeout_ms: u64,
    /// Number of pooled runtimes (0 = auto-detect CPU cores).
    pub(crate) pool_size: usize,
    /// Maximum script size (e.g. `"1mb"`, default 1 MB).
    #[serde(deserialize_with = "deserialize_byte_size")]
    pub(crate) max_script_size: usize,
    /// Maximum context payload size. Leave at `0` (the default) to auto-derive
    /// `memory_limit / CONTEXT_LIMIT_DIVISOR` — change `memory_limit` alone and this
    /// tracks it. An explicit value is capped at `memory_limit / PARSE_HEAP_FACTOR`
    /// (the parse ceiling) at load; exceeding it is a startup error.
    #[serde(deserialize_with = "deserialize_byte_size")]
    pub(crate) max_context_size: usize,
    /// Maximum HTTP/DB operations per execution (default 50).
    pub(crate) max_ops: usize,
    /// Maximum size (bytes) of the JSON the handler may return. `0` = off (bounded only by
    /// `memory_limit`). Set it in an untrusted-script deployment so one handler can't return
    /// a `memory_limit`-sized blob as a bandwidth/amplification channel — a request over the
    /// cap fails with `OUTPUT_TOO_LARGE` (422). Accepts human sizes (`"1mb"`).
    #[serde(deserialize_with = "deserialize_byte_size")]
    pub(crate) max_output_size: usize,
    /// Bulkhead: max concurrent executions in flight. `0` = auto
    /// (`pool_size × AUTO_CONCURRENCY_FACTOR`). Excess load fast-fails `429 OVERLOADED`
    /// rather than exhausting blocking threads / DB connections under a slow downstream
    /// (see `docs/design/resilience.md`). Tune to the downstream connection budget.
    pub(crate) max_concurrent_executions: usize,
    /// Per-partition fairness (Tier 5): max concurrent executions per partition key
    /// (`X-Partition-Key` header / `partition` field). `0` = off. A key over its share
    /// fast-fails `429 PARTITION_OVERLOADED` even when global capacity remains, so one
    /// noisy key can't monopolize this pod. Per-pod backstop, not a global guarantee
    /// (the gateway owns global per-partition fairness — see `docs/design/resilience.md`).
    pub(crate) max_concurrent_per_partition: usize,
    /// Number of hashed partition buckets, used only when per-partition fairness is on.
    /// More buckets = fewer key collisions, more semaphores. `0` = default 256.
    pub(crate) partition_buckets: usize,
    /// Operator ceiling (ms) for the `db` `statement_timeout`. `0` = no ceiling. A
    /// per-request `config.db.statement_timeout_ms` is clamped to this, and a request
    /// value of `0` ("unlimited") becomes this ceiling — so jsbox never issues an
    /// unbounded `SET`. The robust, pooler-proof ceiling is still a server-side role
    /// default; this is defense in depth (see `docs/design/resilience.md`).
    pub(crate) max_statement_timeout_ms: u64,
    /// Circuit breaker (Tier 5/3): consecutive `db` connect failures (per `host:port`)
    /// that trip the breaker open. `0` = off. While open, `db` requests to that target
    /// fast-fail `DB_CIRCUIT_OPEN` (retryable) instead of waiting on the connect timeout
    /// to a dead database (see `docs/design/resilience.md`).
    pub(crate) db_breaker_threshold: u32,
    /// How long (ms) the `db` circuit breaker stays open before allowing a half-open
    /// probe. Used only when `db_breaker_threshold > 0`. `0` = default 5000.
    pub(crate) db_breaker_cooldown_ms: u64,
    /// Whether a request may use the `allowed_hosts: ["*"]` wildcard for the `api` client.
    /// Default `false`: a `*` is ignored (matches nothing), so a request must name each host
    /// explicitly. `*` is dangerous because it removes the host allowlist and leaves only the
    /// private-IP filter — so it is honored only when this is `true` **and** `debug` is off
    /// (the SSRF-relaxed local mode never permits a wildcard).
    pub(crate) allow_wildcard_hosts: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 3000,
        }
    }
}

/// `EngineConfig`
///
/// This configuration controls execution limits for the scripting engine,
/// and each group of settings maps to a specific safety boundary:
///
/// # CPU safety
/// - `max_ops`: limits total instruction execution count per script
/// - `timeout_ms`: limits wall-clock execution time
///
/// Together, these prevent runaway computation and infinite loops.
///
/// # Data safety
/// - `memory_limit`: caps total heap usage for script execution
/// - `max_context_size`: limits size of input context passed into the script
///
/// Together, these prevent memory exhaustion from large payloads or allocations.
///
/// # Recursion safety
/// - `max_stack_size`: limits call stack depth and prevents stack overflow
///
/// This protects against deep recursion or excessively nested function calls.
///
/// # Throughput
/// - `pool_size`: controls number of concurrent execution workers
///
/// Higher values increase parallelism and request throughput, but may increase
/// resource contention under load.
impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            memory_limit: 32 * 1024 * 1024,  // 32mb
            max_stack_size: 512 * 1024,      // 512kb
            timeout_ms: 4000,                // 4s balanced default
            pool_size: 0,                    // Auto
            max_script_size: 1024 * 1024,    // 1mb
            max_context_size: 0,             // 0 = auto: memory_limit / CONTEXT_LIMIT_DIVISOR
            max_ops: 1500,                   // safe cap for API workloads
            max_output_size: 0,              // 0 = off (bounded by memory_limit)
            max_concurrent_executions: 0,    // 0 = auto: pool_size * AUTO_CONCURRENCY_FACTOR
            max_statement_timeout_ms: 0,     // 0 = no operator ceiling (opt-in)
            max_concurrent_per_partition: 0, // 0 = per-partition fairness off (opt-in)
            partition_buckets: 0,            // 0 = default DEFAULT_PARTITION_BUCKETS
            db_breaker_threshold: 0,         // 0 = circuit breaker off (opt-in)
            db_breaker_cooldown_ms: 0,       // 0 = default DEFAULT_BREAKER_COOLDOWN_MS
            allow_wildcard_hosts: false,     // `*` in allowed_hosts ignored unless opted in
        }
    }
}

impl ServerConfig {
    /// Returns the socket address from host + port.
    pub(crate) const fn addr(&self) -> SocketAddr {
        SocketAddr::new(self.host, self.port)
    }
}

impl EngineConfig {
    /// Returns the timeout as a `Duration`.
    pub(crate) const fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }

    /// Resolves the concurrency bulkhead size: the configured value, or the auto default
    /// `pool_size × AUTO_CONCURRENCY_FACTOR` when left at `0`. Never returns `0` (a
    /// zero-permit semaphore would deadlock every request).
    pub(crate) const fn resolved_max_concurrent(&self, pool_size: usize) -> usize {
        if self.max_concurrent_executions > 0 {
            return self.max_concurrent_executions;
        }
        let auto = pool_size.saturating_mul(AUTO_CONCURRENCY_FACTOR);
        if auto > 0 { auto } else { 1 }
    }

    /// Resolves the partition-bucket count: the configured value, or
    /// `DEFAULT_PARTITION_BUCKETS` when left at `0`.
    pub(crate) const fn resolved_partition_buckets(&self) -> usize {
        if self.partition_buckets > 0 {
            self.partition_buckets
        } else {
            DEFAULT_PARTITION_BUCKETS
        }
    }

    /// Resolves the `db` circuit-breaker cool-down: the configured value, or
    /// `DEFAULT_BREAKER_COOLDOWN_MS` when left at `0`.
    pub(crate) const fn resolved_breaker_cooldown_ms(&self) -> u64 {
        if self.db_breaker_cooldown_ms > 0 {
            self.db_breaker_cooldown_ms
        } else {
            DEFAULT_BREAKER_COOLDOWN_MS
        }
    }

    /// Maximum HTTP request body size (derived from script + context limits + overhead).
    pub(crate) const fn max_body_size(&self) -> usize {
        self.max_script_size
            .saturating_add(self.max_context_size)
            .saturating_add(64 * 1024)
    }

    /// Resolves the auto-derived context limit and enforces the parse-headroom
    /// invariant. Run once at load so the live config can never sit in a state where a
    /// byte-legal context is too large to parse — which would surface as a runtime
    /// out-of-memory instead of an up-front `CONTEXT_TOO_LARGE`.
    ///
    /// `max_context_size == 0` means "auto": derive `memory_limit / CONTEXT_LIMIT_DIVISOR`.
    ///
    /// # Errors
    ///
    /// Returns an error if an explicit `max_context_size` exceeds the parse ceiling
    /// `memory_limit / PARSE_HEAP_FACTOR`.
    fn resolve_limits(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        if self.max_context_size == 0 {
            self.max_context_size = self
                .memory_limit
                .checked_div(CONTEXT_LIMIT_DIVISOR)
                .unwrap_or(0);
        }
        let parse_ceiling = self
            .memory_limit
            .checked_div(PARSE_HEAP_FACTOR)
            .unwrap_or(0);
        if self.max_context_size > parse_ceiling {
            return Err(format!(
                "max_context_size ({} bytes) exceeds the parse ceiling memory_limit/{} ({} bytes): \
                 a context that large cannot be parsed within the {}-byte memory limit and would \
                 fail at runtime. Lower it to <= {} bytes, omit it to auto-derive memory_limit/{}, \
                 or raise memory_limit.",
                self.max_context_size,
                PARSE_HEAP_FACTOR,
                parse_ceiling,
                self.memory_limit,
                parse_ceiling,
                CONTEXT_LIMIT_DIVISOR,
            )
            .into());
        }
        Ok(())
    }
}

impl Config {
    /// Fail-closed start gate: refuse to bind a **non-loopback** address with no
    /// `access_token` unless the operator explicitly set `allow_unauthenticated` (auth
    /// terminated upstream). A loopback bind is always fine. Keeps a misconfigured
    /// deployment from silently exposing an unauthenticated arbitrary-code executor.
    ///
    /// # Errors
    ///
    /// Returns an error describing the missing gate when the bind is exposed and neither a
    /// token nor the explicit opt-out is present.
    pub(crate) fn check_exposure(&self) -> Result<(), Box<dyn Error + Send + Sync>> {
        let exposed = !self.server.host.is_loopback();
        if exposed && self.access_token.is_none() && !self.allow_unauthenticated {
            return Err(format!(
                "refusing to start: binding {host} (non-loopback) with no `access_token` and \
                 `allow_unauthenticated` unset. /execute runs caller-supplied code with \
                 caller-supplied credentials, so an unauthenticated reachable port is a full \
                 compromise. Set `access_token`, bind loopback, or set \
                 `allow_unauthenticated: true` if auth is terminated upstream.",
                host = self.server.host,
            )
            .into());
        }
        Ok(())
    }

    /// Loads config from a file path. Returns defaults if the file doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be read or parsed, or if the
    /// resolved limits violate the parse-headroom invariant (see [`EngineConfig::resolve_limits`]).
    pub(crate) fn load(path: &Path) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let mut config = if path.exists() {
            let contents = fs::read_to_string(path)?;
            serde_json::from_str::<Self>(&contents)?
        } else {
            Self::default()
        };
        config.engine.resolve_limits()?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    //! The fail-closed exposure gate (`check_exposure`): a non-loopback bind requires either a
    //! token or the explicit `allow_unauthenticated` opt-out.

    use super::{Config, ServerConfig};
    use std::net::{IpAddr, Ipv4Addr};

    /// Builds a config with a chosen bind host, token, and opt-out (everything else default).
    fn exposure_cfg(host: IpAddr, token: Option<&str>, allow_unauth: bool) -> Config {
        Config {
            server: ServerConfig { host, port: 3000 },
            access_token: token.map(str::to_owned),
            allow_unauthenticated: allow_unauth,
            ..Config::default()
        }
    }

    /// A loopback bind never needs a token.
    #[test]
    fn loopback_needs_no_token() {
        let cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::LOCALHOST), None, false);
        assert!(
            cfg.check_exposure().is_ok(),
            "loopback is fine without a token"
        );
    }

    /// A non-loopback bind with no token and no opt-out refuses to start.
    #[test]
    fn exposed_without_token_fails_closed() {
        let cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::UNSPECIFIED), None, false);
        assert!(
            cfg.check_exposure().is_err(),
            "0.0.0.0 with no token must refuse to start"
        );
    }

    /// A token unlocks an exposed bind.
    #[test]
    fn exposed_with_token_ok() {
        let cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), Some("tok"), false);
        assert!(
            cfg.check_exposure().is_ok(),
            "a token unlocks an exposed bind"
        );
    }

    /// The explicit opt-out unlocks an exposed bind (auth terminated upstream).
    #[test]
    fn exposed_with_explicit_optout_ok() {
        let cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), None, true);
        assert!(
            cfg.check_exposure().is_ok(),
            "allow_unauthenticated unlocks an exposed bind"
        );
    }
}
