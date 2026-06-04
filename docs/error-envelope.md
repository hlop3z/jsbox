# Structured Error Envelope — Design

Status: **proposed** · Audience: maintainers + client integrators · Scope: the
`/execute` response contract.

This document captures **how errors flow today**, **the new structured envelope**,
and **why** — so a client can deterministically classify every failure (what to log,
what to surface, what to escalate/retry) without parsing message strings.

---

## 1. Goals

A client calling `/execute` must be able to **branch on a stable, explicit field** —
never on heuristics or message text — to decide:

- what is safe to **log** internally,
- what is safe to **surface** to an end user,
- what requires **escalation / telemetry**, and
- what is safe to **retry**.

To do that, every system-generated error carries an explicit `type` (category),
`source` (origin), machine `code`, an optional human-safe `message`, a `retryable`
hint, and gated `debug` context.

---

## 2. The flow today

Every response is `{ data, error, meta }`. On failure, `error` collapses to a bare
`{ "message": string }` (or whatever shape the developer chose), with **no category,
source, or code**. The HTTP status is the only — coarse and inconsistent — signal.

| Failure                                                                   | Caught at                                                              | `error` today   | HTTP    |
| ------------------------------------------------------------------------- | ---------------------------------------------------------------------- | --------------- | ------- |
| Input too large (script/context)                                          | [`handler.rs::execute`](../src/handler.rs)                             | `{ message }`   | 400     |
| Syntax error, bad capability config, `handler` missing, inject failure    | `engine::run` → `Err` → [`build_response`](../src/handler.rs)          | `{ message }`   | 422     |
| Task panic / malformed handler output                                     | [`handler.rs`](../src/handler.rs)                                      | `{ message }`   | 500     |
| Handler **throws** (incl. uncaught **capability** error) or **times out** | [`engine.rs::call_handler`](../src/engine.rs) → `build_error_envelope` | `{ message }`   | **200** |
| Developer `return json(null, x)`                                          | passthrough                                                            | `x` (any shape) | 200     |

### Why this is a problem

1. **No classification.** A db outage, a developer `throw`, a syntax error, and a
   timeout all arrive as `{ message: "..." }`. The client must regex the text to tell
   them apart — brittle and non-portable.
2. **The hardest split is invisible.** A **capability** error and a **developer**
   `throw` are caught at the _same place_ ([`engine.rs`](../src/engine.rs) where
   `handler.call(...)` returns `Err`) and both become `{ message }`. Origin is lost.
3. **Status codes are inconsistent.** Script/capability failures return `200`, engine
   failures `422`/`500`, validation `400` — with no documented contract, so meshes and
   gateways can't react correctly (e.g. retrying things they shouldn't).

---

## 3. Design decisions

| #   | Decision                                           | Choice                                                                                                                                         | Rationale                                                                                                                                                                                      |
| --- | -------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| D1  | Developer-returned errors (`return json(null, x)`) | **Pass through unchanged**                                                                                                                     | Only _system-generated_ errors are structured. Devs keep full control of their explicit error payloads; zero breakage for existing scripts.                                                    |
| D2  | `debug` context (stacks, raw capability payloads)  | **Included by default, gated by config**                                                                                                       | `/execute` is an internal service, never exposed directly to end users, so debug detail is valuable. A new `error_debug` flag (default `true`) lets the edge turn it off if that ever changes. |
| D3  | HTTP status mapping                                | **5xx only for real server faults; 4xx for client/script-submission faults; 200 for executed app/capability errors; `retryable` flag in body** | Best practice for high-throughput microservices: keep deterministic, per-request failures out of 5xx so circuit breakers / retry budgets / SLO alerts aren't poisoned. See §6.                 |

> **D1 nuance:** "only structure system errors" applies to the _explicit_
> `return json(null, x)` path. An **uncaught `throw`** from the handler is caught and
> serialized by the engine — that _is_ system-generated, so it is structured as
> `type: "script"`. The single thing that stays raw is the payload the developer
> deliberately returns.

---

## 4. The new error envelope

