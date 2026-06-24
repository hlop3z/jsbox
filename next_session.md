# Next session — handoff

> Working note for resuming the **Resource egress / Network Fabric** work after a context clear.
> Authoritative design: `docs/design/resource-egress.md` (Project A) and
> `docs/design/network-fabric.md` (Project B). Memory: `resource-egress-fabric-direction`.
>
> **Standing instruction:** when context reaches ~100k tokens, refresh/rewrite this file so the
> session can be cleared and resumed from here.

## The goal (one line)

Pull all vendor drivers out of the `runlet-core` sandbox behind a single egress port, so a
sidecar (`fabricd`, eventually a global QUIC/NATS fabric) holds the drivers. The box keeps only
pure compute (`$`/Decimal/determinism), `http` (SSRF-guarded), and `s3` (SigV4 signing).

## Naming (DECIDED — do not relitigate)

- Egress port: Rust **`Egress`** (`fabric_wire::Egress`/`EgressError`); script-facing global +
  FFI + JS file + request field are **`io`** (`io.call(name, action, payload)`, `__io`, `js/io.js`,
  `config.io`). The in-process adapter is **`BackendSet`** (was `InProcessEgress`).
- `Gate` (engine.rs) = a 2-variant `Off`/`On` enum for the per-capability inject gates.

## Crate layout (AS OF Step 4b — the current shape)

Six workspace members (`Cargo.toml`): `fabric-wire`, `fabric-backends`, `fabricd`, `runlet-core`,
`runlet`, `runlet-bench`.
- **`fabric-wire`** — shared leaf, no drivers/QuickJS: `Egress` trait + `EgressError`, the error
  taxonomy (`ErrorOwner`/`Fault`/`DynamicFault` + `dynamic_fault_json`), `CircuitBreaker`/
  `BreakerConfig`, and the metric `Collector`. Depended on by both core and backends.
- **`fabric-backends`** — the driver bag (featureless, always all 6): `db`/`mongo`/`mail`/`kv`/
  `amq`/`auth` `*Backend`s + `*Config`/`*Metric`/`*Error`/`*Deps`, and `BackendSet`/`AsyncDeps`
  (`backendset.rs`). Depends on `fabric-wire` ONLY (never `runlet-core`). This is the shape
  `fabricd` will host.
- **`runlet-core`** — the sandbox. Links NO driver even with `full` (proven via `cargo tree`).
  Keeps the JS wrappers: `db.rs`/`mongo.rs`/`mail.rs`/`kv.rs`/`amq.rs`/`auth.rs` are now private
  `mod`s with just `inject_wrapper` (+ `js/*.js`). `egress.rs`/`breaker.rs` re-export from
  `fabric-wire`; `errors.rs` re-exports `ErrorOwner`/`Fault`/`DynamicFault`/`dynamic_fault_json`;
  `sandbox.rs` re-exports the `Collector` apparatus (gated `any(http, s3)`). `http`/`s3` stay
  in-engine.
- **`fabricd`** — NEW bin (Step 4b): the egress sidecar. Hosts `BackendSet` behind a UDS wire
  protocol (`fabric_backends::wire`); one connection = one box-request session
  (`Init`→`Call`*→`Drain`). Dispatches each call on `spawn_blocking` (the backends `block_on`).
- **`runlet`** — the binary. `build_adapter` (in `handler.rs`) wires either an in-process
  `BackendSet` or a `uds::UdsEgress` client (when `config.fabricd_socket` is set) with in-process
  fallback — both as `Arc<dyn MeteredEgress>`, built + drained INSIDE the `spawn_blocking` task
  (UDS connect/calls/drain all `block_on`). Imports `*Config`/`*Metric`/`BackendMetrics`/`WireInit`
  from `fabric_backends`, `Egress`/`EgressError` from `fabric_wire`.
- **`runlet-bench`** — unchanged (no runlet-core dep).

## Branch / commits

On branch **`resource-egress`** (off `main`). Prior commits did steps 1–3 + the in-box trust-flip
+ Egress/io rename + docs/live-smoke. **Step 4a committed** as `88a1eff` (extract fabric-wire +
fabric-backends). **Step 4b is implemented in the working tree** (fabricd + UdsEgress) — commit it
(suggested: "Step 4b: fabricd UDS egress sidecar + UdsEgress client with in-process fallback").

## Status: Steps 4a + 4b DONE & verified (this session)

- **4a:** Drivers fully gone from `runlet-core` (`cargo tree -p runlet-core` with `full` shows none;
  `fabric-backends` carries them all). The driver-backed `ExecMetrics`/`ExecResult` fields left the
  engine (`host::ExecMetrics` = `http`/`s3` only); the binary drains driver metrics from the
  adapter. `inject_apis` reworked (cfg-params for `http`/`s3` collectors; the `Collectors` struct is
  gone).
