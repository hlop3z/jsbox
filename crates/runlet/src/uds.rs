//! Box-side UDS client for the `fabricd` egress sidecar.
//!
//! [`UdsEgress`] implements [`Egress`] (and [`MeteredEgress`]) by forwarding each `io.call(...)`
//! over a Unix-domain socket to `fabricd`, which hosts the real `BackendSet`. One `UdsEgress` owns
//! one connection for the life of a box-request (so a transaction's `begin`→`commit` hit the same
//! daemon-side client), opened with a [`WireInit`] handshake carrying the resolved operator configs
//! + deadline.
//!
//! Every socket round-trip runs via `Handle::block_on` (the engine drives capabilities
//! synchronously on a `spawn_blocking` thread), bounded by the per-execution deadline — the same
//! shape as the in-process async backends. So construct **and** drain this on that blocking thread,
//! never on a runtime worker. See `docs/design/resource-egress.md` step 4b.

use std::io::{Error as IoError, ErrorKind};
use std::time::{Duration, Instant};

use fabric_backends::wire::{
    BackendMetrics, WireCall, WireInit, WireRequest, WireResponse, read_frame, write_frame,
};
use fabric_backends::MeteredEgress;
use fabric_wire::{Egress, EgressError, ErrorOwner};
use tokio::net::UnixStream;
use tokio::runtime::Handle;
use tokio::sync::Mutex;
use tokio::time::timeout;

/// A box-request session to `fabricd` over a Unix-domain socket.
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
    /// Connects to `socket_path`, performs the [`WireInit`] handshake, and anchors the deadline.
    ///
    /// Must run on the `spawn_blocking` thread (it `block_on`s the connect + handshake).
    ///
    /// # Errors
    ///
    /// Returns a [`EgressError`] (source `engine`, retryable) if the socket can't be reached or the
    /// daemon doesn't acknowledge the session.
    pub(crate) fn connect(
        socket_path: &str,
        init: WireInit,
        handle: &Handle,
        budget: Duration,
    ) -> Result<Self, EgressError> {
        let stream = handle.block_on(async {
            let mut stream = UnixStream::connect(socket_path)
                .await
                .map_err(|err| connect_error(&err.to_string()))?;
            write_frame(&mut stream, &WireRequest::Init(Box::new(init)))
                .await
                .map_err(|err| connect_error(&err.to_string()))?;
            match read_frame::<_, WireResponse>(&mut stream).await {
                Ok(Some(WireResponse::Ack)) => Ok(stream),
                Ok(Some(WireResponse::ProtocolError(msg))) => Err(connect_error(&msg)),
                Ok(Some(WireResponse::Reply(_) | WireResponse::Metrics(_))) => {
                    Err(connect_error("unexpected response to Init"))
                }
                Ok(None) => Err(connect_error("daemon closed the connection during handshake")),
                Err(err) => Err(connect_error(&err.to_string())),
            }
        })?;
        let deadline = Instant::now()
            .checked_add(budget)
            .unwrap_or_else(Instant::now);
        Ok(Self {
            handle: handle.clone(),
            conn: Mutex::new(stream),
            deadline,
        })
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
                Ok(Ok(WireResponse::Ack | WireResponse::Metrics(_))) => {
                    Err(transport_error(&source, "IO_PROTOCOL", "unexpected daemon response"))
                }
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
                Ok(WireResponse::Ack | WireResponse::Reply(_) | WireResponse::ProtocolError(_))
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

/// Builds the connect-failure error (source `engine`, retryable) the caller uses to decide on the
/// in-process fallback — it never reaches a script.
fn connect_error(message: &str) -> EgressError {
    EgressError::new(
        "engine",
        "IO_CONNECT",
        format!("fabricd connect failed: {message}"),
    )
    .retryable()
}

/// Builds a transport/protocol error tagged to the calling capability `source` so the engine
/// classifies it as a (retryable) capability error, exactly like an in-process backend fault.
fn transport_error(source: &str, code: &str, message: &str) -> EgressError {
    EgressError::new(source, code, message.to_owned())
        .retryable()
        .owner(ErrorOwner::Operator)
}
