## Why

`/execute` runs untrusted, caller-supplied JavaScript but has no tenant model: authentication is a single shared `access_token` (one principal for everyone), the fairness partition key is caller-asserted (`X-Partition-Key`, trivially spoofed), and any caller can name any egress resource. That is unsafe for untrusted multi-tenant traffic — the stated target deployment. This change makes `runlet` a trusted-boundary backend behind the first-party **nexus** edge platform (Envoy + tenant-router + identity plane), so per-tenant isolation, fairness, egress scope, and quota all key off a **trusted, edge-authorized tenant identity** instead of anything the caller can assert.

## What Changes

- **Trust posture flip.** `runlet` is deployed as a nexus backend pool (`pool_jsbox`), reachable **only** through the edge. It performs **no** TLS termination, JWT verification, or user authentication — the edge does (`jwt_authn` + on-demand TLS). `runlet` consumes trusted identity headers the edge injects (and the edge strips any client-supplied `x-*`).
- **Trusted-identity request contract.** `/execute` derives tenant + user identity from configurable trusted headers: `x-tenant-id` (opaque, already-authorized acting workspace), `x-user-id` (audit), `x-user-roles`/`x-user-entitlements` (member authz), `x-user-suspended` + `x-auth-anonymous`. Requests that are anonymous or from a suspended principal are **rejected**.
- **Boot guard (replaces TLS as the safety net).** `runlet` refuses to start trusting `x-*` headers on a non-loopback bind unless the operator explicitly asserts network isolation — mirroring the existing `allow_unauthenticated` guard. The existing `access_token` is repurposed as the edge→`runlet` service credential (belt-and-suspenders with a NetworkPolicy).
- **Tenant becomes the universal key.** The trusted tenant id — not the caller-asserted `X-Partition-Key` — sources per-tenant concurrency fairness (Tier 5), bytecode-cache namespacing, egress scope, and quota. **BREAKING**: the `X-Partition-Key` caller-asserted path is removed.
- **Tenant-scoped egress isolation.** The box forwards the trusted tenant id to `fabricd` in `WireInit`; `fabricd` resolves logical resource names **only within that tenant's binding set**, so credentials and resources never cross workspaces.
- **Per-workspace, plan-gated quota + accounting.** Compute/usage is bounded per tenant with a fail-closed `plan → limit` table and a structured `quota_exceeded` outcome.
- **Coarse member authz (fast-follow).** A per-request capability gate off `x-user-roles`/`x-user-entitlements` (e.g. "may this member use `db` at all").

## Capabilities

### New Capabilities
- `tenant-identity`: the trusted-header identity contract (tenant/user/roles derivation, configurable header names), anonymous/suspended rejection, the trusted-headers boot guard, and coarse role/entitlement member authz.
- `tenant-egress`: tenant-scoped egress resolution — the box forwards a trusted tenant id in the box↔`fabricd` wire protocol, and `fabricd` resolves resource names only within the requesting tenant's bindings (cross-tenant resolution forbidden).
- `tenant-quota`: per-workspace, plan-gated usage limits with a fail-closed conservative default for unknown plans and a structured over-limit result.

### Modified Capabilities
- `execution`: `/execute` now requires a valid trusted identity to run tenant-scoped work; the bytecode cache is namespaced by the trusted tenant id (no cross-tenant dedup / compile-timing leak).
- `resilience`: the Tier 5 per-partition fairness key MUST be the trusted tenant id; the caller-asserted partition-key source is removed.

## Impact

- **Code:** `runlet/src/{config.rs,handler.rs,main.rs}` (trusted-header ingress, boot guard, tenant sourcing), `runlet-core` partition/bytecode-cache keying, `fabric-wire` `WireInit` (carry tenant id), `fabricd` resource resolution (tenant-scoped), a new quota surface.
- **APIs:** `/execute` request contract gains trusted-header inputs and drops the caller-asserted `X-Partition-Key`; new `403`/`401` rejection and `quota_exceeded` outcomes.
- **Deployment:** requires the nexus edge in front + a k8s NetworkPolicy restricting `pool_jsbox` to the edge. Not exposed directly.
- **Cross-repo dependency (nexus, not jsbox work):** upstream requirement **N5** — the identity plane must emit the *authorized acting org* (via ZITADEL org-scoped token + grants), distinct from the user's home-org `resourceowner`, so `x-tenant-id` is a trustworthy acting-workspace id.
- **Out of scope:** tenant-scoped script registry (the `scripts_dir` registry is platform-provided first-party scripts only; tenants submit inline `script`), fine-grained role→resource policy (v2), full `ring` eviction (needs quinn 0.12+).
