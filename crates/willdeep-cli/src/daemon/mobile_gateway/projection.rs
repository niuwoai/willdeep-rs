//! Runtime 状态 → 手机端信封：快照、事件翻译、信封构造。
//!
//! 字段名与取值沿用 macOS 桌面端（`AgentMobileGatewayBridge.swift`），Android 两边
//! 都能直接吃。可能为空的 id 一律**省略键**而不是写 `null`：Android 的
//! `optString` 会把 JSON `null` 读成字符串 `"null"`。

use willdeep_core::session::{SessionDigest, SessionStore, format_iso8601};
use willdeep_runtime_protocol::{
    PendingApproval, PendingQuestion, RuntimeSession, RuntimeTask, SessionStatus,
};

use super::*;

/// 快照里的会话条数上限（选中的那条总在其中）。
const MAX_SNAPSHOT_SESSIONS: usize = 50;
/// 快照里选中会话的消息条数上限，与 macOS 桌面端每会话 40 条一致。
const MAX_SNAPSHOT_MESSAGES: usize = 40;
const MAX_MESSAGE_CHARS: usize = 8_000;
const MAX_APPROVAL_CHARS: usize = 600;
const MAX_TITLE_CHARS: usize = 120;
/// 轮次结束后，实时尾巴里还没在历史里出现的消息再留多久。
const TAIL_GRACE: Duration = Duration::from_secs(30);
const MAX_HISTORY_CACHE: usize = 16;
const MAX_TASK_CACHE: usize = 4_096;

pub(super) fn envelope(
    kind: &str,
    id: Option<&str>,
    session_id: Option<uuid::Uuid>,
    payload: Value,
) -> Value {
    let mut value = json!({
        "id": id.map_or_else(|| uuid::Uuid::new_v4().to_string(), str::to_owned),
        "type": kind,
        "payload": payload,
        "ts": format_iso8601(now()),
    });
    if let Some(session_id) = session_id {
        value["session_id"] = json!(session_id.to_string());
    }
    value
}

pub(super) fn reply(
    id: Option<&str>,
    kind: &str,
    session_id: Option<uuid::Uuid>,
    payload: Value,
) -> Value {
    envelope(kind, id, session_id, payload)
}

pub(super) fn ack(
    id: Option<&str>,
    command: &str,
    session_id: Option<uuid::Uuid>,
    message: Option<&str>,
) -> Value {
    let mut payload = json!({ "type": command });
    if let Some(message) = message {
        payload["message"] = json!(message);
    }
    envelope("ack", id, session_id, payload)
}

pub(super) fn command_error(id: Option<&str>, command: &str, message: &str) -> Value {
    envelope(
        "error",
        id,
        None,
        json!({ "type": command, "message": message }),
    )
}

pub(super) fn tool_updated(id: uuid::Uuid, status: &str, session_id: Option<uuid::Uuid>) -> Value {
    let mut payload = json!({ "id": id.to_string(), "status": status });
    insert_session(&mut payload, session_id);
    envelope("tool.updated", None, session_id, payload)
}

fn insert_session(payload: &mut Value, session_id: Option<uuid::Uuid>) {
    if let Some(session_id) = session_id {
        payload["session_id"] = json!(session_id.to_string());
    }
}

pub(super) fn workspace_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Workspace")
        .to_owned()
}

