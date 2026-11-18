//! Worker Task authority is shared with the other Harnesses; history is native.
use super::traits::ChannelMessage;
use crate::openhuman::inference::provider::ChatMessage;
use matrix_sdk::{config::RequestConfig, Client, RoomState};
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, Notify};
use tokio_util::sync::CancellationToken;

static RUNS: Lazy<Mutex<HashMap<String, Arc<Run>>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static ADMITTED: Lazy<Mutex<HashSet<String>>> = Lazy::new(|| Mutex::new(HashSet::new()));
static SOURCES: Lazy<Mutex<HashMap<String, String>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static READY: AtomicBool = AtomicBool::new(false);
static MATRIX_INBOX_READY: Notify = Notify::const_new();
static WAKE: Notify = Notify::const_new();

tokio::task_local! { pub(crate) static TASK_RUN: Arc<Run>; }

pub(crate) async fn check_authority(for_tool: bool) -> anyhow::Result<()> {
    if let Ok(run) = TASK_RUN.try_with(Arc::clone) {
        let current = run.current().await.map_err(anyhow::Error::msg)?;
        if for_tool && current["task"]["execution"]["state"] == "released" {
            anyhow::bail!("Task execution was released; only saved-result delivery is permitted");
        }
    }
    Ok(())
}

/// Called only after the native tool loop has appended its actual tool results.
pub(crate) async fn checkpoint(history: &[ChatMessage]) -> anyhow::Result<()> {
    if let Ok(run) = TASK_RUN.try_with(Arc::clone) {
        run.remember(history).await.map_err(anyhow::Error::msg)?;
    }
    Ok(())
}

pub(crate) struct Run {
    identity: Value,
    work: Value,
    scope: Value,
    admission: String,
    client: Client,
    room: String,
    session: String,
    restored: Vec<ChatMessage>,
    native_history: Mutex<Vec<ChatMessage>>,
    cache_path: PathBuf,
    pub cancel: CancellationToken,
    pub started: AtomicBool,
    pub finished: AtomicBool,
    pub succeeded: AtomicBool,
}

pub(crate) fn enabled() -> bool {
    std::env::var_os("AGENTTEAMS_CONTROLLER_URL").is_some()
        && std::env::var_os("AGENTTEAMS_PROCESS_ID").is_some()
        && matches!(
            std::env::var("AGENTTEAMS_AGENT_ROLE")
                .as_deref()
                .unwrap_or("worker"),
            "worker" | "standalone" | "leader" | "team_leader" | "remote-member"
        )
}

pub(crate) fn get(id: &str) -> Option<Arc<Run>> {
    RUNS.lock().unwrap().get(id).cloned()
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn digest(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string()))
}
fn authority() -> String {
    digest(&json!([
        std::env::var("AGENTTEAMS_CONTROLLER_URL").ok(),
        std::env::var("AGENTTEAMS_WORKER_NAME").ok()
    ]))
}
fn home() -> PathBuf {
    std::env::var_os("AGENTTEAMS_WORKER_HOME")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .expect("Worker home is required")
}

pub(crate) async fn rpc(mut request: Value) -> Result<Value, &'static str> {
    request["device_delivery"] = json!(true);
    request["runtime"] = json!("openhuman");
    let mut child = tokio::process::Command::new("python3")
        .arg("/opt/agentteams/scripts/lib/task_recovery_rpc.py")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "unavailable")?;
    let mut stdin = child.stdin.take().ok_or("unavailable")?;
    stdin
        .write_all(request.to_string().as_bytes())
        .await
        .map_err(|_| "unavailable")?;
    drop(stdin);
    let result = tokio::time::timeout(Duration::from_secs(20), child.wait_with_output())
        .await
        .map_err(|_| "unavailable")?
        .map_err(|_| "unavailable")?;
    if result.stdout.len() > 2 * 1024 * 1024 {
        return Err("unavailable");
    }
    let output: Value = serde_json::from_slice(&result.stdout).map_err(|_| "unavailable")?;
    match output.get("error").and_then(Value::as_str) {
        Some("conflict") => Err("conflict"),
        Some(_) => Err("unavailable"),
        None if result.status.success() => output.get("value").cloned().ok_or("unavailable"),
        None => Err("unavailable"),
    }
}

