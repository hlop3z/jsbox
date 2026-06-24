# runlet-core — consumer gaps & wishlist

> Notes from an external consumer of `runlet-core` (the **reactive-database-pg** project, which
> embeds the host as its T1/T2 logic plane with `default-features = false`). These are gaps and
> rough edges hit while integrating the `LogicHost` port — recorded here so jsbox knows what's
> missing. Each item says **what's there today**, **why it's a gap**, and a **proposed shape**.
> Nothing here is urgent for the `runlet` binary's own behavior; they're API/lifecycle ergonomics.
>
> First recorded: 2026-06-23.

---

## 1. No graceful shutdown / teardown API  — *✅ ADDRESSED 2026-06-23*

> **Resolved** with the minimal surface-agnostic primitive (per this item's own proposal):
> `LogicHost::shutdown()` flips the host to stop-accepting (new `run` calls return the new
> retryable `EngineError::ShuttingDown`, HTTP 503) and disposes the warm runtime pool — in-flight
> executions finish and dispose their own runtime on release, so the pool drains to empty without
> interrupting work. Paired with `LogicHost::pool_stats() -> PoolStats { size, idle, in_flight }`
> (also closes item #5) so the consumer drives its own bounded drain loop. The `runlet` binary now
> calls it after axum's graceful-shutdown drain. **Correction to the framing below:** the host holds
> **no** long-lived driver connections — every I/O capability uses a *fresh per-request connection
> torn down at request end* (the per-execution-connection model, `docs/design/pooled-capabilities.md`;
> `db.rs` spawns a connection-driver task that ends when the per-request `Client` drops). So "drivers
> live until process exit" does not hold; teardown is just stop-accepting + dispose the runtime pool.

**Today:** `LogicHost` exposes `new`, `run`, `registry`, and `bytecode_cache_stats` only — there is
**no** `shutdown()` / `close()` / `dispose()` and **no `impl Drop`**. `JsPool` drops a runtime only
when the pool is already full (`pool.rs`); otherwise the pooled QuickJS runtimes — and, with any I/O
feature enabled (`db`/`redis`/`amq`/`mongo`/`mail`/`s3`/`http`/`auth`), the driver connections and the
`db` circuit breaker the host holds — live until process exit.

**Gap:** a consumer handling `SIGTERM` (the `runlet` binary, or an embedder like reactive-database-pg)
can't cleanly:
1. stop accepting new `run()` calls,
2. let in-flight invocations finish within the wall-clock cap, then
3. dispose the runtime pool and release the I/O driver connections / flush the breaker.

Right now shutdown = "drop the process and let the OS reclaim everything." Fine for a deterministic-only
embed (no external resources), but for the binary and any I/O-feature build it means no clean drain.

**Proposed:** add `LogicHost::shutdown(&self)` (stop-accepting → drain in-flight bounded by the cap →
dispose pool + close drivers), optionally backed by `impl Drop` as a best-effort backstop. Keep it
**surface-agnostic** — signal/HTTP handling stays in `runlet` and in each embedder; the host only
exposes the teardown primitive. (This is the item the reactive-database-pg author specifically asked to
flag while building its own app-level graceful shutdown.)

## 2. `Invocation` is not evolution-safe  — *✅ ADDRESSED 2026-06-23*

> **Resolved** per this item's own proposal: `Invocation` is now `#[non_exhaustive]` with a
> constructor + builder — `Invocation::inline(source, ctx)` / `Invocation::registered(key, ctx)`,
> then chainable `.profile(p)`, `.caps(c)`, `.read_hook(h)`, `.resource(r)`,
> `.cache_namespace(ns)`, all defaulting the rest (profile `Full`, `CapabilitySet::NONE`, the
> hooks/egress `None`, global cache namespace). Additive fields are now backward-compatible —
> the change landed alongside the new `resource` egress field (`docs/design/resource-egress.md`)
> precisely so that field addition wouldn't be another silent break. **One-time migration:**
> external consumers must switch their `Invocation { … }` literals to the builder (the binary's
> own `handler.rs` is converted).

**Was:** `pub struct Invocation<'a>` (host.rs) had all-public fields, **no `#[non_exhaustive]`**, and
no builder/constructor. The only way to build one was a struct literal naming every field.

**Gap:** adding a field is a **silent breaking change** for every consumer. The recent
`cache_namespace: Option<&'a str>` addition broke reactive-database-pg's `Invocation { … }` literal —
it failed to compile until each call site added the field. (This happened on a routine upstream bump.)

**Proposed:** mark `Invocation` `#[non_exhaustive]` and offer a builder or constructor with sensible
defaults, e.g. `Invocation::inline(code, ctx).profile(p).caps(c)` defaulting `read_hook: None`,
`cache_namespace: None`. Then additive fields are backward-compatible.

## 3. `LogicHost` is a concrete type, not a trait port  — *priority: medium / design*

**Today:** `run` is an inherent method — `impl LogicHost { pub fn run(&self, Invocation) -> Result<Outcome, EngineError> }`.
CLAUDE.md calls it "the callable `LogicHost` port," but consumers must depend on the concrete struct.

**Gap:** consumers that want to depend on a *port* — to mock the host in tests, or to swap the engine
(e.g. a WASM backend) — can't abstract over it. (reactive-database-pg's own architecture rules ask its
core to depend on a `LogicHost` *port*, not on rquickjs types; it currently can't, so it wraps the
concrete type.)

**Proposed:** extract `trait LogicHost { fn run(&self, Invocation) -> Result<Outcome, EngineError>; }`
with the current struct as the impl (rename the struct, e.g. `QuickJsHost`, if needed). Additive,
low-risk; the binary keeps using the concrete impl.

## 4. Constructor async-context requirement is a runtime footgun  — *priority: low (docs)*

**Today:** `LogicHost::new` takes a `Handle` explicitly (good — the requirement is type-visible). But in
practice the host must be **built/warmed from within a tokio runtime context** so the captured `Handle`
is valid when `run` is later driven from `spawn_blocking` threads; otherwise first use can panic on
`Handle::current()` at the call site that obtains the handle.

**Proposed:** a doc line on `new` (and any pooled-construction helper) stating "construct on a tokio
runtime thread / warm before off-runtime use." Pure documentation — no code change.

## 5. Pool / liveness introspection  — *✅ ADDRESSED 2026-06-23 (with item #1)*

> **Resolved:** `LogicHost::pool_stats() -> PoolStats { size, idle, in_flight }` exposes the warm-slot
> count, currently-idle runtimes, and in-flight executions for operability gauges and drain loops.

**Today:** only `bytecode_cache_stats()` is exposed.

**Gap:** consumers exporting operability metrics can't see pool size / in-flight / saturation.

**Proposed:** a small `pool_stats()` (size, idle, in-flight) so embedders can surface gauges.

## 6. Module/script registries are filesystem-load-time-only — no runtime/DB source, no trust tiers  — *priority: high / blocks a consumer feature*

> Added 2026-06-24 while building reactive-database-pg's CL1 "logic as versioned state": logic units
> are now ordinary entities in Postgres (`sys.script`), edited live, hot-reloaded off the change feed.
> Units run fine today via `CodeRef::Inline` (the bytecode cache keys on source-hash, so a source edit
> recompiles automatically — hot-reload for free). The gap is **shared modules** (`import`).

**Today:** `ScriptRegistry::load(dir, max)` and `ModuleRegistry::load(dir, max)` build an **immutable
`HashMap<String, Arc<str>>` from a filesystem directory at startup**. There is no runtime mutation
(`new()` + `insert(specifier, source)` / `replace` / `remove`), and `RegistryResolver`/`RegistryLoader`
resolve a module `import` only against that fixed, **flat, bare-specifier** map. There is no namespacing
or trust-tier concept — every specifier lives in one global space.

**Gap:** an embedder whose **source of truth is a database, not the filesystem**, can't offer cross-unit
modules. reactive-database-pg stores reusable modules as `sys.module` entities (versioned, RLS-scoped,
operator- vs tenant-authored). For a unit to `import { quote } from "@system/pricing"`, the host needs to:
1. **Resolve a specifier against a live, mutable map sourced from the DB** — not a startup filesystem walk.
2. **Hot-reload**: when a module entity's revision changes, swap its source so the next compile uses it
   (the bytecode cache already handles the recompile once the source can be replaced — same property
   units enjoy).
3. **Trust-tier namespacing**: `@system/...` specifiers are operator-authored and cross-tenant; bare/
   tenant specifiers resolve tenant-local only; **a tenant module must not shadow or override a
   `@system` one.** Today everything is one flat namespace, so this policy can't be expressed.

Without (1)–(3), DB-sourced module imports are impossible, so reactive-database-pg has **deferred the
`sys.module` half of CL1** and shipped unit-level hot-reload only (via `CodeRef::Inline`).

**Proposed:** a **dynamic module registry the consumer owns and mutates at runtime**, that the pooled
runtimes' resolver/loader read live (behind the existing `Arc`):
- `ModuleRegistry::new()` + `insert(specifier, Arc<str>)` / `remove(specifier)` (or a `DynModuleRegistry`
  type / a `ModuleSource` trait the host calls on resolve, so the embedder can serve source from its own
  store + cache). Concurrency-safe (e.g. `arc-swap` / `RwLock`) since it changes while runtimes are warm.
- Optional **specifier namespacing hook** so the embedder can enforce tier rules at resolve time
  (classify `@system/*` vs bare, reject tenant→`@system` shadowing) — or at minimum, don't bake in the
  assumption that specifiers are a single flat space, so the consumer can prefix/segment them.
- Same-shaped runtime `insert` on `ScriptRegistry` would be nice for symmetry, but is **not** required —
  `CodeRef::Inline` already covers DB-sourced *units*; this item is specifically about *modules/imports*.

Relates to #3 (a `LogicHost`/registry *port* would let the consumer supply its own DB-backed
resolver without forking the concrete types).

