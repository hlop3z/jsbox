# tenant-identity Specification

## Purpose

The trusted-header identity contract for running `/execute` behind the nexus edge: the box derives
tenant + user identity solely from operator-configured trusted headers the edge injects, rejects
anonymous/suspended principals, is protected by a trusted-headers boot guard (network-isolation
assertion), and can gate capabilities on a member's roles/entitlements. Rationale:
`docs/design/multitenant-trust.md`.

## Requirements

### Requirement: Trusted identity ingress

When running in trusted-header mode, the system SHALL derive the request's tenant and user
identity solely from operator-configured trusted headers injected by the edge, and SHALL
ignore any identity value supplied by the client. The tenant identifier (`x-tenant-id` by
default) is treated as an opaque, already-authorized acting-workspace id.

#### Scenario: Tenant identity is taken from the trusted header

- **WHEN** a request arrives with the configured tenant header set to a workspace id
- **THEN** that value is used as the request's tenant identity and echoed in `meta`

#### Scenario: Client-supplied identity is ignored

- **WHEN** a request carries both a client-set identity value and the trusted headers
- **THEN** only the trusted-header values are used and the client-set value has no effect

#### Scenario: Missing tenant identity for tenant-scoped work

- **WHEN** trusted-header mode is enabled and a request requires tenant scope but carries no tenant header
- **THEN** the request is rejected and no execution or egress session begins

### Requirement: Reject anonymous and suspended principals

The system SHALL reject a request whose trusted identity indicates an anonymous caller
(`x-auth-anonymous: true`) or a suspended principal (`x-user-suspended: true`), because
executing caller-supplied code requires an authenticated, active principal.

#### Scenario: Anonymous request refused

- **WHEN** a request carries `x-auth-anonymous: true`
- **THEN** the response is an authorization failure and no handler runs

#### Scenario: Suspended principal refused

- **WHEN** a request carries `x-user-suspended: true`
- **THEN** the response is an authorization failure and no handler runs

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

### Requirement: Trusted-headers boot guard

The system SHALL refuse to start in trusted-header mode when bound to a non-loopback address
unless the operator has explicitly asserted network isolation, so identity headers are never
trusted on an exposed bind. When configured, the edge service credential SHALL be required on
inbound requests as defense in depth.

#### Scenario: Exposed bind without asserted isolation refuses to start

- **WHEN** trusted-header mode is enabled, the bind address is non-loopback, and isolation is not asserted
- **THEN** the process refuses to start with a configuration error

#### Scenario: Missing service credential is rejected

- **WHEN** the edge service credential is configured and an inbound request omits or mismatches it
- **THEN** the request is rejected before identity is trusted

### Requirement: Coarse member capability authorization

The system SHALL support gating a requested capability against the caller's trusted roles or
entitlements, so a member without the required role/entitlement cannot invoke that capability
even within their own workspace.

#### Scenario: Member lacks the required entitlement

- **WHEN** a request invokes a capability the caller's `x-user-roles`/`x-user-entitlements` do not permit
- **THEN** the request is rejected before the capability runs

#### Scenario: Member holds the required entitlement

- **WHEN** the caller's trusted roles/entitlements permit the requested capability
- **THEN** the capability proceeds subject to the remaining tenant checks
