//! `amq` capability wrapper injection for the `QuickJS` sandbox.
//!
//! The `amq` global's JS surface (`RabbitMQ`/NATS messaging) routes every call through the
//! `io.call("amq", …)` egress port; the driver-backed dispatch + metrics live in the
//! `fabric-backends` crate's `amq` backend. This module keeps only the wrapper injection —
//! see `docs/design/resource-egress.md`.

use rquickjs::{Ctx, Value as JsValue};

/// JS wrapper — loaded from `src/js/amq.js` at compile time.
const AMQ_WRAPPER: &str = include_str!("js/amq.js");

/// Injects the `amq` global (the `amq.js` wrapper, routing through `io.call`).
///
/// No connection happens here — the wired [`fabric_wire::Egress`] port (e.g. an in-process
/// `BackendSet`, or a sidecar) serves the calls. The presence of a `amq` resource on the
/// invocation gates this wrapper (the engine never sees credentials).
///
/// # Errors
///
/// Returns an error if evaluating the wrapper fails.
pub(crate) fn inject_wrapper(qctx: &Ctx<'_>) -> Result<(), rquickjs::Error> {
    let wrapper: JsValue<'_> = qctx.eval(AMQ_WRAPPER)?;
    drop(wrapper);
    Ok(())
}
