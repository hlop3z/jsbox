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
- **Acting-org assurance (enforced box-side):** on every authorized-acting-org request the edge
  SHALL also emit a trusted `x-tenant-scope: acting` header. `runlet` now **enforces** this
  fail-closed: in trusted-header mode a tenant-scoped `/execute` whose `x-tenant-scope` is absent or
  not equal to `acting` is rejected `403 ACTING_SCOPE_REQUIRED` before any egress session or
  execution. This turns a silent multi-org mis-scope (an edge that has not shipped N5, or has
  drifted) into a loud rejection. It is a **contract tripwire, not cryptographic proof** — the header
  rides the same trusted-edge boundary as `x-tenant-id`, so it is only as strong as the NetworkPolicy
  (see D3). The header name is configurable box-side (`trusted.headers.scope`, default
  `x-tenant-scope`); pin any rename in both repos.
- **Bring-up ordering (producer before consumer):** the edge must emit `x-tenant-scope: acting`
  **before** a box that enforces it is rolled out, or all trusted-mode traffic 403s. There is no live
  traffic today (pre-users), so this is a fresh-deploy ordering note, not a migration.
- **Scope of impact:** single-workspace users are unaffected (their acting org is their only org, so
  the edge always emits `acting`); the requirement gates the **multi-org** case.
- **Status:** box side **enforced**; nexus side open — track as a release gate for multi-org
  tenancy. Until the edge emits `x-tenant-scope: acting`, trusted-mode traffic is rejected, so bring
  the edge up first.

Related: `docs/design/multitenant-trust.md` (decision D3; the acting-org gate).

## N6 — the edge must propagate a W3C `traceparent` so edge→box→`fabricd` is one trace

`runlet` emits an OpenTelemetry span per `/execute` (tenant/user/plan as span **attributes**, never
metric labels) and exports it OTLP to a collector. For a request's trace to span the whole path,
the nexus edge must start the trace and inject a standard **W3C `traceparent`** (and optionally
`tracestate`) header, which the box reads and **continues** (parent-based sampling honors the
edge's sample decision).

- **Why:** without a propagated `traceparent`, each box starts its own orphan root span — traces
  still work but cannot be tied back to the edge request or correlated across hops. With it, one
  trace id threads edge → box → (later) `fabricd`.
- **Contract:** the edge SHALL inject `traceparent` per the W3C Trace Context spec on requests it
  forwards to the box; it makes the head sampling decision. The box does no tail sampling (that is
  the collector's job).
- **Graceful degradation (no hard dependency):** if the edge does not emit `traceparent`, the box
  starts its own root span and applies its configured sample ratio — so this is an *enhancement*,
  not a release gate. `meta.trace_id` is the propagated id when present, else the box-rooted id.
- **Bring-up ordering:** stand up the collector and set `telemetry.otlp_endpoint` on the box, then
  enable edge propagation — the box tolerates any order (fail-open, D6).
- **Status:** box side implemented (continues `traceparent`, fail-open); nexus side open — track as
  an observability enhancement, not a gate.

Related: `docs/design/multitenant-trust.md`; the `observability` spec (distributed tracing).
