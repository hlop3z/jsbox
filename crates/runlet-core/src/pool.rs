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

use crate::bytecode::{self, BytecodeCache};
use crate::config::EngineConfig;
use crate::modules::{ModuleRegistry, RegistryLoader, RegistryResolver};

/// Max distinct compiled scripts to retain as bytecode. Sized for the hot working set, not the
/// full tenant space — moka's `TinyLFU` keeps the most valuable entries under churn.
const BYTECODE_CACHE_CAPACITY: u64 = 1024;

/// A pool of pre-configured `QuickJS` runtimes.
#[derive(Debug, Clone)]
pub struct JsPool {
    /// The inner pool of runtimes.
    inner: Arc<ArrayQueue<Runtime>>,
    /// Number of slots in the pool.
    size: usize,
    /// Engine config applied to each runtime.
    engine_config: EngineConfig,
    /// Injectable ES modules, wired as the per-runtime `import` loader.
    modules: Arc<ModuleRegistry>,
    /// Shared compiled-bytecode cache, handed to every execution.
    bytecode_cache: BytecodeCache,
}

impl JsPool {
    /// Creates a new pool from engine config (pool size 0 = auto-detect CPU cores).
    ///
    /// # Errors
    ///
    /// Returns an error if runtime creation fails.
    pub fn new(
        engine_config: EngineConfig,
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
            modules,
            bytecode_cache: BytecodeCache::new(
                BYTECODE_CACHE_CAPACITY,
                bytecode::DEFAULT_MIN_SOURCE_BYTES,
            ),
        })
    }

    /// The shared compiled-bytecode cache. Handed to each execution so a hot script (large
    /// enough to clear the size floor) is parsed/compiled once and thereafter loaded as bytecode.
    #[must_use]
    pub const fn bytecode_cache(&self) -> &BytecodeCache {
        &self.bytecode_cache
    }

    /// Takes a runtime from the pool. Creates a new one if the pool is empty.
    ///
    /// # Errors
    ///
    /// Returns an error if creating a fallback runtime fails.
    pub fn acquire(&self) -> Result<Runtime, Box<dyn Error + Send + Sync>> {
        self.inner
            .pop()
            .map_or_else(|| create_runtime(&self.engine_config, &self.modules), Ok)
    }

    /// Returns a runtime to the pool. Drops it if the pool is full.
    pub fn release(&self, runtime: Runtime) {
        runtime.run_gc();
        drop(self.inner.push(runtime));
    }

    /// Returns the pool size.
    #[must_use]
    pub const fn size(&self) -> usize {
        self.size
    }

    /// Returns the engine config.
    #[must_use]
    pub const fn engine_config(&self) -> &EngineConfig {
        &self.engine_config
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
