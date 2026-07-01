## MODIFIED Requirements

### Requirement: Per-request isolation and sandbox limits

Each execution SHALL run in a fresh QuickJS context with no global scope leaking between
requests, under operator-configured memory, stack, wall-clock timeout, and operation-count
limits, with `eval` and `Proxy` removed before the handler runs. Any cross-request compilation
or bytecode cache SHALL be namespaced by the trusted tenant identity, so identical source from
different tenants never shares a cache entry (no cross-tenant dedup or compile-timing leak).

#### Scenario: No cross-request global leakage

- **WHEN** one request mutates global scope
- **THEN** a subsequent request observes a clean global scope

#### Scenario: Wall-clock timeout

- **WHEN** a handler runs past the configured `timeout_ms`
- **THEN** execution is interrupted and the response error code is `TIMEOUT`

#### Scenario: Operation cap

- **WHEN** a handler exceeds `max_ops` external operations
- **THEN** the offending capability call fails with an operation-limit error

#### Scenario: Compilation cache does not cross tenants

- **WHEN** two different tenants submit byte-identical source
- **THEN** each tenant's compilation is cached under its own tenant namespace and neither observes the other's cache entry
