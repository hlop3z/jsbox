## ADDED Requirements

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
