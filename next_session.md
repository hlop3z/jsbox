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

- The egress port is **`Egress`** in Rust (`Egress`/`EgressError`/`InProcessEgress`,
  `crates/runlet-core/src/egress.rs`). The **script-facing** global + FFI + JS file + request
  field are **`io`** (`io.call(name, action, payload)`, `__io`, `js/io.js`, `config.io`). The
  sandbox makes "syscalls" to its host. (Was `resource`/`Resource`/`__resource` — fully renamed.)
- `Gate` (engine.rs) = a 2-variant `Off`/`On` enum used for the per-capability inject gates
  (avoids `clippy::struct_excessive_bools`).

## Branch / commits

On branch **`resource-egress`** (off `main`). Commits so far:
1. `7f26dc8` — Route driver-backed caps through the egress port (steps 1–3; was `resource`).
2. `d1361d6` — Flip to logical-resource egress + rename to Egress/io (this session).

## Decisions already made (don't relitigate)

- Fabric transport: hybrid — NATS JetStream for pub/sub+queues, custom `quinn` QUIC for
  RPC+streaming. For the **local** sidecar, start with **UDS** (zero new deps, sidesteps the
  quinn+aws-lc-rs landmine); QUIC is for the cross-node fabric (Project B).
- Scale tens–hundreds of nodes → skip SWIM/HyParView/Plumtree.
- Request shape = **capability-keyed map** (`config.io = {"db":["orders-db"]}`). Trust = **hard
  cut**: no creds in the request, ever; they live operator-side.

## Status: in-box trust-flip + rename DONE & verified (this session)

- **Request surface flipped:** `RequestConfig` dropped `db/mongo/mail/redis/amq/auth` configs;
  added `io: RequestIo` (capability-keyed `Vec<String>` of logical names). Kept `allowed_hosts`,
  `s3`, `sys`. (`crates/runlet/src/handler.rs`)
- **Operator resource table:** server `Config` gained `resources: HashMap<String, ResourceBinding>`
  (`crates/runlet/src/config.rs`). `ResourceBinding` is an **internally-tagged** enum
  (`#[serde(tag="kind")]`, variants boxed) → `{"kind":"db", <DbConfig fields>}`. Wired into
  `AppState.resources` in `main.rs`.
- **Resolution = trust boundary:** `handler::resolve_egress(table, &io)` maps the first named
  resource per kind → operator config; unknown name → `400 RESOURCE_NOT_FOUND`, wrong kind →
  `400 RESOURCE_KIND_MISMATCH`. `build_egress` now wires `InProcessEgress` from the resolved
  configs; the Tier-0 statement-timeout clamp runs on the resolved db config. **Interim limit:
  one binding per kind** (the JS wrapper still dispatches by kind via `io.call('db',…)`, so JS
  wrappers were NOT changed; multi-binding `db('orders-db')` is a future enhancement needing the
  parameterized wrapper).
- **Engine gates:** the six driver `Option<&*Config>` fields on `CapabilitySet`/`ExecParams`
  became `Gate` (engine never saw creds, only presence). `inject_apis` gates on `.is_on()`.
- **Verified:** `cargo clippy` full workspace clean; **per-cap cfg sweep clean** (`--no-default-
  features --features <db|mongo|mail|redis|amq|auth|http|s3|inproc>` + none); `cargo test` green
  (57 core + 12 runlet, incl. new `egress_resolution_tests` covering the ResourceBinding serde +
  resolve NOT_FOUND/KIND_MISMATCH). Breaker timing test still a known flake.
  - NOTE: the project gate is plain `cargo clippy` (NOT `--all-targets`). `--all-targets` flags
    pre-existing `unwrap_err()` in `inproc.rs` test code — that's committed style, out of scope.

## What's next (in order)

1. **Close the in-box step (Task #5):** the hard cut broke `test_simple.py` (it sends `config.db`
   + generates `.test-run/config.json`). Update the harness to emit an operator `resources` map
   and send `config.io`. Update `docs/` + `README.md` for the new surface + the io/Egress rename.
   Then **live-smoke** db end-to-end (operator config.json `resources` → request `config.io` →
   `db.query`) via Docker on the `jsbox_default` network. This is the only unproven runtime path
   (static + unit-tested, not yet end-to-end).
