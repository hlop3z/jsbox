//! Configuration loaded from an optional `config.json` file.
//!
//! All fields have sensible defaults. If the file is missing,
//! the server starts with defaults.
//!
//! Size fields accept human-readable strings: `"8mb"`, `"256kb"`, `"1gb"`,
//! or plain numbers in bytes: `8388608`.

use std::error::Error;
use std::fmt;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::time::Duration;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer};

/// Top-level configuration.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Config {
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

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            memory_limit: 8 * 1024 * 1024,
            max_stack_size: 256 * 1024,
            timeout_ms: 100,
            pool_size: 0,
            max_script_size: 1024 * 1024,
            max_context_size: 5 * 1024 * 1024,
            max_ops: 50,
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

/// Parses a byte size from a human-readable string (integer math, no floats).
///
/// Accepted: `"8mb"`, `"256kb"`, `"1gb"`, `"8 MB"`, `"4096"`.
fn parse_byte_size(input: &str) -> Result<usize, String> {
    let lower = input.trim().to_lowercase();

    // Find where the unit starts.
    let boundary = lower
        .find(|ch: char| ch.is_alphabetic())
        .unwrap_or(lower.len());
    let (num_str, unit_str) = lower.split_at(boundary);

    let number: usize = num_str.trim().parse().map_err(|_err| {
        if num_str.contains('.') {
            format!("decimal values are not supported for byte sizes: \"{input}\" — use whole numbers (e.g. \"1536kb\" instead of \"1.5mb\")")
        } else {
            format!("invalid number in byte size: \"{input}\"")
        }
    })?;

    let multiplier: usize = match unit_str.trim() {
        "" | "b" => 1,
        "kb" | "k" => 1024,
        "mb" | "m" => 1024 * 1024,
        "gb" | "g" => 1024 * 1024 * 1024,
        other => return Err(format!("unknown size unit '{other}' in: {input}")),
    };

    number
        .checked_mul(multiplier)
        .ok_or_else(|| format!("byte size overflow: {input}"))
}

/// Serde visitor that accepts either a number or a string like `"8mb"`.
struct ByteSizeVisitor;

#[expect(
    clippy::missing_trait_methods,
    reason = "serde Visitor has 20+ default methods — only the relevant ones are overridden"
)]
impl Visitor<'_> for ByteSizeVisitor {
    type Value = usize;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a byte size as a number (8388608) or string (\"8mb\", \"256kb\")")
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
        usize::try_from(v).map_err(de::Error::custom)
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
        usize::try_from(v).map_err(de::Error::custom)
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        parse_byte_size(v).map_err(de::Error::custom)
    }
}

/// Deserializes a byte size field — accepts `"8mb"`, `"256kb"`, or plain numbers.
fn deserialize_byte_size<'de, D: Deserializer<'de>>(deserializer: D) -> Result<usize, D::Error> {
    deserializer.deserialize_any(ByteSizeVisitor)
}
