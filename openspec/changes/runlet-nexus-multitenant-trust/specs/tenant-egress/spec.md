## ADDED Requirements

### Requirement: Tenant identity carried on the egress session

The box SHALL include the request's trusted tenant identity in the `fabricd` session
handshake (`WireInit`) when opening an egress session, so the daemon can scope resolution to
that tenant. The tenant identity SHALL never be sourced from a value the executing script can
influence.

#### Scenario: Session opens with the trusted tenant id

- **WHEN** a request that names driver resources opens a `fabricd` session
- **THEN** the handshake carries the request's trusted tenant identity

#### Scenario: No tenant identity, no tenant-scoped session

- **WHEN** trusted-header mode is enabled and no trusted tenant identity is present
- **THEN** no tenant-scoped egress session is opened

### Requirement: Tenant-scoped resource resolution in fabricd

`fabricd` SHALL resolve a logical resource name only within the binding set authorized for the
session's tenant, and SHALL refuse to resolve a name outside that tenant's bindings, so
credentials and resources never cross workspace boundaries.

#### Scenario: Name within the tenant's bindings resolves

- **WHEN** a session for tenant A requests a logical name bound for tenant A
- **THEN** the name resolves to tenant A's configured resource

#### Scenario: Name outside the tenant's bindings is refused

- **WHEN** a session for tenant A requests a logical name that is bound only for tenant B
- **THEN** resolution fails and no connection to tenant B's resource is attempted

#### Scenario: Credentials never reach the box

- **WHEN** any resource is resolved for a session
- **THEN** the resolved credentials remain in `fabricd` and only the logical result crosses the wire
