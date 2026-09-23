use std::io::Cursor;

use base64::Engine;

use super::commands::{approved_decision, derived_request_id, phone_image};
use super::projection::{clip, tool_updated};
use super::*;

/// 一个真的 Runtime 控制面：命令一路走 `control_api::execute`，不打桩。调度通道的
/// 接收端留着但没人消费，所以提交的轮次停在排队状态，不会真去调模型。
struct Harness {
    workspace: PathBuf,
    state: Arc<ServerState>,
    _scheduled: tokio::sync::mpsc::UnboundedReceiver<uuid::Uuid>,
}

impl Harness {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("willdeep-mobile-gateway-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("project")).unwrap();
        let root = root.canonicalize().unwrap();
        let workspace = root.join("project");
        let events = Arc::new(EventLog::open(root.join("events.ndjson")).unwrap());
        let agents = Arc::new(AgentStore::open(root.join("agents.json")).unwrap());
        let sessions = Arc::new(
            session_store::RuntimeSessionStore::open(root.join("runtime-sessions.json"), &root)
                .unwrap(),
        );
        let (turn_scheduler, scheduled) = tokio::sync::mpsc::unbounded_channel();
        let tasks = Arc::new(
            TaskManager::open(TaskManagerOptions {
                path: root.join("tasks.json"),
                interactions_path: root.join("interactions.json"),
                home: root.clone(),
                events: events.clone(),
                agents: agents.clone(),
                sessions: sessions.clone(),
                turn_scheduler,
                runtime_url: "http://127.0.0.1:1".to_owned(),
                runtime_token: "test-token".to_owned(),
            })
            .unwrap(),
        );
        let state = Arc::new(ServerState {
            home: root.clone(),
            token: "test-token".to_owned(),
            started_at: 0,
            shutdown: watch::channel(false).0,
            events,
            tasks: tasks.clone(),
            agents,
            agent_commands: Arc::new(
                AgentCommandStore::open(root.join("agent-commands.json")).unwrap(),
            ),
            sessions,
            workspaces: tasks.workspaces.clone(),
            diff_review_lock: Arc::new(tokio::sync::Mutex::new(())),
            idempotency: Arc::new(control_api::IdempotencyStore::default()),
            local_transport: None,
            tools: tasks.tools.clone(),
            work_gate: Arc::new(RwLock::new(false)),
            kernel_store: willdeep_core::kernel_store::KernelStore::new(&root),
            mobile: Arc::new(MobileRelay::new(&root)),
        });
        state.mobile.bind(&state);
        Self {
            workspace,
            state,
            _scheduled: scheduled,
        }
    }

    fn gateway(&self) -> Gateway {
        Gateway::new(
            Arc::downgrade(&self.state),
            Arc::new(RelayShared::default()),
        )
    }

    async fn execute(&self, operation: &str, params: Value) -> Value {
        let response = control_api::execute(&self.state, ApiRequest::new(operation, params)).await;
        match response.body {
            ApiResponse::Ok { data, .. } => data,
            ApiResponse::Error { error, .. } => panic!("{operation} failed: {}", error.message),
        }
    }

    async fn register_workspace(&self) {
        self.execute(
            "workspace.register",
            serde_json::to_value(willdeep_runtime_protocol::RegisterWorkspaceParams {
                root: self.workspace.display().to_string(),
                name: Some("project".to_owned()),
                access: Default::default(),
                provider_profile: None,
                skills: Vec::new(),
                mcp_servers: Vec::new(),
            })
            .unwrap(),
        )
        .await;
    }

    async fn workspace_count(&self) -> usize {
        self.execute("workspace.list", json!({}))
            .await
            .as_array()
            .unwrap()
            .len()
    }

    /// 在已登记工作区里建一条会话（走 Runtime，不经手机）。
    async fn session(&self) -> uuid::Uuid {
        let session = self
            .execute(
                "session.create",
                json!({
                    "id": null,
                    "workspace": self.workspace.display().to_string(),
                    "profile": null,
                    "model": null,
                    "title": null,
                }),
            )
            .await;
        session["id"].as_str().unwrap().parse().unwrap()
    }

