# Handoff — QUIC remote transport for `fabricd` (Project B, thin slice)

**Branch:** `quic-remote-transport` (off `main`). **Status: all 5 tasks done & verified in Docker.**
The slice is complete except the **SA-token (OIDC) authenticator**, which is a deliberate wired-but-
unimplemented seam (see task 4 below). Ready to commit.

This is the **one approved slice of Project B** (the "network fabric"): let `fabricd` run on a
*different host* than the box, as a **shared, stateless, replicated cluster service** (egress /
credential broker) that many `runlet` pods reach over QUIC. It deliberately does **not** pull in
NATS, membership/gossip, or the autonomous mesh — those stay parked. Full design:
`docs/design/network-fabric.md` → section **"QUIC remote transport (approved slice)"**. Strategic
context + running status: the `resource-egress-fabric-direction` memory.

---

## Locked design decisions (do not relitigate)

- **Topology:** `fabricd` = shared, stateless, replicated cluster service. Credential `resources`
  table is replicated read-only config (a mounted Secret); sessions are per-connection; no shared
  state. Breaker is per-replica.
- **Transport:** QUIC (`quinn`) on the existing `aws-lc-rs` rustls provider. **UDS stays the
  zero-config local default**; QUIC only when `fabricd_quic` is configured. Chosen over TCP+rustls
  (would be rewritten later) and ZeroMQ (C dep + CurveZMQ second crypto stack).
- **Three independent security layers** (mapped to k8s):
  1. *Encryption / anti-MITM* — QUIC-TLS with a **pinned self-signed server cert** (box pins the
     daemon cert by SHA-256 fingerprint; **no CA, no cert manager**).
  2. *Reachability* — k8s **NetworkPolicy** (not WireGuard; CNI is the underlay inside a cluster).
  3. *Identity* — **pluggable auth seam** on `WireInit.token`: **primary = k8s projected
     ServiceAccount token** (audience-bound, **OIDC-verified** vs cluster JWKS → per-pod identity,
     auto-rotation, revocation); **fallback = opaque shared static token** (constant-time compare,
     `current + previous` rotation). Rejected full mTLS (cert-manager burden) and a homemade JWT.
- **HA:** headless Service + box-side discovery & **client-side failover** across replicas; the
  box endpoint config is a **list**.
- **Hardening in scope:** connection resilience/reconnect, daemon connection+stream caps,
  auth-failure metric, token/secret redaction (never log the token).

---

## Done & verified (tasks 1–3)

All verified in Docker (`rust:1.92-alpine`): clippy clean on the **gate** (plain `cargo clippy`,
**not** `--all-targets` — the gate does not lint `#[cfg(test)]` code), tests green, single crypto
stack (`cargo tree -i ring` empty).

1. **Design doc** — `docs/design/network-fabric.md`: flipped the top banner, added the full
   "QUIC remote transport (approved slice)" section.
2. **Foundation — `crates/fabric-wire/src/quic.rs`** (new): `server_endpoint` / `client_endpoint`
   builders, `ServerTls` (PEM + DER loaders), `cert_fingerprint`, and `PinnedServerVerifier` (custom
   rustls `ServerCertVerifier` trusting one cert by SHA-256 DER fingerprint; sig checks delegate to
   the provider). ALPN `fabricd/1`, 30s idle / 10s keepalive. Deps added to `fabric-wire`: `quinn`
   0.11 (`default-features=false` + `runtime-tokio,rustls-aws-lc-rs,log`), `rustls-pemfile`, `sha2`;
   `rcgen` 0.13 (`aws_lc_rs`) as dev-dep. **2 round-trip tests pass** (`frame_round_trips_over_quic`,
   `wrong_pin_is_refused`); 10/10 crate tests green.
