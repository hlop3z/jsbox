//! The error-taxonomy primitives shared across the egress seam.
//!
//! [`ErrorOwner`] and [`Fault`] are the classification vocabulary every capability uses to
//! turn a typed driver error into a stable `code` + retry hint + responsible owner, above the
//! "stringify cliff". [`DynamicFault`] / [`dynamic_fault_json`] render that classification into
//! the `__jsbox` tagged-error JSON the engine reads back ([`crate::egress::EgressError`] builds
//! it via `to_tag_json`).
//!
//! The response-envelope side of the taxonomy (`ErrorSource`, `ErrorCategory`, `ErrorEnvelope`,
//! the throwing-capability `capability_fault_json`) stays in `runlet-core` — it is HTTP-front /
//! assembly concern, not part of the wire contract a sidecar needs.

use serde::Serialize;
use serde_json::Value;

/// *Who* should act on an error — orthogonal to the error category (the layer) and `retryable`
/// (the action). Routes alerts: don't page ops for a developer's bug.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ErrorOwner {
    /// The API client sent a bad request (fix the request).
    Caller,
    /// The script author's code/logic/usage (fix the script).
    Developer,
    /// Infrastructure / a downstream dependency (page ops).
    Operator,
}

impl ErrorOwner {
    /// Parses a lowercase tag string back into an owner. Defaults handled by the caller.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "caller" => Some(Self::Caller),
            "developer" => Some(Self::Developer),
            "operator" => Some(Self::Operator),
            _ => None,
        }
    }
}

/// A classified fault: a stable machine `code`, a retry hint, and a responsible `owner`.
///
/// Derived from a typed error *above the stringify cliff*. Capability-agnostic; the `code`
/// constants live with the capability that owns them.
#[derive(Debug, Clone, Copy)]
pub struct Fault {
    /// Stable `SCREAMING_SNAKE` code, safe for a client to switch on.
    pub code: &'static str,
    /// `true` ⇒ a retry may succeed (transient); `false` ⇒ deterministic.
    pub retryable: bool,
    /// Who should act on this fault.
    pub owner: ErrorOwner,
}

impl Fault {
    /// Builds a fault from a `code`, retry hint, and responsible owner.
    #[must_use]
    pub const fn new(code: &'static str, retryable: bool, owner: ErrorOwner) -> Self {
        Self {
            code,
            retryable,
            owner,
        }
    }
}

/// Serializable `__jsbox` tag built from dynamic (caller-owned) string fields.
///
/// For the [`crate::egress::Egress`] port. Borrows `&str`/`&Value` and is not feature-gated (the
/// egress seam is core). Serializes to `{ error, code, retryable, owner, source, details? }` — the
/// shape the engine classifies structurally.
#[derive(Debug, Serialize)]
pub struct DynamicFault<'a> {
    /// Raw cause (the human-readable message — surfaced gated, in `debug.raw`).
    pub error: &'a str,
    /// Stable machine code.
    pub code: &'a str,
    /// Retry hint.
    pub retryable: bool,
    /// Responsible owner (serialized lowercase: `caller`/`developer`/`operator`).
    pub owner: ErrorOwner,
    /// Originating capability source tag (lowercase: `db`/`mongo`/…).
    pub source: &'a str,
    /// Structured, safe machine context (e.g. `{sqlstate}`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<&'a Value>,
}

/// Last-resort JSON if a [`DynamicFault`] ever fails to serialize.
const FALLBACK_DYNAMIC_FAULT_JSON: &str = r#"{"error":"internal error","code":"INTERNAL","retryable":true,"owner":"operator","source":"engine"}"#;

/// Builds the `__jsbox` tag JSON for a [`crate::egress::Egress`] port failure.
///
/// A egress's `source`/`code`/`owner` are supplied dynamically (by the sidecar / adapter)
/// rather than from a static per-capability [`Fault`]. Produces the
/// `{ error, code, retryable, owner, source, details? }` shape the engine classifies.
#[must_use]
pub fn dynamic_fault_json(fault: &DynamicFault<'_>) -> String {
    serde_json::to_string(fault).unwrap_or_else(|_err| FALLBACK_DYNAMIC_FAULT_JSON.to_owned())
}
