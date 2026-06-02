# jsbox

A sandboxed JavaScript execution engine built in Rust. Send a JS handler function + context via HTTP, get structured `{data, errors, meta}` back.

Powered by QuickJS (via rquickjs), axum, and mimalloc.

> 🧒 **New here?** Start with the friendly, beginner-first guide in **[`docs/`](docs/README.md)** —
> it explains `api`, `db`, `mail`, and how to handle money/decimals in plain language.

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

### Response

```json
{
  "data": { "greeting": "hello Alice" },
  "errors": null,
  "meta": {
    "script_bytes": 82,
    "context_bytes": 16,
    "total_input_bytes": 98,
    "exec_time_us": 950,
    "http_requests": [],
    "db_requests": [],
    "mail_requests": []
  }
}
```

Always `{data, errors, meta}`. The handler controls `data` and `errors` via the `json()` bridge.

## JS API

### json(data, errors)

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
See [`docs/06-decimal.md`](docs/06-decimal.md).

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

## Configuration

Optional `config.json` in the working directory. All fields have defaults:

```json
{
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

| Field              | Default    | Description                               |
| ------------------ | ---------- | ----------------------------------------- |
| `memory_limit`     | `"32mb"`   | Max JS heap per execution                 |
| `max_stack_size`   | `"512kb"`  | Max native call stack (recursion depth)   |
| `timeout_ms`       | `4000`     | Max wall-clock execution time             |
| `pool_size`        | `0` (auto) | QuickJS runtime pool size (0 = CPU cores) |
| `max_script_size`  | `"1mb"`    | Max script source size                    |
| `max_context_size` | `"10mb"`   | Max context JSON size                     |
| `max_ops`          | `1500`     | Max HTTP + DB operations per execution    |

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
          -> eval user script
          -> remove eval/Proxy
          -> call handler(context)
        <- extract JSON result
      <- release runtime to pool (GC first)
    <- attach meta (sizes, timing, http/db/mail metrics)
  <- {data, errors, meta} response
```

## License

MIT
