use super::*;

fn test_agent_store(root: &Path) -> Arc<AgentStore> {
    Arc::new(AgentStore::open(root.join("agents.json")).unwrap())
}

fn test_runtime_session_store(root: &Path) -> Arc<session_store::RuntimeSessionStore> {
    Arc::new(
        session_store::RuntimeSessionStore::open(root.join("runtime-sessions.json"), root).unwrap(),
    )
}

fn test_turn_scheduler() -> tokio::sync::mpsc::UnboundedSender<uuid::Uuid> {
    tokio::sync::mpsc::unbounded_channel().0
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
            TaskManager::open(TaskManagerOptions {
                path: root.join("tasks.json"),
                interactions_path: root.join("interactions.json"),
                home: root.clone(),
                events,
                agents: agents.clone(),
                sessions: test_runtime_session_store(&root),
                turn_scheduler: test_turn_scheduler(),
                runtime_url: "http://127.0.0.1:1".to_owned(),
                runtime_token: "test-token".to_owned(),
            })
            .unwrap(),
        ),
        agents,
        agent_commands: Arc::new(
            AgentCommandStore::open(root.join("agent-commands.json")).unwrap(),
        ),
        sessions: Arc::new(
            session_store::RuntimeSessionStore::open(root.join("runtime-sessions.json"), &root)
                .unwrap(),
        ),
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
    let child_event = |kind: &str, extra: serde_json::Value| {
        let mut value = serde_json::json!({"type": kind, "id": child_id});
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        value.to_string()
    };
    store
        .apply_harness_event(
            task_id,
            &child_event(
                "subagent_started",
                serde_json::json!({
                    "profile": "scout",
                    "label": "inspect files",
                    "background": true
                }),
            ),
        )
        .unwrap();
    store
        .apply_harness_event(
            task_id,
            &child_event("subagent_turn_started", serde_json::json!({"turn": 2})),
        )
        .unwrap();
    store
        .apply_harness_event(
            task_id,
            &child_event(
                "subagent_usage",
                serde_json::json!({
                    "input_tokens": 7,
                    "output_tokens": 2,
                    "total_tokens": 9
                }),
            ),
        )
        .unwrap();
    store
        .apply_harness_event(
            task_id,
            &child_event(
                "subagent_completed",
                serde_json::json!({"status": "completed"}),
            ),
        )
        .unwrap();
    let completed_child = store.get(child_id).unwrap().unwrap();
    assert_eq!(completed_child.status, RuntimeAgentStatus::Completed);
    assert_eq!(completed_child.current_turn, 2);
    assert_eq!(completed_child.total_tokens, Some(9));

    store
        .apply_harness_event(
            task_id,
            &child_event(
                "subagent_started",
                serde_json::json!({
                    "profile": "scout",
                    "label": "inspect files retry",
                    "background": true
                }),
            ),
        )
        .unwrap();
    let retried_child = store.get(child_id).unwrap().unwrap();
    assert_eq!(retried_child.status, RuntimeAgentStatus::Running);
    assert_eq!(retried_child.current_turn, 0);
    assert_eq!(retried_child.total_tokens, None);
    store
        .apply_harness_event(
            task_id,
            &child_event(
                "subagent_completed",
                serde_json::json!({"status": "cancelled"}),
            ),
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
    assert_eq!(child.status, RuntimeAgentStatus::Cancelled);
    assert_eq!(child.current_turn, 0);
    assert_eq!(child.total_tokens, None);
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
        session_id: None,
        turn_id: None,
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
    let agents = test_agent_store(&root);
    let events = Arc::new(EventLog::open(root.join("events.ndjson")).unwrap());
    let manager = TaskManager::open(TaskManagerOptions {
        path,
        interactions_path: root.join("interactions.json"),
        home: root.clone(),
        events: events.clone(),
        agents,
        sessions: test_runtime_session_store(&root),
        turn_scheduler: test_turn_scheduler(),
        runtime_url: "http://127.0.0.1:1".to_owned(),
        runtime_token: "test-token".to_owned(),
    })
    .unwrap();
    let recovered = manager.tasks.blocking_read();
    assert_eq!(recovered[&id].status, RuntimeTaskStatus::Interrupted);
    assert_eq!(recovered[&id].pid, None);
    assert!(recovered[&id].completed_at.is_some());
    drop(recovered);
    let recovered_events = events.read_after(0, 10).unwrap();
    assert_eq!(recovered_events.len(), 1);
    assert_eq!(recovered_events[0].kind, "task.interrupted");
    assert!(
        recovered_events[0]
            .message
            .contains(&format!("task_id={id}"))
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn task_recovery_interrupts_waiting_task_and_cancels_its_interaction() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-waiting-task-recovery-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let task_id = uuid::Uuid::new_v4();
    let interaction_id = uuid::Uuid::new_v4();
    let tasks_path = root.join("tasks.json");
    let interactions_path = root.join("interactions.json");
    persist_tasks(
        &tasks_path,
        &HashMap::from([(
            task_id,
            RuntimeTask {
                id: task_id,
                session_id: None,
                turn_id: None,
                agent_id: None,
                event_start_sequence: 0,
                status: RuntimeTaskStatus::WaitingApproval,
                workspace: root.clone(),
                profile: None,
                pid: None,
                created_at: 1,
                started_at: Some(2),
                completed_at: None,
                exit_code: None,
                error: None,
            },
        )]),
    )
    .unwrap();
    persist_interactions(
        &interactions_path,
        &HashMap::from([(
            interaction_id,
            RuntimeInteraction {
                id: interaction_id,
                task_id,
                kind: InteractionKind::Approval {
                    description: "test approval".to_owned(),
                    always_allow_available: true,
                },
                status: InteractionStatus::Pending,
                resolution: None,
                created_at: 3,
                resolved_at: None,
            },
        )]),
    )
    .unwrap();
    let events = Arc::new(EventLog::open(root.join("events.ndjson")).unwrap());
    let manager = TaskManager::open(TaskManagerOptions {
        path: tasks_path,
        interactions_path: interactions_path.clone(),
        home: root.clone(),
        events: events.clone(),
        agents: test_agent_store(&root),
        sessions: test_runtime_session_store(&root),
        turn_scheduler: test_turn_scheduler(),
        runtime_url: "http://127.0.0.1:1".to_owned(),
        runtime_token: "test-token".to_owned(),
    })
    .unwrap();

    assert_eq!(
        manager.tasks.blocking_read()[&task_id].status,
        RuntimeTaskStatus::Interrupted
    );
    let interactions = load_interactions(&interactions_path).unwrap();
    assert_eq!(
        interactions[&interaction_id].status,
        InteractionStatus::Cancelled
    );
    let kinds = events
        .read_after(0, 10)
        .unwrap()
        .into_iter()
        .map(|event| event.kind)
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        vec!["task.interrupted", "task.interaction_cancelled"]
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn task_recovery_preserves_the_session_root_agent_id() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-session-task-recovery-{}",
        uuid::Uuid::new_v4()
    ));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let sessions = test_runtime_session_store(&root);
    let session = sessions
        .create(session_store::CreateRuntimeSession {
            id: None,
            workspace: workspace.clone(),
            profile: None,
            config: None,
            title: None,
        })
        .unwrap();
    let task_id = uuid::Uuid::new_v4();
    let turn_id = uuid::Uuid::new_v4();
    let path = root.join("tasks.json");
    let task = RuntimeTask {
        id: task_id,
        session_id: Some(session.id),
        turn_id: Some(turn_id),
        agent_id: Some(session.root_agent_id),
        event_start_sequence: 0,
        status: RuntimeTaskStatus::Running,
        workspace,
        profile: None,
        pid: Some(10),
        created_at: 1,
        started_at: Some(2),
        completed_at: None,
        exit_code: None,
        error: None,
    };
    persist_tasks(&path, &HashMap::from([(task_id, task)])).unwrap();
    let agents = test_agent_store(&root);
    let events = Arc::new(EventLog::open(root.join("events.ndjson")).unwrap());
    let manager = TaskManager::open(TaskManagerOptions {
        path,
        interactions_path: root.join("interactions.json"),
        home: root.clone(),
        events: events.clone(),
        agents: agents.clone(),
        sessions,
        turn_scheduler: test_turn_scheduler(),
        runtime_url: "http://127.0.0.1:1".to_owned(),
        runtime_token: "test-token".to_owned(),
    })
    .unwrap();

    assert_eq!(
        manager.tasks.blocking_read()[&task_id].agent_id,
        Some(session.root_agent_id)
    );
    assert_eq!(agents.list().unwrap().len(), 1);
    assert_eq!(agents.list().unwrap()[0].id, session.root_agent_id);
    assert_eq!(
        events
            .read_after(0, 10)
            .unwrap()
            .into_iter()
            .map(|event| event.kind)
            .collect::<Vec<_>>(),
        vec!["task.interrupted", "turn.interrupted"]
    );
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

