//! Pool of pre-warmed `QuickJS` runtimes for reuse across requests.
//!
//! Each slot is a `Runtime` with sandbox limits already configured.
//! Contexts are created fresh per request on a pooled runtime (cheap ~100us)
//! to ensure clean global scope without needing sanitization.

use std::error::Error;
use std::num::NonZero;
use std::sync::Arc;
use std::thread::available_parallelism;

use crossbeam_queue::ArrayQueue;
use rquickjs::Runtime;

use crate::config::EngineConfig;
use crate::modules::{ModuleRegistry, RegistryLoader, RegistryResolver};

/// A pool of pre-configured `QuickJS` runtimes.
#[derive(Debug, Clone)]
pub(crate) struct JsPool {
    /// The inner pool of runtimes.
    inner: Arc<ArrayQueue<Runtime>>,
    /// Number of slots in the pool.
    size: usize,
    /// Engine config applied to each runtime.
    engine_config: EngineConfig,
    /// Local-dev flag: relax the SSRF private-IP block when `true`.
    debug: bool,
    /// Include `error.debug` (stack traces) in responses when `true`.
    error_debug: bool,
    /// Injectable ES modules, wired as the per-runtime `import` loader.
    modules: Arc<ModuleRegistry>,
}

impl JsPool {
    /// Creates a new pool from engine config (pool size 0 = auto-detect CPU cores).
    ///
    /// # Errors
    ///
    /// Returns an error if runtime creation fails.
    pub(crate) fn new(
        engine_config: EngineConfig,
        debug: bool,
        error_debug: bool,
        modules: Arc<ModuleRegistry>,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let size = if engine_config.pool_size > 0 {
            engine_config.pool_size
        } else {
            available_parallelism().map(NonZero::get).unwrap_or(4)
        };

        let queue = ArrayQueue::new(size);

        for _ in 0..size {
            let runtime = create_runtime(&engine_config, &modules)?;
            queue
                .push(runtime)
                .map_err(|_err| "pool queue full during init")?;
        }

        Ok(Self {
            inner: Arc::new(queue),
            size,
            engine_config,
            debug,
            error_debug,
            modules,
        })
    }

    /// Takes a runtime from the pool. Creates a new one if the pool is empty.
    ///
    /// # Errors
    ///
    /// Returns an error if creating a fallback runtime fails.
    pub(crate) fn acquire(&self) -> Result<Runtime, Box<dyn Error + Send + Sync>> {
        self.inner
            .pop()
            .map_or_else(|| create_runtime(&self.engine_config, &self.modules), Ok)
    }

    /// Returns a runtime to the pool. Drops it if the pool is full.
    pub(crate) fn release(&self, runtime: Runtime) {
        runtime.run_gc();
        drop(self.inner.push(runtime));
    }

    /// Returns the pool size.
    pub(crate) const fn size(&self) -> usize {
        self.size
    }

    /// Returns the engine config.
    pub(crate) const fn engine_config(&self) -> &EngineConfig {
        &self.engine_config
    }

    /// Returns whether debug mode (relaxed SSRF guard) is enabled.
    pub(crate) const fn debug(&self) -> bool {
        self.debug
    }

    /// Returns whether `error.debug` (stack traces) should be included in responses.
    pub(crate) const fn error_debug(&self) -> bool {
        self.error_debug
    }
}

/// Creates a new runtime with sandbox limits from config and the module loader wired in
/// (so a handler can `import` registered modules). The loader holds an `Arc` to the shared
/// immutable registry, so every pooled runtime resolves `import` against the same modules.
fn create_runtime(
    config: &EngineConfig,
    modules: &Arc<ModuleRegistry>,
) -> Result<Runtime, Box<dyn Error + Send + Sync>> {
    let runtime = Runtime::new()?;
    runtime.set_memory_limit(config.memory_limit);
    runtime.set_max_stack_size(config.max_stack_size);
    runtime.set_loader(
        RegistryResolver(Arc::clone(modules)),
        RegistryLoader(Arc::clone(modules)),
    );
    Ok(runtime)
}
