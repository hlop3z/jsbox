//! Server configuration loaded from an optional `config.json` file.
//!
//! The HTTP front's own config: bind address, the `/execute` auth gate, and the script/
//! module directories — plus the embedded [`EngineConfig`] sandbox limits owned by
//! `runlet-core`. All fields have sensible defaults; a missing file starts with defaults.
//!
//! Size fields accept human-readable strings: `"8mb"`, `"256kb"`, `"1gb"`,
//! or plain numbers in bytes: `8388608`.

use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use runlet_core::config::EngineConfig;
use serde::Deserialize;

use crate::quota::PlanLimit;

/// Top-level configuration. `Default` is derived — every field's default is its type
/// default (`false` / `None` / the nested config's own `Default`), including the
/// security-relevant `error_debug: false` (secure by default) and `access_token: None`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Config {
    /// Local-dev switch. When `true`, the SSRF private-IP block is relaxed so
    /// localhost / LAN targets (e.g. `MinIO`) work for `s3` and `api`. Never enable in
    /// production — it removes the guard against internal/local targets.
    pub(crate) debug: bool,
    /// Include `error.debug` (stack traces + raw driver causes) in responses. Default
    /// `false` (secure by default): the raw cause can carry internal hostnames / driver
    /// detail, so an operator running purely internally opts *in* to the verbosity. The
    /// `trace_id` is always present and the raw cause is always logged server-side, so
    /// support can correlate without leaking detail across the boundary. Kept separate from
    /// `debug` (which only relaxes the SSRF guard) so the two don't entangle.
    pub(crate) error_debug: bool,
    /// Server configuration.
    pub(crate) server: ServerConfig,
    /// JS engine sandbox limits.
    pub(crate) engine: EngineConfig,
    /// Directory of registered scripts (`*.js`), loaded once at startup; a script's
    /// key is its relative path without the extension (`acme/billing/pricing.js` →
    /// `acme/billing/pricing`). Omit to disable execute-by-key (`key` requests then
    /// fail with `SCRIPT_NOT_FOUND`).
    pub(crate) scripts_dir: Option<PathBuf>,
    /// Directory of injectable ES modules (`*.js` / `*.mjs`), loaded once at startup; a
    /// module's specifier is its relative path without the extension (`acme/pricing.mjs`
    /// → `acme/pricing`). A handler `import`s them by that specifier. Omit to disable
    /// `import` (any `import` of a module then fails to resolve).
    pub(crate) modules_dir: Option<PathBuf>,
    /// Shared-secret bearer token gating `/execute`. When set, a request must carry
    /// `Authorization: Bearer <token>` (constant-time compared) or it is rejected `401
    /// UNAUTHORIZED`. `/health` and `/metrics` stay open (probe/scrape paths). This is
    /// defense in depth behind the gateway, not a replacement for it — the `/execute` caller
    /// is fully trusted (it supplies credentials), so an unauthenticated reachable port is a
    /// full compromise. Omit only when auth is genuinely terminated upstream (see
    /// `allow_unauthenticated`).
    #[serde(default)]
    pub(crate) access_token: Option<String>,
    /// Explicit acknowledgement that `/execute` may run without a token on a non-loopback
    /// bind (auth handled by an upstream gateway/mesh). Default `false`: jsbox **refuses to
    /// start** on a non-loopback address when no `access_token` is set, so a misconfigured
    /// deployment fails closed instead of silently exposing an unauthenticated executor. A
    /// loopback bind never needs this.
    #[serde(default)]
    pub(crate) allow_unauthenticated: bool,
    /// Path to the `fabricd` egress sidecar's Unix-domain socket. **Required** to use any
    /// driver-backed capability (`db`/`mongo`/`mail`/`redis`/`amq`/`auth`): the box links no
    /// driver and holds no credentials — it sends the request's `config.io` logical names to
    /// `fabricd`, which resolves them against its own operator config and performs the I/O. Omit
    /// when the deployment serves only deterministic / `http` / `s3` capabilities; a request that
    /// names a driver resource with no `fabricd_socket` set is rejected `503 EGRESS_UNAVAILABLE`.
    /// See `docs/design/resource-egress.md` step 5.
    #[serde(default)]
    pub(crate) fabricd_socket: Option<String>,
    /// Remote `fabricd` over QUIC — the alternative to `fabricd_socket` for a shared `fabricd`
    /// cluster service on a different host. When set (and `fabricd_socket` is not), driver-backed
    /// capabilities route over QUIC to one of the configured replicas. See
    /// `docs/design/network-fabric.md` (QUIC remote transport).
    #[serde(default)]
    pub(crate) fabricd_quic: Option<FabricdQuic>,
    /// Trusted-identity ("nexus edge") mode — off by default. When enabled the box consumes
    /// trusted identity headers the edge injects (tenant/user/roles/entitlements/suspended/
    /// anonymous), keys fairness + cache + egress + quota off the trusted tenant id, and rejects
    /// anonymous/suspended principals. Requires network isolation (see the boot guard) because the
    /// box then blindly trusts `x-*`. See `docs/design/multitenant-trust.md`.
    #[serde(default)]
    pub(crate) trusted: TrustedConfig,
    /// Distributed-tracing + structured-logging config (the `telemetry` block). Off by default:
    /// with no `otlp_endpoint` the box emits structured JSON logs only (no OTLP export). See
    /// `telemetry.rs` and `docs/design/nexus-upstream-requirements.md` (N6).
    #[serde(default)]
    pub(crate) telemetry: TelemetryConfig,
}

