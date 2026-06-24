//! `auth` capability wrapper injection for the `QuickJS` sandbox.
//!
//! The `auth` global's JS surface (OIDC/IAM identity) routes every call through the
//! `io.call("auth", …)` egress port; the driver-backed dispatch + metrics live in the
//! `fabric-backends` crate's `auth` backend. This module keeps only the wrapper injection —
//! see `docs/design/resource-egress.md`.

use rquickjs::{Ctx, Value as JsValue};

/// JS wrapper — loaded from `src/js/auth.js` at compile time.
const AUTH_WRAPPER: &str = include_str!("js/auth.js");

/// Injects the `auth` global (the `auth.js` wrapper, routing through `io.call`).
///
/// No connection happens here — the wired [`fabric_wire::Egress`] port (e.g. an in-process
/// `BackendSet`, or a sidecar) serves the calls. The presence of a `auth` resource on the
/// invocation gates this wrapper (the engine never sees credentials).
///
/// # Errors
///
/// Returns an error if evaluating the wrapper fails.
pub(crate) fn inject_wrapper(qctx: &Ctx<'_>) -> Result<(), rquickjs::Error> {
    let wrapper: JsValue<'_> = qctx.eval(AUTH_WRAPPER)?;
    drop(wrapper);
    Ok(())
}
