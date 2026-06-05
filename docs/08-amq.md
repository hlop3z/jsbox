# 8. `amq` — Send Messages to a Queue 📮

[← Back to the guide](README.md)

`amq` lets your script **drop a message into a queue** for some _other_ program to pick
up and handle later. Think of it like a **mailbox** 📬: you post a letter and walk away;
a worker on the other side collects it when it's ready.

This is great for slow or background jobs — "send a welcome email", "resize this image",
"charge this card" — that you don't want to wait around for inside your handler.

> jsbox talks to **RabbitMQ**. jsbox is a **producer** only: it _sends_ messages. It does
> **not** receive/consume them — that's the worker's job, somewhere else.

## Turn it on first 🔑

Give the robot the address of your broker with `config.amq`:

```json
{
  "config": {
    "amq": {
      "host": "localhost",
      "port": 5672,
      "username": "guest",
      "password": "guest",
      "vhost": "/",
      "exchange": "",
      "max_batch": 100
    }
  }
}
```

No `config.amq` → `amq` is turned off.

> **Managed RabbitMQ (CloudAMQP, etc.)?** Add `"tls": true` (the port is usually `5671`)
> for `amqps://`. For a self-hosted broker with a private certificate, also set
> `"ca_cert": "/path/to/ca.pem"`.

## Sending: `amq.send`

You always hand it a **list** of `[routingKey, payload]` pairs — even for one message.
The robot sends the whole batch in one trip and tells you how many it published:

```js
function handler(ctx) {
  var published = amq.send([
    ["user.created", { id: 1, email: ctx.email }],
    ["user.created", { id: 2, email: "sam@example.com" }],
  ]);
  // published === 2
  return json({ published: published }, null);
}
```

- **`routingKey`** — _where_ the message goes. With the default exchange (`""`), this is
  simply the **queue name**.
- **`payload`** — _what_ you're sending. Any JSON value; it's published as its JSON text.
- All calls are **synchronous** (no `await`).

Sending just one? You can skip the outer list:

```js
amq.send(["emails", { to: ctx.email, subject: "Welcome!" }]); // → 1
```

## One batch = one trip 🚚

The whole `amq.send([...])` call opens **one** connection and publishes every message,
then closes. That's why it's list-always: batching is the whole point. It also counts as
**one** operation against your `max_ops` budget, no matter how many messages are in it.

To stop a runaway batch, there's `config.amq.max_batch` (default 100). Send more than
that in a single call and you get an `AMQ_BATCH_TOO_LARGE` error.

## When something goes wrong

If the broker can't be reached or a publish fails, `amq` **throws an error**. Catch it
with `try/catch`:

```js
function handler(ctx) {
  try {
    amq.send([["jobs", { task: "resize", file: ctx.file }]]);
    return json({ queued: true }, null);
  } catch (e) {
    return json(null, { message: "could not queue the job", detail: e.message });
  }
}
```

(Uncaught, the robot turns it into a labeled error — a broker that's down is a retryable
`AMQ_CONNECTION`. See **[When Things Go Wrong](99-errors.md)**.)

## A heads-up about queues 📭

With the default exchange, a message is delivered to the queue whose **name equals the
routing key**. If no such queue exists yet, RabbitMQ quietly **drops** the message — your
`amq.send` still reports success (the broker _accepted_ it), but nobody's listening. Make
sure the worker side has declared the queue.

## It shows up on the receipt 🧾

Each `amq.send` is listed in `meta.amq_requests` (how many messages, total bytes, how
long it took).

**Next:** [When Things Go Wrong (Errors) →](99-errors.md)