/// Distributed-tracing config (the `telemetry` block). Metrics stay Prometheus PULL; this block
/// governs only trace export + the log/service name. Tracing is enabled iff `otlp_endpoint` is set.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub(crate) struct TelemetryConfig {
    /// OTLP/gRPC collector endpoint (e.g. `http://localhost:4317`). `None` (default) ⇒ tracing
    /// disabled; the box still emits structured JSON logs to stdout. Plaintext by default (a
    /// local/in-pod collector terminates TLS to the backend, not the box).
    pub(crate) otlp_endpoint: Option<String>,
    /// Sampling ratio in `[0.0, 1.0]` for box-started root spans (a parent `traceparent` decision
    /// is always honored). Default `1.0` — sample every self-rooted trace.
    pub(crate) sample_ratio: f64,
    /// `service.name` resource attribute reported to the collector. Default `runlet`.
    pub(crate) service_name: String,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            otlp_endpoint: None,
            sample_ratio: 1.0,
            service_name: "runlet".to_owned(),
        }
    }
}

/// Trusted-identity mode configuration (the `trusted` block).
///
/// `Default` (all off / empty) preserves the pre-change single-principal, caller-asserted-partition
/// behavior: `enabled: false` means no header is trusted and `/execute` behaves exactly as before.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct TrustedConfig {
    /// Turn on trusted-header identity mode. When `false`, no `x-*` identity header is read and the
    /// caller-asserted `X-Partition-Key` path stays active (single-tenant behavior).
    pub(crate) enabled: bool,
    /// Operator assertion that this bind is reachable **only** through the edge (enforced out of
    /// band by a k8s `NetworkPolicy`). Required to run trusted-header mode on a non-loopback bind —
    /// the box trusts `x-*` blindly, so an exposed bind without this fails closed (see
    /// [`Config::check_exposure`]). Mirrors `allow_unauthenticated`.
    pub(crate) assert_network_isolation: bool,
    /// The trusted header names (defaults `x-tenant-id`/`x-user-*`/`x-auth-anonymous`/`x-tenant-plan`).
    pub(crate) headers: TrustedHeaders,
    /// Coarse member-capability gate: capability kind (`"db"`, `"mongo"`, …) → the entitlement (or
    /// role) a caller must hold in `x-user-entitlements`/`x-user-roles` to invoke it. A kind absent
    /// from this map is ungated. Empty by default (no member gating).
    pub(crate) capability_entitlements: HashMap<String, String>,
    /// Per-tenant plan-gated quota (section 6). Off by default.
    pub(crate) quota: QuotaConfig,
}

