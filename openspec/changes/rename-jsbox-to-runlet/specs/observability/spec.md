## MODIFIED Requirements

### Requirement: Prometheus metrics endpoint

The system SHALL expose `GET /metrics` as dependency-free Prometheus text exposition reporting
process-wide counters and live gauges. All emitted series use the `runlet_` name prefix.

#### Scenario: Execution outcome counters

- **WHEN** executions complete
- **THEN** `runlet_executions_total{outcome}` increments per terminal outcome (`success`, `script_error`, `capability_error`, `timeout`, `memory_limit`, `malformed_response`, `internal_error`)

#### Scenario: Shed-load and rejection counters

- **WHEN** requests are rejected or shed
- **THEN** `runlet_rejections_total` and `runlet_overload_total{scope="global"|"partition"}` increment accordingly

#### Scenario: Circuit-breaker and bulkhead signals

- **WHEN** the metrics are scraped
- **THEN** `runlet_db_breaker_trips_total` reports cumulative breaker opens and `runlet_bulkhead_permits_available` / `runlet_bulkhead_permits_total` report live and configured capacity

#### Scenario: Latency histograms

- **WHEN** executions run
- **THEN** `runlet_execution_duration_seconds` (overall) and `runlet_capability_op_duration_seconds{capability}` (per downstream) are exposed as Prometheus histograms
