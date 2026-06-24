# Resource egress: decoupling the box from vendor drivers

Companion to [resilience.md](resilience.md) and [network-fabric.md](network-fabric.md).

> **Status: design / in progress.** This is the rationale and the staged plan for moving the
> driver-backed I/O capabilities (`db`, `mongo`, `mail`, `redis`, `amq`, `auth`) out of
> `runlet-core` and behind a single network egress port, leaving the core as
> deterministic-compute + pure-signing capabilities + one `Resource` seam. The behavioral
> contract for the seam will land in `openspec/specs/` as the implementation settles.

## The principle

A sandbox that runs untrusted JavaScript should link as little native code as possible. Every
vendored driver (`tokio-postgres`, `mongodb` + `mongocrypt`, an SMTP client, `redis`,
`async-nats`, an OIDC stack) is attack surface, a supply-chain audit obligation
(`cargo vet`/`cargo deny`), and a second crypto stack risk (the `ring`/`aws-lc` duplication we
already fight). The box's actual job is **execute logic safely and cheaply**; talking to
Postgres is not its job, it is a *dependency* of the logic.

So: the box should depend on a **stable egress port**, not on a pile of drivers. Each external
resource is reached as `resource.call("db", action, payload, binding)` — a string-in /
string-out call that crosses a process boundary to a sidecar (`fabricd`) that holds the
drivers. Whether that sidecar reaches Postgres directly or routes across a global mesh is the
sidecar's problem, invisible to the box.

This mirrors the move already made on the inbound side: [`LogicHost`](../../crates/runlet-core/src/host.rs)
is a callable port (`Invocation → Outcome`) that knows nothing about HTTP. `Resource` is the
symmetric port on the I/O side.

## The realization: the capabilities are already a wire protocol

Decoupling is **"swap the function body for a network call"**, not a rewrite, because the
capability pattern is *already* a string-in/string-out FFI with a wire-ready error contract:

- The native function (`db.rs`'s `__db`, `mail.rs`'s `__mail`, …) takes
  `(action: String, payload: String, …) -> String`. JSON in, JSON out. No rich type crosses
  the QuickJS boundary today.
- The **error path is already a wire format**: a capability throws a `__jsbox`-tagged JSON
  object (`{code, retryable, source, owner, details}`) that
  [`engine.rs::read_capability_tag`](../../crates/runlet-core/src/engine.rs) parses back into a
  typed `CapabilityErr`. That tag is a protocol that currently travels zero distance.
- The **resilience pattern is already remote-shaped**: `db.rs::block_on_db` runs
  `handle.block_on(timeout(deadline, fut))` on the `spawn_blocking` thread, so a hung call is
  bounded by the wall-clock budget. A QUIC round-trip slots into the exact same shape.

So the only thing that changes is *what is inside the closure*: `dispatch(&call, …)` →
`tokio_postgres` becomes `resource.call("db", action, payload, binding)` → sidecar. The JS
wrapper (`db.js`), the metrics collector, `check_op_limit`/`record`, and the `__jsbox`
envelope are all unchanged.

## The seam: a `Resource` egress port (mirror of `ReadHook`)

`runlet-core` already has the deterministic-side precedent:
[`ReadHook`](../../crates/runlet-core/src/engine.rs) — a consumer-supplied
`dyn Fn(&str) -> Result<String, String> + Send + Sync` injected as the `read()` global, with
the core staying domain-agnostic about what is read. The I/O egress is the same move:

```rust
/// Consumer-supplied I/O egress. The core knows nothing about Postgres/SMTP/QUIC — it only
/// forwards (capability, action, payload) and surfaces the string result or a tagged error.
pub trait Resource: Send + Sync {
    fn call(
        &self,
        cap: &str,           // logical capability: "db", "mail", "mongo", …
        action: &str,        // "query", "execute", "send", "find_one", …
        payload_json: &str,  // the script's JSON arguments (untrusted)
        binding: &ResourceBinding, // operator-resolved target; never script-reachable
    ) -> Result<String, ResourceError>;
}
```

Wiring (all symmetric to `read_hook`):

- `Option<Arc<dyn Resource>>` on `Invocation` → threaded into `ExecParams`.
- A **single** generic native function `__resource(cap, action, payload_json) -> String`
  replaces the N per-capability native functions. The per-capability JS wrappers stay
  (`db.js`, `mail.js`, …) — they give the discoverable `db.query()` / `mail.send()` surface and
  just call `__resource("db", "query", …)` underneath.
- The call body is `block_on(timeout(deadline, quic_roundtrip))` — **identical to
  `block_on_db`**. The wall-clock-bounded-egress guarantee comes for free.