pub(crate) fn source(room: &str, task: &str, event: &str) {
    SOURCES
        .lock()
        .unwrap()
        .insert(digest(&json!([room, task])), event.to_owned());
    WAKE.notify_one();
}

pub(crate) async fn commit_input(
    workspace: &std::path::Path,
    room: &str,
    event: &str,
    session: &str,
) -> Result<(), &'static str> {
    if !enabled() || !event.starts_with('$') {
        return Ok(());
    }
    let key = digest(&json!([room, event]));
    let file = workspace.join(".agentteams/matrix-sessions").join(format!(
        "{:x}.json",
        Sha256::digest(format!("{room}\0{event}"))
    ));
    let data = std::fs::read(file).map_err(|_| "native input missing")?;
    let row: Value = serde_json::from_slice(&data).map_err(|_| "invalid native input")?;
    if row["session"] != session
        || !row["receipts"].as_array().is_some_and(|items| {
            items
                .iter()
                .any(|r| r["room_id"] == room && r["event_id"] == event)
        })
    {
        return Err("native source identity changed");
    }
    let view = rpc(json!({"action":"matrix.native.get","event_key":key})).await?;
    rpc(json!({"action":"matrix.native.commit","event_key":key,"snapshot":view,"native_ref":session,"native_digest":format!("{:x}",Sha256::digest(&data))})).await?;
    Ok(())
}

pub(crate) fn matrix_inbox_scanned() {
    MATRIX_INBOX_READY.notify_one();
}

pub(crate) async fn wait_ready() {
    while enabled() && !READY.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn select_session(
    path: &PathBuf,
    checkpoint: Option<&str>,
    scope: &Value,
) -> (String, Vec<ChatMessage>) {
    let Some(checkpoint) = checkpoint else {
        return (String::new(), Vec::new());
    };
    let saved: Option<Value> = std::fs::read(path)
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok());
    if let Some(saved) = saved {
        if saved["format_version"] == 1
            && saved["runtime"] == "openhuman"
            && saved["authority"] == authority()
            && saved["scope"] == *scope
            && saved["checkpoint_digest"] == checkpoint
            && saved["expires_at"].as_u64().unwrap_or(0) > now()
            && saved["native_digest"] == digest(&saved["history"])
        {
            if let (Some(session), Ok(history)) = (
                saved["session_id"].as_str(),
                serde_json::from_value::<Vec<ChatMessage>>(saved["history"].clone()),
            ) {
                if !session.is_empty() && !history.is_empty() {
                    return (session.to_owned(), history);
                }
            }
        }
    }
    (uuid::Uuid::new_v4().to_string(), Vec::new())
}

fn delivered_event(
    event: &Value,
    room: &str,
    sender: &str,
    text: &str,
    txn: &str,
    legacy: bool,
) -> Option<String> {
    let content = &event["content"];
    if event["type"] != "m.room.message"
        || event.get("room_id").is_some_and(|id| id != room)
        || event["sender"] != sender
        || content["msgtype"] != "m.text"
        || content["body"] != text
    {
        return None;
    }
    if content["org.agentteams.reply_txn"] == txn
        || (legacy
            && content.get("org.agentteams.reply_txn").is_none()
            && event["unsigned"]["transaction_id"] == txn)
    {
        return event["event_id"]
            .as_str()
            .filter(|id| id.starts_with('$') && id.len() > 1)
            .map(str::to_owned);
    }
    None
}