    /// 一个正在跑的任务，好让它提出审批或提问。
    async fn running_task(&self, session_id: uuid::Uuid) -> uuid::Uuid {
        let task_id = uuid::Uuid::new_v4();
        let task: RuntimeTask = serde_json::from_value(json!({
            "id": task_id,
            "session_id": session_id,
            "status": "running",
            "workspace": self.workspace,
            "profile": null,
            "pid": null,
            "created_at": 1,
            "started_at": 1,
            "completed_at": null,
            "exit_code": null,
            "error": null,
        }))
        .unwrap();
        self.state.tasks.tasks.write().await.insert(task_id, task);
        task_id
    }
}

async fn phone(gateway: &mut Gateway, command: Value) -> Vec<Value> {
    gateway.on_phone_text(&command.to_string()).await
}

fn of_type<'a>(outgoing: &'a [Value], kind: &str) -> Vec<&'a Value> {
    outgoing
        .iter()
        .filter(|envelope| envelope["type"] == kind)
        .collect()
}

fn one<'a>(outgoing: &'a [Value], kind: &str) -> &'a Value {
    let matches = of_type(outgoing, kind);
    assert_eq!(matches.len(), 1, "expected one {kind} in {outgoing:#?}");
    matches[0]
}

fn png_data_url(width: u32, height: u32) -> String {
    let mut png = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        width,
        height,
        image::Rgba([10, 20, 30, 128]),
    ))
    .write_to(&mut png, image::ImageFormat::Png)
    .unwrap();
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png.into_inner())
    )
}

/// 中继是广播：自己的回声、同一 room 里别的桌面端的回复都会进来。只认手机命令，
/// 其余一律不回——否则两个桌面端会把对方的 `ack` / `error` 当命令，无限往返。
#[test]
fn replies_and_echoes_are_never_treated_as_phone_commands() {
    for kind in [
        "ack",
        "error",
        "command.error",
        "state.snapshot",
        "session.upsert",
        "message.append",
        "message.done",
        "tool.pending",
        "tool.updated",
        "capabilities.updated",
    ] {
        let text = json!({ "id": "x", "type": kind, "payload": {} }).to_string();
        assert!(PhoneEnvelope::parse(&text).is_none(), "{kind} 不是手机命令");
    }
    let reply = json!({ "type": "workspace.list", "payload": { "workspaces": [] } });
    assert!(
        PhoneEnvelope::parse(&reply.to_string()).is_none(),
        "带 workspaces 的是回复"
    );
    let command = json!({ "id": "c1", "type": "workspace.list", "payload": {} });
    assert!(PhoneEnvelope::parse(&command.to_string()).is_some());
    assert!(PhoneEnvelope::parse("not json").is_none());
}

#[tokio::test]
async fn ignored_envelopes_produce_no_output_and_do_not_count_as_a_phone() {
    let harness = Harness::new();
    let mut gateway = harness.gateway();
    let outgoing = phone(
        &mut gateway,
        json!({ "id": "a", "type": "ack", "payload": { "type": "message.send" } }),
    )
    .await;
    assert!(outgoing.is_empty());
    assert!(!gateway.phone_active());
}

/// 白名单是字面量：手机能碰到的 Runtime 操作一条一条写死，破坏性的一个都不在。
#[test]
fn runtime_operation_whitelist_is_literal_and_excludes_destructive_operations() {
    let allowed = RUNTIME_OPERATIONS
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(allowed.len(), RUNTIME_OPERATIONS.len());
    for forbidden in [
        "session.delete",
        "session.archive",
        "session.rename",
        "session.fork",
        "session.rewind",
        "session.update_model",
        "session.update_approval_mode",
        "workspace.register",
        "workspace.ensure",
        "workspace.activate",
        "workspace.remove",
        "agent.spawn",
        "agent.stop",
        "agent.retry",
        "agent.prompt",
        "task.cancel",
        "task.diagnostics",
        "diff.revert",
        "worktree.merge",
        "kernel.ignore",
        "mobile.enable",
        "mobile.disable",
    ] {
        assert!(!allowed.contains(forbidden), "{forbidden} 不该对手机开放");
    }
    for operation in RUNTIME_OPERATIONS {
        assert!(
            willdeep_runtime_protocol::SUPPORTED_OPERATIONS.contains(operation),
            "{operation} 不是 Runtime 的公开操作"
        );
    }
}

