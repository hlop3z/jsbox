## Why

The box is deployed for untrusted multi-tenant traffic at scale (millions of users), but its
observability has only one of the three pillars: aggregate Prometheus metrics. Logs are
plain-text `fmt` to stdout with no structure, and there is **no distributed tracing** — so a
slow or failing request cannot be followed across the edge → box → `fabricd` hops, and nothing
is attributed to the tenant/user behind it. The metrics are already the enterprise-correct
shape (aggregate + bounded labels, no identity dimension), so the gap is precisely the *other
two pillars*. Adding tracing + structured logs now — before per-tenant metering/audit (Change
C) rides on top — gives every request a trace id, a tenant/user attribution, and a
correlation-ready log line, without touching the low-cardinality metric surface.

## What Changes

- **Adopt OpenTelemetry** as the emission layer for traces (build-vs-adopt gate; recorded in
  design.md). Wire it on the existing `aws-lc-rs`/`reqwest`-rustls path so no second crypto
  stack is linked.
- **Distributed tracing:** each `/execute` becomes a span carrying **tenant/user/plan as span
  attributes** (never metric labels). Spans export via **OTLP push** to a collector using the
  SDK's `BatchSpanProcessor` (async, bounded, drop-on-full — no hot-path blocking). Sampling is
  **parent-based**: the box continues the edge's trace when a W3C `traceparent` is present and
  head-samples its own root as a fallback; tail-sampling is left to the collector.
- **Structured logging:** replace the plain-text `fmt` subscriber with **structured JSON to
  stdout** (12-factor; the collector tails it, so logs survive a collector outage). Every line
  carries the OTel `trace_id`/`span_id` for cross-signal correlation.
- **Thread identity into the finish path:** `TrustedIdentity` (tenant/user/plan) is currently
  resolved in `execute()` and never reaches `build_response`; wire it through so the span and
  log lines carry attribution. **No** per-tenant *metric* series are added (that is Change C).
- **Metrics unchanged:** the `runlet_*` Prometheus PULL surface is untouched. The optional
  `plan`-tier label is explicitly deferred to Change C.
- **Record the nexus upstream requirement (N6):** the edge SHALL emit a W3C `traceparent` (and
  keep the tenant in trusted headers) so edge → box → `fabricd` is one trace. Producer-before-
  consumer, like N5; the box degrades gracefully (starts its own root) until the edge ships it.
- **Out of scope (→ Change C):** per-tenant metering, the audit log, the durable-outbox event
  schema, the `plan`-tier metric label, and any OTLP *metrics* export (metrics stay pull).

## Capabilities

### New Capabilities
<!-- none — tracing + structured logging extend the existing observability capability -->

### Modified Capabilities
- `observability`: adds a **distributed tracing** requirement (spans per request with
  identity attributes, OTLP export, parent-based sampling) and a **structured logging**
  requirement (JSON to stdout, trace-correlated); refines the existing **correlation id**
  requirement so `meta.trace_id` is the OTel trace id, propagated from the edge when present.
  The `/metrics` and `/health` requirements are unchanged.

## Impact

- **Dependencies (new):** `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`,
  `tracing-opentelemetry`, and a JSON `tracing-subscriber` layer — all pinned to the
  `aws-lc-rs` provider (no `ring`/`native-tls`). Supply-chain: re-run `cargo vet` / add
  exemptions; confirm `cargo tree -i ring` unchanged.
- **Code:** `runlet/src/main.rs` (subscriber + OTLP exporter init, graceful shutdown flush),
  `runlet/src/handler.rs` (span creation around `/execute`, `traceparent` extraction, thread
  `TrustedIdentity` into `build_response`), `runlet/src/config.rs` (OTLP endpoint, sampling
  ratio, service name, on/off). Likely a small `runlet/src/telemetry.rs` for init.
- **`runlet-core`:** unchanged — it stays HTTP/identity-agnostic; tracing is a `runlet`-layer
  concern. `fabricd` may later join the trace (separate follow-on), not required here.
- **Config/deploy:** new OTLP collector endpoint + sampling config; docs for the collector.
- **Contract:** `docs/design/nexus-upstream-requirements.md` gains **N6** (edge `traceparent`).
- **No behavior change to execution semantics**; metrics surface unchanged.
