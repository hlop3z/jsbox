# Resilience: timeouts, bulkheads, and cancellation

Companion to [pooled-capabilities.md](pooled-capabilities.md).

> **Behavioral contract → [`openspec/specs/resilience`](../../openspec/specs/resilience/spec.md).**
> The testable requirements for each tier (clamp, bulkhead, deadline, breaker, partition
> fairness) live there. This note is the **rationale**: why each layer exists, the async-Rust
> pitfalls it navigates, and the measured A/B results — the "why" a spec doesn't carry.

## The principle

Enterprise-grade resilience for a timeout is never a single mechanism — it is **defense in
depth with a server-enforced ceiling the client cannot raise**. Any single timeout can be
lost or bypassed: a `SET` is lost through a transaction-mode pooler; a wall-clock interrupt
cannot cancel a blocking syscall. So each layer catches what the one below it missed, and
when a timeout _does_ fire the failure stays contained instead of cascading into an SLO
breach. The layers are introduced in order of resilience-per-effort.

## The failure this defends against

jsbox runs the synchronous `postgres` client on `tokio::task::spawn_blocking`, and the
`QuickJS` wall-clock interrupt **cannot cancel a blocking libpq call**. A slow or hung query
therefore pins a `spawn_blocking` thread (default ceiling ~512) and a DB connection until
_some_ server-side cap fires — and `SET statement_timeout` is best-effort through a
transaction-mode pooler (see [pooled-capabilities.md](pooled-capabilities.md)). Under load
against a degraded database, jsbox's **unbounded** execution concurrency (`JsPool::acquire()`
mints a fresh runtime when the pool is empty) means hundreds of executions each pin a thread
plus a connection: the service collapses before any per-query timeout matters. That is the
SLO killer, and it is independent of the timeout question.

## The layered model

| Tier | Layer                                     | What it guarantees                                                                            |
| ---- | ----------------------------------------- | --------------------------------------------------------------------------------------------- |
| 0    | **Server-side ceiling + clamp**           | A `statement_timeout` cap the request cannot raise; jsbox never _issues_ an unbounded one     |
| 1    | **Bulkhead (bounded concurrency)**        | A slow downstream can't consume all threads/connections; excess load fast-fails 429           |
| 2    | **Client deadline + active cancellation** | A hung query is cancelled and its thread/connection freed promptly, independent of any pooler |
| 3    | **Circuit breaker**                       | A struggling DB isn't buried under retries; jsbox stays responsive                            |
| 4    | **Pooler timeouts**                       | PgBouncer `query_timeout`/`query_wait_timeout` as an independent layer                        |
| 5    | **Per-partition fairness**                | One key can't monopolize a pod (per-pod cap; global fairness is the gateway's job)            |

### Tier 0 — server-side ceiling + clamp

Tier 0 has two parts:

- **Robust ceiling (operator, pooler-proof):** `statement_timeout` set as a Postgres
  role/database default — `ALTER ROLE app SET statement_timeout = '30s'`. It is applied to
  every backend at connection start, so it survives PgBouncer transaction mode and every
  pooler, and no client can escape it. This is the authoritative floor.
- **jsbox clamp (code, defense-in-depth):** `engine.max_statement_timeout_ms` (`0` = off). A
  per-request `config.db.statement_timeout_ms` is clamped to it, and a request value of `0`
  ("unlimited") becomes the ceiling. This guarantees jsbox never _sends_ an unbounded `SET`,
  which fully bounds direct connections and session-mode pooling. Behind a transaction-mode
  pooler the `SET` is best-effort, so the clamp there is belt-and-suspenders to the operator
  role default — not a replacement.

The per-request `config.db` is set by the _trusted_ caller of `/execute`, not by the
sandboxed script (a script cannot set `statement_timeout`). The clamp is defense in depth: it
lets the operator _running jsbox_ guarantee a ceiling no caller can exceed, which matters as
the platform moves toward customer-authored scripts and multi-tenancy.

