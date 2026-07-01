//! Centralized structured-error vocabulary for the `/execute` response contract.
//!
//! One definition of the error shape, reused by every layer instead of each
//! hand-rolling its own (see `docs/99-errors.md`). Separation of concerns:
//!
//! - **classify** — each capability derives a [`Fault`] (stable `code` + `retryable` +
//!   `owner`) from its *typed* error, above the "stringify cliff". This module owns the
//!   carrier type, never the per-capability mapping (the `code` constants live with each
//!   capability — `db`/`mail`/`s3`/`http`).
//! - **transport** — [`capability_fault_json`] builds the FFI JSON a *throwing*
//!   capability returns on failure; the JS wrapper forwards it wholesale as its
//!   `__runlet` tag. [`api_inband_error_json`] is the non-throwing `api` twin (§13).
//! - **assemble** — [`ErrorEnvelope`] is the response object the handler serializes
//!   into `{ data, error, meta }`.
//!
//! Deliberately free of any HTTP/axum dependency: status-code policy belongs with the
//! handler, not the vocabulary.

use serde::Serialize;
use serde_json::Value;

// The error-taxonomy primitives + the `__runlet` wire envelope moved to `fabric-wire` (the shared
// egress contract). Re-export so `crate::errors::{ErrorOwner, Fault}` stays public (consumers +
// the in-engine capabilities) and `DynamicFault`/`dynamic_fault_json` stay crate-internal (the
// engine's egress seam).
pub(crate) use fabric_wire::{DynamicFault, dynamic_fault_json};
pub use fabric_wire::{ErrorOwner, Fault};

/// Coarse category a client branches on (the response `error.type`).
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ErrorCategory {
    /// Caller's fault — the submitted request is invalid.
    Request,
    /// Engine / `QuickJS` level (syntax, timeout, memory, internal).
    Runtime,
    /// Developer code threw an uncaught error.
    Script,
    /// A capability's external dependency failed.
    Capability,
}

/// Where an error originated — the `source` field of both the FFI transport and the
/// response envelope.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ErrorSource {
    /// Request validation, before the engine.
    Request,
    /// The execution engine itself.
    Engine,
    /// The user's `handler` function.
    Handler,
    /// The `db` capability.
    Db,
    /// The `mongo` capability.
    Mongo,
    /// The `mail` capability.
    Mail,
    /// The `s3` capability.
    S3,
    /// The `api` (HTTP) capability.
    Api,
    /// The `redis` capability.
    Redis,
    /// The `amq` (`RabbitMQ`) capability.
    Amq,
    /// The `auth` (OIDC/IAM) capability.
    Auth,
}

impl ErrorSource {
    /// Parses a lowercase tag string back into a source (engine reads the `__runlet`
    /// tag). `None` for an unknown value → the throw is treated as a script error.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "request" => Some(Self::Request),
            "engine" => Some(Self::Engine),
            "handler" => Some(Self::Handler),
            "db" => Some(Self::Db),
            "mongo" => Some(Self::Mongo),
            "mail" => Some(Self::Mail),
            "s3" => Some(Self::S3),
            "api" => Some(Self::Api),
            "redis" => Some(Self::Redis),
            "amq" => Some(Self::Amq),
            "auth" => Some(Self::Auth),
            _ => None,
        }
    }
}

/// FFI failure payload a *throwing* capability returns across the `QuickJS` boundary.
///
/// Serializes to `{ error, code, retryable, owner, source, details? }`. The JS wrapper
/// throws `new Error(error)` and tags it (`e.__runlet = res`) so the engine classifies
/// the throw structurally (docs/99-errors.md).
#[cfg(feature = "_throws")]
#[derive(Debug, Serialize)]
struct CapabilityFault {
    /// Raw driver message (the human-readable cause — surfaced gated, in `debug.raw`).
    error: String,
    /// Stable machine code.
    code: &'static str,
    /// Retry hint.
    retryable: bool,
    /// Responsible owner.
    owner: ErrorOwner,
    /// Originating capability.
    source: ErrorSource,
    /// Structured, safe machine context (e.g. `{sqlstate}` / `{http_status}`).
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<Value>,
}

/// In-band `api` transport-error payload (§13): HTTP never throws, so a transport
/// failure is returned as data the script can inspect, not an exception.
#[cfg(feature = "http")]
#[derive(Debug, Serialize)]
struct ApiInbandError {
    /// Always `0` — signals "no HTTP response" (transport failed before a status).
    status: u16,
    /// The structured fault the script can branch on.
    error: InbandFault,
}

