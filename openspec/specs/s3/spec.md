# s3 Specification

## Purpose

The `s3` capability gives a handler an `s3` global for **presigning object-store URLs** and
running a few server-side store operations against an S3-compatible bucket (AWS S3, Cloudflare
R2, Backblaze B2, MinIO). Presigning is pure `SigV4` crypto — the server computes a signed URL
and hands it back so the browser uploads/downloads directly, never streaming bytes through
jsbox; only `usage` and `delete` actually connect to the store. Endpoint and credentials are
operator-supplied in `config.s3`. Rationale: `src/s3.rs`, `src/js/s3.js`, `docs/06-s3.md`.

## Requirements

### Requirement: Opt-in injection via config.s3

The `s3` global SHALL exist only when the request supplies a `config.s3` block; with no such
block `s3` is undefined and no operation is possible.

#### Scenario: Injected when configured

- **WHEN** a request includes a `config.s3` block (endpoint, region, bucket, access_key, secret_key)
- **THEN** the handler can call `s3` and presign/usage/delete operations are available

#### Scenario: Absent without config

- **WHEN** a request omits `config.s3`
- **THEN** `typeof s3 === "undefined"` and no S3 operation can run

### Requirement: Presign operations are pure crypto with no network

The presign operations SHALL compute a `SigV4`-signed URL or POST policy locally and return it
without ever connecting to the object store. This covers `s3.upload_url`, `s3.download_url`,
`s3.upload_form`, and the general `s3.sign_url`.

#### Scenario: Sign an upload URL

- **WHEN** the handler calls `s3.upload_url({ key })`
- **THEN** it returns `{ url, method: "PUT", expires }` where `url` carries an `X-Amz-Signature`, computed with no network request

#### Scenario: Sign a download URL

- **WHEN** the handler calls `s3.download_url({ key })`
- **THEN** it returns `{ url, method: "GET", expires }` with no network request

#### Scenario: General signing helper

- **WHEN** the handler calls `s3.sign_url({ method, key })` with method `PUT`, `GET`, `HEAD`, or `DELETE` (default `PUT`)
- **THEN** it returns a signed URL for that method; an unsupported method is rejected

### Requirement: Presigned POST form upload with config-only size cap

`s3.upload_form` SHALL return a `SigV4` browser POST policy whose `content-length-range`
condition is bounded by `config.s3.max_upload_size`; the size cap SHALL come only from operator
config and never from the script payload.

#### Scenario: Form policy carries the configured cap

- **WHEN** the handler calls `s3.upload_form({ key })` and `config.s3.max_upload_size` is set
- **THEN** it returns `{ url, fields, max_bytes, expires }` where `max_bytes` equals the configured cap and the script supplies no size

#### Scenario: Missing size cap is rejected

- **WHEN** the handler calls `s3.upload_form` but `config.s3.max_upload_size` is unset (`0`)
- **THEN** the call fails with an `S3_ERROR` (the cap is required for `upload_form`)

### Requirement: Link lifetime defaulted and clamped

Each presigned link's lifetime SHALL come from the call's `expires` (seconds) or the configured
default `config.s3.expires`, clamped to `[1, config.s3.max_expires]` (the `SigV4` maximum is
604800 seconds / 7 days).

#### Scenario: Default lifetime applied

- **WHEN** a presign call omits `expires`
- **THEN** the link uses `config.s3.expires` (default 900 seconds)

#### Scenario: Requested lifetime clamped to the cap

- **WHEN** a presign call requests an `expires` greater than `config.s3.max_expires`
- **THEN** the effective lifetime is clamped down to `config.s3.max_expires`

### Requirement: Operator-supplied endpoint under the http SSRF guard

The object-store endpoint and credentials SHALL be operator-supplied in `config.s3` (trusted,
like `db`/`mail`) and the endpoint host SHALL pass the same SSRF guard as `http`: only `http`/`https`
schemes are accepted, and localhost / private / internal addresses are blocked so a presigned
URL can never name a local or internal target (relaxed only in `debug` mode).

#### Scenario: Non-http scheme rejected

- **WHEN** `config.s3.endpoint` uses a scheme other than `http://` or `https://`
- **THEN** the operation fails and no URL is produced

#### Scenario: Private or internal host blocked

- **WHEN** `config.s3.endpoint` resolves to localhost or a private/internal address and the server is not in `debug` mode
- **THEN** the operation is blocked by the SSRF guard

#### Scenario: Public store supported

- **WHEN** `config.s3.endpoint` names a public S3-compatible store (AWS S3, Cloudflare R2, Backblaze B2, or a publicly reachable MinIO), with `path_style` selecting virtual-hosted or path addressing
- **THEN** signing and store operations target that endpoint

### Requirement: Usage listing connects to the store and paginates

`s3.usage({ prefix })` SHALL connect to the store, sign and send `ListObjectsV2` requests, page
through continuation tokens summing each object's size, and return `{ prefix, bytes, objects }`;
each list page SHALL count as one operation against `max_ops`.

#### Scenario: Totals a prefix

- **WHEN** the handler calls `s3.usage({ prefix: "user-a/" })`
- **THEN** it returns `{ prefix, bytes, objects }` totalling every object under the prefix (an omitted prefix totals the whole bucket)

#### Scenario: Large listing hits the op limit

- **WHEN** a usage scan needs more list pages than the remaining `max_ops` budget allows
- **THEN** it stops with an `S3_OP_LIMIT` error rather than running unbounded

#### Scenario: Store unreachable or errors

- **WHEN** the store is unreachable or returns a non-2xx status during a list request
- **THEN** the call fails with a retryable `S3_UPSTREAM` error

### Requirement: Destructive delete gated behind allow_delete

Object deletion — `s3.delete` and presigning a `DELETE` URL — SHALL be disabled unless the
operator sets `config.s3.allow_delete = true`, even when `s3` is otherwise configured.

#### Scenario: Delete blocked by default

- **WHEN** the handler calls `s3.delete({ key })` (or signs a `DELETE` URL) and `config.s3.allow_delete` is not `true`
- **THEN** the call throws an `S3_FORBIDDEN` error and no object is removed

#### Scenario: Delete allowed when opted in

- **WHEN** `config.s3.allow_delete = true` and the handler calls `s3.delete({ key })`
- **THEN** it signs and sends a `DELETE` to the store and returns `{ key, deleted: true }` (idempotent — a missing key still succeeds)

### Requirement: Operations metered into meta.s3_requests

Every `s3` operation SHALL be recorded as a metric and drained into the response
`meta.s3_requests`, carrying at least the action, signed method, duration, and link lifetime
(with byte/object counts for `usage` pages).

#### Scenario: Presign recorded

- **WHEN** a handler presigns a URL
- **THEN** `meta.s3_requests` includes an entry with its `action`, `method`, and `expires`

#### Scenario: Usage page records bytes and objects

- **WHEN** a `usage` scan reads a list page
- **THEN** that page is recorded in `meta.s3_requests` with its `bytes` and `objects` counts