/// The configurable trusted-header names. Defaults match the nexus edge contract; every name is
/// overridable so a drift between the edge and the box is pinned in one place.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub(crate) struct TrustedHeaders {
    /// Tenant / acting-workspace id header (default `x-tenant-id`).
    pub(crate) tenant: String,
    /// User id header, for audit (default `x-user-id`).
    pub(crate) user: String,
    /// Comma-separated roles header (default `x-user-roles`).
    pub(crate) roles: String,
    /// Comma-separated entitlements header (default `x-user-entitlements`).
    pub(crate) entitlements: String,
    /// Suspended-principal flag header (default `x-user-suspended`).
    pub(crate) suspended: String,
    /// Anonymous-caller flag header (default `x-auth-anonymous`).
    pub(crate) anonymous: String,
    /// Tenant plan header, selecting the quota tier (default `x-tenant-plan`).
    pub(crate) plan: String,
    /// Acting-org assurance header (default `x-tenant-scope`). The edge asserts, per request, that
    /// the tenant id is the caller's *authorized acting org* by setting this to `acting`; a
    /// tenant-scoped request whose value is not `acting` is rejected fail-closed (nexus upstream
    /// requirement N5). See `docs/design/multitenant-trust.md`.
    pub(crate) scope: String,
}

impl Default for TrustedHeaders {
    fn default() -> Self {
        Self {
            tenant: "x-tenant-id".to_owned(),
            user: "x-user-id".to_owned(),
            roles: "x-user-roles".to_owned(),
            entitlements: "x-user-entitlements".to_owned(),
            suspended: "x-user-suspended".to_owned(),
            anonymous: "x-auth-anonymous".to_owned(),
            plan: "x-tenant-plan".to_owned(),
            scope: "x-tenant-scope".to_owned(),
        }
    }
}

/// Per-tenant plan-gated quota configuration (the `trusted.quota` block).
///
/// Off by default. When `enabled`, every tenant-scoped request is gated: the tenant's plan (from
/// the trusted plan header) selects a [`PlanLimit`], and the tenant's in-flight usage is capped at
/// it. An unknown plan resolves to the most restrictive configured limit, and an **empty** `plans`
/// map denies (fail-closed) — a misconfiguration never grants unbounded usage.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct QuotaConfig {
    /// Consult the quota engine. `false` (default) disables all quota checks.
    pub(crate) enabled: bool,
    /// Plan name → its limit. Empty while `enabled` denies every request (fail-closed).
    pub(crate) plans: HashMap<String, PlanLimit>,
}

/// Remote-`fabricd` QUIC transport settings (the box client side).
///
/// The box pins the daemon's self-signed certificate by fingerprint (no CA / cert manager) and
/// presents an auth token; `fabricd` validates the token and resolves the logical names operator-
/// side. Exactly one of `auth_token` / `auth_token_file` is the credential; omit both only when the
/// daemon's auth provider is disabled.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FabricdQuic {
    /// Replica endpoints to dial (`host:port`); a headless-Service DNS name resolving to many pod
    /// addresses is tried in turn (client-side failover). At least one is required.
    pub(crate) replicas: Vec<String>,
    /// TLS server name presented on the handshake — must match the daemon certificate's name.
    pub(crate) server_name: String,
    /// The daemon certificate's pinned SHA-256 fingerprint, hex-encoded (64 hex chars). The box
    /// trusts exactly this certificate.
    pub(crate) server_cert_pin: String,
    /// A static opaque shared-secret token (mutually exclusive with `auth_token_file`).
    #[serde(default)]
    pub(crate) auth_token: Option<String>,
    /// Path to a k8s projected `ServiceAccount` token file, re-read per session as it rotates
    /// (mutually exclusive with `auth_token`).
    #[serde(default)]
    pub(crate) auth_token_file: Option<PathBuf>,
}

/// HTTP server settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub(crate) struct ServerConfig {
    /// Address to bind to.
    pub(crate) host: IpAddr,
    /// Port to listen on.
    pub(crate) port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 3000,
        }
    }
}