impl Run {
    async fn new(
        client: &Client,
        room: &str,
        identity: Value,
        work: Value,
        scope: Value,
        checkpoint_digest: &str,
        delivery_only: bool,
    ) -> Result<Arc<Self>, &'static str> {
        let key = digest(&scope);
        let cache_path = home()
            .join(if work["kind"] == "task" {
                "runtime/openhuman-task-sessions"
            } else {
                "runtime/openhuman-message-sessions"
            })
            .join(format!("{key}.json"));
        // Delivery uses only the verified saved bytes, SDK device and durable
        // transaction. A submitted Task no longer admits session-cache reads.
        let (mut session, mut restored) = select_session(
            &cache_path,
            (!delivery_only).then_some(checkpoint_digest),
            &scope,
        );
        if !restored.is_empty() {
            let view = rpc(json!({"action":"session.load","work":work})).await?;
            let native = serde_json::to_value(&restored).map_err(|_| "invalid native history")?;
            if rpc(json!({"action":"session.usable","snapshot":view,"native_ref":session,"native_digest":digest(&native)})).await? != true {
                restored.clear();
                session = uuid::Uuid::new_v4().to_string();
            }
        }
        Ok(Arc::new(Run {
            work,
            scope,
            admission: identity.to_string(),
            identity,
            client: client.clone(),
            room: room.to_owned(),
            session: session.clone(),
            native_history: Mutex::new(restored.clone()),
            restored,
            cache_path,
            cancel: CancellationToken::new(),
            started: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            succeeded: AtomicBool::new(false),
        }))
    }

    fn message(&self) -> bool {
        self.work["kind"] == "message"
    }

    async fn call(
        &self,
        task_action: &str,
        message_action: &str,
        mut fields: Value,
    ) -> Result<Value, &'static str> {
        fields["action"] = json!(if self.message() {
            message_action
        } else {
            task_action
        });
        fields["identity"] = self.identity.clone();
        rpc(fields).await
    }
    pub(crate) async fn current(&self) -> Result<Value, &'static str> {
        if self.cancel.is_cancelled() {
            return Err("conflict");
        }
        self.call("current", "matrix.message.current", json!({}))
            .await
    }

    pub(crate) fn restore(&self) -> Vec<ChatMessage> {
        self.restored.clone()
    }

    pub(crate) async fn guard(self: Arc<Self>) {
        while !self.finished.load(Ordering::Acquire) {
            if self
                .call("current", "matrix.message.owned", json!({}))
                .await
                == Err("conflict")
            {
                self.cancel.cancel();
                return;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    async fn deliver(&self, text: &str, checkpoint: &Value) -> Result<(), &'static str> {
        let room_id = self
            .room
            .parse::<matrix_sdk::ruma::OwnedRoomId>()
            .map_err(|_| "invalid room")?;
        let room = self
            .client
            .get_room(&room_id)
            .filter(|room| room.state() == RoomState::Joined)
            .ok_or("room not joined")?;
        let who = self
            .client
            .whoami()
            .await
            .map_err(|_| "Matrix whoami unavailable")?;
        let device = who.device_id.ok_or("Matrix device unavailable")?;
        if self.client.user_id() != Some(&who.user_id) || self.client.device_id() != Some(&device) {
            return Err("Matrix identity differs from the restored SDK session");
        }
        let plan = self
            .call(
                "plan_reply",
                "matrix.message.plan_reply",
                json!({"text":text,
            "sender":who.user_id.as_str(), "device":device.as_str()}),
            )
            .await?;
        let transaction = plan["reply_txn"]
            .as_str()
            .ok_or("missing durable transaction")?;
        if checkpoint["reply_txn"] != transaction {
            return Err("saved reply changed before native delivery");
        }
        let event_id = match plan["mode"].as_str() {
            Some("send") => room
                .send_raw(
                    "m.room.message",
                    json!({"msgtype":"m.text", "body":text,
                    "org.agentteams.reply_txn":transaction, "org.agentteams.progress_only":true,
                    "m.mentions":{"user_ids":[]}}),
                )
                .with_transaction_id(transaction.into())
                .with_request_config(
                    RequestConfig::default()
                        .disable_retry()
                        .timeout(Duration::from_secs(15)),
                )
                .await
                .map_err(|_| "Matrix delivery failed")?
                .event_id
                .to_string(),
            Some("find") => {
                let mut page = matrix_sdk::room::MessagesOptions::backward();
                let mut seen = HashSet::new();
                loop {
                    let entries = room
                        .messages(page)
                        .await
                        .map_err(|_| "Matrix history unavailable")?;
                    if let Some(id) = entries.chunk.iter().find_map(|entry| {
                        let event: Value = serde_json::from_str(entry.raw().json().get()).ok()?;
                        delivered_event(
                            &event,
                            &self.room,
                            who.user_id.as_str(),
                            text,
                            transaction,
                            plan["legacy"] == true,
                        )
                    }) {
                        break id;
                    }
                    let from = entries.end.ok_or("prior delivery needs verification")?;
                    if !seen.insert(from.clone()) {
                        return Err("Matrix history pagination loop");
                    }
                    page = matrix_sdk::room::MessagesOptions::backward();
                    page.from = Some(from);
                }
            }
            _ => return Err("invalid delivery mode"),
        };
        self.call(
            "delivered",
            "matrix.message.delivered",
            json!({"event_id":event_id}),
        )
        .await?;
        Ok(())
    }

    pub(crate) async fn complete(&self, text: &str) -> Result<(), &'static str> {
        let reply = self
            .call(
                "prepare_reply",
                "matrix.message.save_reply",
                json!({"text":text}),
            )
            .await?;
        let (cp, body) = if self.message() {
            (
                &reply["execution"]["checkpoint"],
                reply["saved_result"].as_str().unwrap_or(text),
            )
        } else {
            (
                &reply["saved"]["execution"]["checkpoint"],
                reply["text"].as_str().ok_or("missing saved reply")?,
            )
        };
        if cp["delivery"] != "delivered" {
            self.deliver(body, cp).await?;
        }
        let history = self.native_history.lock().unwrap().clone();
        if let Err(error) = self.remember(&history).await {
            // The result is already durable/delivered. A cache failure must not
            // regress its authoritative checkpoint to an unknown operation.
            tracing::warn!("Delivered Task native cache was not refreshed: {error}");
        }
        Ok(())
    }

    async fn remember(&self, history: &[ChatMessage]) -> Result<(), &'static str> {
        let work = &self.work;
        let current = rpc(json!({"action":"session.load","work":work})).await?;
        // Preserve the actual native tool transcript. New startup system policy
        // is projected separately, never restored from an old Harness session.
        let history: Vec<ChatMessage> = history
            .iter()
            .filter(|message| message.role != "system")
            .cloned()
            .collect();
        let serialized =
            serde_json::to_value(&history).map_err(|_| "native history serialization failed")?;
        let cache = json!({"format_version":1, "runtime":"openhuman", "authority":authority(), "scope":self.scope,
            "session_id":self.session, "checkpoint_digest":current["checkpoint_digest"],
            "native_digest":digest(&serialized), "history":serialized, "expires_at":now()+86400});
        let directory = self.cache_path.parent().ok_or("missing cache directory")?;
        std::fs::create_dir_all(directory).map_err(|_| "native cache persistence failed")?;
        let temporary = directory.join(uuid::Uuid::new_v4().to_string());
        let result = (|| -> std::io::Result<()> {
            use std::io::Write;
            let mut options = std::fs::OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(cache.to_string().as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&temporary, &self.cache_path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
            return Err("native cache persistence failed");
        }
        rpc(json!({"action":"session.remember","work":work,"snapshot":current,"identity":self.identity,
            "native_ref":self.session,"native_digest":cache["native_digest"],"expires_at":cache["expires_at"]})).await?;
        *self.native_history.lock().unwrap() = history;
        Ok(())
    }

    pub(crate) async fn cancel_message(&self) {
        if self.message() {
            let _ = rpc(json!({"action":"matrix.message.cancel","identity":self.identity})).await;
        }
    }

    pub(crate) async fn close(&self, id: &str, success: bool) {
        if !success && self.started.load(Ordering::Acquire) && !self.message() {
            let _ = rpc(json!({"action":"interrupted", "identity":self.identity})).await;
        } else if !success {
            ADMITTED.lock().unwrap().remove(&self.admission);
        }
        self.finished.store(true, Ordering::Release);
        RUNS.lock().unwrap().remove(id);
    }
}

fn same_input(event: &Value, record: &Value) -> bool {
    event["event_id"] == record["event_id"]
        && event["sender"] == record["input"]["sender"]
        && event["type"] == record["input"]["type"]
        && event["content"] == record["input"]["content"]
        && event
            .get("room_id")
            .is_none_or(|id| id == &record["room_id"])
}

async fn room_context(client: &Client, record: &Value) -> Result<Vec<Value>, &'static str> {
    let room_id = record["room_id"]
        .as_str()
        .ok_or("missing source room")?
        .parse::<matrix_sdk::ruma::OwnedRoomId>()
        .map_err(|_| "invalid source room")?;
    let id = record["event_id"]
        .as_str()
        .ok_or("missing source event")?
        .parse::<matrix_sdk::ruma::OwnedEventId>()
        .map_err(|_| "invalid source event")?;
    let room = client.get_room(&room_id).ok_or("missing SDK room")?;
    let page = room
        .event_with_context(&id, false, 24u32.into(), None)
        .await
        .map_err(|_| "Matrix source context unavailable")?;
    let original: Value = serde_json::from_str(
        page.event
            .as_ref()
            .ok_or("source event unavailable")?
            .raw()
            .json()
            .get(),
    )
    .map_err(|_| "invalid source event")?;
    if !same_input(&original, record) {
        return Err("Matrix source differs from the shared input");
    }
    let mut history = Vec::new();
    for item in page.events_before.iter().rev() {
        let raw: Value =
            serde_json::from_str(item.raw().json().get()).map_err(|_| "invalid history")?;
        if raw["type"] != "m.room.message" {
            continue;
        }
        let Some(prior) = raw["event_id"].as_str() else {
            continue;
        };
        let key = digest(&json!([room_id.as_str(), prior]));
        let accepted = rpc(json!({"action":"matrix.input","event_key":key})).await?;
        if matches!(accepted["intent"].as_str(), Some("context" | "message")) {
            if !same_input(&raw, &accepted) {
                return Err("Matrix history differs from the shared input");
            }
            history.push(
                json!({"event_id":prior,"sender":raw["sender"],"body":raw["content"]["body"]}),
            );
        }
    }
    let reply_to = original
        .pointer("/content/m.relates_to/m.in_reply_to/event_id")
        .and_then(Value::as_str);
    if let Some(parent) = reply_to.filter(|p| !history.iter().any(|e| e["event_id"] == *p)) {
        let key = digest(&json!([room_id.as_str(), parent]));
        let accepted = rpc(json!({"action":"matrix.input","event_key":key})).await?;
        if matches!(accepted["intent"].as_str(), Some("context" | "message")) {
            history.insert(0,json!({"event_id":parent,"sender":accepted["input"]["sender"],"body":accepted["input"]["content"]["body"]}));
        }
    }
    Ok(history)
}

