//! S3 presigned-URL generator for the `QuickJS` sandbox (`s3`).
//!
//! JS API: `s3.presignPut({ key, expires? })` / `s3.presignGet({ key, expires? })`
//! (also `s3.presign({ method, key, expires? })`).
//!
//! The server **never connects** to the object store: it computes an AWS `SigV4`
//! presigned URL and returns it to the script, which hands it to the frontend for a
//! direct browser upload/download. Signing itself is pure crypto.
//!
//! Endpoint + credentials are operator-supplied in `config.s3`. The endpoint host is
//! put through the **same SSRF guard as `http`** ([`crate::ssrf`]): non-`http(s)`
//! schemes are rejected and localhost / private / internal addresses are blocked, so
//! a presigned URL can never name a local or internal target (one DNS lookup per
//! sign resolves the host). Works with any public `SigV4` store: AWS S3, Cloudflare
//! R2, Backblaze B2, `MinIO` reachable on a public address, ...

use std::error::Error;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use rquickjs::{Ctx, Function, Value as JsValue};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::bytesize::deserialize_byte_size;
use crate::sandbox::{self, Collector};
use crate::ssrf;

/// JS wrapper — loaded from `src/js/s3.js` at compile time.
const S3_WRAPPER: &str = include_str!("js/s3.js");

/// `SigV4` service name for S3.
const SERVICE: &str = "s3";

/// HMAC-SHA256 alias used throughout `SigV4` signing.
type HmacSha256 = Hmac<Sha256>;

/// Encoding set for a single path/query token: escape everything non-alphanumeric
/// except the RFC3986 unreserved marks `- _ . ~`.
const SEGMENT_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// Like [`SEGMENT_SET`] but also leaves `/` unescaped — for object-key paths.
const PATH_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~')
    .remove(b'/');

/// Per-request S3 configuration (operator-supplied, like `db`/`mail`).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct S3Config {
    /// Endpoint URL including scheme, e.g. `https://s3.us-east-1.amazonaws.com`
    /// or `http://localhost:9000` (`MinIO`). The bucket is added per addressing mode.
    pub(crate) endpoint: String,
    /// `SigV4` region scope (e.g. `us-east-1`; Cloudflare R2 uses `auto`).
    pub(crate) region: String,
    /// Bucket name.
    pub(crate) bucket: String,
    /// Access key id.
    pub(crate) access_key: String,
    /// Secret access key.
    pub(crate) secret_key: String,
    /// Path-style addressing (`host/bucket/key`). Default `false` = virtual-hosted
    /// (`bucket.host/key`). `MinIO` and most self-hosted stores need `true`.
    #[serde(default)]
    pub(crate) path_style: bool,
    /// Default link lifetime in seconds when a call omits `expires`.
    #[serde(default = "default_expires")]
    pub(crate) expires: u64,
    /// Hard cap on link lifetime in seconds (`SigV4` max is 604800 = 7 days).
    #[serde(default = "default_max_expires")]
    pub(crate) max_expires: u64,
    /// Maximum upload size for `presignPost`, human-readable (`"25mb"`, `"50gb"`, or
    /// bytes). Operator-supplied — the script can never raise or set it. Required for
    /// `presignPost` (0 = unset → `presignPost` errors). Unused by `presignPut`/`Get`.
    #[serde(default, deserialize_with = "deserialize_byte_size")]
    pub(crate) max_upload_size: usize,
}

/// Default presigned-link lifetime in seconds (15 minutes).
const fn default_expires() -> u64 { 900 }
/// Default lifetime cap in seconds (`SigV4` maximum, 7 days).
const fn default_max_expires() -> u64 { 604_800 }

/// Metric recorded for each S3 operation.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct S3Metric {
    /// Operation type (`presign` or `presign_post`).
    action: String,
    /// HTTP method the URL is signed for.
    method: String,
    /// Duration in microseconds.
    duration_us: u128,
    /// Link lifetime in seconds.
    expires: u64,
}

/// Parsed payload for a `presign` operation.
#[derive(Debug, Deserialize)]
struct PresignPayload {
    /// HTTP method (empty = `PUT`).
    #[serde(default)]
    method: String,
    /// Object key (path within the bucket).
    #[serde(default)]
    key: String,
    /// Requested lifetime in seconds (0 = use the configured default).
    #[serde(default)]
    expires: u64,
}

