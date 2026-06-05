# Ideas: `virtual-host` — a programmatic container for cloud-native resources

> Status: brainstorm / design sketch. No code here. The goal is to find the
> **lightest** set of primitives that turn a jsbox execution from "run this JS"
> into "run this JS _as a small serverless function with bound resources_" —
> without dragging cloud SDKs, a second crypto stack, or a runtime into the binary.

## Naming conventions (applied)

The JS API uses **snake_case** for methods, options, and result fields — matching the
config keys (`allowed_hosts`, `max_upload_size`) and giving a Python-ish feel — and prefers
**plain, short verbs** over jargon. Rules of thumb:

- **snake_case everywhere.** No camelCase — `upload_url` / `max_bytes`, not `presignPut` /
  `maxBytes`.
- **Plain words over jargon.** `upload_url` beats `presignPut`.
- **Keep the known short idiom.** Don't lengthen `get` / `set` / `del` / `incr`,
  `query` / `execute`, or `send` — those are the vocabulary people already know.
- **Shortest word a newcomer guesses right.** `download_url` beats `presignGet`; but `get`
  beats `retrieve`.

Already applied to `s3`: `presignPut → upload_url`, `presignGet → download_url`,
`presignPost → upload_form`, `presign → sign_url`, result field `maxBytes → max_bytes`.
New primitives below should follow this (e.g. `$sys.crypto.random_bytes`, not `randomBytes`).

## Namespace: bare capabilities vs `$sys.*` (decided)

Two kinds of globals, two conventions — the split _is_ the mental model:

- **Bare name = a service you connect to.** `api`, `db`, `mail`, `s3`, `redis`, `amq` —
  opt-in via `config`, do I/O, metered. Each stays a bare top-level global. Don't nest
  these.
- **`$sys.*` = the box you run inside.** The always-on standard library + execution
  context — `$sys.crypto`, `$sys.env`, `$sys.secrets`, `$sys.log`, `$sys.date`,
  `$sys.net`. One `$`-prefixed global holds them all.

Why a single `$sys` umbrella for the runtime surface (and **not** a `utils` grab-bag):

- **Collision-proof.** The risk with bare `log` / `env` / `net` is a script's own
  `var log = []` shadowing the global. Nobody writes `var $sys`, so the `$`-prefix
  reserves the name.
- **Short, and a _learned idiom_.** `$` (Decimal) already teaches users that a
  `$`-prefix means "engine builtin, not your code." `$sys` is its sibling. `$sys.crypto`
  is barely longer than bare and far shorter than `utils.crypto`.
- **Self-describing.** `$sys` reads as "system / runtime surface" literally — unlike the
  internal `virtual-host`/`$vh` metaphor, which a newcomer can't guess (fails the
  "shortest word a newcomer guesses right" rule above).
- **Not a junk drawer.** The rule is precise — `$sys.*` is _only_ the runtime/stdlib
  surface; services never go under it.

`$` (Decimal) stays bare — it predates this split and is a beloved one-char idiom; treat
it as grandfathered, not a counterexample.

---

## 1. The mental model

Today a request is stateless and anonymous: `script` + `context` + a flat `config`
that happens to hold `db`/`mail`/`s3`/etc. Each capability is wired up ad-hoc per request.

A **virtual-host** reframes that same execution as a _programmatic container_:

- It has an **identity** (a name) and an **environment** (config + secrets).
- It exposes **network + cloud primitives** that are pure or cheap (DNS, TCP probe,
  signing, encoding) so a script can _manage_ resources, not just call them.
- It still has **no OS, no filesystem, no process, no persistent memory** — the
  "host" is virtual. That's the whole point and the reason it stays tiny.

Think Cloudflare Workers "bindings" or a Lambda execution context, but shrunk to
the subset that costs ~nothing in binary size because we already link the crates.

This is **additive sugar over the existing capability pattern**, not a rewrite.
Every idea below is opt-in per request, string-in/string-out across the QuickJS
boundary, and metered through `sandbox.rs`, exactly like `db`/`mail`/`s3`.

## 2. Design constraints (the weight budget)

What "lightweight" means concretely for this repo:

1. **Reuse crates already in `Cargo.toml`.** `sha2`, `hmac`, `base64`, `hex`,
   `percent-encoding`, `uuid`, `chrono`, `serde_json`, and `tokio` (`net`) are
   already linked. Anything built only from these adds _near-zero_ to the ~18 MB
   distroless binary.
2. **No second crypto stack.** Per CLAUDE.md, TLS must reuse the `aws-lc-rs`
   rustls provider. No `ring`, no `native-tls`, no OpenSSL.
3. **No cloud SDKs.** `aws-sdk-*`, `google-cloud-*`, `azure_*` are each tens of
   crates and megabytes. The S3 module already proves the pattern: _presign with
   our own SigV4 over `hmac`/`sha2`_ instead of pulling a whole SDK. Apply the
   same discipline to any new cloud primitive.
4. **Respect the two trust models.** Script-controlled targets (a URL/host the JS
   chooses) must go through the SSRF guard in `ssrf.rs`. Operator-supplied targets
   (connection blobs in `config`) are trusted and skip it. Each idea below states
   which model it falls under.
5. **Keep it pure where possible.** Pure CPU primitives (hashing, encoding, cron
   math) need no I/O, no metering of network ops, and can be _always-injected_
   like `$`/Decimal — the cheapest possible capability.

## 3. Proposed primitives (lightest first)

### 3.1 `$sys.env` + `$sys.secrets` — _zero new deps_

A read-only key/value surface injected from operator config. Scripts stop
embedding credentials in `context`.

```js
const region = $sys.env.REGION; // plain config values (readable strings)
// secrets are USABLE but not EXTRACTABLE -- pass the handle into the one-way HMAC
// sink; the plaintext only ever materializes inside Rust, never as a JS string:
$sys.crypto.hmac("sha256", $sys.secrets.SIGNING_KEY, body); // -> digest (one-way)
// String(secret) / JSON.stringify(secret) / `${secret}` all yield "[secret:NAME]".
// NOT supported (dropped, see below): handing the raw value to api/db/mail auth.
```

- **Why:** every serverless platform separates code from config/secrets. Lets the
  same script run across environments.
- **Weight:** pure serde. The real work is the **redaction guard** below.
- **Trust:** operator-supplied → trusted. Inject only the keys present in config
  (opt-in, like every other capability).
- **`secrets` are use-not-extract (IMPLEMENTED — opaque handles only).** A script must be
  able to _use_ a secret but must never _exfiltrate_ it by returning it in `data` — the
  core multi-tenant worry. The shipped guarantee is **structural, single-mechanism**:
  - **Plaintext never enters the JS heap.** It stays Rust-side in a per-request
    `SecretStore` (`sys.rs`); `$sys.secrets.X` is a frozen **opaque handle** carrying only
    the secret's _name_. Every coercion (`String(x)`, template literal, `JSON.stringify`,
    `valueOf`) yields `"[secret:X]"` — never the bytes — so `return json({ k:
    $sys.secrets.X })` leaks nothing.
  - **One one-way sink.** A handle is resolved to plaintext solely inside
    `$sys.crypto.hmac` (key position), whose output is a digest. Reflecting ops
    (`base64`/`hex`/`url`/`sha*`) **reject** a handle; a secret can't be the HMAC message.
  - **No output scrubber, by choice.** We deliberately ship _no_ redaction fallback: a
    scan only catches un-transformed values, so it's evadable security theater, not a
    guarantee. Keeping plaintext out of JS _is_ the guarantee — there is no decode path to
    filter. _Honest caveat:_ HMAC of a low-entropy secret is brute-forceable; secrets must
    be high-entropy.
  - **DROPPED (too risky for multi-tenant):** a `secrets.reveal` operator opt-in that
    hands a script the raw plaintext string, and using secrets as `api`/`db`/`mail` auth.
    Both create a transmit-to-observable path (a tenant-chosen, even allowlisted,
    destination can echo the value) that can't be proven 100% at this layer. Per the
    "100% or drop it" rule they stay out — revisit only with operator-pinned per-secret
    destination binding.

### 3.2 `$sys.crypto` — _zero new deps, highest value_

