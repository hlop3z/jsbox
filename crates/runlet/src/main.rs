//! runlet: A sandboxed JS execution service powered by `QuickJS` (HTTP front for `runlet-core`).

mod authz;
mod config;
mod handler;
mod identity;
mod quota;
mod sidecar;
mod telemetry;

use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use axum::serve::ListenerExt as _;
use rustls::crypto::aws_lc_rs;
use tokio::net::TcpListener;
use tokio::signal;
use tokio::sync::Semaphore;
use tracing::info;

use runlet_core::host::{HostSettings, LogicHost};
use runlet_core::metrics::Metrics;
use runlet_core::modules::ModuleRegistry;
use runlet_core::partition::PartitionLimiter;
use runlet_core::pool::JsPool;
use runlet_core::registry::ScriptRegistry;

use crate::config::Config;
use crate::handler::{AppState, TrustedRuntime};
use crate::quota::TenantQuota;
use crate::sidecar::SidecarTransport;

/// Use `mimalloc` as the global allocator for better small-allocation performance.
/// `QuickJS` benefits significantly (~20-40%) from this via the `rust-alloc` feature.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Entry point — loads config and starts the HTTP server.
///
/// # Errors
///
/// Returns an error if config is invalid or the server fails to start.
#[tokio::main]
#[expect(
    clippy::too_many_lines,
    reason = "linear startup wiring (config → registries → pool → host → bulkhead → serve); \
              splitting it would scatter the one-shot setup state across helpers"
)]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    // Load config first so telemetry init can read its `telemetry` block. A load failure returns
    // before any subscriber is installed; the error propagates from `main` and is printed.
    let config_path = PathBuf::from("config.json");
    let config = Config::load(&config_path)?;

    // Structured JSON logs to stdout (always) + optional OTLP tracing (when `otlp_endpoint` is
    // set). Held for the process lifetime; flushed on graceful shutdown so buffered spans export.
    let telemetry_guard = telemetry::init(&telemetry::TelemetrySettings {
        otlp_endpoint: config.telemetry.otlp_endpoint.clone(),
        sample_ratio: config.telemetry.sample_ratio,
        service_name: config.telemetry.service_name.clone(),
    });

    // Install `aws-lc-rs` as the single process-wide rustls provider — reused by every TLS
    // path (`db` SSL, `redis` rediss://, `amq` amqps://) so the binary links one crypto
    // stack. `Err` just means a default was already installed; either way we're set.
    if aws_lc_rs::default_provider().install_default().is_err() {
        tracing::warn!("rustls crypto provider was already installed");
    }

    // Fail closed: refuse an exposed bind with no `/execute` auth gate (see config.rs).
    config.check_exposure()?;

    info!(
        memory_limit = config.engine.memory_limit,
        max_stack_size = config.engine.max_stack_size,
        timeout_ms = config.engine.timeout_ms,
        "engine config"
    );

    if config.debug {
        info!(
            "DEBUG MODE: SSRF private-IP block relaxed (local testing only — do not use in production)"
        );
    }

    // Load the read-only script registry before the engine config moves into the pool.
    let script_registry = config.scripts_dir.as_deref().map_or_else(
        || Ok(ScriptRegistry::default()),
        |dir| ScriptRegistry::load(dir, config.engine.max_script_size),
    )?;
    if script_registry.count() > 0 {
        info!(
            "script registry: {} scripts loaded",
            script_registry.count()
        );
    }

    // Load the injectable ES-module registry (handlers `import` from it).
    let module_registry = config.modules_dir.as_deref().map_or_else(
        || Ok(ModuleRegistry::default()),
        |dir| ModuleRegistry::load(dir, config.engine.max_script_size),
    )?;
    if module_registry.count() > 0 {
        info!(
            "module registry: {} modules loaded",
            module_registry.count()
        );
    }
    let modules = Arc::new(module_registry);

    // Capture the `/execute` bearer token before the engine config moves into the pool. In
    // trusted-header mode this is the edge→box service credential.
    let access_token: Option<Arc<str>> = config.access_token.clone().map(Arc::from);
    info!(
        execute_auth = access_token.is_some(),
        "/execute bearer auth"
    );

    // Build the trusted-identity runtime (header names, member-authz gate, per-tenant quota) when
    // trusted-header mode is enabled; `None` keeps the single-tenant, caller-asserted behavior.
    let trusted = config.trusted.enabled.then(|| {
        let quota = config
            .trusted
            .quota
            .enabled
            .then(|| TenantQuota::new(config.trusted.quota.plans.clone()));
        Arc::new(TrustedRuntime {
            headers: config.trusted.headers.clone(),
            capability_entitlements: config.trusted.capability_entitlements.clone(),
            quota,
        })
    });
    info!(
        trusted_mode = trusted.is_some(),
        quota = trusted
            .as_ref()
            .is_some_and(|runtime| runtime.quota.is_some()),
        "trusted-identity mode"
    );

    let js_pool = JsPool::new(config.engine, modules)?;
    info!("JS runtime pool: {} slots", js_pool.size());

    // `EngineConfig` is `Copy`, so read the resolved limits out once for the server-side
    // wiring before the pool moves into the `LogicHost`.
    let engine_cfg = *js_pool.engine_config();
    let body_limit = engine_cfg.max_body_size();
    let max_concurrent = engine_cfg.resolved_max_concurrent(js_pool.size());
    let per_partition = engine_cfg.max_concurrent_per_partition;
    let partition_buckets = engine_cfg.resolved_partition_buckets();
    info!("execution bulkhead: {max_concurrent} concurrent");
    let partition_limiter = PartitionLimiter::new(partition_buckets, per_partition);
    if partition_limiter.is_some() {
        info!(
            "per-partition fairness: {per_partition} concurrent/partition across {partition_buckets} buckets"
        );
    }
    // The egress sidecar transport. Driver-backed capabilities (`db`/`mongo`/`mail`/`redis`/`amq`/
    // `auth`) route to `fabricd` — over a local UDS or a remote QUIC link — which holds the drivers
    // + credentials; the box links no driver. Built before the engine config moves into the pool.
    let transport = SidecarTransport::from_config(
        config.fabricd_socket.as_deref(),
        config.fabricd_quic.as_ref(),
    )?;
    match transport.label() {
        "none" => info!("no fabricd egress sidecar: driver-backed capabilities are unavailable"),
        label => info!(transport = label, "fabricd egress sidecar configured"),
    }

    let registry = Arc::new(script_registry);
    // The callable logic host owns the pool + engine limits; the HTTP front is one consumer of it
    // (a non-HTTP scheduler could be another). It drives no I/O itself — driver capabilities run in
    // the wired `fabricd` egress.
    let host = LogicHost::new(
        js_pool,
        Arc::clone(&registry),
        HostSettings {
            limits: engine_cfg,
            allow_private_targets: config.debug,
        },
    );
    // A cheap clone (all `Arc`-backed) kept out of `AppState` so the warm runtime pool can be
    // disposed after axum has drained in-flight requests.
    let host_lifecycle = host.clone();
    let state = AppState {
        host,
        registry,
        engine_cfg,
        error_debug: config.error_debug,
        limiter: Arc::new(Semaphore::new(max_concurrent)),
        partition_limiter,
        transport,
        metrics: Arc::new(Metrics::default()),
        bulkhead_capacity: max_concurrent,
        access_token,
        trusted,
    };

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/metrics", get(handler::metrics))
        .route("/execute", post(handler::execute))
        .layer(DefaultBodyLimit::max(body_limit))
        .with_state(state);

    let addr = config.server.addr();
    info!("listening on {addr}");

    let tcp_listener = TcpListener::bind(addr).await?;
    let listener = tcp_listener.tap_io(|tcp_stream| {
        if let Err(err) = tcp_stream.set_nodelay(true) {
            tracing::warn!("failed to set TCP_NODELAY: {err}");
        }
    });

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    // axum has drained in-flight requests (so every `host.run` has returned); reject any
    // stragglers and dispose the warm runtime pool before exit.
    host_lifecycle.shutdown();

    // Flush + shut down the tracer provider so buffered spans are exported before exit (no-op
    // when tracing is disabled).
    telemetry_guard.shutdown();

    info!("server shut down gracefully");
    Ok(())
}

/// Waits for SIGTERM (container stop) or Ctrl+C.
async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c().await.unwrap_or_else(|_err| unreachable!());
    };

    #[cfg(unix)]
    let terminate = async {
        let _ = signal::unix::signal(signal::unix::SignalKind::terminate())
            .unwrap_or_else(|_err| unreachable!())
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = {
        use core::future::pending;
        pending::<()>()
    };

    tokio::select! {
        () = ctrl_c => info!("received Ctrl+C, shutting down"),
        () = terminate => info!("received SIGTERM, shutting down"),
    }
}
