//! Native bindings for the public Task bridge. Business rules stay in Core.
use crate::openhuman::channels::traits::ChannelMessage;
use crate::openhuman::inference::provider::ChatMessage;
use crate::openhuman::security::SecurityPolicy;
use crate::openhuman::tools::traits::{PermissionLevel, Tool, ToolResult};
use async_trait::async_trait;
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncWriteExt;

static NOTIFICATIONS: Lazy<Mutex<HashMap<String, Value>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
tokio::task_local! { static CURRENT: Arc<NativeTurn>; }

struct NativeTurn {
    context: Mutex<Value>,
    pending: Mutex<Vec<Value>>,
    room: String,
    session: String,
    coordination: Value,
    turn_id: String,
    carrier: HashMap<String, String>,
}

pub(crate) fn register(event: &str, mut meta: Value) {
    let mut notifications = NOTIFICATIONS.lock().unwrap();
    if meta.get("otel_carrier").is_none() {
        if let Some(carrier) = notifications
            .get(event)
            .and_then(|old| old.get("otel_carrier"))
        {
            meta["otel_carrier"] = carrier.clone();
        }
    }
    notifications.insert(event.to_owned(), meta);
}

pub(crate) fn assistant_notification(event: &str) -> bool {
    NOTIFICATIONS.lock().unwrap().get(event)
        .is_some_and(|meta| meta["teamharness_coordination"]["kind"] == "assistant_digest")
}

/// A scan may recover the same source after its verified Matrix packet arrived.
/// Fill absent metadata only; never replace the authoritative source packet.
pub(crate) fn register_recovered(event: &str, meta: Value) {
    let mut notifications = NOTIFICATIONS.lock().unwrap();
    let existing = notifications
        .entry(event.to_owned())
        .or_insert_with(|| json!({}));
    if let Some(fields) = meta.as_object() {
        for (key, value) in fields {
            if existing.get(key).is_none() {
                existing[key] = value.clone();
            }
        }
    }
}

fn home() -> PathBuf {
    std::env::var_os("AGENTTEAMS_WORKER_HOME")
        .map(PathBuf::from)
        .expect("Task member home is required")
}

