//! Compile and exercise the real Rust binding with SDK interface stubs.
//! This proves context/bridge IO, not a compiled full SDK or model certification.
#![allow(dead_code)]
#[path = "../../patches/hitl.rs"]
pub(crate) mod hitl;
#[path = "../../patches/session_receipts.rs"]
pub(crate) mod session_receipts;
#[path = "../../patches/task_protocol.rs"]
pub(crate) mod task_protocol_source;
#[allow(unused_imports)]
pub(crate) use openhuman::channels::{matrix_direct, task_progress, task_recovery};
#[path = "../../patches/task_telemetry.rs"]
pub(crate) mod task_telemetry;
mod openhuman {
    pub mod security {
        pub struct SecurityPolicy {
            pub workspace_dir: std::path::PathBuf,
        }
    }
    pub mod inference {
        pub mod provider {
            #[derive(Clone, serde::Serialize)]
            pub struct ChatMessage {
                pub role: String,
                pub content: String,
            }
            impl ChatMessage {
                pub fn user(content: impl Into<String>) -> Self {
                    Self {
                        role: "user".into(),
                        content: content.into(),
                    }
                }
                pub fn system(content: impl Into<String>) -> Self {
                    Self {
                        role: "system".into(),
                        content: content.into(),
                    }
                }
            }
        }
    }
    pub mod tools {
        pub mod traits {
            #[derive(PartialEq, Debug)]
            pub enum PermissionLevel {
                None,
            }
            pub struct ToolResult {
                pub text: String,
                pub is_error: bool,
            }
            impl ToolResult {
                pub fn success(text: impl Into<String>) -> Self {
                    Self {
                        text: text.into(),
                        is_error: false,
                    }
                }
                pub fn error(text: impl Into<String>) -> Self {
                    Self {
                        text: text.into(),
                        is_error: true,
                    }
                }
            }
            #[async_trait::async_trait]
            pub trait Tool {
                fn name(&self) -> &str;
                fn description(&self) -> &str;
                fn parameters_schema(&self) -> serde_json::Value;
                fn permission_level(&self) -> PermissionLevel;
                fn native_operation_registration(&self) -> Option<serde_json::Value> {
                    None
                }
                async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult>;
            }
        }
    }
    pub mod channels {
        pub mod traits {
            pub struct ChannelMessage {
                pub id: String,
                pub reply_target: String,
                pub sender: String,
            }
        }
        pub mod matrix_direct {
            pub struct Direct {
                pub source_event_ids: Vec<String>,
            }
            pub fn get(_: &str) -> Option<Direct> {
                None
            }
        }
        pub mod task_progress {
            pub fn run_id() -> Option<String> { None }
            pub fn phase_completed() {}
        }
        pub mod task_recovery {
            pub fn mark_planned_control() {}
            pub static RETIRED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            pub async fn retire_settled_planning_control() -> anyhow::Result<()> {
                RETIRED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            }
            pub fn handoff_target() -> Option<serde_json::Value> { None }
            pub async fn checkpoint(
                _: &[super::super::inference::provider::ChatMessage],
            ) -> anyhow::Result<()> {
                Ok(())
            }
        }
        pub(crate) use crate::task_protocol_source as task_protocol;
        #[allow(unused_imports)]
        pub(crate) use crate::task_telemetry;
    }
}

#[cfg(test)]
mod tests {
    use super::openhuman::channels::{task_protocol as native, traits::ChannelMessage};
    use super::openhuman::tools::traits::Tool;
    use serde_json::json;

