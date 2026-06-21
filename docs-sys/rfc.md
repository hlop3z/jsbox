# RFC — What To Build

> The single, abstract, language-agnostic specification of **what** this system is and must do.
> Maintained via `/sys-plan`. This is the source of truth for scope and behavior.

## Abstract

This system is a **sandboxed code-execution service**. A caller submits a unit of
caller-authored logic together with a data context; the service runs that logic in a
strongly isolated, resource-bounded environment with opt-in, mediated access to external
effects, and returns a single structured result. The execution surface is the whole
product: there is exactly one way to run logic, one result shape, and one trust contract.

The system MUST make untrusted, caller-supplied logic safe to run on shared infrastructure:
bounded in time, memory, and effect; unable to leak state across executions; and unable to
reach resources it was not explicitly granted.

## Document Governance

This document is the **entry point** of the project's planning and rules. All lower-level,
per-capability behavioral specifications are **subordinate** to it and are managed under the
rules in [`./rules.md`](./rules.md):

- This RFC defines the abstract WHAT for the whole system. Per-capability specs refine it
  into testable requirements; they MUST NOT contradict it. On conflict, this RFC wins and
  the per-capability spec MUST be corrected.
- Any change that alters scope, a contract, an invariant, or a non-functional limit defined
  here MUST update this RFC first, then propagate down to the affected per-capability specs.
- The detailed specification process (proposing, refining, and archiving per-capability
  specs and changes) is a managed process governed by this RFC and `rules.md`; it produces
  the testable detail, never the canonical scope.

## Terminology

- **Caller** — the party that submits an execution request. Untrusted.
- **Author** — the party that writes the executed logic (the *handler*). Untrusted; may
  differ from the Caller.
- **Operator** — the party that deploys and configures the service, supplies trusted
  resource connections, and sets sandbox limits. Trusted.
- **Handler** — the caller-authored unit of logic invoked with a context, producing a result.
- **Context** — the caller-supplied data passed to the handler.
- **Execution** — one isolated run of one handler against one context.
- **Capability** — an opt-in facility that lets a handler cause an external effect
  (e.g. outbound request, data access, messaging, storage, identity verification).
- **Effect** — any observable interaction with the world outside the isolated execution.
- **Registry** — an operator-curated, load-once collection of named, reusable artifacts
  (pre-registered sources and shared modules) referenced by callers without inlining them.
- **Envelope** — the single canonical result shape returned for every execution.

## 1. Purpose & Scope

**Problem (one sentence).** Run caller-supplied logic safely on shared infrastructure with
controlled, metered access to external resources, returning a structured, machine-branchable
result.

### In scope

- A single execution entry point that accepts a handler (inline or referenced) plus a context.
- Strong per-execution isolation and operator-bounded resource limits.
- Opt-in, per-request, metered capabilities for external effects, under two distinct trust
  models (see §3.4).
- Always-available pure utilities that perform no effect.
- Load-once registries of reusable sources and modules.
- A uniform result envelope, a structured error taxonomy, and per-execution observability.
- Layered resilience that bounds the blast radius of slow or failing downstreams.

### Out of scope

- Persistent state owned by the service itself across executions (each execution is stateless
  except through explicitly granted capabilities).
- Scheduling, orchestration, or long-running/background jobs beyond a single bounded execution.
- Authoring, editing, or storage of handler logic by the service (registries are operator-curated
  out of band).
- A general management/administration surface beyond execution and health.
- Acting as a trust authority for the effects it brokers (it mediates and meters; it does not
  own downstream authorization policy).

### Actors

- **Caller** (untrusted) — submits execution requests.
- **Author** (untrusted) — supplies handler logic, inline or for registration.
- **Operator** (trusted) — configures limits, curates registries, supplies trusted resource
  connections, sets effect allow-policies.

## 2. Capabilities (functional requirements)

### 2.1 Execute a handler

- **Trigger:** a caller submits an execution request naming exactly one source and an optional
  context.
- **Outcome:** the handler is invoked with the context; its produced result becomes the response.
- **MUST NOT:** run more than one source per request; share any mutable state with another
  execution; persist any state beyond the request except via a granted capability.

### 2.2 Resolve the source (inline XOR referenced)

- **Trigger:** a request supplies inline logic or a registry reference.
- **Outcome:** exactly one source is selected and executed; the execution path is identical
  regardless of how the source was supplied.
- **MUST NOT:** accept zero or both forms; execute an unknown reference. Each of these is a
  distinct, classified request-level rejection.

### 2.3 Isolate every execution

- **Trigger:** any execution begins.
- **Outcome:** the handler runs in a fresh isolated environment with no global state inherited
  from, or surviving to, any other execution; effect-causing escape facilities not explicitly
  granted are unavailable.
- **MUST NOT:** allow one execution to observe or influence another except through an
  operator-owned external resource it was explicitly granted.

### 2.4 Bound every execution

