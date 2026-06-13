# script-registry Specification

## Purpose

Execute pre-deployed scripts by `key` instead of sending source on every call. Scripts are
loaded once at startup from `scripts_dir`, keyed by relative path, and served read-only — so
the service stays stateless and replicas stay trivially consistent. Rationale:
`docs/design/script-registry.md`.

## Requirements

### Requirement: Startup load from scripts_dir

The system SHALL load every `*.js` file under the configured `scripts_dir` once at startup,
keying each by its path relative to `scripts_dir` without the `.js` extension.

#### Scenario: Nested key derivation

- **WHEN** a file `acme/billing/pricing.js` exists under `scripts_dir`
- **THEN** it is registered under the key `acme/billing/pricing`

#### Scenario: Registry disabled

- **WHEN** no `scripts_dir` is configured
- **THEN** the registry is empty and any `key` request fails with `SCRIPT_NOT_FOUND`

#### Scenario: Oversized registered script

- **WHEN** a registered file exceeds `max_script_size`
- **THEN** startup fails with an error rather than deferring it to a request

### Requirement: Execute by key

A request SHALL execute a registered script by supplying its `key`, running through the
identical engine path as an inline script (same sandbox, same fresh context, per-request config).

#### Scenario: Known key

- **WHEN** a request supplies a `key` present in the registry
- **THEN** the registered source executes and `meta.key` echoes the key

#### Scenario: Unknown key

- **WHEN** a request supplies a `key` absent from the registry
- **THEN** the response is HTTP 404 with error code `SCRIPT_NOT_FOUND`

### Requirement: Immutable, in-memory resolution

Key lookup SHALL be a pure in-memory map lookup with no filesystem access at request time, so a
traversal-shaped key can never escape `scripts_dir`, and the registry never changes at runtime.

#### Scenario: Traversal key does not resolve

- **WHEN** a request supplies a key like `../secret` or `/etc/passwd`
- **THEN** it resolves to no script (`SCRIPT_NOT_FOUND`); `../` has no filesystem meaning

#### Scenario: Read-only at runtime

- **WHEN** the registry is loaded
- **THEN** changing scripts requires redeploying files and restarting; no runtime mutation occurs
