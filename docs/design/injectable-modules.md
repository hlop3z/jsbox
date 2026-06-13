# Injectable modules (operator-authored JS libraries)

Companion to [script-registry.md](script-registry.md) and
[pooled-capabilities.md](pooled-capabilities.md).

> **Behavioral contract → [`openspec/specs/module-registry`](../../openspec/specs/module-registry/spec.md)**
> (and [`execution`](../../openspec/specs/execution/spec.md) for handler-as-module). This note carries the
> **rationale** — the trust model, the platform-lessons principles, and the ESM design.

## Purpose

Injectable modules let internal developers author reusable JS libraries that
customer scripts compose with inside the sandbox: validation helpers, a pricing
engine, a company-SDK wrapper around `db`/`api`, formatting utilities. Customers
write less boilerplate; internal teams ship a tool once instead of pasting snippets
into every handler.

## The tool trichotomy

jsbox has three ways to ship functionality to scripts, and they are **not
interchangeable** — picking the wrong one is the root of most failure modes other
platforms have hit:

| You need…                                        | Build a…              | Why                                                                                                   |
| ------------------------------------------------ | --------------------- | ----------------------------------------------------------------------------------------------------- |
| I/O, secrets, metering, a real security boundary | **Rust capability**   | Only Rust-side code lives outside the sandbox. JS modules cannot hold secrets or enforce anything.    |
| Logic the customer must NOT read or alter        | **Registered script** | The customer calls it by `key` and never sees source. Module source IS readable by customer code.     |
| In-script helpers the customer composes with     | **Injectable module** | Plain JS in the same sandbox — convenient, composable, exactly as trusted as the customer's own code. |

## The trust property (read this first)

A JS module injected into the customer's context is **readable and patchable by the
customer's script.** `Function.prototype.toString()` returns module source;
assignment replaces module globals. Fresh-context-per-request means tampering only
ever affects the tamperer's own execution, but a module therefore provides **zero
confidentiality and zero integrity** against the script it shares a context with.

This makes injectable modules suitable for _helpers_ (a SQL builder, validators,
formatters) and unsuitable for _anything load-bearing_. License enforcement, hidden
business rules, and input sanitization the platform relies on do not belong in a
module — they belong in a Rust capability or a registered script. This rule is
enforced in review, not in code, because it cannot be enforced in code.

## How it fits the architecture

The injection seam already exists and is exercised on every request. `engine.rs::run`
evals operator-controlled JS into the fresh context before the user script:
`bridge.js`, the Decimal global, `$sys`, and each capability wrapper (`src/js/*.js`
via `include_str!`). An injectable module is the same mechanism with the source coming
from a registry instead of the binary.

- The registry pattern reuses the `ScriptRegistry` shape verbatim: a `modules_dir`
  loaded once at startup into an immutable map keyed by relative path. Stateless,
  replica-safe, deploy-time registration.
- Fresh context per request keeps isolation airtight: module state cannot leak between
  requests, and a script that breaks a module breaks only its own run.
- Sandbox limits apply unchanged: module code runs under the same memory cap, timeout,
  `max_ops`, and stack limit. Nothing new to meter.

## Design principles (distilled from other platforms)

Larger platforms got into trouble when shared code acquired **its own lifecycle**
(versioning, resolution, mutation, trust) separate from the deployment lifecycle.
jsbox's script registry deliberately has no such lifecycle, and modules inherit that
property. The specific rules:

- **Immutable deploy-time artifacts; explicit, flat composition.** A script that needs
  a module names it; nothing is ambient. (AWS Lambda Layers' opaque integer versioning,
  5-layer cap, and absent dependency resolution drove the ecosystem back to bundling code
  with the function.)

- **The module format is a permanent commitment, so it is minimal and standard.** Native
  ESM is the format; no bespoke `module.exports` objects or custom resolution that would
  have to be supported forever. (Cloudflare Workers' service-worker "everything is a global"
  format cost years of migration and a compatibility-flag regime they still carry.)

- **Reserved-name validation at load; no inter-module dependency graph.** A module may not
  shadow `db`, `api`, `$`, `json`, `handler`, and similar globals. (Salesforce's global
  namespace pollution and implicit cross-package dependencies — "Happy Soup" — produced a
  decade-long 1GP→2GP repackaging.)

- **No runtime mutation, no version ranges.** A module's content changes only via redeploy
  and restart, atomically with everything else. If versioning is ever needed it is an
  explicit name suffix (`pricing-v2`), not a resolver. (npm's mutable shared registry and
  semver ranges produced left-pad and non-reproducible builds.)

- **Static, load-time composition only.** No hot-reload, no module lifecycle, no unloading.
  Restart is the reload mechanism — jsbox restarts in milliseconds. (OSGi/Jigsaw's dynamic
  load/unload and classloader graphs were so complex the industry routed around them.)

- **Plain QuickJS-compatible JS with no jsbox-specific magic.** Internal developers can
  unit-test modules with any JS runner; the only contract is to define the export and touch
  nothing else. (Google App Engine's forked/whitelisted stdlibs made code non-portable and
  untestable, and were scrapped for standard runtimes.)

- **Modules never get powers scripts don't have.** Anything needing real authority goes
  through the Rust FFI like every capability. (Shopify Scripts' shared-library Ruby inside
  the platform runtime became unsandboxable and was replaced by WASM functions with a strict
  data-in/data-out boundary.)