/// Parsed payload for a `presign_post` operation.
///
/// Note: there is **no** size field — the upload cap comes only from
/// `config.s3.max_upload_size`, never from the (untrusted) script payload.
#[derive(Debug, Deserialize)]
struct PresignPostPayload {
    /// Object key (path within the bucket).
    #[serde(default)]
    key: String,
    /// Requested lifetime in seconds (0 = use the configured default).
    #[serde(default)]
    expires: u64,
}

/// Successful presign result plus the stats needed to build a metric.
#[derive(Debug)]
struct PresignOutcome {
    /// JSON returned to JS.
    json: String,
    /// Signed HTTP method (for the metric).
    method: String,
    /// Effective lifetime in seconds (for the metric).
    expires: u64,
}

// -- Public API -------------------------------------------------------------

/// Injects the `s3` global. Returns a metrics collector.
///
/// # Errors
///
/// Returns an error if function registration or JS eval fails.
pub(crate) fn inject_s3(
    qctx: &Ctx<'_>,
    config: &S3Config,
    max_ops: usize,
    allow_private: bool,
) -> Result<Collector<S3Metric>, Box<dyn Error + Send + Sync>> {
    let owned = config.clone();

    let metrics: Collector<S3Metric> = sandbox::new_collector();
    let metrics_clone = Arc::clone(&metrics);

    let s3_fn = Function::new(
        qctx.clone(),
        move |action: String, payload_json: String| -> String {
            if let Err(err) = sandbox::check_op_limit(&metrics_clone, max_ops) {
                return sandbox::error_json(&err);
            }

            let start = Instant::now();
            let result = dispatch(&owned, &action, &payload_json, allow_private);
            let metric = build_metric(&action, result.as_ref().ok(), start);
            sandbox::record(&metrics_clone, metric);

            match result {
                Ok(outcome) => outcome.json,
                Err(err) => sandbox::error_json(&err),
            }
        },
    )?
    .with_name("__s3")?;

    qctx.globals().set("__s3", s3_fn)?;

    let wrapper: JsValue<'_> = qctx.eval(S3_WRAPPER)?;
    drop(wrapper);

    Ok(metrics)
}

// -- Dispatch ---------------------------------------------------------------

/// Routes a `__s3` call to the correct handler.
fn dispatch(
    config: &S3Config,
    action: &str,
    payload_json: &str,
    allow_private: bool,
) -> Result<PresignOutcome, String> {
    match action {
        "presign" => do_presign(config, payload_json, allow_private),
        "presign_post" => do_presign_post(config, payload_json, allow_private),
        other => Err(format!("unknown s3 action: {other}")),
    }
}

// -- Presign ----------------------------------------------------------------

/// Builds a `SigV4` presigned URL for one object operation.
fn do_presign(
    config: &S3Config,
    payload_json: &str,
    allow_private: bool,
) -> Result<PresignOutcome, String> {
    let payload: PresignPayload =
        serde_json::from_str(payload_json).map_err(|err| format!("invalid s3 payload: {err}"))?;

    let method = normalize_method(&payload.method)?;
    if payload.key.trim().is_empty() {
        return Err("s3 presign requires a non-empty key".to_owned());
    }
    let expires = clamp_expires(payload.expires, config);

    let (amz_date, datestamp, _now_secs) = current_timestamps()?;
    let (scheme, host) = resolve_host(config, allow_private)?;
    let canonical_uri = build_uri(config, &payload.key);
    let scope = format!("{datestamp}/{}/{SERVICE}/aws4_request", config.region);
    let query = build_canonical_query(&config.access_key, &scope, &amz_date, expires);

    let signature = sign(&SignInput {
        secret_key: &config.secret_key,
        region: &config.region,
        datestamp: &datestamp,
        amz_date: &amz_date,
        scope: &scope,
        method,
        canonical_uri: &canonical_uri,
        query: &query,
        host: &host,
    })?;

    let url = format!("{scheme}://{host}{canonical_uri}?{query}&X-Amz-Signature={signature}");
    let escaped_url =
        serde_json::to_string(&url).map_err(|err| format!("failed to encode url: {err}"))?;
    let json = format!("{{\"url\":{escaped_url},\"method\":\"{method}\",\"expires\":{expires}}}");

    Ok(PresignOutcome { json, method: method.to_owned(), expires })
}

