//! Server configuration loaded from an optional `config.json` file.
//!
//! The HTTP front's own config: bind address, the `/execute` auth gate, and the script/
//! module directories — plus the embedded [`EngineConfig`] sandbox limits owned by
//! `runlet-core`. All fields have sensible defaults; a missing file starts with defaults.
//!
//! Size fields accept human-readable strings: `"8mb"`, `"256kb"`, `"1gb"`,
//! or plain numbers in bytes: `8388608`.

use std::error::Error;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use runlet_core::config::EngineConfig;
use serde::Deserialize;

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
}
