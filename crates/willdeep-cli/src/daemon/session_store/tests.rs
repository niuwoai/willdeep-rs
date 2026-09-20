use super::*;

#[test]
fn completion_keeps_harness_boundary_when_another_writer_advances_history() {
    let root = std::env::temp_dir().join(format!("runtime-end-boundary-{}", uuid::Uuid::new_v4()));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let store = RuntimeSessionStore::open(root.join("runtime-sessions.json"), &root).unwrap();
    let session = store
        .create(CreateRuntimeSession {
            id: None,
            workspace,
            profile: None,
            model: None,
            config: None,
            title: None,
        })
        .unwrap();
    let (turn, _) = store
        .enqueue_turn(
            session.id,
            CreateRuntimeTurn {
                request_id: uuid::Uuid::new_v4(),
                prompt: "first request".into(),
                attachments: Vec::new(),
                origin_client: None,
            },
        )
        .unwrap();
    store.claim_next(session.id).unwrap().unwrap();
    let task = uuid::Uuid::new_v4();
    store.bind_task(turn.id, task).unwrap();
    let completed = store
        .core
        .update(session.id, |core| {
            core.messages
                .push(willdeep_core::Message::user("first request"));
            core.messages.push(willdeep_core::Message::assistant(
                "first result",
                Vec::new(),
            ));
        })
        .unwrap();
    store
        .record_execution_end(
            task,
            completed.messages.len(),
            completed.compression_generation,
        )
        .unwrap();
    store
        .core
        .update(session.id, |core| {
            core.messages
                .push(willdeep_core::Message::user("next request"));
            core.compression_generation += 1;
        })
        .unwrap();
    store
        .complete_task(task, RuntimeTaskStatus::Completed, None)
        .unwrap();
    let finished = store.get_turn(turn.id).unwrap().unwrap();
    assert_eq!(finished.message_end, Some(completed.messages.len()));
    assert_eq!(
        finished.message_generation,
        completed.compression_generation
    );
    let reopened = RuntimeSessionStore::open(root.join("runtime-sessions.json"), &root).unwrap();
    assert_eq!(
        reopened.get_turn(turn.id).unwrap().unwrap().message_end,
        finished.message_end
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn execution_start_rebases_queued_boundary_and_rejects_changed_replay() {
    let root =
        std::env::temp_dir().join(format!("runtime-start-boundary-{}", uuid::Uuid::new_v4()));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let store = RuntimeSessionStore::open(root.join("runtime-sessions.json"), &root).unwrap();
    let session = store
        .create(CreateRuntimeSession {
            id: None,
            workspace,
            profile: None,
            model: None,
            config: None,
            title: None,
        })
        .unwrap();
    let (turn, _) = store
        .enqueue_turn(
            session.id,
            CreateRuntimeTurn {
                request_id: uuid::Uuid::new_v4(),
                prompt: "queued request".into(),
                attachments: Vec::new(),
                origin_client: None,
            },
        )
        .unwrap();
    store.claim_next(session.id).unwrap().unwrap();
    let task = uuid::Uuid::new_v4();
    store.bind_task(turn.id, task).unwrap();
    let core = store
        .core
        .update(session.id, |core| {
            core.messages
                .push(willdeep_core::Message::user("intervening local turn"));
            core.compression_generation += 1;
        })
        .unwrap();
    store.prepare_execution(task, &core).unwrap();
    let updated = store.get_turn(turn.id).unwrap().unwrap();
    assert_eq!(updated.message_start, Some(core.messages.len()));
    assert_eq!(updated.message_generation, core.compression_generation);
    let mut replay = core;
    replay
        .messages
        .push(willdeep_core::Message::user("queued request"));
    store
        .turns_lock()
        .unwrap()
        .get_mut(&turn.id)
        .unwrap()
        .replay_existing_user_message = true;
    store.prepare_execution(task, &replay).unwrap();
    replay.messages.last_mut().unwrap().content = "different request".into();
    assert!(store.prepare_execution(task, &replay).is_err());
    assert_eq!(
        store.get_turn(turn.id).unwrap().unwrap().message_start,
        updated.message_start
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn migrates_schema_one_with_private_backup_and_rejects_future_schema() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-runtime-session-migration-{}",
        uuid::Uuid::new_v4()
    ));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let path = root.join("runtime-sessions.json");
    let id = uuid::Uuid::new_v4();
    let legacy = serde_json::to_vec_pretty(&vec![serde_json::json!({
        "schema": 1,
        "id": id,
        "root_agent_id": uuid::Uuid::new_v4(),
        "workspace": workspace.canonicalize().unwrap(),
        "profile": null,
        "model": null,
        "config": null,
        "status": "idle",
        "active_turn_id": null,
        "created_at": 1,
        "updated_at": 1,
        "last_error": null
    })])
    .unwrap();
    std::fs::write(&path, &legacy).unwrap();

    let store = RuntimeSessionStore::open(path.clone(), &root).unwrap();
    let migrated = store.get(id).unwrap().unwrap();
    assert_eq!(migrated.schema, RUNTIME_SESSION_SCHEMA);
    let persisted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(persisted[0]["schema"], RUNTIME_SESSION_SCHEMA);
    let backups = std::fs::read_dir(&root)
        .unwrap()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().contains(".schema1."))
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    assert_eq!(backups.len(), 1);
    assert_eq!(std::fs::read(&backups[0]).unwrap(), legacy);
    drop(store);
    drop(RuntimeSessionStore::open(path.clone(), &root).unwrap());
    assert_eq!(
        std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().contains(".schema1."))
            .count(),
        1,
        "a completed migration must not create another backup"
    );

    let future = root.join("future-sessions.json");
    let mut future_value: serde_json::Value = serde_json::from_slice(&legacy).unwrap();
    future_value[0]["schema"] = serde_json::json!(RUNTIME_SESSION_SCHEMA + 1);
    std::fs::write(&future, serde_json::to_vec_pretty(&future_value).unwrap()).unwrap();
    assert!(RuntimeSessionStore::open(future, &root).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn first_turn_titles_only_unnamed_sessions_without_exposing_secrets() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-runtime-auto-title-{}",
        uuid::Uuid::new_v4()
    ));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let store = RuntimeSessionStore::open(root.join("runtime-sessions.json"), &root).unwrap();
    let core = willdeep_core::SessionStore::new(&root);
    let create = |title| {
        store
            .create(CreateRuntimeSession {
                id: None,
                workspace: workspace.clone(),
                profile: None,
                model: None,
                config: None,
                title,
            })
            .unwrap()
    };

    let automatic = create(None);
    store
        .enqueue_turn(
            automatic.id,
            CreateRuntimeTurn {
                origin_client: None,
                request_id: uuid::Uuid::new_v4(),
                prompt: "  Analyze   the session migration architecture  ".to_owned(),
                attachments: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(
        core.load(automatic.id).unwrap().title,
        "Analyze the session migration architecture"
    );
    assert_eq!(
        core.load(automatic.id).unwrap().title_source,
        willdeep_core::TitleSource::Derived
    );

    let renamed = create(None);
    store.rename(renamed.id, "User title".to_owned()).unwrap();
    store
        .enqueue_turn(
            renamed.id,
            CreateRuntimeTurn {
                origin_client: None,
                request_id: uuid::Uuid::new_v4(),
                prompt: "must not replace the title".to_owned(),
                attachments: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(core.load(renamed.id).unwrap().title, "User title");

    let sensitive = create(None);
    store
        .enqueue_turn(
            sensitive.id,
            CreateRuntimeTurn {
                origin_client: None,
                request_id: uuid::Uuid::new_v4(),
                prompt: "debug password = NeverCopyThisValue123".to_owned(),
                attachments: Vec::new(),
            },
        )
        .unwrap();
    // 凭据样的提示词一个字都不许进标题：占位符原地不动。
    assert_eq!(core.load(sensitive.id).unwrap().title, "New session");

    let attachment = create(None);
    store
        .enqueue_turn(
            attachment.id,
            CreateRuntimeTurn {
                origin_client: None,
                request_id: uuid::Uuid::new_v4(),
                prompt: String::new(),
                attachments: vec![willdeep_core::MessageAttachment::Text {
                    name: "notes.txt".to_owned(),
                    content: "fixture".to_owned(),
                }],
            },
        )
        .unwrap();
    assert_eq!(
        core.load(attachment.id).unwrap().title,
        "Attachment conversation"
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// 历史面板此前只列 Runtime 登记过的会话。同一个工作区里还躺着两类文件：
/// 桌面版 Xedit 写的，以及 TUI 建了却从没提交过 Runtime 轮次的——它们
/// `--resume` 一直打得开，却从来不出现在列表里。列表少一半等于列表说谎。
#[test]
fn search_lists_sessions_the_runtime_never_registered() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-runtime-unmanaged-search-{}",
        uuid::Uuid::new_v4()
    ));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let canonical = workspace.canonicalize().unwrap();
    let store = RuntimeSessionStore::open(root.join("runtime-sessions.json"), &root).unwrap();
    let core = willdeep_core::SessionStore::new(&root);

    let managed = store
        .create(CreateRuntimeSession {
            id: None,
            workspace: workspace.clone(),
            profile: None,
            model: None,
            config: None,
            title: Some("Runtime 登记过的会话".to_owned()),
        })
        .unwrap();

    // 只有 Core 文件、Runtime 一无所知的那一类。
    let mut orphan = willdeep_core::Session::new(canonical.clone(), None, "只有会话文件的历史对话");
    orphan
        .messages
        .push(willdeep_core::Message::user("只有会话文件的历史对话"));
    core.save(&mut orphan).unwrap();

    // 另一个工作区的会话不能混进来。
    let elsewhere = root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let mut foreign =
        willdeep_core::Session::new(elsewhere.canonicalize().unwrap(), None, "别的工作区的会话");
    core.save(&mut foreign).unwrap();

    let results = store
        .search(SessionSearchQuery {
            q: None,
            workspace: Some(workspace.clone()),
            status: None,
            profile: None,
            model: None,
            updated_after: None,
            updated_before: None,
        })
        .unwrap();
    let found = results
        .iter()
        .map(|result| (result.id, result.origin))
        .collect::<Vec<_>>();
    assert!(
        found.contains(&(managed.id, SessionOrigin::Runtime)),
        "{found:?}"
    );
    assert!(
        found.contains(&(orphan.id, SessionOrigin::Local)),
        "{found:?}"
    );
    assert!(
        !found.iter().any(|(id, _)| *id == foreign.id),
        "workspace filter must still hold: {found:?}"
    );
    let orphan_result = results
        .iter()
        .find(|result| result.id == orphan.id)
        .unwrap();
    assert_eq!(orphan_result.title, "只有会话文件的历史对话");
    assert_eq!(orphan_result.message_count, 1);

    // 关键词同时匹配标题与正文，两条路都要走通。
    for query in ["只有会话文件", "历史对话"] {
        let hits = store
            .search(SessionSearchQuery {
                q: Some(query.to_owned()),
                workspace: Some(workspace.clone()),
                status: None,
                profile: None,
                model: None,
                updated_after: None,
                updated_before: None,
            })
            .unwrap();
        assert!(
            hits.iter().any(|result| result.id == orphan.id),
            "{query} missed the unmanaged Session"
        );
    }

    // Provider / 模型过滤器只存在于 Runtime 元数据里。未登记的会话没有这些
    // 字段，混进来就是让过滤器撒谎——整段跳过才是诚实的。
    let filtered = store
        .search(SessionSearchQuery {
            q: None,
            workspace: Some(workspace.clone()),
            status: None,
            profile: None,
            model: Some("mock-model".to_owned()),
            updated_after: None,
            updated_before: None,
        })
        .unwrap();
    assert!(
        !filtered.iter().any(|result| result.id == orphan.id),
        "a model filter must not match a Session that has no model"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn manages_session_snapshot_lifecycle_without_exporting_private_queue_data() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-runtime-session-management-{}",
        uuid::Uuid::new_v4()
    ));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let store = RuntimeSessionStore::open(root.join("runtime-sessions.json"), &root).unwrap();
    let session = store
        .create(CreateRuntimeSession {
            id: None,
            workspace,
            profile: Some("mock".to_owned()),
            model: Some("mock-model".to_owned()),
            config: Some(root.join("config.toml")),
            title: Some("Original title".to_owned()),
        })
        .unwrap();
    let core_store = willdeep_core::SessionStore::new(&root);
    let mut core = core_store.load(session.id).unwrap();
    core.messages
        .push(willdeep_core::Message::user("needle in durable history"));
    core.attention_read.insert("job-private".to_owned());
    core.runtime_event_cursor = 42;
    core_store.save(&mut core).unwrap();

    store
        .rename(session.id, "  Renamed   session  ".to_owned())
        .unwrap();
    assert_eq!(
        core_store.load(session.id).unwrap().title,
        "Renamed session"
    );
    let title_results = store
        .search(SessionSearchQuery {
            q: Some("renamed".to_owned()),
            workspace: None,
            status: None,
            profile: None,
            model: None,
            updated_after: None,
            updated_before: None,
        })
        .unwrap();
    assert_eq!(title_results.len(), 1);
    assert_eq!(title_results[0].id, session.id);
    let message_results = store
        .search(SessionSearchQuery {
            q: Some("needle".to_owned()),
            workspace: None,
            status: None,
            profile: None,
            model: None,
            updated_after: None,
            updated_before: None,
        })
        .unwrap();
    assert_eq!(message_results.len(), 1);
    assert!(
        message_results[0]
            .snippet
            .as_deref()
            .unwrap()
            .contains("needle")
    );
    let filtered_results = store
        .search(SessionSearchQuery {
            q: None,
            workspace: Some(root.join("workspace")),
            status: Some(RuntimeSessionStatus::Idle),
            profile: Some("MOCK".to_owned()),
            model: Some("MOCK-MODEL".to_owned()),
            updated_after: Some(0),
            updated_before: Some(u64::MAX),
        })
        .unwrap();
    assert_eq!(filtered_results.len(), 1);
    assert_eq!(filtered_results[0].id, session.id);
    assert_eq!(filtered_results[0].profile.as_deref(), Some("mock"));
    assert_eq!(filtered_results[0].model.as_deref(), Some("mock-model"));
    assert!(
        store
            .search(SessionSearchQuery {
                q: None,
                workspace: None,
                status: None,
                profile: Some("other".to_owned()),
                model: None,
                updated_after: None,
                updated_before: None,
            })
            .unwrap()
            .is_empty()
    );

    let (queued, _) = store
        .enqueue_turn(
            session.id,
            CreateRuntimeTurn {
                origin_client: None,
                request_id: uuid::Uuid::new_v4(),
                prompt: "private queued prompt".to_owned(),
                attachments: Vec::new(),
            },
        )
        .unwrap();
    assert!(store.archive(session.id).is_err());
    assert!(
        store
            .fork_through(session.id, None, None, None, None)
            .is_err()
    );
    assert!(store.delete(session.id, session.id).is_err());
    store.request_cancel(queued.id).unwrap();

    assert_eq!(
        store.archive(session.id).unwrap().status,
        RuntimeSessionStatus::Archived
    );
    assert!(
        store
            .enqueue_turn(
                session.id,
                CreateRuntimeTurn {
                    origin_client: None,
                    request_id: uuid::Uuid::new_v4(),
                    prompt: "blocked while archived".to_owned(),
                    attachments: Vec::new(),
                },
            )
            .is_err()
    );
    assert_eq!(
        store.unarchive(session.id).unwrap().status,
        RuntimeSessionStatus::Idle
    );

    let fork = store
        .fork_through(
            session.id,
            Some("Forked snapshot".to_owned()),
            None,
            None,
            None,
        )
        .unwrap();
    assert_ne!(fork.id, session.id);
    assert_ne!(fork.root_agent_id, session.root_agent_id);
    assert!(store.list_turns(fork.id).unwrap().is_empty());
    let fork_core = core_store.load(fork.id).unwrap();
    assert_eq!(fork_core.title, "Forked snapshot");
    assert_eq!(fork_core.messages.len(), core.messages.len());
    assert_eq!(fork_core.messages[0].content, core.messages[0].content);
    assert!(fork_core.attention_read.is_empty());
    assert_eq!(fork_core.runtime_event_cursor, 0);

    let export = store.export(session.id).unwrap();
    let export_json = serde_json::to_string(&export).unwrap();
    assert!(export_json.contains("needle in durable history"));
    assert!(!export_json.contains("private queued prompt"));
    assert!(!export_json.contains("job-private"));
    assert!(store.delete(fork.id, session.id).is_err());
    store.delete(fork.id, fork.id).unwrap();
    assert!(store.get(fork.id).unwrap().is_none());
    assert!(core_store.load(fork.id).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn creates_matching_core_session_and_recovers_active_state() {
    let root =
        std::env::temp_dir().join(format!("willdeep-runtime-session-{}", uuid::Uuid::new_v4()));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let path = root.join("runtime-sessions.json");
    let store = RuntimeSessionStore::open(path.clone(), &root).unwrap();
    let session = store
        .create(CreateRuntimeSession {
            id: None,
            workspace: workspace.clone(),
            profile: Some("some-im".to_owned()),
            model: Some("qwen3".to_owned()),
            config: Some(root.join("config.toml")),
            title: Some("Persistent work".to_owned()),
        })
        .unwrap();
    let core = willdeep_core::SessionStore::new(&root)
        .load(session.id)
        .unwrap();
    assert_eq!(core.id, session.id);
    assert_eq!(core.workspace, workspace.canonicalize().unwrap());
    assert_eq!(session.status, RuntimeSessionStatus::Idle);
    assert_eq!(store.list().unwrap(), vec![session.clone()]);

    {
        let mut sessions = store.lock().unwrap();
        let stored = sessions.get_mut(&session.id).unwrap();
        stored.status = RuntimeSessionStatus::Running;
        stored.active_turn_id = Some(uuid::Uuid::new_v4());
        persist_sessions(&path, &sessions).unwrap();
    }
    drop(store);
    let reopened = RuntimeSessionStore::open(path, &root).unwrap();
    let recovered = reopened.get(session.id).unwrap().unwrap();
    assert_eq!(recovered.status, RuntimeSessionStatus::Interrupted);
    assert!(recovered.active_turn_id.is_none());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn updates_idle_session_model_in_runtime_and_core_metadata() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-runtime-model-update-{}",
        uuid::Uuid::new_v4()
    ));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let store = RuntimeSessionStore::open(root.join("runtime-sessions.json"), &root).unwrap();
    let session = store
        .create(CreateRuntimeSession {
            id: None,
            workspace,
            profile: Some("some-im".to_owned()),
            model: Some("old-model".to_owned()),
            config: None,
            title: Some("Switch model".to_owned()),
        })
        .unwrap();

    let updated = store
        .update_model(session.id, "new-model".to_owned())
        .unwrap();
    let core = willdeep_core::SessionStore::new(&root)
        .load(session.id)
        .unwrap();

    assert_eq!(updated.model.as_deref(), Some("new-model"));
    assert_eq!(core.model.as_deref(), Some("new-model"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn adopts_an_existing_core_session_idempotently() {
    let root =
        std::env::temp_dir().join(format!("willdeep-runtime-adopt-{}", uuid::Uuid::new_v4()));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let core_store = willdeep_core::SessionStore::new(&root);
    let mut core = willdeep_core::Session::new(
        workspace.canonicalize().unwrap(),
        Some("mock".to_owned()),
        "existing",
    );
    core.model = Some("stored-model".to_owned());
    let stored_config = root.join("private-config.toml");
    core.config = Some(stored_config.clone());
    core_store.save(&mut core).unwrap();
    let store = RuntimeSessionStore::open(root.join("runtime-sessions.json"), &root).unwrap();
    let request = || CreateRuntimeSession {
        id: Some(core.id),
        workspace: workspace.clone(),
        profile: Some("mock".to_owned()),
        model: None,
        config: Some(root.join("client-supplied-config.toml")),
        title: Some("ignored for adoption".to_owned()),
    };

    let (adopted, created) = store.ensure(request()).unwrap();
    assert!(created);
    assert_eq!(adopted.id, core.id);
    assert_eq!(adopted.profile.as_deref(), Some("mock"));
    assert_eq!(adopted.model.as_deref(), Some("stored-model"));
    assert_eq!(adopted.config.as_ref(), Some(&stored_config));
    let (same, created) = store.ensure(request()).unwrap();
    assert!(!created);
    assert_eq!(same, adopted);
    assert_eq!(store.list().unwrap(), vec![adopted]);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn rewinding_truncates_in_place_and_forgets_the_dropped_turns() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-runtime-turn-rewind-{}",
        uuid::Uuid::new_v4()
    ));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let store = RuntimeSessionStore::open(root.join("runtime-sessions.json"), &root).unwrap();
    let session = store
        .create(CreateRuntimeSession {
            id: None,
            workspace,
            profile: None,
            model: None,
            config: None,
            title: Some("Rewind".to_owned()),
        })
        .unwrap();
    let core_store = willdeep_core::SessionStore::new(&root);
    let complete_turn = |prompt: &str, checkpoint: Option<&str>| {
        let (turn, _) = store
            .enqueue_turn(
                session.id,
                CreateRuntimeTurn {
                    origin_client: None,
                    request_id: uuid::Uuid::new_v4(),
                    prompt: prompt.to_owned(),
                    attachments: Vec::new(),
                },
            )
            .unwrap();
        store.claim_next(session.id).unwrap().unwrap();
        let mut core = core_store.load(session.id).unwrap();
        core.messages.push(willdeep_core::Message::user(prompt));
        core.messages
            .push(willdeep_core::Message::assistant("answer", Vec::new()));
        core_store.save(&mut core).unwrap();
        let task_id = uuid::Uuid::new_v4();
        assert!(store.bind_task(turn.id, task_id).unwrap());
        if let Some(commit) = checkpoint {
            store
                .record_workspace_checkpoint(turn.id, commit.to_owned())
                .unwrap();
        }
        store
            .complete_task(task_id, RuntimeTaskStatus::Completed, None)
            .unwrap();
        turn.id
    };

    let first = complete_turn("first", Some("c1"));
    let second = complete_turn("second", Some("c2"));
    let third = complete_turn("third", None);
    assert_eq!(
        store
            .get_turn(second)
            .unwrap()
            .unwrap()
            .workspace_checkpoint,
        Some("c2".to_owned())
    );

    // 已经在最后一步：没有可回退的。
    let error = store.rewind_plan(session.id, Some(third)).unwrap_err();
    assert!(error.to_string().contains("nothing to rewind"), "{error:#}");

    // 回到第 1 步：丢第 2、3 步，文件回到第 2 步开始前的检查点。
    let plan = store.rewind_plan(session.id, Some(first)).unwrap();
    assert_eq!(plan.message_end, 2);
    assert_eq!(plan.dropped_turn_ids, vec![second, third]);
    assert_eq!(plan.workspace_checkpoint.as_deref(), Some("c2"));
    let rewound = store.commit_rewind(&plan).unwrap();
    assert_eq!(rewound.id, session.id);
    let core = core_store.load(session.id).unwrap();
    assert_eq!(core.messages.len(), 2);
    assert_eq!(core.messages[0].content, "first");
    let remaining = store.list_turns(session.id).unwrap();
    assert_eq!(
        remaining.iter().map(|turn| turn.id).collect::<Vec<_>>(),
        vec![first]
    );
    assert!(store.get_turn(second).unwrap().is_none());

    // 同一份计划不能提交两次：轮次已经没了。
    assert!(
        store.commit_rewind(&plan).is_err() || store.list_turns(session.id).unwrap().len() == 1
    );

    // 回到开头：没压缩过的会话可以；文件检查点是第 1 步的。
    let plan = store.rewind_plan(session.id, None).unwrap();
    assert_eq!(plan.message_end, 0);
    assert_eq!(plan.dropped_turn_ids, vec![first]);
    assert_eq!(plan.workspace_checkpoint.as_deref(), Some("c1"));
    store.commit_rewind(&plan).unwrap();
    assert!(core_store.load(session.id).unwrap().messages.is_empty());
    assert!(store.list_turns(session.id).unwrap().is_empty());

    // 压缩过的会话回不到开头。
    let fourth = complete_turn("fourth", None);
    let mut core = core_store.load(session.id).unwrap();
    assert!(core.replace_with_compressed_messages(vec![willdeep_core::Message::user("summary")]));
    core_store.save(&mut core).unwrap();
    let error = store.rewind_plan(session.id, None).unwrap_err();
    assert!(error.to_string().contains("compressed"), "{error:#}");
    let error = store.rewind_plan(session.id, Some(fourth)).unwrap_err();
    assert!(
        error.to_string().contains("compression checkpoint"),
        "{error:#}"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn records_message_boundaries_and_forks_through_a_completed_turn() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-runtime-turn-fork-{}",
        uuid::Uuid::new_v4()
    ));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let store = RuntimeSessionStore::open(root.join("runtime-sessions.json"), &root).unwrap();
    let session = store
        .create(CreateRuntimeSession {
            id: None,
            workspace,
            profile: None,
            model: Some("source-model".to_owned()),
            config: None,
            title: Some("Turn boundary".to_owned()),
        })
        .unwrap();
    let core_store = willdeep_core::SessionStore::new(&root);

    let complete_turn = |prompt: &str, answer: &str| {
        let (turn, _) = store
            .enqueue_turn(
                session.id,
                CreateRuntimeTurn {
                    origin_client: None,
                    request_id: uuid::Uuid::new_v4(),
                    prompt: prompt.to_owned(),
                    attachments: Vec::new(),
                },
            )
            .unwrap();
        let claimed = store.claim_next(session.id).unwrap().unwrap();
        assert_eq!(claimed.request.model.as_deref(), Some("source-model"));
        let mut core = core_store.load(session.id).unwrap();
        core.messages.push(willdeep_core::Message::user(prompt));
        core.messages
            .push(willdeep_core::Message::assistant(answer, Vec::new()));
        core_store.save(&mut core).unwrap();
        let task_id = uuid::Uuid::new_v4();
        assert!(store.bind_task(turn.id, task_id).unwrap());
        store
            .complete_task(task_id, RuntimeTaskStatus::Completed, None)
            .unwrap();
        turn.id
    };

    let first = complete_turn("first", "first answer");
    let second = complete_turn("second", "second answer");
    let first_turn = store.get_turn(first).unwrap().unwrap();
    let second_turn = store.get_turn(second).unwrap().unwrap();
    assert_eq!(
        (first_turn.message_start, first_turn.message_end),
        (Some(0), Some(2))
    );
    assert_eq!(
        (second_turn.message_start, second_turn.message_end),
        (Some(2), Some(4))
    );
    assert_eq!(first_turn.message_generation, 0);
    assert_eq!(second_turn.message_generation, 0);

    let fork = store
        .fork_through(
            session.id,
            Some("Through first".to_owned()),
            Some(first),
            Some("research".to_owned()),
            Some("deep-model".to_owned()),
        )
        .unwrap();
    assert_eq!(fork.profile.as_deref(), Some("research"));
    assert_eq!(fork.model.as_deref(), Some("deep-model"));
    let fork_core = core_store.load(fork.id).unwrap();
    assert_eq!(fork_core.profile.as_deref(), Some("research"));
    assert_eq!(fork_core.model.as_deref(), Some("deep-model"));
    assert_eq!(fork_core.messages.len(), 2);
    assert_eq!(fork_core.messages[0].content, "first");
    assert_eq!(fork_core.messages[1].content, "first answer");
    assert!(store.list_turns(fork.id).unwrap().is_empty());

    let mut compressed_core = core_store.load(session.id).unwrap();
    assert!(
        compressed_core.replace_with_compressed_messages(vec![willdeep_core::Message::user(
            "<context-summary>summary</context-summary>"
        )])
    );
    core_store.save(&mut compressed_core).unwrap();
    assert!(
        store
            .fork_through(
                session.id,
                Some("Stale boundary".to_owned()),
                Some(first),
                None,
                None,
            )
            .unwrap_err()
            .to_string()
            .contains("compression checkpoint")
    );

    let after_compression = complete_turn("third", "third answer");
    assert_eq!(
        store
            .get_turn(after_compression)
            .unwrap()
            .unwrap()
            .message_generation,
        1
    );
    let current_fork = store
        .fork_through(
            session.id,
            Some("Current boundary".to_owned()),
            Some(after_compression),
            None,
            None,
        )
        .unwrap();
    assert_eq!(core_store.load(current_fork.id).unwrap().messages.len(), 3);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn restores_provider_model_and_config_before_claiming_the_next_turn() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-runtime-session-execution-settings-{}",
        uuid::Uuid::new_v4()
    ));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let path = root.join("runtime-sessions.json");
    let config = root.join("session-config.toml");
    let store = RuntimeSessionStore::open(path.clone(), &root).unwrap();
    let session = store
        .create(CreateRuntimeSession {
            id: None,
            workspace: workspace.clone(),
            profile: Some("session-provider".to_owned()),
            model: Some("session-model".to_owned()),
            config: Some(config.clone()),
            title: Some("Execution settings".to_owned()),
        })
        .unwrap();
    store
        .enqueue_turn(
            session.id,
            CreateRuntimeTurn {
                origin_client: None,
                request_id: uuid::Uuid::new_v4(),
                prompt: "continue with the restored settings".to_owned(),
                attachments: Vec::new(),
            },
        )
        .unwrap();
    drop(store);

    let restored = RuntimeSessionStore::open(path.clone(), &root).unwrap();
    let claimed = restored.claim_next(session.id).unwrap().unwrap();
    assert_eq!(claimed.request.profile.as_deref(), Some("session-provider"));
    assert_eq!(claimed.request.model.as_deref(), Some("session-model"));
    assert_eq!(claimed.request.config.as_ref(), Some(&config));
    let core = willdeep_core::SessionStore::new(&root)
        .load(session.id)
        .unwrap();
    assert_eq!(core.profile.as_deref(), Some("session-provider"));
    assert_eq!(core.model.as_deref(), Some("session-model"));
    assert_eq!(core.config.as_ref(), Some(&config));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn safely_requeues_active_turn_without_deleting_a_persisted_user_message() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-runtime-safe-turn-replay-{}",
        uuid::Uuid::new_v4()
    ));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let path = root.join("runtime-sessions.json");
    let store = RuntimeSessionStore::open(path.clone(), &root).unwrap();
    let session = store
        .create(CreateRuntimeSession {
            id: None,
            workspace,
            profile: None,
            model: None,
            config: None,
            title: Some("Replay".to_owned()),
        })
        .unwrap();
    let prompt = "resume this exact turn";
    let (turn, _) = store
        .enqueue_turn(
            session.id,
            CreateRuntimeTurn {
                origin_client: None,
                request_id: uuid::Uuid::new_v4(),
                prompt: prompt.to_owned(),
                attachments: Vec::new(),
            },
        )
        .unwrap();
    let claimed = store.claim_next(session.id).unwrap().unwrap();
    assert!(!claimed.request.replay_existing_user_message);
    assert!(store.bind_task(turn.id, uuid::Uuid::new_v4()).unwrap());
    let core_store = willdeep_core::SessionStore::new(&root);
    let mut core = core_store.load(session.id).unwrap();
    core.messages.push(willdeep_core::Message::user(prompt));
    core_store.save(&mut core).unwrap();
    drop(store);

    let restored = RuntimeSessionStore::open(path.clone(), &root).unwrap();
    assert_eq!(
        restored.get(session.id).unwrap().unwrap().status,
        RuntimeSessionStatus::Idle
    );
    assert_eq!(
        restored.get_turn(turn.id).unwrap().unwrap().status,
        RuntimeTurnStatus::Queued
    );
    assert_eq!(core_store.load(session.id).unwrap().messages.len(), 1);
    assert_eq!(restored.schedulable_sessions().unwrap(), vec![session.id]);
    let replay = restored.claim_next(session.id).unwrap().unwrap();
    assert!(replay.request.replay_existing_user_message);
    assert_eq!(replay.request.prompt, prompt);
    assert_eq!(core_store.load(session.id).unwrap().messages.len(), 1);
    drop(restored);
    let restored_again = RuntimeSessionStore::open(path, &root).unwrap();
    assert_eq!(
        restored_again.get(session.id).unwrap().unwrap().status,
        RuntimeSessionStatus::Idle
    );
    let replay_again = restored_again.claim_next(session.id).unwrap().unwrap();
    assert!(replay_again.request.replay_existing_user_message);
    assert_eq!(core_store.load(session.id).unwrap().messages.len(), 1);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn ambiguous_partial_turn_history_is_preserved_and_not_requeued() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-runtime-ambiguous-turn-replay-{}",
        uuid::Uuid::new_v4()
    ));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let path = root.join("runtime-sessions.json");
    let store = RuntimeSessionStore::open(path.clone(), &root).unwrap();
    let session = store
        .create(CreateRuntimeSession {
            id: None,
            workspace,
            profile: None,
            model: None,
            config: None,
            title: Some("Ambiguous replay".to_owned()),
        })
        .unwrap();
    let (turn, _) = store
        .enqueue_turn(
            session.id,
            CreateRuntimeTurn {
                origin_client: None,
                request_id: uuid::Uuid::new_v4(),
                prompt: "do not discard partial history".to_owned(),
                attachments: Vec::new(),
            },
        )
        .unwrap();
    store.claim_next(session.id).unwrap().unwrap();
    assert!(store.bind_task(turn.id, uuid::Uuid::new_v4()).unwrap());
    let core_store = willdeep_core::SessionStore::new(&root);
    let mut core = core_store.load(session.id).unwrap();
    core.messages.push(willdeep_core::Message::user(
        "do not discard partial history",
    ));
    core.messages.push(willdeep_core::Message::assistant(
        "partial but durable output",
        Vec::new(),
    ));
    core_store.save(&mut core).unwrap();
    drop(store);

    let restored = RuntimeSessionStore::open(path, &root).unwrap();
    assert_eq!(
        restored.get(session.id).unwrap().unwrap().status,
        RuntimeSessionStatus::Interrupted
    );
    assert_eq!(
        restored.get_turn(turn.id).unwrap().unwrap().status,
        RuntimeTurnStatus::Interrupted
    );
    assert!(restored.claim_next(session.id).unwrap().is_none());
    let messages = core_store.load(session.id).unwrap().messages;
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].content, "partial but durable output");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn persisted_tool_activity_blocks_automatic_turn_replay() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-runtime-tool-replay-guard-{}",
        uuid::Uuid::new_v4()
    ));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let path = root.join("runtime-sessions.json");
    let tools_path = root.join("tools.json");
    let store = RuntimeSessionStore::open(path.clone(), &root).unwrap();
    let session = store
        .create(CreateRuntimeSession {
            id: None,
            workspace,
            profile: None,
            model: None,
            config: None,
            title: Some("Tool replay guard".to_owned()),
        })
        .unwrap();
    let (turn, _) = store
        .enqueue_turn(
            session.id,
            CreateRuntimeTurn {
                origin_client: None,
                request_id: uuid::Uuid::new_v4(),
                prompt: "a tool may already have changed the workspace".to_owned(),
                attachments: Vec::new(),
            },
        )
        .unwrap();
    store.claim_next(session.id).unwrap().unwrap();
    let task_id = uuid::Uuid::new_v4();
    assert!(store.bind_task(turn.id, task_id).unwrap());
    std::fs::write(
        &tools_path,
        serde_json::to_vec_pretty(&vec![willdeep_runtime_protocol::RuntimeTool {
            id: uuid::Uuid::new_v4(),
            session_id: Some(session.id),
            turn_id: Some(turn.id),
            task_id,
            agent_id: session.root_agent_id,
            name: "edit_file".to_owned(),
            status: willdeep_runtime_protocol::ToolStatus::Completed,
            started_at_ms: 1,
            completed_at_ms: Some(2),
        }])
        .unwrap(),
    )
    .unwrap();
    drop(store);

    let restored = RuntimeSessionStore::open_guarded(path, &root, &tools_path).unwrap();
    assert_eq!(
        restored.get_turn(turn.id).unwrap().unwrap().status,
        RuntimeTurnStatus::Interrupted
    );
    assert!(restored.claim_next(session.id).unwrap().is_none());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn turn_queue_is_idempotent_strictly_serial_and_persistent() {
    let root =
        std::env::temp_dir().join(format!("willdeep-runtime-turns-{}", uuid::Uuid::new_v4()));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let path = root.join("runtime-sessions.json");
    let store = RuntimeSessionStore::open(path.clone(), &root).unwrap();
    let session = store
        .create(CreateRuntimeSession {
            id: None,
            workspace,
            profile: None,
            model: None,
            config: None,
            title: None,
        })
        .unwrap();
    let first_request = uuid::Uuid::new_v4();
    let (first, created) = store
        .enqueue_turn(
            session.id,
            CreateRuntimeTurn {
                origin_client: None,
                request_id: first_request,
                prompt: "first".to_owned(),
                attachments: Vec::new(),
            },
        )
        .unwrap();
    assert!(created);
    let (duplicate, created) = store
        .enqueue_turn(
            session.id,
            CreateRuntimeTurn {
                origin_client: None,
                request_id: first_request,
                prompt: "must not replace first".to_owned(),
                attachments: Vec::new(),
            },
        )
        .unwrap();
    assert!(!created);
    assert_eq!(duplicate.id, first.id);
    let (second, _) = store
        .enqueue_turn(
            session.id,
            CreateRuntimeTurn {
                origin_client: None,
                request_id: uuid::Uuid::new_v4(),
                prompt: "second".to_owned(),
                attachments: Vec::new(),
            },
        )
        .unwrap();

    let claimed = store.claim_next(session.id).unwrap().unwrap();
    assert_eq!(claimed.metadata.id, first.id);
    assert_eq!(claimed.request.prompt, "first");
    assert!(store.claim_next(session.id).unwrap().is_none());
    let task_id = uuid::Uuid::new_v4();
    assert!(store.bind_task(first.id, task_id).unwrap());
    assert_eq!(
        store.get_turn(first.id).unwrap().unwrap().status,
        RuntimeTurnStatus::Running
    );
    assert_eq!(
        store
            .complete_task(task_id, RuntimeTaskStatus::Completed, None)
            .unwrap(),
        Some(session.id)
    );
    {
        let turns = store.turns_lock().unwrap();
        let stored = turns.get(&first.id).unwrap();
        assert!(stored.prompt.is_empty());
        assert!(stored.attachments.is_empty());
    }
    assert_eq!(
        store.claim_next(session.id).unwrap().unwrap().metadata.id,
        second.id
    );
    drop(store);

    let reopened = RuntimeSessionStore::open(path, &root).unwrap();
    assert_eq!(reopened.list_turns(session.id).unwrap().len(), 2);
    assert_eq!(
        reopened.get_turn(first.id).unwrap().unwrap().status,
        RuntimeTurnStatus::Completed
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn cancelling_a_claimed_turn_releases_the_session_and_next_turn() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-runtime-turn-cancel-{}",
        uuid::Uuid::new_v4()
    ));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let store = RuntimeSessionStore::open(root.join("runtime-sessions.json"), &root).unwrap();
    let session = store
        .create(CreateRuntimeSession {
            id: None,
            workspace,
            profile: None,
            model: None,
            config: None,
            title: None,
        })
        .unwrap();
    let enqueue = |prompt: &str| CreateRuntimeTurn {
        request_id: uuid::Uuid::new_v4(),
        prompt: prompt.to_owned(),
        attachments: Vec::new(),
        origin_client: None,
    };
    let (first, _) = store.enqueue_turn(session.id, enqueue("first")).unwrap();
    let (second, _) = store.enqueue_turn(session.id, enqueue("second")).unwrap();
    assert_eq!(
        store.claim_next(session.id).unwrap().unwrap().metadata.id,
        first.id
    );

    let cancellation = store.request_cancel(first.id).unwrap();
    assert!(cancellation.task_id.is_none());
    assert!(cancellation.cancelled_queued);
    assert_eq!(cancellation.session_id, session.id);
    assert_eq!(
        store.get(session.id).unwrap().unwrap().status,
        RuntimeSessionStatus::Idle
    );
    assert!(!store.bind_task(first.id, uuid::Uuid::new_v4()).unwrap());
    assert_eq!(
        store.claim_next(session.id).unwrap().unwrap().metadata.id,
        second.id
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn session_stop_targets_only_its_declared_active_turn() {
    let active_turn_id = uuid::Uuid::new_v4();
    let session = willdeep_runtime_protocol::RuntimeSession {
        id: uuid::Uuid::new_v4(),
        root_agent_id: uuid::Uuid::new_v4(),
        workspace: None,
        profile: None,
        model: None,
        approval_mode: None,
        status: willdeep_runtime_protocol::SessionStatus::Running,
        active_turn_id: Some(active_turn_id),
        created_at: 1,
        updated_at: 2,
    };
    assert_eq!(active_turn_for_stop(&session).unwrap(), active_turn_id);
    let idle = willdeep_runtime_protocol::RuntimeSession {
        active_turn_id: None,
        status: willdeep_runtime_protocol::SessionStatus::Idle,
        ..session
    };
    assert!(active_turn_for_stop(&idle).is_err());
}
#[test]
fn partial_turn_preserves_boundary_without_marking_completed() {
    let root = std::env::temp_dir().join(format!("willdeep-partial-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let store = RuntimeSessionStore::open(root.join("runtime-sessions.json"), &root).unwrap();
    let session = store
        .create(CreateRuntimeSession {
            id: None,
            workspace: root.clone(),
            profile: None,
            model: None,
            config: None,
            title: None,
        })
        .unwrap();
    let (turn, _) = store
        .enqueue_turn(
            session.id,
            CreateRuntimeTurn {
                origin_client: None,
                request_id: uuid::Uuid::new_v4(),
                prompt: "unfinished objective".into(),
                attachments: Vec::new(),
            },
        )
        .unwrap();
    store.claim_next(session.id).unwrap().unwrap();
    let core_store = willdeep_core::SessionStore::new(&root);
    let mut core = core_store.load(session.id).unwrap();
    core.messages
        .push(willdeep_core::Message::user("unfinished objective"));
    core.messages.push(willdeep_core::Message::assistant(
        "partial work",
        Vec::new(),
    ));
    core_store.save(&mut core).unwrap();
    let task_id = uuid::Uuid::new_v4();
    store.bind_task(turn.id, task_id).unwrap();
    store
        .complete_task(task_id, RuntimeTaskStatus::Partial, None)
        .unwrap();
    let saved = store.get_turn(turn.id).unwrap().unwrap();
    assert_eq!(saved.status, RuntimeTurnStatus::Partial);
    assert_eq!(saved.message_end, Some(2));
    let reopened = RuntimeSessionStore::open(root.join("runtime-sessions.json"), &root).unwrap();
    assert_eq!(
        reopened.get_turn(turn.id).unwrap().unwrap().status,
        RuntimeTurnStatus::Partial
    );
    std::fs::remove_dir_all(root).unwrap();
}