#[tokio::test]
async fn calls_outside_the_whitelist_are_refused_before_dispatch() {
    let harness = Harness::new();
    let session_id = harness.session().await;
    let result = call::<Value>(
        &harness.state,
        "session.delete",
        json!({ "id": session_id }),
        None,
    )
    .await;
    assert!(result.is_err());
    // 会话还在：请求根本没发出去。
    harness
        .execute("session.get", json!({ "id": session_id }))
        .await;
}

#[test]
fn decisions_accept_the_mac_gateway_vocabulary() {
    assert!(approved_decision(&json!({ "approved": true })).unwrap());
    assert!(!approved_decision(&json!({ "approved": false, "decision": "approve" })).unwrap());
    for word in ["approve", "Allow", "yes", "confirm"] {
        assert!(approved_decision(&json!({ "decision": word })).unwrap());
    }
    for word in ["reject", "deny", "No"] {
        assert!(!approved_decision(&json!({ "decision": word })).unwrap());
    }
    assert!(approved_decision(&json!({ "decision": "maybe" })).is_err());
    assert!(approved_decision(&json!({})).is_err());
}

#[test]
fn derived_request_ids_are_stable_and_distinct() {
    let id = uuid::Uuid::new_v4();
    assert_eq!(derived_request_id(id), derived_request_id(id));
    assert_ne!(derived_request_id(id), id);
}

#[test]
fn phone_images_are_fitted_and_reencoded_as_jpeg() {
    let MessageAttachmentImage {
        media_type,
        width,
        height,
        data,
    } = image_parts(phone_image(&png_data_url(3_000, 1_500), 1).unwrap());
    assert_eq!(media_type, "image/jpeg");
    assert_eq!(width, crate::plugin_ai_media::MAX_IMAGE_PIXELS);
    assert_eq!(height, crate::plugin_ai_media::MAX_IMAGE_PIXELS / 2);
    assert!(
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .is_ok()
    );

    let small = image_parts(phone_image(&png_data_url(40, 30), 2).unwrap());
    assert_eq!((small.width, small.height), (40, 30));

    assert!(phone_image("https://example.com/a.png", 1).is_err());
    assert!(phone_image("data:text/plain;base64,aGk=", 1).is_err());
    assert!(phone_image("data:image/png,rawbytes", 1).is_err());
    assert!(phone_image("data:image/png;base64,@@@", 1).is_err());
}

struct MessageAttachmentImage {
    media_type: String,
    width: u32,
    height: u32,
    data: String,
}

fn image_parts(attachment: willdeep_runtime_protocol::MessageAttachment) -> MessageAttachmentImage {
    match attachment {
        willdeep_runtime_protocol::MessageAttachment::Image {
            media_type,
            data,
            width,
            height,
            ..
        } => MessageAttachmentImage {
            media_type,
            width,
            height,
            data,
        },
        other => panic!("expected an image, got {other:?}"),
    }
}

#[tokio::test]
async fn unsupported_phone_commands_use_the_mac_gateway_wording() {
    let harness = Harness::new();
    let mut gateway = harness.gateway();
    for kind in [
        "patch.decide",
        "diff.get",
        "file.read",
        "job.kill",
        "queue.update",
        "push.register",
    ] {
        let outgoing = phone(
            &mut gateway,
            json!({ "id": "cmd-1", "type": kind, "payload": {} }),
        )
        .await;
        let error = one(&outgoing, "error");
        assert_eq!(error["id"], "cmd-1");
        assert_eq!(
            error["payload"]["message"],
            format!("Unsupported mobile command: {kind}.")
        );
    }
}

#[tokio::test]
async fn new_sessions_are_only_created_in_registered_workspaces() {
    let harness = Harness::new();
    harness.register_workspace().await;
    let mut gateway = harness.gateway();
    let stray = harness.workspace.parent().unwrap().join("elsewhere");
    std::fs::create_dir_all(&stray).unwrap();

    let refused = phone(
        &mut gateway,
        json!({
            "id": "c1",
            "type": "session.create",
            "payload": { "workspace_path": stray.display().to_string() },
        }),
    )
    .await;
    let error = one(&refused, "error");
    assert!(
        error["payload"]["message"]
            .as_str()
            .unwrap()
            .contains("not registered"),
        "{error}"
    );
    assert_eq!(harness.workspace_count().await, 1, "手机不能顺手登记新目录");

    let created = phone(
        &mut gateway,
        json!({
            "id": "c2",
            "type": "session.create",
            "payload": { "workspace_path": harness.workspace.display().to_string() },
        }),
    )
    .await;
    let upsert = one(&created, "session.upsert");
    assert_eq!(upsert["id"], "c2");
    let session_id: uuid::Uuid = upsert["payload"]["session"]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(gateway.selected, Some(session_id));
    assert_eq!(upsert["payload"]["session"]["is_active"], true);
}

