# Design note: injectable modules (operator-authored JS libraries)

Status: **proposal — not implemented, planning only.** Companion to
[script-registry.md](script-registry.md) and
[pooled-capabilities.md](pooled-capabilities.md).
Revision 2: the loader-function shape (`use("name")`) replaced auto-injected
globals as the proposed Phase A — see "Loader API" below for why.

## Idea

Let internal developers author reusable JS libraries ("modules") that customer
scripts can use inside the sandbox: validation helpers, a pricing engine, a
company-SDK wrapper around `db`/`api`, formatting utilities. Customers write less
boilerplate; internal teams ship tools once instead of pasting snippets into every
handler.

## The tool trichotomy (decide which thing you're building first)

jsbox would then have three ways to ship functionality to scripts, and they are
**not interchangeable** — picking the wrong one is the root of most failures below:

| You need…                                      | Build a…              | Why                                                                                                       |
| ---------------------------------------------- | --------------------- | --------------------------------------------------------------------------------------------------------- |
| I/O, secrets, metering, real security boundary | **Rust capability**   | Only Rust-side code is outside the sandbox. JS modules can't hold secrets or enforce anything.            |
| Logic the customer must NOT read or alter      | **Registered script** | Customer calls it by `key` and never sees source. Module source IS readable by customer code (see below). |
| In-script helpers the customer composes with   | **Injectable module** | Plain JS in the same sandbox — convenient, composable, and exactly as trusted as the customer's own code. |

