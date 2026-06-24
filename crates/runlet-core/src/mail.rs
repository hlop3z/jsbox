//! `mail` capability wrapper injection for the `QuickJS` sandbox.
//!
//! The `mail` global's JS surface (SMTP mail) routes every call through the
//! `io.call("mail", …)` egress port; the driver-backed dispatch + metrics live in the
//! `fabric-backends` crate's `mail` backend. This module keeps only the wrapper injection —
//! see `docs/design/resource-egress.md`.

use rquickjs::{Ctx, Value as JsValue};

/// JS wrapper — loaded from `src/js/mail.js` at compile time.
const MAIL_WRAPPER: &str = include_str!("js/mail.js");

/// Injects the `mail` global (the `mail.js` wrapper, routing through `io.call`).
///
/// No connection happens here — the wired [`fabric_wire::Egress`] port (e.g. an in-process
/// `BackendSet`, or a sidecar) serves the calls. The presence of a `mail` resource on the
/// invocation gates this wrapper (the engine never sees credentials).
///
/// # Errors
///
/// Returns an error if evaluating the wrapper fails.
pub(crate) fn inject_wrapper(qctx: &Ctx<'_>) -> Result<(), rquickjs::Error> {
    let wrapper: JsValue<'_> = qctx.eval(MAIL_WRAPPER)?;
    drop(wrapper);
    Ok(())
}