## Implementation (as built — native ESM)

QuickJS is a native ES module engine and rquickjs exposes the whole surface, so modules
are real ESM rather than a bespoke loader format.

- **`modules_dir`** (`config.json`) loads `*.js` / `*.mjs` into an immutable
  `ModuleRegistry` (`src/modules.rs`); the specifier is the relative path without
  extension — the exact `ScriptRegistry` shape, including in-memory lookup and
  no-traversal safety.
- **`RegistryResolver` + `RegistryLoader`** (`src/modules.rs`) wire that registry into
  each pooled `Runtime` via `Runtime::set_loader` (the `loader` feature). Resolution is a
  pure `HashMap` hit: a bare specifier resolves **iff** registered; `../`, `/etc/…`, and
  unknown names fail. A script reaches only registered modules, never the filesystem.
- **Handler-as-module** (`src/engine.rs`): `resolve_handler` detects a top-level `export`
  (`is_es_module`), and in module mode `Module::declare(...).eval()` + `Promise::finish()`
  settles synchronously, then the handler is read from the namespace (`default`, else
  `handler`). Classic `function handler(ctx)` scripts keep running unchanged (script mode).
- **`MODULE_NOT_FOUND`:** an unresolvable `import` (unknown specifier, `../`, `/etc/…`) is
  classified as a dedicated `MODULE_NOT_FOUND` error (owner: developer, not retryable, 422),
  not a generic syntax error. The resolver/loader embed a sentinel
  (`modules::UNRESOLVED_MARKER`) in the thrown exception that the engine matches
  structurally, so the classification does not depend on rquickjs's wording.
- **Synchronous settle.** Module evaluation returns a `Promise` (QuickJS allows top-level
  await), but `Promise::finish()` pumps the job queue to completion synchronously. Every
  jsbox capability is sync FFI, so a module never genuinely suspends — it settles on the
  first pump. No async runtime is introduced; this fits the `spawn_blocking` model exactly,
  and the existing wall-clock interrupt still bounds it.
- **Sandbox budget and read-only registry** apply unchanged: module code runs under the
  same memory, timeout, `max_ops`, and stack limits as the handler, and the registry is
  immutable after startup.

### Bytecode caching is blocked by `unsafe_code = "forbid"`

`Module::write(WriteOptions)` to produce bytecode is safe, but reading it back is
`unsafe Module::load(bytes)`. jsbox sets `unsafe_code = "forbid"`, so precompiling
modules at startup to skip per-request parsing is not available without relaxing the
unsafe ban. The measured saving (~201 µs/import, see Performance) does not justify that
cost, so per-request compilation is the accepted floor.

## Alternative considered: `use()` loader (superseded)

An earlier design proposed a synchronous `use("acme/pricing")` loader function — the
script names its own binding — with completion-value export semantics, per-request
memoization, transitive `use` with cycle detection, and a `meta.modules` audit. Native
ESM `import` made it redundant: a handler-module imports directly, the format is the
standard, and the `MODULE_NOT_FOUND`, sandbox-budget, and read-only-registry properties
all carry over. Prior text is in git history.

## Out of scope

- Runtime module registration or mutation (same reasoning as script-registry Phase B).
- Version ranges or any resolution algorithm — a name maps to exactly one file; no
  versions, no fallbacks, no search paths.
- `Object.freeze` on module globals: deferred. It adds a false sense of integrity
  (deep-freeze is leaky through prototypes and closures) while breaking legitimate
  test-stubbing patterns. Revisit only with a concrete threat it solves.
- Per-module metering or billing — module code is indistinguishable from script code
  inside the sandbox, so metering it separately is not honestly possible.
- Bytecode caching of modules — blocked by the `unsafe_code = "forbid"` lint, for a
  measured saving not worth relaxing the unsafe ban (see above).

## Performance

Per-request cost is one extra module instantiation per requested module in the fresh
context — the same class as the capability wrappers already eval'd today. Module objects
live in the per-request heap under `memory_limit`, so a fat module eats the customer's own
budget, which is the correct incentive.

Measured (`stress_breaker_esm.py`, sequential, tiny handlers, ~2.5 ms request floor):
an export-default handler adds ~39 µs/request over a classic script (module vs script
compile — negligible); a handler importing one small registry module adds ~201 µs/request
(compile + resolve + the imported module's own compile and eval). Both are well under 10 %
of the per-request floor. Keep imported modules small and few.

## Intended consumer profile

The target is **helper/utility tools extracted from observed repetition** in customer
scripts — a SQL query builder on top of `db`, validation helpers, pagination and response
shaping. Critical logic stays in Rust capabilities. This is the low-risk variant of the
feature: no secrecy or integrity requirements, small files, negligible eval cost. Two
authoring rules follow:

- **Extract, don't speculate.** A module is born when the same pattern appears in ~3 real
  scripts, not from an up-front SDK design that no code matches.
- **Helpers must make the safe path the only path.** The SQL builder is the canonical
  example: it always emits `{text, params}` pairs (parameterized `$1, $2` placeholders)
  with no raw-string interpolation escape hatch. A convenience helper that made string-built
  SQL easy would actively encourage injection in customer scripts — the one way a
  "non-critical" helper causes critical damage.

Customers do not supply their own modules. Customer-authored code is just inline script
they can paste or bundle themselves; a customer module registry would be a separate, larger
feature with a real trust boundary.
