//! Engine sandbox limits — the reusable execution-config half of the host.
//!
//! Server/listener configuration (bind address, auth token, script/module dirs) lives
//! with the consumer (`runlet` keeps it in its own `config` module); this module owns only
//! the limits that bound a single execution, so a non-HTTP consumer can configure the
//! engine without inheriting any HTTP server concept.
//!
//! Size fields accept human-readable strings: `"8mb"`, `"256kb"`, `"1gb"`,
//! or plain numbers in bytes: `8388608`.

use std::error::Error;
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

/// JS engine sandbox limits.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct EngineConfig {
    /// Maximum memory a script can allocate (e.g. `"8mb"`, `"16mb"`, or bytes).
    #[serde(deserialize_with = "deserialize_byte_size")]
    pub memory_limit: usize,
    /// Maximum native stack size (e.g. `"256kb"`, `"512kb"`, or bytes).
    #[serde(deserialize_with = "deserialize_byte_size")]
    pub max_stack_size: usize,
    /// Maximum execution time in milliseconds.
    pub timeout_ms: u64,
    /// Number of pooled runtimes (0 = auto-detect CPU cores).
    pub pool_size: usize,
    /// Maximum script size (e.g. `"1mb"`, default 1 MB).
    #[serde(deserialize_with = "deserialize_byte_size")]
    pub max_script_size: usize,
    /// Maximum context payload size. Leave at `0` (the default) to auto-derive
    /// `memory_limit / CONTEXT_LIMIT_DIVISOR` — change `memory_limit` alone and this
    /// tracks it. An explicit value is capped at `memory_limit / PARSE_HEAP_FACTOR`
    /// (the parse ceiling) at load; exceeding it is a startup error.
    #[serde(deserialize_with = "deserialize_byte_size")]
    pub max_context_size: usize,
    /// Maximum HTTP/DB operations per execution (default 50).
    pub max_ops: usize,
    /// Maximum size (bytes) of the JSON the handler may return. `0` = off (bounded only by
    /// `memory_limit`). Set it in an untrusted-script deployment so one handler can't return
    /// a `memory_limit`-sized blob as a bandwidth/amplification channel — a request over the
    /// cap fails with `OUTPUT_TOO_LARGE` (422). Accepts human sizes (`"1mb"`).
    #[serde(deserialize_with = "deserialize_byte_size")]
    pub max_output_size: usize,
    /// Bulkhead: max concurrent executions in flight. `0` = auto
    /// (`pool_size × AUTO_CONCURRENCY_FACTOR`). Excess load fast-fails `429 OVERLOADED`
    /// rather than exhausting blocking threads / DB connections under a slow downstream
    /// (see `docs/design/resilience.md`). Tune to the downstream connection budget.
    pub max_concurrent_executions: usize,
    /// Per-partition fairness (Tier 5): max concurrent executions per partition key
    /// (`X-Partition-Key` header / `partition` field). `0` = off. A key over its share
    /// fast-fails `429 PARTITION_OVERLOADED` even when global capacity remains, so one
    /// noisy key can't monopolize this pod. Per-pod backstop, not a global guarantee
    /// (the gateway owns global per-partition fairness — see `docs/design/resilience.md`).
    pub max_concurrent_per_partition: usize,
    /// Number of hashed partition buckets, used only when per-partition fairness is on.
    /// More buckets = fewer key collisions, more semaphores. `0` = default 256.
    pub partition_buckets: usize,
    /// Operator ceiling (ms) for the `db` `statement_timeout`. `0` = no ceiling. A
    /// per-request `config.db.statement_timeout_ms` is clamped to this, and a request
    /// value of `0` ("unlimited") becomes this ceiling — so jsbox never issues an
    /// unbounded `SET`. The robust, pooler-proof ceiling is still a server-side role
    /// default; this is defense in depth (see `docs/design/resilience.md`).
    pub max_statement_timeout_ms: u64,
    /// Circuit breaker (Tier 5/3): consecutive `db` connect failures (per `host:port`)
    /// that trip the breaker open. `0` = off. While open, `db` requests to that target
    /// fast-fail `DB_CIRCUIT_OPEN` (retryable) instead of waiting on the connect timeout
    /// to a dead database (see `docs/design/resilience.md`).
    pub db_breaker_threshold: u32,
    /// How long (ms) the `db` circuit breaker stays open before allowing a half-open
    /// probe. Used only when `db_breaker_threshold > 0`. `0` = default 5000.
    pub db_breaker_cooldown_ms: u64,
    /// Whether a request may use the `allowed_hosts: ["*"]` wildcard for the `api` client.
    /// Default `false`: a `*` is ignored (matches nothing), so a request must name each host
    /// explicitly. `*` is dangerous because it removes the host allowlist and leaves only the
    /// private-IP filter — so it is honored only when this is `true` **and** `debug` is off
    /// (the SSRF-relaxed local mode never permits a wildcard).
    pub allow_wildcard_hosts: bool,
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

impl EngineConfig {
    /// Returns the timeout as a `Duration`.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }

    /// Resolves the concurrency bulkhead size: the configured value, or the auto default
    /// `pool_size × AUTO_CONCURRENCY_FACTOR` when left at `0`. Never returns `0` (a
    /// zero-permit semaphore would deadlock every request).
    #[must_use]
    pub const fn resolved_max_concurrent(&self, pool_size: usize) -> usize {
        if self.max_concurrent_executions > 0 {
            return self.max_concurrent_executions;
        }
        let auto = pool_size.saturating_mul(AUTO_CONCURRENCY_FACTOR);
        if auto > 0 { auto } else { 1 }
    }

    /// Resolves the partition-bucket count: the configured value, or
    /// `DEFAULT_PARTITION_BUCKETS` when left at `0`.
    #[must_use]
    pub const fn resolved_partition_buckets(&self) -> usize {
        if self.partition_buckets > 0 {
            self.partition_buckets
        } else {
            DEFAULT_PARTITION_BUCKETS
        }
    }

    /// Resolves the `db` circuit-breaker cool-down: the configured value, or
    /// `DEFAULT_BREAKER_COOLDOWN_MS` when left at `0`.
    #[must_use]
    pub const fn resolved_breaker_cooldown_ms(&self) -> u64 {
        if self.db_breaker_cooldown_ms > 0 {
            self.db_breaker_cooldown_ms
        } else {
            DEFAULT_BREAKER_COOLDOWN_MS
        }
    }

    /// Maximum HTTP request body size (derived from script + context limits + overhead).
    #[must_use]
    pub const fn max_body_size(&self) -> usize {
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
    pub fn resolve_limits(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
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
