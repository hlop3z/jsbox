# 10. `auth` — Who Is This Person? 🪪

[← Back to the guide](README.md)

`auth` checks a login token for you. Your app's caller hands you a **bearer token**;
`auth` asks your identity server (the "IAM" — Zitadel, Keycloak, Auth0, …) _"is this
real, and who is it?"_ and hands you back the person's details.

It never does any tricky crypto itself. It just **asks the IAM** — the identity server
is the one true judge of whether a token is good.

## Turn it on first 🔑

Give the robot your identity server's address with `config.auth`:

```json
{
  "config": {
    "auth": {
      "issuer": "https://login.example.com"
    }
  }
}
```

| Setting          | What it means                                            | Default      |
| ---------------- | -------------------------------------------------------- | ------------ |
| `issuer`         | Your identity server's base address                      | (must set)   |
| `userinfo_url`   | Exact "who is this" address (skip auto-discovery)        | _discovered_ |
| `introspect_url` | Exact "is this token live" address (skip auto-discovery) | _discovered_ |
| `client_id`      | App id — only needed for `introspect`                    | `""`         |
| `client_secret`  | App secret — only needed for `introspect`                | `""`         |
| `timeout_ms`     | How long to wait before giving up                        | `10000`      |

No `config.auth` → `auth` is turned off (`typeof auth === "undefined"`).

> **Auto-discovery.** You usually only set `issuer`. The robot finds the right
> endpoints by reading `{issuer}/.well-known/openid-configuration` — the standard
> OIDC "menu" every provider publishes. Set `userinfo_url` / `introspect_url` only if
> you want to skip that lookup.

## Check a token: `auth.user_info` 🙋

```js
function handler(ctx) {
  const u = auth.user_info(ctx.token);
  if (!u.ok) {
    return json(null, { code: "unauthorized" }); // bad/expired token
  }
  // u.claims = { sub, email, name, ... }
  return json({ hello: u.claims.email }, null);
}
```

`auth.user_info` gives you back one of two shapes:

| You get                                                  | Means                                 |
| -------------------------------------------------------- | ------------------------------------- |
| `{ ok: true, claims: { sub, email, … } }`                | Good token! `claims` is who they are. |
| `{ ok: false, status: 401, code: "AUTH_INVALID_TOKEN" }` | Bad, expired, or under-scoped token.  |

**It does _not_ throw** for a bad token — an unknown person knocking on the door is
normal, everyday business, so you just check `u.ok` and branch. (This mirrors how
`api` never throws.) See [the in-band vs. throw idea below](#why-bad-tokens-dont-throw).

## Look closer: `auth.introspect` 🔎

`introspect` is the official "is this token still alive?" question (the OAuth standard
calls it RFC 7662). It can see things `user_info` can't, like whether a token was
**revoked** or when it **expires** — but it needs your app's own `client_id` and
`client_secret`:

```json
{
  "config": {
    "auth": {
      "issuer": "https://login.example.com",
      "client_id": "my-api",
      "client_secret": "shhh"
    }
  }
}
```

```js
function handler(ctx) {
  const r = auth.introspect(ctx.token);
  // r.claims = { active, scope, exp, sub, ... }
  if (!r.claims.active) {
    return json(null, { code: "token_revoked" });
  }
  return json({ scope: r.claims.scope }, null);
}
```

Read `r.claims.active`: `true` means the token is live, `false` means it's dead
(expired or revoked). Without `client_id` / `client_secret`, `introspect` **throws**
(it's a setup mistake, not a caller mistake).

## Free re-checks within one request ♻️

Asking `auth.user_info(token)` twice for the **same token** in the same request only
hits the network **once** — the second call returns the remembered answer, so it's
instant and doesn't count against your operation budget. (The memory is wiped between
requests, so nothing leaks from one caller to the next.)

## Why bad tokens don't throw 🤔

There are two kinds of "no":

- **"This token isn't valid"** — that's the _caller's_ situation, and a totally normal
  thing to happen. You get it back as data (`{ ok: false }`) so you can answer "please
  log in" without a `try/catch`.
- **"I couldn't even reach the identity server"** — that's an _infrastructure_ problem
  you can't fix in your handler, so `auth` **throws** a labeled error
  (`AUTH_UNAVAILABLE`, retryable) like `db` and `mail` do. Catch it if you want:

```js
function handler(ctx) {
  try {
    const u = auth.user_info(ctx.token);
    return json({ ok: u.ok }, null);
  } catch (e) {
    return json(null, { message: "login service is down, try again" });
  }
}
```

## It shows up on the receipt 🧾

Each check is listed in `meta.auth_requests` (which call, the identity server's host,
the status it returned, how long it took). Cached re-checks don't add a line.

## You're safe 🛡️

The `issuer` is **yours** (operator-supplied), so the robot trusts it and talks to it
directly — the caller's token can never point the robot at a server _you_ didn't name.
And there's no JWT/JWKS crypto to misconfigure: the identity server itself is the judge.

**Next:** [When Things Go Wrong (Errors) →](99-errors.md)
