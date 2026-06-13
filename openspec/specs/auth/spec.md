# auth Specification

## Purpose

The `auth` capability lets a handler verify a caller-supplied OIDC bearer token against an
operator-named identity provider (IAM) and read the resulting claims. It exposes
`auth.user_info(token)` (OIDC userinfo) and `auth.introspect(token)` (RFC 7662). Token
validation is delegated entirely to the IAM — there is no local JWT/JWKS/crypto. The trust
model mirrors `db`/`mail` (operator-supplied issuer, not script-controlled), and the
error surface is hybrid: an invalid/expired caller token returns in-band, while infra
failures throw a tagged capability error. Rationale: `src/auth.rs`, `src/js/auth.js`,
`docs/10-auth.md`.

## Requirements

### Requirement: Opt-in injection via config.auth

The `auth` global SHALL exist only when the request supplies a `config.auth` block; with no
such block the global is absent (`typeof auth === "undefined"`).

#### Scenario: Config present

- **WHEN** a request includes `config.auth` with an `issuer`
- **THEN** the handler can call `auth.user_info` / `auth.introspect`

#### Scenario: Config absent

- **WHEN** a request omits `config.auth`
- **THEN** `typeof auth` is `"undefined"` and no IAM client is created

### Requirement: Trusted operator-supplied target (no SSRF guard)

The issuer and endpoints SHALL be taken from operator-supplied `config.auth` (matching the
`db`/`mail` trust model), so no SSRF / private-IP block is applied; the caller's token is
placed verbatim into the `Authorization` header toward the operator-named host.

#### Scenario: Issuer is operator-controlled

- **WHEN** `config.auth.issuer` names an internal or private host
- **THEN** the request is allowed (no private-IP block), because the host is operator-supplied, not script-controlled

### Requirement: Endpoint resolution via OIDC discovery with explicit overrides

The capability SHALL resolve the userinfo and introspection endpoints by reading
`{issuer}/.well-known/openid-configuration`, unless an explicit `userinfo_url` /
`introspect_url` override is supplied, in which case discovery is skipped for that endpoint.

#### Scenario: Discovery used

- **WHEN** only `issuer` is configured and `user_info` is called
- **THEN** the engine fetches the discovery document and uses its `userinfo_endpoint`

#### Scenario: Explicit override

- **WHEN** `userinfo_url` (or `introspect_url`) is set
- **THEN** that URL is used directly and no discovery request is made for that endpoint

#### Scenario: Endpoint not published

- **WHEN** discovery succeeds but the issuer exposes no `userinfo_endpoint` (or `introspection_endpoint`) and no override is set
- **THEN** the call throws an `AUTH_REQUEST` capability error

### Requirement: user_info validates via the IAM userinfo endpoint

`auth.user_info(token)` SHALL perform `GET {userinfo}` with `Authorization: Bearer <token>`
and return `{ ok: true, claims }` on a 2xx response — validation is delegated to the IAM with
no local crypto.

#### Scenario: Valid token

- **WHEN** the IAM returns 2xx with a claims body
- **THEN** the call returns `{ ok: true, claims: { ... } }`

### Requirement: introspect uses RFC 7662 with operator client credentials

`auth.introspect(token)` SHALL perform an RFC 7662 `POST {introspect}` with the operator's
`client_id`/`client_secret` as HTTP Basic auth and a `token=` form body, returning
`{ ok: true, claims }` on 2xx (the script reads `claims.active`). When `client_id` is empty it
SHALL throw rather than call the IAM.

#### Scenario: Introspection succeeds

- **WHEN** `client_id`/`client_secret` are configured and the IAM returns 2xx
- **THEN** the call returns `{ ok: true, claims: { active, scope, exp, ... } }`

#### Scenario: Missing client credentials

- **WHEN** `auth.introspect` is called without a configured `client_id`
- **THEN** the call throws an `AUTH_REQUEST` capability error (operator setup mistake, not a caller error)

### Requirement: Hybrid error surface (invalid token in-band, infra failures throw)

An invalid/expired caller token SHALL be returned in-band as `{ ok: false, status, code:
"AUTH_INVALID_TOKEN" }` (never thrown), while infrastructure failures the handler cannot act
on SHALL throw a tagged capability error.

#### Scenario: Invalid or expired token (userinfo)

- **WHEN** the userinfo endpoint returns 401 or 403
- **THEN** the call returns `{ ok: false, status, code: "AUTH_INVALID_TOKEN" }` and does not throw

#### Scenario: Issuer unavailable

- **WHEN** the IAM returns 5xx or the request times out / fails transport
- **THEN** the call throws a tagged `AUTH_UNAVAILABLE` capability error marked `retryable: true`, owned by the operator

#### Scenario: Deterministic request failure

- **WHEN** the IAM returns an unexpected non-2xx status (other than 401/403/5xx) or a malformed body
- **THEN** the call throws a non-retryable `AUTH_REQUEST` capability error owned by the operator

### Requirement: Per-token within-request caching

A repeated `auth` call for the same action and token within one request SHALL return the
remembered result without a new network round-trip, consuming no additional operation budget;
the cache SHALL be request-scoped with no process-global state.

#### Scenario: Repeated lookup is cached

- **WHEN** `auth.user_info(token)` is called twice with the same token in one request
- **THEN** the IAM is contacted once and the second call returns the cached result, adding no metric line and no op count

#### Scenario: Cache does not leak across requests

- **WHEN** a later request runs in a fresh context
- **THEN** it observes an empty cache and performs its own IAM round-trip

### Requirement: Metering into meta.auth_requests and op-limit enforcement

Each non-cached `auth` call SHALL be metered (action, issuer host only, IAM status, duration)
into `meta.auth_requests`, and each call SHALL be gated by the per-execution `max_ops` budget.

#### Scenario: Call is recorded

- **WHEN** a non-cached `auth` call completes
- **THEN** `meta.auth_requests` gains a line with the action, the issuer host (no path/query), the IAM status, and the duration

#### Scenario: Operation cap exceeded

- **WHEN** an `auth` call would exceed `max_ops`
- **THEN** the call fails with an `AUTH_OP_LIMIT` error before contacting the IAM
