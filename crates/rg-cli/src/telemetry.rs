//! OpenTelemetry (OTLP) distributed-tracing wiring for the Plombir Git server.
//!
//! Layers an OTLP span exporter on top of the existing `tracing` fmt subscriber
//! **when an OTLP endpoint is configured** — via `[observability].otlp_endpoint`
//! in the config file or the standard `OTEL_EXPORTER_OTLP_ENDPOINT` /
//! `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` environment variables. When no endpoint
//! is set the OTel layer is skipped entirely, so the default build keeps the
//! plain logs-plus-Prometheus behaviour with zero tracing overhead.
//!
//! The exporter uses OTLP/HTTP (protobuf) with the blocking `reqwest` client and
//! rustls, driven by the thread-based batch span processor — so it needs no
//! extra Tokio runtime and pulls in no OpenSSL. Prometheus metrics
//! (`/metrics`) are untouched: this adds *traces*, it does not replace metrics.

use anyhow::Context;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{Protocol, WithExportConfig};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
use opentelemetry_sdk::Resource;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// `service.name` reported to the collector when neither `OTEL_SERVICE_NAME`
/// nor `[observability].service_name` is set.
///
/// Named rather than written inline because `plombir-git.example.toml` states it
/// to the operator twice — in the prose above the knob and in the commented
/// line they are invited to uncomment — and a default nobody can point at is a
/// default no test can check those statements against.
pub(crate) const DEFAULT_OTEL_SERVICE_NAME: &str = "plombir-git";

/// Head sampling ratio applied when `[observability].sample_ratio` is unset:
/// every trace is recorded. Named for the same reason as
/// [`DEFAULT_OTEL_SERVICE_NAME`] — and it is the threshold below which the
/// ratio sampler is worth installing at all, which is how
/// [`build_otel_layer`] reads it.
pub(crate) const DEFAULT_OTEL_SAMPLE_RATIO: f64 = 1.0;

/// Resolved OTLP tracing configuration (see [`resolve_otel_config`]).
pub(crate) struct OtelConfig {
    /// Full traces endpoint URL, already normalised to include `/v1/traces`.
    pub endpoint: String,
    /// `service.name` resource attribute reported to the collector.
    pub service_name: String,
    /// Head-based sampling ratio in `0.0..=1.0`. `None` = always sample.
    pub sample_ratio: Option<f64>,
}

/// Held for the process lifetime. Owns the OTLP tracer provider and the
/// non-blocking log-appender guard so both survive until shutdown; dropping /
/// [`shutdown`](TelemetryGuard::shutdown)-ing it flushes any buffered spans and
/// log lines.
pub(crate) struct TelemetryGuard {
    provider: Option<SdkTracerProvider>,
    _appender_guard: Option<WorkerGuard>,
}

impl TelemetryGuard {
    /// Flush and shut down the OTLP exporter. Call on graceful shutdown so the
    /// final batch of spans reaches the collector before the process exits.
    pub(crate) fn shutdown(self) {
        if let Some(provider) = self.provider {
            if let Err(e) = provider.shutdown() {
                // The subscriber may already be torn down here, so report via
                // stderr rather than through `tracing`.
                eprintln!("OpenTelemetry: tracer provider shutdown failed: {e}");
            }
        }
    }
}

/// Resolve the OTLP configuration from environment + config-file inputs.
///
/// Endpoint precedence (any one present turns tracing on):
/// `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` > `OTEL_EXPORTER_OTLP_ENDPOINT` >
/// config-file `otlp_endpoint`. Returns `None` when no endpoint is configured,
/// in which case OTel tracing stays off.
pub(crate) fn resolve_otel_config(
    cfg_endpoint: Option<String>,
    cfg_service_name: Option<String>,
    cfg_sample_ratio: Option<f64>,
) -> Option<OtelConfig> {
    let raw_endpoint = std::env::var("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .or_else(|| cfg_endpoint.filter(|s| !s.trim().is_empty()))?;

    let endpoint = normalise_traces_endpoint(raw_endpoint.trim());

    let service_name = std::env::var("OTEL_SERVICE_NAME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| cfg_service_name.filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| DEFAULT_OTEL_SERVICE_NAME.to_string());

    Some(OtelConfig {
        endpoint,
        service_name,
        sample_ratio: cfg_sample_ratio,
    })
}

/// Ensure the endpoint targets the OTLP/HTTP traces path. The base-endpoint
/// convention (`http://collector:4318`) needs the `/v1/traces` suffix appended;
/// a per-signal endpoint that already ends in `/v1/traces` is used verbatim.
fn normalise_traces_endpoint(raw: &str) -> String {
    let trimmed = raw.trim_end_matches('/');
    if trimmed.ends_with("/v1/traces") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/v1/traces")
    }
}

/// Install the global tracing subscriber: the fmt layer (writing to `writer`)
/// plus an optional OTLP export layer, both gated by the same `RUST_LOG`
/// `EnvFilter` (defaulting to `info`). Returns a [`TelemetryGuard`] that must be
/// kept alive for the process lifetime and `shutdown()`-ed on exit.
pub(crate) fn init(
    writer: BoxMakeWriter,
    appender_guard: Option<WorkerGuard>,
    otel: Option<OtelConfig>,
) -> anyhow::Result<TelemetryGuard> {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    // `.with_target(false)` preserves the terser log format the server used
    // before the registry refactor.
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_writer(writer);

    // Build the optional OTLP layer. `Option<Layer>` itself implements `Layer`,
    // so the `None` arm contributes nothing to the subscriber.
    let (otel_layer, provider) = match otel {
        Some(cfg) => {
            let (layer, provider) = build_otel_layer(&cfg)
                .with_context(|| format!("initialising OTLP trace export to {}", cfg.endpoint))?;
            tracing::debug!(endpoint = %cfg.endpoint, service = %cfg.service_name, "OTLP layer built");
            (Some(layer), Some(provider))
        }
        None => (None, None),
    };

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt_layer)
        .with(otel_layer)
        .init();

    if provider.is_some() {
        tracing::info!("OpenTelemetry OTLP trace export enabled");
    }

    Ok(TelemetryGuard {
        provider,
        _appender_guard: appender_guard,
    })
}

