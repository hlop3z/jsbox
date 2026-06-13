# Registered scripts (execute by key)

> **Behavioral contract → [`openspec/specs/script-registry`](../../openspec/specs/script-registry/spec.md).**
> This note is the **rationale** — why deploy-time registration, the in-memory/no-traversal
> safety property, and what a future registration phase would add.

## Overview

`POST /execute` accepts **exactly one** of two source forms:

```jsonc
// 1. Execution by registered key
{ "key": "acme/billing/pricing", "context": { ... }, "config": { ... } }

// 2. Execution by inline script
{ "script": "function handler(ctx) { ... }", "context": { ... }, "config": { ... } }
```

Both forms support persistent registered scripts and disposable one-off scripts through
the same endpoint, with identical execution semantics and the same `{data, error, meta}`
envelope.

## Why it fits

- The engine boundary is clean: `engine::run` takes `script: &str` (`src/engine.rs`) and
  does not care where the bytes came from. A resolved script is indistinguishable from an
  inline one.
- jsbox is stateless, and the design keeps it that way. Script registration does not turn
  jsbox into a stateful service.
- A script key provides an identity hook for adjacent features: per-script metrics
  aggregation, versioning and audit, bytecode caching, and config bound to a script.

## Read-only file registry

Scripts are deploy-time artifacts: a directory of `.js` files loaded **once at startup**
into an immutable in-memory map. Registration means deploying files (an image layer, a
ConfigMap, a mounted volume). There is no write API, no external store, and no runtime
invalidation.

The registry sits alongside `pool.rs`, `engine.rs`, the capabilities, metrics, and error
classification without touching them. The components involved are:

1. **`handler.rs`** — `script: Option<String>` and `key: Option<String>`; exactly one must
   be present, otherwise a hard `400`. There is no silent precedence, which would be a
   debugging trap. An unknown key resolves to the error code `SCRIPT_NOT_FOUND`.
2. **`registry.rs`** — loads `scripts_dir` recursively at startup into
   `HashMap<String, Arc<str>>`. The key is the relative path without extension
   (`acme/billing/pricing.js` → `acme/billing/pricing`). Each file is validated against
   `max_script_size` at load time, so a violation is a startup error rather than a runtime
   surprise. The map is shared via app state next to `JsPool`.
3. **`config.rs`** — one optional `scripts_dir` field. When absent, the registry is empty,
   `key` requests fail with `SCRIPT_NOT_FOUND`, and inline mode is unaffected.
4. **`meta`** — `script_bytes` reports the _resolved_ script, and the `key` is echoed back.

Because the key maps to a relative path resolved against an in-memory map populated only at
startup, there is no filesystem traversal at request time and no path-injection surface: a
key that was not loaded simply does not exist.

This form preserves the core properties. jsbox stays stateless, so N replicas are trivially
consistent — they all load the same files. Config remains per-request, a fresh `Context` is
created per request, and both source forms execute through the identical engine path.
Existing callers that send `script` are unaffected.

## Invariants

- **Fresh `Context` per request.** A pre-evaluated context is never kept per key: it would
  leak global state between requests, and capability injection is driven by _per-request_
  config (`engine.rs::inject_apis` runs before eval, per request).
- **Mutual exclusivity of `script` and `key` is a hard 400.**
- The registry is read-only at runtime. Mutability arrives only together with a durable
  store.

## Out of scope and future work

Runtime mutation of the registry is deliberately excluded. The registry resolves through a
single `resolve(key) -> Arc<str>` seam, so the following evolutions can plug into that seam
when a concrete consumer requires them.

| Evolution                                            | What it adds                                        | Why out of scope today                                                                                                                                                                                                                                                                                            |
| ---------------------------------------------------- | --------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Admin write API (`PUT /scripts/{key}`)               | Runtime registration                                | Breaks multi-replica consistency with an in-memory store and forces an authn/authz story jsbox does not have. It belongs only on top of a durable store.                                                                                                                                                          |
| External store (Postgres/Redis) + read-through cache | Durable runtime registration                        | Adds a hard infra dependency and cache-invalidation machinery. Plugs into the same `resolve(key) -> Arc<str>` seam.                                                                                                                                                                                               |
| Per-key bytecode cache                               | Skip QuickJS parse per request                      | The saving is parse time only (tens of µs for KB-scale scripts, small next to capability I/O), and the rquickjs bytecode path is module-oriented while jsbox evals classic scripts. The seam exists once keys exist. Inline scripts are **never** cached by content hash (an unbounded, caller-influenced cache). |
| Config bound to a key                                | Registered script carries its own capability config | Drags in secret storage. Config stays per-request in both modes.                                                                                                                                                                                                                                                  |

When scripts change only at deploy time, by the operator, the read-only file registry is
not merely the minimal implementation — it is the correct final shape, and the evolutions
above may never be needed.