#[tokio::test]
async fn message_send_queues_one_turn_owned_by_the_phone_even_when_retried() {
    let harness = Harness::new();
    harness.register_workspace().await;
    let session_id = harness.session().await;
    let mut gateway = harness.gateway();
    let envelope_id = uuid::Uuid::new_v4();
    let command = json!({
        "id": envelope_id.to_string(),
        "type": "message.send",
        "session_id": session_id.to_string(),
        "payload": {
            "text": "run the tests",
            // 这几项手机上改不了，必须被忽略。
            "approval_mode": "full_access",
            "model": "someone-else",
        },
    });

    let outgoing = phone(&mut gateway, command.clone()).await;
    let ack = one(&outgoing, "ack");
    assert_eq!(ack["id"], envelope_id.to_string());
    assert_eq!(ack["session_id"], session_id.to_string());
    let echo = one(&outgoing, "message.append");
    assert_eq!(echo["payload"]["role"], "user");
    assert_eq!(echo["payload"]["content"], "run the tests");
    let snapshot = one(&outgoing, "state.snapshot");
    assert_eq!(
        snapshot["payload"]["active_session_id"],
        session_id.to_string()
    );
    let messages = snapshot["payload"]["messages"].as_array().unwrap();
    assert!(
        messages
            .iter()
            .any(|message| message["content"] == "run the tests"),
        "快照里要带着刚发的消息，否则 Android 下一次心跳就把它冲掉：{messages:#?}"
    );

    // 手机重发同一条命令：命中幂等缓存，不会排两轮，也不会回显两次。
    let retried = phone(&mut gateway, command).await;
    one(&retried, "ack");
    let turns = harness
        .execute("turn.list", json!({ "session_id": session_id }))
        .await;
    let turns = turns.as_array().unwrap();
    assert_eq!(turns.len(), 1, "{turns:#?}");
    assert_eq!(turns[0]["request_id"], envelope_id.to_string());
    assert_eq!(gateway.tails[&session_id].len(), 1);

    let session = harness
        .execute("session.get", json!({ "id": session_id }))
        .await;
    assert!(session["model"].is_null(), "手机不能改模型：{session}");

    let claimed = harness
        .state
        .sessions
        .claim_next(session_id)
        .unwrap()
        .expect("the queued turn");
    assert_eq!(claimed.request.prompt, "run the tests");
    assert!(
        claimed
            .request
            .origin_client
            .as_deref()
            .is_some_and(|origin| origin.starts_with("mobile:")),
        "手机发起的轮次要记在手机名下，审批才会弹回手机"
    );
}

#[tokio::test]
async fn message_send_with_a_workspace_path_opens_a_session_there() {
    let harness = Harness::new();
    harness.register_workspace().await;
    let mut gateway = harness.gateway();
    let outgoing = phone(
        &mut gateway,
        json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "type": "message.send",
            "payload": {
                "text": "hello",
                "workspace_path": harness.workspace.display().to_string(),
            },
        }),
    )
    .await;
    let ack = one(&outgoing, "ack");
    let session_id: uuid::Uuid = ack["session_id"].as_str().unwrap().parse().unwrap();
    assert_eq!(gateway.selected, Some(session_id));
    let session = harness
        .execute("session.get", json!({ "id": session_id }))
        .await;
    assert_eq!(
        session["workspace"],
        harness.workspace.display().to_string()
    );
}

