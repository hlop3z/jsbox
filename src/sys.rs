//! `$sys` — the always-on runtime standard library for the sandbox.
//!
//! One `$`-prefixed global grouping pure, zero-I/O helpers: `$sys.crypto`
//! (hashing, HMAC, UUID, encoding) and `$sys.date` (parse, `timedelta` math,
//! diff, formatting). Pure like `$`/Decimal — always injected, no config, no
//! per-op metering.
//!
//! FFI: every op crosses the boundary as `__sys(domain, op, payload_json)` and
//! returns `{"v": <result>}` on success or `{"error": <message>}` on failure
//! (mirroring `decimal.rs`). Errors throw a plain JS `Error` in the wrapper, so
//! they classify as developer/script errors — no `__jsbox` capability tag is
//! needed (these helpers do no I/O).
//!
//! ## Secrets are use-not-extract — the hard multi-tenant guarantee
//!
//! Tenants run arbitrary JS, so a secret must be **usable** without being
//! **extractable**. The guarantee is structural, not heuristic:
//!
//! 1. **Plaintext never enters the JS heap.** Secret values stay in this Rust
//!    module's per-request [`SecretStore`]; the JS side receives only **opaque
//!    handles** carrying the secret's *name* (see `js/sys.js`). There is no JS
//!    value a script can coerce, slice, or stringify back into the plaintext —
//!    every coercion (`String`, template, `JSON.stringify`, `valueOf`) yields
//!    `"[secret:NAME]"`.
//! 2. **The only op that resolves a handle is HMAC, in the key position.** A
//!    handle reaches plaintext solely via `crypto.hmac({key_ref})`, whose output
//!    is a one-way digest. No native op returns, encodes, or echoes plaintext;
//!    encode/hash/url ops reject handles outright (in JS).
//!
//! That makes extraction impossible *by construction*: there is no decode path to
//! filter, because the plaintext is never present in any JS value to begin with. (We
//! deliberately keep **no** output-redaction fallback — a scan only catches
//! un-transformed values, so it would be evadable security theater, not a guarantee.)
//!
//! *Honest caveat:* HMAC of a **low-entropy** secret is offline-brute-forceable
//! by anyone who can call `hmac` — inherent to HMAC, not a leak here. Secrets
//! must be high-entropy.

use std::collections::HashMap;
use std::error::Error;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use hmac::{Hmac, Mac};
use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use rquickjs::{Ctx, Function, Object, Value as JsValue};
use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256, Sha512};
use uuid::Uuid;

use crate::sandbox;

/// Per-request `$sys` context: operator-supplied env + secrets (both opt-in).
///
/// `env` values are returnable plain config; `secrets` plaintext never reaches JS —
/// it stays Rust-side in the [`SecretStore`] and surfaces only as opaque handles.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct SysConfig {
    /// Plain config values, exposed (and returnable) at `$sys.env`.
    #[serde(default)]
    pub(crate) env: Map<String, Value>,
    /// Secret values: plaintext kept Rust-side; `$sys.secrets` exposes opaque handles.
    #[serde(default)]
    pub(crate) secrets: Map<String, Value>,
}

impl SysConfig {
    /// Per-request name→plaintext map, kept Rust-side and resolved only by HMAC.
    /// Non-string secrets are skipped — a credential is always a string.
    fn secret_store(&self) -> SecretStore {
        self.secrets
            .iter()
            .filter_map(|(name, val)| val.as_str().map(|plain| (name.clone(), plain.to_owned())))
            .collect()
    }

    /// The configured secret names (string-valued only) — the only thing the JS
    /// side learns; it builds an opaque handle per name (no plaintext crosses).
    fn secret_names(&self) -> Vec<&str> {
        self.secrets
            .iter()
            .filter(|(_name, val)| val.is_string())
            .map(|(name, _val)| name.as_str())
            .collect()
    }
}

/// Per-request secret plaintext, indexed by name. Lives only in Rust (captured by
/// the `__sys` native closure); the JS side never sees a value, only opaque handles.
type SecretStore = HashMap<String, String>;

/// JS wrapper — loaded from `src/js/sys.js` at compile time.
const SYS_WRAPPER: &str = include_str!("js/sys.js");

/// Milliseconds in one second.
const MILLIS_PER_SECOND: i64 = 1000;
/// Seconds in one minute.
const SECONDS_PER_MINUTE: u64 = 60;
/// Seconds in one hour.
const SECONDS_PER_HOUR: u64 = 3600;
/// Seconds in one day.
const SECONDS_PER_DAY: u64 = 86_400;