The hard truth that anchors this table: **a JS module injected into the customer's
context is readable and patchable by the customer's script.**
`Function.prototype.toString()` returns module source; assignment replaces module
globals. Fresh-context-per-request means tampering only ever affects the tamperer's
own execution (nobody else's), but it means modules provide **zero confidentiality
and zero integrity** against the script they share a context with. Any plan that
quietly assumes otherwise (license enforcement in a module, "hidden" business rules,
input sanitization the platform relies on) is broken on day one. That logic belongs
in a capability or a registered script.

## How it fits the current architecture

Mechanically this is the cheapest feature jsbox could add, because the injection
seam already exists and is exercised four times per request:

- `engine.rs::run` already evals operator-controlled JS into the fresh context
  before the user script: `bridge.js`, the Decimal global, `$sys`, and each
  capability wrapper (`src/js/*.js` via `include_str!`). An injectable module is
  _literally the same mechanism_ with the source coming from a registry instead of
  the binary.
- The registry pattern from Phase A reuses verbatim: a `modules_dir` loaded once at
  startup into an immutable map, keyed by relative path. Stateless, replica-safe,
  deploy-time registration.
- Fresh context per request keeps isolation airtight: module state cannot leak
  between requests, and a script that breaks a module breaks only its own run.
- Sandbox limits apply unchanged: module code runs under the same memory cap,
  timeout, `max_ops`, and stack limit. Nothing new to meter.

## Lessons from bigger platforms (what failed, and the rule we adopt)

| Platform attempt                             | What went wrong                                                                                                                                                                                               | Rule for jsbox                                                                                                                                                                                                                                                                                                 |
| -------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **AWS Lambda Layers**                        | Opaque integer versioning, no dependency resolution, 5-layer cap, code invisible while debugging, cross-account sharing friction. The ecosystem largely abandoned layers for bundling code with the function. | Modules are **immutable deploy-time artifacts**, and composition is **explicit and flat**. If a script needs a module, it names it; nothing is ambient. Prefer "bundle at deploy" over "resolve at runtime".                                                                                                   |
| **Cloudflare Workers service-worker format** | Started with "everything is a global in one scope"; migrating the ecosystem to ES modules took years and a compatibility-flag regime they still carry.                                                        | Whatever shape we pick is a **format commitment**. Keep it minimal (one file evaluates to its export; loaded via `use()`) so a future ESM migration maps 1:1 (`use("x")` → dynamic `import()`), and never invent a bespoke module _format_ (`module.exports` objects, custom resolution) we'd support forever. |
| **Salesforce managed packages / Apex**       | Global namespace pollution, implicit cross-package dependencies ("Happy Soup"), version lock-in; the packaging redesign (1GP→2GP) took a decade.                                                              | **Reserved-name validation at load** (a module may not shadow `db`, `api`, `$`, `json`, `handler`, …) and **no inter-module dependencies in v1**. Flat list, alphabetical injection, no resolution algorithm.                                                                                                  |
| **npm / left-pad**                           | Mutable shared registry: removing or changing a tiny package broke the world. Semver ranges made builds non-reproducible.                                                                                     | **No runtime mutation, no version ranges.** A module's content changes only via redeploy + restart, atomically with everything else. If versioning is ever needed, it's an explicit name suffix (`pricing-v2`), not a resolver.                                                                                |
| **OSGi / Java Jigsaw**                       | Dynamic load/unload and classloader graphs were so complex the industry routed around them; Jigsaw took 20 years and still broke things.                                                                      | **Static, load-time composition only.** No hot-reload, no module lifecycle, no unloading. Restart is the reload mechanism — jsbox restarts in milliseconds.                                                                                                                                                    |
| **Google App Engine restricted runtimes**    | Forked/whitelisted stdlibs made code non-portable and untestable outside the platform; eventually scrapped for standard runtimes.                                                                             | Modules are **plain QuickJS-compatible JS with no jsbox-specific magic**, so internal devs can unit-test them with any JS runner. The only contract: define your global, touch nothing else.                                                                                                                   |
| **Shopify Scripts (embedded Ruby)**          | Shared-library scripting inside the platform runtime became unsandboxable and unmaintainable; deprecated in favor of WASM functions with a strict data-in/data-out boundary.                                  | Keep the **boundary discipline**: modules never get powers scripts don't have. Anything needing real authority goes through the Rust FFI like every capability.                                                                                                                                                |

The meta-lesson across all seven: platforms get into trouble when shared code
acquires **its own lifecycle** (versioning, resolution, mutation, trust) separate
from the deployment lifecycle. Phase A of the script registry deliberately has no
such lifecycle — modules must inherit that property, not erode it.

## Loader API: `use("name")` — the proposed shape (Phase A, when/if implemented)

Scripts pull modules in explicitly, instead of the platform injecting globals:

```js
function handler(ctx) {
  var pricing = use("acme/pricing"); // script author picks the binding name
  return json(pricing.quote(ctx.items), null);
}
```

Why a loader beats auto-injected globals (the earlier revision of this doc):

- **Namespacing is solved by construction.** The script names its own binding, so
  global-name collisions — the Salesforce-soup failure mode — can't happen. The
  reserved-name machinery mostly evaporates (only `use` itself joins the built-ins).
- **The dependency declaration lives in the code.** `config.modules` on every
  request and a `// @modules` directive for registered scripts both disappear —
  callers don't need to know what a script uses.
- **Lazy, pay-per-use.** A module evals only if and when requested; `meta.modules`
  reports what was _actually_ loaded (better audit than a config list).
- **Mechanically trivial because jsbox is synchronous.** `use` is a native function
  (the same `Function::new` pattern as `__db`) holding an `Arc<ModuleRegistry>`; on
  first call it evals the module source in the current context and memoizes the
  result per request. No async-loading problem.

### Semantics to commit to

1. **Export contract = completion value.** QuickJS `eval` returns the script's
   completion value, so a module file is plain code whose final expression is its
   export — Lua's `require` semantics, battle-tested for decades. No `module.exports`
   object, no bespoke format:
   ```js
   // modules/acme/pricing.js
   function quote(items) {
     /* ... */
   }
   ({ quote: quote }); // ← the export
   ```
2. **Memoized per request.** Two `use("x")` calls in one execution return the same
   instance; a fresh context next request gets a fresh instance. State never leaks.
3. **Transitive `use` allowed, flat tree.** Modules may call `use` (forbidding it
   would surprise more than it protects; the Salesforce/OSGi pain was _versioned
   package graphs_, not function calls within one operator-owned tree). Guardrails:
   memoization handles diamonds, cycle detection throws, small depth cap (~8).
4. **Unknown module → structured runtime error** (`MODULE_NOT_FOUND`, owner:
   developer, not retryable) via the existing tagged-error path. Dynamic names
   (`use(ctx.x)`) are legal but discouraged; for registered scripts the registry
   additionally best-effort scans `use("literal")` calls at load as a fail-fast
   lint — a warning, not a guarantee.
5. **`modules_dir`** in `config.json`; immutable `ModuleRegistry` loaded at startup
   (same code shape as `ScriptRegistry`): size cap per file plus a **scratch-eval
   syntax check** in a throwaway runtime so a broken module fails the boot, not a
   customer request.
6. **Meta:** `meta.modules` lists the keys actually loaded during the execution.
7. **`use` survives `sanitize_globals`** (it is not `eval` — it can only reach the
   immutable registry), and module code runs under the same memory/timeout/stack/
   `max_ops` limits as everything else in the context.

One consciously accepted tension: the lessons table says "no bespoke `require()`".
The refined rule is **no bespoke module _format_** — `use` keeps the format minimal
(a file evaluates to its export) and adopts well-trodden CJS/Lua _loader_ semantics.
Real ESM would change jsbox's whole eval model (rquickjs `Module` API, different
lifecycle) and stays out of scope; `use("x")` maps 1:1 onto dynamic `import()` if
that migration ever happens.