async fn bridge(request: Value) -> anyhow::Result<Value> {
    let plugin = std::env::var_os("AGENTTEAMS_PLUGIN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/opt/agentteams/plugins/teamharness"));
    let mut child = tokio::process::Command::new("python3")
        .arg(plugin.join("mcp/common/task_native.py"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut input = child.stdin.take().expect("Task bridge stdin");
    input.write_all(request.to_string().as_bytes()).await?;
    drop(input);
    let output = child.wait_with_output().await?;
    let result: Value = serde_json::from_slice(&output.stdout)?;
    anyhow::ensure!(
        output.status.success() || result["effect"] == "none",
        "Public Task operation outcome requires authoritative verification"
    );
    Ok(result)
}

pub(crate) async fn scope<F: Future>(
    msg: &ChannelMessage,
    session: &str,
    future: F,
) -> anyhow::Result<F::Output> {
    let role = std::env::var("AGENTTEAMS_AGENT_ROLE").unwrap_or_else(|_| "worker".to_owned());
    let mut context = json!({"role":role,"sourceEvent":msg.id,"sources":[]});
    let meta = NOTIFICATIONS.lock().unwrap().remove(&msg.id);
    context["actor"] = meta.as_ref().map(|value| value["agent_matrix_user_id"].clone()).unwrap_or(Value::Null);
    let coordination = meta
        .as_ref()
        .map(|value| value["teamharness_coordination"].clone())
        .unwrap_or(Value::Null);
    let carrier = meta
        .as_ref()
        .and_then(|meta| meta.get("otel_carrier"))
        .and_then(Value::as_object)
        .map(|fields| {
            fields
                .iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default();
    if let Some(meta) = meta.filter(|meta| meta.get("teamharness_coordination").is_some()) {
        let mut event = meta["teamharness_coordination"].clone();
        if matches!(
            event["kind"].as_str(),
            Some("handoff" | "task_submitted" | "main_task_ready")
        ) {
            let id = event["id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Verified Task coordination identity required"))?;
            let admitted = bridge(json!({"operation":"admit_event","eventId":id,
                "nativeEventId":msg.id,"action":"begin"}))
            .await?;
            anyhow::ensure!(admitted["active"] == true, "Task phase is no longer active");
            if event["kind"] == "handoff" {
                event["pending_task_ids"] = admitted["pending_task_ids"].clone();
            }
        }
        let prepared = bridge(json!({"operation":"prepare_event","event":event})).await?;
        anyhow::ensure!(prepared["ok"] != false, "Task inputs are unavailable");
        context
            .as_object_mut()
            .unwrap()
            .extend(prepared.as_object().cloned().unwrap_or_default());
    } else if msg.id.starts_with('$') {
        let ids = super::matrix_direct::get(&msg.id)
            .map(|run| run.source_event_ids.clone())
            .unwrap_or_else(|| vec![msg.id.clone()]);
        let frames: Vec<_> = ids
            .into_iter()
            .map(|id| json!({"event_id":id,"room_id":msg.reply_target,"sender_id":msg.sender}))
            .collect();
        let sources =
            bridge(json!({"operation":"sources","meta":{"matrix_input_sources":frames}})).await?;
        anyhow::ensure!(
            sources.is_array(),
            "Native Human source identities are unavailable"
        );
        context["sources"] = sources;
    }
    if context["role"] == "manager" {
        let middle_path = home().join(".agentteams/task-delivery-context.json");
        let middle = if middle_path.is_file() {
            serde_json::from_slice::<Value>(&std::fs::read(middle_path)?)?
        } else {
            json!({})
        };
        context["delivery"] =
            bridge(json!({"operation":"delivery_context","room":msg.reply_target,"middle":middle}))
                .await?;
        context["deliveryMiddle"] = middle;
    }
    let turn = Arc::new(NativeTurn {
        context: Mutex::new(context),
        pending: Mutex::new(vec![]),
        room: msg.reply_target.clone(),
        session: session.to_owned(),
        coordination,
        turn_id: uuid::Uuid::new_v4().to_string(),
        carrier,
    });
    Ok(CURRENT
        .scope(turn, super::task_telemetry::turn(future))
        .await)
}

pub(crate) fn suppress_progress() -> bool {
    CURRENT.try_with(|turn| turn.coordination["kind"] == "assistant_digest").unwrap_or(false)
}

pub(crate) fn allowed_tool(name: &str) -> bool {
    CURRENT
        .try_with(|turn| {
            let context = turn.context.lock().unwrap();
            !(context.get("review").is_some()
                || context.get("completion").is_some()
                || context.get("planning").is_some())
                || name == "task"
        })
        .unwrap_or(true)
}

pub(crate) async fn prepare(history: &mut Vec<ChatMessage>) -> anyhow::Result<()> {
    let Ok(turn) = CURRENT.try_with(Arc::clone) else {
        return Ok(());
    };
    let context = turn.context.lock().unwrap().clone();
    if let Some(fact) = context["fact"].as_str() {
        let projected = bridge(json!({"operation":"model_context","context":context["context"]})).await?;
        history.push(ChatMessage::system(format!(
            "{fact}\n{}",
            projected
        )));
    }
    if let Some(sources) = context["sources"]
        .as_array()
        .filter(|items| !items.is_empty())
    {
        let refs: Vec<_> = sources
            .iter()
            .map(|source| json!({"inputId":source["inputId"],"eventId":source["eventId"]}))
            .collect();
        history.push(ChatMessage::system(format!(
            "[Task input references]\n{}",
            json!(refs)
        )));
    }
    if let Some(facts) = context["delivery"]["facts"]
        .as_array()
        .filter(|items| !items.is_empty())
    {
        for fact in facts {
            history
                .push(ChatMessage::system(fact.as_str().ok_or_else(|| {
                    anyhow::anyhow!("Task delivery fact is invalid")
                })?));
        }
        let source = context["sourceEvent"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Native source identity missing"))?;
        super::session_receipts::checkpoint(
            &home(),
            &turn.room,
            source,
            &turn.session,
            history,
            &[source.to_owned()],
        )?;
        let middle = bridge(json!({"operation":"ack_delivery_context","middle":context["deliveryMiddle"],"acknowledgements":context["delivery"]["acknowledgements"]})).await?;
        let path = home().join(".agentteams/task-delivery-context.json");
        let temporary = path.with_extension("tmp");
        std::fs::write(&temporary, serde_json::to_vec(&middle)?)?;
        std::fs::File::open(&temporary)?.sync_all()?;
        std::fs::rename(temporary, path)?;
    }
    Ok(())
}

pub(crate) async fn attach_pending(history: &mut Vec<ChatMessage>) -> anyhow::Result<()> {
    let Ok(turn) = CURRENT.try_with(Arc::clone) else {
        return Ok(());
    };
    let pending = std::mem::take(&mut *turn.pending.lock().unwrap());
    for source in &pending {
        let event = source["eventId"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Task source identity missing"))?;
        let body = source["body"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Task source content missing"))?;
        let input = ChatMessage::user(format!("[Message {event}]\n{body}"));
        history.push(input.clone());
        super::session_receipts::checkpoint(
            &home(),
            &turn.room,
            event,
            &turn.session,
            &input,
            &[event.to_owned()],
        )?;
    }
    if !pending.is_empty() {
        super::task_recovery::checkpoint(history).await?;
        turn.context.lock().unwrap()["inputRefreshPending"] = json!(false);
    }
    Ok(())
}

/// Called at the actual inference/tool boundary with the owned recovery result.
pub(crate) async fn admit(history: &mut Vec<ChatMessage>, owned: &Value) -> anyhow::Result<()> {
    let Ok(turn) = CURRENT.try_with(Arc::clone) else {
        return Ok(());
    };
    let result = if owned.get("receipts").is_some() {
        owned.clone()
    } else {
        let context = turn.context.lock().unwrap().clone();
        let result = bridge(json!({"operation":"canonical_admit","context":context})).await?;
        anyhow::ensure!(
            result["ok"] == true,
            "Canonical Task outcome requires authoritative verification"
        );
        result
    };
    let mut changed = false;
    for receipt in result["receipts"].as_array().into_iter().flatten() {
        let fact = receipt["fact"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid canonical receipt"))?;
        if !history
            .iter()
            .any(|message| message.role == "system" && message.content == fact)
        {
            history.push(ChatMessage::system(fact));
            changed = true;
        }
    }
    if changed {
        let context = turn.context.lock().unwrap().clone();
        let source = context["sourceEvent"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Native source missing"))?;
        super::session_receipts::checkpoint(
            &home(),
            &turn.room,
            source,
            &turn.session,
            history,
            &[source.to_owned()],
        )?;
        super::task_recovery::checkpoint(history).await?;
    }
    Ok(())
}

pub(crate) fn completed() -> Option<String> {
    CURRENT
        .try_with(|turn| {
            turn.context.lock().unwrap()["phaseResult"]
                .as_str()
                .map(str::to_owned)
        })
        .ok()
        .flatten()
}

pub(crate) fn output_directory() -> Option<PathBuf> {
    CURRENT
        .try_with(|turn| {
            turn.context.lock().unwrap()["context"]["outputDirectory"]
                .as_str()
                .map(PathBuf::from)
        })
        .ok()
        .flatten()
}

pub(crate) fn tool_timeout_secs(name: &str, configured: u64, explicit_override: bool) -> u64 {
    if name == "shell" && configured == 120 && !explicit_override && output_directory().is_some() {
        310 // Leave time for the inner 300-second command and process cleanup.
    } else {
        configured
    }
}

pub(crate) fn scratch_directory() -> Option<PathBuf> {
    CURRENT
        .try_with(|turn| {
            turn.context.lock().unwrap()["context"]["scratchDirectory"]
                .as_str()
                .map(PathBuf::from)
        })
        .ok()
        .flatten()
}

pub(crate) fn file_scope(
    policy: &SecurityPolicy,
    path: &str,
    write: bool,
) -> anyhow::Result<(PathBuf, String)> {
    let Some(output) = output_directory() else {
        return Ok((policy.workspace_dir.clone(), path.to_owned()));
    };
    let root = if write {
        output.clone()
    } else {
        CURRENT
            .try_with(|turn| {
                turn.context.lock().unwrap()["context"]["taskDirectory"]
                    .as_str()
                    .map(PathBuf::from)
            })?
            .ok_or_else(|| anyhow::anyhow!("Task directory missing"))?
    };
    let target = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        output.join(path)
    };
    let relative = target
        .strip_prefix(&root)
        .map_err(|_| anyhow::anyhow!("File is outside the current Task scope"))?;
    let relative = relative.to_string_lossy().into_owned();
    Ok((root, relative))
}

pub(crate) struct TaskTool;
#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        "task"
    }
    fn description(&self) -> &str {
        "Operate on the current Task through the common task system. Use Task and artifact IDs; the system owns routing, input preparation and notifications."
    }
    fn parameters_schema(&self) -> Value {
        std::fs::read(home().join(".agentteams/native-task-schema.json"))
            .ok()
            .and_then(|data| serde_json::from_slice::<Value>(&data).ok())
            .expect("Public Task schema must be prepared before native startup")["inputSchema"]
            .clone()
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::None
    }
    fn native_operation_registration(&self) -> Option<Value> {
        Some(
            json!({"origin":"agentteams.common.task_native", "entrypoint":"/opt/agentteams/plugins/teamharness/mcp/common/task_native.py"}),
        )
    }
    async fn execute(&self, arguments: Value) -> anyhow::Result<ToolResult> {
        let turn = CURRENT.try_with(Arc::clone)?;
        let context = turn.context.lock().unwrap().clone();
        let outcome =
            bridge(json!({"operation":"task","arguments":arguments,"context":context})).await?;
        {
            let mut current = turn.context.lock().unwrap();
            if let Some(update) = outcome["contextUpdate"].as_object() {
                current.as_object_mut().unwrap().extend(update.clone());
            }
            if outcome["requiresInputCheckpoint"] == true {
                current["inputRefreshPending"] = json!(true);
                *turn.pending.lock().unwrap() = outcome["refreshedInputs"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
            }
            if outcome["phaseComplete"] == true {
                current["phaseResult"] = json!(outcome["displayText"].as_str().unwrap_or(""));
            }
        }
        if outcome["effect"] == "none" && outcome["ok"] == false {
            return Ok(ToolResult::error(outcome.to_string()));
        }
        let result = bridge(json!({"operation":"model_result","outcome":outcome})).await?;
        let text = result.to_string();
        Ok(if result["ok"] == false {
            ToolResult::error(text)
        } else {
            ToolResult::success(text)
        })
    }
}

pub(crate) fn trace_fields() -> Vec<opentelemetry::KeyValue> {
    use opentelemetry::KeyValue;
    CURRENT
        .try_with(|turn| {
            let context = turn.context.lock().unwrap();
            let mut fields = vec![
                KeyValue::new("agentteams.runtime", "openhuman"),
                KeyValue::new("agentteams.telemetry.schema", "1"),
                KeyValue::new("agentteams.telemetry.capture_content", false),
                KeyValue::new("agentteams.room.id", turn.room.clone()),
                KeyValue::new("agentteams.session.id", turn.session.clone()),
                KeyValue::new("agentteams.turn.id", turn.turn_id.clone()),
                KeyValue::new(
                    "agentteams.worker.name",
                    std::env::var("AGENTTEAMS_WORKER_NAME").unwrap_or_else(|_| "unknown".into()),
                ),
            ];
            if let Ok(team) = std::env::var("AGENTTEAMS_TEAM_NAME") {
                if !team.is_empty() {
                    fields.push(KeyValue::new("agentteams.team.id", team));
                }
            }
            if let Some(event) = context["sourceEvent"]
                .as_str()
                .filter(|id| id.starts_with('$'))
            {
                fields.push(KeyValue::new("agentteams.event.id", event.to_owned()));
            }
            let sources = context["sources"]
                .as_array()
                .map(|sources| {
                    sources
                        .iter()
                        .filter_map(|source| source["eventId"].as_str().map(str::to_owned))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            fields.push(KeyValue::new(
                "agentteams.input.event_ids",
                opentelemetry::Value::Array(opentelemetry::Array::String(
                    sources.into_iter().map(Into::into).collect(),
                )),
            ));
            for key in ["execution", "completion"] {
                if let Some(id) = context[key].as_str() {
                    fields.push(KeyValue::new("agentteams.task.id", id.to_owned()));
                }
            }
            if let Some(id) = context["review"][0].as_str() {
                fields.push(KeyValue::new("agentteams.task.id", id.to_owned()));
            }
            if let Some(generation) = context["context"]["generation"].as_u64() {
                fields.push(KeyValue::new(
                    "agentteams.execution.generation",
                    generation as i64,
                ));
            }
            for (key, field) in [
                ("id", "agentteams.coordination.id"),
                ("kind", "agentteams.coordination.kind"),
            ] {
                if let Some(value) = turn.coordination[key].as_str() {
                    fields.push(KeyValue::new(field, value.to_owned()));
                }
            }
            fields
        })
        .unwrap_or_default()
}

pub(crate) fn trace_parent() -> opentelemetry::Context {
    use opentelemetry::propagation::TextMapPropagator;
    CURRENT
        .try_with(|turn| {
            opentelemetry_sdk::propagation::TraceContextPropagator::new().extract(&turn.carrier)
        })
        .unwrap_or_default()
}
