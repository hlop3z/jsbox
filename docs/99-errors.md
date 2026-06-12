# 99. When Things Go Wrong (Errors) 🚦

[← Back to the guide](README.md)

Sometimes a script can't finish — a database is down, an email bounces, or your code
has a typo. The robot never just crashes on you. It hands back a **clear, labeled
error** you can act on, instead of a scary blob of text. This page is the decoder ring. 🔑

## Two kinds of "error"

Remember `json(data, error)`? The **second slot** is for errors. An error shows up in
the answer in one of two ways:

1. **Your error** — you call `return json(null, { message: "name required" })`. Whatever
   you put there comes back **exactly as you wrote it**. The robot doesn't touch it.
2. **The robot's error** — something broke that you didn't hand-write (a timeout, a
   database hiccup, a typo that crashes your script). The robot fills `error` with a
   neat **labeled object** (below).

So `error` is `null` when all is well, **your shape** when you set it, or the robot's
**labeled shape** when the robot caught the problem.

## What the robot's error looks like 🔖

```json
{
  "data": null,
  "error": {
    "type": "capability",
    "source": "db",
    "code": "DB_CONSTRAINT",
    "message": "database request failed",
    "retryable": false,
    "owner": "developer",
    "details": { "sqlstate": "23505" }
  },
  "meta": { "trace_id": "be04...", "...": "..." }
}
```

You never have to read the message text to know what happened — every field is there
so a **program** can decide what to do:

| Field       | What it tells you                                                                              |
| ----------- | ---------------------------------------------------------------------------------------------- |
| `type`      | the big bucket: `request`, `runtime`, `script`, or `capability` (see below)                    |
| `source`    | who it came from: `engine`, `handler`, or a tool (`db`/`mail`/`s3`/`api`/`redis`/`amq`/`auth`) |
| `code`      | a stable label you can switch on, like `DB_CONSTRAINT`. Never changes meaning.                 |
| `message`   | a short, safe sentence for humans. (Secrets/PII never go here.)                                |
| `retryable` | `true` = trying again might help; `false` = it won't, don't bother                             |
| `owner`     | **who should fix it**: `caller`, `developer`, or `operator`                                    |
| `details`   | extra machine-readable bits, like `{ "sqlstate": "23505" }`                                    |
| `debug`     | the nerdy stuff (stack trace + raw text) — only when turned on (see the bottom)                |

## The four buckets (`type`) 🪣

| `type`       | Means                                                                      | What you do                                        |
| ------------ | -------------------------------------------------------------------------- | -------------------------------------------------- |
| `request`    | the **message** you sent was bad (too big)                                 | fix the request                                    |
| `runtime`    | the **engine** couldn't run your script (typo, ran too long, no `handler`) | fix the script                                     |
| `script`     | your code **threw** an error (`throw`, or a bug like a `TypeError`)        | fix the script; `message` is your error's own text |
| `capability` | a **tool** failed (database down, email bounced)                           | if `retryable`, try again; otherwise look closer   |

## Who should fix it? (`owner`) 🧑‍🔧

`owner` is the handiest field for bigger setups — it says **who to call**:

- **`caller`** — whoever sent the request (it was malformed).
- **`developer`** — the script author (a bug, bad SQL, too many operations).
- **`operator`** — the people running the servers (a database/broker is down).

So a dead database pages the ops team, but your `TypeError` doesn't. 🙂

## The traffic light (HTTP status) 🚥

The HTTP status is a quick signal for gateways and load balancers:

| You get | Means                                                                            |
| ------- | -------------------------------------------------------------------------------- |
| **200** | the robot ran fine; if there's an `error` it's an app/tool issue (read the body) |
| **400** | your request was bad (`request` type)                                            |
| **404** | the `key` you asked for isn't registered (`SCRIPT_NOT_FOUND`)                    |
| **422** | your script can't be processed (typo, timeout, no `handler`)                     |
| **500** | the robot itself broke (rare!) — safe to retry, someone should look              |

The rule: **5xx means "infrastructure, react!"** Everything else is explained in the
body, so a gateway never retries things it shouldn't.

