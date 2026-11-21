//! Commit the SDK-decrypted Worker batch and its same-device cursor before dispatch.
use super::*;
use crate::openhuman::channels::task_recovery::{admit_message, rpc, wait_ready};
use matrix_sdk::config::SyncToken;
use matrix_sdk::room::MessagesOptions;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::future::Future;
use std::time::Duration;

pub(super) fn event_key(room: &str, event: &str) -> String {
    format!("{:x}", Sha256::digest(json!([room, event]).to_string()))
}

pub(super) async fn retire_control(key: &str) {
    if let Ok(entry) = rpc(json!({"action":"matrix.message.claim","event_key":key})).await {
        if entry["recovery"] == "resume" {
            let _ =
                rpc(json!({"action":"matrix.message.cancel","identity":entry["identity"]})).await;
        }
    }
}

pub(super) async fn admit(
    client: &MatrixSdkClient,
    tx: &mpsc::Sender<ChannelMessage>,
    key: &str,
    room: &Room,
    sender: &str,
    body: &str,
    direct: bool,
    inputs: Vec<String>,
    source_events: Vec<String>,
) -> Result<(), &'static str> {
    let record = rpc(json!({"action":"matrix.input","event_key":key})).await?;
    if record["input"]["sender"] != sender || record["room_id"] != room.room_id().as_str() {
        return Err("source changed");
    }
    admit_message(client, &record, key, tx, body, || {
        if direct {
            crate::openhuman::channels::matrix_direct::register(
                room.clone(),
                record["event_id"].as_str().unwrap_or_default().to_owned(),
                sender.to_owned(),
                inputs,
                source_events,
            );
            if let Some(run) = crate::openhuman::channels::matrix_direct::get(
                record["event_id"].as_str().unwrap_or_default(),
            ) {
                run.set_transport(tx.clone());
            }
        }
    })
    .await
}

fn classify(
    channel: &MatrixChannel,
    target: &OwnedRoomId,
    account: &OwnedUserId,
    room: &OwnedRoomId,
    raw: &Value,
) -> Result<Option<Value>, &'static str> {
    if raw["type"] == "m.room.encrypted" {
        return Err("Matrix event has not been decrypted");
    }
    if raw["type"] != "m.room.message" {
        return Ok(None);
    }
    let event = raw["event_id"].as_str().ok_or("missing Matrix event ID")?;
    let sender = raw["sender"].as_str().ok_or("missing Matrix sender")?;
    let c = &raw["content"];
    let text = matches!(c["msgtype"].as_str(), Some("m.text" | "m.notice"))
        && c["body"]
            .as_str()
            .is_some_and(MatrixChannel::has_non_empty_body);
    let allowed = sender != account.as_str()
        && text
        && c["org.agentteams.progress_only"] != true
        && MatrixChannel::agentteams_sender_allowed(&channel.allowed_users, sender, room == target);
    let mentioned = c["m.mentions"]["user_ids"]
        .as_array()
        .is_some_and(|ids| ids.iter().any(|id| id == account.as_str()));
    let intent = if allowed && (room == target || mentioned) {
        Some("message")
    } else {
        None
    };
    Ok(Some(
        json!({"event_id":event,"intent":intent,"input":{"sender":sender,"type":raw["type"],"origin_server_ts":raw["origin_server_ts"],"content":c}}),
    ))
}

async fn inputs<'a>(
    channel: &MatrixChannel,
    target: &OwnedRoomId,
    account: &OwnedUserId,
    room: &OwnedRoomId,
    raw_events: impl Iterator<Item = &'a str>,
    until: Option<&str>,
) -> Result<Value, &'static str> {
    let mut events = Vec::new();
    for raw in raw_events {
        let event: Value = serde_json::from_str(raw).map_err(|_| "invalid native event")?;
        if let Some(item) = classify(channel, target, account, room, &event)? {
            events.push(item);
        }
        if until.is_some() && event["event_id"].as_str() == until {
            break;
        }
    }
    if events.is_empty() {
        return Ok(json!([]));
    }
    rpc(json!({"action":"matrix.prepare","room_id":room.as_str(),"account":account.as_str(),"events":events})).await
}

async fn advance_gap(
    channel: &MatrixChannel,
    client: &MatrixSdkClient,
    target: &OwnedRoomId,
    account: &OwnedUserId,
    room: &OwnedRoomId,
    gap_id: &str,
) -> Result<(), &'static str> {
    let snapshot = rpc(json!({"action":"matrix.load"})).await?;
    let gap = &snapshot["state"]["rooms"][room.as_str()]["backfill"][gap_id];
    if gap.is_null() || gap["boundary"].as_bool() == Some(true) {
        return Ok(());
    }
    let from = gap["from_token"].as_str().ok_or("missing history bound")?;
    let mut options = MessagesOptions::backward();
    options.from = Some(from.to_owned());
    let native = client.get_room(room).ok_or("joined room unavailable")?;
    let page = native
        .messages(options)
        .await
        .map_err(|_| "native Matrix history unavailable")?;
    if page.start != from
        || page
            .end
            .as_deref()
            .is_some_and(|end| end == from || end == gap_id)
    {
        return Err("invalid history pagination");
    }
    let batch = inputs(
        channel,
        target,
        account,
        room,
        page.chunk.iter().map(|e| e.raw().json().get()),
        gap["until"].as_str(),
    )
    .await?;
    rpc(json!({"action":"matrix.backfill","snapshot":snapshot,"page":{"room_id":room.as_str(),"gap_id":gap_id,"from_token":from,
             "next_token":page.end.unwrap_or_default(),"inputs":batch}})).await?;
    Ok(())
}

