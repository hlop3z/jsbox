# 12. `mongo` — Talk to a Document Database 🍃

[← Back to the guide](README.md)

`mongo` lets your script read and write **documents** (JSON-shaped records) in a MongoDB
database. If `db` is a spreadsheet of rows, `mongo` is a **folder of little JSON files** you
can search, add to, change, and delete.

> Like `db` and `mail`, the connection is **operator-supplied** — it lives in the server's
> `config.json`, not your request, so it's trusted (it connects to exactly the host the
> operator named, no SSRF guard). All calls are **synchronous** (no `await`).

## Turn it on first 🔑

The operator gives the connection a nickname like `shop-docs` in the server's `config.json`:

```json
{
  "resources": {
    "shop-docs": {
      "kind": "mongo",
      "host": "localhost",
      "port": 27017,
      "username": "app",
      "password": "secret",
      "database": "shop",
      "auth_source": "admin",
      "op_timeout_ms": 5000,
      "max_docs": 1000
    }
  }
}
```

Then your request asks for it by nickname with `config.io.mongo`:

```json
{
  "config": {
    "io": { "mongo": ["shop-docs"] }
  }
}
```

No nickname in `config.io.mongo` → `mongo` is turned off (`typeof mongo === "undefined"`).

- `username`/`password` are optional — omit them for a database with no auth.
- `auth_source` is the database your user is defined in (default `admin`).
- For TLS add `"tls": true`; for a self-hosted server with a private CA also set
  `"ca_cert": "/path/to/ca.pem"`.

## Reading 🔍

```js
function handler(ctx) {
  // Find many — returns { docs, count, truncated }
  var active = mongo.find("users", { active: true }, { limit: 50, sort: { name: 1 } });

  // Find one — returns the document, or null
  var me = mongo.find_one("users", { _id: ctx.id });

  // Count — returns a number
  var total = mongo.count("users", { active: true });

  // Aggregate — returns { docs, count, truncated }
  var byCity = mongo.aggregate("users", [
    { $group: { _id: "$city", n: { $sum: 1 } } },
  ]);

  return json({ active: active.docs, me: me, total: total, byCity: byCity.docs }, null);
}
```

`find` and `aggregate` cap their result at `max_docs` (default 1000) and set
`truncated: true` when there was more. `find`'s options are `limit`, `skip`, `sort`, and
`projection`.

## Writing ✍️

```js
function handler(ctx) {
  var ins = mongo.insert_one("users", { name: ctx.name, active: true }); // { inserted_id }
  mongo.insert_many("logs", [{ at: 1 }, { at: 2 }]);                     // { inserted_count }

  // Updates need atomic operators like $set
  var up = mongo.update_one("users", { _id: ins.inserted_id }, { $set: { active: false } });
  // up === { matched, modified }

  var del = mongo.delete_many("logs", { at: { $lt: 2 } }); // { deleted }

  return json({ id: ins.inserted_id, up: up, del: del }, null);
}
```

## Numbers come back exact 💯

Just like `db`, any value that a JavaScript number can't hold exactly comes back as a
**string** so nothing is silently rounded:

- `Int64` and `Decimal128` → strings (use [`$`](05-decimal.md) for exact math)
- `ObjectId` → its 24-character hex string
- `Date` → an ISO string (e.g. `"2026-06-21T00:00:00Z"`)
- `Binary` → base64
- `Int32` and `Double` → normal numbers; booleans, strings, arrays, and nested objects pass
  through as themselves.

## When something goes wrong

`mongo` **throws** a labeled error you can catch:

```js
try {
  mongo.insert_one("users", { _id: "taken", name: "dup" });
} catch (e) {
  return json(null, { message: "could not save", detail: e.message });
}
```

- Database unreachable → retryable `MONGO_CONNECTION`.
- Duplicate key / write rule broken → `MONGO_WRITE` (your fault — fix the data).
- A bad filter/update/pipeline → `MONGO_QUERY`.
- Too slow → retryable `MONGO_TIMEOUT` (bounded by the execution budget). See
  **[When Things Go Wrong](99-errors.md)**.

## It shows up on the receipt 🧾

Each `mongo` call is listed in `meta.mongo_requests` (which operation, how many documents,
how long it took).

**Next:** [When Things Go Wrong →](99-errors.md)
