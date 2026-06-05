//! Redis key/value client for the `QuickJS` sandbox (`redis` global).
//!
//! JS API: `redis.get/set/del/incr/expire`. Trust model matches `db`/`mail`: the
//! connection is operator-supplied in `config.redis`, so no SSRF guard — internal
//! Redis instances are intended to work. Values are **strings in / strings out**;
//! the script owns (de)serialization. Each op is metered.
//!
//! The module is named `kv` (not `redis`) so it doesn't shadow the `redis` crate.

use std::error::Error;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use redis::{Commands, Connection};
use rquickjs::{Ctx, Function, Value as JsValue};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::errors::{self, ErrorOwner, ErrorSource, Fault};
use crate::sandbox::{self, Collector};

/// JS wrapper — loaded from `src/js/redis.js` at compile time.
const KV_WRAPPER: &str = include_str!("js/redis.js");

/// Fallback fault for a Redis error with no specific predicate.
const REDIS_FALLBACK: Fault = Fault::new("REDIS_ERROR", true, ErrorOwner::Operator);
/// Fault for exhausting the per-execution op budget mid-call.
const REDIS_OP_LIMIT: Fault = Fault::new("REDIS_OP_LIMIT", false, ErrorOwner::Developer);
/// Fault for a Redis command that timed out.
const REDIS_TIMEOUT: Fault = Fault::new("REDIS_TIMEOUT", true, ErrorOwner::Operator);
/// Fault for a failure to reach Redis (inject-time connect, or a mid-session IO error).
pub(crate) const REDIS_CONNECTION_FAULT: Fault =
    Fault::new("REDIS_CONNECTION", true, ErrorOwner::Operator);

/// Per-request Redis configuration.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RedisConfig {
    /// Connection URL, e.g. `redis://user:pass@host:6379/0`.
    pub(crate) url: String,
    /// Connect + command timeout in milliseconds (default 5000).
    #[serde(default = "default_timeout")]
    pub(crate) timeout_ms: u64,
}

/// Default command timeout in milliseconds.
const fn default_timeout() -> u64 { 5000 }

/// Metric recorded for each Redis operation.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct RedisMetric {
    /// Operation type.
    action: String,
    /// Duration in microseconds.
    duration_us: u128,
    /// Value size in bytes (get/set; 0 otherwise).
    bytes: usize,
    /// Whether a `get` found the key (false otherwise).
    hit: bool,
}

/// A Redis error carrying its classified [`Fault`] plus the raw message.
#[derive(Debug)]
struct RedisError {
    /// Classified code + retry hint + owner.
    fault: Fault,
    /// Raw driver/usage message.
    message: String,
}

impl RedisError {
    /// Builds a fallback (`REDIS_ERROR`) error — used for non-driver failures (payload
    /// parsing, lock, serialization, unknown action).
    const fn fallback(message: String) -> Self {
        Self { fault: REDIS_FALLBACK, message }
    }

    /// Classifies a `redis` driver error.
    fn from_driver(err: &redis::RedisError) -> Self {
        Self { fault: classify(err), message: err.to_string() }
    }
}

