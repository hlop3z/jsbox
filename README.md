# jsbox (runlet)

A sandboxed JavaScript execution engine built in Rust. Send a JS handler function + context via HTTP, get structured `{data, error, meta}` back.

Powered by QuickJS (via rquickjs), axum, and mimalloc.

> 🧒 **New here?** Start with the friendly, beginner-first guide in **[`docs/`](docs/README.md)** —
> it explains `api`, `db`, `mail`, `s3`, and how to handle money/decimals in plain language.

## [Docker](https://github.com/hlop3z/jsbox/pkgs/container/jsbox)

- [Docker-Compose](container/)

```sh
docker run --rm -it -p 4172:3000 ghcr.io/hlop3z/jsbox:latest
```

```sh
curl -X POST http://localhost:4172/execute -H "Content-Type: application/json" \
-d '{
  "script": "function handler(ctx) { return json({ greeting: \"hello \" + ctx.name }, null); }",
  "context": { "name": "Alice" }
}'
```

## Quick start

```sh
cargo run
# Server starts on http://127.0.0.1:3000
```

## Endpoint

```
POST /execute
```

### Request

```json
{
  "script": "function handler(ctx) { return json({ greeting: 'hello ' + ctx.name }, null); }",
  "context": { "name": "Alice" },
  "config": {
    "allowed_hosts": ["api.example.com"],
    "db": {
      "host": "localhost",
      "port": 5432,
      "user": "app",
      "password": "secret",
      "database": "mydb"
    }
  }
}
```

