# Authoring injectable modules

Injectable modules are operator-authored JS libraries a handler can `import` — validation
helpers, a query builder, formatting utilities. This is the how-to for writing and deploying
them. The design rationale (trust model, lessons from other platforms) lives in
[design/injectable-modules.md](design/injectable-modules.md).

## The shape

A module is a native **ES module**: it `export`s the things a handler imports.

```js
// modules/acme/pricing.mjs
export function quote(items, unit) {
  return items * unit;
}

export const TAX_RATE = 0.1;

export function withTax(amount) {
  return amount + amount * TAX_RATE;
}
```

Drop the file under `modules_dir` (set in `config.json`). It's loaded **once at startup**, with
a specifier that is its path relative to `modules_dir` without the extension —
`modules/acme/pricing.mjs` → `acme/pricing`. A handler imports by that specifier:

```js
import { quote, withTax } from "acme/pricing";

export default function handler(ctx) {
  return json(withTax(quote(ctx.items, ctx.unit)), null);
}
```

Both `export default function handler` and `export function handler` are accepted. A classic
`function handler(ctx) { … }` script still works unchanged — module mode is auto-detected by a
top-level `export`.

## Authoring with npm / TypeScript

You don't have to write raw `.mjs` by hand. Write a normal npm/TypeScript package and **bundle
it to a single self-contained ES module** — esbuild is the simplest path:

```bash
# one module file out, all npm deps inlined, no bare imports left to resolve at runtime
esbuild src/pricing.ts \
  --bundle \
  --format=esm \
  --platform=neutral \
  --packages=bundle \
  --outfile=modules/acme/pricing.mjs
```

Then `cp` / mount `modules/` as `modules_dir`. Notes:

- `--format=esm` is the format jsbox loads. `--bundle --packages=bundle` inlines your npm
  dependencies into the one file, so there are **no bare specifiers** (`import "lodash"`) left
  for the runtime to resolve — only imports of _other jsbox modules_ resolve at runtime, and
  only if they're registered.
- `--platform=neutral` keeps Node built-ins (`fs`, `net`, …) out — they don't exist in the
  sandbox anyway. If your code imports them, that's a sign the logic belongs in a Rust
  capability, not a module.
- Unit-test modules with any JS runner — they're plain QuickJS-compatible ES modules with no
  jsbox-specific magic. The only contract is "export what the handler imports."

## What runs where (pick the right tool)

A module is **plain JS in the same sandbox as the handler** — it is exactly as trusted as the
customer's own code, and a customer script can read or patch it (`Function.prototype.toString`).
So:

- I/O, secrets, a real security boundary → a **Rust capability** (`db`, `api`, …). Only Rust is
  outside the sandbox.
- Logic the customer must not read or alter → a **registered script** (called by `key`).
- In-script helpers the customer composes with → an **injectable module**.

Never put license checks, hidden business rules, or platform-relied-on sanitization in a module —
they provide zero confidentiality and zero integrity against the script sharing their context.

## Two rules for good helpers

- **Extract, don't speculate.** A module is worth creating when the same pattern shows up in ~3
  real handlers — not from an up-front SDK design nobody's code matches yet.
- **Make the safe path the only path.** The canonical example is a SQL builder: it must always
  emit parameterized `{text, params}` pairs and offer **no raw-string interpolation escape
  hatch**. A helper that makes string-built SQL easy would actively encourage injection in
  customer code — the one way a "non-critical" helper causes critical damage.

```js
// modules/sql/where.mjs — safe-by-construction: only ever emits $1,$2 placeholders
export function eqAll(filters) {
  const keys = Object.keys(filters);
  const text = keys.map((k, i) => `${k} = $${i + 1}`).join(" AND ");
  const params = keys.map((k) => filters[k]);
  return { text: text || "TRUE", params };
}
```

```js
import { eqAll } from "sql/where";
export default function handler(ctx) {
  const w = eqAll({ status: ctx.status, owner: ctx.owner });
  const r = db.query(`SELECT * FROM orders WHERE ${w.text}`, w.params);
  return json(r.rows, null);
}
```

## Errors & limits

- Importing an unregistered specifier (typo, `../`, `/etc/…`) fails with `MODULE_NOT_FOUND`
  (owner: developer, not retryable) — resolution is a pure in-memory lookup, so a script can
  reach **only** registered modules, never the filesystem.
- Modules run under the same sandbox budget as the handler — `memory_limit`, `timeout_ms`,
  `max_ops`, stack limit. A fat module eats the request's own budget.
- The registry is read-only at runtime. Changing a module = redeploy the file (image layer,
  ConfigMap, volume) + restart, so every replica stays consistent.

## Performance

A handler authored as a module adds **~39 µs/request** over a classic script (negligible); a
handler that `import`s one small module adds **~201 µs/request** (compile + resolve + the
imported module's own eval), against a ~2.5 ms request floor — see
[design/injectable-modules.md](design/injectable-modules.md). Module **bytecode caching** would
shave the import cost, but it's **not available**: rquickjs's bytecode load is an `unsafe` API
and jsbox forbids `unsafe` entirely. The per-import compile is therefore the accepted floor;
keep imported modules small and few, and the cost stays in the noise.
