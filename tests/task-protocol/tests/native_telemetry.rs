//! Real OTLP exporter/producer readback; does not replace a full SDK turn test.
#![allow(dead_code)]
#[path = "../../../patches/task_telemetry.rs"]
mod native;
mod openhuman {
    pub mod channels {
        pub mod task_protocol {
            pub fn trace_parent() -> opentelemetry::Context {
                use opentelemetry::propagation::TextMapPropagator;
                opentelemetry_sdk::propagation::TraceContextPropagator::new().extract(
                    &std::collections::HashMap::from([(
                        "traceparent".to_owned(),
                        "00-123456789012345678901234567890ab-12345678901234ab-01".to_owned(),
                    )]),
                )
            }
            pub fn trace_fields() -> Vec<opentelemetry::KeyValue> {
                vec![
                    opentelemetry::KeyValue::new("agentteams.event.id", "$synthetic-native"),
                    opentelemetry::KeyValue::new("agentteams.telemetry.capture_content", false),
                ]
            }
        }
    }
}

#[tokio::test]
async fn native_exporter_exports_real_boundary_usage_without_contents() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    std::env::set_var(
        "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
        format!("http://{}/v1/traces", listener.local_addr().unwrap()),
    );
    std::env::set_var("AGENTTEAMS_OTEL_ENABLED", "true");
    let collector = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let mut request = vec![];
        let body;
        loop {
            let mut buffer = [0; 4096];
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0);
            request.extend_from_slice(&buffer[..read]);
            if let Some(offset) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..offset]);
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        line.to_lowercase()
                            .strip_prefix("content-length:")
                            .map(|s| s.trim().parse().unwrap())
                    })
                    .unwrap();
                if request.len() >= offset + 4 + length {
                    body = request[offset + 4..offset + 4 + length].to_vec();
                    break;
                }
            }
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
        body
    });
    native::turn(async {
        let generation = native::boundary(
            "gen_ai.chat",
            vec![opentelemetry::KeyValue::new(
                "gen_ai.request.model",
                "own-model",
            )],
        );
        tokio::task::yield_now().await;
        generation.finish(true, Some((11, 3, 2)));
        let tool = native::boundary(
            "tool.execute",
            vec![opentelemetry::KeyValue::new("tool.name", "task")],
        );
        tool.finish(false, None);
    })
    .await;
    tokio::task::spawn_blocking(native::flush).await.unwrap();
    let body = collector.join().unwrap();
    for marker in [
        "agent.turn",
        "gen_ai.chat",
        "tool.execute",
        "gen_ai.usage.input_tokens",
        "gen_ai.usage.output_tokens",
        "$synthetic-native",
        "native_operation_failed",
    ] {
        assert!(
            body.windows(marker.len())
                .any(|bytes| bytes == marker.as_bytes()),
            "missing {marker}"
        );
    }
    assert!(!body
        .windows(b"SYNTHETIC_PRIVATE_CONTENT".len())
        .any(|bytes| bytes == b"SYNTHETIC_PRIVATE_CONTENT"));
    let trace_id = [
        0x12, 0x34, 0x56, 0x78, 0x90, 0x12, 0x34, 0x56, 0x78, 0x90, 0x12, 0x34, 0x56, 0x78, 0x90,
        0xab,
    ];
    assert!(
        body.windows(16).filter(|bytes| *bytes == trace_id).count() >= 3,
        "native turn/model/tool spans lost the incoming Matrix trace carrier"
    );
}