pub(crate) async fn admit_message(
    client: &Client,
    record: &Value,
    key: &str,
    tx: &mpsc::Sender<ChannelMessage>,
    body: &str,
    on_admit: impl FnOnce(),
) -> Result<(), &'static str> {
    let id = record["event_id"].as_str().ok_or("missing source event")?;
    if get(id).is_some() {
        return Ok(());
    }
    let claim = rpc(json!({"action":"matrix.message.claim","event_key":key})).await?;
    if !matches!(claim["recovery"].as_str(), Some("resume" | "deliver")) {
        return Ok(());
    }
    let room = record["room_id"].as_str().ok_or("missing source room")?;
    let run = Run::new(
        client,
        room,
        claim["identity"].clone(),
        json!({"kind":"message","event_key":key}),
        json!([room, id]),
        claim["checkpoint_digest"]
            .as_str()
            .ok_or("missing checkpoint digest")?,
        claim["recovery"] == "deliver",
    )
    .await?;
    if claim["recovery"] == "deliver" {
        return run
            .deliver(
                claim["saved_result"]
                    .as_str()
                    .ok_or("missing saved reply")?,
                &claim["execution"]["checkpoint"],
            )
            .await;
    }
    let mut prompt = claim["resume_text"]
        .as_str()
        .ok_or("missing recovery context")?
        .to_owned();
    if run.restored.is_empty() {
        let previous = room_context(client, record).await?;
        if !previous.is_empty() {
            prompt.push_str("\nEarlier accepted room messages for context:\n");
            prompt.push_str(&json!(previous).to_string());
        }
    }
    prompt.push_str("\nNative Matrix input:\n");
    prompt.push_str(body);
    let input = ChannelMessage {
        id: id.to_owned(),
        sender: record["input"]["sender"]
            .as_str()
            .ok_or("missing sender")?
            .to_owned(),
        reply_target: room.to_owned(),
        content: prompt,
        channel: "matrix".into(),
        timestamp: now(),
        thread_ts: Some(run.session.clone()),
    };
    if !ADMITTED.lock().unwrap().insert(run.admission.clone()) {
        return Ok(());
    }
    on_admit();
    RUNS.lock().unwrap().insert(id.to_owned(), run.clone());
    if tx.send(input).await.is_err() {
        run.close(id, false).await;
        return Err("native dispatcher stopped");
    }
    Ok(())
}