#[tokio::test]
async fn approvals_are_allowed_once_even_when_always_allow_is_offered() {
    let harness = Harness::new();
    harness.register_workspace().await;
    let session_id = harness.session().await;
    let task_id = harness.running_task(session_id).await;
    let receiver = harness
        .state
        .tasks
        .create_interaction(
            task_id,
            InteractionKind::Approval {
                description: "Run `cargo test`?".to_owned(),
                always_allow_available: true,
            },
        )
        .await
        .unwrap();
    let mut gateway = harness.gateway();

    let snapshot = phone(
        &mut gateway,
        json!({ "id": "s", "type": "session.list", "payload": {} }),
    )
    .await;
    let pending = one(&snapshot, "state.snapshot")["payload"]["pending_tools"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["session_id"], session_id.to_string());
    assert_eq!(pending[0]["title"], "Run `cargo test`?");
    let approval_id = pending[0]["id"].as_str().unwrap().to_owned();

    let outgoing = phone(
        &mut gateway,
        json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "type": "tool.decide",
            "session_id": session_id.to_string(),
            "payload": { "id": approval_id, "decision": "approve", "approved": true },
        }),
    )
    .await;
    one(&outgoing, "ack");
    let updated = one(&outgoing, "tool.updated");
    assert_eq!(updated["payload"]["status"], "approved");
    assert!(matches!(
        receiver.await.unwrap(),
        InteractionResolution::AllowOnce
    ));

    let again = phone(
        &mut gateway,
        json!({
            "id": "again",
            "type": "tool.decide",
            "payload": { "id": approval_id, "approved": false },
        }),
    )
    .await;
    assert_eq!(
        one(&again, "error")["payload"]["message"],
        "This approval is no longer pending."
    );
}

#[tokio::test]
async fn questions_need_an_answer_before_they_are_approved() {
    let harness = Harness::new();
    harness.register_workspace().await;
    let session_id = harness.session().await;
    let task_id = harness.running_task(session_id).await;
    let receiver = harness
        .state
        .tasks
        .create_interaction(
            task_id,
            InteractionKind::Question {
                question: "Which branch?".to_owned(),
                options: vec!["main".to_owned(), "develop".to_owned()],
                multi_select: false,
            },
        )
        .await
        .unwrap();
    let question_id = harness.state.tasks.pending_interactions().await[0].id;
    let mut gateway = harness.gateway();

    let missing = phone(
        &mut gateway,
        json!({
            "id": "q1",
            "type": "tool.decide",
            "payload": { "id": question_id.to_string(), "approved": true },
        }),
    )
    .await;
    assert_eq!(
        one(&missing, "error")["payload"]["message"],
        "An answer is required."
    );

    let answered = phone(
        &mut gateway,
        json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "type": "tool.decide",
            "payload": { "id": question_id.to_string(), "approved": true, "answer": " main " },
        }),
    )
    .await;
    assert_eq!(
        one(&answered, "tool.updated")["payload"]["status"],
        "answered"
    );
    match receiver.await.unwrap() {
        InteractionResolution::Answer(answer) => assert_eq!(answer.as_deref(), Some("main")),
        _ => panic!("expected an answer"),
    }
}

/// 「需要你处理」是整个 Runtime 的，不只是选中的会话；没选过会话时，快照先落在
/// 有人等着的那一条上。
#[tokio::test]
async fn snapshots_carry_every_pending_gate_and_default_to_the_waiting_session() {
    let harness = Harness::new();
    harness.register_workspace().await;
    let idle = harness.session().await;
    let waiting = harness.session().await;
    // 让空闲的那条更「新」，确认选中不是按更新时间碰巧选对的。
    phone(
        &mut harness.gateway(),
        json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "type": "message.send",
            "session_id": idle.to_string(),
            "payload": { "text": "touch" },
        }),
    )
    .await;
    let task_id = harness.running_task(waiting).await;
    let _receiver = harness
        .state
        .tasks
        .create_interaction(
            task_id,
            InteractionKind::Approval {
                description: "Write file\nsrc/main.rs".to_owned(),
                always_allow_available: false,
            },
        )
        .await
        .unwrap();

    let mut gateway = harness.gateway();
    let outgoing = phone(
        &mut gateway,
        json!({ "id": "s1", "type": "session.list", "payload": {} }),
    )
    .await;
    let snapshot = one(&outgoing, "state.snapshot");
    assert_eq!(snapshot["id"], "s1");
    let payload = &snapshot["payload"];
    assert_eq!(payload["sessions"].as_array().unwrap().len(), 2);
    assert_eq!(payload["active_session_id"], waiting.to_string());
    let gate = &payload["pending_tools"][0];
    assert_eq!(gate["title"], "Write file");
    assert_eq!(gate["summary"], "src/main.rs");
    assert_eq!(gate["session_id"], waiting.to_string());
}

