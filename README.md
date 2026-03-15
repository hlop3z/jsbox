# jsbox

A sandboxed JavaScript execution engine built in Rust. Send a JS handler function + context via HTTP, get structured `{data, errors, meta}` back.

Powered by QuickJS (via rquickjs), axum, and mimalloc.

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
    "db_requests": []
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

## Configuration

Optional `config.json` in the working directory. All fields have defaults:

```json
{
  "server": {
    "host": "127.0.0.1",
    "port": 3000
  },
  "engine": {
    "memory_limit": "8mb",
    "max_stack_size": "256kb",
    "timeout_ms": 100,
    "pool_size": 0,
    "max_script_size": "1mb",
    "max_context_size": "5mb",
    "max_ops": 50
  }
}
```

| Field              | Default    | Description                               |
| ------------------ | ---------- | ----------------------------------------- |
| `memory_limit`     | `"8mb"`    | Max JS heap per execution                 |
| `max_stack_size`   | `"256kb"`  | Max native call stack (recursion depth)   |
| `timeout_ms`       | `100`      | Max wall-clock execution time             |
| `pool_size`        | `0` (auto) | QuickJS runtime pool size (0 = CPU cores) |
| `max_script_size`  | `"1mb"`    | Max script source size                    |
| `max_context_size` | `"5mb"`    | Max context JSON size                     |
| `max_ops`          | `50`       | Max HTTP + DB operations per execution    |

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
          -> eval user script
          -> remove eval/Proxy
          -> call handler(context)
        <- extract JSON result
      <- release runtime to pool (GC first)
    <- attach meta (sizes, timing, http/db metrics)
  <- {data, errors, meta} response
```

## License

MIT
