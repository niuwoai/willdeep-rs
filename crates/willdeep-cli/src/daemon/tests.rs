use super::*;

fn test_agent_store(root: &Path) -> Arc<AgentStore> {
    Arc::new(AgentStore::open(root.join("agents.json")).unwrap())
}

#[test]
fn state_round_trips_without_exposing_token_in_logs() {
    let root = std::env::temp_dir().join(format!("willdeep-daemon-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("daemon.json");
    let state = DaemonState {
        schema: STATE_SCHEMA,
        version: "1.2.3".to_owned(),
        pid: 42,
        address: "127.0.0.1:9847".parse().unwrap(),
        token: "private-token".to_owned(),
        started_at: 10,
    };
    write_state(&path, &state).unwrap();
    assert_eq!(load_state(&path).unwrap(), state);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn authorization_requires_exact_local_token() {
    let root = std::env::temp_dir().join(format!("willdeep-daemon-auth-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let events = Arc::new(EventLog::open(root.join("events.ndjson")).unwrap());
    let agents = test_agent_store(&root);
    let state = ServerState {
        token: "expected".to_owned(),
        started_at: 0,
        shutdown: Arc::new(Notify::new()),
        events: events.clone(),
        tasks: Arc::new(
            TaskManager::open(
                root.join("tasks.json"),
                root.join("interactions.json"),
                PathBuf::from("willdeep"),
                events,
                agents.clone(),
                "http://127.0.0.1:1".to_owned(),
                "test-token".to_owned(),
            )
            .unwrap(),
        ),
        agents,
    };
    assert_eq!(
        authorize(&state, &HeaderMap::new()),
        Err(StatusCode::UNAUTHORIZED)
    );
    let mut headers = HeaderMap::new();
    headers.insert(TOKEN_HEADER, HeaderValue::from_static("expected"));
    assert_eq!(authorize(&state, &headers), Ok(()));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn event_log_assigns_sequences_and_resumes_after_cursor() {
    let root = std::env::temp_dir().join(format!("willdeep-events-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("events.ndjson");
    let log = EventLog::open(path.clone()).unwrap();
    assert_eq!(log.append("first", "one").unwrap().sequence, 1);
    assert_eq!(log.append("second", "two").unwrap().sequence, 2);
    assert_eq!(log.read_after(1, 10).unwrap()[0].kind, "second");
    drop(log);

    let reopened = EventLog::open(path).unwrap();
    assert_eq!(reopened.append("third", "three").unwrap().sequence, 3);
    assert_eq!(reopened.read_after(0, 2).unwrap().len(), 2);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_persists_structured_harness_lifecycle() {
    let root = std::env::temp_dir().join(format!("willdeep-agent-store-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("agents.json");
    let task_id = uuid::Uuid::new_v4();
    let store = AgentStore::open(path.clone()).unwrap();
    let agent = store
        .ensure_root(
            task_id,
            root.clone(),
            Some("editor".to_owned()),
            RuntimeAgentStatus::Queued,
        )
        .unwrap();
    store
        .apply_harness_event(task_id, r#"{"type":"turn_started","turn":2}"#)
        .unwrap();
    store
        .apply_harness_event(task_id, r#"{"type":"tool_requested","name":"read_file"}"#)
        .unwrap();
    store
        .apply_harness_event(
            task_id,
            r#"{"type":"usage","input_tokens":10,"output_tokens":4,"total_tokens":14}"#,
        )
        .unwrap();
    let child_id = uuid::Uuid::new_v4();
    store
        .apply_harness_event(
            task_id,
            &serde_json::json!({
                "type": "subagent_started",
                "id": child_id,
                "profile": "scout",
                "label": "inspect files",
                "background": true
            })
            .to_string(),
        )
        .unwrap();
    store
        .apply_harness_event(
            task_id,
            &serde_json::json!({
                "type": "subagent_turn_started",
                "id": child_id,
                "turn": 2
            })
            .to_string(),
        )
        .unwrap();
    store
        .apply_harness_event(
            task_id,
            &serde_json::json!({
                "type": "subagent_usage",
                "id": child_id,
                "input_tokens": 7,
                "output_tokens": 2,
                "total_tokens": 9
            })
            .to_string(),
        )
        .unwrap();
    store
        .apply_harness_event(
            task_id,
            &serde_json::json!({
                "type": "subagent_completed",
                "id": child_id,
                "status": "completed"
            })
            .to_string(),
        )
        .unwrap();
    store
        .set_status_for_task(task_id, RuntimeAgentStatus::Completed, None)
        .unwrap();
    drop(store);

    let restored = AgentStore::open(path)
        .unwrap()
        .get(agent.id)
        .unwrap()
        .unwrap();
    assert_eq!(restored.status, RuntimeAgentStatus::Completed);
    assert_eq!(restored.current_turn, 2);
    assert_eq!(restored.current_tool, None);
    assert_eq!(restored.total_tokens, Some(14));
    assert!(restored.completed_at.is_some());
    let child = AgentStore::open(root.join("agents.json"))
        .unwrap()
        .get(child_id)
        .unwrap()
        .unwrap();
    assert_eq!(child.parent_id, Some(agent.id));
    assert_eq!(child.profile.as_deref(), Some("scout"));
    assert!(child.background);
    assert_eq!(child.status, RuntimeAgentStatus::Completed);
    assert_eq!(child.current_turn, 2);
    assert_eq!(child.total_tokens, Some(9));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn task_store_marks_active_tasks_interrupted_after_restart() {
    let root = std::env::temp_dir().join(format!("willdeep-tasks-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("tasks.json");
    let id = uuid::Uuid::new_v4();
    let task = RuntimeTask {
        id,
        agent_id: None,
        event_start_sequence: 0,
        status: RuntimeTaskStatus::Running,
        workspace: root.clone(),
        profile: None,
        pid: Some(10),
        created_at: 1,
        started_at: Some(2),
        completed_at: None,
        exit_code: None,
        error: None,
    };
    persist_tasks(&path, &HashMap::from([(id, task)])).unwrap();
    let events = Arc::new(EventLog::open(root.join("events.ndjson")).unwrap());
    let agents = test_agent_store(&root);
    let manager = TaskManager::open(
        path,
        root.join("interactions.json"),
        PathBuf::from("willdeep"),
        events,
        agents,
        "http://127.0.0.1:1".to_owned(),
        "test-token".to_owned(),
    )
    .unwrap();
    let recovered = manager.tasks.blocking_read();
    assert_eq!(recovered[&id].status, RuntimeTaskStatus::Interrupted);
    assert!(recovered[&id].completed_at.is_some());
    drop(recovered);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn daemon_lock_is_exclusive_and_owned_cleanup_is_safe() {
    let root = std::env::temp_dir().join(format!("willdeep-lock-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("daemon.lock");
    let first = acquire_daemon_lock(&path).unwrap();
    assert!(acquire_daemon_lock(&path).is_err());
    remove_owned_lock(&path, "not-the-owner");
    assert!(path.exists());
    remove_owned_lock(&path, &first.token);
    assert!(acquire_daemon_lock(&path).is_ok());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn daemon_lock_recovers_after_stale_lease() {
    let root = std::env::temp_dir().join(format!("willdeep-stale-lock-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("daemon.lock");
    write_json_atomic(
        &path,
        &DaemonLock {
            token: "stale".to_owned(),
            created_at: 0,
        },
    )
    .unwrap();
    assert_ne!(acquire_daemon_lock(&path).unwrap().token, "stale");
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn concurrent_task_updates_persist_a_complete_snapshot() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-concurrent-tasks-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("tasks.json");
    let events = Arc::new(EventLog::open(root.join("events.ndjson")).unwrap());
    let agents = test_agent_store(&root);
    let manager = Arc::new(
        TaskManager::open(
            path.clone(),
            root.join("interactions.json"),
            PathBuf::from("willdeep"),
            events,
            agents,
            "http://127.0.0.1:1".to_owned(),
            "test-token".to_owned(),
        )
        .unwrap(),
    );
    let mut updates = Vec::new();
    for index in 0..20 {
        let manager = manager.clone();
        let workspace = root.clone();
        updates.push(tokio::spawn(async move {
            let id = uuid::Uuid::new_v4();
            manager
                .insert_and_persist(RuntimeTask {
                    id,
                    agent_id: None,
                    event_start_sequence: 0,
                    status: RuntimeTaskStatus::Completed,
                    workspace,
                    profile: None,
                    pid: None,
                    created_at: index,
                    started_at: Some(index),
                    completed_at: Some(index),
                    exit_code: Some(0),
                    error: None,
                })
                .await
                .unwrap();
        }));
    }
    for update in updates {
        update.await.unwrap();
    }
    assert_eq!(load_tasks(&path).unwrap().len(), 20);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn pending_approval_blocks_until_a_valid_resolution_arrives() {
    let root = std::env::temp_dir().join(format!("willdeep-interaction-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let events = Arc::new(EventLog::open(root.join("events.ndjson")).unwrap());
    let agents = test_agent_store(&root);
    let manager = TaskManager::open(
        root.join("tasks.json"),
        root.join("interactions.json"),
        PathBuf::from("willdeep"),
        events,
        agents,
        "http://127.0.0.1:1".to_owned(),
        "test-token".to_owned(),
    )
    .unwrap();
    let task_id = uuid::Uuid::new_v4();
    manager
        .insert_and_persist(RuntimeTask {
            id: task_id,
            agent_id: None,
            event_start_sequence: 0,
            status: RuntimeTaskStatus::Running,
            workspace: root.clone(),
            profile: None,
            pid: Some(42),
            created_at: 1,
            started_at: Some(1),
            completed_at: None,
            exit_code: None,
            error: None,
        })
        .await
        .unwrap();
    let receiver = manager
        .create_interaction(
            task_id,
            InteractionKind::Approval {
                description: "run tests".to_owned(),
                always_allow_available: true,
            },
        )
        .await
        .unwrap();
    let interaction = manager.pending_interactions().await.remove(0);
    assert_eq!(
        manager.get(task_id).await.unwrap().status,
        RuntimeTaskStatus::WaitingApproval
    );
    assert!(
        manager
            .resolve_interaction(
                interaction.id,
                InteractionResolution::Answer(Some("wrong kind".to_owned()))
            )
            .await
            .is_err()
    );
    manager
        .resolve_interaction(interaction.id, InteractionResolution::AlwaysAllow)
        .await
        .unwrap();
    assert_eq!(receiver.await.unwrap(), InteractionResolution::AlwaysAllow);
    assert_eq!(
        manager.get(task_id).await.unwrap().status,
        RuntimeTaskStatus::Running
    );
    assert!(manager.pending_interactions().await.is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn runtime_connection_is_encoded_in_private_stdin_payload() {
    let task_id = uuid::Uuid::new_v4();
    let data = runtime_harness_input(
        "safe prompt".to_owned(),
        vec![willdeep_core::MessageAttachment::Text {
            name: "notes.txt".to_owned(),
            content: "attached".to_owned(),
        }],
        RuntimeConnection {
            url: "http://127.0.0.1:1234".to_owned(),
            token: "private-control-token".to_owned(),
            task_id,
        },
    )
    .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&data).unwrap();
    assert_eq!(value["prompt"], "safe prompt");
    assert_eq!(value["runtime"]["token"], "private-control-token");
    assert_eq!(value["runtime"]["task_id"], task_id.to_string());
    assert_eq!(value["attachments"][0]["content"], "attached");
}
