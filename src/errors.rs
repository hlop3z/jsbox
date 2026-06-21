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
//!   `__jsbox` tag. [`api_inband_error_json`] is the non-throwing `api` twin (§13).
//! - **assemble** — [`ErrorEnvelope`] is the response object the handler serializes
//!   into `{ data, error, meta }`.
//!
//! Deliberately free of any HTTP/axum dependency: status-code policy belongs with the
//! handler, not the vocabulary.

use serde::Serialize;
use serde_json::Value;

/// Coarse category a client branches on (the response `error.type`).
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ErrorCategory {
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
pub(crate) enum ErrorSource {
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
    /// Parses a lowercase tag string back into a source (engine reads the `__jsbox`
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

/// *Who* should act on an error — orthogonal to `type` (the layer) and `retryable`
/// (the action). Routes alerts: don't page ops for a developer's bug.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ErrorOwner {
    /// The API client sent a bad request (fix the request).
    Caller,
    /// The script author's code/logic/usage (fix the script).
    Developer,
    /// Infrastructure / a downstream dependency (page ops).
    Operator,
}

impl ErrorOwner {
    /// Parses a lowercase tag string back into an owner. Defaults handled by the caller.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "caller" => Some(Self::Caller),
            "developer" => Some(Self::Developer),
            "operator" => Some(Self::Operator),
            _ => None,
        }
    }
}

/// A classified fault: a stable machine `code`, a retry hint, and the responsible
/// `owner` — derived from a typed error *above the stringify cliff*. Capability-agnostic;
/// the `code` constants live with the capability that owns them.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Fault {
    /// Stable `SCREAMING_SNAKE` code, safe for a client to switch on.
    pub(crate) code: &'static str,
    /// `true` ⇒ a retry may succeed (transient); `false` ⇒ deterministic.
    pub(crate) retryable: bool,
    /// Who should act on this fault.
    pub(crate) owner: ErrorOwner,
}

impl Fault {
    /// Builds a fault from a `code`, retry hint, and responsible owner.
    pub(crate) const fn new(code: &'static str, retryable: bool, owner: ErrorOwner) -> Self {
        Self {
            code,
            retryable,
            owner,
        }
    }
}

/// FFI failure payload a *throwing* capability returns across the `QuickJS` boundary.
///
/// Serializes to `{ error, code, retryable, owner, source, details? }`. The JS wrapper
/// throws `new Error(error)` and tags it (`e.__jsbox = res`) so the engine classifies
/// the throw structurally (docs/99-errors.md).
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
#[derive(Debug, Serialize)]
struct ApiInbandError {
    /// Always `0` — signals "no HTTP response" (transport failed before a status).
    status: u16,
    /// The structured fault the script can branch on.
    error: InbandFault,
}

/// The `error` object embedded in an [`ApiInbandError`].
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
const FALLBACK_FAULT_JSON: &str = r#"{"error":"internal error","code":"INTERNAL","retryable":true,"owner":"operator","source":"engine"}"#;

/// Last-resort JSON if an [`ApiInbandError`] ever fails to serialize.
const FALLBACK_API_JSON: &str = r#"{"status":0,"error":{"code":"HTTP_ERROR","retryable":true,"owner":"operator","source":"api"}}"#;

/// Builds the FFI failure JSON a *throwing* capability returns (`db`/`mail`/`s3`).
///
/// The single place the `{ error, code, retryable, owner, source, details? }` shape is
/// produced, so every capability stays DRY: it supplies its [`ErrorSource`], a classified
/// [`Fault`], the raw message, and any structured `details`.
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
pub(crate) struct ErrorDebug {
    /// JS stack trace, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stack: Option<String>,
    /// Raw driver/internal cause (may contain secrets/PII — why it's gated).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) raw: Option<String>,
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
pub(crate) struct ErrorEnvelope {
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
    pub(crate) const fn new(
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
    pub(crate) fn with_message(mut self, message: String) -> Self {
        self.message = Some(message);
        self
    }

    /// Attaches structured, ungated machine context.
    #[must_use]
    pub(crate) fn with_details(mut self, details: Option<Value>) -> Self {
        self.details = details;
        self
    }

    /// Attaches internal-only debug context, dropping it entirely if empty.
    #[must_use]
    pub(crate) fn with_debug(mut self, debug: ErrorDebug) -> Self {
        self.debug = if debug.is_empty() { None } else { Some(debug) };
        self
    }
}
