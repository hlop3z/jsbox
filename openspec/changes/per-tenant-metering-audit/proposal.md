## Why

The box now derives a trusted tenant/user/plan per request and traces each execution, but it
still produces **no per-tenant record of what happened**: usage is not attributed (billing/quota
tuning are blind), and every rejection collapses into a single blind `record_rejection()` counter
with no tenant and no reason (no compliance trail). At untrusted multi-tenant scale this is the
last in-box gap — you cannot bill, cannot answer "who did what, was it allowed?", and cannot tune
plans. The facts are already computed (the response `meta` carries per-request usage; `fabricd`
already drains per-tenant egress metrics; the gates already know the reject reason) — they are
simply not emitted as attributed events. This change emits them, **observability-grade now**, with
a schema and sink seam designed so a **durable, billing-grade outbox can be added later without
re-plumbing**.

## What Changes

- Add a **unified, versioned event** emitted **once per request** (never sampled): a shared
  envelope `{ v, event_id, ts, tenant, user, plan, trace_id, type }` + a type-specific `body`,
  where `type ∈ {usage, audit}`. `event_id` is the idempotency/dedup key a future outbox needs.
- **Usage metering:** one `usage` event per **executed** request at the finish seam
  (`build_response`), carrying the billing/compliance dimensions already in `meta` (wall-clock,
  per-capability op counts incl. `fabricd` egress, request/response sizes, outcome) keyed by the
  trusted tenant + plan.
- **Audit log:** one `audit` event per request — `allowed` at the finish, or `denied` at whichever
  gate terminated it (anonymous / suspended / tenant-less / acting-scope / member-authz / quota /
  oversized / egress-session / shed) — each with tenant, user, and the **reason code**. The quota
  decision (plan/limit/usage) folds into the audit event. Replaces the blind aggregate
  `record_rejection()` with attributed, reasoned events (the aggregate counter is kept for metrics).
- Add a **non-blocking `Sink` port** with a lossy-now implementation: a bounded channel + writer
  task emitting **structured JSON to a dedicated stdout stream** (distinct target, collector-routed),
  **drop-on-full** with a `dropped-events` gauge as the backpressure signal. Emission never blocks
  or fails the request path (fail-open, like tracing).
- Thread the `TrustedIdentity` **values** into `build_response` (Change B put them only on the span).
- **Metrics stay unchanged** (no per-tenant labels — the cardinality invariant holds); tenant lives
  in events, not metric labels.
- **Out of scope (deferred, enabled-but-not-built):** the durable outbox itself (WAL/queue,
  near-exactly-once, reconciliation), the billing aggregation pipeline, and any OTLP-log transport
  for events. The schema + `Sink` port are designed so these drop in later without changing call sites.

## Capabilities

### New Capabilities
- `tenant-metering`: per-request, per-tenant usage events (the billing/quota-tuning record) — the
  event envelope, the usage `body` dimensions, non-blocking emission, and the durable-outbox seam.
- `tenant-audit`: per-request, per-tenant decision events (the compliance trail) — allowed vs
  denied-with-reason at every gate, keyed by tenant/user, sharing the same envelope + sink.

### Modified Capabilities
<!-- none — metrics/observability behavior is unchanged; these are new capabilities -->

## Impact

- **Code:** `runlet/src/` new `events.rs` (envelope + `Sink` port + bounded-channel `LogSink` +
  writer task), `handler.rs` (emit usage at `build_response`; emit audit at each gate + finish;
  thread `TrustedIdentity` into the finish path), `main.rs` (build the sink + writer task, wire into
  `AppState`, flush on shutdown), `config.rs` (events on/off, channel bound, stream target).
- **`runlet-core`:** unchanged — stays identity-agnostic; it already returns the usage facts in
  `Outcome`/metrics. `fabricd`: unchanged — already drains per-tenant egress metrics.
- **Config/deploy:** new `events` config; docs for the stdout event stream + collector routing.
- **No new dependencies** (serde + the existing `uuid` cover the envelope). Metrics surface + the
  cardinality invariant unchanged. Gate: fmt/clippy/test + a `test_simple.py` events section.
