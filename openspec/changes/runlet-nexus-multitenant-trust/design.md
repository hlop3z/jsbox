## Context

`runlet` executes untrusted caller-supplied JavaScript. It currently has no tenant model:
a single shared `access_token` authenticates everyone, the Tier 5 fairness partition key is
caller-asserted (`X-Partition-Key`), and any caller may name any egress resource that
`fabricd` holds. The target deployment is **untrusted multi-tenant** traffic.

The deployment context is fixed and changes the problem shape: `runlet` runs as a backend
pool (`pool_jsbox`) behind the first-party **nexus** edge platform (`dufeutech/nexus` — Envoy
data plane + tenant-router + identity plane). nexus already performs on-demand TLS,
`jwt_authn`, strips client-supplied `x-*` headers (its RFC C3), and injects trusted identity
(`x-user-*`). `runlet` is never exposed directly to the internet. This lets `runlet` consume
a trusted identity rather than authenticating anyone itself.

Constraints: the strict lint gauntlet (no `unwrap`/`expect`/`panic`/`as`/bare-arith), the
string-in/string-out FFI contract for capabilities, and the existing tiered resilience and
`fabric-wire` box↔`fabricd` protocol. Build/test are Docker-only.

## Goals / Non-Goals

**Goals:**
- Make per-tenant isolation, fairness, egress scope, and quota key off a **trusted,
  edge-authorized tenant identity** that no caller can assert or forge.
- Enforce the tenant boundary where it matters: fairness + cache in `runlet`, egress
  credentials in `fabricd`.
- Keep `runlet` free of TLS/JWT/authN — consume trusted headers, fail closed if the trust
  precondition (network isolation) is not asserted.
- Reuse existing machinery: the `PartitionLimiter` (Tier 5), the `WireInit` handshake, and
  the nexus `plan → limit` quota shape.

**Non-Goals:**
- TLS termination, JWT/JWKS verification, or user authentication — the edge owns these.
- The mechanism by which a user selects/authorizes an *acting* workspace across multiple
  orgs (ZITADEL org-scoped tokens + grants). That is a nexus upstream requirement (N5);
  `runlet` treats `x-tenant-id` as opaque and already-authorized.
- Fine-grained role→resource authorization policy (v2).
- Tenant-scoped script registry — the registry is platform-provided first-party scripts
  only; tenants submit inline `script`, so there is nothing per-tenant to isolate.
- Full `ring` eviction (needs quinn 0.12+).

## Decisions

### D1 — Trust the edge; consume trusted headers (Adopt the platform, don't rebuild authN)
`runlet` derives identity from configurable trusted headers injected by nexus: `x-tenant-id`
(opaque authorized acting workspace), `x-user-id`, `x-user-roles`, `x-user-entitlements`,
`x-user-suspended`, `x-auth-anonymous`. **Alternatives:** (a) verify JWTs in `runlet` — duplicates
the edge, adds a JWKS/crypto surface, rejected; (b) per-tenant API keys in `runlet` — a second
credential store, rejected. Rationale: the edge already authenticates and strips client `x-*`;
identity is trustworthy by construction at the pool boundary.

### D2 — A boot guard is the safety net that replaces TLS
Because `runlet` blindly trusts `x-*`, the security model rests on **`runlet` being reachable
only through the edge** (k8s NetworkPolicy). `runlet` SHALL refuse to start in trusted-header
mode on a non-loopback bind unless the operator explicitly asserts isolation — mirroring the
existing `allow_unauthenticated` guard in `runlet/src/config.rs`. The existing `access_token`
is repurposed as the edge→`runlet` service credential (defense in depth with the NetworkPolicy).
**Alternative:** mTLS edge→pool — heavier (cert-manager), redundant with NetworkPolicy inside a
cluster; deferred.