- **Trigger:** any execution runs.
- **Outcome:** the execution is bounded by operator-configured limits on wall-clock time,
  memory, evaluation depth, and total external-operation count; exceeding any limit terminates
  the execution with a classified failure.
- **MUST NOT:** allow a handler to exceed any configured bound or to disable a bound from within.

### 2.5 Grant capabilities opt-in, per request, metered

- **Trigger:** a request includes configuration for a capability.
- **Outcome:** only the configured capabilities are exposed to the handler; each invocation is
  counted toward the operation cap and recorded into per-execution metrics.
- **MUST NOT:** expose a capability that was not configured for that request; perform an effect
  that is not counted and metered.

### 2.6 Provide always-on pure utilities

- **Trigger:** any execution.
- **Outcome:** pure, effect-free utilities (e.g. exact decimal arithmetic and other
  deterministic helpers) are available without configuration.
- **MUST NOT:** perform any external effect; require opt-in; behave non-deterministically.

### 2.7 Serve from load-once registries

- **Trigger:** the service starts; later, a caller references a registered artifact.
- **Outcome:** registries are loaded once at startup and served read-only; references resolve to
  the curated artifact.
- **MUST NOT:** mutate a registry at request time; resolve a reference that was not registered.

### 2.8 Return a uniform envelope

- **Trigger:** any execution completes, for any reason.
- **Outcome:** the response is the canonical envelope carrying the result payload, an error slot,
  and execution metadata.
- **MUST NOT:** return a shape that omits or renames the envelope slots, on success or failure.

### 2.9 Classify every failure

- **Trigger:** any failure, whether caused by the caller, the author's logic, the system, or a
  downstream.
- **Outcome:** the failure is reported as a structured, branchable error carrying a stable code,
  the failure category/source, an ownership attribution, and a retryability hint.
- **MUST NOT:** require string-parsing to branch on an error; misattribute ownership between
  caller, author, operator, and downstream.

### 2.10 Observe every execution

- **Trigger:** any execution.
- **Outcome:** the response includes a unique correlation identifier, input sizes, timing, and
  per-capability operation metrics; the correlation identifier is also recorded server-side with
  the underlying cause.
- **MUST NOT:** leak internal diagnostic detail to the caller beyond what the error contract
  permits; omit the correlation identifier.

### 2.11 Stay resilient under downstream failure

- **Trigger:** a granted downstream is slow, saturated, or unavailable.
- **Outcome:** layered defenses (an effect-level deadline, a concurrency bulkhead, per-target
  fast-fail after repeated failure, and optional per-partition fairness) keep a single bad
  downstream or noisy caller from breaching the service's responsiveness goals.
- **MUST NOT:** allow an unbounded wait on a downstream; allow one caller or one downstream to
  monopolize shared capacity.

## 3. Contracts (interfaces, abstractly)

### 3.1 Input

- A request MUST identify exactly one source: inline logic **or** a registry reference.
- A request MAY include a context; an omitted context MUST default to an empty context.
- A request MAY include configuration blocks that opt into capabilities and that carry
  operator-trusted connection details and per-request limits.
- A request MAY carry a partition identifier for fairness accounting.
- Inputs MUST be validated for shape and size **before** any execution resource is committed;
  malformed or oversized input MUST be rejected cheaply, without consuming an execution slot.

### 3.2 Output

- Every response MUST be the canonical envelope: a result payload slot, an error slot, and a
  metadata slot.
- On success the result slot carries the handler's value and the error slot is empty; on failure
  the error slot carries a structured error and the result slot is empty.
- The metadata slot MUST always be present and MUST carry at least a correlation identifier,
  input sizes, timing, and per-capability metrics.
- Given identical input and identical downstream responses, the envelope shape and classification
  MUST be deterministic; only metadata values that are inherently variable (identifiers, timings)
  may differ.

### 3.3 Invariants

- **One result shape.** Every outcome is expressed in the same envelope.
- **No leakage.** No mutable state crosses executions except through an explicitly granted
  external resource.
- **No silent effect.** Every effect is opt-in, counted, metered, and bounded.
- **Bounded always.** No execution and no single effect may run unbounded in time.
- **Honest attribution.** Each failure names its true owner and a correct retryability hint.
- **One way.** Each behavior has exactly one canonical form; there are no redundant alternative
  interfaces for the same capability.

### 3.4 Two trust models (mandatory distinction)

Every capability MUST be classified into exactly one trust model, and the classification MUST be
explicit:

- **Caller-targeted (guarded).** When the *target* of an effect is chosen by caller/author
  logic, the capability MUST enforce a target policy: an operator allow-policy, blocking of
  internal/private targets, and re-validation across any redirection or indirection. It MUST be
  impossible for handler logic to reach a target the operator did not permit.
- **Operator-supplied (trusted).** When the *target* is fixed by operator-supplied
  configuration, the capability connects to exactly what the operator named, without a target
  policy. Such a capability MUST NOT accept a caller-chosen target.

