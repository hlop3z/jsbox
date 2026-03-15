//! `PostgreSQL`/`CockroachDB` client for the `QuickJS` sandbox.
//!
//! JS API: `db.query(sql, params?)`, `db.execute(sql, params?)`,
//! `db.begin()`, `db.commit()`, `db.rollback()`.
//!
//! i64/BIGINT/NUMERIC always serialized as strings for JS safety.

use std::error::Error;
use std::sync::Arc;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use postgres::types::{FromSql, IsNull, ToSql, Type};
use postgres::{Client, NoTls};
use rquickjs::{Ctx, Function, Value as JsValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::sandbox::{self, Collector};

/// JS wrapper — loaded from `src/js/db.js` at compile time.
const DB_WRAPPER: &str = include_str!("js/db.js");

/// Per-request database configuration.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct DbConfig {
    /// Database host.
    pub(crate) host: String,
    /// Database port (default 5432).
    #[serde(default = "default_port")]
    pub(crate) port: u16,
    /// Database user.
    pub(crate) user: String,
    /// Database password.
    pub(crate) password: String,
    /// Database name.
    pub(crate) database: String,
    /// Use SSL/TLS.
    #[serde(default)]
    pub(crate) ssl: bool,
    /// Query timeout in milliseconds (default 5000).
    #[serde(default = "default_statement_timeout")]
    pub(crate) statement_timeout_ms: u64,
    /// Max rows returned (default 1000).
    #[serde(default = "default_max_rows")]
    pub(crate) max_rows: usize,
}

/// Default port.
const fn default_port() -> u16 { 5432 }
/// Default statement timeout.
const fn default_statement_timeout() -> u64 { 5000 }
/// Default max rows.
const fn default_max_rows() -> usize { 1000 }

/// Metric recorded for each DB operation.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DbMetric {
    /// Operation type.
    action: String,
    /// Duration in microseconds.
    duration_us: u128,
    /// Rows returned (query only).
    rows_returned: usize,
    /// Rows affected (execute only).
    rows_affected: u64,
    /// Whether result was truncated.
    truncated: bool,
}

// -- Public API -------------------------------------------------------------

/// Connects and injects the `db` global. Returns a metrics collector.
///
/// # Errors
///
/// Returns an error if connection or registration fails.
pub(crate) fn inject_db(
    qctx: &Ctx<'_>,
    config: &DbConfig,
    max_ops: usize,
) -> Result<Collector<DbMetric>, Box<dyn Error + Send + Sync>> {
    let pg_client = connect(config)?;
    let shared_client = Arc::new(Mutex::new(pg_client));
    let max_rows = config.max_rows;

    let metrics: Collector<DbMetric> = sandbox::new_collector();
    let metrics_clone = Arc::clone(&metrics);
    let client_clone = Arc::clone(&shared_client);

    let db_fn = Function::new(
        qctx.clone(),
        move |action: String, query: String, params_json: String| -> String {
            if let Err(err) = sandbox::check_op_limit(&metrics_clone, max_ops) {
                return sandbox::error_json(&err);
            }

            let start = Instant::now();
            let result = dispatch(&client_clone, &action, &query, &params_json, max_rows);
            let metric = build_metric(&action, &result, start);
            sandbox::record(&metrics_clone, metric);

            match result {
                Ok(json) => json,
                Err(err) => sandbox::error_json(&err),
            }
        },
    )?
    .with_name("__db")?;

    qctx.globals().set("__db", db_fn)?;

    let wrapper: JsValue<'_> = qctx.eval(DB_WRAPPER)?;
    drop(wrapper);

    Ok(metrics)
}

// -- Dispatch ---------------------------------------------------------------

/// Routes a `__db` call to the correct handler.
fn dispatch(
    client: &Arc<Mutex<Client>>,
    action: &str,
    query: &str,
    params_json: &str,
    max_rows: usize,
) -> Result<String, String> {
    match action {
        "query" => do_query(client, query, params_json, max_rows),
        "execute" => do_execute(client, query, params_json),
        "begin" => do_simple(client, "BEGIN"),
        "commit" => do_simple(client, "COMMIT"),
        "rollback" => do_simple(client, "ROLLBACK"),
        other => Err(format!("unknown db action: {other}")),
    }
}

// -- Connection -------------------------------------------------------------

/// Connects to the database.
fn connect(config: &DbConfig) -> Result<Client, Box<dyn Error + Send + Sync>> {
    let mut pg_config = postgres::Config::new();
    let _ = pg_config
        .host(&config.host)
        .port(config.port)
        .user(&config.user)
        .password(&config.password)
        .dbname(&config.database)
        .connect_timeout(Duration::from_secs(5));

    let mut pg_client = if config.ssl {
        let mut root_store = rustls::RootCertStore::empty();
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls_config = rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();
        let tls_connector = tokio_rustls::TlsConnector::from(Arc::new(tls_config));
        let tls = postgres_rustls::MakeTlsConnector::new(tls_connector);
        pg_config.connect(tls)?
    } else {
        pg_config.connect(NoTls)?
    };

    // Safe: statement_timeout_ms is u64, cannot produce SQL injection.
    let timeout_cmd = format!("SET statement_timeout = '{}'", config.statement_timeout_ms);
    let _ = pg_client.execute(timeout_cmd.as_str(), &[])?;

    Ok(pg_client)
}

