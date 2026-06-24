//! `redis` capability wrapper injection for the `QuickJS` sandbox.
//!
//! The `redis` global's JS surface (`Redis` key/value) routes every call through the
//! `io.call("redis", …)` egress port; the driver-backed dispatch + metrics live in the
//! `fabric-backends` crate's `kv` backend. This module keeps only the wrapper injection —
//! see `docs/design/resource-egress.md`.

use rquickjs::{Ctx, Value as JsValue};

/// JS wrapper — loaded from `src/js/redis.js` at compile time.
const KV_WRAPPER: &str = include_str!("js/redis.js");

/// Injects the `redis` global (the `redis.js` wrapper, routing through `io.call`).
///
/// No connection happens here — the wired [`fabric_wire::Egress`] port (e.g. an in-process
/// `BackendSet`, or a sidecar) serves the calls. The presence of a `redis` resource on the
/// invocation gates this wrapper (the engine never sees credentials).
///
/// # Errors
///
/// Returns an error if evaluating the wrapper fails.
pub(crate) fn inject_wrapper(qctx: &Ctx<'_>) -> Result<(), rquickjs::Error> {
    let wrapper: JsValue<'_> = qctx.eval(KV_WRAPPER)?;
    drop(wrapper);
    Ok(())
}
