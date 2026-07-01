## Why

The service, its binary, and its core crate are all named `runlet`, but the observable
surface still says `jsbox`: every Prometheus series is `jsbox_*` and the internal
capability-error envelope is the `__jsbox` tag. These were kept for backward compatibility.
The deployment is **greenfield** — there are no deployed scrapers, dashboards, alerts, or
peer daemons pinned to the old names — so the compatibility constraint no longer applies, and
carrying two names is pure confusion. Renaming now, before the enterprise observability
rebuild lands on this surface, means the new work starts on consistent, correct names instead
of renaming a moving target.

## What Changes

- **BREAKING** Rename the Prometheus metric namespace `jsbox_*` → `runlet_*` (all families:
  `executions_total`, `rejections_total`, `overload_total`, `db_breaker_trips_total`,
  `bulkhead_permits_*`, `execution_duration_seconds`, `capability_op_duration_seconds`,
  `bytecode_cache_*`). Label names/values are unchanged. No dashboards/alerts exist to migrate.
- **BREAKING** Rename the internal capability-error wire tag `__jsbox` → `__runlet` in lockstep
  across its three touch points — the JS wrappers (`e.__runlet = res`), the `runlet-core`
  engine/error taxonomy that reads it, and the `fabric-wire` box↔`fabricd` envelope that
  carries it. Single repo, single release: no version skew.
- Rename residual `jsbox` mentions in **log lines and code comments** to `runlet`.
- Update docs that quote the metric names or the tag (`docs/deployment.md`,
  `docs/design/resilience.md`, `docs/design/resource-egress.md`) and drop the now-false
  "kept for compatibility" clauses in `CLAUDE.md`.
- **Out of scope:** the product/repo name "jsbox" (e.g. prose like s3's "…in jsbox"), the
  `X-Jsbox-*`-style HTTP headers if any, and the per-tenant/OpenTelemetry observability rebuild
  (tracked as separate follow-on changes). This change is namespace + tag + text only, with
  **no behavior change**.

## Capabilities

### New Capabilities
<!-- none -->

### Modified Capabilities
- `observability`: the emitted Prometheus metric names change from the `jsbox_` prefix to the
  `runlet_` prefix. This is a spec-level contract change (the metric names are named in the
  observability requirements); all outcomes, labels, and semantics are otherwise identical.

## Impact

- **Code (~12 files):** `crates/runlet-core/src/metrics.rs` (metric names + HELP/TYPE);
  `__jsbox`→`__runlet` across `crates/fabric-wire/src/{egress,errors,wire,lib}.rs`,
  `crates/runlet-core/src/{engine,errors,sys}.rs`, and JS wrappers
  `crates/runlet-core/src/js/{io,s3}.js`.
- **Tests:** `test_simple.py` assertions on `jsbox_*` metric names.
- **Docs:** `docs/deployment.md`, `docs/design/resilience.md`,
  `docs/design/resource-egress.md`, `CLAUDE.md` crate blurbs + the compatibility clauses.
- **Consumers:** none deployed (greenfield) — the "breaking" marks are contractual, not a
  live migration. Box and `fabricd` ship together, so the tag rename cannot skew.
- **No dependencies, no new crates, no behavior change.** Gate impact: `cargo fmt`/`clippy`/
  `test` + the `test_simple.py` metric-name assertions.
