//! Shared SSRF protection — blocks targets that resolve to private/internal IPs.
//!
//! Used by both trust-sensitive capabilities:
//! - `http` validates a **script-controlled** request URL on every call.
//! - `s3` validates the **operator-configured** endpoint host before signing, so a
//!   presigned URL can never name a local/internal target.
//!
//! Keeping the IP classification here (instead of duplicated per module) means the
//! blocklist stays consistent across capabilities.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};

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
        IpAddr::V6(ip) => is_private_v6(ip),
    }
}

/// Returns `true` for non-public IPv6: loopback, unique-local, link-local, and any form
/// embedding a private IPv4 (v4-mapped, 6to4, NAT64).
///
/// `std`'s `Ipv6Addr::is_unique_local` / `is_unicast_link_local` are still unstable, so the
/// ranges are matched directly on the segments — an attacker on an IPv6 network reaches
/// internal hosts via ULA (`fd00::…`) or smuggles a private v4 through 6to4 / NAT64, and the
/// previous filter (loopback + v4-mapped only) let all of those through.
fn is_private_v6(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return true;
    }
    // IPv4-mapped `::ffff:a.b.c.d` — re-check the embedded v4.
    if ip.to_ipv4_mapped().is_some_and(is_private_v4) {
        return true;
    }
    let [seg0, ..] = ip.segments();
    // Unique-local fc00::/7 (covers the common fd00::/8 deployments).
    let unique_local = (seg0 & 0xfe00) == 0xfc00;
    // Link-local fe80::/10 (the IPv6 path to host-local / metadata services).
    let link_local = (seg0 & 0xffc0) == 0xfe80;
    unique_local || link_local || v6_embeds_private_v4(ip)
}

/// Returns `true` if `ip` carries a private IPv4 inside a public-looking IPv6 literal via
/// 6to4 (`2002:a.b.c.d::/48`) or NAT64 (`64:ff9b::a.b.c.d`) — both can smuggle a private v4
/// past a naïve "looks public" check, so they are classified by the embedded address.
const fn v6_embeds_private_v4(ip: Ipv6Addr) -> bool {
    let [seg0, seg1, ..] = ip.segments();
    let [
        _,
        _,
        sx0,
        sx1,
        sx2,
        sx3,
        _,
        _,
        _,
        _,
        _,
        _,
        nx0,
        nx1,
        nx2,
        nx3,
    ] = ip.octets();
    let sixtofour = seg0 == 0x2002 && is_private_v4(Ipv4Addr::new(sx0, sx1, sx2, sx3));
    let nat64 =
        seg0 == 0x0064 && seg1 == 0xff9b && is_private_v4(Ipv4Addr::new(nx0, nx1, nx2, nx3));
    sixtofour || nat64
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

#[cfg(test)]
mod tests {
    //! SSRF classification — the IPv4 ranges plus the IPv6 ranges that the v4-mapped-only
    //! filter used to miss (ULA, link-local, 6to4 / NAT64 embedded private v4).

    use super::{block_private_ip, is_private_ip};
    use std::net::IpAddr;

    /// Parses a literal into an `IpAddr` for the table-driven cases.
    fn ip(text: &str) -> IpAddr {
        text.parse()
            .unwrap_or_else(|_err| unreachable!("test literal must parse: {text}"))
    }

    /// IPv4 loopback / private / link-local / CGNAT are all classified private.
    #[test]
    fn v4_private_ranges_blocked() {
        for literal in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.1.1",
            "172.16.0.1",
            "169.254.169.254", // cloud metadata
            "100.64.0.1",      // CGNAT
            "0.0.0.0",
        ] {
            assert!(is_private_ip(&ip(literal)), "must be private: {literal}");
        }
    }

    /// A routable public v4 is allowed.
    #[test]
    fn v4_public_allowed() {
        assert!(!is_private_ip(&ip("8.8.8.8")), "public v4 is allowed");
        assert!(!is_private_ip(&ip("1.1.1.1")), "public v4 is allowed");
    }

    /// IPv6 loopback, unique-local, and link-local are classified private — the ranges the
    /// old filter let through.
    #[test]
    fn v6_internal_ranges_blocked() {
        for literal in [
            "::1",              // loopback
            "::",               // unspecified
            "fd00::1",          // ULA (fc00::/7)
            "fc00::1",          // ULA boundary
            "fe80::1",          // link-local
            "febf::1",          // link-local boundary
            "::ffff:127.0.0.1", // v4-mapped loopback
            "::ffff:10.0.0.1",  // v4-mapped private
        ] {
            assert!(is_private_ip(&ip(literal)), "must be private: {literal}");
        }
    }

    /// A private v4 smuggled through 6to4 or NAT64 is unwrapped and blocked.
    #[test]
    fn v6_embedded_private_v4_blocked() {
        // 6to4 of 192.168.1.1 → 2002:c0a8:0101::
        assert!(
            is_private_ip(&ip("2002:c0a8:0101::1")),
            "6to4 of a private v4 is blocked"
        );
        // NAT64 of 127.0.0.1 → 64:ff9b::7f00:1
        assert!(
            is_private_ip(&ip("64:ff9b::7f00:1")),
            "NAT64 of loopback is blocked"
        );
    }

    /// A genuine public v6 (and a 6to4/NAT64 wrapping a *public* v4) is allowed.
    #[test]
    fn v6_public_allowed() {
        assert!(
            !is_private_ip(&ip("2606:4700:4700::1111")),
            "public v6 is allowed"
        );
        // 6to4 of 8.8.8.8 → 2002:0808:0808:: must NOT be blocked.
        assert!(
            !is_private_ip(&ip("2002:0808:0808::1")),
            "6to4 of a public v4 is allowed"
        );
    }

    /// `allow_private` (debug mode) short-circuits the block for local testing.
    #[test]
    fn debug_relaxation_allows_private() {
        assert!(
            block_private_ip("127.0.0.1", 80, true).is_ok(),
            "debug relaxes v4 loopback"
        );
        assert!(
            block_private_ip("fd00::1", 80, true).is_ok(),
            "debug relaxes v6 ULA"
        );
        assert!(
            block_private_ip("10.0.0.1", 5432, false).is_err(),
            "production blocks private v4"
        );
        assert!(
            block_private_ip("fe80::1", 80, false).is_err(),
            "production blocks v6 link-local"
        );
    }
}