The top-level response is unchanged: `{ data, error, meta }`. What changes is the
shape of `error` **when the system generates it**:

```jsonc
{
  "data": null,
  "error": {
    "type":      "request" | "runtime" | "script" | "capability", // programmatic branch
    "source":    "request" | "engine" | "handler" | "db" | "mail" | "s3" | "api",
    "code":      "TIMEOUT",                       // stable SCREAMING_SNAKE constant
    "message":   "execution timed out (4000ms limit)", // human-safe, optional
    "retryable": false,                           // hint for meshes/clients
    "details":   { /* structured extra, e.g. raw capability payload */ }, // optional
    "debug":     { "stack": "…", "raw": "…" }     // gated by error_debug; internal-only
  },
  "meta": { /* unchanged: sizes, timings, *_requests metrics */ }
}
```

### Field reference

| Field       | Type   | Always present | Meaning                                                                                              |
| ----------- | ------ | -------------- | ---------------------------------------------------------------------------------------------------- |
| `type`      | enum   | ✅             | Coarse category for programmatic branching (§5).                                                     |
| `source`    | enum   | ✅             | Where it originated (engine, handler, or a specific capability).                                     |
| `code`      | string | ✅             | Stable machine code; safe to switch on. Never changes meaning.                                       |
| `message`   | string | optional       | Human-safe, pre-filtered for display. No stack/secret content.                                       |
| `retryable` | bool   | ✅             | `true` ⇒ a retry _may_ succeed (transient). `false` ⇒ deterministic; don't retry.                    |
| `details`   | object | optional       | Machine-readable extra context (e.g. capability's raw `{error}` payload).                            |
| `debug`     | object | optional       | Stack traces / raw internals. Present only when `error_debug` is on. **Never** surface to end users. |

`error === null` ⇒ success (unchanged).

---

## 5. Categories, sources, and codes

### `type: "request"` — caller's fault (bad input)

Generated before/around the engine. The submitted request is invalid.

| `code`              | `source`  | `retryable` | Notes                               |
| ------------------- | --------- | ----------- | ----------------------------------- |
| `SCRIPT_TOO_LARGE`  | `request` | `false`     | Script exceeds `max_script_size`.   |
| `CONTEXT_TOO_LARGE` | `request` | `false`     | Context exceeds `max_context_size`. |

> Malformed JSON in the request body is rejected by the axum `Json` extractor _before_
> our handler runs, so it does not flow through this envelope (the framework returns its
> own 4xx). Documented here for completeness.

### `type: "runtime"` — engine / QuickJS (untraceable or engine-level)

| `code`                | `source` | `retryable` | Notes                                                                                             |
| --------------------- | -------- | ----------- | ------------------------------------------------------------------------------------------------- |
| `SYNTAX_ERROR`        | `engine` | `false`     | `eval` of the script failed to parse.                                                             |
| `HANDLER_NOT_DEFINED` | `engine` | `false`     | Script defines no `handler(context)`.                                                             |
| `TIMEOUT`             | `engine` | `false`     | Wall-clock limit hit (detected via the interrupt flag — deterministic, no message parsing).       |
| `MEMORY_LIMIT`        | `engine` | `false`     | Memory cap exceeded (best-effort: classified by the thrown error's `name`, e.g. `InternalError`). |
| `MALFORMED_RESPONSE`  | `engine` | `false`     | `handler` returned something that isn't a `{data,error}` envelope.                                |
| `INTERNAL`            | `engine` | `true`      | Our fault: context creation, capability injection, or a task panic. Alert-worthy.                 |

### `type: "script"` — developer code (owned by the script author)

| `code`         | `source`  | `retryable` | Notes                                                                                                                                                  |
| -------------- | --------- | ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `SCRIPT_ERROR` | `handler` | `false`     | Uncaught `throw` from the handler (an explicit `throw`, or a script bug like `TypeError`). `message` is the JS error's message; stack goes in `debug`. |

Explicit `return json(null, x)` is **not** reshaped (D1) — `x` passes through verbatim.

### `type: "capability"` — external service / integration

Produced when a capability's native call fails and the wrapper throws **uncaught**. The
`code`/`retryable` are derived **in Rust, above the stringify cliff** (§12), from the typed
error — `SqlState`, `reqwest`/`lettre` predicates — then tagged at the throw site so origin
is explicit (§7). The raw driver message goes in `details.raw`. A fallback `*_ERROR` code is
used when the typed error matches none of the specific cases below.

The code set is **mechanism now, granularity later**: the contract carries `code`, so finer
codes are added without re-touching the wrapper or the client.

#### `source: "db"` — derived from `SqlState`

| `code`             | `SqlState` (class)          | `retryable` | Notes                                       |
| ------------------ | --------------------------- | ----------- | ------------------------------------------- |
| `DB_SERIALIZATION` | `40001`                     | `true`      | Serialization failure — retry the txn.      |
| `DB_DEADLOCK`      | `40P01`                     | `true`      | Deadlock detected — retry.                  |
| `DB_CONNECTION`    | `08xxx`                     | `true`      | Connection failure — transient.             |
| `DB_CANCELED`      | `57014`                     | `true`      | Query canceled / statement timeout.         |
| `DB_CONSTRAINT`    | `23xxx`                     | `false`     | Integrity violation (unique/FK/check/null). |
| `DB_QUERY`         | `42xxx`                     | `false`     | Bad SQL (syntax/undefined) — caller's bug.  |
| `DB_ERROR`         | (any other / no `SqlState`) | `true`      | Fallback. Raw message in `details.raw`.     |

#### `source: "mail"` — derived from SMTP transient/permanent

| `code`            | Derived from              | `retryable` | Notes                                   |
| ----------------- | ------------------------- | ----------- | --------------------------------------- |
| `MAIL_TRANSIENT`  | 4xx SMTP / `is_transient` | `true`      | Greylisting, mailbox busy — retry.      |
| `MAIL_PERMANENT`  | 5xx SMTP / `is_permanent` | `false`     | Rejected, bad address — don't retry.    |
| `MAIL_CONNECTION` | connect/TLS failure       | `true`      | Couldn't reach the relay.               |
| `MAIL_ERROR`      | (any other)               | `true`      | Fallback. Raw message in `details.raw`. |

#### `source: "s3"`

| `code`        | Derived from                   | `retryable` | Notes                                              |
| ------------- | ------------------------------ | ----------- | -------------------------------------------------- |
| `S3_UPSTREAM` | non-2xx / network from `usage` | `true`      | Object store returned an error or was unreachable. |
| `S3_OP_LIMIT` | `max_ops` reached mid-listing  | `false`     | Prefix too large — deterministic.                  |
| `S3_ERROR`    | signing / payload failure      | `false`     | Bad key/config — caller's bug. Fallback.           |

#### `source: "api"` (http) — **in-band, not thrown**

The HTTP client never throws: a non-2xx **response** is data (`{ status, data }`). Only a
**transport** failure is an error, returned in-band with the same structured shape so the
script can inspect it (§13):

```js
api.get(url); // transport failed → { status: 0, error: { code, retryable, source: "api" } }
```

| `code`                | Derived from (`reqwest`) | `retryable` | Notes                                   |
| --------------------- | ------------------------ | ----------- | --------------------------------------- |
| `HTTP_TIMEOUT`        | `is_timeout()`           | `true`      | Request exceeded the client timeout.    |
| `HTTP_CONNECT`        | `is_connect()`           | `true`      | TCP/TLS connect failure.                |
| `HTTP_DNS`            | resolution failure       | `true`      | Host did not resolve.                   |
| `HTTP_SSRF_BLOCKED`   | SSRF guard rejection     | `false`     | Host/IP disallowed — deterministic.     |
| `HTTP_BODY_TOO_LARGE` | response over cap        | `false`     | Response exceeded the size limit.       |
| `HTTP_ERROR`          | (any other)              | `true`      | Fallback. Raw message in `details.raw`. |

> It only becomes a `capability` **envelope** (vs in-band) if the script deliberately
> `throw`s the returned `error` object.

---

## 6. HTTP status mapping (enterprise / microservice rationale)

The guiding rule: **HTTP status answers "should infrastructure react?", the body answers
"what happened?"** A service mesh, gateway, or retry middleware keys off status; it must
not see a developer's bug or a validation error as a 5xx, or it will retry
non-retryable work and trip circuit breakers across the mesh.

| `type`                                                                                             | HTTP    | Why                                                                                                                               |
| -------------------------------------------------------------------------------------------------- | ------- | --------------------------------------------------------------------------------------------------------------------------------- |
| `request`                                                                                          | **400** | Caller's fault. Never retry; don't alert.                                                                                         |
| `runtime` (`SYNTAX_ERROR`, `HANDLER_NOT_DEFINED`, `TIMEOUT`, `MEMORY_LIMIT`, `MALFORMED_RESPONSE`) | **422** | Deterministic for _this_ script. Unprocessable, but the server is healthy — don't retry, don't trip breakers.                     |
| `runtime` (`INTERNAL`)                                                                             | **500** | Genuine server fault (panic/inject). **Retryable**, alert-worthy — exactly what 5xx should mean.                                  |
| `script`                                                                                           | **200** | The engine ran and produced a deterministic outcome. App-level error → body, not transport.                                       |
| `capability`                                                                                       | **200** | A downstream hiccup must not cascade into mesh-wide 5xx retries. The `retryable: true` flag lets the caller retry _deliberately_. |

This keeps **5xx a clean signal**: it fires only when retrying might help and an operator
should look. Everything else is classified in the body, where `type` + `code` +
`retryable` give the client a deterministic decision table:

```
if (status >= 500)               -> infra fault: retry w/ backoff, alert
else if (error == null)          -> success
else switch (error.type) {
  case "request":    -> 4xx: fix the request, do not retry
  case "runtime":    -> bad script submission, do not retry; log
  case "script":     -> app error: surface error.message to dev/user as appropriate
  case "capability": -> dependency error: if error.retryable, retry; else escalate
}
```

---

## 7. The new flow

```
                       ┌──────────────────────────── handler.rs ───────────────────────────┐
request ──▶ validate sizes ──▶ spawn_blocking ──▶ engine::run ──▶ build_response ──▶ {data,error,meta}
              │ fail                                   │                  │
              ▼                                        ▼                  ▼
        request/SCRIPT_TOO_LARGE              runtime/* or              parse envelope:
        request/CONTEXT_TOO_LARGE             Ok(js_json)               error==null ? success
        (400)                                                           : passthrough (dev) OR
                                                                          structured (system)
```

### Where each category is produced

- **`request`** — [`handler.rs::execute`](../src/handler.rs) size checks, before the engine.
- **`runtime`** — [`engine.rs::run`](../src/engine.rs):
  - `eval_script` error ⇒ `SYNTAX_ERROR`.
  - `handler` global missing ⇒ `HANDLER_NOT_DEFINED`.
  - interrupt flag set ⇒ `TIMEOUT` (deterministic, set by the timeout handler).
  - thrown `name == InternalError` ⇒ `MEMORY_LIMIT` (best-effort).
  - context/inject failure or `JoinError` panic ⇒ `INTERNAL`.
  - non-envelope handler output ⇒ `MALFORMED_RESPONSE` in `build_response`.
- **`script` vs `capability`** — the key mechanism. When `handler.call(...)` returns
  `Err`, the engine retrieves the **actual thrown JS value** via `ctx.catch()` and
  inspects it _structurally_ (no message parsing):
  - If the object carries the **jsbox capability tag** (set by the wrapper, §below) ⇒
    `type: "capability"`, with `source`/`code` read from the tag.
  - Otherwise ⇒ `type: "script"`, `code: SCRIPT_ERROR`.

### Tagging capability errors at the throw site

Each capability wrapper (`src/js/db.js`, `mail.js`, `s3.js`) currently does:

```js
if (res && res.error) throw new Error(res.error);
```

It becomes a tagged throw the engine can classify deterministically. The native layer is
widened from `{ "error": msg }` to `{ error, code, retryable, source }` (the `code`/`retryable`
**derived in Rust** above the cliff, §12); the wrapper **forwards it wholesale** — no
hardcoded code, no classification:

```js
if (res && res.error) {
  var e = new Error(res.error);
  e.__jsbox = res; // { error, code, retryable, source } — transport only, structural marker
  throw e;
}
```

So the engine reads `code`/`retryable`/`source` straight off the tag (set by Rust), and
`.message`/`.stack` off the `Error`. If the developer catches the capability error and
returns their own `json(null, …)`, it stays a **developer** error (D1) — their choice, fully
under their control.

---

## 8. Debug gating

`error.debug` carries stack traces and raw capability payloads — useful internally,
unsafe to ever show an end user.

- New config flag **`error_debug`** (top-level in `config.json`), **default `true`**
  because `/execute` runs as an internal service.
- When `false`, `error.debug` is omitted from every response (the data is still
  available for server-side logging/telemetry).
- Kept **separate** from the existing `debug` flag (which only relaxes the SSRF private-IP
  block) so the two concerns don't entangle.

```jsonc
{ "debug": false, "error_debug": true, "server": { … }, "engine": { … } }
```

---

## 9. Backward compatibility

- **Field rename — `errors` → `error` (breaking):** the top-level envelope key is now
  the singular `error` on **every** response (`{ data, error, meta }`), matching the
  second argument of `json(data, error)`. This is a one-time breaking change for any
  client reading the old plural `errors` key — they must read `error` instead. Done now,
  before the structured envelope lands, so integrators migrate the key and the shape in a
  single pass.
- **Success responses:** shape unchanged save the rename (`{ data, error: null, meta }`).
- **Developer `return json(null, x)`:** unchanged — `x` passes through verbatim (D1).
- **System errors:** `error` changes from `{ message }` to the structured object.
  Existing clients that only read `error.message` keep working — `message` is still
  there. Clients gain `type`/`source`/`code`/`retryable` to branch on.
- **HTTP status:** timeout/script/capability move to a documented model (some shift from
  today's ad-hoc values, e.g. capability errors stay 200; runtime engine errors become a
  consistent 422 vs the current 422/200 mix). Integrators relying on status alone should
  migrate to body classification.

---

## 10. Before → after

**Capability failure (db down), uncaught:**

```jsonc
// today
{ "data": null, "error": { "message": "db query failed: connection refused" }, "meta": {…} } // HTTP 200

// new
{
  "data": null,
  "error": {
    "type": "capability", "source": "db", "code": "DB_CONNECTION",
    "message": "database request failed",
    "retryable": true,
    "details": { "raw": "db query failed: connection refused" },
    "debug": { "stack": "Error: db query failed…\n  at handler…" }
  },
  "meta": {…}
}                                                                                              // HTTP 200, retryable:true
```

**Timeout:**

```jsonc
// new
{ "data": null,
  "error": { "type": "runtime", "source": "engine", "code": "TIMEOUT",
              "message": "execution timed out (4000ms limit)", "retryable": false },
  "meta": {…} }                                                                                // HTTP 422
```

**Developer error (unchanged):**

```jsonc
return json(null, [{ field: "email", reason: "required" }]);
// → { "data": null, "error": [{ "field": "email", "reason": "required" }], "meta": {…} }         // HTTP 200
```

---

## 11. Implementation plan (files touched)

1. **`src/errors.rs`** (new) — `ErrorEnvelope { type, source, code, message, retryable,
details, debug }` + `ErrorCategory`/`ErrorSource` enums + constructors per code.
2. **`src/{db,mail,s3,http}.rs`** — add a small `classify(&TypedError) -> (code,
retryable)` **above each `map_err` cliff** (db.rs:193, mail.rs:251, http.rs:264),
   where the typed error (`SqlState`, `reqwest`/`lettre` predicates) still exists.
   Widen the FFI error JSON from `{error}` to `{error, code, retryable, source}`.
3. **`src/engine.rs`** — classify in `call_handler`: read `ctx.catch()`, inspect the
   `__jsbox` tag + `.stack`, fold in the timeout flag and `runtime.memory_usage()`.
   Return a typed `EngineError` enum (not `Box<dyn Error>`) and **delete
   `build_error_envelope`** — error envelopes are assembled in Rust, not via a JS round-trip.
4. **`src/handler.rs`** — `build_response` / `infra_error` emit `ErrorEnvelope`; map
   category → HTTP status (§6); gate `debug` on `error_debug`.
5. **`src/js/{db,mail,s3}.js`** — transport only: `e.__jsbox = res; throw e` (no classification).
6. **`src/config.rs`** — add `error_debug: bool` (default `true`).
7. **`container/types.d.ts`** — add `ErrorEnvelope`/`ErrorType` types; update `json`'s
   contract notes.
8. **`README.md`** + this doc — document the contract; add the client decision table.

---

## 12. Division of responsibility — Rust classifies, JS transports

**Principle (the "stringify cliff"):** an error's classifiability decreases monotonically
from its typed origin. Each capability has a line where a typed error becomes a string —
[`db.rs:193`](../src/db.rs#L193), [`mail.rs:251`](../src/mail.rs#L251),
[`http.rs:264`](../src/http.rs#L264). Above it the `SqlState` / `reqwest` / `lettre`
predicates exist; below it only text. **Classification must happen above the cliff (Rust);
everything downstream can only carry it, never recover it.**

| Station                        | Typed error?   | Classifies?                                  | Job                                                                 |
| ------------------------------ | -------------- | -------------------------------------------- | ------------------------------------------------------------------- |
| Rust adapter (above the cliff) | ✅ only place  | **yes** — derive `code`+`retryable`+`source` | `classify(&TypedErr)`                                               |
| FFI JSON                       | no             | no                                           | **carry** `{error, code, retryable, source}`                        |
| JS wrapper                     | no             | **never** (can't, without message parsing)   | `e.__jsbox = res; throw e`                                          |
| Engine `ctx.catch()` (Rust)    | reads JS Error | combine                                      | read `.__jsbox` + `.stack`; fold in timeout flag + `memory_usage()` |
| Handler (Rust)                 | —              | assemble                                     | envelope + status + debug gate                                      |

**Rust-only signals JS literally cannot see** (so they must be classified in Rust):
the timeout interrupt flag, `runtime.memory_usage()` (OOM vs user-thrown `InternalError`),
and the JS error's `.stack` read via `ctx.catch()`. JS owns **nothing** classificatory —
the wrapper exists solely so developer `try/catch` / `instanceof Error` works.

## 13. Resolved: per-capability codes & `api` symmetry

**Per-capability codes — adopt the mechanism now; grow granularity later.** The `SqlState`
is _destroyed today_ at `db.rs:193`; shipping `format!` doesn't defer the feature, it
erases the data that enables it. So establish the widened contract + one `classify()` per
capability now, with the high-value codes whose predicate is free. **The canonical code
tables live in §5** (per `source`: db/mail/s3/api).

Finer codes are additive later (the contract already carries `code`, so neither the wrapper
nor the client is re-touched). **Do not** let a code reassign `type` (a SQL `42601` is the
developer's bug, not "db down") — `retryable:false` already conveys the decision; an
`owner: caller|developer|operator` field is the cleaner future axis. Deferred.

**`api` symmetry — rejected. Unify the error _shape_, not the control flow.** A non-2xx HTTP
response is **data** the script must branch on (already kept as `status` at
[`http.rs:266`](../src/http.rs#L266)), not an exception — making `api` throw would force a
`try/catch` around every call and is semantically wrong. Only a _transport_ failure
(today's `status:0`) is a true error; enrich it **in-band** with the same structured object
and **do not throw**:

```js
api.get(url); // ok        → { status, data }
// transport → { status: 0, error: { code: "HTTP_TIMEOUT", retryable: true, source: "api" } }
```

`db`/`mail`/`s3` throw because they have no "successful-but-negative response" concept;
HTTP does. The invariant worth enforcing is the **identical error schema** across all four,
not a uniform throw. A script that wants capability-style propagation can `throw` deliberately.
