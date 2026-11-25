//! Native Matrix display only. No run blocks are appended to model history.
use crate::openhuman::approval::matrix::Context;
use serde_json::{json, Value};
use std::{
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

tokio::task_local! { static DISPLAY: Arc<Display>; }
struct Display {
    context: Arc<Context>,
    run_id: String,
    published: AtomicBool,
}
pub(crate) struct Call {
    display: Arc<Display>,
    event: String,
    block: Value,
}

fn safe(value: &Value) -> Value {
    match value {
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| {
                    let lower = key.to_ascii_lowercase();
                    let secret = [
                        "token",
                        "password",
                        "api_key",
                        "api-key",
                        "apikey",
                        "secret",
                        "authorization",
                        "credential",
                    ]
                    .iter()
                    .any(|part| lower.contains(part));
                    (
                        key.clone(),
                        if secret {
                            json!("[REDACTED]")
                        } else {
                            safe(value)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(safe).collect()),
        Value::String(text) => {
            if let Ok(parsed @ (Value::Object(_) | Value::Array(_))) =
                serde_json::from_str::<Value>(text)
            {
                return json!(safe(&parsed).to_string());
            }
            let mut text = text.clone();
            for (key, secret) in std::env::vars() {
                if key == "AGENTTEAMS_FS_ACCESS_KEY"
                    && secret == std::env::var("AGENTTEAMS_WORKER_NAME").unwrap_or_default()
                {
                    continue;
                }
                if secret.len() >= 8
                    && ["TOKEN", "PASSWORD", "SECRET", "KEY", "AUTH"]
                        .iter()
                        .any(|part| key.contains(part))
                {
                    text = text.replace(&secret, "[REDACTED]");
                }
            }
            static PATTERNS: std::sync::LazyLock<Vec<(regex::Regex, &str)>> =
                std::sync::LazyLock::new(|| {
                    vec![
                (regex::Regex::new(r#"(?i)(Bearer\s+)[^\s\"',]+"#).unwrap(), "${1}[REDACTED]"),
                (regex::Regex::new(r#"(?i)((?:access_token|api_key|apiKey|password|secret|authorization)[\"']?\s*[:=]\s*)(\"[^\"]*\"|'[^']*'|[^\s,}]+)"#).unwrap(), "${1}\"[REDACTED]\""),
                (regex::Regex::new(r"(https?://)[^/\s@]+@").unwrap(), "${1}[REDACTED]@"),
            ]
                });
            for (pattern, replacement) in PATTERNS.iter() {
                text = pattern.replace_all(&text, *replacement).into_owned();
            }
            json!(text)
        }
        _ => value.clone(),
    }
}
fn bounded(value: &Value) -> Value {
    let value = safe(value);
    let text = value.to_string();
    if text.len() > 4096 {
        json!({"preview":text.chars().take(4096).collect::<String>(), "truncated":true})
    } else {
        value
    }
}
fn now() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
impl Display {
    async fn send(&self, block: Value, previous: Option<&str>) -> anyhow::Result<String> {
        let content = json!({"msgtype":"m.notice", "body":block["tool_name"].as_str().unwrap_or("执行过程"),
            "org.agentteams.progress_only":true, "m.mentions":{"user_ids":[]},
            "org.agentteams.run":{"version":"1", "run_id":self.run_id, "blocks":[block]}});
        let content = if let Some(event) = previous {
            json!({
            "msgtype":"m.notice", "body":"执行过程", "org.agentteams.progress_only":true,
            "m.new_content":content, "m.relates_to":{"rel_type":"m.replace","event_id":event}})
        } else {
            content
        };
        let event = self
            .context
            .channel
            .send_progress(&self.context.room, content)
            .await?;
        self.published.store(true, Ordering::Release);
        Ok(event)
    }
    async fn end(&self, kind: &str) {
        if self.published.load(Ordering::Acquire) {
            if let Err(error) = self
                .send(
                    json!({"type":"error","kind":kind,"title":if kind == "quiet" { "已处理" } else { "执行已中断" }}),
                    None,
                )
                .await
            {
                tracing::warn!(%error, "Matrix native progress closure failed");
            }
        }
    }
}
struct CloseOnDrop {
    display: Arc<Display>,
    finished: bool,
}
impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        if !self.finished {
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                let display = self.display.clone();
                runtime.spawn(async move { display.end("cancelled").await });
            }
        }
    }
}
pub(crate) async fn scope<F: Future>(run_id: String, future: F) -> F::Output {
    let run_id = crate::openhuman::channels::task_recovery::progress_run_id().unwrap_or(run_id);
    let context = crate::openhuman::approval::matrix::CONTEXT
        .try_with(Arc::clone)
        .ok();
    let Some(context) = context else {
        return future.await;
    };
    let display = Arc::new(Display {
        context,
        run_id,
        published: AtomicBool::new(false),
    });
    let mut closing = CloseOnDrop {
        display: display.clone(),
        finished: false,
    };
    let result = DISPLAY.scope(display.clone(), future).await;
    display.end("quiet").await;
    closing.finished = true;
    result
}
pub(crate) fn reply_content(text: &str, transaction: &str, run_id: &str) -> Value {
    json!({"msgtype":"m.text", "body":text,
        "org.agentteams.reply_txn":transaction, "org.agentteams.progress_only":true,
        "org.agentteams.run_id":run_id, "m.mentions":{"user_ids":[]}})
}
pub(crate) async fn thinking(text: Option<&str>) {
    if super::task_protocol::suppress_progress() {
        return;
    }
    let Some(text) = text.filter(|text| !text.is_empty()) else {
        return;
    };
    if let Ok(display) = DISPLAY.try_with(Arc::clone) {
        let text = safe(&json!(text))
            .as_str()
            .unwrap_or_default()
            .chars()
            .take(4096)
            .collect::<String>();
        if let Err(error) = display
            .send(json!({"type":"thinking","content":text}), None)
            .await
        {
            tracing::warn!(%error, "Matrix native reasoning delivery failed");
        }
    }
}
pub(crate) async fn start(id: &str, name: &str, arguments: &Value) -> Option<Call> {
    if super::task_protocol::suppress_progress() {
        return None;
    }
    let display = DISPLAY.try_with(Arc::clone).ok()?;
    let block = json!({"type":"tool_call","tool_call_id":id,"tool_name":name,
        "arguments":bounded(arguments),"status":"running","started_at":now()});
    match display.send(block.clone(), None).await {
        Ok(event) => Some(Call {
            display,
            event,
            block,
        }),
        Err(error) => {
            tracing::warn!(%error, "Matrix native tool start delivery failed");
            None
        }
    }
}
pub(crate) async fn finish(call: Option<Call>, result: &str, success: bool) {
    if let Some(mut call) = call {
        call.block["status"] = json!(if success { "succeeded" } else { "failed" });
        call.block["result"] = bounded(&json!(result));
        call.block["finished_at"] = json!(now());
        if let Err(error) = call.display.send(call.block, Some(&call.event)).await {
            tracing::warn!(%error, "Matrix native tool result delivery failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openhuman::channels::traits::{Channel, ChannelMessage, SendMessage};
    use std::sync::Mutex;
    #[test]
    fn saved_business_reply_keeps_the_native_process_identity_without_another_receipt() {
        let content = reply_content("独立验证通过，已交付 calc.py。", "own-transaction", "own-run");
        assert_eq!(content["org.agentteams.run_id"], "own-run");
        assert_eq!(content["body"], "独立验证通过，已交付 calc.py。");
        assert_eq!(content["msgtype"], "m.text");
        assert_eq!(content["org.agentteams.reply_txn"], "own-transaction");
        assert_eq!(content["org.agentteams.progress_only"], true);
        assert_eq!(content["m.mentions"]["user_ids"], json!([]));
        assert!(content.get("org.agentteams.run").is_none());
    }
    #[derive(Default)]
    struct Matrix {
        sent: Mutex<Vec<Value>>,
    }
    #[async_trait::async_trait]
    impl Channel for Matrix {
        fn name(&self) -> &str {
            "matrix"
        }
        async fn send(&self, _: &SendMessage) -> anyhow::Result<()> {
            Ok(())
        }
        async fn listen(&self, _: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
            Ok(())
        }
        async fn send_progress(&self, room: &str, content: Value) -> anyhow::Result<String> {
            assert_eq!(room, "!own");
            let mut rows = self.sent.lock().unwrap();
            rows.push(content);
            Ok(format!("${}", rows.len()))
        }
    }
    #[tokio::test]
    async fn native_progress_keeps_ids_inputs_outputs_and_failure_independent_of_model_context() {
        assert_eq!(
            safe(&json!("Authorization: Bearer unconfigured-private-token")),
            json!("Authorization: \"[REDACTED]\" [REDACTED]")
        );
        assert!(
            !safe(&json!(r#"{"password":"unconfigured-private-value"}"#))
                .to_string()
                .contains("unconfigured-private-value")
        );
        let channel = Arc::new(Matrix::default());
        let context = Arc::new(Context {
            channel: channel.clone(),
            room: "!own".into(),
            sender: "@human".into(),
        });
        let display = Arc::new(Display {
            context,
            run_id: "own-run".into(),
            published: AtomicBool::new(false),
        });
        DISPLAY
            .scope(display.clone(), async {
                thinking(Some("独立检查页面。")).await;
                let first = start(
                    "one",
                    "read",
                    &json!({"path":"index.html","api_key":"sensitive-value"}),
                )
                .await;
                let second = start("two", "read", &json!({"path":"missing.html"})).await;
                finish(second, "not found", false).await;
                finish(first, "<html>ok</html>", true).await;
            })
            .await;
        display.end("quiet").await;
        let rows = channel.sent.lock().unwrap();
        assert_eq!(rows.len(), 6);
        let content = |index: usize| rows[index].get("m.new_content").unwrap_or(&rows[index]);
        let block = |index: usize| content(index)["org.agentteams.run"]["blocks"][0].clone();
        assert_eq!(block(0)["content"], "独立检查页面。");
        assert_eq!(block(3)["tool_call_id"], "two");
        assert_eq!(block(3)["status"], "failed");
        assert_eq!(block(4)["tool_call_id"], "one");
        assert_eq!(block(4)["status"], "succeeded");
        assert_eq!(block(4)["arguments"]["api_key"], "[REDACTED]");
        assert_eq!(block(4)["result"], "<html>ok</html>");
        assert_eq!(rows[3]["m.relates_to"]["event_id"], "$3");
        assert_eq!(rows[4]["m.relates_to"]["event_id"], "$2");
        assert_eq!(block(5)["kind"], "quiet");
        assert!(rows
            .iter()
            .enumerate()
            .all(|(i, _)| content(i)["org.agentteams.progress_only"] == true
                && content(i)["org.agentteams.run"]["run_id"] == "own-run"
                && content(i)["m.mentions"]["user_ids"] == json!([])));
    }

    #[tokio::test]
    async fn cancelled_native_scope_closes_the_process_without_claiming_task_success() {
        let channel = Arc::new(Matrix::default());
        let context = Arc::new(Context {
            channel: channel.clone(),
            room: "!own".into(),
            sender: "@human".into(),
        });
        let turn = crate::openhuman::approval::matrix::CONTEXT.scope(
            context,
            scope("cancelled-run".into(), async {
                start("one", "read", &json!({"path":"calc.py"})).await;
                std::future::pending::<()>().await;
            }),
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), turn)
                .await
                .is_err()
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let rows = channel.sent.lock().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[1]["org.agentteams.run"]["blocks"][0]["kind"],
            "cancelled"
        );
    }
}
