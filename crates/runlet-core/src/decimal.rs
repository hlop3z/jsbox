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
pub fn inject_decimal(qctx: &Ctx<'_>) -> Result<(), Box<dyn Error + Send + Sync>> {
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
        "to_cents" => to_minor(left, rhs),
        "from_cents" => from_minor(left, rhs),
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

/// Parses the minor-unit exponent (fraction digits, e.g. 2 for cents), defaulting to 2.
fn minor_places(places_str: &str) -> Result<u32, String> {
    let trimmed = places_str.trim();
    if trimmed.is_empty() {
        return Ok(2);
    }
    let places: u32 = trimmed
        .parse()
        .map_err(|_err| format!("invalid minor-unit places: '{places_str}'"))?;
    if places > 18 {
        return Err(format!("minor-unit places too large: {places} (max 18)"));
    }
    Ok(places)
}

/// Computes `10^places` as a `Decimal` (the major↔minor scale factor).
fn scale_factor(places: u32) -> Result<Decimal, String> {
    10_u64
        .checked_pow(places)
        .map(Decimal::from)
        .ok_or_else(|| "minor-unit scale overflow".to_owned())
}

/// Converts major units to minor units: `value * 10^places`, rounded half-up to an integer.
fn to_minor(value: Decimal, places_str: &str) -> Result<String, String> {
    let factor = scale_factor(minor_places(places_str)?)?;
    let scaled = value
        .checked_mul(factor)
        .ok_or_else(|| "decimal overflow".to_owned())?;
    Ok(scaled
        .round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero)
        .to_string())
}

/// Converts minor units to major units: `value / 10^places`, fixed to `places` decimals.
fn from_minor(value: Decimal, places_str: &str) -> Result<String, String> {
    let places = minor_places(places_str)?;
    let major = value
        .checked_div(scale_factor(places)?)
        .ok_or_else(|| "decimal overflow".to_owned())?;
    Ok(major
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

#[cfg(test)]
mod tests {
    use super::dispatch;

    #[test]
    fn to_cents_default_places_scales_by_100() {
        assert_eq!(dispatch("to_cents", "19.99", ""), Ok("1999".to_owned()));
        assert_eq!(dispatch("to_cents", "1.5", ""), Ok("150".to_owned()));
    }

    #[test]
    fn to_cents_rounds_sub_cent_half_up() {
        assert_eq!(dispatch("to_cents", "1.005", ""), Ok("101".to_owned()));
        assert_eq!(dispatch("to_cents", "-1.005", ""), Ok("-101".to_owned()));
    }

    #[test]
    fn from_cents_default_places_fixes_two_decimals() {
        assert_eq!(dispatch("from_cents", "1999", ""), Ok("19.99".to_owned()));
        assert_eq!(dispatch("from_cents", "150", ""), Ok("1.50".to_owned()));
    }

    #[test]
    fn round_trip_is_identity_for_whole_cents() {
        assert_eq!(dispatch("to_cents", "42.42", ""), Ok("4242".to_owned()));
        assert_eq!(dispatch("from_cents", "4242", ""), Ok("42.42".to_owned()));
    }

    #[test]
    fn configurable_minor_unit_digits() {
        assert_eq!(dispatch("to_cents", "1000", "0"), Ok("1000".to_owned()));
        assert_eq!(dispatch("to_cents", "1.234", "3"), Ok("1234".to_owned()));
        assert_eq!(dispatch("from_cents", "1234", "3"), Ok("1.234".to_owned()));
    }

    #[test]
    fn rejects_out_of_range_places() {
        assert!(dispatch("to_cents", "1", "19").is_err());
        assert!(dispatch("to_cents", "1", "x").is_err());
    }
}