// -- Presign POST (browser form upload with size policy) --------------------

/// Builds a `SigV4` presigned POST policy for a direct browser form upload.
///
/// The policy's `content-length-range` condition is enforced by the object store,
/// which rejects an upload larger than `config.max_upload_size`. The size cap is
/// **config-only** — the script supplies just the key, never a size.
fn do_presign_post(
    config: &S3Config,
    payload_json: &str,
    allow_private: bool,
) -> Result<PresignOutcome, String> {
    let payload: PresignPostPayload =
        serde_json::from_str(payload_json).map_err(|err| format!("invalid s3 payload: {err}"))?;

    if payload.key.trim().is_empty() {
        return Err("s3 presignPost requires a non-empty key".to_owned());
    }
    let max_bytes = config.max_upload_size;
    if max_bytes == 0 {
        return Err("config.s3.max_upload_size is required for presignPost".to_owned());
    }
    let expires = clamp_expires(payload.expires, config);

    let (amz_date, datestamp, now_secs) = current_timestamps()?;
    let (scheme, host) = resolve_host(config, allow_private)?;
    let expiration = iso8601_expiration(now_secs, expires)?;
    let credential =
        format!("{}/{datestamp}/{}/{SERVICE}/aws4_request", config.access_key, config.region);

    let policy = json!({
        "expiration": expiration,
        "conditions": [
            {"bucket": config.bucket},
            {"key": payload.key},
            {"x-amz-algorithm": "AWS4-HMAC-SHA256"},
            {"x-amz-credential": credential},
            {"x-amz-date": amz_date},
            ["content-length-range", 0, max_bytes],
        ],
    });
    let policy_str =
        serde_json::to_string(&policy).map_err(|err| format!("failed to encode policy: {err}"))?;
    let policy_b64 = BASE64.encode(policy_str);

    let signing_key = derive_signing_key(&config.secret_key, &datestamp, &config.region)?;
    let signature = hex::encode(hmac_sha256(&signing_key, policy_b64.as_bytes())?);

    let response = json!({
        "url": post_url(&scheme, &host, config),
        "fields": {
            "key": payload.key,
            "X-Amz-Algorithm": "AWS4-HMAC-SHA256",
            "X-Amz-Credential": credential,
            "X-Amz-Date": amz_date,
            "Policy": policy_b64,
            "X-Amz-Signature": signature,
        },
        "maxBytes": max_bytes,
        "expires": expires,
    });
    let json_out = serde_json::to_string(&response)
        .map_err(|err| format!("failed to encode response: {err}"))?;

    Ok(PresignOutcome { json: json_out, method: "POST".to_owned(), expires })
}

/// Builds the POST target URL for the configured addressing mode.
///
/// Virtual-hosted: `scheme://{bucket.host}/`. Path-style: `scheme://{host}/{bucket}`.
fn post_url(scheme: &str, host: &str, config: &S3Config) -> String {
    if config.path_style {
        let bucket = utf8_percent_encode(&config.bucket, SEGMENT_SET);
        format!("{scheme}://{host}/{bucket}")
    } else {
        format!("{scheme}://{host}/")
    }
}

/// Inputs to the `SigV4` signing step (grouped to keep the arg count low).
struct SignInput<'a> {
    /// Secret access key.
    secret_key: &'a str,
    /// `SigV4` region scope.
    region: &'a str,
    /// `YYYYMMDD` date stamp.
    datestamp: &'a str,
    /// `YYYYMMDDTHHMMSSZ` timestamp.
    amz_date: &'a str,
    /// Credential scope (`datestamp/region/s3/aws4_request`).
    scope: &'a str,
    /// HTTP method.
    method: &'a str,
    /// Encoded canonical URI path.
    canonical_uri: &'a str,
    /// Encoded canonical query string (without the signature).
    query: &'a str,
    /// Host header value (matches the URL authority).
    host: &'a str,
}

