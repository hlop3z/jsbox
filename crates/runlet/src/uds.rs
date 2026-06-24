//! Box-side UDS client for the `fabricd` egress sidecar.
//!
//! [`connect_session`] (async, run on the request's async task) opens the connection and performs
//! the [`WireInit`] handshake — sending the selected logical resource *names* and the deadline,
//! and surfacing a name-resolution failure as a [`SessionError`] the handler maps to a status.
//! [`UdsEgress`] then wraps the live stream and implements [`Egress`] (and [`MeteredEgress`]) by
//! forwarding each `io.call(...)` over the socket; one `UdsEgress` owns the connection for the life
//! of a box-request, so a transaction's `begin`→`commit` hit the same daemon-side client.
//!
//! Every `UdsEgress` round-trip runs via `Handle::block_on` (the engine drives capabilities
//! synchronously on a `spawn_blocking` thread), bounded by the per-execution deadline — so build
//! and drain it on that blocking thread, never on a runtime worker. The box links no driver and
//! holds no credentials; `fabricd` does. See `docs/design/resource-egress.md` step 5.

use std::io::{Error as IoError, ErrorKind};
use std::time::{Duration, Instant};

use fabric_wire::wire::{
    WireCall, WireInit, WireRequest, WireResponse, read_frame, write_frame,
};
use fabric_wire::{BackendMetrics, Egress, EgressError, ErrorOwner, MeteredEgress};
use tokio::net::UnixStream;
use tokio::runtime::Handle;
use tokio::sync::Mutex;
use tokio::time::timeout;

/// Why opening a `fabricd` session failed — mapped by the handler to an HTTP status.
#[derive(Debug)]
pub(crate) enum SessionError {
    /// No sidecar configured, or it couldn't be reached / handshaked → retryable `503`.
    Unavailable(String),
    /// A requested resource name didn't resolve at `Init` → the caller's `400` (`code` is
    /// `RESOURCE_NOT_FOUND` / `RESOURCE_KIND_MISMATCH`).
    Resolve {
        /// Stable request-category code from the daemon.
        code: String,
        /// Human-safe detail.
        message: String,
    },
    /// An unexpected daemon response to `Init` → `500`.
    Protocol(String),
}

/// Opens a `fabricd` session: connects to `socket` and performs the [`WireInit`] handshake.
///
/// Async — run on the request's async task (no `block_on`), so a name-resolution `400` or an
/// unreachable-sidecar `503` is decided before the blocking execution is admitted.
///
/// # Errors
///
/// Returns a [`SessionError`]: `Unavailable` (no socket / unreachable / closed), `Resolve` (a name
/// failed to resolve), or `Protocol` (an unexpected response).
pub(crate) async fn connect_session(
    socket: Option<&str>,
    init: &WireInit,
) -> Result<UnixStream, SessionError> {
    let Some(path) = socket else {
        return Err(SessionError::Unavailable(
            "no fabricd egress sidecar configured".to_owned(),
        ));
    };
    let mut stream = UnixStream::connect(path)
        .await
        .map_err(|err| SessionError::Unavailable(format!("fabricd unreachable: {err}")))?;
    write_frame(&mut stream, &WireRequest::Init(Box::new(init.clone())))
        .await
        .map_err(|err| SessionError::Unavailable(format!("fabricd write failed: {err}")))?;
    match read_frame::<_, WireResponse>(&mut stream).await {
        Ok(Some(WireResponse::Ack)) => Ok(stream),
        Ok(Some(WireResponse::InitError { code, message })) => {
            Err(SessionError::Resolve { code, message })
        }
        Ok(Some(
            WireResponse::Reply(_) | WireResponse::Metrics(_) | WireResponse::ProtocolError(_),
        )) => Err(SessionError::Protocol("unexpected response to Init".to_owned())),
        Ok(None) => Err(SessionError::Unavailable(
            "fabricd closed the connection during handshake".to_owned(),
        )),
        Err(err) => Err(SessionError::Unavailable(format!(
            "fabricd handshake read failed: {err}"
        ))),
    }
}

