# Network Fabric (Project B)

Companion to [resource-egress.md](resource-egress.md).

> **Status: vision / parked — except one built slice.** Most of this doc (NATS event bus,
> custom membership / gossip / discovery mesh) remains parked. **The single exception, approved and
> built 2026-06-30, is the [QUIC remote transport](#quic-remote-transport-approved-slice)**:
> the one increment that lets `fabricd` run on a *different host* than the box. It deliberately
> does **not** pull in NATS, SWIM/HyParView/Plumtree, or any of the autonomous-mesh machinery —
> those stay shelved until a measured need (see [resource-egress.md](resource-egress.md)). The
> rest of this doc is the longer-horizon backdrop that slice fits into.

## Goal

A **location-independent transport layer**: applications address *services*, not IP addresses,
and Docker / Kubernetes / VMs / bare metal are irrelevant. The fabric is what `fabricd` (the
sidecar that the box's `Resource` egress talks to) becomes once it needs to reach services
across nodes and clouds.

```
fabric.call()      fabric.stream()      fabric.publish()      fabric.subscribe()      fabric.register()
```

## Responsibility boundary

**Responsible for:** connectivity, service discovery, routing, RPC, streaming, pub/sub,
membership, failure detection, load balancing, encryption.

**Not responsible for** (these belong to Docker / containerd / Nomad / Kubernetes):
scheduling, container lifecycle, autoscaling, resource allocation. The fabric does not replace
the scheduler — services simply `register()` themselves and the fabric routes to them, wherever
they run.

## How it connects to the box

The box never imports any of this. The box depends on the `Resource` trait
([resource-egress.md](resource-egress.md)); `fabricd` *implements* the far side. A capability
call `db.query(...)` becomes `__resource("db", "query", payload, binding)` → local `fabricd` →
`fabric.call("orders-db", ...)` → the node that holds that backend. The box's view ends at the
local sidecar.

## Decisions taken (2026-06-23)

| Question | Decision | Consequence |
| --- | --- | --- |
| Transport model | **Hybrid** | Pub/Sub + durable queues on **NATS JetStream**; low-latency RPC + streaming on a **custom `quinn` QUIC** layer. Do not reimplement what NATS already does well. |
| Scale target (≤18 mo) | **Tens–hundreds of nodes** | **Skip SWIM / HyParView / Plumtree for now.** At this scale, NATS/Consul-style discovery (or a simple seed list + periodic full membership exchange) is sufficient. The custom membership/topology/gossip layers are deferred until measured node count justifies them. |
| Underlay (Layer 0) | **NetBird as bootstrap, then autonomous** | See below. |
| First focus | **Box decoupling first** | Project A precedes any fabric build. |

### Why hybrid, not full custom mesh

NATS JetStream already provides ~70% of the originally-sketched fabric: subject-based
addressing (≈ service discovery), request/reply (≈ RPC), queue groups (≈ client-side load
balancing), super-clusters + gateways (≈ cross-cloud), clustering + RAFT (≈ membership /
failure detection). Building SWIM + HyParView + Plumtree + a custom RPC protocol *from scratch*
is, for most needs, NATS reimplemented. The custom `quinn` QUIC layer is justified **only**
where NATS genuinely falls short:

1. Bidirectional long-lived streams with **connection migration** (AI agents, video, telemetry,
   replication) — a real `quinn` advantage.
2. **Broker-free p2p latency** where the extra broker hop is unacceptable.
3. **Tens-of-thousands-of-node** topology beyond comfortable NATS super-cluster scale.

Until #1–3 are concrete present needs, the bespoke membership/topology/gossip stack stays on
the shelf. When that day comes, the proposed stack (SWIM membership, HyParView partial mesh,
Plumtree broadcast, rendezvous-hash routing over a Moka hot cache, optional Postgres for
durable metadata only) is the right shape — it is the *timing* that is deferred, not the design.

### Underlay: NetBird bootstrap → autonomous steady state

Currently using NetBird (WireGuard mesh). The requirement: **NetBird bootstraps the mesh, but
after first connection nodes operate independently of the NetBird controller.**

Key facts that make this tractable:

- NetBird's **data plane is already controller-independent** — once peers exchange endpoints and
  keys via the management + signal servers, WireGuard traffic flows **direct P2P**, not through
  the controller.
- What still needs the controller in steady state is **coordination**: peer/endpoint discovery,
  key rotation, ACLs, and NAT-traversal signaling for *new* paths.

So "autonomous after bootstrap" means: at join, cache the peer set + endpoints + keys + ACLs
obtained from NetBird, then run our **own thin control channel over the QUIC fabric** (authed by
certificates minted at bootstrap) that gossips membership and endpoint/key changes. A dead
NetBird controller then stops affecting routing — the fabric self-maintains.

**The one honest caveat:** re-establishing a path through a *hard / symmetric NAT* for a
brand-new peer while the controller is down still needs a relay or signal server. For
tens–hundreds of **cloud** nodes with stable public/overlay endpoints this is a non-issue; it
only bites laptop/edge peers behind hostile NATs. Document the boundary; don't pretend it away.

## Recommended stack (when the fabric is built)

| Component | Technology | Notes |
| --- | --- | --- |
| Underlay | NetBird (WireGuard) → self-gossiped | Bootstrap-then-autonomous, per above |
| Transport | QUIC (`quinn`) | RPC + streaming; mTLS via the shared `aws-lc-rs` provider |
| Event bus | NATS JetStream | Pub/Sub + durable queues; cross-region via super-clusters |
| Discovery (now) | NATS subjects / Consul / seed list | **Not** custom SWIM at current scale |
| Discovery (later) | SWIM + HyParView + Plumtree | Only at tens-of-thousands of nodes |
| Routing cache | Moka | Hot `ServiceId → [Route]` with 1–5s TTL |
| Durable metadata | PostgreSQL (optional) | Services / certs / ACLs; routing must survive its loss |
| Load balancing | Client-side in the SDK | nearest / least-RTT / weighted RR / rendezvous hash |
| Scheduling | External (Docker/K8s/Nomad) | Explicitly out of scope |

## `fabricd` (the node daemon)

```
fabricd
├── QUIC transport (quinn)
├── Router + Moka hot cache
├── Service registry
├── NATS adapter (pub/sub, queues)
├── Resource backends (db / mongo / mail / redis / amq / auth drivers)  ← moved out of runlet-core
├── Metrics
└── [later] SWIM / HyParView / Plumtree
```

Target footprint ~100–200 MB RAM. The **Resource backends** row is the bridge to Project A:
the drivers that leave `runlet-core` land here.

---

# QUIC remote transport (approved slice)

> **Status: implemented 2026-06-30** (approved same day). The one piece of the fabric vision built
> so far: `fabric-wire::quic` (pinned-cert endpoints), the box's `runlet::sidecar` (UDS-or-QUIC
> egress with cert pinning + client-side failover), and `fabricd`'s QUIC listener + pluggable
> `ClientAuthenticator` (`none`/`static`/`sa-token`) + connection/stream caps. Verified end-to-end
> by `smoke_quic.sh` (happy path + wrong/absent-token negatives). The **`sa-token`** provider (k8s
> projected `ServiceAccount` token, verified offline against the cluster JWKS) is implemented in
> `fabric_backends::sa_token` (`JwksVerifier`: background-refreshed JWKS cache + offline RS256 +
> `aud`/`iss`/`exp` check) and unit-tested hermetically; the remaining open item is the **KIND
> end-to-end** test (`smoke_satoken.sh`) that exercises it against a real projected token in-cluster.
> Everything below the `## Deferred` line at the end stays parked.

## Why only this slice

Project A made `fabricd` a **local** egress sidecar reached over a Unix-domain socket (UDS). The
only structural limit that leaves is *locality* — `fabricd` must share a host with the box. The
single increment that removes that limit, without committing to the full mesh, is **swapping the
UDS for a QUIC link** so `fabricd` can run on a different host. No NATS, no membership/gossip, no
discovery layer — those remain [deferred](#deferred-still-parked).

This is a thin swap because the wire contract is already transport-neutral: `fabric_wire::wire`
(`read_frame`/`write_frame` + the `Init → Call* → Drain` session state machine) is written against
`tokio::io::AsyncRead`/`AsyncWrite`. A `quinn` bidirectional stream (`SendStream`/`RecvStream`)
implements exactly those, so **the framing, name-resolution (`resolve`), metrics, and
error-mapping code do not change** — only the two places that *produce* a stream do.

## Topology: shared cluster broker, not a per-pod sidecar

The driving deployment is **many `runlet` pods → one shared `fabricd` service**, e.g. a Kubernetes
`Deployment` of N `fabricd` replicas that every box pod in the cluster talks to — a centralized
**egress / credential broker**, rather than a `fabricd` sidecar injected into every pod.

Why this is sound and HA-ready:

- **Stateless replicas.** The operator `resources` credential table is replicated read-only config
  (a mounted Secret), sessions are per-connection, and there is no cross-replica shared state. Any
  replica can serve any box session.
- **Credential blast radius shrinks.** Credentials live in one audited place (the broker's Secret),
  not copied into every application pod.
- **Per-replica resilience.** The daemon-side circuit breaker is per-replica — each trips
  independently on its own view of backend health. Acceptable; no coordination needed.

UDS stays the **zero-config local default** (single-host / dev / the existing same-pod sidecar
deployment). QUIC is selected only when a remote endpoint is configured. One `Transport` enum
behind the existing `Egress` seam; the box picks UDS or QUIC at startup.

## Three independent security layers

Inside a single cluster the box→`fabricd` hop rides the **CNI pod network**, which is generally
neither encrypted nor cryptographically per-peer authenticated — so we do **not** assume a
WireGuard/NetBird underlay here (that assumption belongs to the cross-cloud future). Security is
three independent layers, each mapped to a primitive we already run:

| Concern | Mechanism | Notes |
| --- | --- | --- |
| **Encryption / anti-MITM** | QUIC-TLS 1.3 with a **pinned self-signed server cert** | QUIC mandates TLS; we use one static self-signed `fabricd` cert that the box **pins by fingerprint** (a small custom `rustls` verifier). No CA, no cert manager — one cert file. Now load-bearing, since the CNI may not encrypt. |
| **Reachability** | Kubernetes **NetworkPolicy** | Restricts which namespaces/pods may reach the `fabricd` Service. Replaces "the WireGuard peer set." Coarse (allow/deny by label), not identity. |
| **Identity / credential-pull gate** | **Pluggable auth seam** on `WireInit` | Answers "may *this client* pull credentials at all," independent of reachability. Two providers behind one interface (below). |

### Why not mTLS, why not a homemade JWT

- **Full mTLS** (per-box client certs + CA + rotation) re-solves what the cluster already provides
  and forces a **cert manager** (step-ca / cert-manager / Vault PKI / SPIRE) at any real replica
  count. Rejected for this topology; documented as the upgrade path if the underlay ever becomes
  untrusted or per-box revocation/audit is mandated by compliance.
- **A homemade JWT** only earns its complexity when verifier and issuer are *different* parties.
  A symmetric (HS256) token shared by box and broker is a shared secret with extra footguns
  (`alg:none`, RS256↔HS256 confusion); an asymmetric one reintroduces the cert-manager burden.
  **But** the shared-broker case *does* have a real issuer — the cluster — which already mints
  per-pod identity tokens. So the right "JWT" here is **a k8s-issued one we validate**, not one we
  sign (next section). Authorization of *what* a session may touch is unchanged: it still happens
  one frame later, when `fabricd` resolves the `WireInit` logical names against its operator table.

### The pluggable auth seam

`fabricd` validates the `WireInit` credential through one `ClientAuthenticator` interface, with two
providers chosen by config:

1. **k8s projected ServiceAccount token (primary, production).** Each `runlet` pod mounts a
   projected SA token (audience = `fabricd`, short-lived, auto-rotated by the kubelet) and sends it
   in `WireInit`. `fabricd` **verifies it as an OIDC token** against the cluster JWKS (offline
   signature check + audience + expiry — no per-request API-server round-trip), giving **per-pod
   identity, automatic rotation, and revocation** (delete the ServiceAccount) with **no cert
   manager and no shared secret**. Implemented as a new `fabric_backends::sa_token::JwksVerifier`
   (a `background-refreshed JWKS cache so the synchronous accept path does no I/O; offline RS256 +
   `aud`/`iss`/`exp` validation via `jsonwebtoken`), reusing the shared `reqwest` + rustls/`aws-lc-rs`
   stack rather than the (online, introspection-oriented) `auth` capability backend. Fail-closed
   until the first JWKS fetch; the JWKS URL is explicit or OIDC-discovered from the issuer, and the
   client trusts an optional mounted cluster CA.
2. **Opaque shared static token (fallback / bootstrap / non-k8s).** A 32-byte random secret in both
   configs, **constant-time** compared (`subtle`/`constant_time_eq`), with `current + previous`
   accepted for zero-downtime rotation. Simpler, but a cluster-wide blast radius (one secret for
   all pods, no per-pod revocation) — so it is the fallback, not the default.

A failed credential is rejected before `resolve()` and the connection is closed. The token/secret
is **never** logged or echoed in an error; an **auth-failure counter** is exported (a spike is a
security signal).

## High availability & load balancing

QUIC is connection-oriented and must pin a session to one backend, which a round-robin
`ClusterIP`/UDP Service does not respect. So:

- Expose `fabricd` as a **headless Service**; the box resolves all replica IPs and does
  **client-side selection + failover** — open a `quinn::Connection` to a chosen replica, reconnect
  to another on drop. This is exactly the "client-side load balancing in the SDK" already chosen in
  the [Recommended stack](#recommended-stack-when-the-fabric-is-built) table.
- The box config's endpoint is therefore a **list** (or a headless-Service DNS name resolved to a
  set), not a single address.

## Connection lifecycle (the genuinely new surface)

UDS never had transient failures; a network link does. The box therefore:

- Holds a **long-lived `quinn::Connection`** per reachable replica and opens **one bidirectional
  stream per box-request session** (`open_bi()`), so a transaction's `begin → commit` reuse one
  stream, and there is no per-session connect cost.
- **Reconnects on a dropped connection** (replica restart, network blip) and **fails over** to
  another replica for the next request. A drop mid-request already maps to a retryable
  `IO_TRANSPORT` egress error (the engine classifies it exactly like an in-process backend fault),
  so no new error path is needed — only the reconnect.
- Keeps the existing **deadline model**: every round-trip is `block_on(timeout(deadline, …))` on
  the `spawn_blocking` thread. Network RTT now consumes part of the per-execution budget; that is
  the intended bound.

`fabricd` caps **max concurrent connections and max concurrent streams** so one misbehaving or
compromised box cannot exhaust the broker (the current accept loop spawns unbounded — a DoS surface
once it is network-reachable).

## Where the code changes

| Crate | Change |
| --- | --- |
| `fabric-wire` | Add `quinn`; a `quic` module with `client_endpoint`/`server_endpoint` builders on the existing `aws-lc-rs` `rustls` provider; a `TlsConfig` (cert/key + pinned fingerprint) and the pinning verifier. Framing untouched. |
| `runlet` (box) | Config gains the endpoint **list** + `tls` + auth-token/SA-token settings (UDS default unchanged). `uds.rs` generalizes over the stream pair (or a sibling `quic.rs`); add client-side discovery/failover + reconnect. `connect_session`/`roundtrip`/deadline reused. |
| `fabricd` | Bind a `quinn` endpoint when configured (UDS otherwise); `accept_bi` per session → existing `serve()` unchanged. Add the `ClientAuthenticator` seam (SA-token primary, opaque-token fallback), connection/stream caps, auth-failure metric, secret redaction. |

## Verification

Build/clippy/test are **Docker-only** (`aws-lc-sys` needs a C toolchain; `rust:1.92-alpine`). A
new `smoke_quic.sh` runs box and `fabricd` in **separate containers**: box → QUIC → `fabricd` →
Postgres end-to-end, plus a **rejected-credential negative** (bad/absent token ⇒ handshake/`Init`
refused, no query). A framing-over-`quinn` round-trip unit test joins the existing suite; the full
clippy gauntlet and current tests stay green. `quinn` + any new OIDC dependency are re-run through
`cargo vet` / `cargo deny` (exemptions added as needed).

## Deferred (still parked)

NATS JetStream event bus, custom SWIM/HyParView/Plumtree membership/gossip, rendezvous-hash
routing + Moka cache, and the NetBird-bootstrap-then-autonomous underlay all remain shelved per
the [decisions table](#decisions-taken-2026-06-23). This slice is point-to-point box↔broker
transport only; it is not the mesh.
