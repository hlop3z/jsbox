//! The [`Egress`] port — the consumer-supplied seam for out-of-process I/O.
//!
//! A consumer wires one `Egress` implementation and the engine exposes a single
//! `io.call(name, action, payload)` global. The core stays domain-agnostic — it forwards
//! `(name, action, payload_json)` and surfaces the string result, or maps a [`EgressError`]
//! into the same `__runlet` tagged-error JSON a built-in capability throws, so the engine's
//! error-classification path consumes it unchanged.
//!
//! This is the seam that lets driver-backed capabilities (`db`/`mongo`/`mail`/`redis`/`amq`/
//! `auth`) move out of the sandbox process and behind a sidecar — see
//! `docs/design/resource-egress.md`. It lives in `fabric-wire` (not `runlet-core`) so the
//! driver host (`fabric-backends`, eventually `fabricd`) can implement it without linking the
//! sandbox.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::errors::{self, DynamicFault, ErrorOwner};

/// Consumer-supplied I/O egress.
///
/// The implementation maps a logical `name` (e.g. `"orders-db"`) to a concrete backend from
/// operator config the sandbox never sees, performs the I/O, and returns the result as a JSON
/// string (`Ok`) or a [`EgressError`] (`Err`). `Send + Sync` because the pooled runtime is
/// shared across threads (the `parallel` feature).
pub trait Egress: Send + Sync {
    /// Performs one egress call.
    ///
    /// `name` is the logical egress, `action` the operation (e.g. `"query"`), and
    /// `payload_json` the script's JSON-encoded arguments (untrusted). Returns the JSON result
    /// string on success.
    ///
    /// # Errors
    ///
    /// Returns a [`EgressError`] when the backend call fails; the engine renders it into the
    /// `__runlet` tagged error the JS wrapper throws (surfaced to the script as a thrown
    /// capability error).
    fn call(&self, name: &str, action: &str, payload_json: &str) -> Result<String, EgressError>;
}

/// A failed [`Egress::call`], carrying the fields of the `__runlet` error tag.
///
/// `source` should be a known capability tag (`"db"`, `"mongo"`, …) so the engine classifies
/// the throw as a capability error; an unrecognized source degrades to a script error.
///
/// `Serialize`/`Deserialize` so it round-trips a sidecar (`fabricd`) call result over the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EgressError {
    /// Stable machine code (e.g. `"DB_TIMEOUT"`).
    pub code: String,
    /// Human-safe cause (surfaced gated, in `debug.raw`).
    pub message: String,
    /// Originating capability/source tag (`"db"`, `"mail"`, …).
    pub source: String,
    /// Structured, ungated machine context (e.g. `{"sqlstate":"40001"}`). Boxed to keep the
    /// error small (`clippy::result_large_err`) since it rides in a `Result` across the trait.
    pub details: Option<Box<Value>>,
    /// Retry hint.
    pub retryable: bool,
    /// Responsible owner; defaults to [`ErrorOwner::Operator`].
    pub owner: ErrorOwner,
}

impl EgressError {
    /// Builds an error from a source/code/message, defaulting `retryable` to `false`, `owner`
    /// to [`ErrorOwner::Operator`], and `details` to none.
    #[must_use]
    pub fn new<S, C, M>(source: S, code: C, message: M) -> Self
    where
        S: Into<String>,
        C: Into<String>,
        M: Into<String>,
    {
        Self {
            code: code.into(),
            message: message.into(),
            source: source.into(),
            details: None,
            retryable: false,
            owner: ErrorOwner::Operator,
        }
    }

    /// Marks the error retryable (builder-style).
    #[must_use]
    pub const fn retryable(mut self) -> Self {
        self.retryable = true;
        self
    }

    /// Sets the responsible owner (builder-style).
    #[must_use]
    pub const fn owner(mut self, owner: ErrorOwner) -> Self {
        self.owner = owner;
        self
    }

    /// Renders this error as the `__runlet` tagged-error JSON the JS wrapper throws.
    #[must_use]
    pub fn to_tag_json(&self) -> String {
        errors::dynamic_fault_json(&DynamicFault {
            error: &self.message,
            code: &self.code,
            retryable: self.retryable,
            owner: self.owner,
            source: &self.source,
            details: self.details.as_deref(),
        })
    }
}
