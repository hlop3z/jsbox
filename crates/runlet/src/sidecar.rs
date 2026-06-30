//! Box-side egress client for the `fabricd` sidecar — over a **local UDS** or a **remote QUIC**
//! link.
//!
//! The box links no driver and holds no credentials: a request that names driver resources in
//! `config.io` opens a session to `fabricd`, sends the selected logical *names* (+ a per-execution
//! deadline and, on the QUIC path, an auth token), and forwards each `io.call(...)`; `fabricd`
//! resolves the names to operator credentials and performs the I/O.
//!
//! Two transports behind one [`SessionConn`]:
//!
//! - **UDS** ([`SidecarTransport::Uds`]) — the zero-config local default; filesystem permissions
//!   gate it, so no token is sent.
//! - **QUIC** ([`SidecarTransport::Quic`]) — the remote path for a shared `fabricd` cluster
//!   service. One client endpoint multiplexes a session per box-request as a bidirectional stream;
//!   the box pins the daemon's self-signed cert and presents an auth token. The client keeps one
//!   long-lived connection per healthy replica, **failing over** across the configured replica set
//!   and reconnecting on drop. See `docs/design/network-fabric.md` (QUIC remote transport).
//!
//! Every round-trip runs via `Handle::block_on` (the engine drives capabilities synchronously on a
//! `spawn_blocking` thread), bounded by the per-execution deadline — so build and drain the egress
//! on that blocking thread, never on a runtime worker.

use std::fs;
use std::io::{Error as IoError, ErrorKind};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fabric_wire::quic::client_endpoint;
use fabric_wire::wire::{WireCall, WireInit, WireRequest, WireResponse, read_frame, write_frame};
use fabric_wire::{BackendMetrics, Egress, EgressError, ErrorOwner, MeteredEgress};
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use tokio::net::{UnixStream, lookup_host};
use tokio::runtime::Handle;
use tokio::sync::Mutex;
use tokio::time::timeout;

use crate::config::FabricdQuic;

/// How the box reaches `fabricd`. Selected once at startup from config and shared (cheaply cloned)
/// into [`crate::handler::AppState`].
#[derive(Debug, Clone)]
pub(crate) enum SidecarTransport {
    /// No sidecar configured — driver-backed capabilities are unavailable (`503`).
    None,
    /// Local Unix-domain socket (the zero-config default).
    Uds(Arc<str>),
    /// Remote QUIC: a shared client endpoint, the replica set, cert pin, and auth.
    Quic(Arc<QuicClient>),
}

impl SidecarTransport {
    /// Selects the transport from config: QUIC when `fabricd_quic` is set, else the local UDS
    /// `socket`, else none. (QUIC wins when both are set — a remote broker supersedes a local
    /// socket.)
    ///
    /// # Errors
    ///
    /// Returns an error if the QUIC config is invalid (empty replica set, malformed cert pin,
    /// both/neither auth source) or its client endpoint can't be built.
    pub(crate) fn from_config(
        socket: Option<&str>,
        quic: Option<&FabricdQuic>,
    ) -> Result<Self, IoError> {
        if let Some(quic_cfg) = quic {
            if quic_cfg.replicas.is_empty() {
                return Err(IoError::other("fabricd_quic.replicas must not be empty"));
            }
            return Ok(Self::Quic(Arc::new(QuicClient::from_config(quic_cfg)?)));
        }
        socket.map_or(Ok(Self::None), |path| Ok(Self::Uds(Arc::from(path))))
    }

    /// A short, secret-free label for startup logging.
    pub(crate) const fn label(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Uds(_) => "uds",
            Self::Quic(_) => "quic",
        }
    }
}

/// How the box proves to a remote `fabricd` that it may pull credentials.
#[derive(Debug, Clone)]
pub(crate) enum BoxAuth {
    /// No token (only valid when the daemon's auth provider is disabled).
    None,
    /// A static opaque shared secret from config.
    Static(Arc<str>),
    /// A k8s projected `ServiceAccount` token file, re-read per session (the kubelet rotates it).
    ServiceAccountFile(Arc<Path>),
}

impl BoxAuth {
    /// Resolves the current token to send in [`WireInit`]. The SA-file variant re-reads on each
    /// call so a rotated token is always fresh.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the `ServiceAccount` token file can't be read.
    fn token(&self) -> Result<Option<String>, IoError> {
        match self {
            Self::None => Ok(None),
            Self::Static(secret) => Ok(Some((**secret).to_owned())),
            Self::ServiceAccountFile(path) => Ok(Some(fs::read_to_string(path)?.trim().to_owned())),
        }
    }
}

/// A QUIC client for a shared `fabricd` cluster service: one endpoint (one local UDP socket) that
/// trusts the daemon's pinned cert, the configured replica addresses, and the auth token. Holds one
/// live [`Connection`] for reuse, reconnecting / failing over across replicas as needed.
#[derive(Debug)]
pub(crate) struct QuicClient {
    /// The shared client endpoint (its default client config pins the daemon cert).
    endpoint: Endpoint,
    /// Configured replica endpoints (`host:port`); a headless-Service DNS name resolves to many
    /// pod addresses, all tried in turn.
    replicas: Vec<String>,
    /// TLS server name presented on the handshake (the daemon cert's name).
    server_name: Arc<str>,
    /// Client-auth token provider.
    auth: BoxAuth,
    /// The current live connection, lazily (re)established and shared across sessions.
    connection: Mutex<Option<Connection>>,
}

