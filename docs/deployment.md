# Deployment & production hardening

jsbox is a single stateless service: `POST /execute` runs a JS `handler(ctx)` in a
sandboxed QuickJS context and returns `{data, error, meta}`. This guide is the operator's
checklist for running it safely under load. Depth lives in the design notes
([resilience.md](design/resilience.md), [pooled-capabilities.md](design/pooled-capabilities.md));
this page is the "what to actually set, and why" synthesis.

> **TL;DR checklist** (each expanded below)
>
> - [ ] `access_token` set (or `allow_unauthenticated: true` if auth is genuinely terminated upstream) — jsbox refuses to start on a non-loopback bind otherwise.
> - [ ] `debug: false` in production config (it relaxes the SSRF guard — local-testing only).
> - [ ] `error_debug` left `false` (default) at an internet-facing edge (keeps stack/raw out of responses).
> - [ ] `allow_wildcard_hosts` left `false` unless a caller genuinely needs `allowed_hosts: ["*"]` (it collapses the host allowlist to the IP filter alone).
> - [ ] `max_output_size` set in an untrusted-script deployment (caps the handler's returned JSON).
> - [ ] `/metrics` and `/health` reachable only from the pod/mesh, never the public internet.
> - [ ] Bulkhead (`max_concurrent_executions`) set to bound DB connections + blocking threads.
> - [ ] `max_statement_timeout_ms` set **and** a server-side `statement_timeout` role default.
> - [ ] `db_breaker_threshold` > 0 so a dead DB fast-fails instead of pinning threads.
> - [ ] PgBouncer `query_timeout` set if you front Postgres with a transaction-mode pooler.
> - [ ] Global per-tenant fairness lives at the gateway; jsbox's is a per-pod backstop only.
> - [ ] TLS for every operator-supplied backend (`db`/`redis`/`amq`); secrets via `config.sys`.
> - [ ] k8s: SIGTERM grace ≥ `timeout_ms`, `/health` probes, HPA on CPU or bulkhead headroom.
> - [ ] `task supply-chain` (audit + deny + vet) wired as a CI gate.

## 1. Before you expose it (the non-negotiable gates)

These are the difference between "internal demo" and "safe to point traffic at."

- **Authenticate `/execute` (fail-closed).** The `/execute` caller is fully trusted — it
  supplies the credentials (`config.db`/`mail`/…) and arbitrary JS — so an unauthenticated
  reachable port is a full compromise (SSRF pivot, mail relay, credential use). jsbox
  **refuses to start** on a non-loopback bind unless you either set `access_token` (a shared
  secret; requests must send `Authorization: Bearer <token>`, constant-time compared) or
  explicitly set `allow_unauthenticated: true` to assert auth is terminated upstream
  (gateway/mesh). `/health` and `/metrics` stay open for probes/scrape. This is defense in
  depth *behind* the gateway, not a replacement for it.
- **`debug: false`.** `debug: true` relaxes the SSRF private-IP block so `api`/`s3` can reach
  localhost/LAN targets — it exists for local testing only. In production it would let a
  script-controlled URL reach your internal network. The default is already `false`; just
  make sure no production `config.json` sets it `true`.
- **`error_debug` at the edge.** `error_debug` (default `false`, secure by default) gates
  whether stack traces and raw driver causes appear in the error envelope's `debug` block.
  Leave it off at any exposed edge so internal detail (hostnames, driver messages) never
  leaves the boundary; set it `true` only on a purely internal service where you want that
  detail inline. The `trace_id` is always present and the raw cause is always logged
  server-side, so support can correlate either way.
- **Scope `/metrics` and `/health`.** Both are unauthenticated, read-only GET endpoints
  (`/metrics` is Prometheus text; see §8). Expose them only to the scrape path / mesh —
  a `NetworkPolicy`, a sidecar, or binding the scrape to the pod IP. Never route them from a
  public ingress.

## 2. Resilience config (map the tiers to knobs)

The full model is [resilience.md](design/resilience.md). The engine config block:

| Knob                                                 | Tier | What it does                                                                                                | Production guidance                                                                                                                                                                                                                                            |
| ---------------------------------------------------- | ---- | ----------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `max_concurrent_executions`                          | 1    | Bulkhead: caps concurrent executions; excess fast-fails `429 OVERLOADED`.                                   | **Set it explicitly.** The default (auto = cores × 16) is high; size it to your DB connection budget — every concurrent `db` execution holds one connection. A value near your pool/`DEFAULT_POOL_SIZE` keeps a slow DB from exhausting threads + connections. |
| `timeout_ms`                                         | 2    | Wall-clock execution budget; also the per-query client-side DB deadline.                                    | The robust backstop for a hung query. Keep it tight (a few seconds) — it bounds how long a single request can pin a blocking thread.                                                                                                                           |
| `max_statement_timeout_ms`                           | 0    | Ceiling the request's `statement_timeout_ms` cannot exceed.                                                 | Set it (e.g. `1000`–`5000`). Then **also** set a server-side default — see below — because the session `SET` is best-effort through a transaction pooler.                                                                                                      |
| `db_breaker_threshold` / `db_breaker_cooldown_ms`    | 3    | Per-target circuit breaker: fast-fails a DB that keeps failing to connect.                                  | Turn it on (`threshold` 3–5). Measured win under a dead DB: 54× throughput, 281× lower p99 (resilience.md). `0` = off (default).                                                                                                                               |
| `max_concurrent_per_partition` / `partition_buckets` | 5    | Per-`X-Partition-Key` concurrency cap — a noisy key fast-fails `429 PARTITION_OVERLOADED` on its own share. | Optional per-pod backstop. **Global** per-tenant fairness is the gateway's job (see §5). `0` = off.                                                                                                                                                            |

**The Tier 0 server-side default (do not skip).** jsbox issues `statement_timeout` as a
session `SET` at connect. On a direct connection that's a hard guarantee; behind a
**transaction-mode PgBouncer** it is best-effort (the `SET` may bind to a different server
connection than the autocommit query). For a real guarantee set it operator-side:

```sql
ALTER ROLE app_user SET statement_timeout = '5s';
```

or a PgBouncer `connect_query`. See [pooled-capabilities.md](design/pooled-capabilities.md).

**Tier 4 — PgBouncer's own timeouts.** If you front Postgres with PgBouncer, set
`query_timeout` (slightly above your expected `statement_timeout`) and optionally
`query_wait_timeout`. It's an independent layer that catches a runaway query even when the
session `SET` is lost, and below jsbox's wall-clock deadline. There's no jsbox code for this —
it's pooler config.

## 3. Sizing the sandbox

| Knob                                   | Meaning                                                         | Notes                                                                                                                                                                                                                                        |
| -------------------------------------- | --------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `pool_size`                            | Number of pre-warmed runtimes.                                  | `0` = auto (CPU cores). One runtime ≈ one in-flight execution; the bulkhead bounds concurrency above this.                                                                                                                                   |
| `memory_limit`                         | Per-execution heap cap.                                         | Human sizes (`"32mb"`). A fat handler/module eats its own request's budget.                                                                                                                                                                  |
| `max_stack_size`                       | Per-execution stack cap.                                        | Guards runaway recursion.                                                                                                                                                                                                                    |
| `max_script_size` / `max_context_size` | Max bytes for the script and the context payload.               | Validated before execution. `max_context_size` left at `0` auto-derives to `memory_limit / 8`; an explicit value is **capped at `memory_limit / 4`** (the ~4× JSON-parse heap cost), so a max-size context always parses instead of OOM-ing. |
| `max_ops`                              | Cap on total external operations (db/api/mail/…) per execution. | Bounds a handler's downstream fan-out.                                                                                                                                                                                                       |
| `max_output_size`                      | Max bytes of JSON the handler may return.                       | `0` = off (bounded by `memory_limit`). Set it for untrusted scripts so one handler can't return a `memory_limit`-sized blob; over-cap fails `OUTPUT_TOO_LARGE` (422).                                                                         |

Sizes accept human-readable byte strings (`"32mb"`, `"1mb"`). The body limit is derived from
`max_script_size + max_context_size`.

## 4. TLS to backends

`db`, `redis`, and `amq` connect to **operator-supplied** hosts (from `config`), so they are
trusted and not SSRF-guarded — but they should still be encrypted in transit:

- `db`: `"ssl": true` (e.g. AWS RDS/Aurora, managed Postgres).
- `redis`: `rediss://…` (e.g. ElastiCache in-transit encryption).
- `amq`: `amqps://…`.

All three reuse the single process-wide `aws-lc-rs` rustls provider — no second crypto stack.
For an internal CA / self-signed cert, the platform trust store applies; mount your CA bundle
into the image's trust path. (`api`/`s3` are SSRF-guarded because their URLs are
script-controlled — see [resilience.md](design/resilience.md) on the two trust models.)

