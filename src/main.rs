//! jsbox: A sandboxed JS execution service powered by `QuickJS`.

mod amq;
mod auth;
mod bytesize;
mod config;
mod db;
mod decimal;
mod engine;
mod errors;
mod handler;
mod http;
mod kv;
mod mail;
mod pool;
mod registry;
mod s3;
mod sandbox;
mod ssrf;
mod sys;

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
use tracing_subscriber::fmt::init as init_tracing;

use crate::config::Config;
use crate::handler::AppState;
use crate::pool::JsPool;
use crate::registry::ScriptRegistry;

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
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    init_tracing();

    // Install `aws-lc-rs` as the single process-wide rustls provider — reused by every TLS
    // path (`db` SSL, `redis` rediss://, `amq` amqps://) so the binary links one crypto
    // stack. `Err` just means a default was already installed; either way we're set.
    if aws_lc_rs::default_provider().install_default().is_err() {
        tracing::warn!("rustls crypto provider was already installed");
    }

    let config_path = PathBuf::from("config.json");
    let config = Config::load(&config_path)?;

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

    let js_pool = JsPool::new(config.engine, config.debug, config.error_debug)?;
    info!("JS runtime pool: {} slots", js_pool.size());

    let body_limit = js_pool.engine_config().max_body_size();
    let max_concurrent = js_pool
        .engine_config()
        .resolved_max_concurrent(js_pool.size());
    info!("execution bulkhead: {max_concurrent} concurrent");
    let state = AppState {
        pool: js_pool,
        registry: Arc::new(script_registry),
        limiter: Arc::new(Semaphore::new(max_concurrent)),
    };

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
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
