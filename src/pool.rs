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

/// A pool of pre-configured `QuickJS` runtimes.
#[derive(Debug, Clone)]
pub(crate) struct JsPool {
    /// The inner pool of runtimes.
    inner: Arc<ArrayQueue<Runtime>>,
    /// Number of slots in the pool.
    size: usize,
    /// Engine config applied to each runtime.
    engine_config: EngineConfig,
}

impl JsPool {
    /// Creates a new pool from engine config (pool size 0 = auto-detect CPU cores).
    ///
    /// # Errors
    ///
    /// Returns an error if runtime creation fails.
    pub(crate) fn new(engine_config: EngineConfig) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let size = if engine_config.pool_size > 0 {
            engine_config.pool_size
        } else {
            available_parallelism()
                .map(NonZero::get)
                .unwrap_or(4)
        };

        let queue = ArrayQueue::new(size);

        for _ in 0..size {
            let runtime = create_runtime(&engine_config)?;
            queue.push(runtime).map_err(|_err| "pool queue full during init")?;
        }

        Ok(Self {
            inner: Arc::new(queue),
            size,
            engine_config,
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
            .map_or_else(|| create_runtime(&self.engine_config), Ok)
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
}

/// Creates a new runtime with sandbox limits from config.
fn create_runtime(config: &EngineConfig) -> Result<Runtime, Box<dyn Error + Send + Sync>> {
    let runtime = Runtime::new()?;
    runtime.set_memory_limit(config.memory_limit);
    runtime.set_max_stack_size(config.max_stack_size);
    Ok(runtime)
}