impl ServerConfig {
    /// Returns the socket address from host + port.
    pub(crate) const fn addr(&self) -> SocketAddr {
        SocketAddr::new(self.host, self.port)
    }
}

impl Config {
    /// Fail-closed start gate: refuse to bind a **non-loopback** address with no
    /// `access_token` unless the operator explicitly set `allow_unauthenticated` (auth
    /// terminated upstream). A loopback bind is always fine. Keeps a misconfigured
    /// deployment from silently exposing an unauthenticated arbitrary-code executor.
    ///
    /// # Errors
    ///
    /// Returns an error describing the missing gate when the bind is exposed and neither a
    /// token nor the explicit opt-out is present.
    pub(crate) fn check_exposure(&self) -> Result<(), Box<dyn Error + Send + Sync>> {
        let exposed = !self.server.host.is_loopback();
        if exposed && self.access_token.is_none() && !self.allow_unauthenticated {
            return Err(format!(
                "refusing to start: binding {host} (non-loopback) with no `access_token` and \
                 `allow_unauthenticated` unset. /execute runs caller-supplied code with \
                 caller-supplied credentials, so an unauthenticated reachable port is a full \
                 compromise. Set `access_token`, bind loopback, or set \
                 `allow_unauthenticated: true` if auth is terminated upstream.",
                host = self.server.host,
            )
            .into());
        }
        self.check_trusted_isolation(exposed)?;
        Ok(())
    }

    /// Trusted-mode safety net (D2): trusting `x-*` identity headers rests on the box being
    /// reachable **only** through the edge. Refuse to start in trusted-header mode on a non-loopback
    /// bind unless the operator has asserted network isolation — mirroring the `allow_unauthenticated`
    /// guard, because there is no TLS/JWT check to fall back on once headers are trusted.
    ///
    /// # Errors
    ///
    /// Returns an error when trusted mode is enabled, the bind is exposed, and isolation is not
    /// asserted.
    fn check_trusted_isolation(&self, exposed: bool) -> Result<(), Box<dyn Error + Send + Sync>> {
        if self.trusted.enabled && exposed && !self.trusted.assert_network_isolation {
            return Err(format!(
                "refusing to start: trusted-header mode is enabled on {host} (non-loopback) but \
                 `trusted.assert_network_isolation` is unset. The box then trusts `x-*` identity \
                 headers blindly, so it must be reachable only through the edge (enforce with a \
                 NetworkPolicy). Bind loopback, or set `trusted.assert_network_isolation: true` \
                 once the isolation is in place.",
                host = self.server.host,
            )
            .into());
        }
        Ok(())
    }

    /// Loads config from a file path. Returns defaults if the file doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be read or parsed, or if the
    /// resolved limits violate the parse-headroom invariant (see [`EngineConfig::resolve_limits`]).
    pub(crate) fn load(path: &Path) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let mut config = if path.exists() {
            let contents = fs::read_to_string(path)?;
            serde_json::from_str::<Self>(&contents)?
        } else {
            Self::default()
        };
        config.engine.resolve_limits()?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    //! The fail-closed exposure gate (`check_exposure`): a non-loopback bind requires either a
    //! token or the explicit `allow_unauthenticated` opt-out.

    use super::{Config, ServerConfig};
    use std::net::{IpAddr, Ipv4Addr};

    /// Builds a config with a chosen bind host, token, and opt-out (everything else default).
    fn exposure_cfg(host: IpAddr, token: Option<&str>, allow_unauth: bool) -> Config {
        Config {
            server: ServerConfig { host, port: 3000 },
            access_token: token.map(str::to_owned),
            allow_unauthenticated: allow_unauth,
            ..Config::default()
        }
    }

