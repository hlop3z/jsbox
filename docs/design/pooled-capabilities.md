# Design note: pooled capabilities vs. per-execution connections

Status: **analysis + decision — no jsbox code changes planned**. Grounded in the code
as of `main` (2026-06). Companion to [script-registry.md](script-registry.md).
A `pgbouncer` service (transaction mode) ships in `docker-compose.yml`, and the
integration suite runs the full db section through it to prove the claim below.

## Decision

**Solve connection churn with mature external infrastructure (PgBouncer et al.), not
in-process pooling.** jsbox keeps its current per-execution connection model — the
inline capability model stays the only model. In-process named pools are explicitly
deferred, with the bar for revisiting documented at the end.

Rationale in one line: AWS (RDS Proxy) and Cloudflare (Hyperdrive) both concluded that
serverless connection storms are best solved by a pooler _between_ the runtime and the
database, not inside the runtime. We don't reinvent that wheel.

## What the current model costs (measured against the code)

Every I/O capability materializes per execution, inside `engine.rs::inject_apis`:

| Capability | Behavior today                                                                                      | Per-request cost                                                         |
| ---------- | --------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------ |
| `db`       | `connect()` at inject time (`db.rs`) — TCP + optional TLS + Postgres auth, torn down at request end | ~1–5 ms LAN plain, 10–50 ms with TLS — often more than the script itself |
| `redis`    | `client.get_connection()` at inject time (`kv.rs`)                                                  | one handshake; cheap (no TLS) but nonzero                                |
| `amq`      | lazy: one connection + channel per `send` batch (`amq.rs`)                                          | per batch, already amortized                                             |
| `http`     | new reqwest `Client` per execution (`http.rs`)                                                      | client build + **no keep-alive reuse across executions**                 |

The sharper problem is **unbounded concurrency**: `JsPool::acquire()` creates a fresh
runtime when the pool is empty (`pool.rs`), and `spawn_blocking` allows ~512 threads.
A burst can run hundreds of concurrent executions, each opening its own Postgres
connection, multiplied by replicas — a connection storm against a database that
defaults to `max_connections = 100`.

## The adopted solution: external poolers (mature tech, zero jsbox changes)

### Postgres → PgBouncer (or RDS Proxy / pgcat / Supavisor / Hyperdrive)

Deploy PgBouncer between jsbox and Postgres; point `config.db.host` at it.

- **Transaction pooling mode** is the right default for jsbox scripts: each script's
  queries run as independent statements/transactions, and `db.rs` already sets
  `statement_timeout` per session. jsbox's "connect per request" becomes a connect to
  a _local_ pooler — microseconds, no TLS necessary on the loopback/sidecar hop —
  while PgBouncer maintains a small, steady set of real connections to Postgres.
- Solves: handshake latency (mostly), connection storms (entirely), Postgres
  `max_connections` exhaustion (entirely), TLS renegotiation CPU on the DB host.
- Caveats to document for operators:
  - Transaction mode forbids session state (named prepared statements, `SET` without
    `LOCAL`, advisory locks held across transactions, `LISTEN`). jsbox scripts get a
    fresh logical session per request anyway, so this matches the existing semantics —
    scripts never could rely on session state across requests.
  - `connect_timeout` is 5 s in `db.rs`; a saturated pooler queues connects, which
    surfaces as the existing retryable `DB_CONNECTION` error. Correct behavior, worth
    knowing.
  - **`statement_timeout` is not robust through transaction mode** (found by the
    adversarial suite, 2026-06). jsbox issues a session-level `SET statement_timeout`
    at connect (`db.rs`); in transaction pooling that SET only sticks if the timed
    query happens to reuse the same server connection. The hardening probe shows it
    ENFORCED under an idle pool, but under concurrency the SET and the query can land
    on different server connections and the cap is silently lost — and jsbox's
    wall-clock interrupt cannot cancel a blocking libpq call, so a `pg_sleep`-style
    query could hold a `spawn_blocking` thread with no timeout.
    - **The startup-parameter "fix" does NOT work** (tried and reverted, 2026-06):
      `postgres::Config::options("-c statement_timeout=…")` makes PgBouncer **refuse
      the connection** — `unsupported startup parameter in options: statement_timeout`
      — so it breaks db connectivity entirely behind the pooler. Don't reach for it.
    - **Robust fix is operator-side** (survives any pooling, mature-tech, zero jsbox
      code): set the cap server-side where every server session inherits it —
      `ALTER ROLE <user> SET statement_timeout = '<ms>'`, a per-database default, or a
      PgBouncer `connect_query`/`server_reset_query`. The trade-off is that the
      per-request `config.db.statement_timeout_ms` is then not honored *through a
      transaction-mode pooler* (it still is on direct connections and session-mode
      pooling, where the `SET` is left in place).
    - **True per-request enforcement through a txn-mode pooler would need code**: a
      `Client::cancel_token()` watchdog that cancels the query after the deadline from
      another thread (pooler-independent, but a real feature) — deferred unless a
      concrete need appears.
  - Per-tenant credentials keep working: PgBouncer pools per (user, database) pair,
    so the inline multi-tenant model is preserved.
