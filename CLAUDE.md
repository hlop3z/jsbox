# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A sandboxed JavaScript execution service in Rust. Clients `POST /execute` a JS
`handler(ctx)` function plus a JSON context; the server runs it in an isolated QuickJS
context and returns `{data, error, meta}`. The single endpoint is the whole product.

**Cargo workspace (five crates + a bench crate):**

- **`fabric-wire`** (`crates/fabric-wire/`) — the shared egress-port contract: the `Egress`
  trait + `EgressError`, the error taxonomy (`ErrorOwner`/`Fault`/`DynamicFault` + the `__jsbox`
  wire envelope), the per-target `CircuitBreaker`, and the metric `Collector`. A driver-free,
  QuickJS-free leaf depended on by **both** `runlet-core` and `fabric-backends`.
- **`fabric-backends`** (`crates/fabric-backends/`) — the driver-backed egress backends
  (`db`/`mongo`/`mail`/`redis`/`amq`/`auth`), each a JS-free `*Backend` (string-in/string-out
  dispatch + metrics + `into_resource_error`), plus the in-process `BackendSet` that wires them
  behind the `fabric_wire::Egress` port. Holds **all** the vendor drivers. Depends on
  `fabric-wire` only (never `runlet-core` — no QuickJS), so it is the shape a sidecar (`fabricd`)
  will host. Featureless (the driver bag always carries every backend). See
  `docs/design/resource-egress.md`.
- **`runlet-core`** (`crates/runlet-core/`) — the reusable logic host: the QuickJS engine,
  runtime pool, sandbox, resilience, error taxonomy, capabilities, and the callable
  [`LogicHost`] port (`Invocation` → `Outcome`). Knows nothing about HTTP. The public entry
  is `runlet_core::host::LogicHost`; each capability is a cargo **feature** (`db`, `http`,
  `mongo`, `mail`, `s3`, `redis`, `amq`, `auth`), so a deterministic-only consumer builds with
  `default-features = false` and links nothing. **Links no network driver even with `full`** —
  the driver-backed capabilities now keep only their JS wrapper (`<cap>.rs`'s `inject_wrapper` +
  `js/*.js`) here and route through the egress port to `fabric-backends`; only `http` (SSRF-guarded)
  and `s3` (pure SigV4 signing) stay in-engine. See `docs/design/` for the design.
- **`runlet`** (`crates/runlet/`) — the binary: the axum HTTP `/execute` front + server config,
  a thin adapter over `LogicHost::run`. Enables `runlet-core`'s `full` and wires the egress port —
  either an in-process `fabric_backends::BackendSet`, or (when `config.fabricd_socket` is set) a
  `uds::UdsEgress` client to `fabricd` with automatic in-process fallback. Behavior is unchanged
  from the pre-workspace `jsbox` (the binary/image are renamed to `runlet`; the `jsbox_*`
  Prometheus metric names and internal `__jsbox` error tag are kept for compatibility).
- **`fabricd`** (`crates/fabricd/`) — the optional local egress **sidecar** (bin): hosts
  `fabric_backends::BackendSet` behind a Unix-domain-socket wire protocol
  (`fabric_backends::wire`), so the box can reach `db`/`mongo`/`mail`/`redis`/`amq`/`auth` without
  linking the drivers. One client connection = one box-request session (`Init`→`Call`\*→`Drain`).
  The on-ramp to the network fabric (`docs/design/network-fabric.md`).

## Commands