/// The `error` object embedded in an [`ApiInbandError`].
#[cfg(feature = "http")]
#[derive(Debug, Serialize)]
struct InbandFault {
    /// Stable machine code.
    code: &'static str,
    /// Retry hint.
    retryable: bool,
    /// Responsible owner.
    owner: ErrorOwner,
    /// Always [`ErrorSource::Api`].
    source: ErrorSource,
    /// Human-safe cause (omitted when absent).
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

/// Last-resort JSON if a [`CapabilityFault`] ever fails to serialize.
#[cfg(feature = "_throws")]
const FALLBACK_FAULT_JSON: &str = r#"{"error":"internal error","code":"INTERNAL","retryable":true,"owner":"operator","source":"engine"}"#;

/// Last-resort JSON if an [`ApiInbandError`] ever fails to serialize.
#[cfg(feature = "http")]
const FALLBACK_API_JSON: &str = r#"{"status":0,"error":{"code":"HTTP_ERROR","retryable":true,"owner":"operator","source":"api"}}"#;

/// Builds the FFI failure JSON a *throwing* capability returns (`db`/`mail`/`s3`).
///
/// The single place the `{ error, code, retryable, owner, source, details? }` shape is
/// produced, so every capability stays DRY: it supplies its [`ErrorSource`], a classified
/// [`Fault`], the raw message, and any structured `details`.
#[cfg(feature = "_throws")]
pub(crate) fn capability_fault_json(
    source: ErrorSource,
    fault: Fault,
    message: &str,
    details: Option<Value>,
) -> String {
    let payload = CapabilityFault {
        error: message.to_owned(),
        code: fault.code,
        retryable: fault.retryable,
        owner: fault.owner,
        source,
        details,
    };
    serde_json::to_string(&payload).unwrap_or_else(|_err| FALLBACK_FAULT_JSON.to_owned())
}

/// Builds the in-band `api` transport-error JSON: `{ status: 0, error: { … } }`.
///
/// The non-throwing twin of [`capability_fault_json`] — `api` returns this as data so
/// the script can inspect `res.error` without a `try/catch` (§13).
#[cfg(feature = "http")]
pub(crate) fn api_inband_error_json(fault: Fault, message: &str) -> String {
    let payload = ApiInbandError {
        status: 0,
        error: InbandFault {
            code: fault.code,
            retryable: fault.retryable,
            owner: fault.owner,
            source: ErrorSource::Api,
            message: Some(message.to_owned()),
        },
    };
    serde_json::to_string(&payload).unwrap_or_else(|_err| FALLBACK_API_JSON.to_owned())
}

/// Internal-only debug context; gated by `error_debug`. Never surface to end users.
#[derive(Debug, Serialize)]
pub struct ErrorDebug {
    /// JS stack trace, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
    /// Raw driver/internal cause (may contain secrets/PII — why it's gated).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
}

impl ErrorDebug {
    /// `true` if there is nothing to serialize (so the whole `debug` object is omitted).
    pub(crate) const fn is_empty(&self) -> bool {
        self.stack.is_none() && self.raw.is_none()
    }
}

/// Structured `error.*` object the handler serializes into `{ data, error, meta }`.
///
/// Built only for *system-generated* errors; a developer's `return json(null, x)`
/// payload passes through verbatim and never becomes one of these (D1).
#[derive(Debug, Serialize)]
pub struct ErrorEnvelope {
    /// Coarse category for client branching.
    #[serde(rename = "type")]
    category: ErrorCategory,
    /// Origin of the error.
    source: ErrorSource,
    /// Stable machine code (owned: capability codes round-trip through the JS tag).
    code: String,
    /// Human-safe message — generic per code for capability/internal errors so it never
    /// leaks raw driver text (omitted when absent).
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    /// Retry hint for meshes/clients.
    retryable: bool,
    /// Who should act on the error (alert routing).
    owner: ErrorOwner,
    /// Structured, ungated machine context (e.g. `{sqlstate}` / `{http_status}`).
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<Value>,
    /// Internal-only context; present only when `error_debug` is on.
    #[serde(skip_serializing_if = "Option::is_none")]
    debug: Option<ErrorDebug>,
}

impl ErrorEnvelope {
    /// Creates an envelope with the always-present fields.
    #[must_use]
    pub const fn new(
        category: ErrorCategory,
        source: ErrorSource,
        code: String,
        retryable: bool,
        owner: ErrorOwner,
    ) -> Self {
        Self {
            category,
            source,
            code,
            message: None,
            retryable,
            owner,
            details: None,
            debug: None,
        }
    }

    /// Attaches a human-safe message.
    #[must_use]
    pub fn with_message(mut self, message: String) -> Self {
        self.message = Some(message);
        self
    }

    /// Attaches structured, ungated machine context.
    #[must_use]
    pub fn with_details(mut self, details: Option<Value>) -> Self {
        self.details = details;
        self
    }

    /// Attaches internal-only debug context, dropping it entirely if empty.
    #[must_use]
    pub fn with_debug(mut self, debug: ErrorDebug) -> Self {
        self.debug = if debug.is_empty() { None } else { Some(debug) };
        self
    }
}
