//! Shared sandbox utilities: input-size validation, the script-error JSON helper, and the
//! metric-collection apparatus.
//!
//! The generic [`Collector`] + its helpers moved to `fabric-wire` (shared with the driver
//! backends); they are re-exported here under `crate::sandbox` for the in-engine capabilities
//! (`http`/`s3`) and the engine's metric drain, so those call sites stay unchanged.

// The metric apparatus is used only by the in-engine capabilities (`http`/`s3`); a build without
// them (incl. a deterministic-only core) links none of it.
#[cfg(any(feature = "http", feature = "s3"))]
pub(crate) use fabric_wire::metrics::{Collector, check_op_limit, drain, new_collector, record};

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