/// Build the OTLP span exporter + tracer provider and wrap it in a
/// `tracing-opentelemetry` layer. Also installs the W3C `traceparent`
/// propagator so inbound/outbound trace context is honoured.
fn build_otel_layer<S>(
    cfg: &OtelConfig,
) -> anyhow::Result<(Box<dyn Layer<S> + Send + Sync>, SdkTracerProvider)>
where
    S: tracing::Subscriber
        + for<'a> tracing_subscriber::registry::LookupSpan<'a>
        + Send
        + Sync
        + 'static,
{
    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(&cfg.endpoint)
        .with_protocol(Protocol::HttpBinary)
        .build()
        .context("building OTLP/HTTP span exporter")?;

    let resource = Resource::builder()
        .with_service_name(cfg.service_name.clone())
        .build();

    // Parent-based sampler: honour the incoming trace decision, else apply the
    // configured head ratio (default: always sample).
    let root_sampler = match cfg.sample_ratio {
        Some(r) if r < DEFAULT_OTEL_SAMPLE_RATIO => Sampler::TraceIdRatioBased(r.max(0.0)),
        _ => Sampler::AlwaysOn,
    };

    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource)
        .with_sampler(Sampler::ParentBased(Box::new(root_sampler)))
        .build();

    let tracer = provider.tracer(cfg.service_name.clone());
    // Register globally so `opentelemetry::global` helpers (context propagation)
    // resolve to this provider; the guard keeps the returned handle for flush.
    opentelemetry::global::set_tracer_provider(provider.clone());

    let layer = tracing_opentelemetry::layer().with_tracer(tracer).boxed();
    Ok((layer, provider))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_appends_v1_traces_to_base_endpoint() {
        assert_eq!(
            normalise_traces_endpoint("http://localhost:4318"),
            "http://localhost:4318/v1/traces"
        );
        assert_eq!(
            normalise_traces_endpoint("http://localhost:4318/"),
            "http://localhost:4318/v1/traces"
        );
    }

    #[test]
    fn normalise_leaves_full_signal_endpoint_untouched() {
        assert_eq!(
            normalise_traces_endpoint("http://collector:4318/v1/traces"),
            "http://collector:4318/v1/traces"
        );
        assert_eq!(
            normalise_traces_endpoint("http://collector:4318/v1/traces/"),
            "http://collector:4318/v1/traces"
        );
    }

    #[test]
    fn resolve_returns_none_without_any_endpoint() {
        // No env vars in this test process, no config endpoint → disabled.
        // (Guard against a CI env that sets these by skipping when present.)
        if std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").is_ok()
            || std::env::var("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT").is_ok()
        {
            return;
        }
        assert!(resolve_otel_config(None, None, None).is_none());
        assert!(resolve_otel_config(Some("   ".to_string()), None, None).is_none());
    }

    #[test]
    fn resolve_uses_config_endpoint_and_default_service_name() {
        if std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").is_ok()
            || std::env::var("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT").is_ok()
            || std::env::var("OTEL_SERVICE_NAME").is_ok()
        {
            return;
        }
        let cfg = resolve_otel_config(Some("http://otel:4318".to_string()), None, None)
            .expect("endpoint present → Some");
        assert_eq!(cfg.endpoint, "http://otel:4318/v1/traces");
        assert_eq!(cfg.service_name, "plombir-git");
    }
}
