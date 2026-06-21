# Rules — How To Code

> The single, abstract, language-agnostic set of principles and constraints all code in this
> system MUST follow. Maintained via `/sys-rules`. This is the source of truth for how code is
> built. It is a constraint system, not an implementation plan.

## Objective

Keep this system **safe, bounded, and honest** while it runs untrusted logic on shared
infrastructure. Every rule below exists to protect the invariants of [`./rfc.md`](./rfc.md):
one result shape, no cross-execution leakage, no silent effect, always bounded, honest
attribution, one canonical way. Where a rule and the RFC appear to conflict, the RFC's scope
wins and this document is corrected (see Enforcement).

This ruleset governs not only production code but the **managed specification process**: any
subordinate per-capability spec or change MUST be authored to uphold these rules, and a change
that cannot be expressed within them requires re-evaluating the rules, not bypassing them.

## 1. Architectural Priorities (ranked)

Priorities are ordered. When two conflict, the higher-ranked one wins.

1. **Safety of isolation.** Untrusted logic MUST NOT escape its sandbox, exceed its grants, or
   affect another execution. *Prevents:* the catastrophic failure — one caller harming another
   or the host. *Risk if misused:* treating safety as negotiable for convenience or speed; it
   never is.
2. **Boundedness.** Every execution and every individual effect MUST be bounded in time, and
   bounded where applicable in memory, depth, count, and concurrency. *Prevents:* a slow or
   hostile input exhausting shared capacity. *Risk if misused:* a single unbounded wait or
   allocation defeats every layer above it.
3. **Honest, total error handling.** Every failure MUST be surfaced as a classified, attributed
   result; none may be silently swallowed or misattributed. *Prevents:* hidden corruption and
   blame misdirected between caller, author, operator, and downstream. *Risk if misused:*
   defensive catch-alls that hide the real cause.
4. **Trust-boundary clarity.** Every component MUST declare whether its targets are
   caller-chosen (guarded) or operator-supplied (trusted), and enforce the matching policy.
   *Prevents:* server-side request forgery and confused-deputy access. *Risk if misused:* a
   guarded path quietly accepting a caller-chosen target.
5. **One canonical way.** Each behavior and each public surface has exactly one form. *Prevents:*
   drift, redundant code paths, and ambiguity about which path is authoritative. *Risk if
   misused:* duplicating a capability "for convenience," creating two sources of truth.
6. **Simplicity that is verifiable.** Prefer the smallest design a reviewer can fully reason
   about. *Prevents:* complexity that hides the violations above. *Risk if misused:* clever
   abstraction that trades reviewability for elegance.

## 2. Structural Model

- **Core vs. external.** *Core* is the execution engine, the result/error contract, the
  metering and bounding logic, and the isolation guarantees. *External* is every effect-causing
  capability and every concrete resource it talks to. Core MUST NOT depend on the existence of
  any particular external capability.
- **Dependency direction.** Dependencies MUST point inward: external capabilities depend on
  core contracts; core MUST NOT depend on externals. A capability is plugged into core through a
  stable, narrow boundary, never by core reaching into a capability's internals.
- **Boundary shape.** Every capability MUST expose itself across its boundary through the
  narrowest possible contract — a flat, explicit data interchange — with no rich, capability-
  specific types crossing into core. What crosses the boundary MUST be validated on entry.
- **Per-request construction.** Anything that could carry state between executions MUST be
  constructed fresh per execution or be provably immutable and shared read-only. Mutable shared
  state across executions is forbidden.
- **Registries are read-only.** Operator-curated, load-once collections MUST be immutable after
  startup and MUST never be mutated on a request path.
- **Trust models are structural.** The guarded and trusted models of the RFC are enforced at the
  boundary, not by convention inside handler logic. A capability's trust model MUST be evident
  from its boundary.

## 3. Design Principles

Each principle states when it applies and when it MUST NOT be applied.

- **Explicit over implicit.** Every effect, grant, limit, and dependency MUST be visible at the
  point of use, never ambient. *Applies:* always. *Not when:* never — there is no acceptable
  hidden effect.
- **Fail closed.** When a check is missing, ambiguous, or errors, the safe outcome is to deny
  and classify. *Applies:* all security, limit, and trust decisions. *MUST NOT:* be relaxed to
  "fail open" for availability — shed load with a classified rejection instead.
- **Total functions over partial ones.** Operations MUST account for every outcome explicitly,
  including overflow, absence, and failure; no operation may abort the process on bad input.
  *Applies:* all core and capability logic. *MUST NOT:* be set aside via unchecked operations or
  assumptions that "this cannot happen."
- **Composition over inheritance.** Build capability from small, independently testable units
  combined explicitly. *Applies:* structuring behavior. *MUST NOT:* be used to fragment a single
  coherent rule across many indirections that obscure it.
