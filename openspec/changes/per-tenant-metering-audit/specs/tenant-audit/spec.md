## ADDED Requirements

### Requirement: Per-tenant audit event per request

The system SHALL emit exactly one `audit`-type event for every trusted-mode request, recording the
terminal decision: `allowed` when the request ran, or `denied` with a machine-readable reason code
when a gate terminated it. Each audit event SHALL carry the trusted tenant id and user id (for
attribution) and, when relevant, the deciding detail (e.g. the quota plan/limit/usage, the missing
entitlement, the acting-scope value). Audit events SHALL NOT be sampled.

#### Scenario: Allowed request is audited

- **WHEN** a trusted-mode request runs to execution
- **THEN** one `audit` event with decision `allowed` is emitted carrying the tenant id and user id

#### Scenario: Denied request is audited with a reason

- **WHEN** a request is rejected at a gate (anonymous, suspended, tenant-less, non-acting scope, missing entitlement, quota exceeded, oversized, egress-session failure, or shed)
- **THEN** one `audit` event with decision `denied` and the corresponding reason code is emitted, carrying the tenant id (when known) and user id

#### Scenario: Quota decision is captured in the audit trail

- **WHEN** a request is denied by the per-tenant quota gate
- **THEN** the `audit` event's reason is the quota code and the body carries the plan, limit, and usage at decision time

### Requirement: Attributed, reasoned events replace the blind reject counter

Rejections SHALL be recorded as attributed audit events (tenant + reason), not only as an
unattributed aggregate counter. The existing aggregate rejection metric MAY be retained for
low-cardinality alerting, but the per-tenant reason attribution SHALL live in the audit event, never
as a metric label (the cardinality invariant).

#### Scenario: Reason attribution is in the event, not a label

- **WHEN** a rejection is recorded
- **THEN** the tenant id and reason appear in the audit event, and any retained rejection metric stays unlabeled by tenant/reason

### Requirement: Identity is not leaked beyond attribution

Audit events SHALL carry only the non-sensitive trusted identifiers already used for isolation
(tenant id, user id, plan) and the decision metadata; request bodies, secrets, and edge credentials
SHALL NOT appear in events.

#### Scenario: No sensitive payload in audit events

- **WHEN** an audit event is emitted
- **THEN** it contains no request body, secret, or edge credential — only identifiers and decision metadata