- Deployment shape: sidecar next to jsbox, or one pooler tier in front of Postgres.
  Either way it is configuration, not jsbox code.

### Redis / AMQP / HTTP — no action

- **Redis**: handshake is one round trip, no TLS by default; per-request connect is
  near-free. Proxies exist (twemproxy, Envoy's redis filter) but solve sharding/HA,
  not a problem jsbox has. Revisit only if `rediss://` + high volume shows up.
- **AMQP**: already amortized (one connection per send batch); brokers tolerate
  connection churn far better than Postgres.
- **HTTP**: the lost keep-alive reuse is real but small, and the per-request reqwest
  client exists because the redirect policy closes over per-request `allowed_hosts`
  (`http.rs`). Fixing it means restructuring redirect validation — only worth it if
  profiling ever shows connection setup dominating `api` latency.

## The rejected alternative: in-process named pools

Analyzed and shelved, recorded so the reasoning isn't re-derived later.

**Shape it would take** (if ever needed): operator-declared named pools in
`config.json`; requests reference them as `config.db = {"pool": "main-db"}` (inline
creds XOR pool reference, hard 400 otherwise — same rule as `script` XOR `key`). All
modes converge at `inject_apis`; the JS API (`db.query(...)`) is identical either
way. This is the Cloudflare Workers _bindings_ pattern, and it pairs naturally with
the script registry (both are named, deploy-time resources).

**Why it lost to PgBouncer:**

- **Session hygiene becomes jsbox's job.** A pooled connection carries `SET`
  variables, temp tables, prepared statements, and possibly an open transaction
  between checkouts. Requires `DISCARD ALL` on checkin + dirty-connection destruction
  — cost on every request to guard against rare contamination. PgBouncer does this
  for a living.
- **Checkout hold time = script wall-clock.** A script holds its connection for its
  full execution (up to `timeout_ms`, default 4 s), not just during queries —
  head-of-line blocking on the pool. PgBouncer in transaction mode holds a real
  connection only while a transaction runs.
- **Ops surface.** Pool sizing, health checks, idle eviction, reconnect storms after
  a DB restart. jsbox currently owns zero long-lived stateful resources and can be
  killed/restarted with impunity — worth protecting.
- **Multi-tenancy.** A shared pool collapses tenants onto one DB role; per-tenant
  pools re-create the connection-count problem at tenant granularity.

**Bar for revisiting:** profiling shows the _local_ hop to PgBouncer is itself a
bottleneck, or a capability appears with no mature external pooler and a demonstrably
expensive handshake. Until then: not worth the invariant risk.

## Related but independent: bound the concurrency

With or without any pooling, the storm generator is jsbox's unbounded execution
concurrency (`acquire()` fallback + ~512 blocking threads). A small, independent fix —
a semaphore on `/execute` sized to a multiple of the runtime pool, returning 429 (or
queueing briefly) past it — protects every downstream system, including PgBouncer
itself, and makes latency predictable under burst. Recommended as its own change
regardless of this decision.
