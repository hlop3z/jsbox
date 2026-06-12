//! Shared utilities for sandboxed JS modules (HTTP, DB).
//!
//! Provides generic metric collection, error JSON building,
//! and input validation used by both `http.rs` and `db.rs`.

use std::sync::{Arc, Mutex};

use serde::Serialize;

/// Generic metrics collector — shared between HTTP and DB modules.
pub(crate) type Collector<T> = Arc<Mutex<Vec<T>>>;

/// Creates a new empty metrics collector.
pub(crate) fn new_collector<T>() -> Collector<T> {
    Arc::new(Mutex::new(Vec::new()))
}

/// Pushes a metric into the collector.
pub(crate) fn record<T>(collector: &Collector<T>, metric: T) {
    if let Ok(mut vec) = collector.lock() {
        vec.push(metric);
    }
}

/// Extracts all collected metrics, returning an empty vec if unavailable.
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
pub(crate) fn validate_input_sizes(
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

/// Checks if the operation count exceeds the per-execution limit.
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
