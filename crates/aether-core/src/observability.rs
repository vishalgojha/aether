use std::env;

use opentelemetry::trace::TracerProvider as _;
use opentelemetry::{global, KeyValue};
use opentelemetry_otlp::{SpanExporter, WithExportConfig};
use opentelemetry_sdk::trace::{SdkTracer, SdkTracerProvider};
use opentelemetry_sdk::Resource;
use tracing::info;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Registry};

pub struct ObservabilityGuard {
    tracer_provider: Option<SdkTracerProvider>,
}

impl Drop for ObservabilityGuard {
    fn drop(&mut self) {
        if let Some(provider) = self.tracer_provider.take() {
            let _ = provider.shutdown();
        }
    }
}

pub fn init_observability(service_name: &str) -> anyhow::Result<ObservabilityGuard> {
    let log_format = LogFormat::from_env();
    let otel_enabled = env_flag("AETHER_OTEL_ENABLED");

    let tracer_provider = if otel_enabled {
        init_with_otel(service_name, log_format)?
    } else {
        init_without_otel(log_format)?;
        None
    };

    info!(
        service = service_name,
        log_format = ?log_format,
        otel_enabled,
        "observability initialized"
    );

    Ok(ObservabilityGuard { tracer_provider })
}

fn init_without_otel(log_format: LogFormat) -> anyhow::Result<()> {
    match log_format {
        LogFormat::Json => Registry::default()
            .with(default_filter())
            .with(
                fmt::layer()
                    .json()
                    .with_target(true)
                    .with_thread_ids(true)
                    .with_thread_names(true),
            )
            .try_init()?,
        LogFormat::Pretty => Registry::default()
            .with(default_filter())
            .with(
                fmt::layer()
                    .pretty()
                    .with_target(true)
                    .with_thread_ids(true)
                    .with_thread_names(true),
            )
            .try_init()?,
        LogFormat::Both => Registry::default()
            .with(default_filter())
            .with(
                fmt::layer()
                    .pretty()
                    .with_target(true)
                    .with_thread_ids(true)
                    .with_thread_names(true),
            )
            .with(
                fmt::layer()
                    .json()
                    .with_target(true)
                    .with_thread_ids(true)
                    .with_thread_names(true),
            )
            .try_init()?,
    }
    Ok(())
}

fn init_with_otel(
    service_name: &str,
    log_format: LogFormat,
) -> anyhow::Result<Option<SdkTracerProvider>> {
    let (otel_layer, provider) = build_otel_layer(service_name)?;

    match log_format {
        LogFormat::Json => Registry::default()
            .with(otel_layer)
            .with(default_filter())
            .with(
                fmt::layer()
                    .json()
                    .with_target(true)
                    .with_thread_ids(true)
                    .with_thread_names(true),
            )
            .try_init()?,
        LogFormat::Pretty => Registry::default()
            .with(otel_layer)
            .with(default_filter())
            .with(
                fmt::layer()
                    .pretty()
                    .with_target(true)
                    .with_thread_ids(true)
                    .with_thread_names(true),
            )
            .try_init()?,
        LogFormat::Both => Registry::default()
            .with(otel_layer)
            .with(default_filter())
            .with(
                fmt::layer()
                    .pretty()
                    .with_target(true)
                    .with_thread_ids(true)
                    .with_thread_names(true),
            )
            .with(
                fmt::layer()
                    .json()
                    .with_target(true)
                    .with_thread_ids(true)
                    .with_thread_names(true),
            )
            .try_init()?,
    }

    Ok(Some(provider))
}

fn build_otel_layer(
    service_name: &str,
) -> anyhow::Result<(
    tracing_opentelemetry::OpenTelemetryLayer<Registry, SdkTracer>,
    SdkTracerProvider,
)> {
    let mut exporter_builder = SpanExporter::builder().with_tonic();
    if let Ok(endpoint) = env::var("AETHER_OTEL_ENDPOINT") {
        if !endpoint.trim().is_empty() {
            exporter_builder = exporter_builder.with_endpoint(endpoint);
        }
    }

    let exporter = exporter_builder.build()?;
    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(
            Resource::builder()
                .with_attributes(vec![KeyValue::new(
                    "service.name",
                    service_name.to_string(),
                )])
                .build(),
        )
        .build();
    let tracer = provider.tracer(service_name.to_string());
    global::set_tracer_provider(provider.clone());

    Ok((tracing_opentelemetry::layer().with_tracer(tracer), provider))
}

fn default_filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
}

#[derive(Clone, Copy, Debug)]
enum LogFormat {
    Json,
    Pretty,
    Both,
}

impl LogFormat {
    fn from_env() -> Self {
        match env::var("AETHER_LOG_FORMAT")
            .unwrap_or_else(|_| "json".to_string())
            .to_lowercase()
            .as_str()
        {
            "pretty" => LogFormat::Pretty,
            "both" => LogFormat::Both,
            _ => LogFormat::Json,
        }
    }
}

fn env_flag(key: &str) -> bool {
    env::var(key)
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}
