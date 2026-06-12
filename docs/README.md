# jsbox — Friendly Guide 📦

Welcome! This is the easy guide to using **jsbox**. No scary words. Promise.

## What is jsbox? (the 10-second version)

Imagine a little **robot in a box** 🤖. You hand the robot:

1. a **note** that says what to do (a small JavaScript function), and
2. some **stuff to work with** (your data).

The robot does the work _inside the box_ (so it can never make a mess on your
computer), and hands you back an **answer**. That's it!

You talk to the robot by sending a message to `POST /execute`.

## Start here 👇

Read these in order. Each one is short.

1. **[Getting Started](01-getting-started.md)** — your very first script, and the
   shape of every answer.
2. **[`api` — talk to the internet](02-api.md)** — fetch data from other websites.
3. **[`db` — talk to a database](03-database.md)** — read and save rows.
4. **[`mail` — send email](04-mail.md)** — send a real email.
5. **[`$` — Exact Decimal Math](05-decimal.md)** — the built-in money helper. Do exact
   decimal math with `$("19.99").mul(3)`. Always on, no setup. 💵
6. **[`s3` — signed upload/download links](06-s3.md)** — let a browser upload files
   straight to your bucket (S3, R2, MinIO…). 🔗
7. **[`redis` — a super-fast notebook](07-redis.md)** — stash little bits of data by name
   (cache, counters, sessions). 📝
8. **[`amq` — send messages to a queue](08-amq.md)** — drop jobs into RabbitMQ for a
   worker to pick up later. 📮
9. **[`$sys` — the built-in toolbox](09-sys.md)** — hashing, signing, dates, and
   use-but-never-see secrets. Always on (no setup) for `crypto` + `date`. 🧰
10. **[`auth` — who is this person?](10-auth.md)** — check a login token and get the
    user's details from your identity server (Zitadel, Keycloak, Auth0…). 🪪
11. **[When Things Go Wrong (Errors)](99-errors.md)** — what the robot hands back when
    something fails, and how to read it. 🚦

## The super-powers 🦸

Your robot starts with **no** super-powers. You turn each one on by adding a
little `config` to your message. That keeps things safe.

| Super-power             | What it does                | Turn it on with        |
| ----------------------- | --------------------------- | ---------------------- |
| `api`                   | Talk to other websites      | `config.allowed_hosts` |
| `db`                    | Talk to a database          | `config.db`            |
| `mail`                  | Send email                  | `config.mail`          |
| `s3`                    | Signed upload links         | `config.s3`            |
| `redis`                 | A super-fast notebook       | `config.redis`         |
| `amq`                   | Send messages to a queue    | `config.amq`           |
| `auth`                  | Check a login token         | `config.auth`          |
| `$sys.env` / `.secrets` | Settings + use-only secrets | `config.sys`           |

(`$` — exact decimal math — and **`$sys.crypto` / `$sys.date`** are the exceptions:
they're **always on**, no config. Only `$sys.env` / `$sys.secrets` need `config.sys`.)

If you don't turn a super-power on, the robot simply doesn't have it. (For example,
if there's no `config.mail`, then `mail` is `undefined` — it isn't there at all.)