#[tokio::test]
async fn runtime_events_become_phone_envelopes() {
    let harness = Harness::new();
    harness.register_workspace().await;
    let session_id = harness.session().await;
    let task_id = harness.running_task(session_id).await;
    let mut gateway = harness.gateway();
    gateway.record_phone_command();

    let text = gateway
        .translate(
            &harness.state,
            RuntimeEvent {
                sequence: 1,
                timestamp: 100,
                kind: "task.output".to_owned(),
                message: format!(
                    "task_id={task_id} {}",
                    json!({ "type": "assistant_text", "text": "All tests pass." })
                ),
            },
        )
        .await;
    let append = one(&text, "message.append");
    assert_eq!(append["session_id"], session_id.to_string());
    assert_eq!(append["payload"]["role"], "assistant");
    assert_eq!(append["payload"]["content"], "All tests pass.");
    let done = one(&text, "message.done");
    assert_eq!(done["payload"]["message_id"], append["payload"]["id"]);

    // token 级增量和工具细节不上中继。
    let delta = gateway
        .translate(
            &harness.state,
            RuntimeEvent {
                sequence: 2,
                timestamp: 100,
                kind: "task.output".to_owned(),
                message: format!(
                    "task_id={task_id} {}",
                    json!({ "type": "assistant_text_delta", "text": "All" })
                ),
            },
        )
        .await;
    assert!(delta.is_empty());

    let receiver = harness
        .state
        .tasks
        .create_interaction(
            task_id,
            InteractionKind::Approval {
                description: "Run `rm -rf build`?".to_owned(),
                always_allow_available: false,
            },
        )
        .await
        .unwrap();
    drop(receiver);
    let interaction = harness.state.tasks.pending_interactions().await[0].id;
    let pending = gateway
        .translate(
            &harness.state,
            RuntimeEvent {
                sequence: 3,
                timestamp: 101,
                kind: "task.waiting_approval".to_owned(),
                message: format!("task_id={task_id} interaction_id={interaction}"),
            },
        )
        .await;
    let card = one(&pending, "tool.pending");
    assert_eq!(card["payload"]["id"], interaction.to_string());
    assert_eq!(card["session_id"], session_id.to_string());

    let resolved = gateway
        .translate(
            &harness.state,
            RuntimeEvent {
                sequence: 4,
                timestamp: 102,
                kind: "task.interaction_resolved".to_owned(),
                message: format!("task_id={task_id} interaction_id={interaction}"),
            },
        )
        .await;
    assert_eq!(
        one(&resolved, "tool.updated")["payload"]["status"],
        "resolved"
    );

    let finished = gateway
        .translate(
            &harness.state,
            RuntimeEvent {
                sequence: 5,
                timestamp: 103,
                kind: "turn.completed".to_owned(),
                message: format!(
                    "session_id={session_id} turn_id={} task_id={task_id} exit_code=0 error=",
                    uuid::Uuid::new_v4()
                ),
            },
        )
        .await;
    assert_eq!(
        one(&finished, "session.upsert")["payload"]["session"]["id"],
        session_id.to_string()
    );
    // 轮次结束了，但会话文件里还没有这条回复：宽限期内它仍留在快照里。
    let tail = &gateway.tails[&session_id];
    assert!(tail.iter().all(|message| message.settled_at.is_some()));

    for renamed in ["session.renamed", "session.archived"] {
        gateway.snapshot_dirty = false;
        let quiet = gateway
            .translate(
                &harness.state,
                RuntimeEvent {
                    sequence: 6,
                    timestamp: 104,
                    kind: renamed.to_owned(),
                    message: format!("session_id={session_id}"),
                },
            )
            .await;
        assert!(quiet.is_empty());
        assert!(gateway.snapshot_dirty, "{renamed} 要合并进下一份快照");
    }
}

