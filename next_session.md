# Next session — handoff

> Working note for resuming the **Resource egress / Network Fabric** work after a context clear.
> Authoritative design: `docs/design/resource-egress.md` (Project A) and
> `docs/design/network-fabric.md` (Project B). Memory: `resource-egress-fabric-direction`.
>
> **Standing instruction:** when context reaches ~100k tokens, refresh/rewrite this file so the
> session can be cleared and resumed from here.

## The goal (one line)

Pull all vendor drivers out of the `runlet-core` sandbox behind a single `Resource` egress port,
so a sidecar (`fabricd`, eventually a global QUIC/NATS fabric) holds the drivers. The box keeps
only pure compute (`$`/Decimal/determinism), `http` (SSRF-guarded), and `s3` (SigV4 signing).

## Decisions already made (don't relitigate)

- **Fabric transport:** hybrid — NATS JetStream for pub/sub+queues, custom `quinn` QUIC for
  RPC+streaming. Don't reimplement NATS.
- **Scale:** tens–hundreds of nodes → **skip** SWIM/HyParView/Plumtree for now.
- **Underlay:** NetBird bootstraps, then nodes self-gossip and run independent of its controller
  (caveat: hard-NAT new peers still need a relay when the controller is down).
- **First focus:** box decoupling before any fabric.

## Status: Project A steps 1–3 DONE and verified

- **Step 1 (done):** `Resource` trait + `ResourceError` (`crates/runlet-core/src/resource.rs`),
  wired through `Invocation`/`ExecParams` like `read_hook`. Generic `__resource(name,action,payload)`
  injector + `js/resource.js` (gated `Profile::Full`, op-limited). `Invocation` is now
  `#[non_exhaustive]` with a builder (`inline`/`registered` + `.caps()/.resource()/…`).
- **Step 2 (done):** extracted JS-free `DbBackend` (connect + `call` + owns metrics) from `db.rs`;
  `InProcessResource` adapter (`crates/runlet-core/src/inproc.rs`, `inproc` feature).
