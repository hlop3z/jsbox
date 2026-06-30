//! `fabric-wire`: the shared egress-port contract for runlet.
//!
//! A driver-free, QuickJS-free leaf crate holding everything both sides of the egress seam
//! need in common:
//!
//! - [`Egress`] / [`EgressError`] — the callable I/O port and its tagged-error result.
//! - [`ErrorOwner`] / [`Fault`] / [`DynamicFault`] / [`dynamic_fault_json`] — the error-taxonomy
//!   primitives and the `__jsbox` wire envelope.
//! - [`CircuitBreaker`] / [`BreakerConfig`] — the per-target resilience breaker.
//! - [`Collector`] and friends — the per-execution metric buffer.
//!
//! The sandbox (`runlet-core`) depends on this to define the engine seam and re-export the
//! taxonomy; the driver host (`fabric-backends`, eventually `fabricd`) depends on this to
//! implement the backends and the [`Egress`] port — without linking the sandbox. See
//! `docs/design/resource-egress.md`.

pub mod breaker;
pub mod egress;
pub mod errors;
pub mod metrics;
pub mod quic;
pub mod wire;

pub use crate::breaker::{BreakerConfig, CircuitBreaker};
pub use crate::egress::{Egress, EgressError};
pub use crate::errors::{DynamicFault, ErrorOwner, Fault, dynamic_fault_json};
pub use crate::metrics::{Collector, check_op_limit, drain, new_collector, op_count, record};
pub use crate::wire::{
    AmqMetric, AuthMetric, BackendMetrics, DbMetric, MailMetric, MeteredEgress, MongoMetric,
    RedisMetric, WireCall, WireInit, WireRequest, WireResponse,
};
