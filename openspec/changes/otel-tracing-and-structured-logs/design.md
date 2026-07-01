## Context

Observability today is one pillar: `runlet-core::Metrics` renders aggregate Prometheus text at
`GET /metrics` (bounded labels — `outcome`/`scope`/`capability` — no identity dimension), and
logs are plain-text via `tracing_subscriber::fmt::init`. There is no tracing. `meta.trace_id`
exists as a per-response correlation id but is not an OTel trace and is not propagated from the
edge. Identity (`TrustedIdentity`: tenant/user/plan) is resolved in `handler.rs::execute()` and
never reaches `build_response`, so nothing downstream can be attributed to a tenant.

The deployment target is untrusted multi-tenant traffic at scale (millions of users, horizontally
sharded box replicas behind the nexus edge). At that scale the cardinality rule is absolute:
**identity is a trace/log dimension, never a metric label.** The metrics are already correct;
the gap is distributed tracing and structured, correlated logs.

Constraints: strict lint gauntlet (no `unwrap`/`expect`/`panic`, no `as`, pedantic/nursery/cargo);
the rustls provider is `aws-lc-rs` (adding a TLS-using dep must reuse it, not pull `ring`/
`native-tls`); build/test are Docker-only. `runlet-core` must stay HTTP/identity-agnostic.

## Goals / Non-Goals

**Goals:**
- Every `/execute` emits an OTel span with tenant/user/plan as **attributes**, exported OTLP.
- Structured JSON logs to stdout, each carrying `trace_id`/`span_id` for cross-signal correlation.
- Continue the edge trace when a W3C `traceparent` is present; degrade to a box-started root
  otherwise (no dependency on the edge shipping first).
- Export never blocks or fails the request path (batch + drop-on-full).
- Zero change to execution semantics and to the `runlet_*` metric surface.

**Non-Goals:**
- No per-tenant metrics, metering, or audit log (Change C).
- No OTLP *metrics* export; metrics stay Prometheus PULL.
- No `plan`-tier metric label (Change C).
- No `fabricd`-side spans yet (a later follow-on can join the trace over the wire).
- No change to `runlet-core` (tracing is a `runlet`-layer concern).

## Decisions

**D1 — ADOPT OpenTelemetry for tracing (build-vs-adopt gate).**
Per-concern verdict: distributed tracing, context propagation (W3C `traceparent`), sampling, and
batched OTLP export are correctness- and interop-critical and a large surface to hand-roll.
Rent > **Adopt** > Extend > Fork > Build → **Adopt** the OpenTelemetry Rust SDK
(`opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`) bridged to the existing `tracing`
facade via `tracing-opentelemetry`. Maturity: OTel is the CNCF vendor-neutral standard; the Rust
SDK is widely used and interop-tested; the wire format (OTLP) is stable. *Alternatives:* hand-roll
spans + a bespoke exporter (rejected — reinvents propagation/sampling/batching, no collector
interop); Jaeger/Zipkin-native clients (rejected — vendor-specific, OTLP supersedes them).
*Constraint:* configure `opentelemetry-otlp`'s transport (tonic/gRPC or http/protobuf) on the
**`aws-lc-rs`** rustls provider — no `ring`/`native-tls`; verify `cargo tree -i ring` unchanged
and clear supply-chain (`cargo vet`).

**D2 — Traces PUSH (OTLP); metrics stay PULL; logs go to stdout.**
Hybrid transport. Metrics are already low-cardinality and the PULL endpoint is independent of
collector availability — leave it untouched. Traces are new and benefit from the collector's
batching/tail-sampling → OTLP push via `BatchSpanProcessor`. Logs go to **stdout as JSON**, tailed
by the collector, so a collector outage never loses logs. *Alternative:* unify everything on OTLP
(rejected for now — couples metric/log health to collector health for no scale benefit here).

**D3 — Parent-based sampling; the box never tail-samples.**
The box respects the edge's sampling decision when `traceparent` is present and applies a
configured ratio to its own roots otherwise. Keeping-all-errors/slow-traces (tail sampling) is the
**collector's** job. This keeps the hot path O(1) and cheap, and centralizes the expensive policy.
*Alternative:* box-side tail sampling (rejected — needs buffering full traces in-process; wrong
layer at replica scale).

**D4 — Identity is a span attribute, not a metric label (the cardinality invariant).**
tenant/user/plan attach to the span and log line, where the backend is built for high cardinality.
They never become Prometheus labels. This is the load-bearing scale decision; it is why Change B
does not touch metrics. To satisfy it, thread `TrustedIdentity` from `execute()` into
`build_response` (today it stops at `execute()`); the finish path is where outcome + identity meet.

**D5 — Tracing/logging live in `runlet`, not `runlet-core`.**
`runlet-core` stays HTTP- and identity-agnostic (it knows `partition`, not `tenant`, and nothing
about HTTP headers). Span creation, `traceparent` extraction, identity attributes, and subscriber
init are all `runlet`-layer concerns (likely a small `runlet/src/telemetry.rs`). Core continues to
return facts in `Outcome`; `runlet` decorates the span from them.

**D6 — Graceful, fail-open init and shutdown.**
Tracing is opt-in via config (endpoint + ratio + on/off). A missing/unreachable collector must not
fail startup or requests (fail-open: the box runs untraced). On shutdown the tracer provider is
flushed so in-flight spans are not lost.

## Risks / Trade-offs

- **A second crypto stack sneaks in via the OTLP/gRPC transport** → binary bloat + supply-chain
  surface. *Mitigation:* pin the exporter to `aws-lc-rs` (rustls-no-provider + the dep's aws-lc-rs
  feature); gate on `cargo tree -i ring` unchanged + `cargo vet`.
- **Span/log export on the hot path adds latency or blocks** → *Mitigation:* `BatchSpanProcessor`
  (async, bounded, drop-on-full); the request path only records into the buffer. Covered by the
  "export never blocks" scenario.
- **Edge not yet emitting `traceparent`** → traces are box-rooted (still useful), not edge-to-box.
  *Mitigation:* graceful fallback (D3) + record **N6** so the edge ships propagation; no hard dep.
- **PII/secret leakage into traces or logs** → *Mitigation:* attributes limited to the
  non-sensitive tenant/user ids already used for isolation; redact edge credentials; no request
  body or headers in spans/logs. Covered by the structured-logging redaction requirement.
- **Sampling hides a rare failing request** → *Mitigation:* parent-based + collector tail-sampling
  keeps errors/slow traces; the aggregate metrics still count every request.

## Migration Plan

Additive and opt-in. Ship with tracing configurable and default consistent with current behavior
(or off) until a collector endpoint is set. Rollout: (1) land box support (fail-open); (2) stand up
the collector; (3) set the endpoint; (4) bring up edge `traceparent` (N6). Rollback = disable via
config or revert; no persisted state. Gate in Docker: `cargo fmt --check`, `cargo clippy`,
`cargo test`, supply-chain (`cargo audit`/`deny`/`vet`), and `cargo tree -i ring` unchanged.

## Open Questions

- **OTLP transport:** tonic/gRPC vs http/protobuf exporter — both must ride `aws-lc-rs`; pick the
  one with the smaller/cleaner dep tree on this provider (settle during `/opsx:apply`).
- **`fabricd` joining the trace** (propagate context over the box↔`fabricd` wire) — deferred to a
  follow-on; the wire already has a natural place to carry `traceparent`.
- **Default sample ratio** for box-rooted traces — an operational tuning value, not a design blocker.
