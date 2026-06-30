//! QUIC transport for the box↔`fabricd` egress link — the network alternative to the local UDS.
//!
//! `fabricd` becomes a shared, replicated cluster service (an egress / credential broker) that many
//! boxes reach over the network, so the box↔daemon hop needs encryption and a server identity the
//! box can trust. This module builds the two `quinn::Endpoint`s; the existing length-prefixed JSON
//! framing ([`crate::wire`]) rides a `quinn` bidirectional stream unchanged (one box-request
//! session = one `open_bi()` stream), so the protocol, name-resolution, metrics, and error mapping
//! are identical to the UDS path.
//!
//! **Security model (see `docs/design/network-fabric.md`, "QUIC remote transport"):** QUIC mandates
//! TLS 1.3, which here provides encryption + anti-MITM via a **pinned self-signed server cert** —
//! the box pins the daemon's certificate by its SHA-256 fingerprint ([`PinnedServerVerifier`]),
//! so there is no CA and no cert manager. Client *identity* is a separate, higher layer (a token in
//! [`crate::wire::WireInit`]); this module does not do client-certificate (mutual-TLS) auth.
//!
//! Everything stays on the one process-wide `aws-lc-rs` rustls provider — no second crypto stack.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{ClientConfig, Endpoint, ServerConfig, TransportConfig};
use rustls::DigitallySignedStruct;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::aws_lc_rs;
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{DistinguishedName, Error as TlsError, SignatureScheme};
use sha2::{Digest as _, Sha256};

/// ALPN protocol token negotiated on the QUIC handshake (both ends must agree). Bumps with any
/// breaking change to the wire framing.
const ALPN: &[u8] = b"fabricd/1";

/// Idle timeout: a connection with no traffic for this long is dropped (the box reconnects on the
/// next request). Bounds a half-dead peer without being so tight that an idle box session churns.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Keep-alive ping interval (well under [`IDLE_TIMEOUT`]) so a genuinely-live but quiet session
/// stays up across the broker.
const KEEP_ALIVE: Duration = Duration::from_secs(10);

/// Daemon-side hardening: cap how many concurrent bidirectional streams (= in-flight box-request
/// sessions) one connection may open. A box multiplexes one stream per request over a single
/// long-lived connection, so this bounds a single box's fan-out and, with the connection cap, the
/// blast radius of a misbehaving or hostile peer once `fabricd` is network-reachable. Enforced by
/// QUIC flow control (the peer can't open beyond it). Harmless on the client (the daemon opens no
/// streams), so it lives in the shared transport config.
const MAX_BIDI_STREAMS: u32 = 256;

/// The SHA-256 fingerprint of a certificate's DER encoding — what the box pins for the daemon.
#[must_use]
pub fn cert_fingerprint(cert_der: &CertificateDer<'_>) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(cert_der.as_ref());
    let digest = hasher.finalize();
    let mut out = [0_u8; 32];
    out.copy_from_slice(digest.as_slice());
    out
}

/// The daemon's TLS material: its certificate chain and matching private key (operator-supplied,
/// typically a single self-signed cert the box pins).
#[derive(Debug)]
pub struct ServerTls {
    /// Certificate chain, leaf first.
    chain: Vec<CertificateDer<'static>>,
    /// Private key for the leaf certificate.
    key: PrivateKeyDer<'static>,
}

impl ServerTls {
    /// Loads the chain and key from PEM bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the PEM can't be parsed or contains no certificate / no private key.
    pub fn from_pem(cert_pem: &[u8], key_pem: &[u8]) -> io::Result<Self> {
        let mut cert_reader = cert_pem;
        let chain = rustls_pemfile::certs(&mut cert_reader).collect::<Result<Vec<_>, _>>()?;
        if chain.is_empty() {
            return Err(io::Error::other("no certificate found in PEM"));
        }
        let mut key_reader = key_pem;
        let key = rustls_pemfile::private_key(&mut key_reader)?
            .ok_or_else(|| io::Error::other("no private key found in PEM"))?;
        Ok(Self { chain, key })
    }

    /// Builds [`ServerTls`] directly from DER (used by tests and any in-process cert generator).
    #[must_use]
    pub const fn from_der(
        chain: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
    ) -> Self {
        Self { chain, key }
    }
}

