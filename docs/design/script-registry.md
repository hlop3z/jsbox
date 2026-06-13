# Design note: registered scripts (execute by key)

Status: **Phase A implemented** (2026-06): `src/registry.rs`, `scripts_dir` in
`config.json`, `script` XOR `key` in the handler, `meta.key` echo. Phase B+ remains
deferred as described below.

> **Behavioral contract → [`openspec/specs/script-registry`](../../openspec/specs/script-registry/spec.md).**
> This note is the **rationale** — why deploy-time registration, the in-memory/no-traversal
> safety, and what Phase B+ would add (the "why" and the road not taken).

## Idea

`POST /execute` accepts **either** of:

```jsonc
// 1. Execution by registered key
{ "key": "acme/billing/pricing", "context": { ... }, "config": { ... } }

// 2. Execution by inline script (current behavior, unchanged)
{ "script": "function handler(ctx) { ... }", "context": { ... }, "config": { ... } }
```

Goal: support persistent registered scripts _and_ disposable one-off scripts through
the same endpoint, with identical execution semantics and the same `{data, error, meta}`
envelope.

## Why it fits

- The engine boundary is already clean: `engine::run` takes `script: &str`
  (`src/engine.rs`) and does not care where the bytes came from. A resolved script is
  indistinguishable from an inline one.
- jsbox is stateless; the only design constraint is to **keep it that way**. Script
  registration must not quietly turn jsbox into a stateful service.
- A script key creates an identity hook that future features need anyway: per-script
  metrics aggregation, versioning/audit, bytecode caching, config bound to a script.

## Incremental plan

### Phase A — read-only file registry (do this first; smallest possible change)

Scripts are deploy-time artifacts: a directory of `.js` files loaded **once at
startup** into an immutable in-memory map. Registration = deploying files (image
layer, ConfigMap, mounted volume). No write API, no external store, no invalidation.

Changes (everything else untouched — `pool.rs`, `engine.rs`, all capabilities,
metrics, error classification):

1. **`handler.rs`** — `script: Option<String>` + `key: Option<String>`; exactly one
   must be present, otherwise a hard `400` (no silent precedence — that is a
   debugging trap). Unknown key → new error code `SCRIPT_NOT_FOUND`.
2. **New `registry.rs`** — load `scripts_dir` recursively at startup into
   `HashMap<String, Arc<str>>`; key = relative path without extension
   (`acme/billing/pricing.js` → `acme/billing/pricing`). Validate each file against
   `max_script_size` at load (startup error, not runtime surprise). Shared via app
   state next to `JsPool`.
3. **`config.rs`** — one optional `scripts_dir` field. Absent → registry empty →
   `key` requests fail with `SCRIPT_NOT_FOUND`; inline mode unaffected.
4. **`meta`** — `script_bytes` reports the _resolved_ script; echo the `key` back.

Properties preserved: stateless (N replicas trivially consistent — they all load the
same files), per-request config, fresh `Context` per request, both modes execute
through the identical engine path.

Estimated size: ~150–250 lines + tests. Backward compatible — existing callers send
`script` and notice nothing.

### Phase B+ — deferred evolutions (each waits for a concrete consumer)

| Evolution                                            | What it adds                                        | Why deferred                                                                                                                                                                                                                                                                                   |
| ---------------------------------------------------- | --------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Admin write API (`PUT /scripts/{key}`)               | Runtime registration                                | Breaks multi-replica consistency with an in-memory store; forces an authn/authz story jsbox doesn't have. The trap option — avoid until there is a durable store underneath.                                                                                                                   |
| External store (Postgres/Redis) + read-through cache | Durable runtime registration                        | Adds a hard infra dependency + cache-invalidation machinery. Plugs into the same `resolve(key) -> Arc<str>` seam Phase A creates.                                                                                                                                                              |
| Per-key bytecode cache                               | Skip QuickJS parse per request                      | Saving is parse time only (tens of µs for KB-scale scripts — small next to capability I/O); rquickjs bytecode path is module-oriented while jsbox evals classic scripts. The seam exists once keys exist. **Never** cache inline scripts by content hash (unbounded, caller-influenced cache). |
| Config bound to a key                                | Registered script carries its own capability config | Drags in secret storage. Today config stays per-request in both modes.                                                                                                                                                                                                                         |

### Invariants that must not change in any phase

- **Fresh `Context` per request.** Never keep a pre-evaluated context per key: it
  leaks global state between requests, and capability injection is driven by
  _per-request_ config anyway (`engine.rs::inject_apis` runs before eval, per request).
- **Mutual exclusivity of `script` / `key` is a hard 400.**
- The registry is read-only at runtime in Phase A; mutability arrives only together
  with a durable store.

## Decision input still needed

Who registers scripts and how often do they change? If the answer is "at deploy time,
by the operator", Phase A is not just the minimal implementation — it is the correct
final shape, and Phase B+ may never be needed.