Everything else — `pool.rs`, capabilities, error taxonomy, both existing registries —
untouched. Estimated size is similar to script-registry Phase A plus the memoization/
cycle-detection logic (~20 lines).

### Explicitly out of scope (the "won't do" list)

- Runtime module registration/mutation (same reasoning as script-registry Phase B).
- Version ranges or any resolution algorithm (transitive `use` is allowed, but a
  name maps to exactly one file — no versions, no fallbacks, no search paths).
- ES modules / `import` syntax — `use()` is the only loader; ESM would change the
  whole eval model and is deferred until something forces it.
- `Object.freeze` on module globals: considered and **deferred** — it adds a false
  sense of integrity (deep-freeze is leaky: prototypes, closures) while breaking
  legitimate test-stubbing patterns. Revisit only with a concrete threat it solves.
- Per-module metering/billing — module code is indistinguishable from script code
  inside the sandbox; metering it separately is not honestly possible.
- Bytecode caching of modules — same Phase B+ seam as registered scripts; modules
  are actually the _better_ candidate (stable, shared), so do them together.

## Performance note

Per-request cost is one extra `eval` per requested module in the fresh context —
roughly the same class as the capability wrappers already eval'd today (tens of µs
per KB-scale file). A 100 KB SDK costs ~low-single-digit ms per request; fine at
moderate volume, and the bytecode-cache evolution erases it if it ever matters.
Memory: module objects live in the per-request heap, under `memory_limit` — a fat
module eats the customer's own budget, which is the correct incentive.

## Intended consumer profile (answered 2026-06)

The target is **helper/utility tools extracted from observed repetition** in
customer scripts — e.g. a SQL query builder on top of `db`, validation helpers,
pagination/response shaping. Explicitly _not_ critical logic (that stays in Rust
capabilities). This is the low-risk variant of the feature: no secrecy or integrity
requirements, small files, negligible eval cost. Two working rules follow:

- **Extract, don't speculate.** A module is born when the same pattern shows up in
  ~3 real scripts, not from an up-front SDK design nobody's code matches.
- **Helpers must make the safe path the only path.** The SQL builder is the
  canonical example: it must always emit `{text, params}` pairs (parameterized
  `$1, $2` placeholders) with **no raw-string interpolation escape hatch** —
  a convenience helper that makes string-built SQL easy would actively encourage
  injection in customer scripts, which is the one way a "non-critical" helper
  causes critical damage.

## Remaining decision input before building

1. Do customers ever supply their _own_ modules? (Today's answer should be no —
   that's just inline script code they can paste/bundle themselves. A customer
   module registry would be a different, bigger feature.)
2. Timing: the mechanism is cheap (~script-registry-sized), but per "extract,
   don't speculate", build it when the first 2–3 concrete helpers exist as proven
   snippets — not before.