- **Step 3 (done — ALL driver-backed caps):** `db`, `mongo`, `mail`, `redis`, `amq`, `auth` all
  route through `resource.call(...)`. Each has a JS-free backend that owns its metrics +
  `into_resource_error`; bespoke `__<cap>` native fns deleted → replaced by
  `<cap>::inject_wrapper` (evals the JS wrapper only). `InProcessResource` holds a lazy slot per
  cap; `runlet/src/handler.rs::build_egress` wires them per-request and `merge_egress_metrics`
  folds each `meta.<cap>_requests` back into the outcome.
  - Backends: `DbBackend`, `MongoBackend`, `MailBackend`, `RedisBackend`, `AmqProducer` (NOT
    `AmqBackend` — that name is the rabbitmq/nats enum), `AuthBackend`.
  - `http` + `s3` stay in-engine (native `__http`/`__s3`, engine collectors).
  - `_throws` (+ `capability_fault_json`, `CapabilityFault`, `check_op_limit`) now ride **only**
    on `s3`. `check_op_limit` gate = `any(feature="http", feature="_throws")`. `mail`'s
    `max_sends` sub-cap uses `sandbox::op_count` (gated `#[cfg(feature="mail")]`).
  - Engine no longer holds `tokio_handle`/`db_breaker`; `map_redis_inject_error` and
    `EngineError::capability_inject` are gone. `LogicHost`'s `handle`/`db_breaker` fields are
    vestigial (`#[expect(dead_code)]`) — kept on `LogicHost::new` for API stability (external
    consumer reactive-database-pg builds `default-features=false`; don't break the signature).

## Verification done this work

- `cargo clippy` full workspace: **clean**. Every per-capability cfg combo
  (`--no-default-features --features <cap>` for db/mongo/mail/redis/amq/auth/http/s3 + none):
  **clean**. (The cfg matrix is the main risk — always sweep it.)
- `cargo test`: 57 pass. NOTE: `breaker::tests::half_open_after_cooldown` is a **flaky timing
  test** (passes on rerun; unrelated — `breaker.rs` untouched).
- `rustfmt`: clean.
- **Live smokes** (Docker, real backends): `db` (query/params/tx/SQL-error/connect-error),
  `redis` (set/incr/get + connect-error), `mongo` (insert/find via `{collection,data}` envelope).
  All metrics + error classification preserved. `mail`/`amq`/`auth` NOT live-smoked (need
  SMTP/RabbitMQ-NATS/Keycloak-ZITADEL); structurally identical + unit-tested glue.

## How to build/test/smoke (Windows host — native cargo CAN'T build; use Docker)

Run from **PowerShell** (Git Bash mangles `-v` paths). `target/` is host-mounted so builds are
incremental. clippy isn't preinstalled in alpine → `rustup component add clippy`.

```
# clippy + test (the real gate; cargo build alone does NOT run the lints)
docker run --rm -v "C:\Users\Toy\Documents\GitHub\jsbox:/work" -w /work rust:1.92-alpine sh -c "apk add --no-cache musl-dev >/dev/null 2>&1 && rustup component add clippy >/dev/null 2>&1 && cargo clippy --quiet 2>&1 | tail -n 60 && cargo test --quiet 2>&1 | tail -n 12"
# a single cfg combo
docker run --rm -v "C:\Users\Toy\Documents\GitHub\jsbox:/work" -w /work rust:1.92-alpine sh -c "rustup component add clippy >/dev/null 2>&1; cargo clippy -p runlet-core --no-default-features --features mongo --quiet 2>&1 | tail -n 8"
```

Live smoke pattern: start backend container on the `jsbox_default` docker network (no host port
map to avoid 5432 conflict), then run a sh script (in `…/scratchpad/`) inside a `rust:1.92-alpine`
container on the same network that `cargo build -p runlet`, runs the binary from `/tmp` (default
bind 127.0.0.1:3000), polls `/health`, and curls `/execute` with `config.<cap>` pointing at the
backend container name. See `smoke_db.sh` / `smoke_more.sh` in the session scratchpad for templates.

## What's next (not started)

- **Optional:** live-smoke `mail`/`amq`/`auth` (bring up `docker compose up -d` mailpit?/rabbitmq/
  nats/keycloak; configs are env-overridable — see CLAUDE.md auth test notes).
- **Step 4 — `fabricd`:** a sidecar that hosts the same `*Backend`s behind UDS/localhost-QUIC
  (`quinn`); driver crates move out of `runlet-core` (feature matrix shrinks). The box's
  `Resource` impl becomes a QUIC client instead of `InProcessResource`. `DbBackend` etc. are the
  exact shapes to host.
- **Step 5 — trust-model flip:** `CapabilitySet` driver configs (host/creds) → a logical resource
  **allowlist**; creds move entirely into `fabricd`; remove operator-secret fields from the
  `/execute` request surface. Then `LogicHost::new`'s vestigial `handle`/`db_breaker` can be
  removed (coordinate the one-time break with reactive-database-pg).

## Gotchas learned

- Adding an `Invocation` field is a silent break for external consumers → that's why it's
  `#[non_exhaustive]` + builder now (CONSUMER_NOTES item #2, resolved).
- The cfg matrix bites: dropping `_throws` from a cap can leave `capability_fault_json`/
  `check_op_limit`/`CapabilityFault` dead in *single-feature* builds. Always run the per-cap combos.
- `missing_debug_implementations` is denied → every pub backend needs `Debug` (manual
  `finish_non_exhaustive` for ones holding non-Debug driver handles).
- `too_long_first_doc_paragraph` (nursery) → put a blank `///` line after the first sentence.
- Don't reformat the user's pre-existing uncommitted drift; only `rustfmt` the files you edit.