- **Smallest sufficient abstraction.** Introduce an abstraction only when at least two real uses
  justify it and it removes more complexity than it adds. *Applies:* any new boundary. *MUST
  NOT:* be introduced speculatively for one use.
- **Mirror the canonical form.** New code MUST follow the established shape of its peers rather
  than inventing a parallel idiom. *Applies:* adding a capability or surface. *MUST NOT:* deviate
  without the deviation itself becoming the new canonical form, documented as such. This includes
  **naming**: a public surface MUST adopt the naming convention shared by its sibling surfaces of
  the same kind, NOT the convention of whatever underlying library or vendor it wraps. A wrapper
  translating an external API MUST rename to the house convention at the boundary; carrying the
  dependency's casing or terminology inward is a parallel idiom and is forbidden.
- **Single discoverable surface.** A public capability MUST have exactly one canonical, self-
  describing interface; no shadow shortcuts or duplicate entry points. *Applies:* all public
  surfaces. *MUST NOT:* be duplicated "for ergonomics."

## 4. Required Properties

- **Isolation.** No execution may read or write another's state except through an explicitly
  granted external resource. Upheld by per-execution construction and forbidden shared mutability.
- **Boundedness.** Every execution and effect carries an enforceable limit. Upheld by deadlines,
  caps, and bulkheads applied at the boundary, not left to the caller.
- **Modularity & decoupling.** Capabilities are independently addable, removable, and replaceable
  without altering core. Upheld by the inward dependency rule and narrow boundaries.
- **Testability.** Every unit of behavior MUST be exercisable in isolation against its contract,
  without standing up unrelated capabilities. Upheld by narrow boundaries and explicit
  dependencies.
- **Determinism of classification.** Identical inputs and identical downstream responses MUST
  yield identical result shapes and error classifications. Upheld by total error handling and a
  single result contract.
- **Framework independence.** Core logic MUST be expressible and reasoned about without reference
  to any particular runtime or vendor. Upheld by keeping framework concerns at the outermost
  boundary.
- **Auditability.** Every effect MUST be counted and metered, and every execution MUST be
  traceable end to end. Upheld by routing all effects through the metering boundary.

## 5. Anti-Patterns (rejected)

- **Silent effect.** Any effect that is not opt-in, counted, metered, and bounded. Forbidden.
- **Unbounded wait or allocation.** Any operation that can block or grow without a ceiling.
  Forbidden.
- **Process-aborting operations.** Any path that can crash the host on bad input, overflow, or
  absence, instead of returning a classified failure. Forbidden.
- **Swallowed or misattributed errors.** Catching to hide, or blaming the wrong owner. Forbidden.
- **Trust-model violation.** A caller-chosen target reaching a trusted (unguarded) path, or a
  guarded path skipping its target policy. Forbidden.
- **Cross-execution state.** Mutable state shared between executions, or registries mutated at
  request time. Forbidden.
- **Duplicate surfaces.** Two ways to do the same thing; shadow shortcuts beside a canonical
  interface. Forbidden.
- **Speculative or gratuitous abstraction.** Layers, patterns, or indirection introduced without
  a present, real justification. Forbidden.
- **Vendor/framework leakage into core.** Core logic that cannot exist without a specific runtime,
  library, or vendor. Forbidden.
- **Scattered policy.** Security, limit, or trust decisions spread across the codebase instead of
  enforced at the boundary. Forbidden.
- **Suppressing the guardrails.** Disabling, bypassing, or locally exempting an enforcement
  control rather than satisfying it. Forbidden except as a narrowly scoped, justified, recorded
  exception (see Enforcement).

## 6. Enforcement

- **What constitutes a violation.** Any code or subordinate spec that breaks a MUST in this
  document, contradicts an RFC invariant, or weakens a ranked priority in favor of a lower one.
  A violation MUST block acceptance until resolved.
- **Automated guardrails are the floor, not the ceiling.** Mechanical checks MUST be kept strict
  and MUST pass, but passing them does not prove conformance; a reviewer MUST still verify the
  principles above. A guardrail MUST NOT be loosened to admit a violation.
- **Exceptions are scoped and recorded.** Any deviation MUST be narrow, justified by a stated
  reason, and recorded at the point of deviation; an unexplained or blanket suppression is itself
  a violation.
- **When to change the code.** If a change can be made to satisfy these rules, the code (or the
  subordinate spec) is wrong and MUST be corrected.
- **When to change the rules.** If a genuine, recurring need cannot be met without violating a
  rule, that is a signal to re-evaluate **this document** — and, where scope is affected, the RFC
  — deliberately and as a unit, never by silently eroding a rule in place. Rules change by
  amendment here first, then propagation downward.