**The `api` host allowlist.** A request's `allowed_hosts` names the hosts the `api` client may
reach; the SSRF guard additionally blocks any host that resolves to a private/internal IP
(IPv4 **and** IPv6 — loopback, RFC1918, link-local incl. cloud-metadata, ULA, and private v4
smuggled via 6to4/NAT64). The wildcard `allowed_hosts: ["*"]` collapses the allowlist to that
IP filter alone, so it is **ignored unless** `allow_wildcard_hosts: true` is set (and never
honored in `debug` mode). Prefer an explicit host list; reach for the wildcard only when a
trusted caller genuinely needs open egress.

## 5. Multi-tenant fairness under k8s

The per-partition cap (Tier 5) is a **per-pod** control: under N replicas the effective ceiling
is per-pod × N and drifts with the HPA. **Global** per-tenant fairness belongs at the gateway —
the one component with the fleet-wide view (rate limiting, often Redis-backed). The reference
split: **gateway = global per-key policy** (reject over-quota before fan-out); **jsbox = per-pod
bulkhead + per-pod partition backstop** for sticky-routing / hot-key / gateway-gap cases. Pass
the key via the `X-Partition-Key` header (it wins over a `partition` body field; both are
caller-set, never script-set) and it's echoed back in `meta.partition`.

