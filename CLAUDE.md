# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

jsbox is a sandboxed JavaScript execution service in Rust. Clients `POST /execute` a JS
`handler(ctx)` function plus a JSON context; the server runs it in an isolated QuickJS
context and returns `{data, error, meta}`. The single endpoint is the whole product.

## Commands

The project uses [Task](https://taskfile.dev) (`Taskfile.yml`). Raw `cargo` equivalents in parens.

- **Build:** `task build` (`cargo build`) · release: `task build-release` (`cargo build --release`)
- **Run:** `task run` (`cargo run`) — serves on `http://127.0.0.1:3000`
- **Format:** `task fmt` / `task fmt-check`
- **Lint:** `task clippy` (`cargo clippy`) — see the lint warning below
- **Unit tests:** `cargo test`
- **Integration tests:** `task test-db-up` (starts Postgres + CockroachDB via `docker compose`), then `python test_simple.py`. The Python harness **starts its own server** with `cargo run`, so don't run one separately. It is a custom runner (not pytest) — there is no per-test name filter; edit `main()` in `test_simple.py` to narrow what runs.
- **Everything:** `task` (fmt-check + clippy + tests + supply-chain) · `task check` (no supply-chain)
- **Supply chain:** `task supply-chain` (cargo-audit + cargo-deny + cargo-vet; install via `task setup`)
- **Docker:** `task docker-build` / `task docker-run`

### CRITICAL: `cargo build` does not run the clippy lints

The strict lint contract lives in `[lints.clippy]` in `Cargo.toml`, and **`cargo build` / `cargo test` do NOT enforce it** — only `cargo clippy` does. Always run `task clippy` before considering a change done; a clean `cargo build` is not enough. A hard clippy error can also short-circuit later lint passes, so fixing one error often surfaces more on the next run — re-run until truly clean.

## Build environment gotchas

- **`aws-lc-sys` (the rustls crypto backend) needs a C toolchain.** Plain Windows hosts without MSVC build tools + NASM cannot build this project natively. **Build and test via Docker** (the `Dockerfile` cross-compiles to musl/Alpine, which handles `aws-lc-sys` with just `musl-dev`). The release `Dockerfile` is multi-stage (cargo-chef + fat-LTO + `strip` → distroless/static, ~18 MB). For a fast functional test, a debug `cargo build` on `rust:1.92-alpine` is much quicker than the release LTO build and still enforces the rustc lints.
- **rustls provider is `aws-lc-rs`, not `ring`.** When adding a TLS-using dependency, configure it with `rustls-no-provider` + the dep's `aws-lc-rs` feature so it reuses the existing provider. Pulling `ring` (or default `native-tls`/OpenSSL) links a second crypto stack and bloats the binary.

## Architecture (request lifecycle)

`src/main.rs` wires an axum router (`/execute`, `/health`) with a `JsPool` as shared state.

1. **`handler.rs`** — `execute()` validates input sizes, then `tokio::task::spawn_blocking` (QuickJS is synchronous/single-threaded and must run off the async runtime).
2. **`pool.rs`** — `JsPool` is a fixed pool of pre-warmed `Runtime`s (sized to CPU cores). `acquire()`/`release()`; a fresh `Context` is created per request (cheap) so global scope never leaks between requests. `release()` runs GC.
3. **`engine.rs`** — `run()` is the orchestrator: sets a timeout interrupt handler, makes a `Context`, injects the `json()` bridge → injects `$`/Decimal → injects capabilities (`inject_apis`) → evals the user script → removes `eval`/`Proxy` → calls `handler(ctx)` → extracts the JSON result. Returns `ExecResult { js_json, http_metrics, db_metrics, mail_metrics }`.
4. **`handler.rs`** — parses the JS `{data, error}` envelope (zero-copy via `RawValue`), attaches the Rust-computed `meta`, returns `{data, error, meta}`.

`config.rs` loads optional `config.json` (server bind + engine sandbox limits, with human-readable byte sizes like `"32mb"`). Sandbox limits: memory, stack, wall-clock timeout, max script/context bytes, and `max_ops` (caps total external operations per execution).

### The capability pattern (how `api`, `db`, `mail` work)

Each capability is a near-identical module. To add or modify one, follow the existing shape:

- A native function registered with `Function::new` named `__<cap>`, with a **string-in / string-out JSON FFI contract** (no rich types cross the QuickJS boundary). The matching JS wrapper lives in `src/js/<cap>.js`, embedded via `include_str!` and `eval`'d after registration to expose a clean global (`api`, `db`, `mail`, `$`).
- **Per-request, opt-in:** the capability is injected only if its config block is present in the request (`engine.rs::inject_apis`). No config → the global simply doesn't exist (`typeof mail === "undefined"`). `$`/Decimal is the exception — it's pure (no I/O) and **always injected**, no config.
- **Metered:** each op goes through `sandbox.rs` (`check_op_limit`, `record`); metrics collect into a `Collector<T>` and drain into the response `meta.<cap>_requests`.
- Files touched when adding a capability: new `src/<cap>.rs` + `src/js/<cap>.js`, `mod` in `main.rs`, a branch in `engine.rs::inject_apis` + fields in `ExecParams`/`ExecResult`, and `RequestConfig` + `Meta` in `handler.rs`.

### Two trust models — pick the right one

- **`http` (`http.rs`) is SSRF-guarded** because the URL is **script-controlled**: host allowlist (`allowed_hosts`), private/internal IP blocking, redirect re-validation.
- **`db` (`db.rs`) and `mail` (`mail.rs`) are trusted** because the connection is **operator-supplied** in `config.db` / `config.mail` — they connect to whatever host the config names, **no SSRF block**. This is intentional (internal Postgres / self-hosted SMTP relays must work). A new capability that takes operator config follows the db/mail model; one that takes script-supplied targets must guard like http.

### Numbers / decimals

`db.rs` maps Postgres types to JSON with one rule: values that don't fit a JS number exactly come back as **strings** — `BIGINT` (INT8) and `NUMERIC`/`DECIMAL` are strings; INT2/INT4 and floats are numbers. The `$`/`Decimal` global (`decimal.rs`, backed by `rust_decimal` — the same engine that decodes `NUMERIC`) gives exact in-script math. JS has no operator overloading, so it's method-based (`.add().mul().round()`), not `+ - * /`; `__decimal(op, a, b)` does the work and stays panic-free via `Decimal::checked_*`.

## Conventions

- **Lint gauntlet:** `[lints]` in `Cargo.toml` forbids `unsafe`, denies `clippy::{all,pedantic,nursery,cargo}` plus many restriction lints (no `unwrap`/`expect`/`panic`, no bare arithmetic — use `checked_*`/`saturating_*`, no `as` casts, `missing_docs_in_private_items`, no `#[allow]` — use `#[expect(..., reason="...")]`). Mirror an existing module (`db.rs` is the canonical template) and keep functions small (cognitive-complexity and line thresholds in `clippy.toml`).
- **Beginner docs** live in `docs/` (a kid-friendly guide to each capability). Keep them in sync with API changes; `README.md` is the reference version.
- **Releases** are CI-only (`.github/workflows/release.yml`, manual `workflow_dispatch` with a version bump) — it bumps `Cargo.toml`, tags, and pushes the image to GHCR. Don't hand-edit versions for a release.
