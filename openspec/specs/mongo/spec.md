# mongo Specification

## Purpose

The `mongo` capability gives a handler a document-database client inside the QuickJS
sandbox: `mongo.find`/`findOne`/`insertOne`/`insertMany`/`updateOne`/`updateMany`/
`deleteOne`/`deleteMany`/`count`/`aggregate`. The connection is **operator-supplied** in
`config.mongo`, so this capability is trusted (it connects to whatever host the config
names, with no SSRF guard) — the same trust model as `db`/`mail`, and unlike the
script-controlled `api` capability. It is admitted as a first-class capability (not routed
over `api`) per the capability-admission gate in `docs-sys/rfc.md` §3.5: a trusted internal
target, document type-fidelity that JSON-over-HTTP would lose, and a fit to the bounded
single request→response model. Like `db` it is **async** (Tier 2 resilience: a client-side
per-operation deadline anchored to the execution wall-clock). This spec defines opt-in, the
JS surface, the BSON→JSON type-mapping rule, result limits, the client-side deadline, the
mongo-error taxonomy, and metering. Source of truth: `src/mongo.rs`, `src/js/mongo.js`,
`docs/design/resilience.md`, and the canonical async template `src/db.rs`.

## Requirements

### Requirement: Opt-in via config.mongo

The `mongo` global SHALL exist only when the request supplies a `config.mongo` block; absent
that block the global is undefined.

#### Scenario: Config present injects the global

- **WHEN** a request includes a `config.mongo` block and the handler references `mongo`
- **THEN** `mongo` is a defined object exposing `find`, `findOne`, `insertOne`, `insertMany`, `updateOne`, `updateMany`, `deleteOne`, `deleteMany`, `count`, and `aggregate`

#### Scenario: Config absent leaves the global undefined

- **WHEN** a request has no `config.mongo` block
- **THEN** `typeof mongo === "undefined"` inside the handler

### Requirement: Trusted operator-supplied connection (no SSRF guard)

The capability SHALL connect to the host/port named in `config.mongo` without any
private/internal IP block or host allowlist, because the connection target is
operator-supplied rather than script-controlled.

#### Scenario: Connects to operator-named host

- **WHEN** `config.mongo` names an internal or private-network host
- **THEN** the capability attempts the connection without rejecting it as a private/internal address

#### Scenario: Connection config fields

- **WHEN** `config.mongo` is provided
- **THEN** it accepts `host`, `port` (default 27017), `username`, `password`, `database`, `auth_source` (default `admin`), `tls` (default false), `ca_cert` (optional PEM path), `op_timeout_ms` (default 5000), and `max_docs` (default 1000)

### Requirement: TLS reusing the shared provider

The capability SHALL connect over TLS when `config.mongo.tls` is true, reusing the
process-wide `aws-lc-rs` rustls provider, and SHALL accept an optional `ca_cert` PEM path
for a self-hosted database with a private certificate authority.

#### Scenario: TLS connection

- **WHEN** `config.mongo.tls` is true
- **THEN** the connection to the database is established over TLS using the shared crypto provider

#### Scenario: Custom CA certificate

- **WHEN** `config.mongo.tls` is true and `config.mongo.ca_cert` names a PEM file
- **THEN** that CA is used to verify the server certificate; omitting `ca_cert` relies on the bundled webpki roots

### Requirement: Read operations

The system SHALL expose `mongo.find(collection, filter?, options?)`,
`mongo.findOne(collection, filter?)`, `mongo.count(collection, filter?)`, and
`mongo.aggregate(collection, pipeline)`, passing the caller's `filter`/`pipeline`/`options`
as data to the driver (never string-interpolated into a query language).

#### Scenario: find returns a result shape

- **WHEN** the handler calls `mongo.find(collection, filter, options)` and it succeeds
- **THEN** it returns `{docs, count, truncated}` where `docs` is an array of documents

#### Scenario: findOne returns a document or null

- **WHEN** the handler calls `mongo.findOne(collection, filter)`
- **THEN** it returns the first matching document, or `null` when nothing matches

#### Scenario: find honors limit, skip, sort, and projection

- **WHEN** `options` supplies `limit`, `skip`, `sort`, or `projection`
- **THEN** each is applied to the query as the matching driver option

#### Scenario: count returns a number

- **WHEN** the handler calls `mongo.count(collection, filter)`
- **THEN** it returns the number of matching documents

#### Scenario: aggregate returns documents

- **WHEN** the handler calls `mongo.aggregate(collection, pipeline)` with a stage pipeline
- **THEN** it returns `{docs, count, truncated}` for the pipeline's output documents

### Requirement: Write operations

The system SHALL expose `mongo.insertOne`/`insertMany`/`updateOne`/`updateMany`/
`deleteOne`/`deleteMany`, returning a result describing what changed.

#### Scenario: insertOne returns the inserted id

- **WHEN** the handler calls `mongo.insertOne(collection, doc)` and it succeeds
- **THEN** it returns `{inserted_id}` with the new document's id as a string

