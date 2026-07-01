## Context

The runtime is named `runlet` (binary crate, core crate), but two observable/internal
identifiers still carry the historical `jsbox` name:

1. **Prometheus metric namespace** `jsbox_*` — emitted as a hand-rolled text exposition in
   `crates/runlet-core/src/metrics.rs::render` (a single `format!` string; no metrics library).
2. **Capability-error wire tag** `__jsbox` — the JSON envelope a JS capability wrapper attaches
   to a thrown error (`e.__jsbox = res`), which the engine reads back to classify a throw as a
   capability error vs. a script error, and which `fabric-wire` reuses as the box↔`fabricd`
   error envelope. It spans JS wrappers, `runlet-core` (engine/errors/sys), and `fabric-wire`.

Both were previously retained "for compatibility." The deployment is **greenfield**: no
external scrapers/dashboards/alerts and no independently-versioned `fabricd` in the field, so
nothing is pinned to the old names. This change removes the historical name from the two
identifiers before the enterprise observability rebuild (OpenTelemetry + per-tenant signals)
lands on this surface — so the new work is not simultaneously renaming a moving target.

Constraints: the strict lint gauntlet (no `unwrap`/`expect`/`panic`, no `as`, pedantic/nursery)
must stay clean; build/test are Docker-only (`aws-lc-sys` needs a C toolchain).

## Goals / Non-Goals

**Goals:**
- Every emitted Prometheus series uses the `runlet_` prefix; labels/semantics unchanged.
- The capability-error tag is `__runlet` everywhere it is written or read, with the JS wrapper,
  the engine reader, and the `fabric-wire` envelope changed in one atomic commit.
- Residual `jsbox` in log lines and code comments becomes `runlet`; docs and `CLAUDE.md` are
  updated and the stale "kept for compatibility" clauses removed.
- Zero behavior change: same outcomes, same error classification, same metric values.

**Non-Goals:**
- No rebrand of the product/repo name "jsbox" (prose, directory, image repo) — out of scope.
- No new metrics, labels, dimensions, dependencies, or per-tenant attribution (separate change).
- No OpenTelemetry migration — that is the follow-on observability foundation change.
- No change to metric label names/values, cache event names, or the `meta` envelope fields.

## Decisions

**D1 — Rename the tag as a single atomic change across all three layers, not phased.**
The `__jsbox`/`__runlet` tag is a private contract between the JS wrapper (writer), the engine
(reader), and the `fabric-wire` envelope (transport). A phased rename would require a transition
where both names are accepted. Because the box and `fabricd` build and ship from one repo/one
release, there is no version skew to bridge — so a compatibility shim would be dead code the day
it is written. Change all writers and readers in the same commit; no dual-read.
*Alternative considered:* accept both `__jsbox` and `__runlet` on read for one release — rejected:
no independent peer exists, so it adds complexity and a lint surface for zero benefit.

**D2 — Keep the hand-rolled metric render; only substitute the name literals.**
The metric exposition stays a `format!` string in `metrics.rs`; this change edits only the
metric-name literals (and their `# HELP`/`# TYPE` lines), not the structure. Adopting a metrics
library or OpenTelemetry is explicitly deferred to the observability foundation change so this
stays a pure, reviewable rename with no dependency delta.
*Alternative considered:* fold the rename into the OTel adoption — rejected in explore: renaming a
moving target is harder to review and couples a mechanical change to an architectural one.

**D3 — Treat the metric-name change as a spec-level delta; the tag change as implementation.**
The observability spec names the `jsbox_*` series as part of its behavioral contract, so the
prefix change is a `MODIFIED` requirement in the delta spec. The `__jsbox` tag is not named in
any spec (it is an internal wire detail), so it needs no spec change — only code + comments.

**D4 — Assert the new names, don't just remove the old.**
The `test_simple.py` metric assertions and the `metrics.rs` unit tests are updated to assert the
`runlet_*` names (positive assertions), and the gate is the proof the rename is complete and
consistent. Grep for surviving `jsbox_`/`__jsbox` in code/JS/tests is part of task verification.

## Risks / Trade-offs

- **A stray `jsbox_`/`__jsbox` is missed in one layer** → a missed writer/reader on the tag would
  misclassify a capability error as a script error (silent behavior change). *Mitigation:* the tag
  is changed writer-and-reader in one commit; unit test `EgressError` round-trips through the tag
  and classifies as a capability error (existing test at `engine.rs`); final grep gate for both
  tokens across `.rs`/`.js`/`.py`.
- **A missed metric-name literal** → a series silently keeps the old name; a scrape/dashboard
  built later would miss it. *Mitigation:* the render is one `format!` block; `test_simple.py`
  asserts the `runlet_*` names; grep gate.
- **Docs drift** → the `CLAUDE.md` compatibility clauses and design docs still say `jsbox_*`.
  *Mitigation:* explicit doc tasks; the clauses are removed, not just edited.
- **Low overall risk:** no dependency, no data model, no external consumer, no behavior change.

## Migration Plan

Greenfield — no live migration. Single commit changes writers and readers together; box and
`fabricd` ship from the same build so the tag cannot skew. Rollback is a straight revert (no data
or persisted-format implications). Run the full Docker gate (`cargo fmt --check`, `cargo clippy`,
`cargo test`) plus the `test_simple.py` metric assertions before merge.

## Open Questions

- None blocking. (The product/repo rebrand and the OpenTelemetry/per-tenant observability rebuild
  are deliberately separate follow-on changes, already scoped in explore.)
