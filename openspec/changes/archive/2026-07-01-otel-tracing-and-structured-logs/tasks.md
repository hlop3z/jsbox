## 1. Dependencies + telemetry init (runlet)

- [x] 1.1 Add `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`, and
  `tracing-opentelemetry` to `runlet/Cargo.toml`, configuring the OTLP exporter transport on the
  `aws-lc-rs` rustls provider (rustls-no-provider + the dep's `aws-lc-rs` feature) — no `ring`/
  `native-tls`. Add a JSON layer to `tracing-subscriber` (enable its `json` feature).
- [x] 1.2 Create `runlet/src/telemetry.rs`: build the tracer provider (OTLP + `BatchSpanProcessor`,
  parent-based sampler with a configured ratio), the W3C `TraceContextPropagator`, and a
  `tracing-subscriber` registry combining a JSON-to-stdout fmt layer with the OTel layer. Fail-open:
  a missing/unreachable endpoint logs a warning and runs untraced, never panics. Expose an init that
  returns a guard/handle for flush-on-shutdown.
- [x] 1.3 In `runlet/src/main.rs`, replace `init_tracing()` with the `telemetry` init; flush/shutdown
  the tracer provider on the existing graceful-shutdown signal path so in-flight spans aren't lost.

## 2. Config (runlet)

- [x] 2.1 Add a `telemetry`/`tracing` config block to `runlet/src/config.rs`: OTLP endpoint,
  sample ratio, service name, and on/off (default off / consistent with current behavior). Follow the
  existing config-struct + defaults + unit-test pattern.
- [x] 2.2 Unit-test config parse/defaults (present → enabled with values; absent → disabled).

## 3. Request span + identity attribution (runlet)

- [x] 3.1 In `runlet/src/handler.rs::execute`, extract a W3C `traceparent` (via the propagator) into
  the request context and open an `/execute` span as its child (or a new root when absent).
- [x] 3.2 Thread `TrustedIdentity` (tenant/user/plan) from `execute()` into `build_response` and set
  them as span **attributes** (only in trusted mode). Do not add any metric label.
- [x] 3.3 Set `meta.trace_id` from the active OTel trace id (propagated when present, else the
  box-started root id; a generated id when tracing is disabled) so response/log/trace share one id.
- [x] 3.4 Record the outcome on the span (success/script_error/capability_error/timeout/…) mirroring
  the metric outcome, and ensure errors are marked on the span.

## 4. Structured logging (runlet)

- [x] 4.1 Ensure server-side logs are structured JSON to stdout with `trace_id`/`span_id` fields when
  a span is in scope; keep the existing raw-cause log tied to `meta.trace_id`.
- [x] 4.2 Verify redaction: no edge credential, request body, or full headers in spans/logs; only the
  non-sensitive tenant/user ids used for attribution.

## 5. Contract + docs

- [x] 5.1 Add **N6** to `docs/design/nexus-upstream-requirements.md`: the edge SHALL emit a W3C
  `traceparent` so edge→box→`fabricd` is one trace; note the box degrades gracefully (box-rooted)
  until then (producer-before-consumer, like N5).
- [x] 5.2 Document the collector endpoint + sampling config in `docs/deployment.md`; note the hybrid
  model (metrics PULL, traces OTLP PUSH, logs JSON stdout) and update `CLAUDE.md`'s observability
  blurb.

## 6. Integration test + gate

- [x] 6.1 Extend `test_simple.py`: with tracing enabled to a local OTLP sink (or a stub), a request
  carrying a `traceparent` yields a `meta.trace_id` equal to the propagated trace id; without one, a
  fresh id is returned; a request still succeeds when the collector endpoint is unreachable
  (fail-open); server logs are valid JSON carrying the `trace_id`.
- [x] 6.2 Run the full gate in Docker: `cargo fmt --check`, `cargo clippy` (clean), `cargo test`,
  supply-chain (`cargo audit`/`deny`/`vet`); confirm `cargo tree -i ring` unchanged and no second
  crypto stack linked.
- [x] 6.3 Run `/opsx:sync` to fold the `observability` delta (tracing + structured logging +
  refined correlation-id) into the main spec, then archive.
