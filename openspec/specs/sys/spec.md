# sys Specification

## Purpose

The `$sys` capability is the always-on runtime standard library for the sandbox: one
`$`-prefixed global grouping pure, zero-I/O helpers. `$sys.crypto` (hashing, HMAC, UUID,
encoding) and `$sys.date` (now, parse, math, diff, formatting) are always injected — pure
like `$`/Decimal, no config and no per-op metering. Two further surfaces, `$sys.env` (plain
operator settings) and `$sys.secrets` (operator credentials), are populated only when a
`config.sys` block is supplied. The defining guarantee is that `$sys.secrets` values are
opaque handles whose plaintext never enters the JS heap. Rationale: `src/sys.rs`,
`src/js/sys.js`, and `docs/09-sys.md`.

## Requirements

### Requirement: Pure helpers always injected

The system SHALL always expose `$sys.crypto` and `$sys.date` on every execution, without any
configuration and without per-operation metering, because they perform no I/O.

#### Scenario: Crypto and date available with no config

- **WHEN** a request supplies no `config.sys` block
- **THEN** `$sys.crypto` and `$sys.date` are defined and callable in the handler

#### Scenario: Env and secrets default to empty without config

- **WHEN** a request supplies no `config.sys` block
- **THEN** `$sys.env` is an empty object and `$sys.secrets` is an empty object

### Requirement: Crypto hashing and UUID

The `$sys.crypto` surface SHALL provide SHA-256 and SHA-512 hashing of a string (hex-encoded)
and `uuid()` returning a fresh random v4 UUID.

#### Scenario: SHA hashing is deterministic and hex-encoded

- **WHEN** the handler calls `$sys.crypto.sha256(s)` or `$sys.crypto.sha512(s)` on the same string
- **THEN** it returns the same hex digest every time for that input

#### Scenario: UUID is random per call

- **WHEN** the handler calls `$sys.crypto.uuid()` twice
- **THEN** each call returns a distinct UUID string

### Requirement: Crypto HMAC signing

The `$sys.crypto.hmac(algo, key, msg, encoding?)` op SHALL compute an HMAC over `msg` using
`algo` of `"sha256"` or `"sha512"`, with the key being either a plain string or a secret
handle, encoded as `"hex"` (default), `"base64"`, or `"base64url"`.

#### Scenario: HMAC with a plain string key

- **WHEN** the handler calls `$sys.crypto.hmac("sha256", "my-key", "msg")`
- **THEN** it returns the hex-encoded digest, and supplying `"base64"`/`"base64url"` changes only the encoding

#### Scenario: Unsupported algorithm rejected

- **WHEN** the handler calls `$sys.crypto.hmac` with an `algo` other than `"sha256"` or `"sha512"`
- **THEN** the call throws a developer/script error

### Requirement: Crypto encoders

The `$sys.crypto` surface SHALL provide `base64`, `base64url`, `hex`, and `url` codecs, each
with `.encode()` and `.decode()` over UTF-8 strings.

#### Scenario: Encode then decode round-trips

- **WHEN** the handler calls `$sys.crypto.base64.encode(s)` then `.decode()` on the result
- **THEN** it recovers the original string, and `base64url`/`hex`/`url` behave likewise

#### Scenario: Invalid input rejected

- **WHEN** the handler decodes a value that is not valid for the codec (bad base64/hex, non-UTF-8 bytes)
- **THEN** the call throws a developer/script error

### Requirement: Date helpers

The `$sys.date` surface SHALL provide `now()` and `parse(input)` producing date objects with
`add`/`sub` (fixed-length `weeks`/`days`/`hours`/`minutes`/`seconds`/`ms` deltas), `diff`,
`iso()`, and `unix()`, normalizing all inputs to UTC.

#### Scenario: Parse multiple input forms to UTC

- **WHEN** the handler calls `$sys.date.parse` with an RFC 3339 string, a `YYYY-MM-DD` string, or epoch millis
- **THEN** it returns a date object normalized to UTC, and unparseable input throws

#### Scenario: Date math and diff

- **WHEN** the handler calls `.add({days:3, hours:12})`, `.sub({weeks:1})`, or `a.diff(b)`
- **THEN** `add`/`sub` return a shifted date and `diff` returns `{total_ms, total_seconds, days, hours, minutes, seconds}`

#### Scenario: Date serializes as ISO

- **WHEN** a date object is returned via `json(...)` or stringified
- **THEN** it serializes to its RFC 3339 ISO string (`Z`, UTC)

### Requirement: Env is plain operator config

When `config.sys.env` is supplied, the system SHALL expose those values at `$sys.env` as
plain, readable, returnable values.

#### Scenario: Env values readable and returnable

- **WHEN** `config.sys.env` defines `{ "REGION": "us-east-1" }`
- **THEN** `$sys.env.REGION` is `"us-east-1"`, a missing key is `undefined`, and the value may be returned in `data`

### Requirement: Secrets are opaque, use-not-extract handles

When `config.sys.secrets` is supplied, the system SHALL expose each secret at `$sys.secrets`
as an opaque, frozen handle carrying only the secret's name; the plaintext SHALL never enter
the JS heap. The only operation that resolves a handle to its plaintext is HMAC in the key
position, whose output is a one-way digest.

#### Scenario: Coercion yields only a placeholder

- **WHEN** the handler coerces `$sys.secrets.NAME` via `String(...)`, a template literal, `JSON.stringify`, or returns it in `data`
- **THEN** the result is the placeholder `"[secret:NAME]"`, never the secret's plaintext

#### Scenario: Handle usable solely as an HMAC key

- **WHEN** the handler passes `$sys.secrets.NAME` as the `key` argument to `$sys.crypto.hmac`
- **THEN** the digest is computed using the Rust-side plaintext, and no plaintext crosses back into JS

#### Scenario: Handle rejected by hash, encode, and HMAC message

- **WHEN** the handler passes a secret handle to `sha256`/`sha512`, any codec `encode`/`decode`, or as the HMAC `msg`
- **THEN** the call throws a developer/script error rather than echoing or transforming the plaintext

#### Scenario: Unknown secret reference rejected

- **WHEN** an HMAC call references a `key_ref` name that was not configured in `config.sys.secrets`
- **THEN** the call throws a developer/script error