- `inject_apis` collapses from eight `#[cfg(feature = …)]` driver branches to: *if a `Resource`
  egress is wired and `Profile::Full`, inject `__resource` plus whichever JS wrappers the
  invocation's binding permits.* Most of the `db`/`mongo`/`mail`/… cfg matrix in `host.rs`,
  `engine.rs`, `ExecParams`/`ExecResult`/`Collectors` **deletes**.

The error contract needs no new code on the box side: `fabricd` returns errors in the same
`{code, retryable, source, owner, details}` shape, and the existing
`classify_throw`/`read_capability_tag` path consumes it unchanged.

## The trust-model fix (the security-critical part)

Today the *operator connection* rides in per-request config — `DbConfig { host, user,
password, … }` inside `CapabilitySet`. In the egress model the **script must never carry or
even name a real endpoint or credential.** The binding inverts:

| | Before | After |
| --- | --- | --- |
| Request carries | `db: { host: "10.2.4.18", password: "…" }` | `resources: ["orders-db", "billing-db"]` (logical names) |
| Endpoint + creds resolved by | the caller, passed through the box | `fabricd`, from operator config the box never sees |
| Box enforces | size/shape of config | *which logical names this invocation may address* |

This is the existing [two-trust-models](../../CLAUDE.md) rule made physical. `db`/`mail` were
"trusted because operator-supplied"; now that operator supply lives in `fabricd`, and the box's
job shrinks to a per-invocation **allowlist of logical resource names** — exactly how
`allowed_hosts` already gates the `api` client. `http` stays in-box and stays SSRF-guarded,
because its target is still script-controlled.

`ResourceBinding` is the allowlist entry (logical name + an optional capability-scoped token),
resolved by the consumer and opaque to the core. The originally-sketched
`fn("db.path", js_json, internal_dev_settings_json)` maps to `__resource(cap, payload,
binding)` where `internal_dev_settings_json` is operator-bound and unreachable from JS.

## Stays in-box vs. moves to `fabricd`

| Stays (pure compute / signing — no vendor driver) | Moves to `fabricd` (heavy driver) |
| --- | --- |
| `$`/`Decimal`, determinism shims, `emit`, `read` | `db` (`tokio-postgres`) |
| `http` (`reqwest` + rustls, SSRF-guarded) | `mongo` (`mongodb` + `mongocrypt`) |
| `s3` **presign** (`upload_url` — SigV4 signing, no network) | `mail` (SMTP), `redis`, `amq`, `auth` (OIDC) |

Result: `runlet-core`'s default build collapses toward deterministic-core + the `Resource`
trait; the `mongocrypt`/`ring`/`aws-lc` audit tail and most of the cargo-feature matrix
evaporate. A consumer that wants raw in-process drivers can still supply a `Resource` impl that
calls them directly (see step 2).

## Where resilience lands

- **Box keeps:** the wall-clock deadline on the egress (a hung `fabricd` cannot pin a
  `spawn_blocking` thread — same guarantee as `block_on_db` today) and **one** circuit breaker
  keyed by *resource name* (replaces the per-`db` breaker; fast-fails when a backend is down).
- **`fabricd` owns:** backend-specific resilience — connection pooling, `statement_timeout`,
  driver retries. Strictly cleaner than today's per-capability breaker, and it lets the
  pooled-capabilities work ([pooled-capabilities.md](pooled-capabilities.md)) live in one place
  shared by every box.

## Phased plan (each step independently shippable)

