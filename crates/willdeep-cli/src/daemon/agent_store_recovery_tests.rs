use super::*;

#[test]
fn restored_child_follows_same_parent_into_new_task_and_rejects_other_parent() {
    let root = std::env::temp_dir().join(format!("agent-store-recovery-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("agents.json");
    let parent = uuid::Uuid::new_v4();
    let first = uuid::Uuid::new_v4();
    let second = uuid::Uuid::new_v4();
    let child = uuid::Uuid::new_v4();
    let event = serde_json::json!({"type":"subagent_started","id":child,"profile":"scout","background":true,"workspace":root}).to_string();
    let store = AgentStore::open(path.clone()).unwrap();
    store
        .ensure_session_root(
            parent,
            first,
            root.clone(),
            None,
            None,
            RuntimeAgentStatus::Running,
        )
        .unwrap();
    store.apply_harness_event(first, &event).unwrap();
    drop(store);
    let store = AgentStore::open(path.clone()).unwrap();
    assert_eq!(
        store.get(child).unwrap().unwrap().status,
        RuntimeAgentStatus::Interrupted
    );
    store
        .ensure_session_root(
            parent,
            second,
            root.clone(),
            None,
            None,
            RuntimeAgentStatus::Running,
        )
        .unwrap();
    store.apply_harness_event(second, &event).unwrap();
    let restored = store.get(child).unwrap().unwrap();
    assert_eq!(restored.parent_id, Some(parent));
    assert_eq!(restored.task_id, second);
    assert_eq!(restored.status, RuntimeAgentStatus::Running);
    let unrelated = uuid::Uuid::new_v4();
    store
        .ensure_root(
            unrelated,
            root.clone(),
            None,
            None,
            RuntimeAgentStatus::Running,
        )
        .unwrap();
    assert!(store.apply_harness_event(unrelated, &event).is_err());
    assert_eq!(store.get(child).unwrap().unwrap(), restored);
    std::fs::remove_dir_all(root).unwrap();
}