## 6. Secrets

`$sys.secrets` values are **opaque handles** inside JS — the plaintext never enters the sandbox;
a script can only ever return the `"[secret:NAME]"` placeholder, never the value (see
[docs/09-sys.md](09-sys.md)). Supply them in `config.sys`. Treat the request `config` itself as
sensitive (it carries DB passwords, SMTP creds): terminate TLS in front of jsbox, and don't log
request bodies.

**Mail relay abuse (untrusted scripts).** A handler chooses its own `to`/subject/body against the
operator's SMTP relay, so for untrusted scripts constrain it in `config.mail`: set
`allowed_recipient_domains` (a recipient whose domain is off-list is rejected before send) and
`max_sends` (per-execution cap on `mail.send`, on top of `max_recipients` per message). Together
they keep a handler from turning the relay into an open spam cannon.

## 7. Kubernetes specifics

- **Graceful shutdown.** jsbox handles `SIGTERM`/Ctrl-C and drains in-flight requests
  (`axum::serve` with graceful shutdown). Set `terminationGracePeriodSeconds` **≥ `timeout_ms`**
  so an in-flight execution can finish before the kill.
- **Probes.** Liveness and readiness → `GET /health` (returns `200 "ok"`). It's cheap and has no
  dependencies, so it reflects "the process is up," not backend health (by design — backend
  health is per-request and surfaced as retryable capability errors).
- **Autoscaling.** Scale on CPU, or on the bulkhead headroom gauge
  `jsbox_bulkhead_permits_available` (scale up as it trends toward zero). A rising
  `jsbox_overload_total` rate means you're shedding — add replicas or raise the bulkhead.
- **Image.** The release image is multi-stage → distroless/static, ~18 MB. It runs fine as
  non-root with a read-only root filesystem: the script/module registries load **once at
  startup** and nothing is written at runtime. Mount `scripts_dir` / `modules_dir` read-only
  (image layer, ConfigMap, or volume).
- **Replicas are trivially consistent.** Stateless + registries-at-startup means N replicas
  behave identically; "deploy a new script/module" = roll the image/ConfigMap and restart.

## 8. Observability

Scrape `GET /metrics` (Prometheus text, no client library). The series and suggested alerts:

| Metric                                             | Alert on                                                                             |
| -------------------------------------------------- | ------------------------------------------------------------------------------------ |
| `jsbox_executions_total{outcome}`                  | A rising `internal_error` / `timeout` / `capability_error` rate.                     |
| `jsbox_overload_total{scope}`                      | Sustained `global` shedding (under-provisioned) or `partition` shedding (a hot key). |
| `jsbox_db_breaker_trips_total`                     | Any increase = a database is flapping/down.                                          |
| `jsbox_bulkhead_permits_available` / `_total`      | Available trending toward 0 = at capacity (scale).                                   |
| `jsbox_execution_duration_seconds`                 | SLO latency objectives via `histogram_quantile` (p95/p99).                           |
| `jsbox_capability_op_duration_seconds{capability}` | Which downstream (db/api/…) is slow, not just total.                                 |

Every response also carries `meta.trace_id`, logged server-side with the raw cause — grep one
ID across the mesh for support.

## 9. Supply chain

`task supply-chain` runs cargo-audit (advisories) + cargo-deny (licenses/bans/sources) +
cargo-vet (every dependency audited or exempted). Wire it as a CI gate so a new or bumped
dependency that isn't vetted fails the build. Releases are CI-only
(`.github/workflows/release.yml`, manual `workflow_dispatch`) — don't hand-edit versions.