2. **Step 4a (Task #2) — extract crates:** `fabric-wire` (`EgressError`/`ErrorOwner`/`DynamicFault`
   + wire envelope) + `fabric-backends` (the six `*Backend`s + `BackendSet`/`InProcessEgress`);
   drivers leave `runlet-core`, shrinking its feature matrix. runlet still uses in-process
   `BackendSet` (provable no-op). Keep `inject_wrapper` + `js/*.js` in runlet-core. Re-sweep cfg.
3. **Step 4b (Task #3) — fabricd:** new sidecar bin hosting `BackendSet` behind UDS (length-prefixed
   JSON envelope; the existing `__jsbox` error JSON IS the wire error; metrics ride back in the
   response so `merge_egress_metrics` is unchanged). Add a UDS-client `Egress` impl in runlet
   doing `block_on(timeout(deadline, roundtrip))`; swap `InProcessEgress` for it (with in-process
   fallback). Live-smoke box→UDS→fabricd→Postgres.
4. **Step 5 finalize (Task #4):** remove the vestigial `#[expect(dead_code)]` `handle`/`db_breaker`
   from `LogicHost::new` + struct (coordinate the one-time signature break with external consumer
   `reactive-database-pg`; update `CONSUMER_NOTES.md`). Consider `#[non_exhaustive]` + builder on
   `CapabilitySet`.

## How to build/test/smoke (Windows host — native cargo CAN'T build; use Docker via PowerShell)

Git Bash mangles docker `-v`/`-w` paths → **run docker from the PowerShell tool**. `target/` is
host-mounted (incremental). clippy/rustfmt aren't preinstalled in alpine.

```
# full gate (clippy + test). The real gate is plain `cargo clippy` (NOT --all-targets).
docker run --rm -v "C:\Users\Toy\Documents\GitHub\jsbox:/work" -w /work rust:1.92-alpine sh -c "rustup component add clippy >/dev/null 2>&1; cargo clippy --quiet 2>&1 | tail -n 60; cargo test --quiet 2>&1 | tail -n 12"
```
For the per-cap cfg sweep, write a small `sweep.sh` into the repo (mounted at /work) and
`sh sweep.sh` it — **inline multi-line scripts get mangled** through PowerShell→docker→sh.
Loop `for f in NONE db mongo mail redis amq auth http s3 inproc` calling
`cargo clippy -p runlet-core --no-default-features [--features $f]`.

Live-smoke pattern: start backend on the `jsbox_default` docker network (no host port map),
run a sh script in `rust:1.92-alpine` on the same network that `cargo build -p runlet`, writes a
`config.json` with a `resources` map + `debug:true`, runs the binary from /tmp (bind
127.0.0.1:3000), polls `/health`, and curls `/execute` with `config.io` pointing at a named
resource.

## Gotchas learned

- The egress port name maps differently by layer: Rust `Egress`, script/FFI/field `io`. Keep
  them straight (trait `Egress` registers FFI `__io`, exposes global `io`).
- `Gate` import in `host.rs` must be referenced as `engine::Gate` (path), NOT a `use` import —
  otherwise driver-less cfg builds (`NONE`/`http`/`s3`) fail with unused-import.
- `ResourceBinding` is internally-tagged + boxed variants (avoids `clippy::large_enum_variant`);
  works because no `*Config` uses `deny_unknown_fields`.
- The cfg matrix is the #1 risk — always sweep per-cap combos after touching capability wiring.
- `#[expect(...)]` is fragile against the cfg matrix when the lint only fires in some combos
  (e.g. `struct_excessive_bools` depends on how many bool fields survive cfg) — prefer a
  structural fix (the `Gate` enum) over a cfg-conditional `#[expect]`.
- Don't reformat pre-existing committed code; only `cargo fmt` your own edits.
