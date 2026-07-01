# nexus upstream requirements (cross-repo dependencies)

Requirements this project (`jsbox`/`runlet`) depends on the first-party **nexus** edge platform
(`dufeutech/nexus`) to satisfy. Tracked here as a dependency; the canonical home is the nexus repo's
own `nexus-upstream-requirements.md` — mirror any change there.

> **Action:** record **N5** below in the nexus repo. It is a release gate for the multi-org case of
> the `runlet-nexus-multitenant-trust` change.

## N5 — the identity plane must emit the *authorized acting org*, not the home org

`runlet` treats the `x-tenant-id` header as an opaque, **already-authorized acting-workspace id**
and keys all per-tenant isolation (fairness, cache, egress scope, quota) off it. For this to be
correct, the nexus identity plane must inject the tenant id as the **org the user is acting as for
this request** — selected/authorized via a ZITADEL org-scoped token + grants — **not** the user's
home org (`resourceowner`).

- **Why:** a multi-org user acting in workspace B must be scoped to B's fairness bucket, cache
  namespace, egress bindings, and quota. If the edge emits the home org A instead, the user is
  mis-scoped — reaching A's resources and quota while acting in B, or being denied B's.
- **Contract:** `x-tenant-id` = the authorized acting org for this request; `x-user-id` = the
  user (audit); the edge strips any client-supplied `x-*` before injecting these.
- **Scope of impact:** single-workspace users are unaffected (their acting org is their only org);
  the requirement gates the **multi-org** case.
- **Status:** open — track as a release gate for multi-org tenancy. Until shipped, deploy `runlet`
  trusted mode only where users have a single workspace, or accept the home-org scoping limitation.

Related: `docs/design/multitenant-trust.md` (decision D3).
