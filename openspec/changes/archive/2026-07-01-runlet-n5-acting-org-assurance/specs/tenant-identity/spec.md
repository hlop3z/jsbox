## ADDED Requirements

### Requirement: Acting-org assurance is mandatory in trusted mode

When running in trusted-header mode, the system SHALL require the edge to assert, per request, that
the request's tenant identity is the caller's **authorized acting org** — carried in a configurable
trusted scope header (`x-tenant-scope` by default) with the value `acting`. A tenant-scoped
`/execute` request whose trusted scope header is absent or not equal to `acting` SHALL be rejected
with an authorization failure (`403 ACTING_SCOPE_REQUIRED`) before any egress session or execution
begins. This assurance is intrinsic to trusted mode: there is no opt-in flag and no mode that accepts
a non-acting (home-org) scope. The system SHALL continue to treat the tenant identifier as opaque —
it checks the scope label only and never derives the org relationship itself. The scope header value
is client-unspoofable on the same basis as every other trusted header (the edge strips client-supplied
`x-*` and injects the trusted value); the guard is a fail-closed contract tripwire against an edge
that has not satisfied the acting-org requirement, not cryptographic proof.

#### Scenario: Authorized acting-org request proceeds

- **WHEN** a tenant-scoped request carries the trusted tenant header and the trusted scope header set to `acting`
- **THEN** identity is accepted and the request proceeds subject to the remaining tenant checks

#### Scenario: Missing acting-org assurance is refused

- **WHEN** trusted-header mode is enabled and a tenant-scoped request carries a tenant header but no trusted scope header
- **THEN** the request is rejected with an authorization failure and no execution or egress session begins

#### Scenario: Non-acting scope is refused

- **WHEN** a tenant-scoped request carries a trusted scope header whose value is not `acting`
- **THEN** the request is rejected with an authorization failure and no execution or egress session begins

#### Scenario: Non-trusted mode is unaffected

- **WHEN** trusted-header mode is disabled (single-tenant / loopback)
- **THEN** the scope header is not consulted and request handling is unchanged

#### Scenario: Scope header name is configurable

- **WHEN** the operator overrides the trusted scope header name to a non-default value
- **THEN** only that configured header is read for the acting-org assertion and the default name has no effect
