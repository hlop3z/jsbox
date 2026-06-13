# Security hardening — progress tracker

Threat-model-driven hardening pass. Each item names the **trust boundary** it defends,
the **attack** it closes, and the **control**. Status is tracked here as work lands.

> Verification: native Windows can't build `aws-lc-sys`, so each change is checked with a
> Docker debug build that runs the clippy gauntlet + unit tests:
> `docker run --rm -v ${PWD}:/w -w /w rust:1.92-alpine sh -c "apk add musl-dev && cargo clippy --all-targets && cargo test"`

## Trust boundaries (the model)

1. **Network → `/execute`** — caller is fully trusted (supplies JS *and* credentials *and*
   `allowed_hosts`). Today: **no authN in jsbox**; relies entirely on a gateway/NetworkPolicy.
2. **Script → host process** — QuickJS sandbox (mem/stack/timeout/op caps; `eval`/`Proxy`
   removed). Design is trending toward *untrusted, customer-authored* scripts.
3. **Script-chosen target → outside world** — `api`/`s3` SSRF-guarded (script picks URL);
   `db`/`mail`/`redis`/`amq` trusted (operator picks host).

## Work items

| # | Item | Boundary | Severity | Status |
|---|------|----------|----------|--------|
| 1 | IPv6 internal ranges in SSRF filter (ULA `fc00::/7`, link-local `fe80::/10`, embedded-v4 6to4/NAT64) | 3 | High | ✅ done |
| 3 | Gate `allowed_hosts: ["*"]` wildcard (`allow_wildcard_hosts`; never with `debug:true`) | 3 | High | ✅ done |
| 4 | `error_debug` default → `false` (secure by default) | 1 | Medium | ✅ done |
| 5 | Cap handler returned-output size (`max_output_size` → `OUTPUT_TOO_LARGE`) | 2 | Medium | ✅ done |
| 6 | Fail-closed auth on `/execute` (`access_token` bearer + refuse exposed bind w/o it) | 1 | Critical | ✅ done |
| 9 | Document `Function`/`AsyncFunction` survive `eval` removal (isolation-only) | 2 | Low | ✅ done |
| 2 | Pin resolved IP through reqwest (close DNS-rebind TOCTOU on `api`) | 3 | High | ✅ done |
| 7 | Mail: operator recipient-domain allowlist + per-exec send cap | 3 | Medium | ⬜ todo |
| 8 | Verify: ReDoS catastrophic-backtrack is preempted by the wall-clock interrupt | 2 | Low | ⬜ todo |

## What landed (verified: `cargo fmt --check` clean, clippy gauntlet 0 errors, 39 unit tests pass)

- **1 — IPv6 SSRF** (`ssrf.rs`): new `is_private_v6` / `v6_embeds_private_v4` cover loopback,
  v4-mapped, ULA `fc00::/7`, link-local `fe80::/10`, and private-v4 smuggled via 6to4
  (`2002::/16`) / NAT64 (`64:ff9b::/96`). `std`'s `is_unique_local`/`is_unicast_link_local`
  are still unstable, so ranges are matched on segments directly. +6 tests.
- **3 — wildcard gate** (`config.rs`/`engine.rs`/`http.rs`/`handler.rs`): `EngineConfig.allow_wildcard_hosts`
  (default `false`). `is_host_allowed` honors `*` only when `wildcard_allowed`, resolved in the
  handler as `allow_wildcard_hosts && !debug` — a wildcard is never honored in SSRF-relaxed mode.
- **4 — `error_debug` default `false`** (`config.rs`): now derives `Default`; deployment doc updated.
- **5 — output cap** (`config.rs`/`engine.rs`/`handler.rs`/`metrics.rs`): `EngineConfig.max_output_size`
  (`0` = off). `engine::enforce_output_cap` turns an oversized success into `OUTPUT_TOO_LARGE` (422),
  bucketed with `malformed_response` in metrics.
- **6 — `/execute` auth** (`config.rs`/`main.rs`/`handler.rs`): `access_token` bearer gate with a
  constant-time compare (`ct_eq`); `Config::check_exposure` refuses a non-loopback bind with no
  token unless `allow_unauthenticated`. `/health` + `/metrics` stay open. +7 tests.
- **9 — sanitize_globals doc** (`engine.rs`): records that `new Function`/`AsyncFunction`/
  `GeneratorFunction` survive `eval`/`Proxy` removal — isolation-only, not a dynamic-code block.
- **2 — DNS-rebind pinning** (`http.rs`): `SsrfResolver` implements `reqwest::dns::Resolve` and
  is installed via `ClientBuilder::dns_resolver`, so the address reqwest connects to is one the
  SSRF classifier passed — closing the TOCTOU window between the `validate_url` pre-check and the
  connect-time lookup. `getaddrinfo` runs on `spawn_blocking` (the blocking client is a
  current-thread runtime) so the request timeout still fires during a slow lookup; the filter
  fails closed (no public address → error), and skips in `debug`/`allow_private` mode. The
  `block_private_ip` pre-check stays for literal IPs + a clean in-band `HTTP_SSRF_BLOCKED`. +3
  tests (wildcard gating + the resolver filter, hermetic via IP literals).

## Remaining follow-ups (7–8)

- **7 — mail abuse controls.** A script picks arbitrary `to`/subject/body against the operator's
  SMTP relay (open spam cannon if scripts are untrusted). `max_recipients` exists; add an operator
  recipient-domain allowlist and a per-execution send cap.
- **8 — ReDoS vs interrupt.** Confirm the wall-clock interrupt preempts catastrophic-backtracking
  regex in QuickJS (the interrupt fires between ops; libregexp may not yield). One adversarial
  test (`/(a+)+$/` on a long non-match) settles it.

## Log

- _(init)_ Tracker created; threat model captured. Docker verified available (29.1.3).
- Landed items 1, 3, 4, 5, 6, 9. Full gauntlet green in Docker (`rust:1.92-alpine`): fmt-check
  clean, `cargo clippy --all-targets` 0 errors, `cargo test` 36 passed.
- Landed item 2 (DNS-rebind pinning via `reqwest::dns::Resolve`). Gauntlet green: fmt clean,
  clippy 0 errors, `cargo test` 39 passed. Remaining: 7 (mail abuse), 8 (ReDoS verification).