### Tier 1 — bulkhead

`engine.max_concurrent_executions` bounds in-flight executions via a `tokio::sync::Semaphore`
held across the `spawn_blocking` span (`0` = auto = `pool_size × 16`, auto-enabled because
unbounded concurrency is a latent bug with no upside). Acquisition is **fast-fail**: when
saturated, `/execute` returns `429 OVERLOADED` (runtime, retryable, owner operator)
immediately rather than queueing — moving the latency problem into a queue only defers the
SLO breach. Fast-fail plus client backoff beats unbounded buffering. Cheap validation errors
(malformed body, oversize, unknown key) return _before_ taking a permit, so a flood of bad
requests cannot exhaust the bulkhead. The bound is tuned to the downstream connection budget
(PgBouncer `max_client_conn`, DB `max_connections`); a per-capability tighter bound (e.g.
DB-only) is a later refinement.

### Tier 2 — client deadline + active cancellation

The root limitation is the **blocking client on `spawn_blocking`**: the `QuickJS` wall-clock
interrupt cannot cancel a blocking libpq call, so a hung query pinned its thread until _some_
server-side cap fired — and `SET statement_timeout` is lost through a transaction-mode
pooler. `db.rs` uses **async `tokio-postgres`**: each query runs as
`handle.block_on(tokio::time::timeout(deadline, fut))` where `deadline` is anchored to the
execution wall-clock budget. On elapse the future is dropped and a retryable `DB_TIMEOUT` is
returned, freeing the blocking thread regardless of any server-side timeout.

Cancellation is honored **by connection teardown, not an explicit cancel token**, and this is
correct _because of jsbox's model_: connections are per-request and never pooled in-process,
so dropping the timed-out query (and, at execution end, the `Client`) closes the socket and
the backend aborts the query. The rule that you must send an explicit cancel applies to
_kept/pooled_ connections intended for reuse — jsbox discards, so teardown _is_ the
cancellation. An explicit `Client::cancel_token()` for promptness is a possible refinement,
deferred.

With no operator ceiling and `statement_timeout=0` (unlimited server-side), a
`SELECT pg_sleep(30)` _through PgBouncer_ returns `DB_TIMEOUT` at ~4.1s (the engine
wall-clock budget) instead of blocking ~30s — the thread is freed by the client-side deadline
alone. The full suite (152 tests) stays green across direct Postgres, PgBouncer, and
CockroachDB.

Implementation respects the async-Rust principles below: `block_on` is only ever called from
the `spawn_blocking` thread (never a runtime worker — that would panic); the multi-thread
runtime drives the connection task on a worker while the blocking thread is parked in
`block_on`; and CPU-bound `QuickJS` execution stays on `spawn_blocking` (the JS context never
crosses an `.await`).

### Tier 3 — per-target db circuit breaker

Tiers 1–2 keep a single hung query from cascading, but a database that is _down_ or flapping
is still a slow-path tax: every request pays the 5 s connect timeout on a `spawn_blocking`
thread before failing, and N concurrent requests bury the recovering target under a
thundering herd of reconnects. Tier 3 short-circuits that. A breaker keyed per **target**
(`host:port`, operator-supplied in `config.db`, so the key set is small and bounded) counts
consecutive connect failures; at `db_breaker_threshold` it trips **open** and fast-fails
subsequent calls to that target with a retryable `capability/db/DB_CIRCUIT_OPEN` (no connect
attempted, no timeout wait) for `db_breaker_cooldown_ms`. After the cool-down a single
request probes (**half-open**): success closes the breaker, failure re-opens it — the
`allow()` check re-arms the open window so only one caller probes while the rest keep
fast-failing. State is a mutex-guarded map that **fails open** on lock poisoning (a breaker
bug must never wedge the service). Opt-in via `db_breaker_threshold` (0 = off, the default).
Implementation: `src/breaker.rs`, gated in `db::connect_through_breaker`.

