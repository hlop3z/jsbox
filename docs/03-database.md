# 3. `db` — Talk to a Database 🗄️

[← Back to the guide](README.md)

A database is a giant, super-organized **spreadsheet** that remembers things forever.
`db` lets your script read rows and save rows.

> jsbox talks to **PostgreSQL** (and CockroachDB). Don't worry what those are — just
> know it's where the data lives.

## Turn it on first 🔑

Give the robot the keys to your database with `config.db`:

```json
{
  "config": {
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

No `config.db` → `db` is turned off.

## Reading rows: `db.query`

```js
function handler(ctx) {
  var result = db.query("SELECT id, name FROM users WHERE active = $1", [true]);
  // result = {
  //   columns: ["id", "name"],
  //   rows: [ { id: 1, name: "Mia" }, { id: 2, name: "Sam" } ],
  //   row_count: 2,
  //   truncated: false
  // }
  return json(result.rows, null);
}
```

You get back four things:

- **`columns`** — the column names.
- **`rows`** — the actual rows (this is the part you usually want).
- **`row_count`** — how many rows came back.
- **`truncated`** — `true` if there were _too many_ rows and some were left out.

## What is `$1`? (the safe way to add your own values) 🛟

**Never** glue your values straight into the SQL text. Instead, write `$1`, `$2`,
`$3`... in the SQL, and put your real values in the list `[ ... ]`. The robot fills
them in safely.

```js
// ✅ GOOD — safe
db.query("SELECT * FROM users WHERE name = $1 AND age > $2", [ctx.name, 18]);

// ❌ BAD — never do this (someone could trick your database)
db.query("SELECT * FROM users WHERE name = '" + ctx.name + "'");
```

Think of `$1` as a labeled lunchbox 🍱 — the database knows it's _data_, not a command,
so nobody can sneak in something sneaky.

## Changing rows: `db.execute`

Use `db.execute` for **adding, changing, or removing** rows.

```js
function handler(ctx) {
  var out = db.execute("INSERT INTO logs (user_id, action) VALUES ($1, $2)", [
    ctx.user_id,
    "login",
  ]);
  // out = { rows_affected: 1 }
  return json({ saved: out.rows_affected }, null);
}
```

It tells you `rows_affected` — how many rows changed.

## All-or-nothing: transactions 🎁

Sometimes you have several changes that must **all** happen, or **none** of them.
Like moving money: take from one account, add to another. You don't want one without
the other!

Wrap them in `db.begin()` ... `db.commit()`:

```js
function handler(ctx) {
  db.begin(); // start
  try {
    db.execute(
      "UPDATE accounts SET cents = cents - $1 WHERE id = $2",
      [500, 1],
    );
    db.execute(
      "UPDATE accounts SET cents = cents + $1 WHERE id = $2",
      [500, 2],
    );
    db.commit(); // save it all
  } catch (e) {
    db.rollback(); // oops — undo everything
    return json(null, { message: e.message });
  }
  return json({ ok: true }, null);
}
```

- `db.begin()` → "I'm starting a group of changes."
- `db.commit()` → "All good — save them all together."
- `db.rollback()` → "Something broke — pretend none of it happened."

## When something goes wrong

If a query fails, `db` **throws an error**. Catch it with `try/catch` (like above) and
report it nicely in the `error` spot.

## A heads-up about numbers ⚠️

Some numbers come back as **text in quotes**, like `"19.99"` or `"9007199254740993"`.
This is on purpose, so no digits get lost. It matters a lot for **money**.

👉 Read **[Exact Decimal Math](05-decimal.md)** next — it's important!

**Next:** [`mail` — send email →](04-mail.md)