/// Injects the `$sys` global. The pure helpers (`crypto`/`date`) are always on; the
/// `env`/`secrets` context is populated only when `sys_config` is present (opt-in).
///
/// # Errors
///
/// Returns an error if registration or JS eval fails.
pub(crate) fn inject_sys(
    qctx: &Ctx<'_>,
    sys_config: Option<&SysConfig>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    // The secret plaintext lives here, captured by the native closure, and is
    // resolved only by HMAC — it never crosses into JS (see module docs).
    let secrets = sys_config.map(SysConfig::secret_store).unwrap_or_default();

    let sys_fn = Function::new(
        qctx.clone(),
        move |domain: String, op: String, payload: String| -> String {
            match dispatch(&domain, &op, &payload, &secrets) {
                Ok(value) => value_json(&value),
                Err(err) => sandbox::error_json(&err),
            }
        },
    )?
    .with_name("__sys")?;

    qctx.globals().set("__sys", sys_fn)?;

    let wrapper: JsValue<'_> = qctx.eval(SYS_WRAPPER)?;
    drop(wrapper);

    if let Some(cfg) = sys_config {
        inject_context(qctx, cfg)?;
    }
    Ok(())
}

/// Populates `$sys.env` (plain, returnable values) and `$sys.secrets` (opaque
/// handles) from operator config. Crucially, **no secret plaintext is set on any
/// JS value** — only the secret *names* are sent, and the JS wrapper turns each
/// into a frozen handle whose plaintext is reachable solely via Rust-side HMAC.
fn inject_context(qctx: &Ctx<'_>, cfg: &SysConfig) -> Result<(), Box<dyn Error + Send + Sync>> {
    let sys_obj: Object<'_> = qctx.globals().get("$sys")?;
    let env_val: JsValue<'_> = qctx.json_parse(serde_json::to_string(&cfg.env)?)?;
    sys_obj.set("env", env_val)?;

    // Build the handle map from names only — `__sysMakeSecrets` is defined by the
    // wrapper and returns a frozen object of opaque handles (no plaintext).
    let names_json = serde_json::to_string(&cfg.secret_names())?;
    let snippet = format!("$sys.secrets = globalThis.__sysMakeSecrets({names_json});");
    let built: JsValue<'_> = qctx.eval(snippet)?;
    drop(built);
    Ok(())
}

// -- Dispatch ---------------------------------------------------------------

/// Routes a `__sys` call to the right domain handler.
fn dispatch(domain: &str, op: &str, payload: &str, secrets: &SecretStore) -> Result<Value, String> {
    let parsed: Value =
        serde_json::from_str(payload).map_err(|err| format!("invalid payload: {err}"))?;
    match domain {
        "crypto" => crypto_dispatch(op, &parsed, secrets),
        "date" => date_dispatch(op, &parsed),
        other => Err(format!("unknown sys domain: '{other}'")),
    }
}

/// Wraps a result value in the `{"v": ...}` success envelope.
fn value_json(value: &Value) -> String {
    match serde_json::to_string(value) {
        Ok(inner) => format!("{{\"v\":{inner}}}"),
        Err(_err) => "{\"error\":\"failed to encode result\"}".to_owned(),
    }
}

// -- Payload helpers --------------------------------------------------------

/// Reads a required string field from the payload object.
fn field_str<'a>(payload: &'a Value, key: &str) -> Result<&'a str, String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string field '{key}'"))
}

/// Reads a required integer field from the payload object.
fn field_i64(payload: &Value, key: &str) -> Result<i64, String> {
    payload
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("missing integer field '{key}'"))
}

// -- crypto -----------------------------------------------------------------

/// Routes a `$sys.crypto` op.
fn crypto_dispatch(op: &str, payload: &Value, secrets: &SecretStore) -> Result<Value, String> {
    match op {
        "sha256" => Ok(Value::String(sha256_hex(field_str(payload, "data")?))),
        "sha512" => Ok(Value::String(sha512_hex(field_str(payload, "data")?))),
        "hmac" => hmac_op(payload, secrets),
        "uuid" => Ok(Value::String(Uuid::new_v4().to_string())),
        "base64_encode" | "base64_decode" | "base64url_encode" | "base64url_decode"
        | "hex_encode" | "hex_decode" | "url_encode" | "url_decode" => {
            encoding_dispatch(op, payload)
        }
        other => Err(format!("unknown crypto op: '{other}'")),
    }
}

/// SHA-256 of a UTF-8 string, hex-encoded.
fn sha256_hex(data: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data.as_bytes());
    hex::encode(hasher.finalize())
}

/// SHA-512 of a UTF-8 string, hex-encoded.
fn sha512_hex(data: &str) -> String {
    let mut hasher = Sha512::new();
    hasher.update(data.as_bytes());
    hex::encode(hasher.finalize())
}