3. **Box wiring — `crates/runlet/`:**
   - `fabric-wire/src/wire.rs`: `WireInit` gained `token: Option<String>` (serde default +
     skip-if-none; **secret, never log it**).
   - `src/uds.rs` → **renamed `src/sidecar.rs`**: `SidecarTransport` enum (`None | Uds(Arc<str>) |
     Quic(Arc<QuicClient>)`) with `from_config` + `label`; `BoxAuth` (`None | Static |
     ServiceAccountFile`, re-reads the SA file per session); `QuicClient` (one endpoint, replica
     `host:port` list, cached `Connection` + failover/reconnect); egress generalized
     `UdsEgress`→`SidecarEgress` over a `SessionConn` enum (`Uds(UnixStream) | Quic{send,recv}`);
     `connect_session(transport,&init)` dispatches and attaches the token on QUIC.
   - `src/config.rs`: `FabricdQuic { replicas, server_name, server_cert_pin (hex), auth_token?,
     auth_token_file? }` + `Config.fabricd_quic`. UDS `fabricd_socket` kept (QUIC wins if both set).
   - `src/handler.rs`: `AppState.fabricd_socket` → `transport: SidecarTransport`.
   - `src/main.rs`: builds the transport. `runlet` deps += `quinn`, `hex`.
   - Verified: clippy clean (`runlet` + `fabricd`), runlet 9/9, fabric-wire 10/10.

---

## Done: task 4 — the daemon (`crates/fabricd/`)

1. ✅ **QUIC listener.** `main.rs` now runs UDS and/or QUIC (config selects; UDS stays the
   zero-config default, QUIC engages when `quic` is set, a QUIC-only daemon binds no UDS). The QUIC
   accept loop completes the handshake, then serves each inbound `accept_bi()` stream as its own
   session. `serve` was generalized from `serve(UnixStream)` to `serve<R: AsyncRead, W: AsyncWrite>`
   so it feeds on the UDS split halves **or** the QUIC send/recv halves (mirroring the box's
   `SessionConn`). `Init → auth → resolve → Ack → Call*/Drain` unchanged otherwise.
2. ✅ **Pluggable auth seam** (`crates/fabricd/src/auth.rs`). `ClientAuthenticator` trait validates
   `WireInit.token` **before** `resolve()`. Providers selected by `quic.auth.mode`:
   - **`none`** — no client auth (trusted/isolated network).
   - **`static`** — opaque shared secret, **constant-time compare** (own `ct_eq`, no new dep),
     accepts `static_token` + `previous_token` for zero-downtime rotation. **Shipping + tested.**
   - **`sa-token`** (k8s OIDC) — **wired seam, NOT implemented.** **DESIGN DECISION (made, with the
     user):** the `auth` backend does **delegated** validation (a `userinfo` round-trip) — explicitly
     *no local JWT/JWKS crypto* — so there was **nothing to reuse** for offline JWKS verification.
     Full OIDC needs a new JWT/JWKS dep + a cluster to test (un-smoke-testable here), so it was kept
     out of this thin slice. The provider exists behind the trait and returns a clear "not
     implemented; use mode: static" rejection. **To finish it:** add JWKS fetch+cache + RS256/`aud`/
     `exp` verification (the `audience` config field is already plumbed).
   - On failure: an `InitError { code: "UNAUTHENTICATED" }` (the box maps it to `400`); bumps a
     `Shared.auth_failures` counter, logged (never the token).
3. ✅ **Hardening.** Connection cap (`quic.max_connections`, default 1024) via an accept-loop
   `Semaphore`; per-connection **stream cap** via `max_concurrent_bidi_streams(256)` + uni-streams
   refused, set in `fabric-wire`'s shared `transport_config`. Token **redacted**: `WireInit` got a
   hand-written `Debug` that prints `token: Some("<redacted>")`, so even `?`-logging a `WireRequest`
   can't leak it; `StaticAuthenticator`'s `Debug` redacts its secrets too.
4. ✅ **Config:** `fabricd` gained `quic { listen, server_cert (PEM), server_key (PEM),
   max_connections?, auth { mode, static_token?, previous_token?, audience? } }`.

## Done: task 5 — verify

- ✅ **`smoke_quic.sh`** (new): box → QUIC → `fabricd` → Postgres end-to-end (pinned self-signed cert
  via `openssl`, pin = `sha256(cert DER)`), **plus two negatives** — a wrong token and an absent
  token, both `400 UNAUTHENTICATED` with no query run. **Verified:** all three pass (run in
  `rust:1.92-alpine` on the `jsbox_default` compose network; both ends in one container over loopback
  UDP — split across hosts by pointing `replicas` elsewhere, nothing else changes). The
  framing-over-`quinn` round-trip + wrong-pin tests already live in `fabric-wire`; daemon-side auth
  unit tests (valid/rotation/wrong/missing/none/sa-token) are in `auth.rs` (5 tests, green).
