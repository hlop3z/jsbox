//! Generic per-execution metric collection.
//!
//! Shared by both the in-engine capabilities (`http`/`s3`, in `runlet-core`) and the driver
//! backends (`db`/`mongo`/… in `fabric-backends`). A [`Collector`] accumulates per-op metrics
//! during one execution; the consumer drains it into the response `meta`.

use std::sync::{Arc, Mutex};

use serde::Serialize;

/// Generic metrics collector — a per-execution buffer of capability operation metrics.
pub type Collector<T> = Arc<Mutex<Vec<T>>>;

/// Creates a new empty metrics collector.
#[must_use]
pub fn new_collector<T>() -> Collector<T> {
    Arc::new(Mutex::new(Vec::new()))
}

/// Pushes a metric into the collector.
pub fn record<T>(collector: &Collector<T>, metric: T) {
    if let Ok(mut vec) = collector.lock() {
        vec.push(metric);
    }
}

/// Extracts all collected metrics, returning an empty vec if unavailable.
#[must_use]
pub fn drain<T: Clone>(collector: Option<&Collector<T>>) -> Vec<T> {
    collector
        .and_then(|coll| coll.lock().ok().map(|guard| guard.clone()))
        .unwrap_or_default()
}

/// The number of operations recorded so far.
///
/// Used by a backend with a sub-cap, e.g. `mail`'s `max_sends`, to enforce it against its own
/// metrics — the generic `__io` seam already enforces the global `max_ops`.
#[must_use]
pub fn op_count<T>(collector: &Collector<T>) -> usize {
    collector.lock().map_or(0, |vec| vec.len())
}

/// Checks if the operation count exceeds the per-execution limit.
///
/// # Errors
///
/// Returns a human-safe message when the collector already holds `max_ops` metrics.
pub fn check_op_limit<T: Serialize>(
    collector: &Collector<T>,
    max_ops: usize,
) -> Result<(), String> {
    if let Ok(vec) = collector.lock()
        && vec.len() >= max_ops
    {
        return Err(format!(
            "too many operations: limit is {max_ops} per execution"
        ));
    }
    Ok(())
}