/// Computes the lowercase-hex `SigV4` signature for a presigned request.
fn sign(input: &SignInput<'_>) -> Result<String, String> {
    let canonical_request = format!(
        "{}\n{}\n{}\nhost:{}\n\nhost\nUNSIGNED-PAYLOAD",
        input.method, input.canonical_uri, input.query, input.host
    );
    let hashed_request = sha256_hex(canonical_request.as_bytes());
    let string_to_sign =
        format!("AWS4-HMAC-SHA256\n{}\n{}\n{hashed_request}", input.amz_date, input.scope);

    let signing_key = derive_signing_key(input.secret_key, input.datestamp, input.region)?;
    let signature_bytes = hmac_sha256(&signing_key, string_to_sign.as_bytes())?;
    Ok(hex::encode(signature_bytes))
}

// -- Inputs / validation ----------------------------------------------------

/// Normalizes and validates the HTTP method, defaulting empty to `PUT`.
fn normalize_method(raw: &str) -> Result<&'static str, String> {
    match raw.to_ascii_uppercase().as_str() {
        "" | "PUT" => Ok("PUT"),
        "GET" => Ok("GET"),
        "HEAD" => Ok("HEAD"),
        "DELETE" => Ok("DELETE"),
        other => Err(format!("unsupported s3 method: {other}")),
    }
}

/// Resolves the requested lifetime to `[1, max_expires]`, defaulting `0`.
fn clamp_expires(requested: u64, config: &S3Config) -> u64 {
    let base = if requested == 0 { config.expires } else { requested };
    base.clamp(1, config.max_expires.max(1))
}

// -- URL / host construction ------------------------------------------------

/// Splits the endpoint into `(scheme, host)` for the configured addressing mode.
///
/// Virtual-hosted: `bucket.host[:port]`. Path-style: `host[:port]` unchanged.
fn resolve_host(config: &S3Config, allow_private: bool) -> Result<(String, String), String> {
    let (scheme, authority) = config
        .endpoint
        .split_once("://")
        .ok_or_else(|| "s3 endpoint must include a scheme (http:// or https://)".to_owned())?;

    // Only HTTP object stores are valid targets — never `file://` or any other scheme.
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Err(format!("s3 endpoint scheme must be http or https, got '{scheme}'"));
    }

    let authority_clean = authority.split('/').next().unwrap_or(authority);
    if authority_clean.is_empty() {
        return Err("s3 endpoint host is empty".to_owned());
    }

    let (host_only, port_suffix) = split_host_port(authority_clean);

    // SSRF guard — identical to `http`: reject localhost and any private/internal
    // address so a presigned URL can never name a local/internal target (relaxed only
    // in `debug` mode). Resolves the host (one DNS lookup); literal IPs need no DNS.
    let port = endpoint_port(scheme, &port_suffix)?;
    let bare_host = host_only.trim_start_matches('[').trim_end_matches(']');
    ssrf::block_private_ip(bare_host, port, allow_private)?;

    if config.path_style {
        Ok((scheme.to_owned(), authority_clean.to_owned()))
    } else {
        Ok((scheme.to_owned(), format!("{}.{host_only}{port_suffix}", config.bucket)))
    }
}

/// Splits an authority into `(host, ":port")`; the suffix is empty when absent.
///
/// Only a trailing all-digit segment after the final `:` counts as a port, so
/// bracketed IPv6 literals (`[::1]`) are not mis-split.
fn split_host_port(authority: &str) -> (&str, String) {
    if let Some((host_part, port_part)) = authority.rsplit_once(':')
        && !port_part.is_empty()
        && port_part.bytes().all(|byte| byte.is_ascii_digit())
    {
        return (host_part, format!(":{port_part}"));
    }
    (authority, String::new())
}

/// Resolves the effective port from a `:port` suffix or the scheme default.
fn endpoint_port(scheme: &str, port_suffix: &str) -> Result<u16, String> {
    if let Some(port_str) = port_suffix.strip_prefix(':') {
        return port_str
            .parse::<u16>()
            .map_err(|_err| format!("invalid s3 endpoint port '{port_str}'"));
    }
    Ok(if scheme.eq_ignore_ascii_case("https") { 443 } else { 80 })
}

/// Builds the encoded canonical URI path for the configured addressing mode.
fn build_uri(config: &S3Config, key: &str) -> String {
    let key_path = uri_encode_path(key);
    if config.path_style {
        let bucket = utf8_percent_encode(&config.bucket, SEGMENT_SET).to_string();
        format!("/{bucket}{key_path}")
    } else {
        key_path
    }
}

