## ADDED Requirements

### Requirement: Distributed tracing

The system SHALL emit an OpenTelemetry span for every `/execute` request, exported to a
collector over OTLP via a batching processor that never blocks the request path (bounded
buffer; spans are dropped, not awaited, under backpressure). In trusted-header mode the span
SHALL carry the trusted tenant id, user id, and plan as span **attributes** (never as metric
labels). Sampling SHALL be parent-based: when a valid W3C `traceparent` is present the box
continues that trace and honors its sampling decision; otherwise the box starts its own root
span and applies its configured sample ratio. Tracing SHALL be configurable (endpoint, sample
ratio, on/off); when disabled or unconfigured, request handling is unaffected.

#### Scenario: Span emitted per execution with identity attributes

- **WHEN** a trusted-mode `/execute` request completes
- **THEN** a span is exported carrying the tenant id, user id, and plan as attributes, and the request outcome

#### Scenario: Continues the edge trace when traceparent is present

- **WHEN** a request arrives with a valid W3C `traceparent` header
- **THEN** the box's span is a child of that trace (same trace id) and honors the parent sampling decision

#### Scenario: Starts its own trace when no traceparent

- **WHEN** a request arrives without a `traceparent`
- **THEN** the box starts a new root span and applies its configured sample ratio

#### Scenario: Export never blocks the request path

- **WHEN** the OTLP collector is slow or unreachable
- **THEN** request handling is unaffected and spans are dropped rather than awaited

### Requirement: Structured logging

The system SHALL emit server-side logs as structured JSON to stdout (one object per event),
each including the active `trace_id` and `span_id` when a span is in scope, so logs correlate
with traces and the response `meta.trace_id`. Log emission SHALL NOT depend on the collector
being reachable (stdout is the durable path). Trusted identity MUST NOT be logged beyond the
non-sensitive tenant/user ids already carried for attribution; secrets and edge credentials are
redacted.

#### Scenario: Structured JSON with trace correlation

- **WHEN** any request is logged server-side
- **THEN** the log line is a JSON object containing the `trace_id` that matches the response `meta.trace_id`

#### Scenario: Logging survives a collector outage

- **WHEN** the OTLP collector is unreachable
- **THEN** structured logs are still written to stdout

## MODIFIED Requirements

### Requirement: Correlation id on every response

The system SHALL include a `meta.trace_id` on every response and log it server-side with the
raw cause, so one id correlates a response with server logs and (when tracing is enabled) with
the exported trace. When a valid W3C `traceparent` is present, `meta.trace_id` SHALL be the
propagated OpenTelemetry trace id; otherwise it is the id of the box-started root span (or a
generated unique id when tracing is disabled).

#### Scenario: Trace id correlation

- **WHEN** any request completes (success or error)
- **THEN** `meta.trace_id` is present and the same id appears in the server-side log for that request

#### Scenario: Propagated trace id from the edge

- **WHEN** a request carries a valid `traceparent`
- **THEN** `meta.trace_id` equals the propagated trace id, so the response ties back to the edge-originated trace
