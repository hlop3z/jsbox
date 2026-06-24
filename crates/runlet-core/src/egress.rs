//! The [`Egress`] I/O port, re-exported from [`fabric_wire`].
//!
//! The trait and its tagged-error result now live in the shared `fabric-wire` crate so the
//! driver host (`fabric-backends`, eventually `fabricd`) can implement them without linking the
//! sandbox. The engine seam (`engine::inject_egress`) and the public surface (`crate::Egress`)
//! continue to reach them through this path. See `docs/design/resource-egress.md`.

pub use fabric_wire::egress::{Egress, EgressError};