This is deliberately a _connect_ breaker, not a per-query latency breaker — the failure it
targets is a dead/unreachable target, where Tier 2's deadline already bounds a _slow_ live
query. It is per-pod like the bulkhead: each replica learns the target's health
independently, the right granularity since connect failures are observed locally.

Measured with `stress_breaker_esm.py` (16 concurrent against a black-holed DB whose TCP SYN
is dropped so every connect pays the 5 s timeout; bulkhead sized to concurrency so the
breaker is the only variable):

|             | A — breaker off                                                        | B — breaker on (`threshold=3`)                                    |
| ----------- | ---------------------------------------------------------------------- | ----------------------------------------------------------------- |
| throughput  | 5.3 req/s                                                              | **288.7 req/s** (54× higher)                                      |
| latency p99 | 5.02 s                                                                 | **0.02 s** (281× lower)                                           |
| outcome     | every request pins a `spawn_blocking` thread 5 s, then `DB_CONNECTION` | 17 connects trip it, then 1715 fast-fail `DB_CIRCUIT_OPEN` in ~ms |

Under a dead database the breaker turns a 5 s-per-request thread-pinning stall into an
instant retryable fast-fail — the difference between a pod that falls over and one that stays
responsive while the target recovers.

### Tier 4 — pooler timeouts

PgBouncer's own `query_timeout` / `query_wait_timeout` form an independent layer **below**
jsbox. There is no jsbox code to write — the point of Tier 4 is precisely that it does not
depend on jsbox. Tier 0's `SET statement_timeout` is best-effort through a transaction-mode
pooler (the SET can bind to a different server connection than the autocommit query that
follows — see [pooled-capabilities.md](pooled-capabilities.md)). `query_timeout` is the
pooler enforcing its **own** ceiling on every query it proxies, so a runaway query is a
guaranteed kill even when the session SET was lost — and it fires below jsbox's wall-clock
deadline (Tier 2), the final backstop. `query_wait_timeout` similarly bounds time spent
waiting for a pooled server connection, a failure mode (pool-queue starvation) that
`statement_timeout` does not address at all.

