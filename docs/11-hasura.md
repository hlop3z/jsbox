# 11. Hasura — GraphQL without the boilerplate 🚀

[← Back to the guide](README.md)

Hasura turns your database into a GraphQL API. jsbox can talk to it like any other
website (with [`api`](02-api.md)) — but every handler ends up repeating the same three
chores. The **`hasura/client`** module does them for you.

## What the module saves you

1. Building the `/v1/graphql` web address.
2. Adding the right password headers (an admin secret, or a logged-in user's token).
3. **Catching GraphQL errors** — the sneaky part. Hasura often replies `200 OK` even
   when your query was wrong, hiding the real problem _inside_ the answer. Plain `api`
   code that only checks the status thinks it worked. The module turns those hidden
   errors into real errors so you can't miss them. 🎣

## Turn it on (operator setup)

This is a **module**, so it's wired up in two places:

- The operator drops the file `modules/hasura/client.mjs` into the folder named by
  `modules_dir` in the server's `config.json` (loaded once at startup).
- Each request turns on `api` for the Hasura host and tells the module where Hasura is.
  The secret lives in config — **never in your script**:

```jsonc
// server config.json (operator, once)
{ "modules_dir": "/srv/jsbox/modules" }
```

```jsonc
// the "config" you send with each /execute request
{
  "allowed_hosts": ["hasura.internal"],
  "sys": {
    "env": {
      "HASURA_ENDPOINT": "https://hasura.internal",
      "HASURA_ADMIN_SECRET": "super-secret"
    }
  }
}
```

## Use it

```js
import { hasura } from "hasura/client";

export default function handler(ctx) {
  const h = hasura(); // reads the endpoint + secret from config for you
  const data = h.query(
    `query ($id: uuid!) { users_by_pk(id: $id) { id email } }`,
    { id: ctx.userId },
  );
  return json(data.users_by_pk, null);
}
```

`h.query(...)` (and its twin `h.mutate(...)` — the same call, with a nicer name for
writes) hands you back **just the data**. If anything went wrong it **throws**, and the
error carries `.code` and `.graphql` so you know exactly what Hasura complained about.

### Send the user's login instead of the master key 🪪

The admin secret can see _everything_. If you'd rather Hasura show only what **this
user** is allowed to see, hand it their token — Hasura then enforces that user's
row-level permissions:

```js
const h = hasura({ token: ctx.token });
```

You can also pick a permission role: `hasura({ token: ctx.token, role: "viewer" })`.

### Want to peek at the errors yourself?

`h.raw(query, vars)` returns Hasura's untouched answer `{ data, errors }` **without
throwing** — handy when some errors are expected and you want to handle them inline.

## Always send variables, never glue strings 🧷

Put values in the **second argument** (`{ id: ctx.userId }`), the way the examples do.
Don't build the query by pasting user data into the text — same rule as `db`'s
`$1, $2`. The module keeps you on the safe path on purpose.

## Why a module and not a built-in super-power?

Talking to Hasura is just HTTP, and the module holds no secrets of its own (it reads
them from config, which your script could read anyway). That makes it a perfect
**helper module** rather than a Rust super-power. See [Authoring modules](modules.md)
to write your own.

**Next:** [When things go wrong (Errors) →](99-errors.md)
