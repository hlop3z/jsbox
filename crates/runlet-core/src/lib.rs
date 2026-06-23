//! `runlet-core`: a reusable, sandboxed JavaScript logic host powered by `QuickJS`.
//!
//! This crate is the hardened execution core extracted from jsbox: pooled `QuickJS`
//! runtimes, the sync-JS→async-I/O bridge, wall-clock/memory/stack sandboxing, the
//! capability-binding pattern, the opaque-secret model, the `{data,error}` envelope, the
//! error taxonomy, and the 5-tier resilience model. It knows nothing about HTTP or any
//! consumer's data model.
//!
//! Consumers (the `runlet` HTTP front, or a non-HTTP scheduler) drive it through the
//! engine entry points re-exported here. The module surface is currently fully public
//! during the workspace extraction; a curated [`LogicHost`]-style facade narrows it once
//! the callable port lands.

#[cfg(feature = "amq")]
pub mod amq;
#[cfg(feature = "auth")]
pub mod auth;
pub mod breaker;
pub mod bytesize;
pub mod config;
#[cfg(feature = "db")]
pub mod db;
pub mod decimal;
pub mod engine;
pub mod errors;
pub mod host;
#[cfg(feature = "http")]
pub mod http;
#[cfg(feature = "redis")]
pub mod kv;
#[cfg(feature = "mail")]
pub mod mail;
pub mod metrics;
pub mod modules;
#[cfg(feature = "mongo")]
pub mod mongo;
pub mod partition;
pub mod pool;
pub mod registry;
#[cfg(feature = "s3")]
pub mod s3;
pub mod sandbox;
// Only the script-controlled capabilities (`api`/`s3`) need the SSRF guard.
#[cfg(any(feature = "http", feature = "s3"))]
pub mod ssrf;
pub mod sys;

// ── Curated public port ──────────────────────────────────────────────────────
// The blessed entry point; the module surface above stays public during the
// extraction but consumers should prefer these.
pub use crate::config::EngineConfig;
pub use crate::engine::{EngineError, ExecOutcome, Profile, ReadHook};
pub use crate::host::{
    CapabilitySet, CodeRef, ExecMetrics, HostSettings, Invocation, LogicHost, Outcome,
};