The project uses [Task](https://taskfile.dev) (`Taskfile.yml`). Raw `cargo` equivalents in parens.

- **Build:** `task build` (`cargo build`, whole workspace) · release: `task build-release` (`cargo build --release`)
- **Run:** `task run` (`cargo run -p runlet`) — serves on `http://127.0.0.1:3000`
- **Deterministic-only core build (no network drivers):** `cargo build -p runlet-core --no-default-features` (optionally `--features db,http,…` to opt specific capabilities back in)
- **Format:** `task fmt` / `task fmt-check`
- **Lint:** `task clippy` (`cargo clippy`) — see the lint warning below
- **Unit tests:** `cargo test`
- **Integration tests:** `task test-db-up` (starts Postgres + PgBouncer + CockroachDB + a local httpbin via `docker compose`), then `python test_simple.py`. The db section also runs through PgBouncer (transaction pooling, host `:6432`) to keep the external-pooler path covered (`docs/design/pooled-capabilities.md`). The HTTP `api` tests hit the local `httpbin` service (host `:8095`, env-overridable `HTTPBIN_URL`) — hermetic, no httpbin.org dependency; note go-httpbin echoes headers as **arrays**, and reaching it requires `debug: true` (SSRF private-IP relax). The script-registry section needs the server started by the harness itself (it generates `.test-run/config.json` with `debug: true` + `scripts_dir=tests/scripts`) and self-skips otherwise. The Python harness **starts its own server** with `cargo run`, so don't run one separately. It is a custom runner (not pytest) — there is no per-test name filter; edit `main()` in `test_simple.py` to narrow what runs. Each capability section **self-skips** if its backend isn't reachable (it live-probes first), so a partial `docker compose up` only runs what's available.
  - **Auth (`auth`) tests** need an identity provider. `docker compose up -d keycloak zitadel` brings up both. Keycloak (host `:8081`) is fully automatic — the harness mints a token via the `admin-cli` password grant and creates a confidential client for introspection. ZITADEL (host `:8082`) needs its bootstrap service-account PAT: `docker compose exec`-free, it's written to `./.zitadel/zitadel-admin-sa.pat` (a gitignored bind mount) on first start, so run the suite with `ZITADEL_PAT_FILE=./.zitadel/zitadel-admin-sa.pat` (or `ZITADEL_PAT=<token>`). Provider URLs/creds are env-overridable (`KEYCLOAK_ISSUER`, `ZITADEL_ISSUER`, …) for in-network/CI runs. ZITADEL introspection needs an API app, so introspection-with-creds is exercised on Keycloak; ZITADEL covers discovery + userinfo + the throw path.
- **Everything:** `task` (fmt-check + clippy + tests + supply-chain) · `task check` (no supply-chain)
- **Supply chain:** `task supply-chain` (cargo-audit + cargo-deny + cargo-vet; install via `task setup`). cargo-vet is initialized (`supply-chain/`): the dep tree is covered by imported third-party audit sets (Mozilla/Google/Bytecode Alliance/Embark/Zcash/ISRG) with the remainder as `exemptions` — a new/bumped dep that isn't audited or exempted fails `cargo vet`, so re-run it after dependency changes and `cargo vet prune` / add an exemption as needed.
- **Docker:** `task docker-build` / `task docker-run`

### CRITICAL: `cargo build` does not run the clippy lints

The strict lint contract lives in `[lints.clippy]` in `Cargo.toml`, and **`cargo build` / `cargo test` do NOT enforce it** — only `cargo clippy` does. Always run `task clippy` before considering a change done; a clean `cargo build` is not enough. A hard clippy error can also short-circuit later lint passes, so fixing one error often surfaces more on the next run — re-run until truly clean.

## Build environment gotchas

- **`aws-lc-sys` (the rustls crypto backend) needs a C toolchain.** Plain Windows hosts without MSVC build tools + NASM cannot build this project natively. **Build and test via Docker** (the `Dockerfile` cross-compiles to musl/Alpine, which handles `aws-lc-sys` with just `musl-dev`). The release `Dockerfile` is multi-stage (cargo-chef + fat-LTO + `strip` → distroless/static, ~18 MB). For a fast functional test, a debug `cargo build` on `rust:1.92-alpine` is much quicker than the release LTO build and still enforces the rustc lints.
- **rustls provider is `aws-lc-rs`, not `ring`.** When adding a TLS-using dependency, configure it with `rustls-no-provider` + the dep's `aws-lc-rs` feature so it reuses the existing provider. Pulling `ring` (or default `native-tls`/OpenSSL) links a second crypto stack and bloats the binary.

## Architecture (request lifecycle)

Module paths below are under `crates/runlet-core/src/` unless prefixed with `runlet/`.

`runlet/src/main.rs` wires an axum router (`/execute`, `/health`, `/metrics`) with an
`AppState` holding a `LogicHost` (+ the bulkhead/partition/breaker/metrics it doesn't own).

1. **`runlet/src/handler.rs`** — `execute()` resolves the script source — exactly one of inline `script` or registered `key` (looked up in `registry.rs`, a startup-loaded map of `scripts_dir/**/*.js`; see `docs/design/script-registry.md`) — validates input sizes, admits via the bulkhead, then `tokio::task::spawn_blocking` (QuickJS is synchronous/single-threaded and must run off the async runtime) builds an `Invocation` and calls `host.run`.
2. **`host.rs`** — `LogicHost::run(Invocation) -> Outcome` is the callable port (no HTTP assumption): resolves the `CodeRef`, acquires a pooled runtime, builds `ExecParams`, calls `engine::run`, releases the runtime, and maps `ExecResult` → `Outcome { result, effects, metrics }`. The HTTP front and any non-HTTP scheduler both go through this.
3. **`pool.rs`** — `JsPool` is a fixed pool of pre-warmed `Runtime`s (sized to CPU cores). `acquire()`/`release()`; a fresh `Context` is created per request (cheap) so global scope never leaks between requests. `release()` runs GC.
4. **`engine.rs`** — `run()` is the orchestrator: sets a timeout interrupt handler, makes a `Context`, injects the `json()` bridge → `$`/Decimal → `$sys` → `emit`/`read` → capabilities (`inject_apis`, gated by `Profile`) → evals the user script → removes `eval`/`Proxy` (+ determinism sanitizer under `Profile::Deterministic`) → calls `handler(ctx)` → extracts the JSON result. Returns `ExecResult { outcome, effects, *_metrics }`.
5. **`runlet/src/handler.rs`** — parses the JS `{data, error}` envelope (zero-copy via `RawValue`), attaches the Rust-computed `meta`, returns `{data, error, meta}` (ignores `Outcome.effects`).

**`Profile` + `emit` (engine-disposes model):** `Profile::Full` is the jsbox capability set + `emit`; `Profile::Deterministic` injects **no** I/O capabilities and neutralizes nondeterminism (`Math.random`/`Date`/`$sys` time+random — see `js/determinism.js`). `emit(value)` appends opaque JSON to a per-invocation buffer surfaced as `Outcome.effects` (logic proposes, the engine disposes). `Invocation.read_hook` is the consumer-wired read-of-declared-dependencies seam (exposed as the `read()` global). The HTTP front always uses `Profile::Full` with no read-hook.

`config.rs` (core) owns `EngineConfig` (engine sandbox limits, human-readable byte sizes like `"32mb"`: memory, stack, wall-clock timeout, max script/context bytes, `max_ops`). The server `Config` (bind address, `/execute` auth token, `scripts_dir`/`modules_dir`) lives in `runlet/src/config.rs` and embeds `EngineConfig`.

### The capability pattern (how `api`, `db`, `mail` work)

Each capability is a near-identical module. To add or modify one, follow the existing shape:

- A native function registered with `Function::new` named `__<cap>`, with a **string-in / string-out JSON FFI contract** (no rich types cross the QuickJS boundary). The matching JS wrapper lives in `crates/runlet-core/src/js/<cap>.js`, embedded via `include_str!` and `eval`'d after registration to expose a clean global (`api`, `db`, `mail`, `$`).
- **Per-request, opt-in:** the capability is injected only if its config block is present in the request (`engine.rs::inject_apis`) **and** the `Profile` allows I/O. No config → the global simply doesn't exist (`typeof mail === "undefined"`). `$`/Decimal/`$sys` are the exception — pure (no I/O), **always injected**, no config.
- **Metered:** each op goes through `sandbox.rs` (`check_op_limit`, `record`); metrics collect into a `Collector<T>` and drain into the response `meta.<cap>_requests`.
- **Cargo-feature gated:** each I/O capability is a feature on `runlet-core`. A new capability adds a `[features]` entry (with `"_io"` and any `dep:` driver crates) and `#[cfg(feature = "<cap>")]` on its module, its `ExecParams`/`ExecResult`/`Collectors`/`CapabilitySet`/`ExecMetrics` fields, and its `inject_apis` block.
- Files touched when adding a capability: new `crates/runlet-core/src/<cap>.rs` + `src/js/<cap>.js`, a `#[cfg]`'d `pub mod` in `lib.rs`, the feature in `crates/runlet-core/Cargo.toml`, a `#[cfg]`'d branch in `engine.rs::inject_apis` + cfg'd fields in `ExecParams`/`ExecResult`/`Collectors`, cfg'd fields in `host.rs` (`CapabilitySet`/`ExecMetrics` + the `run` wiring), and `RequestConfig` + `Meta` in `runlet/src/handler.rs`.

### Two trust models — pick the right one

- **`http` (`http.rs`) is SSRF-guarded** because the URL is **script-controlled**: host allowlist (`allowed_hosts`), private/internal IP blocking, redirect re-validation.
- **`db` (`db.rs`) and `mail` (`mail.rs`) are trusted** because the connection is **operator-supplied** in `config.db` / `config.mail` — they connect to whatever host the config names, **no SSRF block**. This is intentional (internal Postgres / self-hosted SMTP relays must work). A new capability that takes operator config follows the db/mail model; one that takes script-supplied targets must guard like http.

### `db` is async (Tier 2 resilience)

`db.rs` uses **async `tokio-postgres`**, not the sync `postgres` crate: each query runs
via `handle.block_on(tokio::time::timeout(deadline, fut))` on the `spawn_blocking` thread,
so a hung query is bounded by the execution wall-clock budget (`DB_TIMEOUT`) even when the
server-side `statement_timeout` is lost through a transaction-mode pooler. The `block_on`
**must** run on the blocking thread (never a runtime worker). The string-in/string-out FFI
contract is unchanged — but `db.rs` is no longer a pure copy of the _sync_ capability
template; other capabilities (`mail`/`s3`/`redis`/`amq`) remain sync. See
`docs/design/resilience.md`.

### Numbers / decimals

`db.rs` maps Postgres types to JSON with one rule: values that don't fit a JS number exactly come back as **strings** — `BIGINT` (INT8) and `NUMERIC`/`DECIMAL` are strings; INT2/INT4 and floats are numbers. The `$`/`Decimal` global (`decimal.rs`, backed by `rust_decimal` — the same engine that decodes `NUMERIC`) gives exact in-script math. JS has no operator overloading, so it's method-based (`.add().mul().round()`), not `+ - * /`; `__decimal(op, a, b)` does the work and stays panic-free via `Decimal::checked_*`.

## Conventions

- **Lint gauntlet:** `[workspace.lints]` in the root `Cargo.toml` (inherited by both crates via `[lints] workspace = true`) forbids `unsafe`, denies `clippy::{all,pedantic,nursery,cargo}` plus many restriction lints (no `unwrap`/`expect`/`panic`, no bare arithmetic — use `checked_*`/`saturating_*`, no `as` casts, `missing_docs_in_private_items`, no `#[allow]` — use `#[expect(..., reason="...")]`). Mirror an existing module (`db.rs` is the canonical template) and keep functions small (cognitive-complexity and line thresholds in `clippy.toml`).
- **Beginner docs** live in `docs/` (a kid-friendly guide to each capability). Keep them in sync with API changes; `README.md` is the reference version.
- **Capability method names are `snake_case` — always.** Every method on a capability global (`api`/`db`/`mongo`/`mail`/`s3`/`redis`/`amq`/`auth`, and any future one) uses `snake_case`, e.g. `s3.upload_url`, `auth.user_info`, `mongo.find_one`/`insert_many`/`update_one`. **Do not** copy the underlying library's casing — MongoDB's `findOne`/`insertMany` etc. become `find_one`/`insert_many`. The internal string-in/string-out FFI **action token** (the first arg to `__<cap>`) must use the same `snake_case` name as the JS method, kept in sync between `src/js/<cap>.js` and the Rust dispatch `match`. (Exception: the value-util globals `$`/`Decimal` use JS-idiomatic camelCase fluent methods like `toCents`/`toString` — see the next bullet. The snake_case rule is for the I/O capabilities only.)
- **Util API surface — one canonical, IntelliSense-discoverable form.** New helpers on a value-util global like `$`/`Decimal` (and any future util we add) are exposed as **chainable instance methods only** (e.g. `$(x).toCents()`, camelCase to match JS natives like `toString`), never duplicated as static shortcuts on the factory (no `$.toCents(x)`). Every public method must be declared in `container/types.d.ts` so editor autocomplete (the bundled `tsconfig.json` runs `checkJs`) is the single source of truth for what's callable. One way to do a thing, and it shows up in IntelliSense.
- **Releases** are CI-only (`.github/workflows/release.yml`, manual `workflow_dispatch` with a version bump) — it bumps `Cargo.toml`, tags, and pushes the image to GHCR. Don't hand-edit versions for a release.