impl QuicClient {
    /// Builds a client over `endpoint` for the given `replicas`, server name, and auth.
    fn new(endpoint: Endpoint, replicas: Vec<String>, server_name: &str, auth: BoxAuth) -> Self {
        Self {
            endpoint,
            replicas,
            server_name: Arc::from(server_name),
            auth,
            connection: Mutex::new(None),
        }
    }

    /// Builds a client from validated config: parses the pinned cert fingerprint, builds the QUIC
    /// client endpoint (ephemeral local UDP socket, trusting that one cert), and selects the auth.
    ///
    /// # Errors
    ///
    /// Returns an error if the cert pin is malformed, both/neither auth source is set, or the
    /// endpoint can't be built.
    fn from_config(quic: &FabricdQuic) -> Result<Self, IoError> {
        let pin = parse_pin(&quic.server_cert_pin)?;
        let bind: SocketAddr = "0.0.0.0:0".parse().map_err(IoError::other)?;
        let endpoint = client_endpoint(bind, pin)?;
        let auth = build_auth(quic)?;
        Ok(Self::new(
            endpoint,
            quic.replicas.clone(),
            &quic.server_name,
            auth,
        ))
    }

    /// Opens a fresh bidirectional stream for one box-request session, reconnecting once if the
    /// cached connection has gone stale.
    async fn open_session(&self) -> Result<(SendStream, RecvStream), IoError> {
        let connection = self.live_connection().await?;
        match connection.open_bi().await {
            Ok(pair) => Ok(pair),
            Err(_stale) => {
                self.invalidate().await;
                let reconnected = self.live_connection().await?;
                reconnected.open_bi().await.map_err(IoError::other)
            }
        }
    }

    /// Returns a live connection: the cached one if still open, otherwise a freshly established one.
    async fn live_connection(&self) -> Result<Connection, IoError> {
        let mut guard = self.connection.lock().await;
        if let Some(existing) = guard.as_ref()
            && existing.close_reason().is_none()
        {
            return Ok(existing.clone());
        }
        let fresh = self.connect_any().await?;
        *guard = Some(fresh.clone());
        drop(guard);
        Ok(fresh)
    }

    /// Drops the cached connection so the next session reconnects.
    async fn invalidate(&self) {
        let mut guard = self.connection.lock().await;
        *guard = None;
    }

    /// Connects to the first reachable replica (client-side failover across the resolved set).
    async fn connect_any(&self) -> Result<Connection, IoError> {
        let mut last_error: Option<IoError> = None;
        for replica in &self.replicas {
            let addresses = match lookup_host(replica.as_str()).await {
                Ok(addresses) => addresses,
                Err(err) => {
                    last_error = Some(err);
                    continue;
                }
            };
            for address in addresses {
                match self.dial(address).await {
                    Ok(connection) => return Ok(connection),
                    Err(err) => last_error = Some(err),
                }
            }
        }
        Err(last_error
            .unwrap_or_else(|| IoError::other("no fabricd replica addresses to connect to")))
    }

    /// Dials one resolved replica address and completes the handshake.
    async fn dial(&self, address: SocketAddr) -> Result<Connection, IoError> {
        self.endpoint
            .connect(address, &self.server_name)
            .map_err(IoError::other)?
            .await
            .map_err(IoError::other)
    }
}

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

/// One box-request session connection to `fabricd`, over either transport. A QUIC session is one
/// bidirectional stream (separate send/recv halves); a UDS session is the duplex socket.
#[derive(Debug)]
pub(crate) enum SessionConn {
    /// A Unix-domain-socket session (read and write share the duplex stream).
    Uds(UnixStream),
    /// A QUIC bidirectional-stream session.
    Quic {
        /// Outbound half (frames to the daemon).
        send: SendStream,
        /// Inbound half (frames from the daemon).
        recv: RecvStream,
    },
}

impl SessionConn {
    /// Writes one request frame on the session's outbound half.
    async fn write(&mut self, request: &WireRequest) -> Result<(), IoError> {
        match self {
            Self::Uds(stream) => write_frame(stream, request).await,
            Self::Quic { send, .. } => write_frame(send, request).await,
        }
    }

    /// Reads one response frame from the session's inbound half (`None` = clean EOF).
    async fn read(&mut self) -> Result<Option<WireResponse>, IoError> {
        match self {
            Self::Uds(stream) => read_frame::<_, WireResponse>(stream).await,
            Self::Quic { recv, .. } => read_frame::<_, WireResponse>(recv).await,
        }
    }
}