### D3 — One tenant type: the acting workspace, opaque to `runlet`
Tenant = workspace (ZITADEL org; workspace model — solo users get a personal workspace).
`x-tenant-id` is the **universal key** for partition/fairness, bytecode-cache namespacing,
egress scope, and quota. `runlet` never branches on "user vs org" and never learns how the
acting workspace was chosen. **Alternative:** tenant = user (`sub`) — a dead-end for
collaboration; rejected in favor of the workspace model. **Alternative:** derive tenant from
the user's home org (`resourceowner`) — wrong for multi-org users; the acting org must come
from upstream (N5).

### D4 — Fairness + cache key flips from caller-asserted to trusted tenant (BREAKING)
The Tier 5 partition key and the bytecode-cache namespace SHALL be the trusted tenant id.
The caller-asserted `X-Partition-Key` header / `partition` body source is removed — it was a
noisy-neighbor evasion and cross-tenant cache-dedup/timing vector. Intra-workspace fairness is
**accepted** (one member can consume the workspace's shared bucket — their problem, like a
shared CI runner).

### D5 — Egress isolation lives in `fabricd`, authorized by a trusted tenant id in `WireInit`
The box forwards the trusted tenant id in `WireInit`; `fabricd` resolves logical resource
names **only within that tenant's binding set**. Credentials never cross workspaces, enforced
where credentials live. `runlet` optionally does a **coarse** member-capability gate off
`x-user-roles`/`x-user-entitlements` first (fast-fail, defense in depth). **Alternative:** do
all authz in `runlet` — but `runlet` holds no credentials and no resource table; rejected.

### D6 — Quota: copy the nexus `plan → limit` shape
Per-workspace usage is bounded by a data-driven `plan → limit` table with a **fail-closed
conservative default** for unknown plans and a structured over-limit outcome — mirroring nexus
`routing-rs/plan.rs` (`PlanLimits`/`DomainLimit`/`QuotaExceeded`, "at-or-above"). Rate-limiting
may ride Envoy per-`x-tenant-id`; `runlet` does accounting + the hard cap. **Alternative:**
build a novel quota engine — rejected; adopt the proven sibling pattern.

## Build-vs-Adopt Decisions

### Decision: Identity / authentication / TLS — Rent nexus edge platform

- **Status**: approved
- **Why**: Authentication, on-demand TLS, and trusted-header injection are edge/platform infrastructure the first-party nexus stack already provides; `runlet` must not re-implement them.
- **Considered**: verify JWTs in `runlet` (duplicates the edge, adds a JWKS/crypto surface); per-tenant API keys in `runlet` (a second credential store).
- **Isolation**: `runlet` consumes trusted `x-*` headers behind the `TrustedIdentity` extractor; the pool boundary + NetworkPolicy is the trust seam.

### Decision: Per-tenant quota / rate-limiting — Rent Envoy (throttle) + Extend nexus `plan.rs` (quota cap)

- **Status**: approved
- **Why**: Per-tenant request throttling is edge infrastructure (Envoy per-`x-tenant-id` rate-limit); the in-process quota *cap* is a threshold gate, best served by porting the proven, lint-clean first-party `plan → limit` pattern (`PlanLimits`/`DomainLimit`/`QuotaExceeded`, at-or-above, fail-closed). Concurrency fairness stays on the existing `PartitionLimiter`.
- **Considered**: `governor` (GCRA rate-limiter — wrong shape: a rate-limiter, not a threshold quota, and throttling is Rented to Envoy; the pick if in-process throttling is ever needed); build a bespoke engine (reinvents mature tooling).
- **Isolation**: a `quota` module owning a `PlanLimits`-style table + accounting, keyed on the trusted tenant id; throttling lives in the Envoy config, not in `runlet`.

### Decision: Member authorization — Build coarse entitlement gate now; Adopt `cedar-policy` for v2

