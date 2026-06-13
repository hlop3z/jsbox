# redis Specification

## Purpose

The `redis` capability gives a sandboxed handler a synchronous key/value client
(`redis.get/set/del/incr/expire`) for caches, counters, sessions, and short-lived state
against Redis (or a Redis-compatible store). Because the connection is **operator-supplied**
in `config.redis`, `redis` follows the trusted-connection model shared with `db`/`mail`: it
connects to whatever host the config names with no SSRF guard, so internal Redis instances
work by design. It is opt-in per request, keeps **strings in / strings out** (the script owns
(de)serialization), throws a tagged error on failure, and meters every op into
`meta.redis_requests`. Source of truth: `src/kv.rs`, `src/js/redis.js`, `docs/07-redis.md`.

## Requirements

### Requirement: Opt-in injection via config.redis

The `redis` global SHALL be injected only when the request supplies a `config.redis` block;
otherwise the `redis` global SHALL NOT exist in the handler scope.

#### Scenario: Capability absent without config

- **WHEN** a request omits `config.redis`
- **THEN** the handler observes `typeof redis === "undefined"`

#### Scenario: Capability present with config

- **WHEN** a request supplies a `config.redis` with a `url`
- **THEN** the handler can call `redis.get/set/del/incr/expire`

### Requirement: Trusted operator-supplied connection

The system SHALL connect to the host named in `config.redis.url` without an SSRF or
private-IP guard, applying the configured connect/command `timeout_ms` (default 5000) as the
read and write timeout.

#### Scenario: Internal host is reached

- **WHEN** `config.redis.url` names a private or internal Redis host
- **THEN** the connection is made without an SSRF block (the trusted-connection model)

#### Scenario: Unreachable Redis fails the request

- **WHEN** the Redis host cannot be connected at injection time
- **THEN** the request fails with a retryable error code `REDIS_CONNECTION`

### Requirement: TLS via rediss:// URL

The system SHALL establish a TLS connection when `config.redis.url` uses the `rediss://`
scheme, validated against the system's public certificate authorities.

#### Scenario: TLS connection over rediss://

- **WHEN** `config.redis.url` begins with `rediss://`
- **THEN** the client connects over TLS validated against the usual public CAs

### Requirement: Key/value operation surface

The `redis` global SHALL expose `get(key)`, `set(key, value, opts?)`, `del(key)`,
`incr(key)`, and `expire(key, seconds)`, all synchronous (no `await`), with values coerced to
strings on write and returned as strings on read.

#### Scenario: get returns the stored string

- **WHEN** a handler calls `redis.get(key)` for an existing key
- **THEN** it returns the stored value as a string

#### Scenario: get of a missing key returns null

- **WHEN** a handler calls `redis.get(key)` for a key that does not exist
- **THEN** it returns `null`

#### Scenario: del returns the erased count

- **WHEN** a handler calls `redis.del(key)`
- **THEN** it returns the number of keys removed (0 or 1)

#### Scenario: incr bumps and returns the counter

- **WHEN** a handler calls `redis.incr(key)`
- **THEN** the key's integer value is increased by 1 (starting at 1 if absent) and the new value is returned

### Requirement: TTL and expiry

The system SHALL set a time-to-live in seconds when `set` is called with `opts.ttl`, and
SHALL apply a TTL to an existing key via `expire(key, seconds)`.

#### Scenario: set with ttl expires the key

- **WHEN** a handler calls `redis.set(key, value, { ttl: n })`
- **THEN** the key is written with an `n`-second expiry after which it is removed

#### Scenario: expire returns whether the key existed

- **WHEN** a handler calls `redis.expire(key, seconds)`
- **THEN** it returns `true` if the key existed and the TTL was set, otherwise `false`

### Requirement: Tagged error on failure

A Redis driver or usage failure SHALL throw a JavaScript `Error` carrying a classified code
(`REDIS_TIMEOUT`, `REDIS_CONNECTION`, or `REDIS_ERROR`) with an `owner` and `retryable` hint,
distinct from the never-throw `api` model.

#### Scenario: Command failure throws

- **WHEN** a `redis` call fails at the driver or command level
- **THEN** the call throws an `Error` whose tag the engine classifies (e.g. `REDIS_TIMEOUT` for a timed-out command, retryable and operator-owned)

#### Scenario: Uncaught error is classified

- **WHEN** the handler does not catch a thrown `redis` error
- **THEN** the engine surfaces it as a labeled capability error in the response envelope

### Requirement: Per-call metering

Every `redis` call SHALL be recorded and surfaced in `meta.redis_requests`, capturing the
action, duration, value size in bytes, and whether a `get` found the key.

#### Scenario: Operation metered with size and hit

- **WHEN** a handler issues a `redis.get(key)` that finds a value
- **THEN** `meta.redis_requests` gains an entry with the action, duration, the value's byte size, and `hit` true

#### Scenario: Miss recorded as no hit

- **WHEN** a handler issues a `redis.get(key)` for a missing key
- **THEN** the recorded entry has `hit` false and a byte size of 0

### Requirement: Operation cap

Each `redis` call SHALL count against the per-execution `max_ops` budget, and a call made
after the budget is exhausted SHALL fail.

#### Scenario: Op budget exhausted

- **WHEN** a handler issues a `redis` call after `max_ops` external operations are already used
- **THEN** the call fails with code `REDIS_OP_LIMIT` (non-retryable, developer-owned)