- **4b:** `fabricd` sidecar hosts `BackendSet` over UDS; `runlet::uds::UdsEgress` is the client with
  in-process fallback; wire protocol in `fabric_backends::wire`. `*Config` gained `Serialize`,
  `*Metric` + `EgressError` gained `Deserialize` so they round-trip. The adapter is built + drained
  INSIDE the `spawn_blocking` task (UDS connect/call/drain all `block_on`).
- **Verified (Docker `rust:1.92-alpine`, the only way to build on this Windows host):**
  - `cargo clippy` whole workspace — clean.
  - Per-cap cfg sweep `runlet-core --no-default-features [--features …]` + NONE — clean
    (`sweep.sh`; `fabric-backends`/`fabricd` are featureless, not swept). Gate is plain `cargo
    clippy`, NOT `--all-targets`.
  - `cargo test` — green (fabric-wire 5, fabric-backends 16, runlet-core 40, runlet 12).
  - **Live-smoke (`smoke_4b.sh`)** box→UDS→`fabricd`→Postgres PASSED: `db.query` returned `n=41`
    through the daemon with `db_requests` metrics; after killing `fabricd` the box fell back to
    in-process and still returned `n=41`. (Ran a throwaway `postgres:17-alpine` on a dedicated
    docker network aliased `postgres` — host 5432 is taken by another project.)

## What's next (in order)

1. **Commit Step 4b** (see above) if not already done.
2. **DEFERRED — rewrite `test_simple.py`:** the in-box hard cut (steps 1–3) broke it (it still
   sends `config.db`). Needs a generated server `config.json` `resources` map for the named
   variants (pg, pg-maxrows5, pg-badhost, pg-fast, pgbouncer, redis, mongo, nats, nats-fast, mail)
   and ~40 `config={"db":creds}` → `config={"io":{"db":["name"]}}` rewrites, plus reordering
   `main()` so auth/zitadel discovery runs BEFORE `_start_server`. Needs all backends up.
3. **Step 5 (Task #4) — trust-flip finalize:** the box still resolves creds and ships them to
   `fabricd` in `WireInit` (local UDS, fine for 4b). The full flip: `fabricd` holds the operator
   `resources` table and resolves creds itself; the box sends only logical names — then `runlet`
   can drop the `fabric-backends` dep entirely (no drivers in the box binary). Also remove the
   vestigial `#[expect(dead_code)]` `handle`/`db_breaker` from `LogicHost::new` + struct (coordinate
   the one-time signature break with external consumer `reactive-database-pg`; update
   `CONSUMER_NOTES.md` — see item #7 for the 4a API moves).
4. **`smoke_4b.sh`** in repo root is the reusable live-smoke (run a `postgres:17-alpine` on a docker
   network aliased `postgres`, then `docker run --network <net> … sh smoke_4b.sh`).

## How to build/test/smoke (Windows host — native cargo CAN'T build; use Docker via PowerShell)

Git Bash mangles docker `-v`/`-w` paths → **run docker from the PowerShell tool**. `target/` is
host-mounted (incremental). clippy/rustfmt aren't preinstalled in alpine; `musl-dev` is needed for
`aws-lc-sys`.

```
# full gate (clippy + test). The real gate is plain `cargo clippy` (NOT --all-targets).
docker run --rm -v "C:\Users\Toy\Documents\GitHub\jsbox:/work" -w /work rust:1.92-alpine sh -c "apk add --no-cache musl-dev >/dev/null 2>&1; rustup component add clippy >/dev/null 2>&1; cargo clippy --quiet 2>&1 | tail -n 80; cargo test --quiet 2>&1 | tail -n 20"
# per-cap cfg sweep (runlet-core only):
docker run --rm -v "C:\Users\Toy\Documents\GitHub\jsbox:/work" -w /work rust:1.92-alpine sh -c "apk add --no-cache musl-dev >/dev/null 2>&1; rustup component add clippy >/dev/null 2>&1; sh sweep.sh"
```

## Gotchas learned (4a)

- The egress port name maps differently by layer: Rust `Egress`, script/FFI/field `io`.
- `fabric-backends` must NOT depend on `runlet-core` (would pull QuickJS into `fabricd`). Anything
  the backends share with the sandbox (wire types, breaker, collector) lives in `fabric-wire`.
- The driver `*Metric` fields had to leave the engine: keeping them would force `runlet-core` to
  reference `fabric-backends` types, re-pulling the drivers. So the engine carries only `http`/`s3`
  metrics; the binary drains the rest from the `BackendSet`.
- `unused_crate_dependencies` is denied → the binary reaches wire types via `runlet_core`
  re-exports (no direct `fabric-wire` dep); `fabric-backends` gates every driver dep is moot since
  it's featureless (all used).
- nursery `too_long_first_doc_paragraph` bites new module/item docs — keep the first sentence short.
- `doc_markdown` wants backticks on `MongoDB`/`PostgreSQL`/`CockroachDB`/`RabbitMQ`/`Redis` (NATS/
  SMTP/OIDC all-caps are fine).
- Don't reformat pre-existing committed code; only `cargo fmt` your own edits.
