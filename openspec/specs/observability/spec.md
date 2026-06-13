# observability Specification

## Purpose

Operational visibility into the running service: a liveness endpoint and a dependency-free
Prometheus metrics endpoint exposing per-outcome execution counters, shed-load and breaker
signals, and latency histograms — so SLOs and degraded downstreams are alertable without log
parsing. Every response also carries a correlation id. Alert guidance: `docs/deployment.md`.

## Requirements

### Requirement: Liveness endpoint

The system SHALL expose `GET /health` returning HTTP 200 with body `ok`, with no backend
dependencies (it reflects process liveness, not downstream health).

#### Scenario: Health check

- **WHEN** a client requests `GET /health`
- **THEN** the response is HTTP 200 with body `ok`

### Requirement: Prometheus metrics endpoint

The system SHALL expose `GET /metrics` as dependency-free Prometheus text exposition reporting
process-wide counters and live gauges.

#### Scenario: Execution outcome counters

- **WHEN** executions complete
- **THEN** `jsbox_executions_total{outcome}` increments per terminal outcome (`success`, `script_error`, `capability_error`, `timeout`, `memory_limit`, `malformed_response`, `internal_error`)

#### Scenario: Shed-load and rejection counters

- **WHEN** requests are rejected or shed
- **THEN** `jsbox_rejections_total` and `jsbox_overload_total{scope="global"|"partition"}` increment accordingly

#### Scenario: Circuit-breaker and bulkhead signals

- **WHEN** the metrics are scraped
- **THEN** `jsbox_db_breaker_trips_total` reports cumulative breaker opens and `jsbox_bulkhead_permits_available` / `jsbox_bulkhead_permits_total` report live and configured capacity

#### Scenario: Latency histograms

- **WHEN** executions run
- **THEN** `jsbox_execution_duration_seconds` (overall) and `jsbox_capability_op_duration_seconds{capability}` (per downstream) are exposed as Prometheus histograms

### Requirement: Correlation id on every response

The system SHALL include a unique `meta.trace_id` on every response and log it server-side with
the raw cause, so one id correlates a response with server logs.

#### Scenario: Trace id correlation

- **WHEN** any request completes (success or error)
- **THEN** `meta.trace_id` is present and the same id appears in the server-side log for that request
