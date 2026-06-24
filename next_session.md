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

## Crate layout (AS OF Step 5 — the current shape)

Six workspace members (`Cargo.toml`): `fabric-wire`, `fabric-backends`, `fabricd`, `runlet-core`,
`runlet`, `runlet-bench`.
- **`fabric-wire`** — shared leaf, no drivers/QuickJS: `Egress`+`EgressError`, the taxonomy
  (`ErrorOwner`/`Fault`/`DynamicFault`), `CircuitBreaker`, the metric `Collector`, **and** the
  box↔fabricd wire protocol (`wire.rs`: `WireInit`(names)/`WireCall`/`WireRequest`/`WireResponse` +
  framing, the `*Metric` types, `BackendMetrics`, `MeteredEgress`). Needs tokio `io-util` (framing).
- **`fabric-backends`** — the driver bag (featureless, all 6): `*Backend`s + `*Config` +
  `BackendSet`/`AsyncDeps` + the operator `ResourceBinding` table & `resolve` (`resources.rs`).
  Depends on `fabric-wire` ONLY. **Only `fabricd` links it.**
- **`runlet-core`** — the sandbox. Links NO driver even with `full`. Slim JS-wrapper modules
  (`db.rs`/… private `mod`s) + `js/*.js`; `egress`/`breaker`/`errors`/`sandbox` re-export from
  `fabric-wire`. `http`/`s3` in-engine. `LogicHost::new(pool, registry, settings)` — handle/breaker
  gone.
- **`fabricd`** — the egress sidecar bin. Loads its own `resources` cred table (`FABRICD_CONFIG`,
  default `fabricd.json`); per connection reads `Init`(names) → `resolve` → `BackendSet::from_configs`
  → `Ack`/`InitError`, then `Call`*/`Drain`. Dispatches each call on `spawn_blocking`.
- **`runlet`** — the box binary. **No drivers, no creds, no `fabric-backends` dep.** `handler.rs`:
  if `config.io.any()`, `uds::connect_session` opens a fabricd session (async, validates `Init`:
  `InitError`→400, unreachable→503), then the `spawn_blocking` task wraps it as `UdsEgress`
  (`from_stream`), runs, and drains. No in-process path.
- **`runlet-bench`** — unchanged.

## Branch / commits

On branch **`resource-egress`** (off `main`). `88a1eff` Step 4a, `87dfba6` Step 4b. **Step 5 is in
the working tree** — commit it (suggested: "Step 5: trust flip — box driver-free, creds only in
fabricd; drop LogicHost handle/db_breaker").

## Status: Steps 4a + 4b + 5 DONE & verified (this session)

- **Step 5 (full trust flip):** creds live ONLY in `fabricd`. The `ResourceBinding` table +
  name→config resolution moved box→`fabricd` (`fabric_backends::resources`); the box sends logical
  names in `WireInit`. The box **dropped `fabric-backends`** → links no driver (proven via
  `cargo tree -p runlet`). The wire protocol + `*Metric` types + `BackendMetrics` + `MeteredEgress`
  moved `fabric-backends`→`fabric-wire`. **No in-process fallback:** driver request + no/unreachable
  fabricd → `503 EGRESS_UNAVAILABLE`; unknown name → `400 RESOURCE_NOT_FOUND` (resolved daemon-side).
  `LogicHost::new` lost `handle`/`db_breaker` (breaker → daemon; `/metrics` reports 0 trips).
- **Verified (Docker `rust:1.92-alpine`):** full `cargo clippy` clean; per-cap cfg sweep clean
  (`sweep.sh`; needed `tokio` `io-util` on `fabric-wire` so the deterministic-core build has the
  framing traits); `cargo test` green (fabric-wire 8, fabric-backends 17, runlet-core 40, runlet 9);
  **live-smoke `smoke_5.sh`** box→UDS→fabricd→Postgres PASSED — query+metrics (box holds no creds),
  the `503` no-fallback path, and the `400` unknown-name path. (Throwaway `postgres:17-alpine` on a
  dedicated docker net aliased `postgres`; host 5432 is taken by another project.)

## What's next (in order)

1. **Commit Step 5** (see above) if not already done. **Project A (resource egress) is complete.**
2. **DEFERRED — rewrite `test_simple.py`:** the request surface is now `config.io` names + a
   `fabricd` sidecar. The harness must (a) start `fabricd` with a `resources` config covering the
   named variants (pg, pg-maxrows5, pg-badhost, pg-fast, pgbouncer, redis, mongo, nats, nats-fast,
   mail), (b) start `runlet` with `fabricd_socket` set (no `resources` on the box), (c) rewrite
   `config={"db":creds}` → `config={"io":{"db":["name"]}}`. `smoke_5.sh` is the working reference for
   the two-process setup. Needs all backends up.
3. **Project B — network fabric** (`docs/design/network-fabric.md`): grow `fabricd` from a local UDS
   sidecar into a cross-node QUIC/NATS fabric. Parked until needed.
4. **`smoke_5.sh`** / **`smoke_4b.sh`** in repo root are the reusable live-smokes (run a
   `postgres:17-alpine` on a docker network aliased `postgres`, then `docker run --network <net> …
   sh smoke_5.sh`). `smoke_4b.sh` predates the trust flip (puts creds on the box) — `smoke_5.sh` is
   the current one.

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
