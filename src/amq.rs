//! `RabbitMQ` **producer** for the `QuickJS` sandbox (`amq` global).
//!
//! JS API: `amq.send([[routingKey, payload], …])` — list-always; Rust owns batching.
//! Trust model matches `db`/`mail`: the broker connection is operator-supplied in
//! `config.amq`, so no SSRF guard. Producer only (no consume/subscribe).
//!
//! `amqprs` is async, but capability closures run blocking (inside `spawn_blocking`),
//! so each `send` opens **one** connection + channel for the whole batch inside a
//! per-call current-thread `tokio` runtime (`block_on`), publishes every message, and
//! closes. One `send` call = one metered op, regardless of batch size.

use std::error::Error;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use amqprs::channel::BasicPublishArguments;
use amqprs::connection::{Connection, OpenConnectionArguments};
use amqprs::tls::TlsAdaptor;
use amqprs::BasicProperties;
use rquickjs::{Ctx, Function, Value as JsValue};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use tokio::runtime::Builder;

use crate::errors::{self, ErrorOwner, ErrorSource, Fault};
use crate::sandbox::{self, Collector};

/// JS wrapper — loaded from `src/js/amq.js` at compile time.
const AMQ_WRAPPER: &str = include_str!("js/amq.js");

/// Fallback fault for a publish/protocol error.
const AMQ_FALLBACK: Fault = Fault::new("AMQ_ERROR", true, ErrorOwner::Operator);
/// Fault for exhausting the per-execution op budget.
const AMQ_OP_LIMIT: Fault = Fault::new("AMQ_OP_LIMIT", false, ErrorOwner::Developer);
/// Fault for a failure to reach / authenticate with the broker.
const AMQ_CONNECTION: Fault = Fault::new("AMQ_CONNECTION", true, ErrorOwner::Operator);
/// Fault for a batch larger than `max_batch`.
const AMQ_BATCH: Fault = Fault::new("AMQ_BATCH_TOO_LARGE", false, ErrorOwner::Developer);

/// Per-request `RabbitMQ` configuration.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AmqConfig {
    /// Broker host.
    pub(crate) host: String,
    /// Broker port (default 5672).
    #[serde(default = "default_port")]
    pub(crate) port: u16,
    /// AMQP username (default `guest`).
    #[serde(default = "default_user")]
    pub(crate) username: String,
    /// AMQP password (default `guest`).
    #[serde(default = "default_user")]
    pub(crate) password: String,
    /// Virtual host (default `/`).
    #[serde(default = "default_vhost")]
    pub(crate) vhost: String,
    /// Exchange to publish to (default `""` — the default exchange; routing key is the queue).
    #[serde(default)]
    pub(crate) exchange: String,
    /// Maximum messages per `send` call (default 100).
    #[serde(default = "default_max_batch")]
    pub(crate) max_batch: usize,
    /// Use TLS (`amqps://`). Reuses the `aws-lc-rs` rustls provider.
    #[serde(default)]
    pub(crate) tls: bool,
    /// Path to a custom CA cert (PEM) for a self-hosted broker. Omit for managed services
    /// (their public CAs are covered by the bundled webpki roots).
    #[serde(default)]
    pub(crate) ca_cert: Option<String>,
}

/// Default AMQP port.
const fn default_port() -> u16 { 5672 }
/// Default AMQP user/password.
fn default_user() -> String { "guest".to_owned() }
/// Default virtual host.
fn default_vhost() -> String { "/".to_owned() }
/// Default batch cap.
const fn default_max_batch() -> usize { 100 }

/// Metric recorded for each `amq.send` op.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AmqMetric {
    /// Operation type.
    action: String,
    /// Duration in microseconds.
    duration_us: u128,
    /// Number of messages in the batch.
    messages: usize,
    /// Total payload bytes published.
    bytes: usize,
    /// Whether the batch was accepted by the broker.
    published: bool,
}

/// An amq error carrying its classified [`Fault`] plus the raw message.
#[derive(Debug)]
struct AmqError {
    /// Classified code + retry hint + owner.
    fault: Fault,
    /// Raw message.
    message: String,
}

impl AmqError {
    /// Builds a fallback (`AMQ_ERROR`) error.
    const fn fallback(message: String) -> Self {
        Self { fault: AMQ_FALLBACK, message }
    }

    /// Builds a connection error (`AMQ_CONNECTION`).
    const fn connection(message: String) -> Self {
        Self { fault: AMQ_CONNECTION, message }
    }
}

/// Successful send result plus the stats needed to build a metric.
#[derive(Debug)]
struct SendOutcome {
    /// JSON returned to JS.
    json: String,
    /// Number of messages published.
    messages: usize,
    /// Total payload bytes.
    bytes: usize,
}

// -- Public API -------------------------------------------------------------

