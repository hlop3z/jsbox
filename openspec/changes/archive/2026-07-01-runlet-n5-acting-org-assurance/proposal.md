## Why

`runlet` keys every per-tenant boundary — Tier 5 fairness, the bytecode-cache namespace, egress
scope, and quota — off the trusted `x-tenant-id`, treating it as the caller's **authorized acting
org**. But `runlet` holds no signal distinguishing that from the user's **home org**, and by design
(D3) it cannot derive the difference itself. A nexus edge that has not shipped N5 — or that drifts
out of contract — would inject the home org, and `runlet` would **silently mis-scope** a multi-org
user: serving them another workspace's fairness bucket, cache namespace, egress bindings, and quota
with no alarm. This is the multi-org release gate for the completed `runlet-nexus-multitenant-trust`
change. There are no real users yet, so we make the correct posture the **default**, not a
backward-compatible opt-in.

## What Changes

- **BREAKING** (pre-users, no external impact): in trusted-header mode, a per-request acting-org
  assurance becomes **mandatory and fail-closed**. The nexus edge SHALL inject a trusted
  `x-tenant-scope: acting` header on every authorized-acting-org request; `runlet` rejects any
  tenant-scoped `/execute` whose scope is not `acting` with `403 ACTING_SCOPE_REQUIRED`, before any
  session or execution.
- The assurance gate slots into `resolve_identity` alongside the existing anonymous / suspended /
  tenant-less hard-rejects — one more trusted-header read, same fail-closed altitude.
- The trusted-header **name** is configurable (`trusted.headers.scope`, default `x-tenant-scope`)
  like every other trusted header; enforcement itself is intrinsic to trusted mode — **no enable
  flag, no `accept_home_org_scoping` escape hatch, no boot-forced choice.** Those were compat
  scaffolding an unreleased product does not need.
- `runlet` keeps treating `x-tenant-id` as **opaque** (D3): it checks a scope label, never
  interprets org relationships. Non-trusted / loopback (local-dev) mode is entirely unaffected.
- The N5 contract handoff is made concrete: `docs/design/nexus-upstream-requirements.md` gains the
  exact clause the nexus team implements against (edge SHALL emit `x-tenant-scope: acting`).

Honest scope: this is a **contract/version tripwire, not cryptographic proof.** The assurance
header rides the same trusted-edge boundary as `x-tenant-id` (only as strong as the NetworkPolicy),
so it defends against the *accidental* hazard (an edge without N5) by turning a silent mis-scope
into a loud rejection — not against a compromised edge, which the trust model already owns.
Signed-JWT verification in the box was considered and **rejected** (re-litigates D1 — `runlet` does
no JWT/JWKS/crypto; that is rented to the edge).

## Capabilities

### New Capabilities
<!-- None. This refines an existing capability's requirements. -->

### Modified Capabilities
- `tenant-identity`: add the mandatory acting-org assurance requirement — trusted mode SHALL reject
  a tenant-scoped request lacking the `x-tenant-scope: acting` assurance (fail-closed,
  `403 ACTING_SCOPE_REQUIRED`), and the configurable `scope` trusted-header name.

## Impact

- **Code**: `crates/runlet/src/identity.rs` (extract `scope` into `TrustedIdentity`),
  `crates/runlet/src/config.rs` (`scope` name in `TrustedHeaders`), `crates/runlet/src/handler.rs`
  (`resolve_identity` gate + `ACTING_SCOPE_REQUIRED` code in the error taxonomy).
- **Contract / cross-repo**: `docs/design/nexus-upstream-requirements.md` (N5 gains the concrete
  edge clause); `docs/design/multitenant-trust.md` (note the now-enforced assurance).
- **Deployment**: bring-up ordering — the edge must emit `x-tenant-scope: acting` **before** the box
  enforces it (producer-before-consumer); documented as a runbook note. No migration (no live
  traffic).
- **Dependencies**: none added (no new crates; the lint gauntlet and Docker-only build apply).
