# Design note: resilience for SLO/SLA (timeouts, bulkheads, cancellation)

Status: **Tier 0 + Tier 1 + Tier 2 implemented** (2026-06); Tiers 3–5 planned.
Companion to [pooled-capabilities.md](pooled-capabilities.md). Grounded in the code as
of `main`.

## The principle

Enterprise resilience for a timeout is never a single mechanism — it is **defense in
depth with a server-enforced ceiling the client cannot raise**. Any single timeout can
be lost or bypassed (a `SET` lost through a transaction-mode pooler; a wall-clock
interrupt that can't cancel a blocking syscall), so each layer catches what the one
below it missed, and when a timeout *does* fire the failure stays contained instead of
cascading into an SLO breach. Ship the layers in order of resilience-per-effort.

## The failure this defends against

jsbox runs the synchronous `postgres` client on `tokio::task::spawn_blocking`, and the
`QuickJS` wall-clock interrupt **cannot cancel a blocking libpq call**. So a slow or
hung query pins a `spawn_blocking` thread (default ceiling ~512) and a DB connection
until *some* server-side cap fires — and `SET statement_timeout` is best-effort through
a transaction-mode pooler (see pooled-capabilities.md). Under load against a degraded
database, jsbox's **unbounded** execution concurrency (`JsPool::acquire()` mints a fresh
runtime when the pool is empty) means hundreds of executions each pin a thread + a
connection: the service collapses before any per-query timeout matters. That is the SLO
killer, and it is independent of the timeout question.

## The layered model

| Tier | Layer | What it guarantees | Status |
| --- | --- | --- | --- |
| 0 | **Server-side ceiling + clamp** | A `statement_timeout` cap the request cannot raise; jsbox never *issues* an unbounded one | **done** (clamp) + operator role-default (docs) |
| 1 | **Bulkhead (bounded concurrency)** | A slow downstream can't consume all threads/connections; excess load fast-fails 429 | **done** |
| 2 | **Client deadline + active cancellation** | A hung query is cancelled and its thread/connection freed promptly, independent of any pooler | **done** (async `db`) |
| 3 | **Circuit breaker** | A struggling DB isn't buried under retries; jsbox stays responsive | planned |
| 4 | **Pooler timeouts** | PgBouncer `query_timeout`/`query_wait_timeout` as an independent layer | operator config |
| 5 | **Observability + per-tenant fairness** | SLOs are measurable; one tenant can't starve another | partial (metrics) → planned |

### Tier 0 — server-side ceiling + clamp (done + operator action)

Two parts:

- **Robust ceiling (operator, pooler-proof):** set `statement_timeout` as a Postgres
  role/database default — `ALTER ROLE app SET statement_timeout = '30s'`. Applied to
  every backend at connection start, so it survives PgBouncer transaction mode and
  every pooler, and no client can escape it. This is the authoritative floor.
- **jsbox clamp (code, defense-in-depth):** `engine.max_statement_timeout_ms`
  (`0` = off). A per-request `config.db.statement_timeout_ms` is clamped to it, and a
  request value of `0` ("unlimited") becomes the ceiling. This guarantees jsbox never
  *sends* an unbounded `SET`, which fully bounds direct connections and session-mode
  pooling. Behind a transaction-mode pooler the `SET` is best-effort, so the clamp
  there is belt-and-suspenders to the operator role default — not a replacement.

Security framing: today the per-request `config.db` is set by the *trusted* caller of
`/execute`, not by the sandboxed script (a script cannot set `statement_timeout`). The
clamp is defense in depth — it lets the operator *running jsbox* guarantee a ceiling no
caller can exceed, which matters as the platform moves toward customer-authored scripts
and multi-tenancy.

### Tier 1 — bulkhead (done)

`engine.max_concurrent_executions` bounds in-flight executions via a `tokio::sync::
Semaphore` held across the `spawn_blocking` span (`0` = auto = `pool_size × 16`,
auto-enabled because unbounded concurrency is a latent bug with no upside). Acquisition
is **fast-fail**: when saturated, `/execute` returns `429 OVERLOADED` (runtime,
retryable, owner operator) immediately rather than queueing — moving the latency
problem into a queue just defers the SLO breach. Big-company practice is fast-fail +
client backoff over unbounded buffering. Cheap validation errors (malformed body,
oversize, unknown key) return *before* taking a permit, so a flood of bad requests
can't exhaust the bulkhead. Tune the bound to the downstream connection budget
(PgBouncer `max_client_conn`, DB `max_connections`); a per-capability tighter bound
(e.g. DB-only) is a later refinement.

### Tier 2 — client deadline + active cancellation (done; async `db`)

The root limitation was the **blocking client on `spawn_blocking`**: the `QuickJS`
wall-clock interrupt cannot cancel a blocking libpq call, so a hung query pinned its
thread until *some* server-side cap fired — and `SET statement_timeout` is lost through a
transaction-mode pooler. `db.rs` now uses **async `tokio-postgres`**: each query runs as
`handle.block_on(tokio::time::timeout(deadline, fut))` where `deadline` is anchored to
the execution wall-clock budget. On elapse the future is dropped and a retryable
`DB_TIMEOUT` is returned, freeing the blocking thread regardless of any server-side
timeout.

