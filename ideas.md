# Ideas: `virtual-host` — a programmatic container for cloud-native resources

## `auth` — OIDC / IAM identity capability (Zitadel, Keycloak, Auth0, …)

**Goal:** the client passes a bearer token, the script gets the authenticated
user's claims — `const user = auth.user_info(ctx.token)`.

### Fit with the architecture

This is a standard capability, same shape as the existing six (`http`, `db`,
`mail`, `s3`, `redis`, `amq`). The issuer URL is **operator-supplied** in
`config.auth`, so it follows the **trusted model** (`db`/`mail`): no SSRF guard,
because the operator names the host — not the script. The client's token is just
a string placed into the `Authorization` header toward that operator-named host.

Opt-in per request: no `config.auth` block → `typeof auth === "undefined"`.

### Validation: delegate to the IAM

`GET {issuer}/userinfo` with `Authorization: Bearer <token>`. A bad token gets a
`401` back — the userinfo endpoint _is_ the validation oracle. One HTTP call,
no JWT parsing, no JWKS, **no crypto**. Essentially `http.rs`'s `execute_http`
minus the SSRF block.

This deliberately avoids local JWT verification (JWKS fetch + RS256 verify),
which would pull in a second crypto stack (`jsonwebtoken` defaults to `ring`,
which CLAUDE.md forbids next to the chosen `aws-lc-rs` provider) and cross-request
JWKS caching. Delegation keeps the capability stateless and crypto-free; revisit
only if the per-request round-trip becomes a measured problem.

### Proposed JS surface

```js
const user = auth.user_info(token); // claims: { sub, email, name, ... }
const meta = auth.introspect(token); // RFC 7662: { active, scope, exp, ... }
```

`introspect` needs client credentials — drop `client_id`/`client_secret` into
`$sys.secrets` (opaque handles, plaintext never enters the JS heap; see
`sys.rs`).

### Implementation footprint (~7 touches)

- new `src/auth.rs` — model on `mail.rs` (trusted config) + reuse the
  reqwest-blocking call from `http.rs`; ~150–200 lines, mostly lint-gauntlet
  boilerplate.
- new `src/js/auth.js` — wrapper exposing `auth.user_info` / `auth.introspect`.
- `mod auth;` in `main.rs`.
- `engine.rs`: `auth_config` in `ExecParams`, a branch in `inject_apis`, an
  `AuthMetric` slot in `Collectors`/`ExecResult`.
- `handler.rs`: `auth: Option<AuthConfig>` in `RequestConfig`, `auth_requests`
  in `Meta`.
- `errors.rs`: add `ErrorSource::Auth` + its `parse` arm.
- docs in `docs/`.

**Estimate:** ≈ half a day to a day for someone fluent in the repo (lint gauntlet
+ Docker-only build loop are the main friction, not the logic).

### Decisions (stateless + scalable)

Guiding rule: **no shared/cross-request state** — only request-scoped. That keeps
every instance interchangeable and horizontally scalable.

**1. Cache per-token within a request — yes, in the JS wrapper.** The wrapper is
`eval`'d into a fresh `Context` per request, so a closure-local `cache` resets
automatically — request-scoped, zero Rust state, no cross-request leakage. A
cached second call makes no network round-trip, so it correctly consumes no
`max_ops` slot.

```js
(function () {
  var cache = {}; // fresh per request
  globalThis.auth = {
    user_info: function (token) {
      if (cache[token]) return cache[token];
      var res = JSON.parse(__auth("user_info", token));
      cache[token] = res; // memoize resolved value
      return res;
    },
  };
})();
```

**Explicitly avoid** a process-global token cache: it's shared state (hurts
scaling, needs invalidation) and keeps serving identity after a token is
revoked — a security hazard.

**2. Auto-inject `ctx.user` — no, keep it explicit.** Auto-injection would force
a userinfo call into request assembly for every token-bearing request, taxing
latency and coupling context-building to a network dependency. The explicit
`auth.user_info(ctx.token)` is lazy (pay only when needed), routes through the
existing capability + metering + error machinery, and adds no surface to
`handler.rs`/`engine.rs`. Matches the opt-in capability philosophy.

**3. Error mapping — split by category, owner by who controls the input.** The
bearer token is **caller-supplied** (arrives via `ctx`), so an invalid token is
the caller's, not the developer's. A `401` is expected business flow → return it
**in-band** (mirroring `api`'s never-throw model) so handlers branch without
`try/catch`. Reserve throwing a tagged `__jsbox` capability error (like
`db`/`mail`) for infra failures the handler can't act on.

| IAM outcome                          | Code                 | Owner    | Retryable | Surface                          |
| ------------------------------------ | -------------------- | -------- | --------- | -------------------------------- |
| `200`                                | —                    | —        | —         | return `{ ok: true, claims }`    |
| `401` / `403` (invalid/expired/scope)| `AUTH_INVALID_TOKEN` | Caller   | no        | **in-band** `{ ok: false, status }` |
| `5xx` / timeout / connect            | `AUTH_UNAVAILABLE`   | Operator | **yes**   | **throw** tagged                 |
| other `4xx` (misconfig)              | `AUTH_REQUEST`       | Operator | no        | throw tagged                     |

Handler usage: `const u = auth.user_info(ctx.token); if (!u.ok) return json(null, { code: 'unauthorized' });`