1. ✅ **Done.** Defined `Resource` (`resource.rs`) + `ResourceError`; wired
   `Option<Arc<dyn Resource>>` through `Invocation`/`ExecParams` exactly like `read_hook`. Added
   the generic `__resource` injector + `js/resource.js` wrapper (gated to `Profile::Full`,
   op-limited). The box still works unchanged — the HTTP front passes `None`. `Invocation` is now
   `#[non_exhaustive]` with a builder so this and future field additions don't break consumers
   (resolves CONSUMER_NOTES item #2).
2. ✅ **Done.** Extracted a JS-free, public `DbBackend` (connect + `call`) from `db.rs` — the
   reusable dispatch core shared by the existing `__db` capability and a new in-process
   `Resource` adapter (`inproc.rs`, behind the `inproc` feature). `DbError::into_resource_error`
   maps faults across. `inject_db` now delegates to `DbBackend`, so the existing `db` path is
   structurally unchanged (same `connect`/`dispatch`/metrics). The adapter is built + unit-tested
   (payload unpacking, error mapping) but **not yet wired into `/execute`** — see step 3.
3. ✅ **Done for `db`** (the canonical, async template). `db.js` now calls
   `resource.call("db", …)`; the bespoke `__db` native function and `inject_db` are deleted
   (replaced by `db::inject_wrapper`, which evals only the wrapper). `db` dispatch + connection +
   breaker now live in the consumer's `InProcessResource` adapter (wired into `handler.rs`,
   connecting **lazily** on first use). **Metrics solution:** `DbBackend` owns its
   `Collector<DbMetric>`; the binary drains `adapter.db_metrics()` after the run and merges them
   into `meta.db_requests`, so the response is unchanged. **Verified end-to-end** against a live
   Postgres: query+params+types, transactions (one connection reused across begin/commit), SQL
   error → `capability/db/DB_QUERY` (`sqlstate` preserved), and connect failure → retryable
   `DB_CONNECTION`. Clippy is clean across the full capability cfg-matrix (`db` dropped `_throws`
   since it no longer builds the tag itself; `check_op_limit` regated to `http`+`_throws`).
   **Minor, documented semantic shifts:** (a) connect is now lazy (anchored at first db call, not
   inject time) — a script that never touches `db` never connects, and a connect failure surfaces
   as a thrown capability error rather than aborting before the handler; (b) the per-execution op
   budget for `db` is now the shared `__resource` counter (identical while `db` is the only
   migrated capability; becomes a shared pool as others migrate), and an exhausted budget reports
   `RESOURCE_OP_LIMIT` rather than `DB_OP_LIMIT`.

   ✅ **All driver-backed capabilities migrated** (same turn): `mongo`, `mail`, `redis`, `amq`,
   `auth` now route through `resource.call("<cap>", …)` too. Each got a JS-free backend
   (`MongoBackend`, `MailBackend`, `RedisBackend`, `AmqProducer`, `AuthBackend`) that owns its
   metrics + `into_resource_error`; their bespoke `__<cap>` native functions are deleted
   (replaced by `<cap>::inject_wrapper`); `InProcessResource` holds a lazy slot per capability
   and the binary merges each `meta.<cap>_requests`. `http` and `s3` stay in-engine (no driver /
   pure SigV4 signing). Consequence: the engine's `tokio_handle`, `db_breaker`, `map_redis_inject_error`,
   and `capability_inject` are gone; `_throws` (and the `capability_fault_json` tag machinery +
   `check_op_limit`) now ride **only** on `s3`. **Verified:** full clippy + every per-capability
   cfg combo + unit tests, plus **live** smokes for `db`, `redis`, and `mongo` (the
   `{collection,data}` envelope) against real backends — query/dispatch, metrics, and error
   classification all preserved. `mail`/`amq`/`auth` are structurally identical (dispatch
   unchanged) + unit-tested glue, not yet live-smoked (need SMTP/broker/IdP infra).
4. **Driver crates leave `runlet-core`.**
   - ✅ **4a — done.** Extracted two crates: **`fabric-wire`** (the shared leaf: the `Egress`
     trait + `EgressError`, the `ErrorOwner`/`Fault`/`DynamicFault` taxonomy + `__jsbox` wire
     envelope, the `CircuitBreaker`, and the metric `Collector`) and **`fabric-backends`** (the six
     `*Backend`s + their `*Config`/`*Metric`/`*Error`/`*Deps` + the in-process `BackendSet`, the
     renamed `InProcessEgress`). All vendor drivers (`tokio-postgres`/`mongodb`/`lettre`/`redis`/
     `amqprs`/`async-nats`) now live in `fabric-backends`; `runlet-core` links **none** even with
     `full` (proven via `cargo tree`) — the core feature matrix shrank to just the JS wrappers
     (`<cap>.rs`'s `inject_wrapper` + `js/*.js`) and the engine seam. `fabric-backends` depends on
     `fabric-wire` only — never on `runlet-core` (no QuickJS), so it is the shape `fabricd` will
     host. `runlet` wires an in-process `BackendSet` (provable no-op: clippy clean across the full
     cfg matrix, all tests green, behavior unchanged). The driver-backed `ExecMetrics` fields left
     the engine — the binary drains them straight from the `BackendSet`.
   - **4b — next.** Adapter impl #2: a local `fabricd` sidecar hosting `BackendSet` over UDS
     (length-prefixed JSON; the `__jsbox` error JSON *is* the wire error; metrics ride back in the
     response). Add a UDS-client `Egress` impl in `runlet` doing `block_on(timeout(deadline,
     roundtrip))` and swap `BackendSet` for it (with in-process fallback). localhost-QUIC (`quinn`)
     is the cross-node step (Project B).
5. **Trust-model flip.** `CapabilitySet` driver configs → logical resource allowlist; all
   creds move into `fabricd`. Remove operator-secret fields from the request surface. *(The
   in-box half already landed — the request carries `config.io` logical names, not creds; step 5
   is the `fabricd`-side move + removing the vestigial `handle`/`db_breaker` from `LogicHost::new`.)*

Steps 1–3 are the bulk of the value, touch only `runlet-core`, and require no distributed
systems. Steps 4–5 are the on-ramp to [network-fabric.md](network-fabric.md).
