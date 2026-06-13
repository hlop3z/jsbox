# resilience Specification

## Purpose

Layered defense-in-depth so a slow or dead downstream can't breach the SLO: a server-enforced
`statement_timeout` ceiling, a concurrency bulkhead, a client-side query deadline, a per-target
circuit breaker, and per-partition fairness. Each layer catches what the one below missed.
Rationale, measured A/B results, and the async-Rust lessons: `docs/design/resilience.md`.
Operator deployment guidance: `docs/deployment.md`.

## Requirements

### Requirement: Statement-timeout clamp (Tier 0)

The system SHALL clamp a per-request `config.db.statement_timeout_ms` to the operator ceiling
`max_statement_timeout_ms`, so jsbox never issues an unbounded `SET`.

#### Scenario: Request value clamped

- **WHEN** `max_statement_timeout_ms` is set and a request asks for a larger (or `0`/unlimited) `statement_timeout_ms`
- **THEN** the effective value is clamped to the ceiling and a long query is killed at the ceiling

#### Scenario: Ceiling disabled

- **WHEN** `max_statement_timeout_ms` is `0`
- **THEN** no operator ceiling is applied and the request value is used as-is

### Requirement: Concurrency bulkhead (Tier 1)

The system SHALL bound concurrent executions with `max_concurrent_executions`, fast-failing
excess load rather than queueing.

#### Scenario: Shed on saturation

- **WHEN** the bulkhead is saturated and another request arrives
- **THEN** it fast-fails with HTTP 429 and error code `OVERLOADED` (retryable, owner operator)

#### Scenario: Cheap rejections do not consume a permit

- **WHEN** a request is malformed, oversized, or names an unknown key
- **THEN** it is rejected before taking an execution permit

### Requirement: Client-side query deadline (Tier 2)

The system SHALL bound each `db` query by the execution wall-clock budget on the blocking thread,
so a hung query is freed even when a server-side timeout is lost through a pooler.

#### Scenario: Hung query bounded

- **WHEN** a query runs past the execution deadline (e.g. through a transaction-mode pooler that lost the `SET`)
- **THEN** it is abandoned and returns retryable `DB_TIMEOUT`, freeing the blocking thread

### Requirement: Per-target circuit breaker (Tier 3)

The system SHALL track consecutive `db` connect failures per target (`host:port`) and, after
`db_breaker_threshold`, fast-fail further calls to that target for a cool-down window.

#### Scenario: Breaker opens

- **WHEN** a target fails to connect `db_breaker_threshold` consecutive times
- **THEN** subsequent calls to that target return retryable `DB_CIRCUIT_OPEN` without attempting a connect

#### Scenario: Half-open probe after cool-down

- **WHEN** the cool-down `db_breaker_cooldown_ms` elapses on an open breaker
- **THEN** a single probe is allowed; success closes the breaker, failure re-opens it

#### Scenario: Breaker disabled

- **WHEN** `db_breaker_threshold` is `0`
- **THEN** no breaker is active and every call attempts a connect

### Requirement: Per-partition fairness (Tier 5)

The system SHALL optionally cap concurrency per partition key so a noisy key cannot monopolize a
pod while global capacity remains.

#### Scenario: Noisy partition sheds on its own share

- **WHEN** `max_concurrent_per_partition` is set and one partition exceeds its share
- **THEN** that partition's excess fast-fails HTTP 429 `PARTITION_OVERLOADED` (retryable, owner caller) while other partitions are unaffected

#### Scenario: Partition key source and echo

- **WHEN** a request supplies a partition via the `X-Partition-Key` header and/or a `partition` body field
- **THEN** the header takes precedence and the resolved value is echoed in `meta.partition`