A new capability MUST adopt one of these two models; a capability that takes caller-chosen
targets MUST follow the guarded model.

### 3.5 Capability-admission criterion (the gate)

The system brokers a bounded set of effect classes directly and delegates the rest. A new
effect MUST NOT be added as a first-class, in-process capability unless **all** of the
following hold; otherwise it MUST be reached through the existing caller-targeted request
facility (the guarded model of §3.4):

- **Trusted internal target.** The effect's target is operator-supplied internal
  infrastructure, not a caller-chosen target — i.e. it belongs to the operator-supplied
  (trusted) trust model.
- **Fidelity that mediation would lose.** The effect needs type/value fidelity or resilience
  behavior that routing through a generic request facility would degrade (e.g. exact numeric
  preservation, per-target deadlines, per-target fast-fail, fairness).
- **Fits the bounded execution model.** The effect maps to a single bounded
  request→response within one execution. A streaming, subscription, or long-lived effect does
  **NOT** qualify and MUST be excluded.
- **Sustainable to embed.** A maintained dependency exists that reuses the existing security
  primitives without introducing a second, conflicting low-level stack, and whose
  supply-chain cost is acceptable.

An effect that is merely "call an external service over the network" — a caller-chosen target
with no special fidelity need — MUST NOT become a new capability; it is served by the existing
caller-targeted request facility.

When several admitted capabilities are near-identical, the shared mechanism (the boundary
contract, metering, resilience wiring, and trust-model declaration) SHOULD be factored into a
single reused scaffold so that adding a capability reduces to supplying its driver, its
value-mapping, and its trust-model classification. Capability proliferation MUST NOT be
avoided by pushing trusted internal effects onto the guarded request facility.

#### Classifications (current decisions)

- **Document data store (e.g. document-oriented database):** ADMITTED — operator-supplied
  trusted target; follows the resilience-bearing async data-access shape (per-request deadline
  on the blocking path), not the synchronous template.
- **Subject-based messaging (publish / request-reply):** ADMITTED as part of the existing
  messaging capability (a backend of it, not a new top-level capability). **Subscribe and
  streaming consumption are OUT OF SCOPE** — they do not fit the bounded single-execution model.
- **Arbitrary external service APIs:** NOT ADMITTED — served by the caller-targeted request
  facility.

## 4. Behavior & Edge Cases

For each capability the following MUST hold:

- **Normal path:** the documented outcome of §2 occurs and is reflected in the envelope and
  metadata.
- **Invalid input:** rejected with a request-category error before execution where detectable;
  otherwise classified as an author/script error.
- **Resource exhaustion:** exceeding time, memory, depth, or operation count terminates the
  execution with the corresponding classified, retryable-where-appropriate error.
- **Downstream failure:** a slow downstream is abandoned at its deadline; a repeatedly failing
  target is fast-failed; saturation sheds load rather than queuing unboundedly; each such outcome
  is a distinct, branchable error with correct ownership.
- **Author error:** an uncaught failure in handler logic that is not a tagged capability failure
  is attributed to the author, never to the operator or the system.
- **Ambiguous source:** zero or two sources, or an unknown reference, is a distinct request-level
  rejection, each with its own stable code.

## 5. Non-Functional Expectations

- **Security (primary).** Isolation and the two-trust-model boundary are the foremost goals. A
  handler MUST NOT escape its sandbox, exceed its grants, reach an unpermitted target, or affect
  another execution. Diagnostic detail exposed to callers MUST be limited by the error contract.
- **Reliability.** The service MUST remain responsive while downstreams degrade; no single
  downstream or caller may breach its responsiveness goals. Failures MUST be classified, not
  swallowed.
- **Performance.** Per-execution overhead beyond the handler's own work and granted effects
  SHOULD be small and bounded; isolation setup SHOULD be cheap enough to apply per request.
- **Scalability.** The service SHOULD scale horizontally as stateless replicas; fairness
  mechanisms SHOULD prevent a noisy partition from monopolizing a replica.
- **Determinism & observability.** Identical inputs SHOULD yield identical classifications; every
  execution MUST be traceable end-to-end via its correlation identifier.
- **Operability.** All limits, allow-policies, trusted connections, and registries MUST be
  operator-configurable; safe defaults SHOULD apply when a limit is unset.

## 6. Open Questions

- **Result-size policy.** Should result payloads be bounded the way inputs are, and how is an
  oversized result classified and attributed?
- **Cross-execution coordination.** Is any operator-mediated coordination between executions ever
  in scope, or is strict statelessness permanent?
- **Author vs. caller separation.** Where Author and Caller differ, what additional contract (if
  any) governs trust between a registered artifact and its invoker?
- **Determinism guarantees.** How strong a determinism guarantee can be offered to callers given
  that granted effects are inherently non-deterministic?
- **Fairness key authority.** Who is authoritative for the partition/fairness identifier, and how
  is it protected from spoofing by a caller seeking more than its share?