Expose what `sha2` + `hmac` + `base64` + `hex` + `uuid` already give us. This is
the single best weight-to-value addition because the crates are _already linked_
(S3 SigV4 uses them). **Encoding helpers fold in here too** (see 3.3) so there's one
crypto/encoding global, not two.

```js
$sys.crypto.sha256(data); // -> hex
$sys.crypto.hmac("sha256", key, msg); // -> hex/base64
$sys.crypto.uuid(); // v4, uuid crate already present
$sys.crypto.random_bytes(16); // -> hex/base64
$sys.crypto.jwt.sign(claims, key); // HS256 = hmac+base64url, ~trivial
$sys.crypto.jwt.verify(token, key); // verify signature + exp
// encoding (folded from `codec`):
$sys.crypto.base64.encode(s);
$sys.crypto.hex.encode(bytes);
$sys.crypto.url.encode(s); // percent-encoding
```

- **Why cloud-native:** signing/verifying webhooks (Stripe, GitHub), minting
  short-lived service tokens, idempotency keys, content hashing for caches/ETags.
  These are the bread and butter of glue functions.
- **Weight:** ~0. HS256 JWT is literally `base64url(header).base64url(payload)`
  HMAC-signed — no `jsonwebtoken` crate needed. Random bytes come from the same
  `getrandom` that `uuid`'s v4 already pulls.
- **Trust:** pure CPU, no I/O → **always-injectable** like `$`/Decimal, or gated
  behind a trivial `"crypto": true` flag.
- **Scope guard:** stick to HMAC/SHA/UUID/random. _Do not_ add RSA/ECDSA/asymmetric
  JWT here — that risks pulling a bigger crypto surface. If asymmetric is ever
  needed, route it through `aws-lc-rs` (already linked) rather than a new crate.

### 3.3 Encoding — _folded into `$sys.crypto`_

Encoding helpers QuickJS lacks natively: `base64` / `base64url` / `hex` /
`percent-encoding` (URL escape) — all already in the tree. **Decision: these live under
`$sys.crypto`** (`$sys.crypto.base64` / `.hex` / `.url`, shown in 3.2) rather than a
separate `codec` global — one fewer name, and they pair naturally with hashing/signing.

- **Why:** building auth headers, encoding payloads for queues, URL-safe IDs.
- **Weight:** ~0. Pure, part of the always-on `$sys` surface.

### 3.4 `$sys.net` — DNS resolve + TCP health probe — _zero new deps (tokio `net`)_

Lightweight network _introspection_, distinct from `http` (which is full HTTP).

```js
// $sys.net is OFF unless operator config enables it AND gives an allowlist:
//   "net": { "allow": ["db.internal:5432", "cache.internal:6379"] }
$sys.net.resolve("db.internal"); // -> ["10.0.0.5"]  (only permitted names)
$sys.net.probe("db.internal", 5432); // -> { open: true }  (boolean only — no latency)
```

- **Why cloud-native:** "is the dependency reachable?", service discovery, simple
  readiness gates before doing real work.
