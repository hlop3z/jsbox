//! Human-readable byte-size parsing, shared across config modules.
//!
//! Accepts a number (bytes) or a string like `"8mb"` / `"256kb"` / `"1gb"`. Used by
//! [`crate::config`] (engine limits) and [`crate::s3`] (`max_upload_size`) so the
//! `"25mb"` syntax means the same thing everywhere.

use std::fmt;

use serde::Deserializer;
use serde::de::{self, Visitor};

/// Parses a byte size from a human-readable string (integer math, no floats).
///
/// Accepted: `"8mb"`, `"256kb"`, `"1gb"`, `"8 MB"`, `"4096"`.
fn parse_byte_size(input: &str) -> Result<usize, String> {
    let lower = input.trim().to_lowercase();

    // Find where the unit starts.
    let boundary = lower
        .find(|ch: char| ch.is_alphabetic())
        .unwrap_or(lower.len());
    let (num_str, unit_str) = lower.split_at(boundary);

    let number: usize = num_str.trim().parse().map_err(|_err| {
        if num_str.contains('.') {
            format!("decimal values are not supported for byte sizes: \"{input}\" — use whole numbers (e.g. \"1536kb\" instead of \"1.5mb\")")
        } else {
            format!("invalid number in byte size: \"{input}\"")
        }
    })?;

    let multiplier: usize = match unit_str.trim() {
        "" | "b" => 1,
        "kb" | "k" => 1024,
        "mb" | "m" => 1024 * 1024,
        "gb" | "g" => 1024 * 1024 * 1024,
        other => return Err(format!("unknown size unit '{other}' in: {input}")),
    };

    number
        .checked_mul(multiplier)
        .ok_or_else(|| format!("byte size overflow: {input}"))
}

/// Serde visitor that accepts either a number or a string like `"8mb"`.
struct ByteSizeVisitor;

#[expect(
    clippy::missing_trait_methods,
    reason = "serde Visitor has 20+ default methods — only the relevant ones are overridden"
)]
impl Visitor<'_> for ByteSizeVisitor {
    type Value = usize;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a byte size as a number (8388608) or string (\"8mb\", \"256kb\")")
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
        usize::try_from(v).map_err(de::Error::custom)
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
        usize::try_from(v).map_err(de::Error::custom)
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        parse_byte_size(v).map_err(de::Error::custom)
    }
}

/// Deserializes a byte size field — accepts `"8mb"`, `"256kb"`, or plain numbers.
///
/// # Errors
///
/// Returns an error if the value is neither a non-negative integer nor a valid
/// human-readable size string.
pub(crate) fn deserialize_byte_size<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<usize, D::Error> {
    deserializer.deserialize_any(ByteSizeVisitor)
}
