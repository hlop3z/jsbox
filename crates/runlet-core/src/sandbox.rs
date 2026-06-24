//! Shared utilities for sandboxed JS modules (HTTP, DB).
//!
//! Provides generic metric collection, error JSON building,
//! and input validation used by both `http.rs` and `db.rs`.

// The metric-collection apparatus is used only by the I/O capabilities; a deterministic-only
// build (no capability features) compiles without it.
#[cfg(feature = "_io")]
use std::sync::{Arc, Mutex};

// `Serialize` is only needed by `check_op_limit`, which the per-op-limited native capabilities
// use — everything with a native FFI fn except `db` (now op-limited by the engine's `__resource`
// seam). That set is `http` plus the throwing capabilities (`_throws`).
#[cfg(any(feature = "http", feature = "_throws"))]
use serde::Serialize;

/// Generic metrics collector — shared between HTTP and DB modules.
#[cfg(feature = "_io")]
pub type Collector<T> = Arc<Mutex<Vec<T>>>;

/// Creates a new empty metrics collector.
#[cfg(feature = "_io")]
pub(crate) fn new_collector<T>() -> Collector<T> {
    Arc::new(Mutex::new(Vec::new()))
}

/// Pushes a metric into the collector.
#[cfg(feature = "_io")]
pub(crate) fn record<T>(collector: &Collector<T>, metric: T) {
    if let Ok(mut vec) = collector.lock() {
        vec.push(metric);
    }
}

/// Extracts all collected metrics, returning an empty vec if unavailable.
#[cfg(feature = "_io")]
pub(crate) fn drain<T: Clone>(collector: Option<&Collector<T>>) -> Vec<T> {
    collector
        .and_then(|coll| coll.lock().ok().map(|guard| guard.clone()))
        .unwrap_or_default()
}

/// Builds a JSON error string: `{"error": "message"}`.
///
/// Used by the always-on `$`/Decimal global (`decimal.rs`), whose errors are
/// script-level usage errors. Capability errors go through `errors::` instead.
pub(crate) fn error_json(message: &str) -> String {
    let escaped = serde_json::to_string(message).unwrap_or_else(|_err| "\"internal error\"".into());
    format!("{{\"error\":{escaped}}}")
}

/// Validates that input sizes are within configured limits.
///
/// # Errors
///
/// Returns `(code, message)` if a limit is exceeded — the stable `request`-category
/// code (`SCRIPT_TOO_LARGE` / `CONTEXT_TOO_LARGE`) plus a human-safe message.
pub fn validate_input_sizes(
    script_bytes: usize,
    context_bytes: usize,
    max_script: usize,
    max_context: usize,
) -> Result<(), (&'static str, String)> {
    if script_bytes > max_script {
        return Err((
            "SCRIPT_TOO_LARGE",
            format!("script too large: {script_bytes} bytes (max {max_script})"),
        ));
    }
    if context_bytes > max_context {
        return Err((
            "CONTEXT_TOO_LARGE",
            format!("context too large: {context_bytes} bytes (max {max_context})"),
        ));
    }
    Ok(())
}

/// The number of operations recorded so far (used by a backend with a sub-cap, e.g. `mail`'s
/// `max_sends`, to enforce it against its own metrics — the generic `__resource` seam already
/// enforces the global `max_ops`).
#[cfg(feature = "mail")]
pub(crate) fn op_count<T>(collector: &Collector<T>) -> usize {
    collector.lock().map_or(0, |vec| vec.len())
}

/// Checks if the operation count exceeds the per-execution limit.
#[cfg(any(feature = "http", feature = "_throws"))]
pub(crate) fn check_op_limit<T: Serialize>(
    collector: &Collector<T>,
    max_ops: usize,
) -> Result<(), String> {
    if let Ok(vec) = collector.lock()
        && vec.len() >= max_ops
    {
        return Err(format!(
            "too many operations: limit is {max_ops} per execution"
        ));
    }
    Ok(())
}
