# Network Fabric (Project B)

Companion to [resource-egress.md](resource-egress.md).

> **Status: vision / parked.** This captures the longer-horizon "global service fabric" idea and
> the design decisions already taken, so they are not lost. **Active work is Project A**
> ([resource-egress.md](resource-egress.md)); the fabric is the eventual backend behind the
> `Resource` egress port, not a prerequisite for it.

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