/// Builds a `quinn` server endpoint bound to `addr`, presenting `tls`.
///
/// # Errors
///
/// Returns an error if the rustls config is invalid (e.g. cert/key mismatch) or the UDP socket
/// can't be bound.
pub fn server_endpoint(addr: SocketAddr, tls: ServerTls) -> io::Result<Endpoint> {
    let provider = Arc::new(aws_lc_rs::default_provider());
    let mut crypto = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(io::Error::other)?
        .with_no_client_auth()
        .with_single_cert(tls.chain, tls.key)
        .map_err(io::Error::other)?;
    crypto.alpn_protocols = vec![ALPN.to_vec()];
    let quic_crypto = QuicServerConfig::try_from(crypto).map_err(io::Error::other)?;
    let mut server_config = ServerConfig::with_crypto(Arc::new(quic_crypto));
    let _ = server_config.transport_config(Arc::new(transport_config()?));
    Endpoint::server(server_config, addr)
}

/// Builds a `quinn` client endpoint (ephemeral local socket) that trusts exactly the daemon whose
/// certificate matches `server_pin` (its SHA-256 fingerprint).
///
/// # Errors
///
/// Returns an error if the rustls config is invalid or the local UDP socket can't be bound.
pub fn client_endpoint(bind: SocketAddr, server_pin: [u8; 32]) -> io::Result<Endpoint> {
    let provider = Arc::new(aws_lc_rs::default_provider());
    let verifier = Arc::new(PinnedServerVerifier {
        provider: Arc::clone(&provider),
        pin: server_pin,
    });
    let mut crypto = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(io::Error::other)?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    crypto.alpn_protocols = vec![ALPN.to_vec()];
    let quic_crypto = QuicClientConfig::try_from(crypto).map_err(io::Error::other)?;
    let mut client_config = ClientConfig::new(Arc::new(quic_crypto));
    let _ = client_config.transport_config(Arc::new(transport_config()?));
    let mut endpoint = Endpoint::client(bind)?;
    endpoint.set_default_client_config(client_config);
    Ok(endpoint)
}

/// Shared QUIC transport tuning (idle timeout + keep-alive + stream caps) for both ends.
fn transport_config() -> io::Result<TransportConfig> {
    let mut transport = TransportConfig::default();
    let idle = quinn::IdleTimeout::try_from(IDLE_TIMEOUT).map_err(io::Error::other)?;
    let _ = transport.max_idle_timeout(Some(idle));
    let _ = transport.keep_alive_interval(Some(KEEP_ALIVE));
    // Cap concurrent bidi streams (daemon hardening; see `MAX_BIDI_STREAMS`) and refuse
    // unidirectional streams outright — the wire protocol uses only bidi streams.
    let _ = transport.max_concurrent_bidi_streams(MAX_BIDI_STREAMS.into());
    let _ = transport.max_concurrent_uni_streams(0_u32.into());
    Ok(transport)
}

/// A rustls server-certificate verifier that trusts exactly one certificate, identified by the
/// SHA-256 fingerprint of its DER encoding (certificate pinning). Signature checks delegate to the
/// `aws-lc-rs` provider's algorithms, so only the *trust anchor* is replaced — not the crypto.
#[derive(Debug)]
struct PinnedServerVerifier {
    /// The crypto provider whose signature algorithms back the TLS handshake checks.
    provider: Arc<CryptoProvider>,
    /// The pinned certificate's SHA-256 DER fingerprint.
    pin: [u8; 32],
}

