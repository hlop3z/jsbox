//! The per-target circuit breaker (Tier 3), re-exported from [`fabric_wire`].
//!
//! The breaker moved to the shared `fabric-wire` crate so the driver host (`fabric-backends`)
//! and the sandbox host both reference the same type — the binary builds one breaker and passes
//! it to the egress backends. This path keeps `crate::breaker::CircuitBreaker` stable for
//! consumers. See `docs/design/resilience.md`.

pub use fabric_wire::breaker::{BreakerConfig, CircuitBreaker};
