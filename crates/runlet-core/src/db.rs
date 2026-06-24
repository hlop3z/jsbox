//! `db` capability wrapper injection for the `QuickJS` sandbox.
//!
//! The `db` global's JS surface (`PostgreSQL`/`CockroachDB`) routes every call through the
//! `io.call("db", …)` egress port; the driver-backed dispatch + metrics live in the
//! `fabric-backends` crate's `db` backend. This module keeps only the wrapper injection —
//! see `docs/design/resource-egress.md`.

use rquickjs::{Ctx, Value as JsValue};

/// JS wrapper — loaded from `src/js/db.js` at compile time.
const DB_WRAPPER: &str = include_str!("js/db.js");

/// Injects the `db` global (the `db.js` wrapper, routing through `io.call`).
///
/// No connection happens here — the wired [`fabric_wire::Egress`] port (e.g. an in-process
/// `BackendSet`, or a sidecar) serves the calls. The presence of a `db` resource on the
/// invocation gates this wrapper (the engine never sees credentials).
///
/// # Errors
///
/// Returns an error if evaluating the wrapper fails.
pub(crate) fn inject_wrapper(qctx: &Ctx<'_>) -> Result<(), rquickjs::Error> {
    let wrapper: JsValue<'_> = qctx.eval(DB_WRAPPER)?;
    drop(wrapper);
    Ok(())
}
