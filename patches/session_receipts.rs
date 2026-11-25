//! Save the accepted native Matrix input before model execution.
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::Path;

/// Locate original business inputs when an opaque Task session cannot be reused.
/// Callers must re-read each source from the authenticated shared input ledger.
pub(crate) fn task_business_sources(
    path: &Path,
    authority: &str,
    scope: &serde_json::Value,
    room: &str,
) -> Vec<String> {
    let saved: Option<serde_json::Value> = std::fs::read(path)
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok());
    let Some(saved) = saved else {
        return Vec::new();
    };
    if saved["format_version"] != 1
        || saved["runtime"] != "openhuman"
        || saved["authority"] != authority
        || saved["scope"] != *scope
    {
        return Vec::new();
    }
    let mut sources = Vec::new();
    for frame in saved["history"].as_array().into_iter().flatten() {
        if frame["role"] != "user" {
            continue;
        }
        let Some(content) = frame["content"].as_str() else {
            continue;
        };
        let Ok(source) = serde_json::from_str::<serde_json::Value>(content) else {
            continue;
        };
        if source["room_id"] != room {
            continue;
        }
        if let Some(id) = source["event_id"].as_str().filter(|id| id.starts_with('$')) {
            if !sources.iter().any(|seen| seen == id) {
                sources.push(id.to_owned());
            }
        }
    }
    sources
}

pub(crate) fn checkpoint(
    workspace: &Path,
    room_id: &str,
    event_id: &str,
    session: &str,
    input: &impl Serialize,
    source_event_ids: &[String],
) -> std::io::Result<()> {
    let directory = workspace.join(".agentteams/matrix-sessions");
    std::fs::create_dir_all(&directory)?;
    let key = format!("{:x}", Sha256::digest(format!("{room_id}\0{event_id}")));
    let path = directory.join(format!("{key}.json"));
    let temporary = directory.join(format!("{key}.tmp"));
    let snapshot = serde_json::json!({
        "session": session,
        "input": input,
        "receipts": source_event_ids.iter().map(|id| serde_json::json!({
            "room_id": room_id, "event_id": id,
        })).collect::<Vec<_>>(),
    });
    std::fs::write(&temporary, serde_json::to_vec(&snapshot)?)?;
    std::fs::File::open(&temporary)?.sync_all()?;
    std::fs::rename(temporary, path)?;
    std::fs::File::open(directory)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incompatible_task_cache_retains_only_ordered_source_identities() {
        use serde_json::json;
        let root = std::env::temp_dir().join(format!("task-source-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("task.json");
        let scope = json!(["team", "T"]);
        let source = |id: &str, room: &str| {
            json!({"role":"user", "content":
            json!({"room_id":room,"event_id":id,"sender":"@human:test","content":{"body":"Use the browser"}}).to_string()})
        };
        let history = json!([
            {"role":"system","content":"old policy"},
            source("$first", "!task:test"),
            {"role":"tool","content":"old tool result"},
            source("$second", "!task:test"),
            source("$first", "!task:test"),
            source("$elsewhere", "!other:test"),
            {"role":"user","content":"rendered old recovery snapshot"}
        ]);
        let saved = json!({"format_version":1,"runtime":"openhuman","authority":"worker",
            "scope":scope,"checkpoint_digest":"obsolete","expires_at":0,"history":history});
        std::fs::write(&path, saved.to_string()).unwrap();
        assert_eq!(
            task_business_sources(&path, "worker", &scope, "!task:test"),
            vec!["$first", "$second"]
        );
        for (key, value) in [
            ("runtime", json!("hermes")),
            ("authority", json!("other")),
            ("scope", json!(["team", "other-task"])),
            ("format_version", json!(2)),
        ] {
            let mut invalid = saved.clone();
            invalid[key] = value;
            std::fs::write(&path, invalid.to_string()).unwrap();
            assert!(
                task_business_sources(&path, "worker", &scope, "!task:test").is_empty(),
                "{key}"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn saves_native_input_and_exact_source_without_waiting_for_a_reply() {
        let root = std::env::temp_dir().join(format!("receipt-test-{}", std::process::id()));
        let input = serde_json::json!({"role": "user", "content": "hello"});
        checkpoint(
            &root,
            "!room:hs",
            "$supplement",
            "native-session",
            &input,
            &["$input".into(), "$supplement".into()],
        )
        .unwrap();
        let path = std::fs::read_dir(root.join(".agentteams/matrix-sessions"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(saved["input"], input);
        assert_eq!(saved["session"], "native-session");
        assert_eq!(saved["receipts"][0]["event_id"], "$input");
        assert_eq!(saved["receipts"][1]["event_id"], "$supplement");
        std::fs::remove_dir_all(root).unwrap();
    }
}