/// Returns `true` if an `inject_redis` error is a driver failure — a boxed
/// `redis::RedisError` — vs an engine-setup failure (function registration / eval). Lets
/// the engine map a dead Redis to a retryable `capability/redis/REDIS_CONNECTION`.
pub(crate) fn is_connect_error(err: &(dyn Error + Send + Sync + 'static)) -> bool {
    err.downcast_ref::<redis::RedisError>().is_some()
}

/// Maps a `redis::RedisError` to a [`Fault`] (timeout / IO → retryable infra).
fn classify(err: &redis::RedisError) -> Fault {
    if err.is_timeout() {
        REDIS_TIMEOUT
    } else if err.is_io_error() {
        REDIS_CONNECTION_FAULT
    } else {
        REDIS_FALLBACK
    }
}

// -- Public API -------------------------------------------------------------

/// Connects and injects the `redis` global. Returns a metrics collector.
///
/// # Errors
///
/// Returns an error if connection or registration fails.
pub(crate) fn inject_redis(
    qctx: &Ctx<'_>,
    config: &RedisConfig,
    max_ops: usize,
) -> Result<Collector<RedisMetric>, Box<dyn Error + Send + Sync>> {
    let conn = connect(config)?;
    let shared_conn = Arc::new(Mutex::new(conn));

    let metrics: Collector<RedisMetric> = sandbox::new_collector();
    let metrics_clone = Arc::clone(&metrics);
    let conn_clone = Arc::clone(&shared_conn);

    let redis_fn = Function::new(
        qctx.clone(),
        move |action: String, payload_json: String| -> String {
            if let Err(err) = sandbox::check_op_limit(&metrics_clone, max_ops) {
                return errors::capability_fault_json(ErrorSource::Redis, REDIS_OP_LIMIT, &err, None);
            }

            let start = Instant::now();
            let result = dispatch(&conn_clone, &action, &payload_json);
            let metric = build_metric(&action, &result, start);
            sandbox::record(&metrics_clone, metric);

            match result {
                Ok(json_out) => json_out,
                Err(redis_err) => errors::capability_fault_json(
                    ErrorSource::Redis,
                    redis_err.fault,
                    &redis_err.message,
                    None,
                ),
            }
        },
    )?
    .with_name("__redis")?;

    qctx.globals().set("__redis", redis_fn)?;

    let wrapper: JsValue<'_> = qctx.eval(KV_WRAPPER)?;
    drop(wrapper);

    Ok(metrics)
}

/// Builds a Redis connection with read/write timeouts applied.
fn connect(config: &RedisConfig) -> Result<Connection, Box<dyn Error + Send + Sync>> {
    let client = redis::Client::open(config.url.as_str())?;
    let conn = client.get_connection()?;
    let timeout = Duration::from_millis(config.timeout_ms);
    conn.set_read_timeout(Some(timeout))?;
    conn.set_write_timeout(Some(timeout))?;
    Ok(conn)
}

// -- Dispatch ---------------------------------------------------------------

/// Routes a `__redis` call to the correct handler.
fn dispatch(
    conn: &Arc<Mutex<Connection>>,
    action: &str,
    payload_json: &str,
) -> Result<String, RedisError> {
    match action {
        "get" => do_get(conn, payload_json),
        "set" => do_set(conn, payload_json),
        "del" => do_del(conn, payload_json),
        "incr" => do_incr(conn, payload_json),
        "expire" => do_expire(conn, payload_json),
        other => Err(RedisError::fallback(format!("unknown redis action: {other}"))),
    }
}

/// Acquires the shared connection lock.
fn lock_conn(conn: &Arc<Mutex<Connection>>) -> Result<MutexGuard<'_, Connection>, RedisError> {
    conn.lock().map_err(|err| RedisError::fallback(format!("lock error: {err}")))
}

/// Parses a payload, mapping failures to a fallback error.
fn parse<T: for<'de> Deserialize<'de>>(payload_json: &str) -> Result<T, RedisError> {
    serde_json::from_str(payload_json)
        .map_err(|err| RedisError::fallback(format!("invalid redis payload: {err}")))
}

/// Encodes a response object, mapping failures to a fallback error.
fn encode(value: &Value) -> Result<String, RedisError> {
    serde_json::to_string(value)
        .map_err(|err| RedisError::fallback(format!("serialize error: {err}")))
}

// -- Operations -------------------------------------------------------------

/// `GET key` → `{ value: string | null }`.
fn do_get(conn: &Arc<Mutex<Connection>>, payload_json: &str) -> Result<String, RedisError> {
    let payload: KeyPayload = parse(payload_json)?;
    let value: Option<String> = {
        let mut guard = lock_conn(conn)?;
        guard.get(&payload.key).map_err(|err| RedisError::from_driver(&err))?
    };
    encode(&json!({ "value": value }))
}

