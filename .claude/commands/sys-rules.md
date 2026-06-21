---
name: sys-rules
description: Produces the single abstract, language-agnostic set of coding rules to follow, written to docs-sys/rules.md
argument-hint: [system, platform, or product idea to derive coding rules from]
allowed-tools: Read, Grep, Glob, Write
---

# sys-rules — Idea → The single ruleset for HOW to code

Transform the input concept into the project's **one canonical ruleset**: the abstract principles and constraints that all code MUST follow.

Input: **$ARGUMENTS**

This produces the content of **`docs-sys/rules.md`** — the single source of truth for how code is built. There is exactly ONE such document; this command writes/replaces its content. It is a constraint system, NOT an implementation plan.

---

## Hard constraints

- **Single design.** One coherent philosophy, not a menu of options. Decide the rules and state them.
- **Abstract, not literal.** Rules express principles and boundaries, never concrete code, file layouts, or procedures.
- **Programming-language agnostic.** No language, framework, library, vendor, or runtime names. Rules must hold regardless of stack.
- **Declarative & enforceable.** Each rule is a clear MUST / SHOULD / MAY that an author or reviewer can check against.

---

## What to define

### 1. Architectural priorities (ranked)
- The few priorities that govern this system, each with: why it matters, what it prevents, the risk if misused.

### 2. Structural model
- What is "core" vs "external"; the allowed direction of dependencies; how boundaries are enforced — all in abstract terms (ports/adapters, layering, isolation as concepts, not as named tech).

### 3. Design principles
- The guiding principles (e.g. simplicity over abstraction, composition over inheritance, explicit boundaries over implicit coupling).
- Each principle: definition, when it applies, when it MUST NOT be applied.

### 4. Required properties
- The properties code MUST preserve (modularity, decoupling, composability, testability, framework independence) and how each is upheld structurally.

### 5. Anti-patterns (rejected)
- What this codebase explicitly forbids (over-abstraction, hidden dependencies, tight coupling to frameworks/vendors, scattered business logic, gratuitous patterns).

### 6. Enforcement
- What constitutes a violation.
- What requires re-evaluating the rules themselves rather than the code.

---

## Output

Produce a clean, declarative ruleset — Title, Objective, then the sections above — written as stable, long-lived, opinionated-but-justified rules. Nothing language- or implementation-specific may appear.

**Write the document to `docs-sys/rules.md`**, creating the `docs-sys/` folder and the file if they do not exist, and replacing the file's content if it does. The written file is the complete output; do not leave the ruleset only in chat.