| Field                  | Required | Description                                                              |
| ---------------------- | -------- | ------------------------------------------------------------------------ |
| `script`               | one of   | JS source defining a `handler(ctx)` function                             |
| `key`                  | one of   | Registered-script key (see [Registered scripts](#registered-scripts))    |
| `context`              | no       | JSON object passed as `ctx` to the handler                               |
| `config.allowed_hosts` | no       | Hosts the script can reach via `api.*` (`["*"]` = any, `[]` = disabled)  |
| `config.db`            | no       | PostgreSQL/CockroachDB connection (omit to disable `db.*`)               |
| `config.mongo`         | no       | MongoDB connection (omit to disable `mongo.*`)                           |
| `config.mail`          | no       | SMTP relay connection (omit to disable `mail.*`)                         |
| `config.s3`            | no       | S3/R2/MinIO connection for presigned URLs (omit to disable `s3.*`)       |
| `config.auth`          | no       | OIDC/IAM issuer for `auth.*` token validation (omit to disable `auth.*`) |

### Response

```json
{
  "data": { "greeting": "hello Alice" },
  "error": null,
  "meta": {
    "trace_id": "be04701d-2480-45ec-acb9-787a1be024ba",
    "script_bytes": 82,
    "context_bytes": 16,
    "total_input_bytes": 98,
    "exec_time_us": 950,
    "http_requests": [],
    "db_requests": [],
    "mail_requests": [],
    "s3_requests": []
  }
}
```

Always `{data, error, meta}`. The handler controls `data` and `error` via the `json()`
bridge. On a **system-generated** failure, `error` is a structured envelope —
`{ type, source, code, message, retryable, owner, details?, debug? }` — that a client can
branch on without parsing strings; `meta.trace_id` correlates it with server logs. See
[`docs/99-errors.md`](docs/99-errors.md) for the full contract.

### Registered scripts

Instead of sending source on every call, deploy scripts as files and execute them by
key. Point `scripts_dir` (in `config.json`) at a directory; every `*.js` file under it
is loaded **once at startup**, keyed by its relative path without the extension
(`acme/billing/pricing.js` → `acme/billing/pricing`):

```json
{ "key": "acme/billing/pricing", "context": { "qty": 3, "price": 5 } }
```

A request must carry **exactly one** of `script` / `key` (400 `SCRIPT_XOR_KEY`
otherwise); an unknown key is a 404 `SCRIPT_NOT_FOUND`. Both modes execute through the
identical engine path — same sandbox, same fresh context per request, and `config`
stays per-request either way. Key-mode responses echo the key back in `meta.key`. The
registry is read-only at runtime: changing scripts means redeploying files (image
layer, ConfigMap, mounted volume) and restarting — so N replicas stay trivially
consistent. Design notes: [`docs/design/script-registry.md`](docs/design/script-registry.md).

### ES modules (`import` / `export`)

A handler may be authored as a native **ES module** — `export` its handler and `import`
shared helper modules:

```js
import { quote, withTax } from "acme/pricing";

export default function handler(ctx) {
  return json(withTax(quote(ctx.items, ctx.unit)), null);
}
```

Both `export default function handler` and `export function handler` (named) are accepted.
The mode is auto-detected: a source with a top-level `export` runs as a module, anything
else runs as a classic script (`function handler(ctx) { … }` keeps working unchanged).

**Importable modules** are operator-authored libraries under `modules_dir` (in
`config.json`): every `*.js` / `*.mjs` file is loaded **once at startup**, with a specifier
that is its relative path without the extension (`acme/pricing.mjs` → `acme/pricing`). A
handler `import`s by that specifier. Resolution is a pure in-memory lookup with **no
filesystem access** — a script can `import` only registered modules; an unknown or
traversal specifier (`../`, `/etc/…`) never resolves. Modules run in the same sandbox as
the handler (same memory/timeout/`max_ops` budget) and are read-only at runtime, like the
script registry. Author them with any bundler (`esbuild --bundle --format=esm`) and drop
the output in. Authoring how-to: [`docs/modules.md`](docs/modules.md); design notes:
[`docs/design/injectable-modules.md`](docs/design/injectable-modules.md).

### Operational endpoints

Besides `POST /execute`, the server exposes two unauthenticated read-only endpoints for
liveness and scraping:

```
GET /health    -> 200 "ok"
GET /metrics   -> 200 Prometheus text (version 0.0.4)
```

`/metrics` is dependency-free (no client library) and reports per-outcome execution
counters plus live resilience signals — so a dashboard or alert can watch shed load and a
flapping database without parsing logs:

| Metric                                 | Type      | Labels                                                                                                                       | Meaning                                                                         |
| -------------------------------------- | --------- | ---------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------- |
| `jsbox_executions_total`               | counter   | `outcome` (`success`, `script_error`, `capability_error`, `timeout`, `memory_limit`, `malformed_response`, `internal_error`) | Executions by terminal outcome.                                                 |
| `jsbox_rejections_total`               | counter   | —                                                                                                                            | Requests rejected before execution (bad body, routing, oversized).              |
| `jsbox_overload_total`                 | counter   | `scope` (`global`, `partition`)                                                                                              | Requests shed by the bulkhead (Tier 1) / partition cap (Tier 5).                |
| `jsbox_db_breaker_trips_total`         | counter   | —                                                                                                                            | Cumulative db circuit-breaker open transitions (Tier 3).                        |
| `jsbox_bulkhead_permits_available`     | gauge     | —                                                                                                                            | Free global bulkhead permits right now.                                         |
| `jsbox_bulkhead_permits_total`         | gauge     | —                                                                                                                            | Configured global bulkhead capacity.                                            |
| `jsbox_execution_duration_seconds`     | histogram | `le`                                                                                                                         | Execution wall-clock latency (`_bucket`/`_sum`/`_count`; executions that ran).  |
| `jsbox_capability_op_duration_seconds` | histogram | `capability` (`db`/`mongo`/`http`/`mail`/`s3`/`redis`/`amq`/`auth`), `le`                                                            | Per-capability op latency — which downstream is slow, not just total exec time. |

## JS API

### json(data, error)

The return contract. Every handler must return via `json()`:

```js
function handler(ctx) {
  if (!ctx.name) {
    return json(null, { message: "name is required" });
  }
  return json({ greeting: "hello " + ctx.name }, null);
}
```

### $ / Decimal — exact decimal math

Always available (no config). Backed by the same `rust_decimal` engine that reads
`NUMERIC` columns, so in-script math matches the database exactly. JavaScript has no
operator overloading, so use **methods**, not `+ - * /`:

```js
function handler(ctx) {
  var total = $("19.99").mul(ctx.qty).add("0.01").round(2);
  // methods: add sub mul div neg abs round(places) toCents(places) fromCents(places)
  //          cmp eq lt lte gt gte isZero
  // output:  toString() | toNumber() (lossy) | json() serializes as the exact string
  return json({ total: total }, null); // { "total": "..." }
}
```

`.round()` is half-up. `.toCents()` / `.fromCents()` convert major↔minor units (places
defaults to 2). Holds ~28–29 significant digits. Divide-by-zero and overflow throw.
See [`docs/05-decimal.md`](docs/05-decimal.md).

### api.get / post / put / patch / delete

HTTP client (requires `config.allowed_hosts`):

```js
function handler(ctx) {
  var users = api.get("https://api.example.com/users", { page: 1 });
  // users = { status: 200, data: [...] }

  var created = api.post("https://api.example.com/users", { name: ctx.name });

  // Optional headers (last arg) — cannot override Content-Type
  var auth = api.get("https://api.example.com/me", null, {
    Authorization: "Bearer " + ctx.token,
  });

  return json(users.data, null);
}
```

### db.query / db.execute / db.begin / db.commit / db.rollback

PostgreSQL/CockroachDB client (requires `config.db`):

```js
function handler(ctx) {
  var users = db.query("SELECT id, name FROM users WHERE active = $1", [true]);
  // users = { columns: ["id","name"], rows: [{id:"1",name:"Alice"}], row_count: 1, truncated: false }

  db.execute("INSERT INTO logs (user_id, action) VALUES ($1, $2)", [
    ctx.user_id,
    "login",
  ]);

  // Transactions
  db.begin();
  try {
    db.execute("UPDATE inventory SET stock = stock - $1 WHERE id = $2", [
      1,
      ctx.item_id,
    ]);
    db.commit();
  } catch (e) {
    db.rollback();
    return json(null, { message: e.message });
  }

  return json(users.rows, null);
}
```

BIGINT and NUMERIC values are always returned as strings (JS number precision safety).

### mail.send

SMTP client (requires `config.mail`):

```js
function handler(ctx) {
  var res = mail.send({
    from: "App <no-reply@example.com>", // optional, falls back to config.mail.from
    to: ctx.email, // string or array of strings
    cc: ["ops@example.com"], // optional
    bcc: [], // optional
    reply_to: "support@example.com", // optional
    subject: "Welcome, " + ctx.name,
    text: "Plain-text body",
    html: "<b>HTML body</b>", // text + html => multipart/alternative
  });
  // res = { accepted: true, response: "2.0.0 Ok: queued" }
  return json(res, null);
}
```

`config.mail` (operator-supplied, like `config.db` — the relay host is trusted, so
private/internal relays are allowed):

```json
{
  "host": "smtp.example.com",
  "port": 587,
  "user": "apikey",
  "password": "secret",
  "tls": "starttls",
  "from": "no-reply@example.com",
  "max_recipients": 50,
  "timeout_ms": 10000
}
```

| Field            | Default      | Description                                                 |
| ---------------- | ------------ | ----------------------------------------------------------- |
| `host`           | (required)   | SMTP relay host                                             |
| `port`           | `587`        | Relay port                                                  |
| `user`           | `""`         | SMTP auth user (empty = no authentication)                  |
| `password`       | `""`         | SMTP auth password                                          |
| `tls`            | `"starttls"` | `"starttls"` (587) · `"wrapper"` (465, implicit) · `"none"` |
| `from`           | (required)   | Default From address                                        |
| `max_recipients` | `50`         | Max recipients (to + cc + bcc) per send                     |
| `timeout_ms`     | `10000`      | Connect + send timeout                                      |

Addresses, subject, and bodies are assembled with a typed message builder, so caller
input cannot inject SMTP headers (CRLF injection is rejected at parse time).

### s3.upload_url / s3.download_url / s3.upload_form / s3.sign_url

Presigned-URL generator for direct browser uploads/downloads (requires `config.s3`):

```js
function handler(ctx) {
  // Sign a URL the browser uses to PUT the file straight to the bucket.
  var put = s3.upload_url({ key: "uploads/" + ctx.filename, expires: 300 });
  // put = { url: "https://...&X-Amz-Signature=...", method: "PUT", expires: 300 }

  // Sign a short-lived download link.
  var get = s3.download_url({ key: "uploads/" + ctx.filename });

  // s3.sign_url({ method, key, expires }) is the general form (PUT/GET/HEAD/DELETE).
  return json({ upload: put.url, download: get.url }, null);
}
```

The server **never connects** to the object store — signing is pure AWS SigV4 crypto.
The signed URL goes back to the script, which hands it to the frontend; the browser
does the actual transfer. `expires` is in seconds (clamped to `[1, max_expires]`,
default 15 min, SigV4 max 7 days).

**SSRF-guarded like `api`/`http`:** the `endpoint` must use the `http`/`https` scheme
(no `file://`), and its host is checked against the same private/internal-IP blocklist
([`src/ssrf.rs`](src/ssrf.rs)) — `localhost`, `127.0.0.1`, `10.x`, `192.168.x`,
link-local, etc. are **rejected** (one DNS lookup resolves hostnames). So a presigned
URL can only ever target a **publicly reachable** object store, never a local or
internal one. The sandboxed script cannot set `endpoint`; only operator config can.

> ⚠️ Because of the guard, a `MinIO`/S3 instance on `localhost` or a private LAN is
> **blocked** — point `s3` at a public endpoint (AWS S3, Cloudflare R2, or `MinIO`
> exposed on a public address). For local development, set top-level `"debug": true` in
> the server config to relax this (see [Configuration](#configuration)); never in production.

`config.s3` (operator-supplied, like `config.db`/`config.mail`). Works with any
SigV4 store — AWS S3, Cloudflare R2, MinIO, Backblaze B2, DigitalOcean Spaces:

```json
{
  "endpoint": "https://ACCOUNT.r2.cloudflarestorage.com",
  "region": "auto",
  "bucket": "uploads",
  "access_key": "AKID...",
  "secret_key": "SECRET...",
  "path_style": false,
  "expires": 900,
  "max_expires": 604800,
  "max_upload_size": "25mb"
}
```

| Field             | Default    | Description                                                           |
| ----------------- | ---------- | --------------------------------------------------------------------- |
| `endpoint`        | (required) | Public store URL incl. scheme (`https://s3.us-east-1.amazonaws.com`)  |
| `region`          | (required) | SigV4 region scope (`us-east-1`; R2 uses `auto`)                      |
| `bucket`          | (required) | Bucket name                                                           |
| `access_key`      | (required) | Access key id                                                         |
| `secret_key`      | (required) | Secret access key                                                     |
| `path_style`      | `false`    | `true` = `host/bucket/key` (MinIO); `false` = `bucket.host/key` (AWS) |
| `expires`         | `900`      | Default link lifetime in seconds                                      |
| `max_expires`     | `604800`   | Hard cap on link lifetime (SigV4 max, 7 days)                         |
| `max_upload_size` | (unset)    | **`upload_form` only** — max object bytes, human-readable (`"25mb"`)  |
| `allow_delete`    | `false`    | Enable `s3.delete` + presigning `DELETE` URLs (destructive — opt-in)  |

#### s3.upload_form — size-enforced browser uploads

`upload_url` does not cap the body size. `upload_form` returns a **POST policy** whose
`content-length-range` the object store **enforces** — it rejects an upload larger than
`config.s3.max_upload_size`. The cap is **operator-config only**; the script supplies just
the `key` and can never set or raise the size (it cannot read it from `ctx`). This is the
primitive for storage quotas.

```js
function handler(ctx) {
  // max size comes from config.s3.max_upload_size — NOT from ctx.
  var up = s3.upload_form({
    key: "customers/" + ctx.id + "/" + ctx.filename,
    expires: 300,
  });
  // up = { url, fields: { key, "X-Amz-Algorithm", "X-Amz-Credential",
  //                       "X-Amz-Date", "Policy", "X-Amz-Signature" },
  //        max_bytes: 26214400, expires: 300 }
  return json(up, null);
}
```

Frontend (`multipart/form-data`, the `file` field MUST be last):

```js
const form = new FormData();
Object.entries(up.fields).forEach(([k, v]) => form.append(k, v));
form.append("file", file);
await fetch(up.url, { method: "POST", body: form }); // 204 ok · 400 if > max_bytes
```

`config.s3.max_upload_size` is required for `upload_form` (human-readable like
`"25mb"`/`"50gb"`, or bytes). Without it, `upload_form` errors.

#### s3.usage — total bytes/objects under a prefix

```js
function handler(ctx) {
  var u = s3.usage({ prefix: "user-a/" }); // omit prefix → whole bucket
  // u = { prefix: "user-a/", bytes: 5242880, objects: 137 }
  return json(u, null);
}
```

The **only** `s3` op that connects to the store: it signs and sends a `ListObjectsV2`
(`GET /?list-type=2&prefix=…`), pages through `NextContinuationToken`, and sums each
object's `<Size>`. Trusted/operator-config model like `db`/`mail`, but the endpoint host
still goes through the [`ssrf`](src/ssrf.rs) guard ([`resolve_host`](src/s3.rs)); the
list client follows **no redirects**. S3 has no native folder-size API — a prefix is just
a key namespace — so a full scan is the only exact total. Each 1000-key page counts as one
op against `max_ops`, so an oversized prefix fails with the op-limit error instead of
running unbounded; for very large prefixes maintain your own counter (via `db`) and use
`usage` to reconcile. `bytes`/`objects` are returned as JSON numbers (exact below 2⁵³).

#### s3.delete — remove an object (opt-in)

```js
function handler(ctx) {
  var d = s3.delete({ key: "customers/" + ctx.id + "/photo.jpg" });
  // d = { key: "customers/1/photo.jpg", deleted: true }
  return json(d, null);
}
```

Like `usage`, this **connects to the store** (trusted/operator-config, SSRF-guarded host).
It signs and sends a short-lived `DELETE /{bucket}/{key}`. S3 delete is **idempotent** — a
missing key still returns `deleted: true` (HTTP 204). Because deletion is destructive, it
is **gated behind `config.s3.allow_delete`** (default `false`): even with `s3` otherwise
configured, `s3.delete(...)` — and presigning a `DELETE` URL via `s3.sign_url({ method:
"DELETE" })` — throws unless the operator sets `allow_delete: true`. Counts as one op
against `max_ops`.

### redis.get / set / del / incr / expire

Key/value access against an operator-supplied Redis (`config.redis`, trusted like `db`/`mail`
— no SSRF guard). **Strings in / strings out**: the script owns (de)serialization. Synchronous
(no `await`).

```js
function handler(ctx) {
  redis.set("user:1", JSON.stringify({ id: 1 }), { ttl: 60 }); // ttl seconds, optional
  var raw = redis.get("user:1"); // string | null (null if missing)
  var n = redis.incr("visits"); // number (new value)
  redis.expire("user:1", 120); // bool (true if the key existed)
  redis.del("user:1"); // number (keys removed)
  return json({ user: JSON.parse(raw), visits: n }, null);
}
```

Config: `{ "redis": { "url": "redis://[user:pass@]host:6379[/db]", "timeout_ms": 5000 } }`.
Use a **`rediss://`** URL for TLS (managed services) — validated against bundled public
CA roots, reusing the same `aws-lc-rs` provider as the rest of the stack (no extra crypto
in the binary). A failure to reach Redis surfaces as a retryable
`capability/redis/REDIS_CONNECTION` (HTTP 200), not a server fault. Each call is one op
against `max_ops`.

### amq.send / amq.request — messaging producer (RabbitMQ or NATS)

Publishes a **batch** of messages (`config.amq`, trusted — no SSRF guard). **Producer-side
only** (no subscribe/consume). The backend is `config.amq.backend`: `"rabbitmq"` (default) or
`"nats"`. List-always: `amq.send([[routingKey, payload], …])`; Rust opens one connection for
the whole batch. Synchronous.

```js
function handler(ctx) {
  var published = amq.send([
    ["user.created", { id: 1 }],
    ["user.created", { id: 2 }],
  ]); // → 2
  return json({ published: published }, null);
}
```

The message **body is the JSON of each `payload`**; `routingKey` is the RabbitMQ queue name for
the default exchange (override with `config.amq.exchange`) or the NATS **subject**. The **whole
batch is one op** against `max_ops`; a batch over `config.amq.max_batch` (default 100) is
rejected with `AMQ_BATCH_TOO_LARGE`. A broker outage → retryable
`capability/amq/AMQ_CONNECTION` (HTTP 200).

**RabbitMQ config:** `{ "amq": { "host": "...", "port": 5672, "username": "guest", "password":
"guest", "vhost": "/", "exchange": "", "max_batch": 100, "tls": false, "ca_cert": null } }`. Set
**`"tls": true`** (port usually `5671`) for `amqps://` against managed brokers — validated
against bundled public CA roots via the shared `aws-lc-rs` provider. For a self-hosted broker
with a private CA, point `ca_cert` at the CA PEM (mounted into the container).

**NATS** (`"backend": "nats"`): port defaults to `4222`; `routingKey` is the subject; `vhost`/
`exchange` don't apply; auth is optional (`username`+`password` or `token`). Adds
**request-reply**: `amq.request(subject, payload)` publishes and returns the first reply's
parsed JSON body, bounded by `config.amq.request_timeout_ms` (default 5000) → retryable
`AMQ_TIMEOUT` on no reply. `amq.request` on the RabbitMQ backend throws non-retryable
`AMQ_UNSUPPORTED`. NATS config: `{ "amq": { "backend": "nats", "host": "...", "port": 4222,
"token": "...", "request_timeout_ms": 5000, "tls": false, "ca_cert": null } }`.

### mongo.find / find_one / count / aggregate / insert* / update* / delete* — document database

MongoDB client (requires `config.mongo`, **operator-supplied** — trusted, no SSRF guard, like
`db`/`mail`). Async under the hood (per-op client-side deadline anchored to the execution
budget, like `db`). Filters/updates/pipelines are passed as data, never string-interpolated.
Synchronous from JS.

```js
function handler(ctx) {
  var users = mongo.find("users", { active: true }, { limit: 50, sort: { name: 1 } });
  var one = mongo.find_one("users", { _id: ctx.id });
  var ins = mongo.insert_one("users", { name: ctx.name, active: true }); // { inserted_id }
  mongo.update_one("users", { _id: ins.inserted_id }, { $set: { active: false } }); // { matched, modified }
  mongo.delete_many("logs", { at: { $lt: 2 } }); // { deleted }
  return json({ users: users.docs, one: one }, null);
}
```

`find`/`aggregate` return `{ docs, count, truncated }` capped at `max_docs` (default 1000).
**Type fidelity** (same rule as `db`): values a JS number can't hold exactly come back as
strings — `Int64`/`Decimal128` as strings, `ObjectId` as hex, `Date` as RFC 3339, `Binary` as
base64; `Int32`/`Double` as numbers. Errors: retryable `MONGO_CONNECTION` (unreachable),
`MONGO_WRITE` (duplicate key / constraint), `MONGO_QUERY` (bad filter/update/pipeline),
retryable `MONGO_TIMEOUT` (deadline). Config: `{ "mongo": { "host": "...", "port": 27017,
"username": null, "password": null, "database": "app", "auth_source": "admin",
"op_timeout_ms": 5000, "max_docs": 1000, "tls": false, "ca_cert": null } }`.

### $sys — runtime stdlib (crypto, date, env, secrets)

The `$sys` umbrella groups pure, zero-I/O helpers. `$sys.crypto` and `$sys.date` are
**always on** (no config, like `$`); `$sys.env` / `$sys.secrets` populate only from
`config.sys`. Nothing here does network I/O or counts against `max_ops`.

```js
function handler(ctx) {
  // crypto: one-way hashing/signing, IDs, reversible encoders
  $sys.crypto.sha256("hello"); // hex
  $sys.crypto.hmac("sha256", "key", "msg", "base64"); // hex (default) | base64 | base64url
  $sys.crypto.uuid(); // v4
  $sys.crypto.base64.encode("hi"); // also .base64url / .hex / .url, each .encode/.decode

  // date: parse (ISO/RFC3339, YYYY-MM-DD, epoch ms → UTC), timedelta math, diff
  var due = $sys.date.parse(ctx.when).add({ days: 3, hours: 12 });
  due.iso(); // RFC 3339 "Z"  ·  due.unix() // epoch seconds  ·  json() serializes as ISO
  $sys.date.parse(b).diff($sys.date.parse(a)); // { total_ms, total_seconds, days, hours, ... }

  return json({ due: due }, null);
}
```

**Secrets are use-not-extract** (the multi-tenant guarantee). With
`config.sys = { "env": { "REGION": "us-east-1" }, "secrets": { "SIGNING_KEY": "sk_live_…" } }`:

```js
$sys.env.REGION; // "us-east-1"  (plain, returnable)
var sig = $sys.crypto.hmac("sha256", $sys.secrets.SIGNING_KEY, body); // ✅ handle → one-way sign
String($sys.secrets.SIGNING_KEY); // "[secret:SIGNING_KEY]"  (never the plaintext)
$sys.crypto.base64.encode($sys.secrets.SIGNING_KEY); // ❌ throws — secrets can't be encoded
```

The plaintext never enters JS — it stays Rust-side and is resolved only by the one-way HMAC
sink, so a script can only ever return the `"[secret:NAME]"` placeholder. There is **no**
output scrubber and **no** reveal escape hatch (both evadable/transmit-to-observable). Use
high-entropy secrets. See [`docs/09-sys.md`](docs/09-sys.md).

### auth — OIDC/IAM identity

Resolves a caller's bearer token to its claims (`config.auth`, trusted — the issuer is
operator-supplied, so no SSRF guard). Validation is **delegated to the IAM** (a `userinfo`
round-trip), so there is no local JWT/JWKS crypto stack. Endpoints are auto-discovered from
`{issuer}/.well-known/openid-configuration` unless overridden.

```js
function handler(ctx) {
  var u = auth.user_info(ctx.token); // { ok:true, claims:{sub,email,…} } | { ok:false, status, code }
  if (!u.ok) return json(null, { code: "unauthorized" });
  // RFC 7662 introspection (needs config.auth.client_id/secret) — see token liveness:
  // var r = auth.introspect(ctx.token); if (!r.claims.active) { ... }
  return json({ id: u.claims.sub }, null);
}
```

**Hybrid error surface:** an invalid/expired/under-scoped token is the _caller's_ business
flow, so it returns **in-band** (`{ ok:false, status, code:"AUTH_INVALID_TOKEN" }`, never
thrown — like `api`). Infra failures the handler can't act on (issuer down → retryable
`AUTH_UNAVAILABLE`; misconfig → `AUTH_REQUEST`) **throw** a tagged capability error (like
`db`/`mail`). Per-token results are cached within a request (a repeat lookup makes no round
trip and costs no op). Each call is metered in `meta.auth_requests`.

Config: `{ "auth": { "issuer": "https://login.example.com", "userinfo_url": null,
"introspect_url": null, "client_id": "", "client_secret": "", "timeout_ms": 10000 } }`. Only
`issuer` is required (the rest are discovered / introspection-only). See
[`docs/10-auth.md`](docs/10-auth.md).

## Configuration

> Running it for real? See **[`docs/deployment.md`](docs/deployment.md)** — the production
> hardening checklist (what to set before you point traffic at it, and why).

Optional `config.json` in the working directory. All fields have defaults:

```json
{
  "debug": false,
  "server": {
    "host": "127.0.0.1",
    "port": 3000
  },
  "engine": {
    "memory_limit": "32mb",
    "max_stack_size": "512kb",
    "timeout_ms": 4000,
    "pool_size": 0,
    "max_script_size": "1mb",
    "max_context_size": 0,
    "max_ops": 1500,
    "max_concurrent_executions": 0,
    "max_statement_timeout_ms": 0
  },
  "scripts_dir": "scripts"
}
```

| Field                          | Default    | Description                                                                                                                                                                                                                                                                                                                     |
| ------------------------------ | ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `debug`                        | `false`    | **Dev only.** Relaxes the SSRF private-IP block for `s3`/`api` so localhost/LAN targets (e.g. MinIO) work. Never enable in production.                                                                                                                                                                                          |
| `error_debug`                  | `true`     | Include `error.debug` (stack traces) in system-error responses. Set `false` at an exposed edge to omit them.                                                                                                                                                                                                                    |
| `memory_limit`                 | `"32mb"`   | Max JS heap per execution                                                                                                                                                                                                                                                                                                       |
| `max_stack_size`               | `"512kb"`  | Max native call stack (recursion depth)                                                                                                                                                                                                                                                                                         |
| `timeout_ms`                   | `4000`     | Max wall-clock execution time                                                                                                                                                                                                                                                                                                   |
| `pool_size`                    | `0` (auto) | QuickJS runtime pool size (0 = CPU cores)                                                                                                                                                                                                                                                                                       |
| `max_script_size`              | `"1mb"`    | Max script source size                                                                                                                                                                                                                                                                                                          |
| `max_context_size`             | `0` (auto) | Max context JSON size. `0` auto-derives `memory_limit / 8`; explicit values are capped at `memory_limit / 4` (boot fails if exceeded).                                                                                                                                                                                          |
| `max_ops`                      | `1500`     | Max HTTP + DB operations per execution                                                                                                                                                                                                                                                                                          |
| `max_concurrent_executions`    | `0` (auto) | Bulkhead: max in-flight executions. `0` auto-derives `pool_size × 16`. Excess load fast-fails `429 OVERLOADED`. Tune to your DB/PgBouncer connection budget.                                                                                                                                                                    |
| `max_statement_timeout_ms`     | `0` (off)  | Operator ceiling for `db` `statement_timeout`. `0` = no ceiling. Clamps per-request `statement_timeout_ms` (a request `0` becomes this). See [resilience note](docs/design/resilience.md).                                                                                                                                      |
| `max_concurrent_per_partition` | `0` (off)  | Per-partition fairness (per-pod backstop): max concurrent executions per `X-Partition-Key` (or `partition` field). `0` = off. A key over its share fast-fails `429 PARTITION_OVERLOADED` even when global capacity remains, so one noisy key can't monopolize a pod. Not a global guarantee — the gateway owns global fairness. |
| `partition_buckets`            | `0` (256)  | Hashed partition buckets (used only when `max_concurrent_per_partition > 0`). More buckets = fewer key collisions.                                                                                                                                                                                                              |
| `db_breaker_threshold`         | `0` (off)  | Circuit breaker: consecutive `db` connect failures (per `host:port`) that trip the breaker open. `0` = off. While open, `db` requests fast-fail `DB_CIRCUIT_OPEN` instead of waiting on the connect timeout to a dead database.                                                                                                 |
| `db_breaker_cooldown_ms`       | `0` (5000) | How long the `db` breaker stays open before a half-open probe (used only when `db_breaker_threshold > 0`).                                                                                                                                                                                                                      |
| `scripts_dir`                  | _(unset)_  | Directory of registered scripts for execute-by-key. Unset = inline `script` only; `key` requests answer `SCRIPT_NOT_FOUND`.                                                                                                                                                                                                     |

Size fields accept `"8mb"`, `"256kb"`, `"1gb"`, or plain numbers in bytes.

**Context vs. memory.** Parsing a JSON context into JS objects costs ~4× its text size in heap, and a typical transform needs ~6×. So `max_context_size` is tied to `memory_limit`: leave it `0` and it auto-derives `memory_limit / 8` (room to parse _and_ process the input), while any explicit value is hard-capped at `memory_limit / 4` — the point past which a context can't even be parsed. Change `memory_limit` and the context limit follows; to handle larger contexts, raise `memory_limit` rather than lifting the context cap alone.

## Sandbox

Every execution runs in an isolated QuickJS context with:

- Memory limit (configurable)
- Stack size limit (configurable)
- Execution timeout with interrupt handler
- `eval()` and `Proxy` removed from globals
- Fresh context per request (no state leaks)
- HTTP host allowlist per request
- Operation rate limiting per execution
- Input size validation

## Testing

```sh
# Start backing services (PostgreSQL, PgBouncer, CockroachDB, local httpbin, …)
docker compose up -d

# Run the test suite (starts the server itself if one isn't running)
python test_simple.py

# Stop databases
docker compose down
```

## Architecture

```
HTTP request
  -> axum handler (async)
    -> spawn_blocking (off tokio thread pool)
      -> acquire pooled QuickJS runtime
        -> fresh Context per request
          -> inject json() bridge
          -> inject api.* (if allowed_hosts)
          -> inject db.* (if config.db)
          -> inject mail.* (if config.mail)
          -> inject s3.* (if config.s3)
          -> eval user script
          -> remove eval/Proxy
          -> call handler(context)
        <- extract JSON result
      <- release runtime to pool (GC first)
    <- attach meta (sizes, timing, http/db/mail/s3 metrics)
  <- {data, error, meta} response
```

## License

MIT
