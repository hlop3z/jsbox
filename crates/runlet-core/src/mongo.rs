//! `mongo` capability wrapper injection for the `QuickJS` sandbox.
//!
//! The `mongo` global's JS surface (`MongoDB` document-database) routes every call through the
//! `io.call("mongo", …)` egress port; the driver-backed dispatch + metrics live in the
//! `fabric-backends` crate's `mongo` backend. This module keeps only the wrapper injection —
//! see `docs/design/resource-egress.md`.

use rquickjs::{Ctx, Value as JsValue};

/// JS wrapper — loaded from `src/js/mongo.js` at compile time.
const MONGO_WRAPPER: &str = include_str!("js/mongo.js");

/// Injects the `mongo` global (the `mongo.js` wrapper, routing through `io.call`).
///
/// No connection happens here — the wired [`fabric_wire::Egress`] port (e.g. an in-process
/// `BackendSet`, or a sidecar) serves the calls. The presence of a `mongo` resource on the
/// invocation gates this wrapper (the engine never sees credentials).
///
/// # Errors
///
/// Returns an error if evaluating the wrapper fails.
pub(crate) fn inject_wrapper(qctx: &Ctx<'_>) -> Result<(), rquickjs::Error> {
    let wrapper: JsValue<'_> = qctx.eval(MONGO_WRAPPER)?;
    drop(wrapper);
    Ok(())
}
