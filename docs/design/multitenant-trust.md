# Multitenant trust: `runlet` as a nexus edge backend

`/execute` runs untrusted, caller-supplied JavaScript. This design makes `runlet` safe for
**untrusted multi-tenant** traffic by deploying it as a backend pool (`pool_jsbox`) behind the
first-party **nexus** edge platform (Envoy + tenant-router + identity plane), and keying every
per-tenant boundary — fairness, cache, egress scope, quota — off a **trusted, edge-authorized
tenant identity** that no caller can assert or forge.

See `openspec/changes/runlet-nexus-multitenant-trust/` for the proposal, specs, and decision record.

## The trust flip

`runlet` performs **no** TLS termination, JWT verification, or user authentication — the edge does
(`jwt_authn` + on-demand TLS), and it strips any client-supplied `x-*` header before injecting a
trusted identity. `runlet` consumes that identity from operator-configured trusted headers:

| Purpose            | Default header         | Field                     |
| ------------------ | ---------------------- | ------------------------- |
| acting workspace   | `x-tenant-id`          | `tenant` (the universal key) |
| user (audit)       | `x-user-id`            | `user`                    |
| member roles       | `x-user-roles`         | `roles` (comma-separated) |
| member entitlements| `x-user-entitlements`  | `entitlements` (comma-sep)|
| suspended flag     | `x-user-suspended`     | `suspended` → hard reject |
| anonymous flag     | `x-auth-anonymous`     | `anonymous` → hard reject |
| plan (quota tier)  | `x-tenant-plan`        | `plan`                    |

Every name is configurable (`trusted.headers.*`) so a drift between the edge contract and the box is
pinned in one place. Trusted mode is **opt-in** (`trusted.enabled`); the default preserves the
pre-change single-principal, loopback behavior.

## The trust invariant (and its safety net)

Because `runlet` trusts `x-*` blindly once enabled, the entire model rests on one invariant:

> **`runlet` is reachable only through the edge.**

Enforced out of band by a k8s **NetworkPolicy** (`deploy/networkpolicy-pool-jsbox.yaml`) restricting
ingress to `pool_jsbox` to the edge namespace/pod-selector. The in-process **boot guard**
(`config.rs::check_trusted_isolation`) is the fail-closed backstop: trusted mode refuses to start on
a non-loopback bind unless the operator asserts `trusted.assert_network_isolation: true` — mirroring
the existing `allow_unauthenticated` guard, because there is no TLS/JWT check to fall back on once
headers are trusted. The existing `access_token` is repurposed as the **edge→box service
credential** (defense in depth with the NetworkPolicy).

Three independent controls must all hold: the NetworkPolicy, the boot guard, and the service
credential. The guard fails closed.

## Tenant is the universal key

The trusted tenant id (the acting workspace — a ZITADEL org; solo users get a personal workspace)
is the single key for:

- **Tier 5 fairness** (`PartitionLimiter`): the partition key is the trusted tenant id. The
  caller-asserted `X-Partition-Key` header / `partition` body source is **removed** in trusted mode
  (it was a noisy-neighbor evasion + cross-tenant cache-dedup/timing vector). `meta.partition` still
  echoes the resolved value. Intra-workspace fairness is accepted (a shared bucket, like a shared CI
  runner).
- **Bytecode-cache namespace**: identical source from different tenants never shares a cache entry
  (no cross-tenant dedup / compile-timing leak).
- **Egress scope**: the box forwards the trusted tenant id in `WireInit`; `fabricd` resolves logical
  resource names **only within that tenant's binding set** (`tenant` on each `TenantResourceBinding`;
  a cross-tenant name resolves as `NotFound` so existence never leaks). Credentials never cross
  workspaces, enforced where credentials live.
- **Quota**: per-tenant plan-gated usage (below).

`runlet` treats `x-tenant-id` as opaque and already-authorized; it never branches on "user vs org"
and never learns how the acting workspace was chosen (that is nexus upstream requirement **N5** —
see `nexus-upstream-requirements.md`).

## Coarse member authorization

A config-driven `capability → required entitlement` map (`trusted.capability_entitlements`) gates
which capability a member may invoke, off the trusted `x-user-roles` / `x-user-entitlements`. This is
deliberately coarse ("may this member use `db` at all"), not fine-grained role→resource policy — that
is a v2 concern (revisit Cedar). A capability kind absent from the map is ungated. Runs before the
capability does.

## Per-tenant, plan-gated quota

`runlet` does per-tenant usage **accounting + a hard cap**; per-tenant request throttling rides the
edge (Envoy per-`x-tenant-id` rate-limit). The quota engine (`quota.rs`) mirrors the nexus
`routing-rs/plan.rs` shape — a data-driven `plan → limit` table, "at-or-above", **fail-closed**:

- A tenant's plan (from `x-tenant-plan`) selects a `PlanLimit` (today: `max_concurrent` in-flight
  executions per tenant).
- An **unknown/unconfigured plan** resolves to the most restrictive configured limit.
- An **empty** `plans` map (while `quota.enabled`) denies every request — a misconfiguration never
  grants unbounded usage.
- Over-limit returns a structured `429 QUOTA_EXCEEDED` carrying the plan, limit, and current usage.

## Request pipeline (trusted mode)

```
edge service credential  →  trusted identity (reject anonymous/suspended/tenant-less)
  →  partition = trusted tenant (caller-asserted ignored)  →  member-capability authz
  →  per-tenant quota admit  →  fabricd session (tenant-scoped)  →  Tier 5 + bulkhead  →  execute
```

## Out of scope

- Tenant-scoped script registry — the registry is platform-provided first-party scripts only;
  tenants submit inline `script`, so there is nothing per-tenant to isolate.
- Fine-grained role→resource policy (v2 / Cedar).
- Full `ring` eviction (needs quinn 0.12+).