/// Injects the `amq` global. Returns a metrics collector. (No connection is opened
/// here — `send` connects lazily so a broker outage surfaces as a capability error.)
///
/// # Errors
///
/// Returns an error if function registration or JS eval fails.
pub(crate) fn inject_amq(
    qctx: &Ctx<'_>,
    config: &AmqConfig,
    max_ops: usize,
) -> Result<Collector<AmqMetric>, Box<dyn Error + Send + Sync>> {
    let owned = config.clone();

    let metrics: Collector<AmqMetric> = sandbox::new_collector();
    let metrics_clone = Arc::clone(&metrics);

    let amq_fn = Function::new(
        qctx.clone(),
        move |action: String, payload_json: String| -> String {
            if let Err(err) = sandbox::check_op_limit(&metrics_clone, max_ops) {
                return errors::capability_fault_json(ErrorSource::Amq, AMQ_OP_LIMIT, &err, None);
            }

            let start = Instant::now();
            let result = dispatch(&owned, &action, &payload_json);
            let metric = build_metric(&action, result.as_ref().ok(), start);
            sandbox::record(&metrics_clone, metric);

            match result {
                Ok(outcome) => outcome.json,
                Err(amq_err) => {
                    errors::capability_fault_json(ErrorSource::Amq, amq_err.fault, &amq_err.message, None)
                }
            }
        },
    )?
    .with_name("__amq")?;

    qctx.globals().set("__amq", amq_fn)?;

    let wrapper: JsValue<'_> = qctx.eval(AMQ_WRAPPER)?;
    drop(wrapper);

    Ok(metrics)
}

// -- Dispatch ---------------------------------------------------------------

/// Routes an `__amq` call to the correct handler.
fn dispatch(config: &AmqConfig, action: &str, payload_json: &str) -> Result<SendOutcome, AmqError> {
    match action {
        "send" => do_send(config, payload_json),
        other => Err(AmqError::fallback(format!("unknown amq action: {other}"))),
    }
}

/// Parses + validates the batch, then publishes it in one `block_on`.
fn do_send(config: &AmqConfig, payload_json: &str) -> Result<SendOutcome, AmqError> {
    let payload: SendPayload = serde_json::from_str(payload_json)
        .map_err(|err| AmqError::fallback(format!("invalid amq payload: {err}")))?;

    let count = payload.messages.len();
    if count == 0 {
        return Err(AmqError::fallback("amq.send requires at least one message".to_owned()));
    }
    if count > config.max_batch {
        return Err(AmqError {
            fault: AMQ_BATCH,
            message: format!("batch too large: {count} (max {})", config.max_batch),
        });
    }

    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| AmqError::fallback(format!("failed to build runtime: {err}")))?;

    let bytes = runtime.block_on(publish_batch(config, &payload.messages))?;

    let json = format!("{{\"published\":{count}}}");
    Ok(SendOutcome { json, messages: count, bytes })
}

/// Opens one connection + channel, publishes every message, then closes.
async fn publish_batch(config: &AmqConfig, messages: &[AmqMessage]) -> Result<usize, AmqError> {
    let mut args =
        OpenConnectionArguments::new(&config.host, config.port, &config.username, &config.password);
    let _ = args.virtual_host(&config.vhost);
    if config.tls {
        let ca = config.ca_cert.as_deref().map(Path::new);
        let adaptor = TlsAdaptor::without_client_auth(ca, config.host.clone())
            .map_err(|err| AmqError::connection(format!("amq tls setup failed: {err}")))?;
        let _ = args.tls_adaptor(adaptor);
    }

    let connection = Connection::open(&args)
        .await
        .map_err(|err| AmqError::connection(format!("amq connect failed: {err}")))?;
    let channel = connection
        .open_channel(None)
        .await
        .map_err(|err| AmqError::connection(format!("amq channel failed: {err}")))?;

    let mut total_bytes: usize = 0;
    for message in messages {
        let content = message.payload.get().as_bytes().to_vec();
        total_bytes = total_bytes.saturating_add(content.len());
        let publish_args = BasicPublishArguments::new(&config.exchange, &message.key);
        channel
            .basic_publish(BasicProperties::default(), content, publish_args)
            .await
            .map_err(|err| AmqError::fallback(format!("amq publish failed: {err}")))?;
    }

    drop(channel.close().await);
    drop(connection.close().await);
    Ok(total_bytes)
}

// -- Payloads ---------------------------------------------------------------

/// Parsed `send` payload.
#[derive(Debug, Deserialize)]
struct SendPayload {
    /// The messages to publish.
    #[serde(default)]
    messages: Vec<AmqMessage>,
}

/// One message: a routing key + a raw-JSON payload (published as the body bytes).
#[derive(Debug, Deserialize)]
struct AmqMessage {
    /// Routing key (queue name for the default exchange).
    #[serde(default)]
    key: String,
    /// Payload — serialized to its JSON bytes as the message body.
    #[serde(default = "default_payload")]
    payload: Box<RawValue>,
}

/// Default payload (`null`) when a message omits one.
fn default_payload() -> Box<RawValue> {
    RawValue::from_string("null".to_owned()).unwrap_or_else(|_err| unreachable!())
}

// -- Metrics ----------------------------------------------------------------

/// Builds an `AmqMetric` from the outcome (or zeros on failure).
fn build_metric(action: &str, outcome: Option<&SendOutcome>, start: Instant) -> AmqMetric {
    let (messages, bytes, published) =
        outcome.map_or((0, 0, false), |out| (out.messages, out.bytes, true));
    AmqMetric {
        action: action.to_owned(),
        duration_us: start.elapsed().as_micros(),
        messages,
        bytes,
        published,
    }
}