/// `SET key value [EX ttl]` → `{ ok: true }`.
fn do_set(conn: &Arc<Mutex<Connection>>, payload_json: &str) -> Result<String, RedisError> {
    let payload: SetPayload = parse(payload_json)?;
    // Compute under the lock, then handle the error after the guard drops.
    let outcome: redis::RedisResult<()> = {
        let mut guard = lock_conn(conn)?;
        match payload.ttl {
            Some(ttl) => guard.set_ex(&payload.key, &payload.value, ttl),
            None => guard.set(&payload.key, &payload.value),
        }
    };
    outcome.map_err(|err| RedisError::from_driver(&err))?;
    encode(&json!({ "ok": true }))
}

/// `DEL key` → `{ count: number }`.
fn do_del(conn: &Arc<Mutex<Connection>>, payload_json: &str) -> Result<String, RedisError> {
    let payload: KeyPayload = parse(payload_json)?;
    let count: i64 = {
        let mut guard = lock_conn(conn)?;
        guard.del(&payload.key).map_err(|err| RedisError::from_driver(&err))?
    };
    encode(&json!({ "count": count }))
}

/// `INCR key` → `{ value: number }`.
fn do_incr(conn: &Arc<Mutex<Connection>>, payload_json: &str) -> Result<String, RedisError> {
    let payload: KeyPayload = parse(payload_json)?;
    let value: i64 = {
        let mut guard = lock_conn(conn)?;
        guard.incr(&payload.key, 1_i64).map_err(|err| RedisError::from_driver(&err))?
    };
    encode(&json!({ "value": value }))
}

/// `EXPIRE key seconds` → `{ set: bool }`.
fn do_expire(conn: &Arc<Mutex<Connection>>, payload_json: &str) -> Result<String, RedisError> {
    let payload: ExpirePayload = parse(payload_json)?;
    let set: bool = {
        let mut guard = lock_conn(conn)?;
        guard
            .expire(&payload.key, payload.seconds)
            .map_err(|err| RedisError::from_driver(&err))?
    };
    encode(&json!({ "set": set }))
}

// -- Payloads ---------------------------------------------------------------

/// Payload carrying just a key (`get`/`del`/`incr`).
#[derive(Debug, Deserialize)]
struct KeyPayload {
    /// The Redis key.
    #[serde(default)]
    key: String,
}

/// Payload for `set`.
#[derive(Debug, Deserialize)]
struct SetPayload {
    /// The Redis key.
    #[serde(default)]
    key: String,
    /// The value (a string — the script serializes objects itself).
    #[serde(default)]
    value: String,
    /// Optional TTL in seconds.
    #[serde(default)]
    ttl: Option<u64>,
}

/// Payload for `expire`.
#[derive(Debug, Deserialize)]
struct ExpirePayload {
    /// The Redis key.
    #[serde(default)]
    key: String,
    /// TTL in seconds.
    #[serde(default)]
    seconds: i64,
}

// -- Metrics ----------------------------------------------------------------

/// Builds a `RedisMetric` from the result of an operation.
fn build_metric(action: &str, result: &Result<String, RedisError>, start: Instant) -> RedisMetric {
    let (bytes, hit) = result.as_ref().map_or((0, false), |json| value_stats(action, json));
    RedisMetric {
        action: action.to_owned(),
        duration_us: start.elapsed().as_micros(),
        bytes,
        hit,
    }
}

/// Extracts `(bytes, hit)` from a `get` response (zero/false for other ops).
fn value_stats(action: &str, json: &str) -> (usize, bool) {
    if action != "get" {
        return (0, false);
    }
    let parsed: Value = serde_json::from_str(json).unwrap_or(Value::Null);
    parsed.get("value").and_then(Value::as_str).map_or((0, false), |text| (text.len(), true))
}
