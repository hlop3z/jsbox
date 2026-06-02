//! Exact decimal helper for the `QuickJS` sandbox (`$` / `Decimal`).
//!
//! Backed by `rust_decimal` — the same engine used to read `NUMERIC` columns in
//! `db.rs`, so in-script math matches the database exactly. Always injected (it is
//! pure: no I/O, no config, no per-op metering).
//!
//! JS API: `$(value)` builds a decimal; methods `add/sub/mul/div/neg/abs/round/cmp/...`.
//! Every op crosses the FFI boundary as strings, mirroring the `db`/`mail` pattern.

use std::cmp::Ordering;
use std::error::Error;
use std::str::FromStr;

use rquickjs::{Ctx, Function, Value as JsValue};
use rust_decimal::{Decimal, RoundingStrategy};

use crate::sandbox;

/// JS wrapper — loaded from `src/js/decimal.js` at compile time.
const DECIMAL_WRAPPER: &str = include_str!("js/decimal.js");

/// Injects the `$` / `Decimal` global. Always on — pure, no I/O, no config.
///
/// # Errors
///
/// Returns an error if registration or JS eval fails.
pub(crate) fn inject_decimal(qctx: &Ctx<'_>) -> Result<(), Box<dyn Error + Send + Sync>> {
    let decimal_fn = Function::new(
        qctx.clone(),
        |op: String, lhs: String, rhs: String| -> String {
            match dispatch(&op, &lhs, &rhs) {
                Ok(value) => value_json(&value),
                Err(err) => sandbox::error_json(&err),
            }
        },
    )?
    .with_name("__decimal")?;

    qctx.globals().set("__decimal", decimal_fn)?;

    let wrapper: JsValue<'_> = qctx.eval(DECIMAL_WRAPPER)?;
    drop(wrapper);
    Ok(())
}

// -- Dispatch ---------------------------------------------------------------

/// Routes a `__decimal` call to the right operation, returning the result string.
fn dispatch(op: &str, lhs: &str, rhs: &str) -> Result<String, String> {
    let left = parse_decimal(lhs)?;
    match op {
        "parse" => Ok(left.to_string()),
        "neg" => negate(left),
        "abs" => Ok(left.abs().to_string()),
        "round" => round(left, rhs),
        "cmp" => compare(left, rhs),
        "add" | "sub" | "mul" | "div" => arithmetic(op, left, rhs),
        other => Err(format!("unknown decimal op: {other}")),
    }
}

// -- Operations -------------------------------------------------------------

/// Parses a decimal string, trimming surrounding whitespace.
fn parse_decimal(value: &str) -> Result<Decimal, String> {
    Decimal::from_str(value.trim()).map_err(|err| format!("invalid decimal '{value}': {err}"))
}

/// Negates a decimal via `0 - x` (cannot overflow for `Decimal`'s symmetric range).
fn negate(value: Decimal) -> Result<String, String> {
    Decimal::ZERO
        .checked_sub(value)
        .map(|out| out.to_string())
        .ok_or_else(|| "decimal overflow".to_owned())
}

/// Rounds to `places` decimal places using half-up (money-friendly) rounding.
fn round(value: Decimal, places_str: &str) -> Result<String, String> {
    let places: u32 = places_str
        .trim()
        .parse()
        .map_err(|_err| format!("invalid round places: '{places_str}'"))?;
    Ok(value
        .round_dp_with_strategy(places, RoundingStrategy::MidpointAwayFromZero)
        .to_string())
}

/// Compares two decimals, returning `"-1"`, `"0"`, or `"1"`.
fn compare(left: Decimal, rhs: &str) -> Result<String, String> {
    let right = parse_decimal(rhs)?;
    let token = match left.cmp(&right) {
        Ordering::Less => "-1",
        Ordering::Equal => "0",
        Ordering::Greater => "1",
    };
    Ok(token.to_owned())
}

/// Performs a checked binary operation (`add`/`sub`/`mul`/`div`).
fn arithmetic(op: &str, left: Decimal, rhs: &str) -> Result<String, String> {
    let right = parse_decimal(rhs)?;
    let out = match op {
        "add" => left.checked_add(right),
        "sub" => left.checked_sub(right),
        "mul" => left.checked_mul(right),
        "div" => {
            if right == Decimal::ZERO {
                return Err("division by zero".to_owned());
            }
            left.checked_div(right)
        }
        other => return Err(format!("unknown decimal op: {other}")),
    };
    out.map(|value| value.to_string())
        .ok_or_else(|| "decimal overflow".to_owned())
}

// -- Output -----------------------------------------------------------------

/// Builds the success envelope `{"v":"<value>"}`.
fn value_json(value: &str) -> String {
    let escaped = serde_json::to_string(value).unwrap_or_else(|_err| "\"0\"".to_owned());
    format!("{{\"v\":{escaped}}}")
}