// -- Query / Execute --------------------------------------------------------

/// Acquires the shared DB client lock.
fn lock_client(client: &Arc<Mutex<Client>>) -> Result<MutexGuard<'_, Client>, String> {
    client.lock().map_err(|err| format!("lock error: {err}"))
}

/// SELECT — returns `{columns, rows, row_count, truncated}`.
fn do_query(
    client: &Arc<Mutex<Client>>,
    sql: &str,
    params_json: &str,
    max_rows: usize,
) -> Result<String, String> {
    let params = parse_params(params_json)?;
    let param_refs = build_param_refs(&params);

    let rows = {
        let mut guard = lock_client(client)?;
        guard.query(sql, &param_refs).map_err(|err| format!("query error: {err}"))?
    };

    let columns = extract_columns(&rows);
    let (json_rows, truncated) = rows_to_json(&rows, max_rows);
    let row_count = json_rows.len();

    let result = serde_json::json!({
        "columns": columns,
        "rows": json_rows,
        "row_count": row_count,
        "truncated": truncated,
    });

    serde_json::to_string(&result).map_err(|err| format!("serialize error: {err}"))
}

/// INSERT/UPDATE/DELETE — returns `{rows_affected}`.
fn do_execute(
    client: &Arc<Mutex<Client>>,
    sql: &str,
    params_json: &str,
) -> Result<String, String> {
    let params = parse_params(params_json)?;
    let param_refs = build_param_refs(&params);

    let affected = {
        let mut guard = lock_client(client)?;
        guard.execute(sql, &param_refs).map_err(|err| format!("execute error: {err}"))?
    };

    Ok(format!("{{\"rows_affected\":{affected}}}"))
}

/// Simple command (BEGIN/COMMIT/ROLLBACK).
fn do_simple(client: &Arc<Mutex<Client>>, cmd: &str) -> Result<String, String> {
    {
        let mut guard = lock_client(client)?;
        let _ = guard.execute(cmd, &[]).map_err(|err| format!("{cmd} error: {err}"))?;
    }
    Ok("{\"ok\":true}".into())
}

// -- Column extraction ------------------------------------------------------

/// Extracts column names from query results.
fn extract_columns(rows: &[postgres::Row]) -> Vec<String> {
    rows.first()
        .map(|first| first.columns().iter().map(|col| col.name().into()).collect())
        .unwrap_or_default()
}

/// Converts rows to JSON values, truncating at `max_rows`.
fn rows_to_json(rows: &[postgres::Row], max_rows: usize) -> (Vec<Value>, bool) {
    let truncated = rows.len() > max_rows;
    let limit = if truncated { max_rows } else { rows.len() };

    let json_rows: Vec<Value> = rows.iter().take(limit).map(row_to_json).collect();
    (json_rows, truncated)
}

/// Converts a single row to a JSON object.
fn row_to_json(row: &postgres::Row) -> Value {
    let mut obj = serde_json::Map::new();
    for (idx, col) in row.columns().iter().enumerate() {
        drop(obj.insert(col.name().into(), column_to_json(row, idx, col.type_())));
    }
    Value::Object(obj)
}

/// Converts a column value to `serde_json::Value`.
///
/// Rule: i32 and smaller -> number. i64 and larger -> string. Always.
fn column_to_json(row: &postgres::Row, idx: usize, col_type: &Type) -> Value {
    match *col_type {
        Type::INT2 => get_or_null::<i16>(row, idx, Value::from),
        Type::INT4 | Type::OID => get_or_null::<i32>(row, idx, Value::from),
        Type::INT8 => get_or_null::<i64>(row, idx, |val| Value::String(val.to_string())),
        Type::FLOAT4 => get_or_null::<f32>(row, idx, |val| {
            serde_json::Number::from_f64(f64::from(val)).map_or(Value::Null, Value::Number)
        }),
        Type::FLOAT8 => get_or_null::<f64>(row, idx, |val| {
            serde_json::Number::from_f64(val).map_or(Value::Null, Value::Number)
        }),
        Type::BOOL => get_or_null::<bool>(row, idx, Value::Bool),
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME => {
            get_or_null::<String>(row, idx, Value::String)
        }
        Type::JSON | Type::JSONB => get_or_null::<Value>(row, idx, |val| val),
        Type::UUID => get_or_null::<uuid::Uuid>(row, idx, |val| Value::String(val.to_string())),
        Type::TIMESTAMP => get_or_null::<chrono::NaiveDateTime>(row, idx, |val| {
            Value::String(val.format("%Y-%m-%dT%H:%M:%S%.f").to_string())
        }),
        Type::TIMESTAMPTZ => get_or_null::<chrono::DateTime<chrono::Utc>>(row, idx, |val| {
            Value::String(val.to_rfc3339())
        }),
        Type::DATE => get_or_null::<chrono::NaiveDate>(row, idx, |val| Value::String(val.to_string())),
        Type::TIME => get_or_null::<chrono::NaiveTime>(row, idx, |val| {
            Value::String(val.format("%H:%M:%S%.f").to_string())
        }),
        Type::NUMERIC => get_or_null::<rust_decimal::Decimal>(row, idx, |val| {
            Value::String(val.to_string())
        }),
        Type::BYTEA => get_or_null::<Vec<u8>>(row, idx, |val| Value::String(BASE64.encode(&val))),
        // Fallback: try as String.
        _ => get_or_null::<String>(row, idx, Value::String),
    }
}