impl ServerCertVerifier for PinnedServerVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        if cert_fingerprint(end_entity) == self.pin {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(TlsError::General(
                "fabricd server certificate fingerprint does not match the configured pin"
                    .to_owned(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }

    fn root_hint_subjects(&self) -> Option<&[DistinguishedName]> {
        None
    }

    fn requires_raw_public_keys(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    //! A round-trip over a real loopback QUIC connection: a pinned-cert client opens a bidirectional
    //! stream to the server and the existing wire frame survives end to end; a wrong pin is refused.

    use std::net::{Ipv4Addr, SocketAddr};

    use rustls::pki_types::PrivateKeyDer;

    use super::{ServerTls, cert_fingerprint, client_endpoint, server_endpoint};
    use crate::wire::{WireRequest, read_frame, write_frame};

    /// Mints a self-signed cert+key for `localhost`/loopback and returns the DER pair.
    fn self_signed() -> (
        Vec<rustls::pki_types::CertificateDer<'static>>,
        PrivateKeyDer<'static>,
    ) {
        let names = vec!["localhost".to_owned()];
        let certified = rcgen::generate_simple_self_signed(names)
            .unwrap_or_else(|_err| unreachable!("self-signed generation"));
        let cert = certified.cert.der().clone();
        let key = PrivateKeyDer::try_from(certified.key_pair.serialize_der())
            .unwrap_or_else(|_err| unreachable!("key der"));
        (vec![cert], key)
    }

    /// Server cert/key + the client's matching pin, ready to build both endpoints.
    fn endpoints_pin() -> ([u8; 32], ServerTls) {
        let (chain, key) = self_signed();
        let pin = cert_fingerprint(&chain[0]);
        (pin, ServerTls::from_der(chain, key))
    }

    /// The loopback bind address (ephemeral port).
    const fn loopback() -> SocketAddr {
        SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
    }

    /// A pinned client reaches the server and a wire frame round-trips intact.
    #[tokio::test]
    async fn frame_round_trips_over_quic() {
        let (pin, server_tls) = endpoints_pin();
        let server = server_endpoint(loopback(), server_tls)
            .unwrap_or_else(|_err| unreachable!("server endpoint"));
        let server_addr = server
            .local_addr()
            .unwrap_or_else(|_err| unreachable!("server addr"));

        let accept = tokio::spawn(async move {
            let incoming = server
                .accept()
                .await
                .unwrap_or_else(|| unreachable!("an inbound connection"));
            let connection = incoming
                .await
                .unwrap_or_else(|_err| unreachable!("accepted connection"));
            let (_send, mut recv) = connection
                .accept_bi()
                .await
                .unwrap_or_else(|_err| unreachable!("accepted bi-stream"));
            read_frame::<_, WireRequest>(&mut recv)
                .await
                .unwrap_or_else(|_err| unreachable!("read frame"))
        });

        let client =
            client_endpoint(loopback(), pin).unwrap_or_else(|_err| unreachable!("client endpoint"));
        let connection = client
            .connect(server_addr, "localhost")
            .unwrap_or_else(|_err| unreachable!("connect call"))
            .await
            .unwrap_or_else(|_err| unreachable!("handshake"));
        let (mut send, _recv) = connection
            .open_bi()
            .await
            .unwrap_or_else(|_err| unreachable!("open bi-stream"));
        write_frame(&mut send, &WireRequest::Drain)
            .await
            .unwrap_or_else(|_err| unreachable!("write frame"));
        send.finish()
            .unwrap_or_else(|_err| unreachable!("finish stream"));

        let received = accept
            .await
            .unwrap_or_else(|_err| unreachable!("server task"));
        assert!(
            matches!(received, Some(WireRequest::Drain)),
            "the Drain frame survived the QUIC round-trip"
        );
    }

    /// A client pinning the wrong fingerprint is refused at the handshake.
    #[tokio::test]
    async fn wrong_pin_is_refused() {
        let (_good_pin, server_tls) = endpoints_pin();
        let server = server_endpoint(loopback(), server_tls)
            .unwrap_or_else(|_err| unreachable!("server endpoint"));
        let server_addr = server
            .local_addr()
            .unwrap_or_else(|_err| unreachable!("server addr"));
        // Keep accepting so the handshake has a peer to fail against.
        drop(tokio::spawn(async move {
            if let Some(incoming) = server.accept().await {
                drop(incoming.await);
            }
        }));

        let client = client_endpoint(loopback(), [0_u8; 32])
            .unwrap_or_else(|_err| unreachable!("client endpoint"));
        let outcome = client
            .connect(server_addr, "localhost")
            .unwrap_or_else(|_err| unreachable!("connect call"))
            .await;
        assert!(outcome.is_err(), "a wrong pin must fail the handshake");
    }
}
