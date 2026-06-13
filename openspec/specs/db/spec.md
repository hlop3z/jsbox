# db Specification

## Purpose

The `db` capability gives a handler a `PostgreSQL`/`CockroachDB` client inside the QuickJS
sandbox: parameterized `db.query`/`db.execute` and explicit `db.begin`/`commit`/`rollback`
transactions. The connection is **operator-supplied** in `config.db`, so this capability is
trusted (it connects to whatever host the config names, with no SSRF guard) — unlike the
script-controlled `api` capability. This spec defines opt-in, the JS surface, the
Postgres→JSON type-mapping rule, result limits, the client-side query deadline, the
db-error taxonomy, and metering. Source of truth: `src/db.rs`, `src/js/db.js`,
`docs/03-database.md`, and `docs/design/resilience.md`.

## Requirements

### Requirement: Opt-in via config.db

The `db` global SHALL exist only when the request supplies a `config.db` block; absent that
block the global is undefined.

#### Scenario: Config present injects the global

- **WHEN** a request includes a `config.db` block and the handler references `db`
- **THEN** `db` is a defined object exposing `query`, `execute`, `begin`, `commit`, and `rollback`

#### Scenario: Config absent leaves the global undefined

- **WHEN** a request has no `config.db` block
- **THEN** `typeof db === "undefined"` inside the handler

### Requirement: Trusted operator-supplied connection (no SSRF guard)

The capability SHALL connect to the host/port named in `config.db` without any private/internal
IP block or host allowlist, because the connection target is operator-supplied rather than
script-controlled.

#### Scenario: Connects to operator-named host

- **WHEN** `config.db` names an internal or private-network host
- **THEN** the capability attempts the connection without rejecting it as a private/internal address

#### Scenario: Connection config fields

- **WHEN** `config.db` is provided
- **THEN** it accepts `host`, `port` (default 5432), `user`, `password`, `database`, `ssl` (default false), `statement_timeout_ms` (default 5000), and `max_rows` (default 1000)

### Requirement: Parameterized query and execute

The system SHALL expose `db.query(sql, params?)` for row-returning statements and
`db.execute(sql, params?)` for write statements, binding `params` to positional `$1`, `$2`…
placeholders so values are never interpolated into the SQL text.

#### Scenario: query returns a result shape

- **WHEN** the handler calls `db.query(sql, params)` and it succeeds
- **THEN** it returns `{columns, rows, row_count, truncated}` where `rows` is an array of column-keyed objects

#### Scenario: execute returns affected count

- **WHEN** the handler calls `db.execute(sql, params)` and it succeeds
- **THEN** it returns `{rows_affected}` with the number of rows changed

#### Scenario: Positional parameter binding

- **WHEN** a statement uses `$1`/`$2` placeholders with a `params` array
- **THEN** each value is bound positionally as data, not concatenated into the SQL string

### Requirement: Explicit transactions

The system SHALL expose `db.begin()`, `db.commit()`, and `db.rollback()` so a handler can group
statements into an all-or-nothing transaction on the per-request connection.

#### Scenario: Commit persists grouped changes

- **WHEN** a handler calls `db.begin()`, runs writes, then `db.commit()`
- **THEN** the grouped changes are persisted together

#### Scenario: Rollback discards grouped changes

- **WHEN** a handler calls `db.begin()`, runs writes, then `db.rollback()`
- **THEN** none of the grouped changes are persisted

### Requirement: Postgres-to-JSON type mapping

Column values SHALL map to JSON such that any value that does not fit a JS number exactly is
returned as a **string**: `BIGINT`/INT8 and `NUMERIC`/`DECIMAL` come back as strings, while
INT2/INT4 and float columns come back as JSON numbers.

#### Scenario: Large and exact-precision integers as strings

- **WHEN** a query returns a `BIGINT`/INT8 or a `NUMERIC`/`DECIMAL` value
- **THEN** the value is serialized as a JSON string (e.g. `"9007199254740993"`, `"19.99"`)

#### Scenario: Small integers and floats as numbers

- **WHEN** a query returns an INT2/INT4 or a FLOAT4/FLOAT8 value
- **THEN** the value is serialized as a JSON number

#### Scenario: Other typed columns

- **WHEN** a query returns a boolean, text, JSON/JSONB, UUID, date/time, or `BYTEA` value
- **THEN** booleans are JSON booleans, JSON/JSONB pass through as JSON, and UUID, date/time, and `BYTEA` (base64) are strings; a NULL maps to JSON `null`

### Requirement: Row-count truncation

A `db.query` result SHALL be truncated to the configured `max_rows`, with `truncated` flagging
when rows were dropped.

#### Scenario: Result within the limit

- **WHEN** a query returns at most `max_rows` rows
- **THEN** all rows are returned and `truncated` is `false`

#### Scenario: Result exceeds the limit

- **WHEN** a query returns more than `max_rows` rows
- **THEN** the result is capped at `max_rows` rows and `truncated` is `true`

### Requirement: Per-query client-side deadline and statement timeout

Each query SHALL be bounded by a client-side execution deadline anchored to the execution
wall-clock budget, in addition to a best-effort server-side `statement_timeout` set from
`statement_timeout_ms`; a query that runs past the deadline SHALL be cancelled and fail with a
retryable `DB_TIMEOUT`.

#### Scenario: Server-side statement timeout applied

- **WHEN** a connection is established for a request
- **THEN** the per-request `statement_timeout_ms` is applied as a session `SET statement_timeout`

#### Scenario: Hung query bounded by the deadline

- **WHEN** a query runs past the client-side execution deadline (e.g. the server-side timeout was lost through a transaction-mode pooler)
- **THEN** the query is cancelled, the blocking thread is freed, and the call fails with code `DB_TIMEOUT` marked retryable

### Requirement: Db error taxonomy

A failed `db` call SHALL throw a classified error the handler can branch on: driver errors carry
their Postgres `sqlstate`, a connection failure is a retryable `DB_CONNECTION`, and a request
refused by the open circuit breaker is a retryable `DB_CIRCUIT_OPEN`.

#### Scenario: Driver error carries sqlstate and class-based code

- **WHEN** a query fails with a Postgres driver error
- **THEN** the error carries the raw `sqlstate` in its details and a class-derived code (e.g. `DB_SERIALIZATION` for 40001, `DB_CONSTRAINT` for class 23, `DB_QUERY` for class 42), with `DB_ERROR` as the fallback

#### Scenario: Connection failure is retryable

- **WHEN** the database cannot be reached (connect failure or a class 08 drop)
- **THEN** the call fails with code `DB_CONNECTION` marked retryable

#### Scenario: Circuit open fast-fails

- **WHEN** the per-target circuit breaker is open from repeated connect failures
- **THEN** the request fast-fails without a connect attempt with code `DB_CIRCUIT_OPEN` marked retryable

#### Scenario: Operation budget exhausted

- **WHEN** a `db` call would exceed the per-execution `max_ops` budget
- **THEN** the call fails with code `DB_OP_LIMIT`

### Requirement: Operation metering

Every `db` operation SHALL be metered and surfaced in the response `meta.db_requests`.

#### Scenario: Metrics drained into meta

- **WHEN** a handler performs one or more `db` operations
- **THEN** `meta.db_requests` contains one entry per operation with its `action`, `duration_us`, `rows_returned`, `rows_affected`, and `truncated`
