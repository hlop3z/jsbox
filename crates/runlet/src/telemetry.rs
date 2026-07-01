//! Observability init: structured JSON logs to stdout (always) + optional OpenTelemetry
//! distributed tracing over OTLP/gRPC (when an endpoint is configured).
//!
//! Hybrid transport (design D2): metrics stay Prometheus PULL (`/metrics`, `runlet-core`);
//! traces PUSH via the SDK's `BatchSpanProcessor` (async, bounded, drop-on-full — never blocks
//! the request path, D2/D6); logs go to stdout as JSON so a collector outage cannot lose them.
//!
//! Fail-open (D6): a missing or unbuildable exporter logs a warning and runs untraced — it never
//! panics or fails startup. Sampling is parent-based (D3): the box honors the edge's decision when
//! a W3C `traceparent` is present and applies its own ratio to self-started roots.

use core::time::Duration;

use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{SpanExporter, WithExportConfig as _};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{EnvFilter, fmt};

/// Timeout for a single OTLP export round-trip to the collector.
const EXPORT_TIMEOUT: Duration = Duration::from_secs(3);

/// Resolved tracing settings (from `config.telemetry`).
pub(crate) struct TelemetrySettings {
    /// OTLP/gRPC collector endpoint (e.g. `http://localhost:4317`). `None` ⇒ tracing disabled;
    /// logs still emit. Plaintext by default (local/in-pod collector; no app-side TLS).
    pub(crate) otlp_endpoint: Option<String>,
    /// Sampling ratio in `[0.0, 1.0]` applied to box-started root spans (parent decisions are
    /// always honored). `1.0` samples every self-rooted trace.
    pub(crate) sample_ratio: f64,
    /// `service.name` resource attribute reported to the collector.
    pub(crate) service_name: String,
}

/// Owns the tracer provider so in-flight spans can be flushed on shutdown. `None` when tracing
/// is disabled or the exporter could not be built (fail-open).
pub(crate) struct TelemetryGuard {
    /// The provider to flush + shut down, if tracing is active.
    provider: Option<SdkTracerProvider>,
}

impl TelemetryGuard {
    /// Flushes and shuts down the tracer provider so buffered spans are exported before exit.
    /// A no-op when tracing is disabled.
    pub(crate) fn shutdown(self) {
        let Some(provider) = self.provider else {
            return;
        };
        if let Err(err) = provider.shutdown() {
            tracing::warn!("tracer provider shutdown error: {err}");
        }
    }
}

/// Installs the global subscriber: an `env-filter` level gate, a JSON-to-stdout log layer, and
/// (when an OTLP endpoint is set) the OpenTelemetry span layer. Returns a guard for shutdown.
///
/// Idempotent per process — call once at startup. Failure to build the exporter degrades to
/// logs-only rather than erroring.
pub(crate) fn init(settings: &TelemetrySettings) -> TelemetryGuard {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_err| EnvFilter::new("info"));
    // Structured JSON logs to stdout; the active span's `trace_id`/`span_id` ride each line via
    // the OTel layer's context, so logs correlate with traces and `meta.trace_id`.
    let log_layer = fmt::layer().json().with_current_span(true);

    let provider = build_provider(settings);
    let otel_layer = provider.as_ref().map(|prov| {
        global::set_text_map_propagator(TraceContextPropagator::new());
        // Returns the previous (no-op) provider; discard it (owned, so `drop` per house idiom).
        drop(global::set_tracer_provider(prov.clone()));
        tracing_opentelemetry::layer().with_tracer(prov.tracer(settings.service_name.clone()))
    });

    tracing_subscriber::registry()
        .with(filter)
        .with(log_layer)
        .with(otel_layer)
        .init();

    if provider.is_some() {
        tracing::info!(
            endpoint = settings.otlp_endpoint.as_deref().unwrap_or_default(),
            sample_ratio = settings.sample_ratio,
            "OTLP tracing enabled"
        );
    }
    TelemetryGuard { provider }
}

/// Builds the OTLP tracer provider when an endpoint is configured, else `None`. Any exporter
/// build error is logged and folded to `None` (fail-open, D6).
fn build_provider(settings: &TelemetrySettings) -> Option<SdkTracerProvider> {
    let endpoint = settings.otlp_endpoint.as_deref()?;
    let exporter = match SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .with_timeout(EXPORT_TIMEOUT)
        .build()
    {
        Ok(exporter) => exporter,
        Err(err) => {
            tracing::warn!("OTLP exporter unavailable, running untraced: {err}");
            return None;
        }
    };
    // Parent-based ratio sampler: honor the edge's `traceparent` decision; sample self-started
    // roots at the configured ratio. Batch export runs on the SDK's own background thread.
    let sampler = Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(settings.sample_ratio)));
    let resource = Resource::builder()
        .with_service_name(settings.service_name.clone())
        .build();
    Some(
        SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_sampler(sampler)
            .with_resource(resource)
            .build(),
    )
}
