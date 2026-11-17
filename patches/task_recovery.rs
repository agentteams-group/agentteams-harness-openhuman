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

async fn rpc(request: Value) -> Result<Value, &'static str> {
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

pub(crate) async fn wait_ready() {
    while enabled() && !READY.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn select_session(path: &PathBuf, checkpoint: &str, scope: &Value) -> (String, Vec<ChatMessage>) {
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

impl Run {
    pub(crate) async fn current(&self) -> Result<Value, &'static str> {
        if self.cancel.is_cancelled() {
            return Err("conflict");
        }
        rpc(json!({"action":"current", "identity":self.identity})).await
    }

    pub(crate) fn restore(&self) -> Vec<ChatMessage> {
        self.restored.clone()
    }

    pub(crate) async fn guard(self: Arc<Self>) {
        while !self.finished.load(Ordering::Acquire) {
            if rpc(json!({"action":"current", "identity":self.identity})).await == Err("conflict") {
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
        let transaction = checkpoint["reply_txn"]
            .as_str()
            .ok_or("missing durable transaction")?;
        rpc(json!({"action":"current", "identity":self.identity})).await?;
        // One SDK wire attempt. The next discovery rechecks authority and reuses
        // the durable transaction; SDK encryption/device state remains native.
        let response = room
            .send_raw(
                "m.room.message",
                json!({"msgtype":"m.text", "body":text,
            "org.agentteams.progress_only":true, "m.mentions":{"user_ids":[]}}),
            )
            .with_transaction_id(transaction.into())
            .with_request_config(
                RequestConfig::default()
                    .disable_retry()
                    .timeout(Duration::from_secs(20)),
            )
            .await
            .map_err(|_| "Matrix delivery failed")?;
        rpc(json!({"action":"delivered", "identity":self.identity, "event_id":response.event_id.to_string()})).await?;
        Ok(())
    }

    pub(crate) async fn complete(&self, text: &str) -> Result<(), &'static str> {
        let reply =
            rpc(json!({"action":"prepare_reply", "identity":self.identity, "text":text})).await?;
        let cp = &reply["saved"]["execution"]["checkpoint"];
        if cp["delivery"] != "delivered" {
            self.deliver(reply["text"].as_str().ok_or("missing saved reply")?, cp)
                .await?;
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
        let current = self.current().await?;
        // Preserve the actual native tool transcript. New startup system policy
        // is projected separately, never restored from an old Harness session.
        let history: Vec<ChatMessage> = history
            .iter()
            .filter(|message| message.role != "system")
            .cloned()
            .collect();
        let serialized =
            serde_json::to_value(&history).map_err(|_| "native history serialization failed")?;
        let cache = json!({"format_version":1, "runtime":"openhuman", "authority":authority(), "scope":json!([self.identity["team_id"], self.identity["task_id"]]),
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
        *self.native_history.lock().unwrap() = history;
        Ok(())
    }

    pub(crate) async fn close(&self, id: &str, success: bool) {
        if !success && self.started.load(Ordering::Acquire) {
            let _ = rpc(json!({"action":"interrupted", "identity":self.identity})).await;
        } else if !success {
            ADMITTED.lock().unwrap().remove(&self.admission);
        }
        self.finished.store(true, Ordering::Release);
        RUNS.lock().unwrap().remove(id);
    }
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
        let identity = json!({"task_id":task["task_id"], "team_id":entry["team_id"], "generation":execution["generation"], "instance_id":execution["instance_id"]});
        let admission = identity.to_string();
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
        let cache_path = home()
            .join("runtime/openhuman-task-sessions")
            .join(format!("{key}.json"));
        let (session, restored) = select_session(
            &cache_path,
            claimed["checkpoint_digest"]
                .as_str()
                .ok_or("missing checkpoint digest")?,
            &json!([entry["team_id"], task["task_id"]]),
        );
        let run = Arc::new(Run {
            identity,
            admission: admission.clone(),
            client: client.clone(),
            room: room.clone(),
            session: session.clone(),
            native_history: Mutex::new(restored.clone()),
            restored,
            cache_path,
            cancel: CancellationToken::new(),
            started: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            succeeded: AtomicBool::new(false),
        });
        if execution["checkpoint"]["delivery"] == "pending" {
            run.deliver(
                claimed["saved_result"]
                    .as_str()
                    .ok_or("missing saved result")?,
                &execution["checkpoint"],
            )
            .await?;
            continue;
        }
        if execution["state"] == "needs_verification" {
            // A fresh process cannot establish the outcome of an interrupted
            // native effect from its old session. Wait for shared verification
            // evidence instead of asking the model to replay that operation.
            tracing::warn!("Owned Task is waiting for side-effect verification");
            continue;
        }
        if task["status"] == "submitted" || !ADMITTED.lock().unwrap().insert(admission) {
            continue;
        }
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
            thread_ts: Some(session),
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
        let (id, restored) = select_session(&path, "checkpoint-4", &scope);
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
            let (new_id, restored) = select_session(&path, "checkpoint-4", &scope);
            assert_ne!(new_id, "native-thread-a", "{field}");
            assert!(restored.is_empty(), "{field}");
        }
        std::fs::remove_file(&path).unwrap();
        assert!(select_session(&path, "checkpoint-4", &scope).1.is_empty());
        std::fs::write(&path, "{broken").unwrap();
        assert!(select_session(&path, "checkpoint-4", &scope).1.is_empty());
    }
}