- **Status**: approved
- **Why**: v1 authz is a set-membership check (does `x-user-roles`/`x-user-entitlements` permit the requested capability) — a policy engine's expressiveness would be unused weight against the project's minimal-dependency, strict-supply-chain discipline. When v2 fine-grained role→resource/ABAC policy lands, `cedar-policy` is the recorded adopt: it keeps authz **in-process, offline, and deterministic** (a pure function of request + policies + entities — no hot-path RPC, no stateful service), which are the four properties `/execute` is built around and which mirror the existing offline `sa-token` posture. Its PARC model maps directly onto data `runlet` already holds as trusted claims (principal = `x-user-id`/`x-tenant-id`, action = capability + operation, resource = the logical binding name, context = roles/entitlements/plan), and today's capability→entitlement map is a degenerate one-line Cedar policy — so v2 lands as **policy data, not new evaluation code**. Cedar also ships a schema validator and a formally-verified evaluator, which is the build-vs-adopt rationale for security-critical code.
- **Considered**: **OpenFGA / SpiceDB (Zanzibar ReBAC) — rejected for this layer**: solves relationship/graph authz (sharing, nested groups, org trees) that `runlet` does not have (its decision is set-membership over edge-computed claims), at the cost of a stateful service + its own datastore + a network `Check()` on every `/execute` and a fail-open/closed dependency in the hot path; if genuinely relationship-shaped authz ever appears it belongs in the nexus identity plane (a different repo), not the execution backend. `casbin-rs` (lighter embeddable RBAC/ABAC, still heavier than a contains-check for v1, weaker assurance story than Cedar for v2).
- **Isolation**: a config-driven capability→required-entitlement map behind a small `authz` gate function; swappable for a Cedar evaluator behind the same signature without touching call sites.

### Decision: Constant-time credential comparison — Adopt `subtle::ConstantTimeEq`

- **Status**: approved
- **Why**: `subtle` is already in the dependency tree (zero new supply-chain cost) and is a vetted, single-purpose primitive; adopting it replaces two duplicated hand-rolled `ct_eq` copies (`fabricd/src/auth.rs`, `runlet/src/handler.rs`) — the gate prefers a mature tool for security-critical code.
- **Considered**: `constant_time_eq` (fine, but a *new* dep when `subtle` is already present); keep the hand-rolled `ct_eq` (duplicated hand-written security code).
- **Isolation**: a single shared helper wrapping `ConstantTimeEq`, used by both the edge service-credential check and the `fabricd` static-token check.

## Risks / Trade-offs

- **Forged `x-tenant-id` if `runlet` is reachable off-edge** → the boot guard (D2) +
  NetworkPolicy + the edge service credential. All three must hold; the guard fails closed.
- **N5 not shipped in nexus** → until the identity plane emits the *authorized acting org*,
  `x-tenant-id` would carry the home org and multi-org users are mis-scoped. Mitigation: track
  N5 as a release gate for the multi-org case; single-workspace users are unaffected.
- **Intra-workspace starvation** → accepted, documented, not mitigated (shared bucket by design).
- **Header-name drift between edge and `runlet`** → header names are config, pinned in one place
  and asserted in an integration test against the edge contract.
- **Breaking removal of `X-Partition-Key`** → no external tenant depends on it (pre-GA);
  documented in the spec delta with migration = "partitioning is now automatic per tenant".

## Migration Plan

1. Ship `tenant-identity` ingress + boot guard (defaults preserve today's single-tenant,
   loopback behavior — trusted mode is opt-in).
2. Flip Tier 5 + bytecode cache to the trusted tenant id; remove `X-Partition-Key`.
3. Extend `WireInit` with the tenant id; make `fabricd` resolution tenant-scoped.
4. Add per-tenant quota + accounting.
5. Coarse member authz (fast-follow).
6. Deploy behind nexus with a NetworkPolicy; land nexus N5 for multi-org.

Rollback: trusted mode is opt-in; disabling it reverts `runlet` to the pre-change single-
principal behavior without redeploying the edge.

## Open Questions

- Final trusted header names (recommend `x-tenant-id`; configurable regardless).
- Whether coarse member authz (L2) ships in v1 or as the immediate fast-follow.
- Quota dimensions for v1: executions/sec, concurrent, or compute-time — and which are edge
  (Envoy) vs `runlet` (accounting + hard cap).