/// 手机不在场：不为没人收的信封去查审批、扫摘要；但实时尾巴照记，手机回来时
/// 补的那份快照里有这段时间的回复。
#[tokio::test]
async fn an_absent_phone_gets_no_envelopes_but_the_tail_keeps_up() {
    let harness = Harness::new();
    harness.register_workspace().await;
    let session_id = harness.session().await;
    let task_id = harness.running_task(session_id).await;
    let mut gateway = harness.gateway();
    assert!(!gateway.phone_active());

    for (kind, message) in [
        (
            "task.output",
            format!(
                "task_id={task_id} {}",
                json!({ "type": "assistant_text", "text": "Done while you were away." })
            ),
        ),
        (
            "turn.completed",
            format!("session_id={session_id} task_id={task_id} exit_code=0 error="),
        ),
        (
            "task.waiting_approval",
            format!("task_id={task_id} interaction_id={}", uuid::Uuid::new_v4()),
        ),
    ] {
        let outgoing = gateway
            .on_runtime_event(RuntimeEvent {
                sequence: 1,
                timestamp: 100,
                kind: kind.to_owned(),
                message,
            })
            .await;
        assert!(outgoing.is_empty(), "{kind} 不该推给不在场的手机");
    }
    assert_eq!(
        gateway.tails[&session_id][0].content,
        "Done while you were away."
    );
}