async fn recover<F, Fut>(
    channel: &MatrixChannel,
    client: &MatrixSdkClient,
    target: &OwnedRoomId,
    account: &OwnedUserId,
    handle: &F,
) -> Result<(), &'static str>
where
    F: Fn(OriginalSyncRoomMessageEvent, Room, Value) -> Fut,
    Fut: Future<Output = ()>,
{
    let snapshot = rpc(json!({"action":"matrix.load"})).await?;
    for (id, room) in snapshot["state"]["rooms"]
        .as_object()
        .ok_or("missing rooms")?
    {
        let room_id: OwnedRoomId = id.as_str().try_into().map_err(|_| "invalid room ID")?;
        if let Some(gaps) = room["gaps"].as_array() {
            for gap in gaps.iter().filter_map(Value::as_str) {
                if let Err(error) =
                    advance_gap(channel, client, target, account, &room_id, gap).await
                {
                    tracing::warn!("Matrix history remains pending in {id}: {error}");
                }
            }
        }
    }
    let snapshot = rpc(json!({"action":"matrix.load"})).await?;
    // Each accepted input has its own storage key. Keep RPC response sizes
    // bounded when a Worker has accumulated many events while stopped.
    let mut records = Vec::new();
    for key in snapshot["state"]["pending"]
        .as_array()
        .ok_or("missing pending inputs")?
    {
        records.push(rpc(json!({"action":"matrix.input","event_key":key})).await?);
    }
    for intent in ["task_notification", "message"] {
        for record in records.iter().filter(|r| r["intent"] == intent) {
            let room = record["room_id"].as_str().ok_or("missing room ID")?;
            if snapshot["state"]["rooms"][room]["gaps"]
                .as_array()
                .is_some_and(|g| !g.is_empty())
            {
                continue;
            }
            let room_id: OwnedRoomId = room.try_into().map_err(|_| "invalid room ID")?;
            let Some(native) = client
                .get_room(&room_id)
                .filter(|r| r.state() == RoomState::Joined)
            else {
                continue;
            };
            let mut raw = record["input"].clone();
            raw["event_id"] = record["event_id"].clone();
            let Ok(typed) = serde_json::from_value::<OriginalSyncRoomMessageEvent>(raw.clone())
            else {
                continue;
            };
            handle(typed, native, raw).await;
        }
        if intent == "task_notification" {
            crate::openhuman::channels::task_recovery::matrix_inbox_scanned();
        }
    }
    wait_ready().await;
    rpc(json!({"action":"recovery.scanned"})).await?;
    Ok(())
}

pub(super) async fn listen<F, Fut>(
    channel: &MatrixChannel,
    client: &MatrixSdkClient,
    target: &OwnedRoomId,
    account: &OwnedUserId,
    tx: &mpsc::Sender<ChannelMessage>,
    handle: F,
) -> anyhow::Result<()>
where
    F: Fn(OriginalSyncRoomMessageEvent, Room, Value) -> Fut + Sync,
    Fut: Future<Output = ()> + Send,
{
    let who = client.whoami().await?;
    anyhow::ensure!(&who.user_id == account, "Worker Matrix identity changed");
    let device = who
        .device_id
        .ok_or_else(|| anyhow::anyhow!("Worker Matrix device unavailable"))?;
    while !tx.is_closed() {
        let snapshot = match rpc(json!({"action":"matrix.start"})).await {
            Ok(v) => v,
            Err(_) => {
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        let consumer=rpc(json!({"action":"matrix.consumer","snapshot":snapshot,"account":account.as_str(),"device":device.as_str(),"format":"matrix-rust-sdk-sync-v3"})).await.map_err(anyhow::Error::msg)?;
        let from = consumer["token"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing Matrix cursor"))?;
        let token = if from.is_empty() {
            SyncToken::NoToken
        } else {
            SyncToken::Specific(from.to_owned())
        };
        let settings = SyncSettings::new()
            .timeout(Duration::from_secs(3))
            .token(token);
        let response = match client.sync_once(settings).await {
            Ok(v) => v,
            Err(error) => {
                tracing::warn!("Matrix sync deferred: {error}");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        // The SDK may already have consumed this response in its local store.
        // Keep the decoded batch across failed Controller CAS attempts.
        loop {
            let commit=async {
                let fresh=rpc(json!({"action":"matrix.load"})).await?;
                let mut timelines=Vec::new();
                for (room,update) in &response.rooms.joined {
                    let timeline=&update.timeline;
                    let batch=inputs(channel,target,account,room,timeline.events.iter().map(|e|e.raw().json().get()),None).await?;
                    timelines.push(json!({"room_id":room.as_str(),"inputs":batch,"limited":timeline.limited,"prev_batch":timeline.prev_batch}));
                }
                rpc(json!({"action":"matrix.sync","snapshot":fresh,"consumer":consumer,"next_batch":response.next_batch,"timelines":timelines})).await
            }.await;
            match commit {
                Ok(_) => break,
                Err(error) => {
                    let fresh = rpc(json!({"action":"matrix.load"}))
                        .await
                        .map_err(anyhow::Error::msg)?;
                    let current=rpc(json!({"action":"matrix.consumer","snapshot":fresh,"account":account.as_str(),"device":device.as_str(),"format":"matrix-rust-sdk-sync-v3"})).await.map_err(anyhow::Error::msg)?;
                    if current["token"] == response.next_batch {
                        break;
                    }
                    anyhow::ensure!(
                        current["token"] == consumer["token"],
                        "Matrix cursor changed during native commit"
                    );
                    tracing::warn!("Matrix batch remains pending: {error}");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
        if let Err(error) = recover(channel, client, target, account, &handle).await {
            tracing::warn!("Worker Matrix recovery deferred: {error}");
        }
    }
    Ok(())
}
