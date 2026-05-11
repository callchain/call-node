//! OpenTelemetry tracing integration.

use std::sync::atomic::Ordering;

use opentelemetry::trace::{Span, Tracer};
use opentelemetry::KeyValue;
use opentelemetry_sdk::{
    propagation::TraceContextPropagator,
    trace::{RandomIdGenerator, Sampler, TracerProvider},
    Resource,
};
use opentelemetry_semantic_conventions::resource;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::{layer::SubscriberExt, Registry};

use super::registry::TelemetryRegistry;

/// Global tracer provider for shutdown and tracer creation.
pub static GLOBAL_PROVIDER: std::sync::OnceLock<TracerProvider> = std::sync::OnceLock::new();

/// Initialize OpenTelemetry tracing with a stdout exporter.
/// Returns a configured `tracing` subscriber that sends spans to OpenTelemetry.
pub fn init_opentelemetry_tracing(
    service_name: &str,
    log_level: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    init_opentelemetry_tracing_with_file(service_name, log_level, None)
}

/// Initialize OpenTelemetry tracing with an optional file logging layer.
pub fn init_opentelemetry_tracing_with_file(
    service_name: &str,
    log_level: &str,
    file_layer: Option<crate::logging::file_log::FileLogLayer>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Set up propagator for distributed tracing
    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());

    // Create tracer provider with resource attributes
    let provider = TracerProvider::builder()
        .with_sampler(Sampler::AlwaysOn)
        .with_id_generator(RandomIdGenerator::default())
        .with_resource(Resource::new(vec![
            KeyValue::new(resource::SERVICE_NAME, service_name.to_string()),
            KeyValue::new(resource::SERVICE_VERSION, env!("CARGO_PKG_VERSION")),
        ]))
        .build();

    // Store provider for shutdown
    let _ = GLOBAL_PROVIDER.set(provider);
    let provider = GLOBAL_PROVIDER.get().expect("invariant: init_telemetry called before get_tracer");

    let tracer = opentelemetry::trace::TracerProvider::tracer(provider, "call-node");

    // Build the tracing subscriber with OpenTelemetry layer
    let filter = tracing_subscriber::EnvFilter::try_new(log_level)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::try_new("info")
            .expect("invariant: default log level 'info' is valid"));

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()));

    if let Some(file_layer) = file_layer {
        let subscriber = Registry::default()
            .with(filter)
            .with(fmt_layer)
            .with(OpenTelemetryLayer::new(tracer))
            .with(file_layer);
        tracing::subscriber::set_global_default(subscriber)?;
    } else {
        let subscriber = Registry::default()
            .with(filter)
            .with(fmt_layer)
            .with(OpenTelemetryLayer::new(tracer));
        tracing::subscriber::set_global_default(subscriber)?;
    }

    Ok(())
}

/// Get a named tracer for a subsystem (returns None if OTel not initialized).
fn get_tracer(name: &'static str) -> Option<opentelemetry_sdk::trace::Tracer> {
    GLOBAL_PROVIDER
        .get()
        .map(|provider| opentelemetry::trace::TracerProvider::tracer(provider, name))
}

/// Record a span for a block production event via OpenTelemetry.
/// Caller is responsible for updating `registry.record_block_produced()` /
/// `record_block_committed()` to avoid double-counting in BFT paths.
pub fn record_block_span(registry: &TelemetryRegistry, height: u64, duration_ms: u64) {
    let Some(tracer) = get_tracer("call-node/consensus") else {
        return;
    };
    let mut span = tracer.start("block_produced");
    span.set_attribute(KeyValue::new("block.height", height as i64));
    span.set_attribute(KeyValue::new("block.duration_ms", duration_ms as i64));
    span.set_attribute(KeyValue::new(
        "consensus.round",
        registry.consensus_rounds.load(Ordering::Relaxed) as i64,
    ));
    span.end();
}

/// Record a span for a transaction processing event via OpenTelemetry.
pub fn record_tx_span(
    registry: &TelemetryRegistry,
    tx_type: &str,
    duration_ms: u64,
    accepted: bool,
) {
    if !accepted {
        registry.record_tx_rejected();
    }

    let Some(tracer) = get_tracer("call-node/mempool") else {
        return;
    };

    if accepted {
        let mut span = tracer.start("tx_processed");
        span.set_attribute(KeyValue::new("tx.type", tx_type.to_string()));
        span.set_attribute(KeyValue::new("tx.duration_ms", duration_ms as i64));
        span.set_attribute(KeyValue::new("tx.accepted", true));
        span.set_attribute(KeyValue::new(
            "mempool.size",
            registry.mempool_tx_count.load(Ordering::Relaxed) as i64,
        ));
        span.end();
    } else {
        let mut span = tracer.start("tx_rejected");
        span.set_attribute(KeyValue::new("tx.type", tx_type.to_string()));
        span.set_attribute(KeyValue::new("tx.duration_ms", duration_ms as i64));
        span.set_attribute(KeyValue::new("tx.accepted", false));
        span.end();
    }
}

/// Record a span for a P2P message event via OpenTelemetry.
/// Caller is responsible for updating bytes-sent/received counters to avoid
/// double-counting (those are already emitted in the network receive loop).
pub fn record_p2p_span(
    registry: &TelemetryRegistry,
    direction: &str,
    message_type: &str,
    bytes: usize,
) {
    let Some(tracer) = get_tracer("call-node/p2p") else {
        return;
    };
    let mut span = tracer.start("p2p_message");
    span.set_attribute(KeyValue::new("p2p.direction", direction.to_string()));
    span.set_attribute(KeyValue::new("p2p.message_type", message_type.to_string()));
    span.set_attribute(KeyValue::new("p2p.bytes", bytes as i64));
    span.set_attribute(KeyValue::new(
        "p2p.peers",
        registry.p2p_peers.load(Ordering::Relaxed) as i64,
    ));
    span.end();
}