/// 已经落进会话文件的消息不能在快照里出现两次。
#[tokio::test]
async fn persisted_messages_leave_the_live_tail() {
    let harness = Harness::new();
    harness.register_workspace().await;
    let session_id = harness.session().await;
    let mut gateway = harness.gateway();
    gateway.selected = Some(session_id);
    gateway.push_user_echo(session_id, uuid::Uuid::new_v4(), "ship it", 0);

    let store = willdeep_core::SessionStore::new(&harness.state.home);
    let mut session = store.load(session_id).unwrap();
    session
        .messages
        .push(willdeep_core::Message::user("ship it"));
    session
        .messages
        .push(willdeep_core::Message::assistant("Shipped.", Vec::new()));
    store.save(&mut session).unwrap();

    let outgoing = phone(
        &mut gateway,
        json!({ "id": "s", "type": "session.list", "payload": {} }),
    )
    .await;
    let messages = one(&outgoing, "state.snapshot")["payload"]["messages"]
        .as_array()
        .unwrap()
        .clone();
    let contents = messages
        .iter()
        .map(|message| message["content"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(contents, ["ship it", "Shipped."], "{messages:#?}");
    assert!(!gateway.tails.contains_key(&session_id));
}

/// 打码不能把 Markdown 压扁：换行、缩进和代码块要原样保住，凭据照样打掉。
#[test]
fn redaction_keeps_the_message_layout() {
    let message =
        "Set it like this:\n\n```sh\n    export API_KEY=sk-live-abcdef0123456789\n```\nDone.";
    let clipped = clip(message, 8_000);
    assert!(
        clipped.contains("\n\n```sh\n    export API_KEY=[REDACTED]\n```\nDone."),
        "{clipped}"
    );
    assert!(!clipped.contains("sk-live"));
    assert_eq!(clip("  padded  ", 100), "padded");
    assert_eq!(clip("", 100), "");
    assert_eq!(clip("abcdef", 3), "abc…");
}

/// Android 的 `optString` 会把 JSON `null` 读成字符串 `"null"`：没有会话就省略键。
#[test]
fn unknown_sessions_are_omitted_rather_than_null() {
    let envelope = tool_updated(uuid::Uuid::new_v4(), "resolved", None);
    assert!(envelope.get("session_id").is_none());
    assert!(envelope["payload"].get("session_id").is_none());
}

fn write_credentials(home: &Path, relay_base_url: &str) {
    let credentials = home.join("mobile-relay.toml");
    std::fs::write(
        &credentials,
        format!(
            "relay_base_url = \"{relay_base_url}\"\nroom = \"wd-0123456789abcdef0123456789abcdef\"\ntoken = \"fedcba9876543210fedcba9876543210\"\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&credentials, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

type PhoneSocket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

/// 从假中继上读，直到读到某种类型的信封（其余的跳过）。
async fn next_of_type(socket: &mut PhoneSocket, kind: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match socket.next().await {
                Some(Ok(WebSocketMessage::Text(text))) => {
                    let value: Value = serde_json::from_str(&text).unwrap();
                    if value["type"] == kind {
                        return value;
                    }
                }
                Some(Ok(_)) => {}
                other => panic!("relay closed while waiting for {kind}: {other:?}"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {kind}"))
}

/// 端到端：网关连上中继（这里是本机的假中继，扮演手机），带着 token 鉴权；手机拿到
/// 快照、收到审批卡、批准之后卡片撤回；关掉中继后连接断开。
// 握手回调的 `Err` 类型是 tungstenite 定死的 `http::Response`，不是这里能改的。
#[allow(clippy::result_large_err)]
#[tokio::test]
async fn the_gateway_serves_a_phone_through_the_relay() {
    let harness = Harness::new();
    harness.register_workspace().await;
    let session_id = harness.session().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    write_credentials(&harness.state.home, &format!("http://{address}"));

    harness.state.mobile.enable().unwrap();
    let (stream, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut seen_request = None;
    let mut socket = tokio_tungstenite::accept_hdr_async(
        stream,
        |request: &http::Request<()>, response: http::Response<()>| {
            seen_request = Some((
                request.uri().path().to_owned(),
                request
                    .headers()
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned),
            ));
            Ok(response)
        },
    )
    .await
    .unwrap();
    let (path, authorization) = seen_request.unwrap();
    assert_eq!(path, "/ws/broadcast/wd-0123456789abcdef0123456789abcdef");
    assert_eq!(
        authorization.as_deref(),
        Some("Bearer fedcba9876543210fedcba9876543210")
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while !harness.state.mobile.status().connected {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("status reports the connection");

    socket
        .send(WebSocketMessage::Text(
            json!({ "id": "hb", "type": "session.list", "payload": {} })
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let snapshot = next_of_type(&mut socket, "state.snapshot").await;
    assert_eq!(snapshot["id"], "hb");
    assert_eq!(
        snapshot["payload"]["active_session_id"],
        session_id.to_string()
    );
    assert!(harness.state.mobile.status().phone_active);

    // 手机在场时，Runtime 里新冒出来的审批直接推过去。
    let task_id = harness.running_task(session_id).await;
    let receiver = harness
        .state
        .tasks
        .create_interaction(
            task_id,
            InteractionKind::Approval {
                description: "Run `cargo test`?".to_owned(),
                always_allow_available: false,
            },
        )
        .await
        .unwrap();
    let card = next_of_type(&mut socket, "tool.pending").await;
    assert_eq!(card["session_id"], session_id.to_string());
    let approval_id = card["payload"]["id"].as_str().unwrap().to_owned();

    let decide_id = uuid::Uuid::new_v4().to_string();
    socket
        .send(WebSocketMessage::Text(
            json!({
                "id": decide_id,
                "type": "tool.decide",
                "payload": { "id": approval_id, "decision": "approve" },
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let ack = next_of_type(&mut socket, "ack").await;
    assert_eq!(ack["id"], decide_id);
    let updated = next_of_type(&mut socket, "tool.updated").await;
    assert_eq!(updated["payload"]["id"], approval_id);
    assert!(matches!(
        receiver.await.unwrap(),
        InteractionResolution::AllowOnce
    ));

    harness.state.mobile.disable().unwrap();
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match socket.next().await {
                None | Some(Err(_)) | Some(Ok(WebSocketMessage::Close(_))) => return,
                Some(Ok(_)) => {}
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "关掉中继后连接要断开");
    assert!(!harness.state.mobile.status().connected);
}

#[tokio::test]
async fn relay_switch_is_persisted_and_reported_without_the_token() {
    let harness = Harness::new();
    // 指向本机一个没人监听的端口：打开中继会起连接循环，测试不能去碰公网中继。
    write_credentials(&harness.state.home, "http://127.0.0.1:9");
    let relay = &harness.state.mobile;
    assert!(!relay.status().enabled);

    let enabled = relay.enable().unwrap();
    assert!(enabled.status.enabled);
    assert!(enabled.pairing_url.contains("t="));
    let status_json = serde_json::to_string(&relay.status()).unwrap();
    let token = enabled
        .pairing_url
        .split("t=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();
    assert!(!status_json.contains(token), "状态里不能带 token");
    assert!(
        crate::mobile::RelayCredentials::load(&harness.state.home)
            .unwrap()
            .unwrap()
            .enabled()
    );

    let status = relay.disable().unwrap();
    assert!(!status.enabled && !status.connected);
    assert!(
        !crate::mobile::RelayCredentials::load(&harness.state.home)
            .unwrap()
            .unwrap()
            .enabled()
    );
}
