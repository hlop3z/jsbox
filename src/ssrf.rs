//! Shared SSRF protection — blocks targets that resolve to private/internal IPs.
//!
//! Used by both trust-sensitive capabilities:
//! - `http` validates a **script-controlled** request URL on every call.
//! - `s3` validates the **operator-configured** endpoint host before signing, so a
//!   presigned URL can never name a local/internal target.
//!
//! Keeping the IP classification here (instead of duplicated per module) means the
//! blocklist stays consistent across capabilities.

use std::net::{IpAddr, Ipv4Addr, ToSocketAddrs};

/// Blocks a host that is — or resolves to — a private/internal IP address.
///
/// Literal IPs are checked directly. Hostnames are resolved (one DNS lookup) and
/// every returned address is checked. DNS failure is not fatal here: the eventual
/// connection will fail on its own.
///
/// When `allow_private` is `true` (server `debug` mode), the check is skipped so
/// localhost / LAN targets work for local testing. Production runs with it `false`.
///
/// # Errors
///
/// Returns an error if the host is, or resolves to, a private/internal address.
pub(crate) fn block_private_ip(host: &str, port: u16, allow_private: bool) -> Result<(), String> {
    if allow_private {
        return Ok(());
    }

    // Literal IP check (no DNS needed).
    if let Ok(addr) = host.parse::<IpAddr>() {
        if is_private_ip(&addr) {
            return Err(format!("requests to private IP {addr} are blocked"));
        }
        return Ok(());
    }

    // Resolve hostname and check every address it points at.
    if let Ok(addrs) = (host, port).to_socket_addrs() {
        for sock_addr in addrs {
            if is_private_ip(&sock_addr.ip()) {
                return Err(format!(
                    "host '{host}' resolves to private IP {}, blocked",
                    sock_addr.ip()
                ));
            }
        }
    }
    // DNS failure will surface later when the actual connection is attempted.
    Ok(())
}

/// Returns `true` if the address is private/internal (SSRF protection).
pub(crate) fn is_private_ip(addr: &IpAddr) -> bool {
    match *addr {
        IpAddr::V4(ip) => is_private_v4(ip),
        IpAddr::V6(ip) => {
            ip.is_loopback() || ip.is_unspecified() || ip.to_ipv4_mapped().is_some_and(is_private_v4)
        }
    }
}

/// Returns `true` for loopback, private, link-local, and other non-public IPv4.
const fn is_private_v4(ip: Ipv4Addr) -> bool {
    let [oct_a, oct_b, _, _] = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_unspecified()
        || (oct_a == 100 && oct_b >= 64 && oct_b <= 127) // CGNAT 100.64.0.0/10
}
