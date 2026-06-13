# decimal Specification

## Purpose

The `$` / `Decimal` global gives handlers exact base-10 decimal arithmetic inside the QuickJS
sandbox, avoiding binary-float drift (e.g. `0.1 + 0.2`). It is backed by `rust_decimal` — the
same engine `db.rs` uses to decode Postgres `NUMERIC`/`DECIMAL` — so in-script math matches the
database exactly. JavaScript has no operator overloading, so the API is method-based and
chainable rather than `+ - * /`. Rationale: `src/decimal.rs`, `src/js/decimal.js`,
`docs/05-decimal.md`, and `CLAUDE.md`.

## Requirements

### Requirement: Always-on injection

The system SHALL inject the `$` / `Decimal` global into every execution context unconditionally,
because the capability is pure (no I/O, no per-op metering) and takes no configuration.

#### Scenario: Available with no config

- **WHEN** a handler runs with no capability config in the request
- **THEN** `typeof $ === "function"` and `typeof Decimal === "function"`, and `$` and `Decimal` are the same constructor

#### Scenario: Not metered against the operation cap

- **WHEN** a handler performs decimal operations
- **THEN** those operations do not count toward `max_ops` and produce no `meta` capability metrics

### Requirement: Decimal construction

`$(value)` / `Decimal(value)` SHALL build a decimal from a string, a number, or another decimal,
preserving exact value when the input is a string.

#### Scenario: Construct from a string

- **WHEN** a handler calls `$("19.99")`
- **THEN** a decimal whose `.toString()` is `"19.99"` is produced

#### Scenario: Construct from an existing decimal

- **WHEN** a handler passes a decimal back into `$(...)`
- **THEN** the same value is returned without re-parsing

#### Scenario: Invalid input throws

- **WHEN** a handler calls `$("not-a-number")`
- **THEN** a JavaScript error is thrown that the handler can catch with `try/catch`

### Requirement: Method-based arithmetic

The system SHALL expose arithmetic as chainable methods (`add`, `sub`, `mul`, `div`, `neg`,
`abs`, `round`) — not the `+ - * /` operators — each returning a new decimal.

#### Scenario: Chained arithmetic

- **WHEN** a handler evaluates `$("19.99").mul(3).add("0.01").toString()`
- **THEN** the result is the exact string `"59.98"`

#### Scenario: Method arguments are coerced

- **WHEN** a method receives a number, string, or another decimal as its argument
- **THEN** it is coerced to a decimal before the operation

### Requirement: Exactness

Decimal arithmetic SHALL be exact in base 10, free of the binary-floating-point drift that
afflicts native JS number math.

#### Scenario: No 0.1 + 0.2 drift

- **WHEN** a handler evaluates `$("0.1").add("0.2").toString()`
- **THEN** the result is exactly `"0.3"`, not `0.30000000000000004`

#### Scenario: Matches the database NUMERIC engine

- **WHEN** a handler wraps a `NUMERIC`/`DECIMAL` value read as a string from the database in `$(...)` and does math
- **THEN** the result is exact and consistent with the database's own decimal arithmetic

### Requirement: Half-up rounding

`round(places)` SHALL round to the given number of decimal places using half-away-from-zero
(money-friendly half-up) rounding.

#### Scenario: Round to cents

- **WHEN** a handler evaluates `$("19.985").round(2).toString()`
- **THEN** the result is `"19.99"`

#### Scenario: Default places

- **WHEN** a handler calls `.round()` with no argument
- **THEN** it rounds to 0 decimal places

### Requirement: Comparison

The system SHALL provide comparison helpers (`cmp`, `eq`, `lt`, `lte`, `gt`, `gte`, `isZero`,
`isNegative`) over exact decimal values.

#### Scenario: Ordering predicates

- **WHEN** a handler evaluates `$("19.99").gt("9.99")`
- **THEN** the result is `true`

#### Scenario: cmp tri-state

- **WHEN** a handler calls `.cmp(x)`
- **THEN** it returns `-1`, `0`, or `1` for less-than, equal, or greater-than

### Requirement: Panic-free failure

Every decimal operation SHALL use checked arithmetic and surface overflow, division by zero, and
parse failures as catchable JavaScript errors rather than crashing the engine.

#### Scenario: Division by zero throws

- **WHEN** a handler evaluates `$("10").div(0)`
- **THEN** a JavaScript error is thrown (no panic, no process abort)

#### Scenario: Overflow throws

- **WHEN** an operation produces a value outside the decimal's representable range
- **THEN** a `"decimal overflow"` error is thrown that the handler can catch

### Requirement: Output and serialization

A decimal SHALL expose `toString()` for its exact text and `toNumber()` for a lossy JS number,
and SHALL serialize to its exact string in `json(...)` / `JSON.stringify`.

#### Scenario: Exact string output

- **WHEN** a handler calls `.toString()` on a decimal
- **THEN** it returns the exact decimal text (e.g. `"59.98"`)

#### Scenario: Auto-stringified in the response

- **WHEN** a handler returns a decimal inside `json(data, error)`
- **THEN** the decimal is serialized as its exact string value in the response `data`
