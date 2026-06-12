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

/// Top-level configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub(crate) struct Config {
    /// Local-dev switch. When `true`, the SSRF private-IP block is relaxed so
    /// localhost / LAN targets (e.g. `MinIO`) work for `s3` and `api`. Never enable in
    /// production — it removes the guard against internal/local targets.
    pub(crate) debug: bool,
    /// Include `error.debug` (stack traces) in responses. Default `true` because
    /// `/execute` runs as an internal service; set `false` at an exposed edge. Kept
    /// separate from `debug` (which only relaxes the SSRF guard) so the two don't entangle.
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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            debug: false,
            error_debug: true,
            server: ServerConfig::default(),
            engine: EngineConfig::default(),
            scripts_dir: None,
        }
    }
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
    /// Bulkhead: max concurrent executions in flight. `0` = auto
    /// (`pool_size × AUTO_CONCURRENCY_FACTOR`). Excess load fast-fails `429 OVERLOADED`
    /// rather than exhausting blocking threads / DB connections under a slow downstream
    /// (see `docs/design/resilience.md`). Tune to the downstream connection budget.
    pub(crate) max_concurrent_executions: usize,
    /// Operator ceiling (ms) for the `db` `statement_timeout`. `0` = no ceiling. A
    /// per-request `config.db.statement_timeout_ms` is clamped to this, and a request
    /// value of `0` ("unlimited") becomes this ceiling — so jsbox never issues an
    /// unbounded `SET`. The robust, pooler-proof ceiling is still a server-side role
    /// default; this is defense in depth (see `docs/design/resilience.md`).
    pub(crate) max_statement_timeout_ms: u64,
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
            memory_limit: 32 * 1024 * 1024, // 32mb
            max_stack_size: 512 * 1024,     // 512kb
            timeout_ms: 4000,               // 4s balanced default
            pool_size: 0,                   // Auto
            max_script_size: 1024 * 1024,   // 1mb
            max_context_size: 0,            // 0 = auto: memory_limit / CONTEXT_LIMIT_DIVISOR
            max_ops: 1500,                  // safe cap for API workloads
            max_concurrent_executions: 0,   // 0 = auto: pool_size * AUTO_CONCURRENCY_FACTOR
            max_statement_timeout_ms: 0,    // 0 = no operator ceiling (opt-in)
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