pub(crate) async fn begin_message_tools(calls: Value) -> anyhow::Result<Option<String>> {
    let Ok(run) = TASK_RUN.try_with(Arc::clone) else {
        return Ok(None);
    };
    if !run.message() {
        return Ok(None);
    }
    let op =
        rpc(json!({"action":"matrix.message.begin_tools","identity":run.identity,"calls":calls}))
            .await
            .map_err(anyhow::Error::msg)?;
    Ok(Some(
        op.as_str()
            .ok_or_else(|| anyhow::anyhow!("missing tool operation identity"))?
            .to_owned(),
    ))
}

pub(crate) async fn finish_message_tools(op: Option<String>, results: Value) -> anyhow::Result<()> {
    if let Some(op) = op {
        let run = TASK_RUN.try_with(Arc::clone)?;
        rpc(json!({"action":"matrix.message.finish_tools","identity":run.identity,"operation_id":op,"results":results})).await.map_err(anyhow::Error::msg)?;
    }
    Ok(())
}

async fn scan(
    client: &Client,
    own: &str,
    tx: &mpsc::Sender<ChannelMessage>,
) -> Result<(), &'static str> {
    let snapshot = rpc(json!({"action":"discover"})).await?;
    let mut entries = snapshot["tasks"]
        .as_array()
        .cloned()
        .ok_or("invalid discovery")?;
    entries.sort_by_key(|entry| entry["task"]["status"] == "assigned");
    for entry in entries {
        let task = &entry["task"];
        let key = digest(&json!([entry["team_id"], task["task_id"]]));
        if RUNS
            .lock()
            .unwrap()
            .values()
            .any(|run| digest(&json!([run.identity["team_id"], run.identity["task_id"]])) == key)
        {
            continue;
        }
        let claimed = match rpc(json!({"action":"claim", "snapshot":snapshot, "entry":entry})).await
        {
            Ok(v) => v,
            Err("conflict") => continue,
            Err(e) => return Err(e),
        };
        let task = &claimed["task"];
        let execution = &task["execution"];
        let identity = json!({"task_id":task["task_id"], "team_id":entry["team_id"], "generation":execution["generation"], "instance_id":execution["instance_id"],"process_id":execution["process_id"]});
        let admission = identity.to_string();
        let delivery_only = execution["checkpoint"]["delivery"] == "pending";
        if !delivery_only
            && (task["status"] == "submitted" || ADMITTED.lock().unwrap().contains(&admission))
        {
            continue;
        }
        if execution["state"] == "needs_verification" {
            tracing::warn!("Owned Task is waiting for side-effect verification");
            continue;
        }
        let room = task["room_id"]
            .as_str()
            .ok_or("missing Task room")?
            .to_owned();
        let room_id = room
            .parse::<matrix_sdk::ruma::OwnedRoomId>()
            .map_err(|_| "invalid Task room")?;
        if client
            .get_room(&room_id)
            .filter(|room| room.state() == RoomState::Joined)
            .is_none()
        {
            // Discovery and claim already verified that this Task belongs to us.
            if client.join_room_by_id(&room_id).await.is_err() {
                continue;
            }
        }
        let run = Run::new(
            client,
            &room,
            identity.clone(),
            json!({"kind":"task","task_id":task["task_id"],"team_id":entry["team_id"]}),
            json!([entry["team_id"], task["task_id"]]),
            claimed["checkpoint_digest"]
                .as_str()
                .ok_or("missing checkpoint digest")?,
            delivery_only,
        )
        .await?;
        if delivery_only {
            run.deliver(
                claimed["saved_result"]
                    .as_str()
                    .ok_or("missing saved result")?,
                &execution["checkpoint"],
            )
            .await?;
            continue;
        }
        ADMITTED.lock().unwrap().insert(admission);
        let id = SOURCES
            .lock()
            .unwrap()
            .get(&digest(&json!([room, task["task_id"]])))
            .cloned()
            .unwrap_or_else(|| format!("recovery:{}", uuid::Uuid::new_v4()));
        RUNS.lock().unwrap().insert(id.clone(), run.clone());
        let input = ChannelMessage {
            id: id.clone(),
            sender: own.to_owned(),
            reply_target: room,
            content: claimed["resume_text"]
                .as_str()
                .ok_or("missing recovery context")?
                .to_owned(),
            channel: "matrix".into(),
            timestamp: now(),
            thread_ts: Some(run.session.clone()),
        };
        if tx.send(input).await.is_err() {
            run.close(&id, false).await;
            return Err("native dispatcher stopped");
        }
    }
    READY.store(true, Ordering::Release);
    Ok(())
}