    /// A loopback bind never needs a token.
    #[test]
    fn loopback_needs_no_token() {
        let cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::LOCALHOST), None, false);
        assert!(
            cfg.check_exposure().is_ok(),
            "loopback is fine without a token"
        );
    }

    /// A non-loopback bind with no token and no opt-out refuses to start.
    #[test]
    fn exposed_without_token_fails_closed() {
        let cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::UNSPECIFIED), None, false);
        assert!(
            cfg.check_exposure().is_err(),
            "0.0.0.0 with no token must refuse to start"
        );
    }

    /// A token unlocks an exposed bind.
    #[test]
    fn exposed_with_token_ok() {
        let cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), Some("tok"), false);
        assert!(
            cfg.check_exposure().is_ok(),
            "a token unlocks an exposed bind"
        );
    }

    /// The explicit opt-out unlocks an exposed bind (auth terminated upstream).
    #[test]
    fn exposed_with_explicit_optout_ok() {
        let cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), None, true);
        assert!(
            cfg.check_exposure().is_ok(),
            "allow_unauthenticated unlocks an exposed bind"
        );
    }

    /// Telemetry defaults to disabled (no endpoint), full sampling, service name `runlet`.
    #[test]
    fn telemetry_defaults_disabled() {
        let cfg = Config::default();
        assert!(
            cfg.telemetry.otlp_endpoint.is_none(),
            "no endpoint by default ⇒ tracing off, logs only"
        );
        assert!(
            (cfg.telemetry.sample_ratio - 1.0).abs() < f64::EPSILON,
            "default sample ratio is 1.0"
        );
        assert_eq!(cfg.telemetry.service_name, "runlet");
    }

    /// A `telemetry` block parses its endpoint / ratio / service name; omitted fields default.
    #[test]
    fn telemetry_block_parses() {
        let json = r#"{"telemetry":{"otlp_endpoint":"http://collector:4317","sample_ratio":0.1}}"#;
        let parsed = serde_json::from_str::<Config>(json);
        assert!(parsed.is_ok(), "telemetry block should parse");
        let cfg = parsed.unwrap_or_default();
        assert_eq!(
            cfg.telemetry.otlp_endpoint.as_deref(),
            Some("http://collector:4317")
        );
        assert!((cfg.telemetry.sample_ratio - 0.1).abs() < f64::EPSILON);
        assert_eq!(
            cfg.telemetry.service_name, "runlet",
            "omitted service_name falls back to the default"
        );
    }

    /// Builds a config in trusted-header mode with a chosen bind + isolation assertion. A token is
    /// set so the base `access_token` guard passes and only the trusted-isolation guard is exercised.
    fn trusted_cfg(host: IpAddr, assert_isolation: bool) -> Config {
        let mut cfg = exposure_cfg(host, Some("edge-cred"), false);
        cfg.trusted.enabled = true;
        cfg.trusted.assert_network_isolation = assert_isolation;
        cfg
    }

    /// Trusted mode on a loopback bind never needs the isolation assertion.
    #[test]
    fn trusted_loopback_needs_no_isolation() {
        let cfg = trusted_cfg(IpAddr::V4(Ipv4Addr::LOCALHOST), false);
        assert!(
            cfg.check_exposure().is_ok(),
            "loopback trusted mode is fine without asserting isolation"
        );
    }

    /// Trusted mode on an exposed bind without asserted isolation refuses to start.
    #[test]
    fn trusted_exposed_without_isolation_fails_closed() {
        let cfg = trusted_cfg(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), false);
        assert!(
            cfg.check_exposure().is_err(),
            "exposed trusted mode must refuse without asserted isolation"
        );
    }

    /// Asserting isolation unlocks an exposed trusted-mode bind.
    #[test]
    fn trusted_exposed_with_isolation_ok() {
        let cfg = trusted_cfg(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), true);
        assert!(
            cfg.check_exposure().is_ok(),
            "asserted isolation unlocks an exposed trusted-mode bind"
        );
    }

    /// Trusted mode disabled leaves an exposed (token-gated) bind unaffected by the isolation guard.
    #[test]
    fn trusted_disabled_ignores_isolation_guard() {
        let mut cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), Some("tok"), false);
        cfg.trusted.enabled = false;
        assert!(
            cfg.check_exposure().is_ok(),
            "isolation guard applies only when trusted mode is enabled"
        );
    }
}
