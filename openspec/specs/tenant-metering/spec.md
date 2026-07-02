# tenant-metering Specification

## Purpose

Per-request, per-tenant usage events — the billing / quota-tuning record. For every executed
request the box emits one `usage` event attributed to the trusted tenant and plan, carrying the
usage dimensions already computed in the response `meta` (execution time, per-capability operation
counts including `fabricd`-drained egress, input sizes, outcome). Events share a versioned envelope
with a per-event `event_id` dedup key and are emitted non-blocking + fail-open, so a durable,
billing-grade outbox can be added later without changing the emission sites. Tenant identity lives
in events, never in metric labels (the cardinality invariant). See `docs/deployment.md`.

## Requirements

### Requirement: Per-tenant usage event per execution

The system SHALL emit exactly one `usage`-type event for every request that runs to execution
(any terminal outcome), attributed to the trusted tenant id and plan. The event SHALL carry the
per-request usage dimensions the response `meta` already computes — wall-clock execution time,
per-capability operation counts (including `fabricd`-drained egress), request/response byte sizes,
and the terminal outcome. Usage events SHALL NOT be sampled (every execution is metered).

#### Scenario: Executed request emits a usage event

- **WHEN** a trusted-mode request completes execution (success or error)
- **THEN** one `usage` event is emitted carrying the tenant id, plan, outcome, execution time, and per-capability op counts

#### Scenario: Rejected-before-execution request emits no usage event

- **WHEN** a request is rejected at a gate before execution (e.g. quota, authz, suspended)
- **THEN** no `usage` event is emitted (only an `audit` event records the rejection)

#### Scenario: Usage is never a metric label

- **WHEN** usage is attributed to a tenant
- **THEN** the tenant id appears only in the event, never as a Prometheus metric label

### Requirement: Versioned event envelope with a dedup key

Every emitted event (usage or audit) SHALL use a shared, versioned envelope carrying a schema
version, a unique `event_id` (the idempotency/dedup key), a timestamp, the trusted tenant id, the
user id, the plan, the `trace_id`, and the event `type`; the type-specific payload lives in `body`.
The `event_id` SHALL be unique per event so a later durable, deduplicating consumer can be added
without changing the emission sites or the schema.

#### Scenario: Envelope fields present on every event

- **WHEN** any event is emitted
- **THEN** it carries `v`, `event_id`, `ts`, `tenant`, `trace_id`, and `type`, with a unique `event_id`

#### Scenario: Correlation with the request trace

- **WHEN** a request emits a usage and/or audit event
- **THEN** each event's `trace_id` equals the request's `meta.trace_id`

### Requirement: Non-blocking, fail-open emission

Event emission SHALL NOT block or fail the request path. Events SHALL be handed to a bounded buffer
and written asynchronously; under backpressure events are dropped (not awaited) and a dropped-events
counter is incremented. When event emission is disabled or unconfigured, request handling is
unaffected. On graceful shutdown, buffered events SHALL be flushed on a best-effort basis.

#### Scenario: Slow or full sink does not block requests

- **WHEN** the event buffer is full or the writer is slow
- **THEN** the request completes normally and the dropped-events counter increments

#### Scenario: Disabled emission is inert

- **WHEN** event emission is disabled in config
- **THEN** requests behave exactly as before and no events are produced
