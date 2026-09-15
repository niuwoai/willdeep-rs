use super::*;

#[cfg(unix)]
#[test]
fn saved_session_files_are_private_after_create_and_replace() {
    use std::os::unix::fs::PermissionsExt;
    let (root, store, mut session) = fixture();
    let path = store.path(session.id);
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    session.messages.push(Message::user("private history"));
    store.save(&mut session).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        store
            .load(session.id)
            .unwrap()
            .messages
            .last()
            .unwrap()
            .content,
        "private history"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn an_explicit_fork_has_an_independent_identity_and_preserves_the_source() {
    let (root, store, source) = fixture();
    let original = std::fs::read(store.path(source.id)).unwrap();
    let mut fork = source.fork_snapshot();
    assert_ne!(fork.id, source.id);
    fork.messages.push(Message::user("fork-only message"));
    store.save(&mut fork).unwrap();
    assert_eq!(std::fs::read(store.path(source.id)).unwrap(), original);
    assert_eq!(
        store
            .load(fork.id)
            .unwrap()
            .messages
            .last()
            .unwrap()
            .content,
        "fork-only message"
    );
    std::fs::remove_dir_all(root).unwrap();
}

fn fixture() -> (PathBuf, SessionStore, Session) {
    let root = std::env::temp_dir().join(format!("session-concurrency-{}", Uuid::new_v4()));
    let store = SessionStore::new(&root);
    let mut session = Session::new(root.clone(), None, "initial");
    store.save(&mut session).unwrap();
    (root, store, session)
}

#[test]
fn stale_metadata_save_preserves_the_entire_new_execution_snapshot() {
    let (root, store, mut stale) = fixture();
    store
        .update(stale.id, |session| {
            session
                .messages
                .push(Message::assistant("new result", Vec::new()));
            session.runtime_event_cursor = 7;
            session.compression_generation = 2;
            session.execution_checkpoint = Some(crate::checkpoint::CheckpointMetadata::default());
        })
        .unwrap();
    stale.title = "user title".into();
    stale.title_source = TitleSource::User;
    store.save(&mut stale).unwrap();
    let saved = store.load(stale.id).unwrap();
    assert_eq!(saved.title, "user title");
    assert_eq!(saved.messages.last().unwrap().content, "new result");
    assert_eq!(saved.runtime_event_cursor, 7);
    assert_eq!(saved.compression_generation, 2);
    assert!(saved.execution_checkpoint.is_some());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn competing_execution_snapshots_are_rejected_without_mutating_disk() {
    let (root, store, mut stale) = fixture();
    store
        .update(stale.id, |session| {
            session
                .messages
                .push(Message::assistant("winner", Vec::new()));
        })
        .unwrap();
    let before = std::fs::read(store.path(stale.id)).unwrap();
    stale.messages.push(Message::assistant("stale", Vec::new()));
    assert!(matches!(
        store.save(&mut stale),
        Err(SessionError::ConcurrentUpdate("execution"))
    ));
    assert_eq!(std::fs::read(store.path(stale.id)).unwrap(), before);
    std::fs::remove_dir_all(root).unwrap();
}

/// 书签类字段（事件游标）要能写进一份别人刚改过的会话。
///
/// 现场：升级 CLI 后第一次开 TUI，或守护进程正在跑这一轮。对方改的是对话本身，
/// 我们改的只是「读到第几条事件」——这两件事不该互相判成执行冲突。
#[test]
fn a_bookmark_write_survives_a_concurrent_execution_rewrite() {
    let (root, store, mut stale) = fixture();
    // 另一个写者（守护进程恢复 / 执行回合）改了执行状态。
    store
        .update(stale.id, |session| {
            session
                .messages
                .push(Message::assistant("daemon recovery", Vec::new()));
        })
        .unwrap();

    // 我们只动书签：直接存就该成功，并且自动接上对方写的那份对话。
    stale.runtime_event_cursor = 42;
    store.save(&mut stale).expect("书签写入不该被判成执行冲突");
    assert_eq!(stale.messages.last().unwrap().content, "daemon recovery");

    let reloaded = store.load(stale.id).unwrap();
    assert_eq!(reloaded.runtime_event_cursor, 42);
    assert_eq!(reloaded.messages.last().unwrap().content, "daemon recovery");
    std::fs::remove_dir_all(root).unwrap();
}

/// 会话被运行时接管（只置 `runtime_managed`）时，前台刚跑完的那一轮不能丢。
#[test]
fn a_finished_turn_is_not_lost_when_the_runtime_flips_an_ownership_flag() {
    let (root, store, mut stale) = fixture();
    store
        .update(stale.id, |session| session.runtime_managed = true)
        .unwrap();

    stale
        .messages
        .push(Message::assistant("turn result", Vec::new()));
    store
        .save(&mut stale)
        .expect("一个路由标志位不该让整轮结果落不了盘");

    let reloaded = store.load(stale.id).unwrap();
    assert!(reloaded.runtime_managed, "运行时置的位必须保住");
    assert_eq!(reloaded.messages.last().unwrap().content, "turn result");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn deleted_session_cannot_be_resurrected_by_a_loaded_writer() {
    let (root, store, mut stale) = fixture();
    assert!(store.delete(stale.id).unwrap());
    stale
        .messages
        .push(Message::assistant("late result", Vec::new()));
    assert!(
        matches!(store.save(&mut stale), Err(SessionError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound)
    );
    assert!(!store.path(stale.id).exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn checkpoint_only_updates_conflict_with_stale_message_writers() {
    let (root, store, mut stale) = fixture();
    store
        .update(stale.id, |session| {
            session.execution_checkpoint = Some(crate::checkpoint::CheckpointMetadata {
                pending_call_ids: vec!["possibly-executed".into()],
                ..Default::default()
            });
        })
        .unwrap();
    stale
        .messages
        .push(Message::assistant("stale result", Vec::new()));
    assert!(matches!(
        store.save(&mut stale),
        Err(SessionError::ConcurrentUpdate("execution"))
    ));
    assert_eq!(
        store
            .load(stale.id)
            .unwrap()
            .execution_checkpoint
            .unwrap()
            .pending_call_ids,
        vec!["possibly-executed"]
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn refreshing_execution_allows_continuation_without_losing_metadata_conflicts() {
    let (root, store, mut local) = fixture();
    store
        .update(local.id, |session| {
            session.title = "remote title".into();
            session.title_source = TitleSource::User;
            session
                .messages
                .push(Message::assistant("checkpoint", Vec::new()));
            session.execution_checkpoint = Some(Default::default());
        })
        .unwrap();
    store.refresh_execution(&mut local).unwrap();
    local.messages.push(Message::user("continue"));
    store.save(&mut local).unwrap();
    assert_eq!(local.title, "remote title");
    assert_eq!(local.messages.last().unwrap().content, "continue");
    assert!(
        local
            .messages
            .iter()
            .any(|message| message.content == "checkpoint")
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn deserialized_snapshot_without_a_baseline_cannot_overwrite_an_existing_session() {
    let (root, store, original) = fixture();
    let mut untracked: Session =
        serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
    untracked
        .messages
        .push(Message::assistant("untracked result", Vec::new()));
    let before = std::fs::read(store.path(original.id)).unwrap();
    assert!(matches!(
        store.save(&mut untracked),
        Err(SessionError::ConcurrentUpdate("missing baseline"))
    ));
    assert_eq!(std::fs::read(store.path(original.id)).unwrap(), before);
    std::fs::remove_dir_all(root).unwrap();
}