    #[tokio::test]
    async fn task_turns_isolate_sources_io_and_shared_receipts() {
        let root = tempfile::tempdir().unwrap();
        let plugin = root.path().join("plugin");
        std::fs::create_dir_all(plugin.join("mcp/common")).unwrap();
        let source =
            std::env::var("AT_SOURCE").expect("AT_SOURCE must point to the matching Core source");
        let fixture = format!(
            r#"import importlib.util,json,sys
from pathlib import Path
base=Path({source:?})/'plugins/teamharness/mcp/common'
sys.path.insert(0,str(base))
spec=importlib.util.spec_from_file_location('binding_fixture',base/'task_native.py')
module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
request=json.load(sys.stdin)
def call(arguments):
 if arguments['action']=='create':
  if not arguments['_nativeSource'].get('frontierEventId'):
   return {{'ok':True,'outcome':'input_changed','noTaskRegistered':True,'sourceEventId':'$first','inputs':[{{'eventId':'$later','roomId':'!room:test','userId':'@human:test','body':'Own later Human amendment 3f21'}}]}}
  return {{'sourceEventId':'$first','task':{{'id':'main','kind':'main','status':'planned'}},'notification':{{'eventId':'handoff','status':'queued','recipient':'leader'}}}}
 identity=arguments['taskId']
 if arguments['action']=='begin':
  task_root=Path({base:?})/'tasks'/identity
  return {{'task':{{'id':identity,'status':'in_progress','mainTaskId':'main','projectId':'','assignedTo':'one','assignmentId':'dispatch'}},'taskDirectory':str(task_root),'outputDirectory':str(task_root/'workspace'),'scratchDirectory':str(task_root/'workspace/.scratch'),'inputs':[{{'files':[{{'path':'/verified/result.json','preview':'Entire upstream body 7d21'}}]}}]}}
 return {{'task':{{'id':identity,'status':'submitted','submissionId':'candidate'}},'notification':{{'eventId':'notice','status':'queued','recipient':'leader'}}}}
async def notify(event,native,action):
 if native == '$settled-plan': return {{'active':False,'settled':True}}
 if native == '$unverified-plan': return {{'active':False}}
 assert native == '$handoff', 'Execution assignments must go directly to Task begin'
 return {{'active':True,'pending_task_ids':['main']}}
def get(route):
 if route.startswith('coordination/events/'):
  return {{'event_id':'notice','task_id':request['context']['execution'],'kind':'task_submitted','actor':'@worker:test','recipient':'@leader:test','room_id':'!room:test','native_id':'matrix:$actual-send'}}
 return {{'deliveries':[{{'eventId':'delivered-own','status':'completed','roomId':'!room:test','taskId':'completed-main','reportMessageId':'$report','report':'Own delivered result 9e27','artifacts':[]}}]}}
try: print(json.dumps(module.invoke(request,call=call,get_json=get,notify=notify)))
except Exception as error:
 print(json.dumps({{'ok':False,'message':str(error),'effect':getattr(error,'effect','unknown')}}));sys.exit(1)
"#,
            source = source,
            base = root.path().to_string_lossy()
        );
        std::fs::write(plugin.join("mcp/common/task_native.py"), fixture).unwrap();
        std::env::set_var("AGENTTEAMS_PLUGIN_DIR", &plugin);
        std::env::set_var("AGENTTEAMS_WORKER_HOME", root.path());
        std::env::set_var("AGENTTEAMS_AGENT_ROLE", "worker");
        async fn run(index: usize, base: &std::path::Path) {
            let id = format!("exec-{index}");
            let event = format!("$source-{index}");
            native::register(
                &event,
                json!({"agent_matrix_user_id":"@worker:test","teamharness_coordination":{"id":format!("{:032x}",index),"kind":"task_assigned","task_id":id,"main_task_id":"main","project_id":"","delivery_id":"dispatch"}}),
            );
            native::register_recovered(&event, json!({"teamharness_coordination": {
                "kind":"task_assigned", "task_id":id,"delivery_id":"fallback"
            }}));
            let msg = ChannelMessage {
                id: event,
                reply_target: "!room:test".into(),
                sender: "@leader:test".into(),
            };
            native::scope(&msg, "native-session", async {
                tokio::task::yield_now().await;
                assert!(native::trace_fields().iter().any(|field|
                    field.key.as_str() == "agentteams.coordination.id" && field.value.to_string() == format!("{:032x}", index)));
                let expected = base.join("tasks").join(&id).join("workspace");
                assert_eq!(native::output_directory(), Some(expected.clone()));
                assert_eq!(native::tool_timeout_secs("shell", 120, false), 310);
                assert_eq!(native::tool_timeout_secs("shell", 120, true), 120);
                assert_eq!(native::tool_timeout_secs("shell", 60, false), 60);
                assert_eq!(native::tool_timeout_secs("task", 120, false), 120);
                assert_eq!(native::scratch_directory(), Some(expected.join(".scratch")));
                let policy = crate::openhuman::security::SecurityPolicy {
                    workspace_dir: base.join("private"),
                };
                let (directory, name) = native::file_scope(&policy, "result.txt", true).unwrap();
                assert_eq!(directory, expected);
                assert_eq!(name, "result.txt");
                assert!(native::file_scope(&policy, "/another-task/output.txt", true).is_err());
                assert_eq!(policy.workspace_dir, base.join("private"));
                let mut history = vec![];
                native::prepare(&mut history).await.unwrap();
                assert!(history.iter().any(|message| message.content.contains("/verified/result.json")));
                assert!(!history.iter().any(|message| message.content.contains("Entire upstream body 7d21")));
                let rejected = native::TaskTool
                    .execute(
                        json!({"action":"submit","taskId":"foreign","summary":"Rejected fixture"}),
                    )
                    .await
                    .unwrap();
                assert!(rejected.is_error);
                assert!(rejected.text.contains("none"));
                let result = native::TaskTool
                    .execute(json!({"action":"submit","taskId":id,"summary":"Finished"}))
                    .await
                    .unwrap();
                assert!(!result.is_error);
                assert!(result.text.contains("Task system receipt"));
                assert!(result.text.contains("$actual-send"));
                assert_eq!(native::completed(), Some(String::new()));
            })
            .await
            .unwrap();
        }
        tokio::join!(run(1, root.path()), run(2, root.path()));
        std::env::set_var("AGENTTEAMS_AGENT_ROLE", "team_leader");
        native::register(
            "$handoff",
            json!({"teamharness_coordination":{"id":"00000000000000000000000000000003","kind":"handoff"}}),
        );
        let handoff = ChannelMessage {
            id: "$handoff".into(),
            reply_target: "!room:test".into(),
            sender: "@manager:test".into(),
        };
        native::scope(&handoff, "leader-session", async {
            assert!(!native::allowed_tool("shell"));
        })
        .await
        .unwrap();

        for id in ["$settled-plan", "$unverified-plan"] {
            native::register(id, json!({"teamharness_coordination":{
                "id":"00000000000000000000000000000003","kind":"handoff"}}));
            let replay = ChannelMessage {
                id: id.into(), reply_target: "!room:test".into(), sender: "@manager:test".into(),
            };
            assert!(native::scope(&replay, "leader-session", async {
                panic!("inactive planning control started a model turn")
            }).await.is_err());
        }
        assert_eq!(super::task_recovery::RETIRED.load(std::sync::atomic::Ordering::SeqCst), 1);

        std::env::set_var("AGENTTEAMS_AGENT_ROLE", "manager");
        let msg = ChannelMessage {
            id: "$first".into(),
            reply_target: "!room:test".into(),
            sender: "@human:test".into(),
        };
        native::scope(&msg,"manager-native-session",async {
            let mut history=vec![];
            native::prepare(&mut history).await.unwrap();
            assert!(history.iter().any(|message|message.content.contains("Own delivered result 9e27")));
            let refs=history.iter().find_map(|message|message.content.strip_prefix("[Task input references]\n")).unwrap();
            let input_id=serde_json::from_str::<serde_json::Value>(refs).unwrap()[0]["inputId"].clone();
            let update=native::TaskTool.execute(json!({"action":"create","leader":"leader","title":"Own original request"})).await.unwrap();
            assert!(!update.is_error);
            let blocked=native::TaskTool.execute(json!({"action":"create","leader":"leader","title":"Before native checkpoint"})).await.unwrap();
            assert!(blocked.is_error);
            native::attach_pending(&mut history).await.unwrap();
            assert!(history.iter().any(|message|message.role=="user" && message.content.contains("Own later Human amendment 3f21")));
            let done=native::TaskTool.execute(json!({"action":"create","leader":"leader","title":"Updated request","inputId":input_id})).await.unwrap();
            assert!(!done.is_error);assert!(done.text.contains("Task system receipt"));
            assert!(native::completed().is_none());
        }).await.unwrap();
        let saved: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.path().join(".agentteams/task-delivery-context.json")).unwrap(),
        )
        .unwrap();
        assert!(saved.is_object());
        assert!(native::output_directory().is_none());
        assert_eq!(native::tool_timeout_secs("shell", 120, false), 120);
        assert!(native::completed().is_none());
        assert_eq!(
            native::TaskTool.permission_level(),
            crate::openhuman::tools::traits::PermissionLevel::None
        );
    }
}
