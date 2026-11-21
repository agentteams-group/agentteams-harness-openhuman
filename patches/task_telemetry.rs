//! Actual native model/tool boundaries. Never export messages or tool payloads.
use once_cell::sync::Lazy;
use opentelemetry::trace::{Span, SpanKind, TraceContextExt, Tracer, TracerProvider};
use opentelemetry::{Context, KeyValue};
use opentelemetry_sdk::trace::{SdkTracerProvider, Span as SdkSpan};
use opentelemetry_sdk::Resource;
use std::future::Future;

static PROVIDER: Lazy<Option<SdkTracerProvider>> =
    Lazy::new(|| std::thread::spawn(build_provider).join().unwrap_or(None));

fn build_provider() -> Option<SdkTracerProvider> {
    if !matches!(
        std::env::var("AGENTTEAMS_OTEL_ENABLED").as_deref(),
        Ok("true" | "1")
    ) {
        return None;
    }
    // The batch exporter owns its worker thread; the blocking client does not
    // depend on a Tokio reactor being entered on that thread.
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .build();
    match exporter {
        Ok(exporter) => Some(
            SdkTracerProvider::builder()
                .with_resource(
                    Resource::builder()
                        .with_service_name(
                            std::env::var("OTEL_SERVICE_NAME")
                                .unwrap_or_else(|_| "agentteams-worker".into()),
                        )
                        .build(),
                )
                .with_batch_exporter(exporter)
                .build(),
        ),
        Err(_) => {
            tracing::warn!("Native Task telemetry exporter is unavailable");
            None
        }
    }
}

tokio::task_local! { static PARENT: Context; }

pub(crate) struct Boundary(Option<SdkSpan>);
impl Boundary {
    pub(crate) fn finish(mut self, success: bool, usage: Option<(u64, u64, u64)>) {
        if let Some(span) = &mut self.0 {
            span.set_attribute(KeyValue::new("success", success));
            if !success {
                span.set_attribute(KeyValue::new("error.type", "native_operation_failed"));
            }
            if let Some((input, output, cached)) = usage {
                span.set_attribute(KeyValue::new("gen_ai.usage.input_tokens", input as i64));
                span.set_attribute(KeyValue::new("gen_ai.usage.output_tokens", output as i64));
                span.set_attribute(KeyValue::new(
                    "gen_ai.usage.cache_read.input_tokens",
                    cached as i64,
                ));
            }
            span.end();
        }
    }
}

pub(crate) fn boundary(name: &str, attributes: Vec<KeyValue>) -> Boundary {
    let Some(provider) = PROVIDER.as_ref() else {
        return Boundary(None);
    };
    let mut attributes = attributes;
    attributes.extend(crate::openhuman::channels::task_protocol::trace_fields());
    let parent = PARENT.try_with(Clone::clone).unwrap_or_default();
    let span = provider
        .tracer("agentteams.openhuman.native")
        .span_builder(name.to_owned())
        .with_kind(SpanKind::Internal)
        .with_attributes(attributes)
        .start_with_context(&provider.tracer("agentteams.openhuman.native"), &parent);
    Boundary(Some(span))
}

pub(crate) async fn turn<F: Future>(future: F) -> F::Output {
    let Some(provider) = PROVIDER.as_ref() else {
        return future.await;
    };
    let span = provider
        .tracer("agentteams.openhuman.native")
        .span_builder("agent.turn")
        .with_attributes(crate::openhuman::channels::task_protocol::trace_fields())
        .start(&provider.tracer("agentteams.openhuman.native"));
    let context = Context::new().with_span(span);
    let result = PARENT.scope(context.clone(), future).await;
    context.span().end();
    result
}

#[cfg(test)]
pub(crate) fn flush() {
    if let Some(provider) = PROVIDER.as_ref() {
        provider.force_flush().unwrap();
    }
}