- **Weight:** ~0 — `tokio` is already built with the `net` feature.
- **Multi-tenant policy (decided — safest stance).** A script-chosen `host:port` is a
  _more_ dangerous SSRF primitive than HTTP (raw TCP, usable as a port-scanner / network
  oracle / connect-flood relay). For multi-tenant, lock it down hard:
  1. **Default off**, opt-in per request like every capability.
  2. **Operator allowlist is the primary control.** Config supplies the exact
     `host:port` targets a script may touch (`net.allow`); anything else is rejected.
     This converts `net` from a _script-chosen-target_ (SSRF) primitive into the
     **trusted-target** model — the operator, not the tenant script, decides what's
     reachable. Single biggest safety win.
  3. **SSRF guard as backstop** (`ssrf.rs`): even allowlisted names are re-resolved and
     the **resolved IP** re-checked; private / loopback / link-local / ULA / cloud-metadata
     ranges (incl. `169.254.169.254`) are blocked unless the operator sets
     `allow_private_targets` (a multi-tenant operator won't). **Pin the connection to the
     validated IP** so resolve-time and connect-time IPs match — closes the DNS-rebinding
     TOCTOU.
  4. **Boolean result, no timing oracle.** Return `{ open: true|false }` only — **no
     `ms`**, and collapse "closed" vs "filtered" into one value, so the box can't map an
     internal network by timing. (A single-tenant / trusted deployment may opt into
     latency.)
  5. **Connect-and-close only.** No TCP read/write of script bytes — no
     port-scanner-with-payload, no tunnel.
  6. **Tight caps.** Short per-probe connect timeout, a cap on distinct targets per run,
     each probe/resolve metered against `max_ops`; `resolve` results filtered the same way
     (private IPs dropped unless allowed) so it can't be a DNS-recon tool.
- **Land last** — highest security bar; give it the scrutiny `http.rs` got.

### 3.5 `$sys.date` — parse, timedelta math, cron — _zero / one tiny dep_

Dates are the thing a glue function juggles most: a timestamp arrives from the
frontend as a string, you need "now + 3 days", or you validate/normalize a cron
expression for a downstream scheduler. The function can't _sleep_ (wall-clock
timeout), but computing and reshaping dates is pure and free — `chrono` is already
linked. Method-chained like `$`/Decimal (JS has no operator overloading), backed by a
panic-free `__date(op, ...)` FFI over chrono's `checked_*` arithmetic.

**Parse what the frontend sends.** Accepts ISO 8601 / RFC 3339 (with or without an
offset), a date-only `YYYY-MM-DD`, or epoch millis; always normalizes to **UTC**. Throws
a typed error on garbage rather than silently guessing.

```js
$sys.date.now(); // current instant (UTC)
$sys.date.parse("2026-06-04T12:00:00+02:00"); // offset-aware -> normalized UTC
$sys.date.parse(1780000000000); // epoch millis -> date
```

**Add / subtract a duration — Python `timedelta`.** Fixed-length units only —
`weeks` / `days` / `hours` / `minutes` / `seconds` / `ms` — matching Python's
`timedelta`, which deliberately omits months/years because they aren't constant length.

```js
const due = $sys.date.now().add({ days: 3, hours: 12 });
const start = $sys.date.parse(ctx.when).sub({ weeks: 1 });
due.iso(); // "2026-06-07T00:00:00Z"  (RFC 3339, to store / send back)
due.unix(); // 1780...                 (epoch seconds)
```

**Difference between two dates → a timedelta breakdown.**

```js
const gap = $sys.date.parse(b).diff($sys.date.parse(a));
// gap = { total_seconds: 270000, days: 3, hours: 3, minutes: 0, seconds: 0 }
```

**Cron math** (carried over from the old `clock`): validate/normalize a cron expression
and compute the next fire time — pairs naturally with the repo's `/schedule` + cron.

```js
$sys.date.cron.next("*/5 * * * *"); // -> next fire time (UTC ISO)
```

