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
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use crate::bytesize::deserialize_byte_size;

/// Top-level configuration.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Config {
    /// Local-dev switch. When `true`, the SSRF private-IP block is relaxed so
    /// localhost / LAN targets (e.g. `MinIO`) work for `s3` and `api`. Never enable in
    /// production — it removes the guard against internal/local targets.
    pub(crate) debug: bool,
    /// Server configuration.
    pub(crate) server: ServerConfig,
    /// JS engine sandbox limits.
    pub(crate) engine: EngineConfig,
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
    /// Maximum context payload size (e.g. `"5mb"`, default 5 MB).
    #[serde(deserialize_with = "deserialize_byte_size")]
    pub(crate) max_context_size: usize,
    /// Maximum HTTP/DB operations per execution (default 50).
    pub(crate) max_ops: usize,
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
            memory_limit: 32 * 1024 * 1024,      // 32mb
            max_stack_size: 512 * 1024,          // 512kb
            timeout_ms: 4000,                    // 4s balanced default
            pool_size: 0,                        // Auto
            max_script_size: 1024 * 1024,        // 1mb
            max_context_size: 10 * 1024 * 1024,  // 10mb
            max_ops: 1500,                       // safe cap for API workloads
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

    /// Maximum HTTP request body size (derived from script + context limits + overhead).
    pub(crate) const fn max_body_size(&self) -> usize {
        self.max_script_size
            .saturating_add(self.max_context_size)
            .saturating_add(64 * 1024)
    }
}

impl Config {
    /// Loads config from a file path. Returns defaults if the file doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be read or parsed.
    pub(crate) fn load(path: &Path) -> Result<Self, Box<dyn Error + Send + Sync>> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = fs::read_to_string(path)?;
        let config: Self = serde_json::from_str(&contents)?;
        Ok(config)
    }
}