#### Scenario: insertMany returns the inserted count

- **WHEN** the handler calls `mongo.insertMany(collection, docs)` and it succeeds
- **THEN** it returns `{inserted_count}` with the number of documents inserted

#### Scenario: update returns matched and modified counts

- **WHEN** the handler calls `mongo.updateOne`/`updateMany(collection, filter, update)`
- **THEN** it returns `{matched, modified}` with the matched and modified document counts

#### Scenario: delete returns the deleted count

- **WHEN** the handler calls `mongo.deleteOne`/`deleteMany(collection, filter)`
- **THEN** it returns `{deleted}` with the number of documents removed

### Requirement: BSON-to-JSON type mapping

Document values SHALL map to JSON such that any value that does not fit a JS number exactly
is returned as a **string** — mirroring the `db` rule. `Int32` and `Double` come back as JSON
numbers; `Int64` and `Decimal128` come back as strings; `ObjectId` as its 24-character hex
string; `Date` as an RFC 3339 string; `Binary` as base64; booleans, strings, null, nested
documents, and arrays pass through structurally.

#### Scenario: Large and exact-precision numbers as strings

- **WHEN** a document field is an `Int64` or a `Decimal128`
- **THEN** the value is serialized as a JSON string (e.g. `"9007199254740993"`, `"19.99"`)

#### Scenario: Small integers and doubles as numbers

- **WHEN** a document field is an `Int32` or a `Double`
- **THEN** the value is serialized as a JSON number

#### Scenario: ObjectId, Date, and Binary as strings

- **WHEN** a document field is an `ObjectId`, a `Date`, or a `Binary`
- **THEN** the `ObjectId` is its hex string, the `Date` is an RFC 3339 string, and the `Binary` is base64

#### Scenario: Structural values pass through

- **WHEN** a document field is a boolean, string, null, nested document, or array
- **THEN** it is serialized as the corresponding JSON value

### Requirement: Result-count truncation

A `mongo.find`/`aggregate` result SHALL be truncated to the configured `max_docs`, with
`truncated` flagging when documents were dropped.

#### Scenario: Result within the limit

- **WHEN** a query returns at most `max_docs` documents
- **THEN** all documents are returned and `truncated` is `false`

#### Scenario: Result exceeds the limit

- **WHEN** a query returns more than `max_docs` documents
- **THEN** the result is capped at `max_docs` documents and `truncated` is `true`

### Requirement: Per-operation client-side deadline

Each `mongo` operation SHALL be bounded by a client-side execution deadline anchored to the
execution wall-clock budget, in addition to a best-effort server-side operation time limit
set from `op_timeout_ms`; an operation that runs past the deadline SHALL be abandoned and
fail with a retryable `MONGO_TIMEOUT`, freeing the blocking thread.

#### Scenario: Server-side operation timeout applied

- **WHEN** an operation is issued
- **THEN** the per-request `op_timeout_ms` is applied as the operation's server-side max time

#### Scenario: Hung operation bounded by the deadline

- **WHEN** an operation runs past the client-side execution deadline
- **THEN** it is abandoned, the blocking thread is freed, and the call fails with code `MONGO_TIMEOUT` marked retryable

### Requirement: Mongo error taxonomy

A failed `mongo` call SHALL throw a classified error the handler can branch on: a connection
failure is a retryable `MONGO_CONNECTION` owned by the operator; a duplicate-key / write
constraint is a non-retryable `MONGO_WRITE` owned by the developer; a malformed
filter/pipeline/update is a non-retryable `MONGO_QUERY` owned by the developer; the deadline
is a retryable `MONGO_TIMEOUT`; with `MONGO_ERROR` as the retryable fallback.

#### Scenario: Connection failure is retryable

- **WHEN** the database cannot be reached or authentication fails
- **THEN** the call fails with code `MONGO_CONNECTION` marked retryable and owned by the operator

#### Scenario: Duplicate key is a developer write error

- **WHEN** a write violates a unique index (duplicate key)
- **THEN** the call fails with code `MONGO_WRITE`, non-retryable, owned by the developer

#### Scenario: Malformed query is a developer error

- **WHEN** a filter, update, or aggregation pipeline is malformed
- **THEN** the call fails with code `MONGO_QUERY`, non-retryable, owned by the developer

#### Scenario: Operation budget exhausted

- **WHEN** a `mongo` call would exceed the per-execution `max_ops` budget
- **THEN** the call fails with code `MONGO_OP_LIMIT`, non-retryable, owned by the developer

### Requirement: Operation metering

Every `mongo` operation SHALL count as exactly one operation against `max_ops` and be
surfaced in the response `meta.mongo_requests`.

#### Scenario: Metrics drained into meta

- **WHEN** a handler performs one or more `mongo` operations
- **THEN** `meta.mongo_requests` contains one entry per operation with its `action`, `duration_us`, documents returned/affected, and `truncated`
