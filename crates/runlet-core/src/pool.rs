//! Pool of pre-warmed `QuickJS` runtimes for reuse across requests.
//!
//! Each slot is a `Runtime` with sandbox limits already configured.
//! Contexts are created fresh per request on a pooled runtime (cheap ~100us)
//! to ensure clean global scope without needing sanitization.

use std::error::Error;
use std::num::NonZero;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::available_parallelism;

use crossbeam_queue::ArrayQueue;
use rquickjs::Runtime;

use crate::bytecode::{self, BytecodeCache};
use crate::config::EngineConfig;
use crate::modules::{ModuleRegistry, RegistryLoader, RegistryResolver};

/// Max distinct compiled scripts to retain as bytecode. Sized for the hot working set, not the
/// full tenant space — moka's `TinyLFU` keeps the most valuable entries under churn.
const BYTECODE_CACHE_CAPACITY: u64 = 1024;

/// Shared lifecycle state for graceful teardown, behind one `Arc` so every [`JsPool`]
/// clone (and thus every [`crate::host::LogicHost`] clone) observes the same flags.
#[derive(Debug)]
struct PoolState {
    /// `true` while the pool accepts new acquisitions; flipped to `false` by
    /// [`JsPool::shutdown`] so in-flight runtimes are disposed (not re-pooled) on release.
    accepting: AtomicBool,
    /// Runtimes currently checked out (acquired but not yet released) — the in-flight gauge.
    in_flight: AtomicUsize,
}

/// A snapshot of pool liveness, for operability gauges (item #5 in `CONSUMER_NOTES.md`).
#[derive(Debug, Clone, Copy)]
pub struct PoolStats {
    /// Configured pool size (the steady-state warm slot count).
    pub size: usize,
    /// Runtimes currently idle in the pool, ready to acquire.
    pub idle: usize,
    /// Runtimes currently checked out by an in-flight execution.
    pub in_flight: usize,
}

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
    /// Shared graceful-teardown state (accepting flag + in-flight gauge).
    state: Arc<PoolState>,
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
            available_parallelism().map_or(4, NonZero::get)
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
            state: Arc::new(PoolState {
                accepting: AtomicBool::new(true),
                in_flight: AtomicUsize::new(0),
            }),
        })
    }

    /// The shared compiled-bytecode cache. Handed to each execution so a hot script (large
    /// enough to clear the size floor) is parsed/compiled once and thereafter loaded as bytecode.
    #[must_use]
    pub const fn bytecode_cache(&self) -> &BytecodeCache {
        &self.bytecode_cache
    }

    /// Takes a runtime from the pool. Creates a new one if the pool is empty. Records it as
    /// in-flight; the matching [`release`](Self::release) clears it.
    ///
    /// # Errors
    ///
    /// Returns an error if creating a fallback runtime fails.
    pub fn acquire(&self) -> Result<Runtime, Box<dyn Error + Send + Sync>> {
        let runtime = self
            .inner
            .pop()
            .map_or_else(|| create_runtime(&self.engine_config, &self.modules), Ok)?;
        let _ = self.state.in_flight.fetch_add(1, Ordering::Relaxed);
        Ok(runtime)
    }

    /// Returns a runtime to the pool. Drops it if the pool is full, or — once
    /// [`shutdown`](Self::shutdown) has been called — disposes it instead of re-pooling, so
    /// the warm set empties as in-flight executions finish.
    pub fn release(&self, runtime: Runtime) {
        let _ = self.state.in_flight.fetch_sub(1, Ordering::Relaxed);
        if self.state.accepting.load(Ordering::Relaxed) {
            runtime.run_gc();
            drop(self.inner.push(runtime));
        } else {
            drop(runtime);
        }
    }

    /// Whether the pool is still accepting new acquisitions (`false` after
    /// [`shutdown`](Self::shutdown)). The host checks this to reject new executions.
    #[must_use]
    pub fn is_accepting(&self) -> bool {
        self.state.accepting.load(Ordering::Relaxed)
    }

    /// Begins graceful teardown: stop accepting new acquisitions and dispose the warm
    /// runtimes currently idle in the pool. In-flight runtimes are disposed by their own
    /// [`release`](Self::release) as they finish, so the pool drains to empty without
    /// interrupting work. Idempotent.
    pub fn shutdown(&self) {
        self.state.accepting.store(false, Ordering::Relaxed);
        while self.inner.pop().is_some() {}
    }

    /// A snapshot of pool liveness (configured size, idle, in-flight).
    #[must_use]
    pub fn stats(&self) -> PoolStats {
        PoolStats {
            size: self.size,
            idle: self.inner.len(),
            in_flight: self.state.in_flight.load(Ordering::Relaxed),
        }
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

#[cfg(test)]
mod tests {
    //! Graceful-teardown primitives: the in-flight gauge tracks acquire/release, and
    //! `shutdown` stops re-pooling so the warm set drains to empty without interrupting work.

    use super::{EngineConfig, JsPool, ModuleRegistry};
    use std::sync::Arc;

    /// A small fixed-size pool over an empty module registry.
    fn pool(size: usize) -> JsPool {
        let config = EngineConfig {
            pool_size: size,
            ..EngineConfig::default()
        };
        JsPool::new(config, Arc::new(ModuleRegistry::default()))
            .unwrap_or_else(|_err| unreachable!("pool init"))
    }

    #[test]
    fn acquire_release_track_in_flight() {
        let pool = pool(2);
        let before = pool.stats();
        assert_eq!((before.size, before.idle, before.in_flight), (2, 2, 0));

        let runtime = pool
            .acquire()
            .unwrap_or_else(|_err| unreachable!("acquire"));
        let mid = pool.stats();
        assert_eq!(
            (mid.idle, mid.in_flight),
            (1, 1),
            "checkout moves idle→in-flight"
        );

        pool.release(runtime);
        let after = pool.stats();
        assert_eq!(
            (after.idle, after.in_flight),
            (2, 0),
            "release restores the slot"
        );
    }

    #[test]
    fn shutdown_stops_accepting_and_drains() {
        let pool = pool(2);
        // Check one out so we can prove an in-flight runtime is disposed (not re-pooled) on
        // release after shutdown.
        let runtime = pool
            .acquire()
            .unwrap_or_else(|_err| unreachable!("acquire"));
        assert!(pool.is_accepting());

        pool.shutdown();
        assert!(!pool.is_accepting(), "shutdown stops acceptance");
        let drained = pool.stats();
        assert_eq!(drained.idle, 0, "shutdown disposes the idle warm runtimes");
        assert_eq!(
            drained.in_flight, 1,
            "the checked-out runtime is still in flight"
        );

        pool.release(runtime);
        let done = pool.stats();
        assert_eq!(
            (done.idle, done.in_flight),
            (0, 0),
            "an in-flight runtime is disposed, not re-pooled, after shutdown"
        );

        // `shutdown` is idempotent.
        pool.shutdown();
        assert!(!pool.is_accepting());
    }
}