/// A box-request session to `fabricd` over an already-connected Unix-domain socket.
#[derive(Debug)]
pub(crate) struct UdsEgress {
    /// Runtime handle to drive the socket I/O via `block_on` (on the `spawn_blocking` thread).
    handle: Handle,
    /// The session connection — `tokio::sync::Mutex` so the `&self` egress can take `&mut` access
    /// across the write+read await without tripping `await_holding_lock`.
    conn: Mutex<UnixStream>,
    /// Absolute per-execution deadline bounding every round-trip.
    deadline: Instant,
}

impl UdsEgress {
    /// Wraps an already-handshaked stream (from [`connect_session`]) and anchors the deadline.
    /// Build this on the `spawn_blocking` thread — its calls `block_on`.
    pub(crate) fn from_stream(stream: UnixStream, handle: Handle, budget: Duration) -> Self {
        let deadline = Instant::now()
            .checked_add(budget)
            .unwrap_or_else(Instant::now);
        Self {
            handle,
            conn: Mutex::new(stream),
            deadline,
        }
    }
}

impl Egress for UdsEgress {
    fn call(&self, name: &str, action: &str, payload_json: &str) -> Result<String, EgressError> {
        let request = WireRequest::Call(WireCall {
            name: name.to_owned(),
            action: action.to_owned(),
            payload: payload_json.to_owned(),
        });
        let source = name.to_owned();
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        self.handle.block_on(async {
            let mut conn = self.conn.lock().await;
            match timeout(remaining, roundtrip(&mut conn, &request)).await {
                Ok(Ok(WireResponse::Reply(result))) => result,
                Ok(Ok(WireResponse::ProtocolError(msg))) => {
                    Err(transport_error(&source, "IO_PROTOCOL", &msg))
                }
                Ok(Ok(
                    WireResponse::Ack | WireResponse::InitError { .. } | WireResponse::Metrics(_),
                )) => Err(transport_error(
                    &source,
                    "IO_PROTOCOL",
                    "unexpected daemon response",
                )),
                Ok(Err(err)) => Err(transport_error(&source, "IO_TRANSPORT", &err.to_string())),
                Err(_elapsed) => Err(transport_error(
                    &source,
                    "IO_TIMEOUT",
                    "fabricd call exceeded the execution deadline",
                )),
            }
        })
    }
}

impl MeteredEgress for UdsEgress {
    fn drain_metrics(&self) -> BackendMetrics {
        // Best-effort: a drain failure (daemon gone, protocol slip) just yields empty metrics — it
        // must never turn a successful execution into an error.
        self.handle.block_on(async {
            let mut conn = self.conn.lock().await;
            match roundtrip(&mut conn, &WireRequest::Drain).await {
                Ok(WireResponse::Metrics(metrics)) => *metrics,
                Ok(
                    WireResponse::Ack
                    | WireResponse::InitError { .. }
                    | WireResponse::Reply(_)
                    | WireResponse::ProtocolError(_),
                )
                | Err(_) => BackendMetrics::default(),
            }
        })
    }
}

/// Writes one request frame and reads one response frame over the session connection. A clean EOF
/// mid-exchange is an unexpected close, surfaced as an I/O error.
async fn roundtrip(conn: &mut UnixStream, request: &WireRequest) -> Result<WireResponse, IoError> {
    write_frame(conn, request).await?;
    read_frame::<_, WireResponse>(conn)
        .await?
        .ok_or_else(|| IoError::new(ErrorKind::UnexpectedEof, "fabricd closed the connection"))
}

/// Builds a transport/protocol error tagged to the calling capability `source` so the engine
/// classifies it as a (retryable) capability error, exactly like an in-process backend fault.
fn transport_error(source: &str, code: &str, message: &str) -> EgressError {
    EgressError::new(source, code, message.to_owned())
        .retryable()
        .owner(ErrorOwner::Operator)
}