/// HMAC over `{algo, key|key_ref, msg, encoding}` → encoded digest.
///
/// The key is either a script-supplied `key` string or a `key_ref` naming an
/// operator secret resolved from [`SecretStore`] — the *only* path by which secret
/// plaintext is read, and a one-way one (the output is a digest, never the key).
fn hmac_op(payload: &Value, secrets: &SecretStore) -> Result<Value, String> {
    let algo = field_str(payload, "algo")?;
    let key = resolve_key(payload, secrets)?;
    let msg = field_str(payload, "msg")?;
    let encoding = payload
        .get("encoding")
        .and_then(Value::as_str)
        .unwrap_or("hex");
    let bytes = match algo {
        "sha256" => hmac_sha256(key.as_bytes(), msg.as_bytes())?,
        "sha512" => hmac_sha512(key.as_bytes(), msg.as_bytes())?,
        other => return Err(format!("unsupported hmac algorithm: '{other}'")),
    };
    Ok(Value::String(encode_bytes(&bytes, encoding)?))
}

/// Resolves the HMAC key: a `key_ref` looks up secret plaintext (Rust-only), else a
/// plain `key` string is used. The resolved plaintext is returned to the caller for
/// immediate one-way hashing and never serialized back across the FFI boundary.
fn resolve_key(payload: &Value, secrets: &SecretStore) -> Result<String, String> {
    if let Some(name) = payload.get("key_ref").and_then(Value::as_str) {
        return secrets
            .get(name)
            .cloned()
            .ok_or_else(|| format!("unknown secret: '{name}'"));
    }
    Ok(field_str(payload, "key")?.to_owned())
}

/// HMAC-SHA-256 raw bytes.
fn hmac_sha256(key: &[u8], msg: &[u8]) -> Result<Vec<u8>, String> {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(key).map_err(|err| format!("hmac key error: {err}"))?;
    mac.update(msg);
    Ok(mac.finalize().into_bytes().to_vec())
}

/// HMAC-SHA-512 raw bytes.
fn hmac_sha512(key: &[u8], msg: &[u8]) -> Result<Vec<u8>, String> {
    let mut mac =
        Hmac::<Sha512>::new_from_slice(key).map_err(|err| format!("hmac key error: {err}"))?;
    mac.update(msg);
    Ok(mac.finalize().into_bytes().to_vec())
}

/// Encodes raw bytes as `hex` / `base64` / `base64url`.
fn encode_bytes(bytes: &[u8], encoding: &str) -> Result<String, String> {
    match encoding {
        "hex" => Ok(hex::encode(bytes)),
        "base64" => Ok(STANDARD.encode(bytes)),
        "base64url" => Ok(URL_SAFE_NO_PAD.encode(bytes)),
        other => Err(format!("unsupported encoding: '{other}'")),
    }
}

/// Routes the encode/decode ops (`base64`/`base64url`/`hex`/`url`).
fn encoding_dispatch(op: &str, payload: &Value) -> Result<Value, String> {
    let data = field_str(payload, "data")?;
    match op {
        "base64_encode" => Ok(Value::String(STANDARD.encode(data.as_bytes()))),
        "base64_decode" => decode_utf8(&base64_decode(&STANDARD, data)?),
        "base64url_encode" => Ok(Value::String(URL_SAFE_NO_PAD.encode(data.as_bytes()))),
        "base64url_decode" => decode_utf8(&base64_decode(&URL_SAFE_NO_PAD, data)?),
        "hex_encode" => Ok(Value::String(hex::encode(data.as_bytes()))),
        "hex_decode" => {
            decode_utf8(&hex::decode(data).map_err(|err| format!("invalid hex: {err}"))?)
        }
        "url_encode" => Ok(Value::String(
            utf8_percent_encode(data, NON_ALPHANUMERIC).to_string(),
        )),
        "url_decode" => url_decode(data),
        other => Err(format!("unknown encoding op: '{other}'")),
    }
}

/// Decodes a base64 string with the given engine.
fn base64_decode<E: Engine>(engine: &E, data: &str) -> Result<Vec<u8>, String> {
    engine
        .decode(data)
        .map_err(|err| format!("invalid base64: {err}"))
}

/// Turns decoded bytes back into a UTF-8 string, erroring on non-text input.
fn decode_utf8(bytes: &[u8]) -> Result<Value, String> {
    String::from_utf8(bytes.to_vec())
        .map(Value::String)
        .map_err(|err| format!("decoded bytes are not valid utf-8: {err}"))
}

/// Percent-decodes a string back to UTF-8.
fn url_decode(data: &str) -> Result<Value, String> {
    percent_decode_str(data)
        .decode_utf8()
        .map(|decoded| Value::String(decoded.into_owned()))
        .map_err(|err| format!("invalid percent-encoding: {err}"))
}

// -- date -------------------------------------------------------------------