/// Opens a `fabricd` session over the configured transport and performs the [`WireInit`] handshake
/// — sending the selected logical resource *names*, the deadline, and (on QUIC) the auth token, and
/// surfacing a name-resolution failure as a [`SessionError`] the handler maps to a status.
///
/// Async — run on the request's async task (no `block_on`), so a name-resolution `400` or an
/// unreachable-sidecar `503` is decided before the blocking execution is admitted.
///
/// # Errors
///
/// Returns a [`SessionError`]: `Unavailable` (no transport / unreachable / closed), `Resolve` (a
/// name failed to resolve), or `Protocol` (an unexpected response).
pub(crate) async fn connect_session(
    transport: &SidecarTransport,
    init: &WireInit,
) -> Result<SessionConn, SessionError> {
    match transport {
        SidecarTransport::None => Err(SessionError::Unavailable(
            "no fabricd egress sidecar configured".to_owned(),
        )),
        SidecarTransport::Uds(path) => {
            let stream = UnixStream::connect(path.as_ref())
                .await
                .map_err(|err| SessionError::Unavailable(format!("fabricd unreachable: {err}")))?;
            // UDS is gated by filesystem permissions — never send a token.
            handshake(SessionConn::Uds(stream), init).await
        }
        SidecarTransport::Quic(client) => {
            let token = client.auth.token().map_err(|err| {
                SessionError::Unavailable(format!("auth token unreadable: {err}"))
            })?;
            let (send, recv) = client
                .open_session()
                .await
                .map_err(|err| SessionError::Unavailable(format!("fabricd unreachable: {err}")))?;
            let mut authed = init.clone();
            authed.token = token;
            handshake(SessionConn::Quic { send, recv }, &authed).await
        }
    }
}

/// Sends the `Init` frame on a freshly-opened session and interprets the daemon's response.
async fn handshake(mut conn: SessionConn, init: &WireInit) -> Result<SessionConn, SessionError> {
    conn.write(&WireRequest::Init(Box::new(init.clone())))
        .await
        .map_err(|err| SessionError::Unavailable(format!("fabricd write failed: {err}")))?;
    match conn.read().await {
        Ok(Some(WireResponse::Ack)) => Ok(conn),
        Ok(Some(WireResponse::InitError { code, message })) => {
            Err(SessionError::Resolve { code, message })
        }
        Ok(Some(
            WireResponse::Reply(_) | WireResponse::Metrics(_) | WireResponse::ProtocolError(_),
        )) => Err(SessionError::Protocol(
            "unexpected response to Init".to_owned(),
        )),
        Ok(None) => Err(SessionError::Unavailable(
            "fabricd closed the connection during handshake".to_owned(),
        )),
        Err(err) => Err(SessionError::Unavailable(format!(
            "fabricd handshake read failed: {err}"
        ))),
    }
}

/// A box-request session to `fabricd` over an already-handshaked [`SessionConn`] (UDS or QUIC).
#[derive(Debug)]
pub(crate) struct SidecarEgress {
    /// Runtime handle to drive the socket I/O via `block_on` (on the `spawn_blocking` thread).
    handle: Handle,
    /// The session connection — `tokio::sync::Mutex` so the `&self` egress can take `&mut` access
    /// across the write+read await without tripping `await_holding_lock`.
    conn: Mutex<SessionConn>,
    /// Absolute per-execution deadline bounding every round-trip.
    deadline: Instant,
}

impl SidecarEgress {
    /// Wraps an already-handshaked session (from [`connect_session`]) and anchors the deadline.
    /// Build this on the `spawn_blocking` thread — its calls `block_on`.
    pub(crate) fn new(conn: SessionConn, handle: Handle, budget: Duration) -> Self {
        let deadline = Instant::now()
            .checked_add(budget)
            .unwrap_or_else(Instant::now);
        Self {
            handle,
            conn: Mutex::new(conn),
            deadline,
        }
    }
}

impl Egress for SidecarEgress {
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

impl MeteredEgress for SidecarEgress {
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
async fn roundtrip(conn: &mut SessionConn, request: &WireRequest) -> Result<WireResponse, IoError> {
    conn.write(request).await?;
    conn.read()
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

/// Decodes the configured server-cert pin (64 hex chars) into the 32-byte SHA-256 fingerprint.
fn parse_pin(hex_pin: &str) -> Result<[u8; 32], IoError> {
    let bytes = hex::decode(hex_pin.trim())
        .map_err(|err| IoError::other(format!("invalid server_cert_pin hex: {err}")))?;
    <[u8; 32]>::try_from(bytes.as_slice())
        .map_err(|_err| IoError::other("server_cert_pin must be 32 bytes (64 hex chars)"))
}

/// Selects the box's auth token source: exactly one of a static secret or a `ServiceAccount` token
/// file (or neither, when the daemon's auth provider is disabled).
fn build_auth(quic: &FabricdQuic) -> Result<BoxAuth, IoError> {
    match (quic.auth_token.as_deref(), quic.auth_token_file.as_deref()) {
        (Some(_token), Some(_path)) => Err(IoError::other(
            "set only one of fabricd_quic.auth_token / auth_token_file",
        )),
        (Some(token), None) => Ok(BoxAuth::Static(Arc::from(token))),
        (None, Some(path)) => Ok(BoxAuth::ServiceAccountFile(Arc::from(path))),
        (None, None) => Ok(BoxAuth::None),
    }
}
