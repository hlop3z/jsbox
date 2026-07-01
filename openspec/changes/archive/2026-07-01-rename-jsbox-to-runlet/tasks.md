## 1. Metric namespace `jsbox_` → `runlet_` (runlet-core)

- [x] 1.1 In `crates/runlet-core/src/metrics.rs::render`, rename every metric name and its
  `# HELP`/`# TYPE` lines from `jsbox_*` to `runlet_*` (families: `executions_total`,
  `rejections_total`, `overload_total`, `db_breaker_trips_total`, `bulkhead_permits_available`,
  `bulkhead_permits_total`, `execution_duration_seconds`, `capability_op_duration_seconds`,
  `bytecode_cache_entries`, `bytecode_cache_events_total`). Leave all labels/values unchanged.
- [x] 1.2 Update the `metrics.rs` unit tests (`renders_zeroed_registry`,
  `renders_bytecode_cache_family`, and any histogram-name asserts) to assert the `runlet_*` names.

## 2. Error tag `__jsbox` → `__runlet` (atomic across all layers)

- [x] 2.1 JS wrappers: rename `e.__jsbox`/`__jsbox` → `__runlet` in
  `crates/runlet-core/src/js/io.js` and `crates/runlet-core/src/js/s3.js`.
- [x] 2.2 Engine/taxonomy reader: rename the `__jsbox` tag key and its references in
  `crates/runlet-core/src/{engine.rs,errors.rs,sys.rs}` (the `obj.get("__jsbox")` read, tag
  builders, and doc comments).
- [x] 2.3 Wire envelope: rename `__jsbox` → `__runlet` in
  `crates/fabric-wire/src/{egress.rs,errors.rs,wire.rs,lib.rs}` (the `EgressError` tag render,
  the error-taxonomy envelope, and doc comments).
- [x] 2.4 Grep-verify no `__jsbox` remains in any `.rs`/`.js` file (writers and readers both moved).

## 3. Logs, comments, and residual mentions

- [x] 3.1 Rename residual `jsbox` in log-line strings and code comments to `runlet` across the
  workspace (e.g. `handler.rs` scrape-compat comment, `config.rs`, `db.rs`/`mongo.rs` comments,
  `breaker.rs`, `engine.rs` blurbs). Do not touch product-name prose (out of scope per proposal).

## 4. Docs + CLAUDE.md

- [x] 4.1 Update metric names in `docs/deployment.md`, `docs/design/resilience.md`, and
  `docs/design/resource-egress.md` to `runlet_*` / `__runlet`.
- [x] 4.2 In `CLAUDE.md`, rename the tokens and remove the now-false "kept for compatibility"
  clauses for the `jsbox_*` metric names and the `__jsbox` error tag.

## 5. Integration test

- [x] 5.1 Update `test_simple.py` `/metrics` assertions to expect the `runlet_*` names.

## 6. Verify

- [x] 6.1 Final grep gate: no `jsbox_` or `__jsbox` remains in `--include=*.rs --include=*.js
  --include=*.py` (excluding `target/`); remaining `jsbox` prose is intentional product-name only.
- [x] 6.2 Run the full gate in Docker: `cargo fmt --check`, `cargo clippy` (clean), `cargo test`
  (incl. the `EgressError`-round-trips-as-capability-error classification test and the renamed
  metric tests). Confirm no new dependencies and `cargo tree -i ring` unchanged.
  (Also fixed a pre-existing `dropping_copy_types` warning in `handler.rs` quota test:
  `drop(plans.insert(..))` → `let _ = plans.insert(..)`, surfaced by the clean-build gate.)
- [x] 6.3 Run `/opsx:sync` to fold the `observability` delta into the main spec, then archive.
