//! jsbox: A sandboxed JS execution service powered by `QuickJS`.

mod config;
mod db;
mod engine;
mod handler;
mod http;
mod pool;
mod sandbox;

use std::error::Error;
use std::path::PathBuf;

use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use axum::serve::ListenerExt as _;
use axum::Router;
use tokio::net::TcpListener;
use tokio::signal;
use tracing::info;
use tracing_subscriber::fmt::init as init_tracing;

use crate::config::Config;
use crate::pool::JsPool;

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

    let config_path = PathBuf::from("config.json");
    let config = Config::load(&config_path)?;

    info!(
        memory_limit = config.engine.memory_limit,
        max_stack_size = config.engine.max_stack_size,
        timeout_ms = config.engine.timeout_ms,
        "engine config"
    );

    let js_pool = JsPool::new(config.engine)?;
    info!("JS runtime pool: {} slots", js_pool.size());

    let body_limit = js_pool.engine_config().max_body_size();

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/execute", post(handler::execute))
        .layer(DefaultBodyLimit::max(body_limit))
        .with_state(js_pool);

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
        signal::ctrl_c()
            .await
            .unwrap_or_else(|_err| unreachable!());
    };

    #[cfg(unix)]
    let terminate = async {
        drop(
            signal::unix::signal(signal::unix::SignalKind::terminate())
                .unwrap_or_else(|_err| unreachable!())
                .recv()
                .await,
        );
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