pub(crate) struct Monitor(tokio::task::JoinHandle<()>);
impl Drop for Monitor {
    fn drop(&mut self) {
        self.0.abort();
        for run in RUNS.lock().unwrap().values() {
            run.cancel.cancel();
        }
    }
}

pub(crate) fn start(
    client: Client,
    own: String,
    tx: mpsc::Sender<ChannelMessage>,
) -> Option<Monitor> {
    if !enabled() {
        return None;
    }
    READY.store(false, Ordering::Release);
    Some(Monitor(tokio::spawn(async move {
        while !tx.is_closed() {
            if rpc(json!({"action":"matrix.start"})).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        // Prefer original assignment events already staged by the first SDK
        // scan. Slow Matrix startup must not indefinitely block assigned Tasks.
        let _ = tokio::time::timeout(Duration::from_secs(5), MATRIX_INBOX_READY.notified()).await;
        while !tx.is_closed() {
            if let Err(error) = scan(&client, &own, &tx).await {
                tracing::warn!("Owned Task recovery deferred: {error}");
            }
            tokio::select! { _ = tokio::time::sleep(Duration::from_secs(10)) => {}, _ = WAKE.notified() => {} }
        }
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_requires_the_same_accepted_event_and_room() {
        let source = json!({"type":"m.room.message","content":{"body":"one"},"sender":"@alice:d"});
        let record = json!({"input":source,"event_id":"$one","room_id":"!a:d"});
        let mut event = source;
        event["event_id"] = json!("$one");
        assert!(same_input(&event, &record));
        for (key, changed) in [
            ("room_id", json!("!b:d")),
            ("event_id", json!("$other")),
            ("sender", json!("@mallory:d")),
            ("content", json!({"body":"changed"})),
        ] {
            let mut altered = event.clone();
            altered[key] = changed;
            assert!(!same_input(&altered, &record));
        }
    }

    #[test]
    fn saved_reply_proof_is_bound_to_room_sender_bytes_and_transaction() {
        let mut event = json!({"type":"m.room.message","room_id":"!room:d","sender":"@me:d","event_id":"$ok",
            "content":{"msgtype":"m.text","body":"final","org.agentteams.reply_txn":"txn"}});
        let confirm = |event: &Value, legacy| {
            delivered_event(event, "!room:d", "@me:d", "final", "txn", legacy)
        };
        assert_eq!(confirm(&event, false).as_deref(), Some("$ok"));
        for (path, changed) in [
            ("/room_id", "!other:d"),
            ("/sender", "@else:d"),
            ("/content/body", "different"),
            ("/content/org.agentteams.reply_txn", "other"),
        ] {
            let mut bad = event.clone();
            *bad.pointer_mut(path).unwrap() = json!(changed);
            assert!(confirm(&bad, false).is_none());
        }
        event["content"]
            .as_object_mut()
            .unwrap()
            .remove("org.agentteams.reply_txn");
        event["unsigned"] = json!({"transaction_id":"txn"});
        assert!(confirm(&event, false).is_none());
        assert_eq!(confirm(&event, true).as_deref(), Some("$ok"));
    }

    #[test]
    fn only_current_scoped_unexpired_native_history_is_reused() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.json");
        let scope = json!(["team-a", "task-a"]);
        let history = json!([
            {"role":"user","content":"Continue task-a"},
            {"role":"assistant","content":"step one native tool call"},
            {"role":"tool","content":"step one result: artifact sha256 verified"}
        ]);
        let saved = json!({"format_version":1,"runtime":"openhuman","scope":scope,
            "authority":authority(),"checkpoint_digest":"checkpoint-4","session_id":"native-thread-a",
            "expires_at":now()+3600,"history":history,"native_digest":digest(&history)});
        std::fs::write(&path, saved.to_string()).unwrap();
        let (delivery_session, delivery_history) = select_session(&path, None, &scope);
        assert!(delivery_session.is_empty() && delivery_history.is_empty());
        let (id, restored) = select_session(&path, Some("checkpoint-4"), &scope);
        assert_eq!(id, "native-thread-a");
        assert_eq!(restored.len(), 3);
        assert_eq!(restored[2].role, "tool");
        assert!(restored[2].content.contains("artifact sha256 verified"));
        for (field, replacement) in [
            ("runtime", json!("hermes")),
            ("checkpoint_digest", json!("checkpoint-3")),
            ("scope", json!(["team-b", "task-a"])),
            ("scope", json!(["team-a", "task-b"])),
            ("authority", json!("another-worker")),
            ("expires_at", json!(now() - 1)),
            ("native_digest", json!("outdated-native-transcript")),
            ("format_version", json!(2)),
            ("history", json!([])),
        ] {
            let mut invalid = saved.clone();
            invalid[field] = replacement;
            std::fs::write(&path, invalid.to_string()).unwrap();
            let (new_id, restored) = select_session(&path, Some("checkpoint-4"), &scope);
            assert_ne!(new_id, "native-thread-a", "{field}");
            assert!(restored.is_empty(), "{field}");
        }
        std::fs::remove_file(&path).unwrap();
        assert!(select_session(&path, Some("checkpoint-4"), &scope)
            .1
            .is_empty());
        std::fs::write(&path, "{broken").unwrap();
        assert!(select_session(&path, Some("checkpoint-4"), &scope)
            .1
            .is_empty());
    }
}
