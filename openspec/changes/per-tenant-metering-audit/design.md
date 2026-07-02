## Context

Change B added tracing + structured logs and made `TrustedIdentity` (tenant/user/plan) reach the
request span. But there is still no per-tenant **record**: usage is unattributed and rejections
collapse into a single blind `state.metrics.record_rejection()` counter (no tenant, no reason). The
facts already exist — the response `meta` carries per-request usage (`exec_time_us`, per-capability
op counts, sizes, outcome), `fabricd` drains per-session (= per-tenant) egress metrics back to the
box, and each of the ~11 gates in `run_execute` knows exactly why it rejected. They are simply not
emitted as attributed events.

Target: untrusted multi-tenant at scale (millions of tenants, horizontally-sharded replicas). The
cardinality invariant from B holds: **identity is an event dimension, never a metric label**. The
box is the single natural emit point — it alone has compute + egress + identity for a request.

User decisions (explore): observability-grade now (lossy-OK); **unified event envelope** (usage and
audit share one schema + a `type`); **structured stdout stream** now; **audit covers every request**
(allow + every deny-with-reason). The overriding constraint: design so a **durable, billing-grade
outbox drops in later without re-plumbing**.

Constraints: strict lint gauntlet; `runlet-core` stays HTTP/identity-agnostic; no blocking on the
hot path; no new crypto/heavy deps.

## Goals / Non-Goals

**Goals:**
- One unified, versioned event per request (usage when executed; audit always), attributed by tenant.
- A `Sink` port + a lossy `LogSink` (bounded channel → JSON to a dedicated stdout stream, drop-on-full).
- Schema completeness + a per-event `event_id` so a durable outbox is a later addition, not a rewrite.
- Zero blocking/failure on the request path; disabled emission is fully inert.
- Metrics unchanged; no per-tenant labels.

**Non-Goals:**
- No durable outbox / WAL / exactly-once / reconciliation (deferred; the seam is built for it).
- No billing aggregation pipeline; no OTLP-log transport for events.
- No `runlet-core` changes; no `fabricd` changes (it already drains per-tenant metrics).
- No new metric cardinality.

## Decisions

**D1 — A `Sink` port + versioned event schema is the load-bearing seam (Rent/Adopt/Build → Build).**
The emission mechanism is small and bespoke (a bounded channel + a JSON writer); there is nothing to
adopt. What matters is the *contract*: `trait Sink { fn record(&self, event: &Event); }` (non-blocking)
plus a stable `Event { v, event_id, ts, tenant, user, plan, trace_id, type, body }`. Today one impl,
`LogSink`, serializes to stdout; later an `OutboxSink` persists with dedup on `event_id` — the call
sites (finish + gates) never change. *Alternative:* emit ad-hoc JSON at each site (rejected — no
swap point for the outbox, schema drifts). *This is the whole point of the change.*

**D2 — Unified envelope, `type`-discriminated (user decision).**
Usage and audit share `{ v, event_id, ts, tenant, user, plan, trace_id, type }`; `body` is
type-specific. One port, one serializer, one future outbox table. `serde` tagged enum for `body`.

**D3 — `event_id` = a fresh per-event ULID/uuid; `trace_id` correlates.**
A request emits up to two events (usage + audit) that must be individually dedupable, so `event_id`
is unique per *event* (a v4 `uuid`, the crate is already a dep), while `trace_id` (shared, from B)
correlates them to the request/trace. The outbox dedups on `event_id`. *Alternative:* derive from
`trace_id:span_id:type` (rejected — ties dedup identity to tracing being enabled; a fresh id is
self-contained and works with tracing off).

**D4 — Emission is a bounded, drop-on-full async pipeline (fail-open).**
The hot path calls `sink.record` which does a non-blocking `try_send` into a bounded `mpsc`; a
dedicated writer task drains it and writes JSON lines to stdout. A full channel drops the event and
bumps a `dropped-events` gauge (a *bounded* metric — safe). Never blocks, never fails a request;
best-effort flush on shutdown. Mirrors the "emit-don't-aggregate, box is one replica" reasoning: the
box streams events; aggregation is downstream. *Alternative:* synchronous stdout write (rejected —
a slow/blocked pipe would stall the request path).

**D5 — Dedicated stream separation via a discriminator + tracing target.**
Events are JSON objects distinct from app logs. They carry a top-level discriminator (the envelope
itself is unmistakable) and are written so a collector can route them (e.g. a `runlet::events`
tracing target or a dedicated writer). Keeping them on stdout (not OTLP) means a collector outage
never loses them — same durability argument as B's logs.

**D6 — Events built + emitted in `runlet`; core stays agnostic.**
`runlet-core` already returns the usage facts in `Outcome`/metrics; identity lives in `runlet`. So
the `Event` is composed in `runlet` (a new `events.rs`), the `Sink` travels in `AppState`, and
`build_response` finally receives the `TrustedIdentity` *values* (B only put them on the span) to
attribute the usage + allowed-audit event. Deny-audit events are emitted at each gate, which already
has identity + reason in scope.

**D7 — Audit emit points: one helper, called at every terminal site.**
Metering has a single site (`build_response`). Audit fires at ~11 gates + the finish. A small
`audit_deny(sink, tenant, user, reason, detail)` / `audit_allow(...)` helper is called at each
`return *rejected` site and at the finish, replacing/augmenting the blind `record_rejection()`.

## Risks / Trade-offs

- **Lossy-now hides events under load** → acceptable for observability-grade, but the `dropped-events`
  gauge must be visible so operators see pressure; the outbox (later) closes this for billing. *Mitigation:*
  bounded channel sized generously; drop counter is a first-class metric; document the guarantee.
- **Schema drift breaks the future outbox** → *Mitigation:* the versioned envelope (`v`) + all
  billing/compliance dimensions captured now (usage: full `meta`; audit: reason + decision detail);
  a spec scenario pins the envelope fields.
- **Double-counting / missing events** (an executed request must emit exactly one usage + one audit;
  a rejected one exactly one audit, zero usage) → *Mitigation:* emit at the single finish seam and at
  mutually-exclusive gate returns; integration test asserts the counts per path.
- **PII/secret leakage** → *Mitigation:* events carry only the identifiers already used for isolation
  + decision metadata; no body/headers/creds (spec requirement + test).
- **Hot-path cost** → one `uuid` + one `try_send` + serialization on the writer thread (off the hot
  path). Negligible; measured against the existing per-request work.

## Migration Plan

Additive and opt-in (`events` config; default off / inert until enabled). Rollout: enable emission →
point the collector at the stdout event stream → (later) add the `OutboxSink` impl + durable store,
selectable by config, with **no change** to `handler.rs` call sites. Rollback = disable via config or
revert; no persisted state. Gate in Docker: fmt/clippy/test + a `test_simple.py` events section
asserting per-path event counts, envelope fields, and fail-open behavior.

## Open Questions

- **`event_id` type:** v4 `uuid` (already a dep) vs a ULID (time-sortable, nicer for outbox ordering).
  Lean uuid to avoid a dep; revisit if the outbox wants time-ordering. Settle at apply.
- **Stream separation mechanism:** a dedicated `tracing` target vs a separate writer/fd. Both work;
  pick the one that keeps the JSON cleanest for collector routing. Settle at apply.
- **Channel bound + overflow policy default:** a tuning value, not a design blocker.