/// Percent-encodes an object key, preserving `/`, with a leading slash.
fn uri_encode_path(key: &str) -> String {
    let trimmed = key.strip_prefix('/').unwrap_or(key);
    let encoded = utf8_percent_encode(trimmed, PATH_SET).to_string();
    format!("/{encoded}")
}

/// Builds the sorted canonical query string (without the signature).
fn build_canonical_query(access_key: &str, scope: &str, amz_date: &str, expires: u64) -> String {
    let credential = encode_token(&format!("{access_key}/{scope}"));
    let date = encode_token(amz_date);
    // Keys are emitted in the `SigV4`-required sorted order.
    format!(
        "X-Amz-Algorithm=AWS4-HMAC-SHA256\
         &X-Amz-Credential={credential}\
         &X-Amz-Date={date}\
         &X-Amz-Expires={expires}\
         &X-Amz-SignedHeaders=host"
    )
}

/// Percent-encodes a single query token (escapes `/` as `%2F`).
fn encode_token(value: &str) -> String {
    utf8_percent_encode(value, SEGMENT_SET).to_string()
}

// -- Crypto primitives ------------------------------------------------------

/// Derives the `SigV4` signing key (the `kDate→kRegion→kService→kSigning` chain).
fn derive_signing_key(secret_key: &str, datestamp: &str, region: &str) -> Result<Vec<u8>, String> {
    let initial = format!("AWS4{secret_key}");
    let k_date = hmac_sha256(initial.as_bytes(), datestamp.as_bytes())?;
    let k_region = hmac_sha256(&k_date, region.as_bytes())?;
    let k_service = hmac_sha256(&k_region, SERVICE.as_bytes())?;
    hmac_sha256(&k_service, b"aws4_request")
}

/// Computes `HMAC-SHA256(key, data)` as raw bytes.
fn hmac_sha256(key: &[u8], data: &[u8]) -> Result<Vec<u8>, String> {
    let mut mac =
        HmacSha256::new_from_slice(key).map_err(|err| format!("hmac key error: {err}"))?;
    mac.update(data);
    Ok(mac.finalize().into_bytes().to_vec())
}

/// Computes the lowercase-hex SHA-256 digest of `data`.
fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

// -- Time -------------------------------------------------------------------

/// Returns the current UTC `(amz_date, datestamp, unix_secs)` for `SigV4`.
///
/// `unix_secs` lets callers derive a coherent expiration from the same instant.
fn current_timestamps() -> Result<(String, String, i64), String> {
    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| format!("clock error: {err}"))?;
    let secs = i64::try_from(since_epoch.as_secs())
        .map_err(|err| format!("timestamp out of range: {err}"))?;
    let datetime =
        DateTime::<Utc>::from_timestamp(secs, 0).ok_or_else(|| "invalid timestamp".to_owned())?;
    let amz_date = datetime.format("%Y%m%dT%H%M%SZ").to_string();
    let datestamp = datetime.format("%Y%m%d").to_string();
    Ok((amz_date, datestamp, secs))
}

/// Formats an ISO8601 UTC timestamp (`2026-06-04T07:00:00.000Z`) for a POST policy
/// expiration, `expires` seconds after `now_secs`.
fn iso8601_expiration(now_secs: i64, expires: u64) -> Result<String, String> {
    let delta = i64::try_from(expires).map_err(|err| format!("expires out of range: {err}"))?;
    let exp_secs = now_secs
        .checked_add(delta)
        .ok_or_else(|| "expiration overflow".to_owned())?;
    let datetime = DateTime::<Utc>::from_timestamp(exp_secs, 0)
        .ok_or_else(|| "invalid expiration timestamp".to_owned())?;
    Ok(datetime.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

// -- Metrics ----------------------------------------------------------------

/// Builds an `S3Metric` from the outcome (or zeros on failure).
fn build_metric(action: &str, outcome: Option<&PresignOutcome>, start: Instant) -> S3Metric {
    let (method, expires) =
        outcome.map_or((String::new(), 0), |out| (out.method.clone(), out.expires));

    S3Metric {
        action: action.to_owned(),
        method,
        duration_us: start.elapsed().as_micros(),
        expires,
    }
}
