# Handoff — QUIC remote transport for `fabricd` (Project B, thin slice)

**Branch:** `quic-remote-transport` (off `main`). **Status:** 3 of 5 tasks done & verified in Docker; daemon + final verification remain.

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

## Next: task 4 — the daemon (`crates/fabricd/`)

`crates/fabricd/src/main.rs` currently binds a `UnixListener` and runs `serve(stream, …)` (which is
already written against `AsyncRead`/`AsyncWrite`, so it is **unchanged**). To do:

1. **QUIC listener.** When QUIC is configured, bind a `quinn` endpoint via
   `fabric_wire::quic::server_endpoint(addr, ServerTls::from_pem(cert, key))`. Accept loop:
   `endpoint.accept()` → `connection.await` → `connection.accept_bi()` per session →
   `serve(SessionStream, shared)`. Keep UDS as the default (config selects). `serve` already does
   `Init → resolve → Ack → Call*/Drain`; feed it the QUIC send/recv halves (it splits the stream
   today — adapt to take the two halves, mirroring the box's `SessionConn`).
2. **Pluggable auth seam.** Add a `ClientAuthenticator` trait validating `WireInit.token` **before**
   `resolve()`. Two providers, selected by daemon config:
   - **Primary: k8s SA-token (OIDC).** Verify the projected token's signature against the cluster
     **JWKS**, plus `aud` (= `fabricd`) and `exp`. **DESIGN CHOICE TO MAKE FIRST:** reuse
     `fabricd`'s existing `auth` backend OIDC machinery (it already links `fabric-backends`, which
     does OIDC discovery + token validation for the `auth` capability — check
     `crates/fabric-backends/src/auth*`) **vs** add a focused JWT/JWKS dep (e.g. `jsonwebtoken` +
     a JWKS fetch/cache). Prefer reuse if the surface fits; it avoids a new supply-chain entry.
     Cache JWKS (don't fetch per request).
   - **Fallback: opaque static token.** Constant-time compare (`subtle`/`constant_time_eq`),
     accept `current + previous` for zero-downtime rotation. (Box already sends it via `BoxAuth`.)
   - On failure: reject (close the connection / an `InitError`-style response) **before** resolve;
     bump an **auth-failure counter** (a spike is a security signal).
3. **Hardening.** Cap **max concurrent connections + streams** (the accept loop spawns unbounded
   today — a DoS surface once network-reachable). **Redact** the token: never log it; consider a
   manual `Debug`/newtype so `WireInit`'s derived `Debug` can't leak it.
4. **Config:** `fabricd` gains `quic { listen, server_cert (PEM path), server_key (PEM path),
   auth: { mode: sa-token|static|none, audience?, jwks/issuer?, static_token?, previous_token? } }`.

## Next: task 5 — verify

- **`smoke_quic.sh`** (new): `fabricd` + `runlet` in **separate containers**, box → QUIC → fabricd →
  Postgres end-to-end, **plus a rejected-token negative** (bad/absent token ⇒ handshake/`Init`
  refused, no query). Mirror the existing `smoke_5.sh` shape (UDS) but over QUIC + a generated
  self-signed cert (use `rcgen` in a tiny helper, or `openssl` in the script). Compute the pin =
  `sha256(cert DER)` and feed it to the box's `server_cert_pin`.
- Add a framing-over-`quinn` round-trip already exists in `fabric-wire` (lib); add a daemon-side
  auth unit test (valid/invalid token).
- **Supply chain:** re-run `cargo vet` / `cargo deny` for `quinn` + `rcgen` + any new OIDC dep; add
  exemptions as needed (`task supply-chain`).
- **Docs:** update the `fabric-wire` / `fabricd` / `runlet` crate blurbs in `CLAUDE.md` and
  `docs/design/resource-egress.md` to mention the QUIC remote transport.

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

Nothing committed before this handoff's commit. Memory `resource-egress-fabric-direction` carries
the same status. After this: `git log --oneline -1` should show the box+foundation commit on
`quic-remote-transport`.