Cancellation is honored **by connection teardown, not an explicit cancel token**, and
this is correct *because of jsbox's model*: connections are per-request and never pooled
in-process, so dropping the timed-out query (and, at execution end, the `Client`) closes
the socket and the backend aborts the query. The "you must send the cancel" lesson
applies to *kept/pooled* connections you intend to reuse — jsbox discards, so teardown
*is* the cancellation. (An explicit `Client::cancel_token()` for promptness is a possible
refinement, deferred.)

**Verified** (2026-06): with no operator ceiling and `statement_timeout=0` (unlimited
server-side), a `SELECT pg_sleep(30)` *through PgBouncer* returned `DB_TIMEOUT` at
**~4.1s** (the engine wall-clock budget) instead of blocking ~30s — the thread is freed
by the client-side deadline alone. The full suite (152 tests) stays green across direct
Postgres, PgBouncer, and CockroachDB.

Implementation notes that honor the async-Rust lessons below: `block_on` is only ever
called from the `spawn_blocking` thread (never a runtime worker — that would panic); the
multi-thread runtime drives the connection task on a worker while the blocking thread is
parked in `block_on`; and the CPU-bound `QuickJS` execution stays on `spawn_blocking`
(the JS context never crosses an `.await`).

### Tier 3–5 — breaker, pooler timeouts, observability + fairness (planned)

Circuit breaker (trip on DB error/latency, fast-fail retryable during cool-down);
PgBouncer's own `query_timeout`; SLO metrics (latency histograms, timeout/cancel/
breaker/saturation counters — extend the existing per-op `duration_us` drain); and
per-tenant concurrency quotas so one customer can't starve another (a hard requirement
for big-company multi-tenancy, not a nice-to-have).

## Learning from big companies' async-in-Rust mistakes

The Tier 2 refactor is where teams get hurt. Rules we adopt up front:

1. **Timeout ≠ cancellation.** `tokio::time::timeout` around a query future cancels the
   *await*, not the work — the database keeps running the query and holding the row
   locks. You **must** also send the Postgres cancel request. Teams that skip this think
   they have a timeout and don't; it's the single most common async-DB mistake.
2. **Cancellation is cooperative, not preemptive.** A dropped future stops at the next
   await point; a blocking call mid-future is never interrupted. This is *why* the
   current blocking model can't be timed out from outside, and why Tier 2 needs genuine
   async I/O, not a `timeout()` wrapper around blocking work.
3. **A cancelled connection is dirty.** Dropping a query future mid-flight can leave the
   connection in an unknown protocol state (half-read results). It must be reset or
   discarded, never returned to a pool for reuse — silent reuse corrupts the next query.
4. **Never block the async runtime.** CPU-bound `QuickJS` execution stays on
   `spawn_blocking` even if DB I/O goes async — a hybrid model. Calling blocking libpq
   on async runtime threads starves the executor; this is the canonical "my service
   froze under load" async-Rust bug.
5. **Bound everything; backpressure is not optional.** Async makes it trivial to accept
   unbounded work (spawn-per-request, unbounded channels) and OOM under load. Every
   queue is bounded, every fan-out has a ceiling (Tier 1 is the first instance).
6. **`Send`/`!Send` across `.await`.** rquickjs `Context` is `!Send`, part of why
   execution is isolated to a blocking task — any async refactor must respect this and
   keep the JS context off the async boundary.

## Validation: A/B stress testing (planned — the next step)

We do not trust the model on reasoning alone; we measure it. The plan:

- **Variants.** A = baseline (bulkhead disabled, `max_concurrent_executions` very high,
  no clamp) vs B = Tier 0+1 enabled. Same build, config-flagged, so the only variable
  is the defense.
- **Fault injection.** Drive load against a *degraded* database: latency injected via
  `pg_sleep` in the script, or a proxy (toxiproxy) adding latency/packet loss between
  jsbox and Postgres/PgBouncer, with PgBouncer in the loop.
- **Load.** Concurrent generator (k6 / vegeta / a small async client) ramping past
  capacity against `/execute`.
- **Metrics.** p50/p95/p99 latency, error-rate by code (esp. `429 OVERLOADED` vs
  timeouts vs 5xx), throughput, `spawn_blocking` thread occupancy, DB/PgBouncer
  connection saturation, and **recovery time** after the database is restored.
- **Hypotheses to confirm.**
  - H1 (Tier 1): under overload, B sheds excess as fast 429s and holds p99 + availability
    within SLO, while A collapses (thread/connection exhaustion, unbounded latency, slow
    recovery).
  - H2 (Tier 0): a request asking for a huge/zero `statement_timeout` is capped at the
    operator ceiling in B; uncapped in A.
  - H3 (future Tier 2): during a hung query, B (async cancel) frees the connection +
    thread within the deadline, while A (blocking) holds them until the server cap fires.
- **No silent caps in the harness.** A backend expected to be up that is unreachable
  must fail the run loudly, not skip — a regression that hides as a skip is how the
  PgBouncer-connectivity break (see pooled-capabilities.md) nearly slipped through.