/// Routes a `$sys.date` op.
fn date_dispatch(op: &str, payload: &Value) -> Result<Value, String> {
    match op {
        "now" => Ok(Value::from(now_millis()?)),
        "parse" => date_parse(payload),
        "add" => date_add(payload),
        "diff" => date_diff(payload),
        "iso" => date_iso(payload),
        "unix" => date_unix(payload),
        other => Err(format!("unknown date op: '{other}'")),
    }
}

/// Current wall-clock instant as epoch milliseconds.
fn now_millis() -> Result<i64, String> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| format!("system clock error: {err}"))?;
    i64::try_from(elapsed.as_millis()).map_err(|err| format!("timestamp overflow: {err}"))
}

/// Parses the `input` field (ISO string, date-only, or epoch millis) → epoch millis.
fn date_parse(payload: &Value) -> Result<Value, String> {
    let input = payload
        .get("input")
        .ok_or_else(|| "missing 'input'".to_owned())?;
    Ok(Value::from(parse_input(input)?))
}

/// Resolves a JSON value (string or number) to epoch milliseconds.
fn parse_input(input: &Value) -> Result<i64, String> {
    match input {
        Value::Number(num) => num
            .as_i64()
            .ok_or_else(|| "epoch millis must be an integer".to_owned()),
        Value::String(text) => parse_date_str(text),
        Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_) => {
            Err("date input must be an ISO string or epoch millis".to_owned())
        }
    }
}

/// Parses an RFC 3339 timestamp or a date-only `YYYY-MM-DD` → epoch milliseconds (UTC).
fn parse_date_str(text: &str) -> Result<i64, String> {
    let trimmed = text.trim();
    if let Ok(parsed) = DateTime::parse_from_rfc3339(trimmed) {
        return Ok(parsed.timestamp_millis());
    }
    if let Ok(date) = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        let naive = date
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| "invalid date".to_owned())?;
        return Ok(naive.and_utc().timestamp_millis());
    }
    Err(format!("cannot parse date: '{text}'"))
}

/// Adds a `delta_ms` offset to a `ms` instant (checked).
fn date_add(payload: &Value) -> Result<Value, String> {
    let base = field_i64(payload, "ms")?;
    let delta = field_i64(payload, "delta_ms")?;
    let out = base
        .checked_add(delta)
        .ok_or_else(|| "date arithmetic overflow".to_owned())?;
    Ok(Value::from(out))
}

/// Formats a `ms` instant as an RFC 3339 string (`Z`, auto sub-second precision).
fn date_iso(payload: &Value) -> Result<Value, String> {
    let millis = field_i64(payload, "ms")?;
    let parsed = DateTime::<Utc>::from_timestamp_millis(millis)
        .ok_or_else(|| "timestamp out of range".to_owned())?;
    Ok(Value::String(
        parsed.to_rfc3339_opts(SecondsFormat::AutoSi, true),
    ))
}

/// Converts a `ms` instant to epoch seconds.
fn date_unix(payload: &Value) -> Result<Value, String> {
    let millis = field_i64(payload, "ms")?;
    let secs = millis
        .checked_div(MILLIS_PER_SECOND)
        .ok_or_else(|| "timestamp overflow".to_owned())?;
    Ok(Value::from(secs))
}

/// Computes `a - b` and breaks the gap into days/hours/minutes/seconds.
fn date_diff(payload: &Value) -> Result<Value, String> {
    let first = field_i64(payload, "a")?;
    let second = field_i64(payload, "b")?;
    let total_ms = first
        .checked_sub(second)
        .ok_or_else(|| "date diff overflow".to_owned())?;
    let total_seconds = total_ms
        .checked_div(MILLIS_PER_SECOND)
        .ok_or_else(|| "timestamp overflow".to_owned())?;
    let (days, hours, minutes, seconds) = split_duration(total_seconds.unsigned_abs())?;
    Ok(serde_json::json!({
        "total_ms": total_ms,
        "total_seconds": total_seconds,
        "days": days,
        "hours": hours,
        "minutes": minutes,
        "seconds": seconds,
    }))
}

/// Breaks an absolute second count into (days, hours, minutes, seconds).
fn split_duration(total_seconds: u64) -> Result<(u64, u64, u64, u64), String> {
    let (days, after_days) = divmod(total_seconds, SECONDS_PER_DAY)?;
    let (hours, after_hours) = divmod(after_days, SECONDS_PER_HOUR)?;
    let (minutes, seconds) = divmod(after_hours, SECONDS_PER_MINUTE)?;
    Ok((days, hours, minutes, seconds))
}

/// Checked `(value / unit, value % unit)`.
fn divmod(value: u64, unit: u64) -> Result<(u64, u64), String> {
    let quot = value
        .checked_div(unit)
        .ok_or_else(|| "duration overflow".to_owned())?;
    let rem = value
        .checked_rem(unit)
        .ok_or_else(|| "duration overflow".to_owned())?;
    Ok((quot, rem))
}
