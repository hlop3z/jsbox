# jsbox

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

| Field                  | Required | Description                                                             |
| ---------------------- | -------- | ----------------------------------------------------------------------- |
| `script`               | yes      | JS source defining a `handler(ctx)` function                            |
| `context`              | no       | JSON object passed as `ctx` to the handler                              |
| `config.allowed_hosts` | no       | Hosts the script can reach via `api.*` (`["*"]` = any, `[]` = disabled) |
| `config.db`            | no       | PostgreSQL/CockroachDB connection (omit to disable `db.*`)              |
| `config.mail`          | no       | SMTP relay connection (omit to disable `mail.*`)                        |
| `config.s3`            | no       | S3/R2/MinIO connection for presigned URLs (omit to disable `s3.*`)      |

### Response

```json
{
  "data": { "greeting": "hello Alice" },
  "error": null,
  "meta": {
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

Always `{data, error, meta}`. The handler controls `data` and `error` via the `json()` bridge.

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
  // methods: add sub mul div neg abs round(places) cmp eq lt lte gt gte isZero
  // output:  toString() | toNumber() (lossy) | json() serializes as the exact string
  return json({ total: total }, null); // { "total": "..." }
}
```

`.round()` is half-up. Holds ~28–29 significant digits. Divide-by-zero and overflow throw.
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

### s3.presignPut / s3.presignGet / s3.presignPost / s3.presign

Presigned-URL generator for direct browser uploads/downloads (requires `config.s3`):

```js
function handler(ctx) {
  // Sign a URL the browser uses to PUT the file straight to the bucket.
  var put = s3.presignPut({ key: "uploads/" + ctx.filename, expires: 300 });
  // put = { url: "https://...&X-Amz-Signature=...", method: "PUT", expires: 300 }

  // Sign a short-lived download link.
  var get = s3.presignGet({ key: "uploads/" + ctx.filename });

  // s3.presign({ method, key, expires }) is the general form (PUT/GET/HEAD/DELETE).
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
| `max_upload_size` | (unset)    | **`presignPost` only** — max object bytes, human-readable (`"25mb"`)  |
| `allow_delete`    | `false`    | Enable `s3.delete` + presigning `DELETE` URLs (destructive — opt-in)  |

#### s3.presignPost — size-enforced browser uploads

`presignPut` does not cap the body size. `presignPost` returns a **POST policy** whose
`content-length-range` the object store **enforces** — it rejects an upload larger than
`config.s3.max_upload_size`. The cap is **operator-config only**; the script supplies just
the `key` and can never set or raise the size (it cannot read it from `ctx`). This is the
primitive for storage quotas.

```js
function handler(ctx) {
  // max size comes from config.s3.max_upload_size — NOT from ctx.
  var up = s3.presignPost({
    key: "customers/" + ctx.id + "/" + ctx.filename,
    expires: 300,
  });
  // up = { url, fields: { key, "X-Amz-Algorithm", "X-Amz-Credential",
  //                       "X-Amz-Date", "Policy", "X-Amz-Signature" },
  //        maxBytes: 26214400, expires: 300 }
  return json(up, null);
}
```

Frontend (`multipart/form-data`, the `file` field MUST be last):

```js
const form = new FormData();
Object.entries(up.fields).forEach(([k, v]) => form.append(k, v));
form.append("file", file);
await fetch(up.url, { method: "POST", body: form }); // 204 ok · 400 if > maxBytes
```

`config.s3.max_upload_size` is required for `presignPost` (human-readable like
`"25mb"`/`"50gb"`, or bytes). Without it, `presignPost` errors.

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
configured, `s3.delete(...)` — and presigning a `DELETE` URL via `s3.presign({ method:
"DELETE" })` — throws unless the operator sets `allow_delete: true`. Counts as one op
against `max_ops`.

## Configuration

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
    "max_context_size": "10mb",
    "max_ops": 1500
  }
}
```

| Field              | Default    | Description                                                                                                                            |
| ------------------ | ---------- | -------------------------------------------------------------------------------------------------------------------------------------- |
| `debug`            | `false`    | **Dev only.** Relaxes the SSRF private-IP block for `s3`/`api` so localhost/LAN targets (e.g. MinIO) work. Never enable in production. |
| `memory_limit`     | `"32mb"`   | Max JS heap per execution                                                                                                              |
| `max_stack_size`   | `"512kb"`  | Max native call stack (recursion depth)                                                                                                |
| `timeout_ms`       | `4000`     | Max wall-clock execution time                                                                                                          |
| `pool_size`        | `0` (auto) | QuickJS runtime pool size (0 = CPU cores)                                                                                              |
| `max_script_size`  | `"1mb"`    | Max script source size                                                                                                                 |
| `max_context_size` | `"10mb"`   | Max context JSON size                                                                                                                  |
| `max_ops`          | `1500`     | Max HTTP + DB operations per execution                                                                                                 |

Size fields accept `"8mb"`, `"256kb"`, `"1gb"`, or plain numbers in bytes.

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
# Start databases (PostgreSQL + CockroachDB)
docker compose up -d

# Run all 87 tests
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
