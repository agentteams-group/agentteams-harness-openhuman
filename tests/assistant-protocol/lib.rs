#[path = "../../patches/task_coordination.rs"]
mod coordination;

#[tokio::test]
async fn assistant_uses_the_same_verified_native_teams_boundary() {
    let content: serde_json::Value = serde_json::from_str(&std::env::var("AT_ASSISTANT_TEST_CONTENT").unwrap()).unwrap();
    let result = coordination::prepare(&content, "!assistant:test", "@leader:test", "@leader:test").await;
    if std::env::var("AT_ASSISTANT_EXPECT_REJECT").is_ok() {
        assert!(result.is_none());
    } else {
        let result = result.expect("verified Teams reminder must wake the native agent");
        assert_eq!(result.text, "Canonical assistant instructions from Controller");
        assert!(result.task_id.is_none());
        assert!(result.assistant, "verified assistant input must bypass native message recovery and its reply delivery");
        assert!(coordination::is_control_room("!assistant:test"));
    }
    assert!(coordination::prepare(&content, "!assistant:test", "@stranger:test", "@leader:test").await.is_none());
}