- **Why cloud-native:** normalizing frontend timestamps, TTLs and expiries ("token good
  for 3 days"), idempotency windows, "run me again at T" for an external scheduler.
- **Weight:** `now` / `parse` / `add` / `sub` / `diff` are free (`chrono`). Only cron
  _parsing_ might add one tiny crate (or a hand-rolled 5-field parser) — keep it
  optional/phase-2 if staying at strict zero matters.
- **Trust:** pure CPU, part of the always-on `$sys` surface.
- **Scope guard:** keep `timedelta` to fixed-length units. _Calendar_ math (`+1 month`,
  `+1 year`) is ambiguous (Jan 31 + 1 month = ?); if ever needed, expose it separately as
  `add_calendar({ months, years })` so the fixed-vs-calendar distinction is explicit —
  don't smuggle months into `timedelta`.

### 3.6 Structured `$sys.log` / events in `meta` — _zero new deps_

A virtual-host should be observable. Give the script a structured sink that drains
into the response `meta` (no stdout — the lint set forbids `print_stdout` anyway).

```js
$sys.log.info("charged", { amount: 42, currency: "usd" });
// -> surfaces under meta.sys.logs, with level + timestamp
```

- **Why:** observability is table stakes for serverless; today a script can only
  communicate via its return value. Structured events let callers trace behavior.
- **Weight:** ~0 — a `Collector<T>` exactly like the existing metric collectors,
  drained into `meta` like `*_requests`.
- **Bounded:** cap count/size via the sandbox so logs can't blow the response.

## 4. What to deliberately NOT add (protecting the weight budget)

- ❌ **Cloud provider SDKs** (AWS/GCP/Azure). Each is megabytes + dozens of crates.
  Follow the S3 precedent: hand-roll the _one_ wire format we need over existing
  crypto crates, or skip it.
- ❌ **A filesystem / persistent volume.** The host is virtual on purpose. State
  belongs in `redis`/`db`, which already exist.
- ❌ **Long-lived sockets / WebSockets / gRPC.** Streaming + HTTP/2 framing pulls
  weight and breaks the synchronous, bounded-wall-clock execution model.
- ❌ **A second crypto stack** for asymmetric JWT/TLS. Reuse `aws-lc-rs`.
- ❌ **A real scheduler/queue runtime.** Compute the _schedule_; let something
  external act on it (return it in `meta`, or push to `amq`).
- ❌ **Process/exec/`Command`.** No subprocesses — antithetical to the sandbox.

## 5. Weight ledger (deps reused vs. added)

All live under the one `$sys.*` global (encoding folded into `$sys.crypto`).

| Idea (under `$sys`)      | Reuses (already linked)                       | New crate? | Trust model        |
| ------------------------ | --------------------------------------------- | ---------- | ------------------ |
| `$sys.env` / `.secrets`  | serde                                         | none       | operator (trusted) |
| `$sys.crypto`            | sha2, hmac, base64, hex, percent, uuid, rand  | none       | pure / always-on   |
| `$sys.net.resolve/probe` | tokio (`net`)                                 | none       | **script → SSRF**  |
| `$sys.date` parse/add/diff | chrono                                       | none       | pure / always-on   |
| `$sys.date.cron`         | chrono (+ maybe 1 tiny cron parser)           | optional   | pure / always-on   |
| `$sys.log` / events      | serde, existing `Collector<T>`                | none       | n/a                |

Every row is strict **zero-new-dependency** (encoding folded into `$sys.crypto`). Only
optional cron parsing might add a single small crate.

## 6. Suggested rollout order

1. **`$sys.crypto`** (incl. folded encoding) — biggest value, zero weight, pure →
   simplest to land (mirror `decimal.rs`, the always-injected pure-capability template).
   Landing this first also establishes the `$sys` global the rest hang off.
2. **`$sys.env` / `$sys.secrets`** — small, unlocks "config separate from code." Get the
   redaction discipline right early.
3. **`$sys.log` / events** — observability; reuses the collector machinery verbatim.
4. **`$sys.net`** — most valuable for "managing network resources," but **highest
   security bar** (script-controlled SSRF target). Land it last, with the same
   scrutiny `http.rs` got.
5. **`$sys.date`** — `parse` + `timedelta` (`add`/`sub`/`diff`) are zero-weight and
   high-value (every frontend timestamp needs them); land those with `$sys.crypto`.
   `$sys.date.cron` is the nice-to-have — defer if avoiding even one new crate matters.

## 7. Open questions

- Should the pure parts of `$sys` (`crypto`/`date`) be **always-injected** (like `$`)
  or gated behind a flag? Leaning always-on: `$sys` is a single global, so the namespace
  stays minimal either way, and "always there" is friendlier. The I/O / config parts
  (`net`, `env`, `secrets`, `log`) still inject only when their config is present —
  `$sys` is the umbrella, not a promise every sub-key exists.
- ~~For `$sys.net.probe`, do we expose latency (`ms`)?~~ **Decided:** no `ms` in
  multi-tenant — boolean-only result, "closed"/"filtered" collapsed, to deny a timing
  oracle (§3.4). A single-tenant deployment may opt into latency.
- ~~Where do `secrets` redaction boundaries live?~~ **Decided & shipped:** secrets are
  _use-not-extract_ via **opaque handles only** — plaintext stays Rust-side and is
  resolved solely by the one-way HMAC sink. No output scrubber and no `reveal` escape
  hatch (both evadable / transmit-to-observable → dropped as not-100% for multi-tenant,
  §3.1).
