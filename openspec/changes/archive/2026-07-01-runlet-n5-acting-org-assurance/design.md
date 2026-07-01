## Context

The completed `runlet-nexus-multitenant-trust` change keys every per-tenant boundary (Tier 5
fairness, bytecode-cache namespace, egress scope, quota) off the trusted `x-tenant-id`, which D3 of
that change defines as an **opaque, already-authorized acting-workspace id**. Correctness rests
entirely on the edge injecting the *authorized acting org* — nexus upstream requirement **N5**. Today
`runlet` has no signal that N5 actually held for a request: an edge that emits the user's *home org*
(pre-N5, or drifted) mis-scopes a multi-org user **silently**. `runlet` cannot detect this itself —
by D3 it never interprets org relationships.

This change gives `runlet` its half of the N5 contract: require the edge to *assert* acting-org
authorization per request, and fail closed without it. There are no real users, so the correct
posture is the default — not a backward-compatible opt-in. Constraints: the strict lint gauntlet,
Docker-only build, and composition with the existing trusted-header model (`identity.rs`), boot
guard (`config.rs::check_trusted_isolation`), and `resolve_identity` reject chain (`handler.rs`).

## Goals / Non-Goals

**Goals:**
- Turn silent multi-org mis-scoping into a loud, fail-closed rejection in trusted mode.
- Make the N5 dependency an explicit, enforced contract clause the nexus team implements against.
- Preserve D3 opacity — `runlet` checks a scope *label*, never derives org relationships.
- Add zero new dependencies and no new hot-path cost beyond one trusted-header read.

**Non-Goals:**
- Cryptographic assurance / verifying the acting-org decision in the box (that is the edge's job; N5).
- Any backward-compatibility path, opt-in flag, or degraded "home-org scoping" mode.
- The nexus-side implementation of N5 itself (cross-repo; this change only defines + enforces the
  box side of the contract).
- Generalizing to a full identity-contract-version handshake now (see D4 / Open Questions).

## Decisions

### D1 — Assurance is mandatory and fail-closed in trusted mode (no opt-in)
Trusted-header mode *means* "behind the nexus edge for multi-tenant traffic," so that edge MUST do
N5. Enforcement is therefore intrinsic to trusted mode, not a knob: a tenant-scoped `/execute`
lacking the acting-org assertion is rejected with `403 ACTING_SCOPE_REQUIRED` before any session or
execution. **Alternatives:** (a) opt-in flag defaulting off — leaves the realistic "operator forgot"
hole and only helps those who already know about N5; rejected as compat scaffolding an unreleased
product does not need. (b) boot-forced "pick one of `edge_emits_acting_org` /
`accept_home_org_scoping`" — needed only to tolerate a non-N5 edge, which we choose not to support.
A single-workspace deployment is unaffected for free: home == acting, so its edge always emits
`acting`.

### D2 — A per-request trusted header (`x-tenant-scope: acting`), enum-valued, name-configurable
The edge injects `x-tenant-scope` per request; `runlet` requires the value `acting` for
tenant-scoped work. The gate lives in `resolve_identity` (after the tenant-present check, alongside
anonymous/suspended/tenant-less); extraction adds a `scope` field to `TrustedIdentity` and a `scope`
name to `TrustedHeaders` (default `x-tenant-scope`), configurable like every other trusted-header
name. **Alternatives:** (a) boot-time operator assertion only — catches deploy-time misconfig but
not *runtime* contract drift; a per-request signal catches both. (b) boolean `x-acting-org-authorized:
true` — an enum (`acting`/future `home`/`impersonation`/`service`) is self-documenting and
future-proof. (c) echo the acting-org id in the header so `runlet` asserts it `== x-tenant-id` — both
values come from the same edge, so it adds no real assurance; not worth the coupling.

### D3 — This is a tripwire, not a proof (reject in-box JWT verification)
The assurance header rides the same trusted-edge boundary as `x-tenant-id`, so it is only as strong
as the NetworkPolicy; a compromised edge could forge it. It defends against the *accidental* hazard
(an edge without N5) by converting a silent mis-scope into a loud rejection — not against a
compromised edge, which the trust model already owns out of band. **Alternative:** the edge mints a
signed short-lived identity token with `acting_org`/`authorized` claims that `runlet` verifies —
genuine unforgeable assurance, but it re-litigates D1 of the multitenant-trust change (`runlet` does
**no** JWT/JWKS/crypto; that is rented to the edge) and puts a JWKS-refresh crypto surface in the hot
path of every `/execute`. Rejected: wrong altitude for a trusted first-party edge.

### D4 — Ship the N5-specific gate (RESOLVED); record contract-versioning as the tracked next step
**Decision: N5-specific.** The enforced header is `x-tenant-scope: acting`. A broader
`x-identity-contract: v1` handshake — one tripwire that trips on *any* edge/box identity-contract
drift, with N5 assurance as a documented guarantee of `v1` — is more future-proof and nearly free to
add now, painful to retrofit once an edge emits a fixed header set. It is **deferred, not rejected**:
same seams (`identity.rs` + `resolve_identity`), same fail-closed posture, broader assertion —
tracked as the next step. Rationale for shipping N5-specific first: it is the smallest correct step
that closes the multi-org gate, and the header name is config + the gate is one function, so
evolving to a contract-version scheme later is cheap. The nexus-upstream clause therefore asks the
edge to emit the scope header (not a contract-version header) for now.

## Risks / Trade-offs

- **Producer-before-consumer bring-up** → the edge must emit `x-tenant-scope: acting` *before* the
  box enforces it, or all trusted-mode traffic 403s. Mitigation: documented runbook ordering; no live
  traffic to migrate (pre-users).
- **Trusted-header limitation (not cryptographic)** → a compromised edge forges the assertion.
  Mitigation: accepted and documented — the NetworkPolicy + boot guard + service credential remain the
  primary trust controls; this is defense-in-depth against the accidental case (D3).
- **N5-specific header may be superseded by a contract-version scheme** → a future `x-identity-contract`
  would subsume `x-tenant-scope`. Mitigation: the header name is config and the gate is one function;
  cheap to evolve. Decide D4 before freezing the nexus-side clause.
- **Contract drift with nexus** → the box and edge must agree on the header name/value. Mitigation:
  the name is pinned in one config place (`trusted.headers.scope`) and asserted in an integration test
  against the edge contract, mirroring the existing header-drift mitigation.

## Open Questions

- ~~**D4 fork**: N5-specific vs `x-identity-contract: v1`.~~ **Resolved: N5-specific** (see D4).
  Contract-versioning is tracked as the next step, not part of this change.
- Should any legitimate tenant-scoped `/execute` flow ever carry a non-`acting` scope (e.g.
  `impersonation` for support tooling)? If so, the accepted-value set becomes config rather than the
  literal `acting`. Assume no for v1.