/// Tries to get a typed value; returns `Value::Null` on NULL or type mismatch.
fn get_or_null<'a, T: FromSql<'a>>(
    row: &'a postgres::Row,
    idx: usize,
    convert: impl FnOnce(T) -> Value,
) -> Value {
    match row.try_get::<_, Option<T>>(idx) {
        Ok(Some(val)) => convert(val),
        Ok(None) => Value::Null,
        Err(_err) => {
            // Type mismatch fallback: try as string.
            match row.try_get::<_, Option<String>>(idx) {
                Ok(Some(text)) => Value::String(text),
                _ => Value::Null,
            }
        }
    }
}

// -- Parameters -------------------------------------------------------------

/// Parses JSON params into typed values.
fn parse_params(params_json: &str) -> Result<Vec<ParamValue>, String> {
    let values: Vec<Value> =
        serde_json::from_str(params_json).map_err(|err| format!("invalid params JSON: {err}"))?;
    Ok(values.into_iter().map(ParamValue::from).collect())
}

/// A typed parameter value.
#[derive(Debug)]
enum ParamValue {
    /// NULL.
    Null,
    /// Boolean.
    Bool(bool),
    /// 32-bit integer.
    Int4(i32),
    /// 64-bit integer.
    Int8(i64),
    /// 64-bit float.
    Float8(f64),
    /// Text.
    Text(String),
}

impl From<Value> for ParamValue {
    fn from(val: Value) -> Self {
        match val {
            Value::Null => Self::Null,
            Value::Bool(flag) => Self::Bool(flag),
            Value::Number(num) => num.as_i64().map_or_else(
                || Self::Float8(num.as_f64().unwrap_or(0.0)),
                |int| i32::try_from(int).map_or(Self::Int8(int), Self::Int4),
            ),
            Value::String(text) => Self::Text(text),
            Value::Array(arr) => Self::Text(serde_json::to_string(&arr).unwrap_or_else(|_| "[]".into())),
            Value::Object(obj) => Self::Text(serde_json::to_string(&obj).unwrap_or_else(|_| "{}".into())),
        }
    }
}

#[expect(clippy::missing_trait_methods, reason = "ToSql has encode_format with a sensible default")]
impl ToSql for ParamValue {
    fn to_sql(&self, ty: &Type, out: &mut bytes::BytesMut) -> Result<IsNull, Box<dyn Error + Sync + Send>> {
        match self {
            Self::Null => Ok(IsNull::Yes),
            Self::Bool(val) => val.to_sql(ty, out),
            Self::Int4(val) => val.to_sql(ty, out),
            Self::Int8(val) => val.to_sql(ty, out),
            Self::Float8(val) => val.to_sql(ty, out),
            Self::Text(val) => val.to_sql(ty, out),
        }
    }

    fn accepts(_ty: &Type) -> bool { true }
    postgres::types::to_sql_checked!();
}

/// Builds trait object references from param values.
fn build_param_refs(params: &[ParamValue]) -> Vec<&(dyn ToSql + Sync)> {
    params.iter().map(as_tosql_ref).collect()
}

/// Coerces a `ParamValue` to a `&dyn ToSql + Sync`.
fn as_tosql_ref(param: &ParamValue) -> &(dyn ToSql + Sync) {
    param
}

// -- Metrics ----------------------------------------------------------------

/// Builds a `DbMetric` from the result of an operation.
fn build_metric(action: &str, result: &Result<String, String>, start: Instant) -> DbMetric {
    let (rows_ret, rows_aff, trunc) = result
        .as_ref()
        .map(|json| extract_metric_info(action, json))
        .unwrap_or((0, 0, false));

    DbMetric {
        action: action.into(),
        duration_us: start.elapsed().as_micros(),
        rows_returned: rows_ret,
        rows_affected: rows_aff,
        truncated: trunc,
    }
}

/// Extracts metric info from a response JSON.
fn extract_metric_info(action: &str, json: &str) -> (usize, u64, bool) {
    let parsed: Value = serde_json::from_str(json).unwrap_or(Value::Null);
    match action {
        "query" => {
            let rows = parsed.get("row_count").and_then(Value::as_u64).unwrap_or(0);
            let trunc = parsed.get("truncated").and_then(Value::as_bool).unwrap_or(false);
            (usize::try_from(rows).unwrap_or(0), 0, trunc)
        }
        "execute" => {
            let affected = parsed.get("rows_affected").and_then(Value::as_u64).unwrap_or(0);
            (0, affected, false)
        }
        _ => (0, 0, false),
    }
}