## 7. Driver-backed capability types moved out of `runlet-core`  — *FYI 2026-06-24 (Step 4a)*

> Heads-up for any consumer enabling the driver features (not the deterministic-only embed).

**What changed:** the driver-backed capabilities (`db`/`mongo`/`mail`/`redis`/`amq`/`auth`) were
split out of `runlet-core` into a new **`fabric-backends`** crate, and the egress wire contract
(the `Egress` trait, `EgressError`, the error taxonomy, the circuit breaker, the metric collector)
into a new leaf **`fabric-wire`** crate. `runlet-core` now links **no** network driver even with
`full`; it keeps only the JS wrappers + the engine seam (see `docs/design/resource-egress.md`).

**Consumer impact:**
- **Deterministic-only embedders (reactive-database-pg, `default-features = false`): unaffected.**
  The blessed surface — `LogicHost`, `Invocation`, `CapabilitySet`, `Outcome`, and `Egress`/
  `EgressError` (still re-exported from `runlet_core::egress`) — is unchanged. No driver feature →
  nothing moved that you referenced.
- **Driver-feature embedders:** the per-capability types relocated. `runlet_core::db::DbConfig`/
  `DbMetric` (and the `mongo`/`mail`/`kv`/`amq`/`auth` equivalents) are now
  `fabric_backends::<cap>::*`; the in-process adapter `runlet_core::inproc::InProcessEgress` is now
  `fabric_backends::BackendSet` (`AsyncDeps` likewise). `host::ExecMetrics` now carries only the
  in-engine `http`/`s3` metrics; the driver-backed metrics are drained directly from the
  `BackendSet` (`.db_metrics()` etc.). The `runlet` binary is converted as the reference.

