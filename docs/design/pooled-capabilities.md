# Pooled capabilities vs. per-execution connections

Companion to [script-registry.md](script-registry.md). A `pgbouncer` service
(transaction mode) ships in `docker-compose.yml`, and the integration suite runs the
full db section through it.

> **Behavioral contract → [`openspec/specs/db`](../../openspec/specs/db/spec.md) and
> [`openspec/specs/resilience`](../../openspec/specs/resilience/spec.md).** This note is the
> **rationale** for the per-execution-connection + external-pooler decision (the "why").

## Decision

Connection churn is solved with mature external infrastructure (PgBouncer and
equivalents), not in-process pooling. jsbox keeps its per-execution connection model —
the inline capability model is the only model. In-process named pools are deferred; the
bar for revisiting them is documented at the end.

The rationale is that serverless connection storms are best solved by a pooler _between_
the runtime and the database, not inside the runtime. AWS (RDS Proxy) and Cloudflare
(Hyperdrive) both reached this conclusion, and jsbox does not reinvent that mechanism.

## Cost of the per-execution model

Every I/O capability materializes per execution, inside `engine.rs::inject_apis`:

| Capability | Behavior                                                                                            | Per-request cost                                                         |
| ---------- | --------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------ |
| `db`       | `connect()` at inject time (`db.rs`) — TCP + optional TLS + Postgres auth, torn down at request end | ~1–5 ms LAN plain, 10–50 ms with TLS — often more than the script itself |
| `redis`    | `client.get_connection()` at inject time (`kv.rs`)                                                  | one handshake; cheap (no TLS) but nonzero                                |
| `amq`      | lazy: one connection + channel per `send` batch (`amq.rs`)                                          | per batch, already amortized                                             |
| `http`     | new reqwest `Client` per execution (`http.rs`)                                                      | client build + **no keep-alive reuse across executions**                 |

The sharper problem is **unbounded concurrency**. `JsPool::acquire()` creates a fresh
runtime when the pool is empty (`pool.rs`), and `spawn_blocking` allows ~512 threads. A
burst can run hundreds of concurrent executions, each opening its own Postgres
connection, multiplied by replicas — a connection storm against a database that defaults
to `max_connections = 100`.

## Adopted solution: external poolers

### Postgres → PgBouncer (or RDS Proxy / pgcat / Supavisor / Hyperdrive)

PgBouncer sits between jsbox and Postgres; `config.db.host` points at it.

- **Transaction pooling mode** is the right default for jsbox scripts: each script's
  queries run as independent statements/transactions, and `db.rs` sets
  `statement_timeout` per session. jsbox's "connect per request" becomes a connect to a
  _local_ pooler — microseconds, no TLS necessary on the loopback/sidecar hop — while
  PgBouncer maintains a small, steady set of real connections to Postgres.
- It addresses handshake latency (mostly), connection storms (entirely), Postgres
  `max_connections` exhaustion (entirely), and TLS renegotiation CPU on the DB host.
- Operator caveats:
  - Transaction mode forbids session state (named prepared statements, `SET` without
    `LOCAL`, advisory locks held across transactions, `LISTEN`). jsbox scripts get a
    fresh logical session per request anyway, so this matches the existing semantics —
    scripts never could rely on session state across requests.
  - `connect_timeout` is 5 s in `db.rs`; a saturated pooler queues connects, which
    surfaces as the retryable `DB_CONNECTION` error. This is correct behavior.
  - **`statement_timeout` is not robust through transaction mode.** jsbox issues a
    session-level `SET statement_timeout` at connect (`db.rs`); in transaction pooling
    that SET only sticks if the timed query happens to reuse the same server connection.
    It is enforced under an idle pool, but under concurrency the SET and the query can
    land on different server connections and the cap is silently lost — and jsbox's
    wall-clock interrupt cannot cancel a blocking libpq call, so a `pg_sleep`-style query
    could hold a `spawn_blocking` thread with no timeout.
    - **The startup-parameter approach does not work.**
      `postgres::Config::options("-c statement_timeout=…")` makes PgBouncer refuse the
      connection (`unsupported startup parameter in options: statement_timeout`), which
      breaks db connectivity entirely behind the pooler.
    - **The robust fix is operator-side** and survives any pooling with zero jsbox code:
      set the cap server-side where every server session inherits it —
      `ALTER ROLE <user> SET statement_timeout = '<ms>'`, a per-database default, or a
      PgBouncer `connect_query` / `server_reset_query`. The trade-off is that the
      per-request `config.db.statement_timeout_ms` is then not honored _through a
      transaction-mode pooler_; it is still honored on direct connections and in
      session-mode pooling, where the `SET` is left in place.
    - **True per-request enforcement through a transaction-mode pooler requires code:** a
      `Client::cancel_token()` watchdog that cancels the query after the deadline from
      another thread (pooler-independent, but a real feature). It is deferred unless a
      concrete need appears.
  - Per-tenant credentials keep working: PgBouncer pools per (user, database) pair, so
    the inline multi-tenant model is preserved.
