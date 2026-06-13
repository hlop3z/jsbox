# module-registry Specification

## Purpose

Operator-authored ES modules a handler can `import` — reusable helper libraries (validation,
query building, formatting). Loaded once at startup from `modules_dir`, resolved in-memory, and
run inside the same sandbox as the handler. Rationale: `docs/design/injectable-modules.md`,
authoring how-to: `docs/modules.md`.

## Requirements

### Requirement: Startup load from modules_dir

The system SHALL load every `*.js` / `*.mjs` file under the configured `modules_dir` once at
startup, keying each by its path relative to `modules_dir` without the extension.

#### Scenario: Specifier derivation

- **WHEN** a file `acme/pricing.mjs` exists under `modules_dir`
- **THEN** it is importable under the specifier `acme/pricing`

#### Scenario: Registry disabled

- **WHEN** no `modules_dir` is configured
- **THEN** the module registry is empty and any `import` of a module fails to resolve

### Requirement: Import resolves only registered modules

A handler SHALL `import` a module by its specifier, resolved by a pure in-memory lookup with no
filesystem access, so only registered modules are reachable.

#### Scenario: Registered module imports successfully

- **WHEN** a handler does `import { x } from "acme/pricing"` and `acme/pricing` is registered
- **THEN** the import resolves and the module's exports are available

#### Scenario: Unresolved import

- **WHEN** a handler imports an unregistered specifier (unknown name, `../…`, `/etc/…`)
- **THEN** execution fails with error code `MODULE_NOT_FOUND` (owner: developer, not retryable)

### Requirement: Modules run under the sandbox budget

Module code SHALL run in the same fresh context as the handler, under the same memory, timeout,
stack, and `max_ops` limits, with no confidentiality or integrity guarantee against the handler
sharing its context.

#### Scenario: Shared budget

- **WHEN** an imported module allocates or performs operations
- **THEN** they count against the same execution's memory and `max_ops` budget

### Requirement: Immutable at runtime

The module registry SHALL be read-only at runtime; changing a module requires redeploying the
file and restarting, so every replica resolves identically.

#### Scenario: No runtime mutation

- **WHEN** the registry is loaded at startup
- **THEN** module sources do not change for the lifetime of the process