#[test]
fn daemon_lock_heartbeat_refreshes_only_the_owner_lease() {
    let root =
        std::env::temp_dir().join(format!("willdeep-lock-heartbeat-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("daemon.lock");
    let lock = acquire_daemon_lock(&path).unwrap();
    assert!(refresh_daemon_lock(&path, "not-the-owner").is_err());
    refresh_daemon_lock(&path, &lock.token).unwrap();
    let refreshed = load_daemon_lock(&path).unwrap();
    assert_eq!(refreshed.token, lock.token);
    assert!(refreshed.created_at >= lock.created_at);
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
        TaskManager::open(TaskManagerOptions {
            path: path.clone(),
            interactions_path: root.join("interactions.json"),
            home: root.clone(),
            events,
            agents,
            sessions: test_runtime_session_store(&root),
            turn_scheduler: test_turn_scheduler(),
            runtime_url: "http://127.0.0.1:1".to_owned(),
            runtime_token: "test-token".to_owned(),
        })
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
                    session_id: None,
                    turn_id: None,
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
    let manager = TaskManager::open(TaskManagerOptions {
        path: root.join("tasks.json"),
        interactions_path: root.join("interactions.json"),
        home: root.clone(),
        events,
        agents,
        sessions: test_runtime_session_store(&root),
        turn_scheduler: test_turn_scheduler(),
        runtime_url: "http://127.0.0.1:1".to_owned(),
        runtime_token: "test-token".to_owned(),
    })
    .unwrap();
    let task_id = uuid::Uuid::new_v4();
    manager
        .insert_and_persist(RuntimeTask {
            id: task_id,
            session_id: None,
            turn_id: None,
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