## Try-again-later (`retryable`) 🔁

A database deadlock or a network blip is **`retryable: true`** — waiting and retrying
might work. A bad query or a constraint violation is **`retryable: false`** — retrying
fails the same way, so don't.

## Catching tool errors yourself

`db`, `mail`, `s3`, `redis`, and `amq` **throw** when they fail, so you can `try/catch`
and turn it into your own friendly answer:

```js
function handler(ctx) {
  try {
    db.execute("INSERT INTO users(email) VALUES($1)", [ctx.email]);
    return json({ ok: true }, null);
  } catch (e) {
    // this is YOUR error now — it passes through exactly as you write it
    return json(null, { message: "could not save user", detail: e.message });
  }
}
```

`api` is the one exception: it **never throws**. A failed request comes back as data
(`{ status: 0, error: { ... } }`), so you just check `res.status`.

## The decoder tables 🗂️

Want to handle specific cases? Switch on `code`. Here's every code, by tool.

### Your request (`type: "request"`)

| `code`              | retry | owner  | When                                                                |
| ------------------- | ----- | ------ | ------------------------------------------------------------------- |
| `SCRIPT_TOO_LARGE`  | no    | caller | Script bigger than `max_script_size`.                               |
| `CONTEXT_TOO_LARGE` | no    | caller | Context bigger than `max_context_size`.                             |
| `SCRIPT_XOR_KEY`    | no    | caller | Request has both `script` and `key`, or neither — send exactly one. |
| `SCRIPT_NOT_FOUND`  | no    | caller | The `key` isn't in the server's script registry (404).              |

### The engine (`type: "runtime"`)

| `code`                | retry | owner     | When                                                   |
| --------------------- | ----- | --------- | ------------------------------------------------------ |
| `SYNTAX_ERROR`        | no    | developer | The script didn't parse.                               |
| `HANDLER_NOT_DEFINED` | no    | developer | No `handler(ctx)` function.                            |
| `TIMEOUT`             | no    | developer | Ran past the time limit.                               |
| `MEMORY_LIMIT`        | no    | developer | The context was too big to load into the memory limit. |
| `MALFORMED_RESPONSE`  | no    | developer | Returned something that isn't a `json(...)` answer.    |
| `INTERNAL`            | yes   | operator  | The robot's own fault (rare) — a 500.                  |

### Your script (`type: "script"`)

| `code`         | retry | owner     | When                                                                     |
| -------------- | ----- | --------- | ------------------------------------------------------------------------ |
| `SCRIPT_ERROR` | no    | developer | Your code threw an error (or hit a bug). `message` is your error's text. |

### Tools (`type: "capability"`)

**`db`** (from the database's `SqlState`):

| `code`             | retry | owner     | When                                                       |
| ------------------ | ----- | --------- | ---------------------------------------------------------- |
| `DB_SERIALIZATION` | yes   | operator  | Serialization failure — retry the transaction.             |
| `DB_DEADLOCK`      | yes   | operator  | Deadlock — retry.                                          |
| `DB_CONNECTION`    | yes   | operator  | Couldn't reach the database (drop, or can't connect).      |
| `DB_CANCELED`      | yes   | operator  | Query canceled / statement timeout.                        |
| `DB_CONSTRAINT`    | no    | developer | Broke a rule (unique/foreign-key/etc). `details.sqlstate`. |
| `DB_QUERY`         | no    | developer | Bad SQL.                                                   |
| `DB_OP_LIMIT`      | no    | developer | Hit `max_ops`.                                             |
| `DB_ERROR`         | yes   | operator  | Anything else (fallback).                                  |

**`mail`** (from the SMTP reply):

| `code`           | retry | owner     | When                                         |
| ---------------- | ----- | --------- | -------------------------------------------- |
| `MAIL_TRANSIENT` | yes   | operator  | 4xx reply (greylisting, mailbox busy).       |
| `MAIL_PERMANENT` | no    | developer | 5xx reply (rejected, bad address).           |
| `MAIL_OP_LIMIT`  | no    | developer | Hit `max_ops`.                               |
| `MAIL_ERROR`     | yes   | operator  | Anything else, incl. connect/TLS (fallback). |