- Deployment shape: sidecar next to jsbox, or one pooler tier in front of Postgres.
  Either way it is configuration, not jsbox code.

### Redis / AMQP / HTTP — no action

- **Redis**: the handshake is one round trip with no TLS by default, so per-request
  connect is near-free. Proxies exist (twemproxy, Envoy's redis filter) but solve
  sharding/HA, which jsbox does not need. Revisit only if `rediss://` plus high volume
  appears.
- **AMQP**: already amortized (one connection per send batch); brokers tolerate
  connection churn far better than Postgres.
- **HTTP**: the lost keep-alive reuse is real but small, and the per-request reqwest
  client exists because the redirect policy closes over per-request `allowed_hosts`
  (`http.rs`). Changing it means restructuring redirect validation — only worth it if
  profiling shows connection setup dominating `api` latency.

## Rejected alternative: in-process named pools

**Shape it would take** (if ever needed): operator-declared named pools in
`config.json`; requests reference them as `config.db = {"pool": "main-db"}` (inline creds
XOR pool reference, hard 400 otherwise — the same rule as `script` XOR `key`). All modes
converge at `inject_apis`; the JS API (`db.query(...)`) is identical either way. This is
the Cloudflare Workers _bindings_ pattern, and it pairs naturally with the script
registry (both are named, deploy-time resources).

**Why it loses to PgBouncer:**

- **Session hygiene becomes jsbox's job.** A pooled connection carries `SET` variables,
  temp tables, prepared statements, and possibly an open transaction between checkouts.
  This requires `DISCARD ALL` on checkin plus dirty-connection destruction — a cost on
  every request to guard against rare contamination. PgBouncer does this as its primary
  function.
- **Checkout hold time equals script wall-clock.** A script holds its connection for its
  full execution (up to `timeout_ms`, default 4 s), not just during queries — head-of-line
  blocking on the pool. PgBouncer in transaction mode holds a real connection only while
  a transaction runs.
- **Ops surface.** Pool sizing, health checks, idle eviction, and reconnect storms after
  a DB restart all become jsbox concerns. jsbox owns zero long-lived stateful resources
  and can be killed or restarted with impunity — a property worth protecting.
- **Multi-tenancy.** A shared pool collapses tenants onto one DB role; per-tenant pools
  re-create the connection-count problem at tenant granularity.

**Bar for revisiting:** profiling shows the _local_ hop to PgBouncer is itself a
bottleneck, or a capability appears with no mature external pooler and a demonstrably
expensive handshake. Until then, the invariant risk is not worth taking on.

## Related but independent: bound the concurrency

With or without any pooling, the storm generator is jsbox's unbounded execution
concurrency (`acquire()` fallback plus ~512 blocking threads). A small, independent
measure — a semaphore on `/execute` sized to a multiple of the runtime pool, returning
429 (or queueing briefly) past it — protects every downstream system, including PgBouncer
itself, and makes latency predictable under burst. It stands as its own change,
independent of this decision.