- ✅ **Supply chain.** The only new crates vs. the prior commit were `pem`/`rcgen`/`rustls-pemfile`/
  `yasna` (quinn/quinn-proto/quinn-udp/lru-slab were already in-tree + exempted). Added
  `safe-to-deploy` exemptions for the four in `supply-chain/config.toml` (canonically ordered per
  `cargo vet fmt`); **`cargo vet` exits 0** (the only change is `config.toml`; `imports.lock`
  untouched). NB: `cargo vet --locked` *fails* — but only because it can't fetch the imported audit
  sets offline (22 *pre-existing* bench/cache deps like criterion/clap/moka show as unvetted); the
  online `cargo vet` that `task supply-chain` runs covers them.
- ✅ **Docs.** `CLAUDE.md` crate blurbs (`fabric-wire`/`runlet`/`fabricd`), `docs/design/
  resource-egress.md` (new step 6), and `docs/design/network-fabric.md` (status → *implemented*)
  all updated.

## Verified clippy/test (Docker, `rust:1.92-alpine`, gate = plain `cargo clippy`)

`fabric-wire` 10/10 + clippy clean · `fabricd` 5/5 (auth) + clippy clean · `runlet` 9/9 + clippy
clean · `cargo tree -i ring` empty (single crypto stack) · my crates `cargo fmt --check` clean.

## Known pre-existing (NOT introduced here, out of scope)

`cargo fmt --check` flags `crates/fabric-backends/{backendset,kv,mail,mongo,resources}.rs` — those
files were committed fmt-dirty **before** this work (I touched none of them). Left as-is to keep this
diff focused; clean them in a separate fmt-only commit if you want the workspace `task fmt-check`
fully green.

---

## Build / verify recipe (Docker-only — native Windows can't build `aws-lc-sys`)

Run from **PowerShell** (Git Bash mangles the `-v` path). The host `target/` is mounted, so rebuilds
are incremental. The **leaf-first loop is much faster**: clippy `-p fabric-wire` only compiles the
leaf; `-p runlet -p fabricd` is the slow one (rquickjs + drivers).

```
# gate (lib+bins) + tests for one crate:
docker run --rm -v "C:\Users\Toy\Documents\GitHub\jsbox:/work" -w /work rust:1.92-alpine sh -c \
  "apk add --no-cache musl-dev >/dev/null 2>&1 && rustup component add clippy >/dev/null 2>&1 && \
   cargo clippy -p fabricd && cargo test -p fabricd 2>&1 | tail -n 40"
```

The gate is **plain `cargo clippy`** (per `Taskfile.yml`), so `#[cfg(test)]` code is **not** linted
— test-only `absolute_paths` / `indexing_slicing` won't fail it (existing tests rely on this).

## Lint gotchas already hit (the gauntlet is strict — no unwrap/expect/panic/as/indexing/arith, docs on every private item)

- rustls 0.23 `ServerCertVerifier` has **no** `request_ocsp_response`, but `missing_trait_methods`
  (denied) **requires** `root_hint_subjects` + `requires_raw_public_keys` — implement them.
- Builder methods returning `&mut` (`transport_config`) trip `unused_results` → bind `let _ =`.
- `significant_drop_tightening` (nursery) → `drop(guard)` after last use of a tokio `MutexGuard`.
- `collapsible_if` → use an edition-2024 let-chain `if let Some(x) = … && cond { … }`.

## State

The foundation (tasks 1–3) is committed as `fee9c86` on `quic-remote-transport`. The daemon +
verification (tasks 4–5) are **staged in the working tree, not yet committed** — changed:
`crates/fabricd/{Cargo.toml,src/main.rs}` + new `src/auth.rs`, `crates/fabric-wire/src/{wire,quic}.rs`,
`supply-chain/config.toml`, `Cargo.lock`, new `smoke_quic.sh`, and docs (`CLAUDE.md`,
`docs/design/{resource-egress,network-fabric}.md`). Ready to commit as the task-4/5 slice. Memory
`resource-egress-fabric-direction` carries the same status.
