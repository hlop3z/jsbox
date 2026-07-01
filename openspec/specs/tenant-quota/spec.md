# tenant-quota Specification

## Purpose

Per-workspace, plan-gated usage limits: a data-driven `plan → limit` table caps a tenant's usage,
with a fail-closed conservative default for unknown plans and a structured over-limit result.
Accounting is keyed on the trusted tenant identity. Modeled on the nexus `routing-rs/plan.rs`
shape. Rationale: `docs/design/multitenant-trust.md`.

## Requirements

### Requirement: Per-tenant plan-gated usage limits

The system SHALL bound usage per tenant according to a data-driven `plan → limit` table, and
SHALL return a structured over-limit result identifying the plan, the limit, and the current
usage so a caller can act on it. Limits SHALL NOT be embedded as constants; they are loaded
from configuration.

#### Scenario: Usage within the plan limit proceeds

- **WHEN** a tenant on a plan is below its configured limit
- **THEN** the request proceeds normally

#### Scenario: Usage at or above the plan limit is refused

- **WHEN** a tenant is at or above its configured limit
- **THEN** the request is refused with a structured result carrying the plan, limit, and current usage

### Requirement: Fail-closed default for unknown plans

An unknown or unconfigured plan SHALL resolve to the most restrictive configured limit, never
to unbounded, so a misconfiguration cannot grant unlimited usage.

#### Scenario: Unknown plan inherits the most restrictive limit

- **WHEN** a request's plan is not present in the configured table
- **THEN** the most restrictive configured limit applies

#### Scenario: Empty configuration denies

- **WHEN** no plan limits are configured
- **THEN** usage is denied rather than allowed unbounded

### Requirement: Per-tenant usage accounting

The system SHALL attribute usage to the request's trusted tenant identity, so accounting and
quota decisions are keyed on the tenant and not on any caller-asserted value.

#### Scenario: Usage attributed to the trusted tenant

- **WHEN** a request executes under a tenant identity
- **THEN** its usage is counted against that tenant for accounting and quota