/// 凭据打码后按字符截断。审批描述、提问和消息正文都要过这一道再上中继。
pub(super) fn clip(text: &str, max_chars: usize) -> String {
    let redacted = redact_preserving_layout(text.trim());
    let mut chars = redacted.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// `judge::redact_credentials` 是给命令行用的：按空白切词、再用单个空格拼回去，会把
/// 助手回复里的换行、缩进和代码块全压扁。它对每个词恰好产出一个词，所以这里按原文
/// 的空白把打码后的词逐个放回去。词数对不上（将来它的行为变了）就退回压扁的版本——
/// 宁可难看，也不能让没打码的词漏出去。
fn redact_preserving_layout(text: &str) -> String {
    let redacted = willdeep_core::judge::redact_credentials(text);
    if redacted.split(' ').filter(|word| !word.is_empty()).count()
        != text.split_whitespace().count()
    {
        return redacted;
    }
    let mut replacements = redacted.split(' ').filter(|word| !word.is_empty());
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(|character: char| !character.is_whitespace()) {
        output.push_str(&rest[..start]);
        rest = &rest[start..];
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        output.push_str(replacements.next().unwrap_or("[REDACTED]"));
        rest = &rest[end..];
    }
    output.push_str(rest);
    output
}

fn is_responding(session: &RuntimeSession) -> bool {
    session.active_turn_id.is_some()
        || matches!(
            session.status,
            SessionStatus::Queued
                | SessionStatus::Running
                | SessionStatus::WaitingApproval
                | SessionStatus::WaitingAnswer
        )
}

pub(super) fn session_json(
    session: &RuntimeSession,
    digest: Option<&SessionDigest>,
    active: bool,
) -> Value {
    let workspace_path = session.workspace.clone().unwrap_or_default();
    let title = digest
        .map(|digest| digest.title.trim())
        .filter(|title| !title.is_empty())
        .map(str::to_owned)
        .or_else(|| digest.and_then(|digest| digest.preview.clone()))
        .unwrap_or_else(|| workspace_name(&workspace_path));
    json!({
        "id": session.id.to_string(),
        "title": clip(&title, MAX_TITLE_CHARS),
        "workspace_name": workspace_name(&workspace_path),
        "workspace_path": workspace_path,
        "message_count": digest.map_or(0, |digest| digest.message_count),
        "is_active": active,
        "is_responding": is_responding(session),
        "updated_at": format_iso8601(session.updated_at),
    })
}

fn approval_json(approval: &PendingApproval, session_id: Option<uuid::Uuid>) -> Value {
    let description = clip(&approval.description, MAX_APPROVAL_CHARS);
    let (title, summary) = match description.split_once('\n') {
        Some((title, rest)) => (title.trim().to_owned(), rest.trim().to_owned()),
        None => (description.clone(), String::new()),
    };
    let mut value = json!({
        "id": approval.id.to_string(),
        "title": clip(&title, MAX_TITLE_CHARS),
        "summary": summary,
        "tool_name": "approval",
        "input_preview": "",
        "requires_answer": false,
        "requires_confirmation": false,
        "status": "pending",
    });
    insert_session(&mut value, session_id);
    value
}

fn question_json(question: &PendingQuestion, session_id: Option<uuid::Uuid>) -> Value {
    let options = question
        .options
        .iter()
        .map(|option| clip(option, MAX_TITLE_CHARS))
        .collect::<Vec<_>>();
    let mut value = json!({
        "id": question.id.to_string(),
        "title": clip(&question.question, MAX_APPROVAL_CHARS),
        "summary": options.join(" / "),
        "tool_name": "ask_user",
        "kind": "ask_user",
        "input_preview": options.join("\n"),
        "requires_answer": true,
        "requires_confirmation": false,
        "status": "pending",
    });
    insert_session(&mut value, session_id);
    value
}

fn message_json(
    id: &str,
    role: &str,
    content: &str,
    created_at: Option<u64>,
    session_id: uuid::Uuid,
) -> Value {
    let mut value = json!({
        "id": id,
        "role": role,
        "content": content,
        "session_id": session_id.to_string(),
        "is_streaming": false,
    });
    if let Some(created_at) = created_at {
        value["created_at"] = json!(format_iso8601(created_at));
    }
    value
}

fn event_field(message: &str, key: &str) -> Option<uuid::Uuid> {
    message
        .split_whitespace()
        .find_map(|part| part.strip_prefix(key)?.strip_prefix('='))
        .and_then(|value| value.parse().ok())
}

/// 选中会话的历史投影：与 Web 端会话详情同一份 `conversation::project`。
fn project_history(home: &Path, session_id: uuid::Uuid) -> Vec<Value> {
    let Ok(session) = SessionStore::new(home).load(session_id) else {
        return Vec::new();
    };
    let items =
        willdeep_core::conversation::project(&session.messages, session.current_plan.as_ref());
    let skip = items.len().saturating_sub(MAX_SNAPSHOT_MESSAGES);
    items
        .into_iter()
        .enumerate()
        .skip(skip)
        .filter_map(|(index, item)| {
            let content = if item.content.trim().is_empty() {
                if item.attachment_count == 0 {
                    return None;
                }
                format!("[{} attachments]", item.attachment_count)
            } else {
                clip(&item.content, MAX_MESSAGE_CHARS)
            };
            Some(message_json(
                &format!("{session_id}:{index}"),
                item.role,
                &content,
                None,
                session_id,
            ))
        })
        .collect()
}

impl Gateway {
    /// 一份完整快照。`id` 是手机那条 `session.list` 的信封 id；主动推送时为 `None`。
    pub(super) async fn snapshot(&mut self, server: &ServerState, id: Option<&str>) -> Value {
        self.snapshot_dirty = false;
        match self.snapshot_payload(server).await {
            Ok(payload) => reply(id, "state.snapshot", self.selected, payload),
            Err(error) => command_error(id, "session.list", &error.0),
        }
    }

    async fn snapshot_payload(&mut self, server: &ServerState) -> Result<Value, CommandError> {
        let sessions: Vec<RuntimeSession> = call(server, "session.list", json!({}), None).await?;
        let tasks: Vec<RuntimeTask> = call(server, "task.list", json!({}), None).await?;
        let approvals: Vec<PendingApproval> =
            call(server, "approval.list", json!({}), None).await?;
        let questions: Vec<PendingQuestion> =
            call(server, "question.list", json!({}), None).await?;
        self.remember_tasks(&tasks);
        let digests = session_digests(&server.home).await;

        let mut sessions = sessions
            .into_iter()
            .filter(|session| session.status != SessionStatus::Archived)
            .collect::<Vec<_>>();
        sessions.sort_by_key(|session| std::cmp::Reverse(session.updated_at));

        // 「需要你处理」的是整个 Runtime，不只是选中的那条会话。
        let mut gates = approvals
            .iter()
            .map(|approval| {
                let session = self.cached_task_session(approval.task_id);
                (
                    approval.created_at,
                    session,
                    approval_json(approval, session),
                )
            })
            .chain(questions.iter().map(|question| {
                let session = self.cached_task_session(question.task_id);
                (
                    question.created_at,
                    session,
                    question_json(question, session),
                )
            }))
            .collect::<Vec<_>>();
        gates.sort_by_key(|(created_at, _, _)| std::cmp::Reverse(*created_at));

        let exists = |id: uuid::Uuid| sessions.iter().any(|session| session.id == id);
        if !self.selected.is_some_and(exists) {
            // 没选过（或选的那条没了）：先落在有人等着的会话上，其次是最近动过的。
            self.selected = gates
                .iter()
                .filter_map(|(_, session, _)| *session)
                .find(|id| exists(*id))
                .or_else(|| sessions.first().map(|session| session.id));
        }
        let mut listed = sessions
            .iter()
            .take(MAX_SNAPSHOT_SESSIONS)
            .collect::<Vec<_>>();
        if let Some(selected) = self.selected
            && !listed.iter().any(|session| session.id == selected)
            && let Some(session) = sessions.iter().find(|session| session.id == selected)
        {
            listed.push(session);
        }
        let session_values = listed
            .iter()
            .map(|session| {
                session_json(
                    session,
                    digests.get(&session.id),
                    Some(session.id) == self.selected,
                )
            })
            .collect::<Vec<_>>();
        let messages = match self.selected {
            Some(selected) => {
                let history = self
                    .history(&server.home, selected, digests.get(&selected))
                    .await;
                self.merge_tail(selected, history)
            }
            None => Vec::new(),
        };
        let mut payload = json!({
            "sessions": session_values,
            "pending_tools": gates.into_iter().map(|(_, _, value)| value).collect::<Vec<_>>(),
            // 这几类 rs 没有对应对象；写空数组，让手机清掉上一个桌面端留下的卡片。
            "patch_proposals": [],
            "jobs": [],
            "queued_messages": [],
            "worktree_changes": [],
            "messages": messages,
        });
        if let Some(selected) = self.selected {
            payload["active_session_id"] = json!(selected.to_string());
        }
        Ok(payload)
    }

    fn remember_tasks(&mut self, tasks: &[RuntimeTask]) {
        if self.task_sessions.len() > MAX_TASK_CACHE {
            self.task_sessions.clear();
        }
        for task in tasks {
            self.task_sessions.insert(task.id, task.session_id);
        }
    }

    fn cached_task_session(&self, task_id: uuid::Uuid) -> Option<uuid::Uuid> {
        self.task_sessions.get(&task_id).copied().flatten()
    }

    /// 任务归属的会话。先查缓存，没有再问一次 Runtime。
    pub(super) async fn task_session(
        &mut self,
        server: &ServerState,
        task_id: uuid::Uuid,
    ) -> Option<uuid::Uuid> {
        if let Some(session) = self.task_sessions.get(&task_id) {
            return *session;
        }
        let task: RuntimeTask = call(server, "task.get", json!({ "id": task_id }), None)
            .await
            .ok()?;
        if self.task_sessions.len() > MAX_TASK_CACHE {
            self.task_sessions.clear();
        }
        self.task_sessions.insert(task_id, task.session_id);
        task.session_id
    }

    async fn history(
        &mut self,
        home: &Path,
        session_id: uuid::Uuid,
        digest: Option<&SessionDigest>,
    ) -> Vec<Value> {
        let key = digest.map(|digest| (digest.updated_at, digest.message_count));
        if let (Some(key), Some(cache)) = (key, self.history.get(&session_id))
            && cache.key == key
        {
            return cache.items.clone();
        }
        let home = home.to_path_buf();
        let items = tokio::task::spawn_blocking(move || project_history(&home, session_id))
            .await
            .unwrap_or_default();
        if let Some(key) = key {
            if self.history.len() >= MAX_HISTORY_CACHE {
                self.history.clear();
            }
            self.history.insert(
                session_id,
                HistoryCache {
                    key,
                    items: items.clone(),
                },
            );
        }
        items
    }

    /// 历史投影 + 还没落盘的实时尾巴。已经出现在历史里的、轮次结束超过宽限期的
    /// 尾巴条目在这里清掉。
    pub(super) fn merge_tail(
        &mut self,
        session_id: uuid::Uuid,
        mut history: Vec<Value>,
    ) -> Vec<Value> {
        let Some(tail) = self.tails.get_mut(&session_id) else {
            return history;
        };
        let persisted = history
            .iter()
            .rev()
            .take(tail.len() + 8)
            .filter_map(|message| {
                Some((
                    message.get("role")?.as_str()?.to_owned(),
                    message.get("content")?.as_str()?.to_owned(),
                ))
            })
            .collect::<HashSet<_>>();
        tail.retain(|message| {
            !persisted.contains(&(message.role.to_owned(), message.content.clone()))
                && message
                    .settled_at
                    .is_none_or(|settled| settled.elapsed() < TAIL_GRACE)
        });
        for message in tail.iter() {
            history.push(message_json(
                &message.id,
                message.role,
                &message.content,
                Some(message.created_at),
                session_id,
            ));
        }
        if tail.is_empty() {
            self.tails.remove(&session_id);
        }
        let excess = history.len().saturating_sub(MAX_SNAPSHOT_MESSAGES);
        history.drain(..excess);
        history
    }

    fn push_tail(&mut self, session_id: uuid::Uuid, message: TailMessage) {
        let tail = self.tails.entry(session_id).or_default();
        if tail.iter().any(|existing| existing.id == message.id) {
            return;
        }
        tail.push(message);
        let excess = tail.len().saturating_sub(MAX_SNAPSHOT_MESSAGES);
        tail.drain(..excess);
    }

    /// 手机发出的那条消息回显给手机。id 由幂等键派生：手机重发同一条命令时不会
    /// 回显两次。
    pub(super) fn push_user_echo(
        &mut self,
        session_id: uuid::Uuid,
        request_id: uuid::Uuid,
        text: &str,
        image_count: usize,
    ) -> Value {
        let content = if text.is_empty() {
            format!("[{image_count} images]")
        } else {
            clip(text, MAX_MESSAGE_CHARS)
        };
        let message = TailMessage {
            id: format!("mobile:{request_id}"),
            role: "user",
            content,
            created_at: now(),
            settled_at: None,
        };
        let payload = message_json(
            &message.id,
            message.role,
            &message.content,
            Some(message.created_at),
            session_id,
        );
        self.push_tail(session_id, message);
        reply(None, "message.append", Some(session_id), payload)
    }

    fn settle_tail(&mut self, session_id: uuid::Uuid) {
        let settled = Instant::now();
        if let Some(tail) = self.tails.get_mut(&session_id) {
            for message in tail.iter_mut() {
                message.settled_at.get_or_insert(settled);
            }
        }
    }

    /// Runtime 事件 → 手机信封。先过一遍公共事件流的脱敏，再挑手机关心的几类。
    ///
    /// 手机不在场时只更新网关自己的状态（实时尾巴、缓存），不为了一份没人收的信封
    /// 去查审批列表、扫会话摘要。
    pub(super) async fn translate(
        &mut self,
        server: &ServerState,
        event: RuntimeEvent,
    ) -> Vec<Value> {
        let event = event_stream::public_event(event);
        let phone_active = self.phone_active();
        match event.kind.as_str() {
            "task.output" => self.translate_output(server, &event).await,
            "task.waiting_approval" | "task.waiting_answer" if phone_active => {
                self.translate_waiting(server, &event).await
            }
            "task.interaction_resolved" | "task.interaction_cancelled" if phone_active => {
                let Some(interaction) = event_field(&event.message, "interaction_id") else {
                    return Vec::new();
                };
                let session = match event_field(&event.message, "task_id") {
                    Some(task_id) => self.task_session(server, task_id).await,
                    None => None,
                };
                let status = if event.kind == "task.interaction_resolved" {
                    "resolved"
                } else {
                    "cancelled"
                };
                vec![tool_updated(interaction, status, session)]
            }
            "turn.started" | "turn.requeued" | "turn.completed" | "turn.partial"
            | "turn.failed" | "turn.cancelled" | "turn.interrupted" => {
                let Some(session_id) = event_field(&event.message, "session_id") else {
                    return Vec::new();
                };
                if !matches!(event.kind.as_str(), "turn.started" | "turn.requeued") {
                    self.settle_tail(session_id);
                    self.history.remove(&session_id);
                }
                if !phone_active {
                    return Vec::new();
                }
                self.session_upsert(server, session_id).await
            }
            "session.renamed"
            | "session.archived"
            | "session.unarchived"
            | "session.deleted"
            | "session.model_updated" => {
                self.snapshot_dirty = true;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// 只转整条的 `assistant_text`，不转 token 级增量：中继是公网往返，一条回复
    /// 几百帧不划算。
    async fn translate_output(&mut self, server: &ServerState, event: &RuntimeEvent) -> Vec<Value> {
        let Some((prefix, payload)) = event.message.split_once(' ') else {
            return Vec::new();
        };
        let Some(task_id) = event_field(prefix, "task_id") else {
            return Vec::new();
        };
        let Ok(value) = serde_json::from_str::<Value>(payload) else {
            return Vec::new();
        };
        if value.get("type").and_then(Value::as_str) != Some("assistant_text") {
            return Vec::new();
        }
        let text = value
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if text.trim().is_empty() {
            return Vec::new();
        }
        let Some(session_id) = self.task_session(server, task_id).await else {
            return Vec::new();
        };
        let message = TailMessage {
            id: uuid::Uuid::new_v4().to_string(),
            role: "assistant",
            content: clip(text, MAX_MESSAGE_CHARS),
            created_at: event.timestamp,
            settled_at: None,
        };
        let payload = message_json(
            &message.id,
            message.role,
            &message.content,
            Some(message.created_at),
            session_id,
        );
        let message_id = message.id.clone();
        self.push_tail(session_id, message);
        vec![
            reply(None, "message.append", Some(session_id), payload),
            reply(
                None,
                "message.done",
                Some(session_id),
                json!({ "message_id": message_id }),
            ),
        ]
    }

    async fn translate_waiting(
        &mut self,
        server: &ServerState,
        event: &RuntimeEvent,
    ) -> Vec<Value> {
        let Some(interaction) = event_field(&event.message, "interaction_id") else {
            return Vec::new();
        };
        let session = match event_field(&event.message, "task_id") {
            Some(task_id) => self.task_session(server, task_id).await,
            None => None,
        };
        let pending = if event.kind == "task.waiting_approval" {
            call::<Vec<PendingApproval>>(server, "approval.list", json!({}), None)
                .await
                .ok()
                .and_then(|approvals| {
                    approvals
                        .iter()
                        .find(|approval| approval.id == interaction)
                        .map(|approval| approval_json(approval, session))
                })
        } else {
            call::<Vec<PendingQuestion>>(server, "question.list", json!({}), None)
                .await
                .ok()
                .and_then(|questions| {
                    questions
                        .iter()
                        .find(|question| question.id == interaction)
                        .map(|question| question_json(question, session))
                })
        };
        pending
            .map(|payload| vec![reply(None, "tool.pending", session, payload)])
            .unwrap_or_default()
    }

    async fn session_upsert(&mut self, server: &ServerState, session_id: uuid::Uuid) -> Vec<Value> {
        let Ok(session) =
            call::<RuntimeSession>(server, "session.get", json!({ "id": session_id }), None).await
        else {
            return Vec::new();
        };
        let digests = session_digests(&server.home).await;
        vec![reply(
            None,
            "session.upsert",
            Some(session.id),
            json!({
                "session": session_json(
                    &session,
                    digests.get(&session.id),
                    Some(session.id) == self.selected,
                )
            }),
        )]
    }
}

/// 会话摘要（标题、条数）。`digests` 自己按文件 mtime 缓存，不物化消息正文。
async fn session_digests(home: &Path) -> HashMap<uuid::Uuid, SessionDigest> {
    let home = home.to_path_buf();
    tokio::task::spawn_blocking(move || {
        SessionStore::new(home)
            .digests()
            .into_iter()
            .map(|digest| (digest.id, digest))
            .collect()
    })
    .await
    .unwrap_or_default()
}
