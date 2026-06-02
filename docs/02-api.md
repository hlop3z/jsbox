# 2. `api` — Talk to the Internet 🌐

[← Back to the guide](README.md)

`api` lets your script visit other websites and ask them for data — like a phone
that can only call numbers you've allowed.

## Turn it on first 🔑

Tell the robot which websites it's allowed to visit, using `allowed_hosts`:

```json
{
  "config": {
    "allowed_hosts": ["api.example.com"]
  }
}
```

- `["api.example.com"]` → only that website is allowed.
- `["*"]` → **any** website (the star means "all").
- `[]` or missing → `api` is turned off.

If your script tries a website that isn't on the list, the robot says no. 🚫

## The five ways to call

| Call                            | When you use it          |
| ------------------------------- | ------------------------ |
| `api.get(url, params, headers)` | Ask for something / read |
| `api.post(url, body, headers)`  | Create something new     |
| `api.put(url, body, headers)`   | Replace something        |
| `api.patch(url, body, headers)` | Change part of something |
| `api.delete(url, headers)`      | Remove something         |

`params`, `body`, and `headers` are all optional.

## Reading data (GET)

```js
function handler(ctx) {
  var res = api.get("https://api.example.com/users", { page: 1 });
  // res looks like: { status: 200, data: [ ...users... ] }
  return json(res.data, null);
}
```

What you get back has **two parts**:

- **`res.status`** — the number the website replied with. `200` means "OK!" 👍
- **`res.data`** — the actual stuff (already unpacked for you, ready to use).

The `{ page: 1 }` becomes `?page=1` on the end of the web address.

## Sending data (POST)

```js
function handler(ctx) {
  var created = api.post("https://api.example.com/users", { name: ctx.name });
  return json(created.data, null);
}
```

The second thing (`{ name: ctx.name }`) is the **body** — what you're sending.

## Adding headers (like a secret password) 🪪

Some websites need a password called a "token". You add it as **headers** (the last thing):

```js
function handler(ctx) {
  var me = api.get("https://api.example.com/me", null, {
    Authorization: "Bearer " + ctx.token,
  });
  return json(me.data, null);
}
```

> Note: you can't change the `Content-Type` header — the robot sets that one for you.

## It shows up on the receipt 🧾

Every call you make is listed in `meta.http_requests` in the answer, so you can see
what happened (which website, how long it took, the status). Handy for checking your work!

**Next:** [`db` — talk to a database →](03-database.md)