## 8. `LogicHost::new` signature change — **breaking, action required** *(2026-06-24, Step 5)*

> Affects **every** consumer, including the deterministic-only embed (reactive-database-pg).

**What changed:** `LogicHost::new` dropped its two vestigial parameters. The host drives no I/O
itself anymore — all driver work runs in the consumer's wired `Egress` (a `fabricd` sidecar) — so
the tokio `Handle` and the `Option<Arc<CircuitBreaker>>` it used to take are gone:

```rust
// before
LogicHost::new(pool, handle, db_breaker, registry, settings)
// after
LogicHost::new(pool, registry, settings)
```

**Action:** drop the two arguments at your `LogicHost::new` call site. Nothing else in the blessed
surface changed — `Invocation`, `CapabilitySet`, `Outcome`, `Egress`/`EgressError` (still
`runlet_core::egress`), and the `read_hook` seam are unchanged. If you wired a breaker only to pass
it here, you can delete it; resilience for driver egress now lives in `fabricd`.

**Also (Step 5, driver-feature consumers only):** the operator credential table + name→config
resolution moved out of the request/box into `fabricd`. The box sends logical names only and holds
no credentials; there is no in-process driver path. See `docs/design/resource-egress.md`.

---

*Maintainer: triage/close items here as they're addressed; this file is a consumer-feedback inbox, not
a spec. The authoritative design lives in `./docs/design/` and `./docs-sys/`.*
