# 1. Getting Started 🚀

[← Back to the guide](README.md)

## The two rules

Every script you give the robot follows **two rules**:

1. You must write a function called **`handler`**. It gets one thing: `ctx` (your data).
2. You must give your answer back using **`json(data, errors)`**.

Here is the smallest script that works:

```js
function handler(ctx) {
  return json({ message: "Hello!" }, null);
}
```

- The first part, `{ message: "Hello!" }`, is your **data** (the good stuff).
- The second part, `null`, is your **errors** (nothing went wrong, so `null`).

## Sending it to the robot

You send a message (called JSON) to `POST /execute`. It has three parts:

```json
{
  "script": "function handler(ctx) { return json({ greeting: 'hi ' + ctx.name }, null); }",
  "context": { "name": "Mia" },
  "config": {}
}
```

| Part      | What it is                              | Needed? |
| --------- | --------------------------------------- | ------- |
| `script`  | Your JavaScript (must have a `handler`) | ✅ yes  |
| `context` | The data your script gets as `ctx`      | no      |
| `config`  | Which super-powers to turn on           | no      |

Try it from a terminal:

```sh
curl -X POST http://localhost:3000/execute -H "Content-Type: application/json" -d '{
  "script": "function handler(ctx){ return json({ greeting: \"hi \" + ctx.name }, null); }",
  "context": { "name": "Mia" }
}'
```

## What the robot hands back

The answer is **always** the same shape. Three boxes: `data`, `errors`, and `meta`.

```json
{
  "data": { "greeting": "hi Mia" },
  "errors": null,
  "meta": {
    "exec_time_us": 950,
    "http_requests": [],
    "db_requests": [],
    "mail_requests": []
  }
}
```

- **`data`** — what your script returned as the good stuff.
- **`errors`** — `null` if all went well, or your error message if not.
- **`meta`** — a little **receipt** 🧾 the robot fills in for you: how long it took,
  and a list of every internet/database/email action it did. You don't write this —
  the robot does.

## Saying "oops, something went wrong"

If your script needs to report a problem, put a message in the **errors** spot:

```js
function handler(ctx) {
  if (!ctx.name) {
    return json(null, { message: "Please give me a name!" });
  }
  return json({ greeting: "hi " + ctx.name }, null);
}
```

When `errors` has something in it, `data` is usually `null`. Think of it like a
traffic light: **green** = data, **red** = errors. 🟢🔴

## House rules (so nobody breaks the box) 🧱

The robot is careful. It will stop your script if it:

- runs **too long** (there's a time limit),
- uses **too much memory**,
- does **too many** internet/database/email actions, or
- gets a `script` or `context` that is **too big**.

You don't need to worry about these for normal scripts — they're just there so one
script can't hog everything.

**Next:** [`api` — talk to the internet →](02-api.md)