**`s3`:**

| `code`         | retry | owner     | When                                                                        |
| -------------- | ----- | --------- | --------------------------------------------------------------------------- |
| `S3_UPSTREAM`  | yes   | operator  | Store errored or was unreachable (`usage`/`delete`). `details.http_status`. |
| `S3_OP_LIMIT`  | no    | developer | Hit `max_ops` while listing.                                                |
| `S3_FORBIDDEN` | no    | operator  | `delete` without `config.s3.allow_delete`.                                  |
| `S3_ERROR`     | no    | developer | Bad key/config / signing (fallback).                                        |

**`api`** (returned **in-band** as `{ status: 0, error }`, never thrown):

| `code`                | retry | owner     | When                            |
| --------------------- | ----- | --------- | ------------------------------- |
| `HTTP_TIMEOUT`        | yes   | operator  | Request timed out.              |
| `HTTP_CONNECT`        | yes   | operator  | TCP/TLS/DNS connect failure.    |
| `HTTP_SSRF_BLOCKED`   | no    | developer | URL/host wasn't allowed.        |
| `HTTP_BODY_TOO_LARGE` | no    | developer | Response was over the size cap. |
| `HTTP_OP_LIMIT`       | no    | developer | Hit `max_ops`.                  |
| `HTTP_ERROR`          | yes   | operator  | Anything else (fallback).       |

**`redis`:**

| `code`             | retry | owner     | When                                  |
| ------------------ | ----- | --------- | ------------------------------------- |
| `REDIS_CONNECTION` | yes   | operator  | Couldn't reach Redis (or it dropped). |
| `REDIS_TIMEOUT`    | yes   | operator  | A command timed out.                  |
| `REDIS_OP_LIMIT`   | no    | developer | Hit `max_ops`.                        |
| `REDIS_ERROR`      | yes   | operator  | Anything else (fallback).             |

**`amq`** (RabbitMQ producer):

| `code`                | retry | owner     | When                                      |
| --------------------- | ----- | --------- | ----------------------------------------- |
| `AMQ_CONNECTION`      | yes   | operator  | Couldn't reach the broker.                |
| `AMQ_BATCH_TOO_LARGE` | no    | developer | Batch bigger than `config.amq.max_batch`. |
| `AMQ_OP_LIMIT`        | no    | developer | Hit `max_ops`.                            |
| `AMQ_ERROR`           | yes   | operator  | Publish/protocol error (fallback).        |

**`auth`** (OIDC/IAM identity). An invalid token is **not** an error — it comes back
**in-band** as `{ ok: false, status, code: "AUTH_INVALID_TOKEN" }` (like `api`, never
thrown). These codes are only for the failures `auth` **throws**:

| `code`             | retry | owner     | When                                                                          |
| ------------------ | ----- | --------- | ----------------------------------------------------------------------------- |
| `AUTH_UNAVAILABLE` | yes   | operator  | Identity server unreachable / 5xx / timeout. `details.http_status`.           |
| `AUTH_REQUEST`     | no    | operator  | Misconfig: bad endpoint, discovery failed, `introspect` without client creds. |
| `AUTH_OP_LIMIT`    | no    | developer | Hit `max_ops`.                                                                |

> New codes can show up over time, but they **never change meaning** and never move to a
> different `type` — so it's always safe to switch on `code`.

## The receipt number — `trace_id` 🧾

Every answer (good or bad) carries a **`meta.trace_id`**. When something goes wrong, the
robot also writes that id to its own logs **with the full error**. So if you hit a
problem, give the operator the `trace_id` and they can find the exact details — even the
parts hidden from you.

## The extra-detail switch — `error_debug` 🔍

By default the robot includes a `debug` box (a stack trace + the raw error text), because
`/execute` is meant to run as an **internal** service. If it's ever put somewhere public,
the operator sets `error_debug: false` and the `debug` box disappears — but `code`,
`owner`, `details`, and the safe `message` stay, so programs still get what they need. The
raw text is never lost: it's always in the server logs under the `trace_id`.

**Next:** [Back to the guide →](README.md)
