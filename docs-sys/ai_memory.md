# AI Memory — persistent

> Long-term, high-level-abstraction memory. Small + clean SoC. `./` relative paths only.
> Tracks how the system fits together: which adapters / ports / domains associate. Folders by default; files only on special occasions.

## Associations (adapter / port / domain)

- **`./crates/runlet-core/`** — the reusable logic host (the `LogicHost` port: `Invocation` →
  `Outcome`). External consumer-feedback inbox at **`./crates/runlet-core/CONSUMER_NOTES.md`**:
  gaps hit by embedders (notably reactive-database-pg) — top item is a missing graceful
  **shutdown/teardown** API on `LogicHost` (no `shutdown()`/`Drop` today); also `Invocation` not
  being `#[non_exhaustive]` (field adds break consumers), and `run` being a concrete method rather
  than a trait port. Triage there.

<!-- Example:
- `./src/domain/order/` — domain. Ports: `./src/domain/order/ports/`. Adapters: `./src/adapters/persistence/order/`.
-->
