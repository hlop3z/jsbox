# 7. `redis` — A Super-Fast Notebook 📝

[← Back to the guide](README.md)

`redis` is a tiny, **super-fast notebook** the robot can scribble in. You write a little
note under a **name** (a "key") and read it back later by that same name. Great for
counters, caches, sessions, and "remember this for a minute" jobs.

> jsbox talks to **Redis** (and Redis-compatible stores). It keeps **text** — small bits
> of data you look up by key. It's much faster than a database, but it's not for big
> permanent storage.

## Turn it on first 🔑

Give the robot the address of your Redis with `config.redis`:

```json
{
  "config": {
    "redis": {
      "url": "redis://localhost:6379/0",
      "timeout_ms": 5000
    }
  }
}
```

No `config.redis` → `redis` is turned off.

> **Managed Redis?** Use a `rediss://` URL (two s's) for TLS — it just works, validated
> against the usual public certificate authorities.

## Strings in, strings out 🧵

The notebook only holds **text**. If you want to store an object, you turn it into text
yourself with `JSON.stringify`, and turn it back with `JSON.parse`:

```js
function handler(ctx) {
  // store an object as text
  redis.set("user:1", JSON.stringify({ id: 1, name: "Mia" }));

  // read it back and turn it into an object
  var user = JSON.parse(redis.get("user:1"));
  return json({ name: user.name }, null);
}
```

All `redis` calls are **synchronous** (no `await`), just like `db` and `$`.

## The five things you can do

### `redis.set(key, value, opts?)` — write a note

```js
redis.set("greeting", "hello");              // remember forever
redis.set("otp:42", "123456", { ttl: 60 });  // remember for 60 seconds, then forget
```

`ttl` is **time-to-live in seconds** — optional. After it runs out, the note vanishes
on its own. Perfect for codes, sessions, and caches.

### `redis.get(key)` — read a note

```js
var v = redis.get("greeting"); // "hello"
var m = redis.get("nope");     // null  (no note under that name)
```

A missing key gives you **`null`**.

### `redis.del(key)` — erase a note

```js
var removed = redis.del("greeting"); // number erased (0 or 1)
```

### `redis.incr(key)` — count things 🔢

```js
var views = redis.incr("page:views"); // adds 1, returns the new number
```

`incr` is the easy way to count: page views, rate limits, "how many times…". It bumps
the number up by one and hands you the result. (If the key didn't exist, it starts at 1.)

### `redis.expire(key, seconds)` — set a timer ⏳

```js
redis.expire("user:1", 120); // forget "user:1" in 2 minutes → true if the key existed
```

Give an existing note a countdown. Returns `true` if the key was there.

## A real example: a simple rate limiter 🚦

Count requests per user, and say "too fast" after 5:

```js
function handler(ctx) {
  var key = "rate:" + ctx.user;
  var hits = redis.incr(key);
  if (hits === 1) redis.expire(key, 60); // reset the count every minute
  if (hits > 5) return json(null, { message: "slow down!" });
  return json({ ok: true, hits: hits }, null);
}
```

## When something goes wrong

If Redis can't be reached or a command fails, `redis` **throws an error**. Catch it with
`try/catch` and report it nicely:

```js
function handler(ctx) {
  try {
    return json({ value: redis.get(ctx.key) }, null);
  } catch (e) {
    return json(null, { message: e.message });
  }
}
```

(If you don't catch it, the robot turns it into a labeled error for you — see
**[When Things Go Wrong](99-errors.md)**.)

## It shows up on the receipt 🧾

Every `redis` call is listed in `meta.redis_requests` (the action, how long it took).

**Next:** [`amq` — send messages to a queue →](08-amq.md)