The reference deployment sets `query_timeout` slightly above the expected `statement_timeout`
so the server-side cap fires first when present and the pooler catches what leaks through.
The test compose configures `query_timeout = 2` on the PgBouncer service (above normal query
latency, below jsbox's 4 s wall clock), and the integration suite asserts a `pg_sleep(3)`
through the pooler is terminated below that deadline — so the layer is exercised in CI, not
just documented. It is a "dangerous timeout" (it will cancel a legitimately long query), so
the value is operator-tuned to the workload's real p99.

### Tier 5 — per-partition fairness

The global bulkhead (Tier 1) protects a pod but sheds load _indiscriminately_: the A/B
harness measured a good tenant being rejected alongside a noisy one (the victim partition
succeeded on zero requests). Tier 5 adds a per-key concurrency cap underneath the global one.
A request carries a **partition key** (`X-Partition-Key` header, or a `partition` body field
— header wins; both caller-set, never script-set), and a key over its share fast-fails
`429 PARTITION_OVERLOADED` _even when global capacity remains_. The key is whatever the
operator/gateway chooses to isolate on — a tenant, an API key, a route. Keys hash into a
fixed array of semaphores (`PartitionLimiter`), so memory is constant and there is no per-key
lifecycle; a single pod never holds more than the bulkhead's worth of _concurrent_ keys, so
collisions stay rare no matter how many keys exist overall. Opt-in via
`max_concurrent_per_partition` (0 = off).

This is a _per-pod_ control, not a global guarantee. Under k8s with N replicas behind a load
balancer, the effective ceiling is per-pod × N, and it drifts as the HPA scales. **Global
per-partition fairness belongs at the gateway** — the one component with the fleet-wide view
of a key, and the place built for millions of tenants (rate limiting, often Redis-backed).
jsbox's role is local self-protection: the global bulkhead stops the pod falling over; the
per-partition cap stops one key monopolizing a _single_ pod under sticky routing, hot-key
skew, or a gateway gap. The reference deployment is therefore: **gateway = global per-key
policy (rejects over-quota before fan-out); jsbox = per-pod bulkhead + per-pod partition
backstop.** A true global limit enforced _in_ jsbox would require a shared store (Redis) on
the hot path — feasible but adds a dependency, latency, and crash-leak handling; it is an
opt-in path, not the default.

### Observability — `GET /metrics`

The resilience tiers are only operable if they can be _seen_ firing. jsbox exposes a
dependency-free Prometheus text endpoint (`src/metrics.rs`, no client library) with
process-wide atomic counters incremented on each request's terminal outcome, plus live gauges
read at scrape time: `jsbox_executions_total{outcome}` (success / script_error /
capability_error / timeout / memory_limit / malformed_response / internal_error),
`jsbox_rejections_total`, `jsbox_overload_total{scope}` (global bulkhead vs partition cap),
`jsbox_db_breaker_trips_total`, and `jsbox_bulkhead_permits_available` / `_total`. Shed load
(Tier 1/5), breaker trips (Tier 3), and bulkhead headroom are all alertable without log
parsing. Execution wall-clock latency is exposed as a Prometheus histogram
(`jsbox_execution_duration_seconds`, fixed buckets from 1 ms to 10 s) covering every
execution that ran, so SLO latency objectives (p50/p95/p99 via `histogram_quantile`) are
computed at the dashboard, not pre-baked. The buckets are integer-microsecond comparisons on
the hot path (no float), and the implicit `+Inf` bucket plus `_sum`/`_count` follow the
standard exposition. Per-capability op latency is exposed the same way as a single labeled
family
`jsbox_capability_op_duration_seconds{capability="db"|"http"|"mail"|"s3"|"redis"|"amq"|"auth"}`
(fed from the per-op `duration_us` already drained into `meta`), so a slow _downstream_ is
attributable — not just a slow total execution.

## Async-in-Rust principles

The Tier 2 design is where async-DB code most commonly goes wrong. The rules it follows:

1. **Timeout ≠ cancellation.** `tokio::time::timeout` around a query future cancels the
   _await_, not the work — the database keeps running the query and holding the row locks. The
   Postgres cancel request must also be sent. Code that skips this believes it has a timeout
   and does not; it is the single most common async-DB mistake.
2. **Cancellation is cooperative, not preemptive.** A dropped future stops at the next await
   point; a blocking call mid-future is never interrupted. This is _why_ a blocking model
   cannot be timed out from outside, and why Tier 2 needs genuine async I/O, not a `timeout()`
   wrapper around blocking work.
3. **A cancelled connection is dirty.** Dropping a query future mid-flight can leave the
   connection in an unknown protocol state (half-read results). It must be reset or discarded,
   never returned to a pool for reuse — silent reuse corrupts the next query.
4. **Never block the async runtime.** CPU-bound `QuickJS` execution stays on `spawn_blocking`
   even when DB I/O is async — a hybrid model. Calling blocking libpq on async runtime threads
   starves the executor; this is the canonical "the service froze under load" async-Rust bug.
5. **Bound everything; backpressure is not optional.** Async makes it trivial to accept
   unbounded work (spawn-per-request, unbounded channels) and OOM under load. Every queue is
   bounded, every fan-out has a ceiling (Tier 1 is the first instance).
6. **`Send`/`!Send` across `.await`.** rquickjs `Context` is `!Send`, part of why execution is
   isolated to a blocking task — any async refactor must respect this and keep the JS context
   off the async boundary.

## Validation: A/B stress testing (`stress_test.py`)

The model is measured, not trusted on reasoning alone. `stress_test.py` manages the server
lifecycle itself (one variant at a time, so config is the only variable), floods `/execute`
with concurrent slow queries through PgBouncer, and interleaves a "victim" — a well-behaved
partition's normal fast query — to measure noisy-neighbor impact.

### Tier 0+1 (40 concurrent, `pg_sleep(2)` via PgBouncer, ~8 s)

|                             | A (baseline)                            | B (Tier 0+1)                              |
| --------------------------- | --------------------------------------- | ----------------------------------------- |
| flood latency p99           | **8.01 s**                              | **0.04 s** (228× lower)                   |
| shed as 429                 | 0% (queues into the pool)               | 100% (fails fast)                         |
| flood outcomes              | 28 OK, 48 `DB_TIMEOUT`, 9 `DB_CANCELED` | 5 OK, 54 `DB_CANCELED`, rest `OVERLOADED` |
| victim (good partition) p99 | 5.56 s                                  | 0.03 s                                    |
| victim succeeded / shed     | 0 / 0 (dragged into the queue)          | 0 / 28 (shed by the global bulkhead)      |

Under overload, A piles every request into the saturated PgBouncer pool — tail latency climbs
to 8 s (worse than the 4 s engine timeout, because even _connecting_ queues) and most
requests time out. B sheds the excess as instant 429s and holds p99 at 40 ms. The victim
result (measured against the Tier 0+1-only build) exposes a real gap: that bulkhead is
_global_, so it rejects the good partition alongside the flood (0 succeeded, 28 shed) —
bounding the victim's latency but not letting it through. That gap is the concrete motivation
for Tier 5.

### Tier 5 closes the gap

The harness tags the flood as partition `noisy` and the victim as `good`; with
`max_concurrent_per_partition` set, the noisy partition sheds on its _own_ cap
(`PARTITION_OVERLOADED`) while the good one keeps its share. Re-running the same scenario with
Tier 5 enabled, victim requests succeeded — A: 0, B: 18 (A drags the good partition to p99
5.6 s and lets 0 through; B lets 18 through at p99 0.09 s). The integration suite asserts the
same mechanism (a noisy partition sheds `PARTITION_OVERLOADED` while a good one gets through).

### Harness design (variants, knobs, hypotheses)

- **Variants.** A = baseline (bulkhead disabled, `max_concurrent_executions` very high, no
  clamp) vs B = Tier 0+1 enabled. Same build, config-flagged, so the only variable is the
  defense.
- **Fault injection.** Load drives against a _degraded_ database: latency injected via
  `pg_sleep` in the script, or a proxy (toxiproxy) adding latency/packet loss between jsbox
  and Postgres/PgBouncer, with PgBouncer in the loop.
- **Load.** A concurrent generator (k6 / vegeta / a small async client) ramps past capacity
  against `/execute`.
- **Metrics.** p50/p95/p99 latency, error-rate by code (especially `429 OVERLOADED` vs
  timeouts vs 5xx), throughput, `spawn_blocking` thread occupancy, DB/PgBouncer connection
  saturation, and recovery time after the database is restored.
- **Hypotheses.**
  - H1 (Tier 1): under overload, B sheds excess as fast 429s and holds p99 + availability
    within SLO, while A collapses (thread/connection exhaustion, unbounded latency, slow
    recovery).
  - H2 (Tier 0): a request asking for a huge/zero `statement_timeout` is capped at the
    operator ceiling in B; uncapped in A.
  - H3 (Tier 2): during a hung query, B (async cancel) frees the connection + thread within
    the deadline, while A (blocking) holds them until the server cap fires.
- **No silent caps in the harness.** A backend expected to be up that is unreachable must fail
  the run loudly, not skip — a regression that hides as a skip is how the PgBouncer-
  connectivity break (see [pooled-capabilities.md](pooled-capabilities.md)) nearly slipped
  through.
