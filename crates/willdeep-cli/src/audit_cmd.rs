//! `willdeep audit export`：把 Agent 在一个会话里干过的事汇成一份能交给审计的报告。
//!
//! 四路来源落盘位置各不相同、键也不统一，这里把它们按会话拼回去：
//!
//! | 来源 | 文件 | 怎么归到会话 |
//! |---|---|---|
//! | 审批放行 | `approvals.jsonl` | rc22 起带 `session_id`；更早的记录按会话时间窗归入并标明 |
//! | 人工裁决 | `runtime/interactions.json` | 经 `runtime/tasks.json` 的 task → session |
//! | hook 拦截 | 会话记录里的工具结果 | `<hook-denied hook="…">` 标记 |
//! | 验证证据 | 会话检查点 + `runtime/diff-verifications.json` | 检查点直接带；快照级的按本会话改过的快照 id 关联 |
//! | 改动归属 | `runtime/diff-attributions.json` | 记录自带 `session_id` |
//! | 审阅 / 回滚 | `runtime/diff-reviews.json`、`runtime/recovery/<快照>-*` | 按本会话的快照 id |
//!
//! 只读磁盘上的状态文件：不需要 Runtime 在跑，不调 Provider，不走
//! `AgentStore::open`（它会把运行中的 Agent 标成中断——审计不能有副作用）。
//! 报告里命令已脱敏、路径是工作区相对路径、不含提示词和模型正文。

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Subcommand;
use serde::Serialize;
use uuid::Uuid;
use willdeep_core::session::{Session, SessionStore, format_iso8601, parse_iso8601_utc};
use willdeep_core::types::Role;

use crate::daemon::diff_review::{
    self, DiffAttributionRecord, DiffReviewRecord, DiffVerificationRecord,
};
use crate::daemon::{
    DaemonPaths, InteractionKind, InteractionResolution, RuntimeInteraction, RuntimeTask,
    StoredAgent,
};
use crate::i18n::Language;

pub(crate) const SCHEMA_VERSION: u32 = 1;
/// hook 拒绝理由进报告的上限。审计要的是「谁拦了什么」，不是 hook 的调试输出。
const HOOK_REASON_CHARS: usize = 200;
const HOOK_DENIED_OPEN: &str = "<hook-denied hook=\"";
const HOOK_DENIED_CLOSE: &str = "</hook-denied>";
const MATCHED_BY_SESSION: &str = "session_id";
const MATCHED_BY_WINDOW: &str = "time_window";

#[derive(Clone, Debug, Subcommand)]
pub(crate) enum AuditAction {
    /// Export one report for a session, or for a workspace over a time range.
    Export {
        /// Session UUID, or `latest`. The default when no other scope is given.
        #[arg(long, value_name = "ID|latest")]
        session: Option<String>,
        /// Every session whose workspace is this directory.
        #[arg(long, value_name = "PATH")]
        workspace: Option<PathBuf>,
        /// Only sessions updated at or after this time: YYYY-MM-DD, YYYY-MM-DDTHH:MM:SSZ or a Unix timestamp.
        #[arg(long, value_name = "TIME")]
        since: Option<String>,
        /// Only sessions created at or before this time.
        #[arg(long, value_name = "TIME")]
        until: Option<String>,
        /// Emit JSON instead of Markdown.
        #[arg(long)]
        json: bool,
        /// Write the report to this file instead of stdout.
        #[arg(long, value_name = "PATH")]
        output: Option<PathBuf>,
    },
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Scope {
    pub session: Option<String>,
    pub workspace: Option<PathBuf>,
    pub since: Option<u64>,
    pub until: Option<u64>,
}

pub(crate) fn run(action: AuditAction, home: &Path, language: Language) -> Result<()> {
    let AuditAction::Export {
        session,
        workspace,
        since,
        until,
        json,
        output,
    } = action;
    let scope = Scope {
        session,
        workspace,
        since: since.as_deref().map(parse_time).transpose()?,
        until: until.as_deref().map(parse_time).transpose()?,
    };
    let report = build_report(home, &scope)?;
    let text = if json {
        format!("{}\n", serde_json::to_string_pretty(&report)?)
    } else {
        render_markdown(&report, language)
    };
    match output {
        Some(path) => {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, text).with_context(|| format!("write {}", path.display()))?;
            println!("{}", path.display());
        }
        None => print!("{text}"),
    }
    Ok(())
}

fn parse_time(text: &str) -> Result<u64> {
    if let Ok(unix) = text.trim().parse::<u64>() {
        return Ok(unix);
    }
    parse_iso8601_utc(text).with_context(|| {
        format!(
            "cannot parse time {text:?}: use YYYY-MM-DD, YYYY-MM-DDTHH:MM:SSZ or a Unix timestamp"
        )
    })
}

#[derive(Debug, Serialize)]
pub(crate) struct AuditReport {
    pub schema_version: u32,
    pub generated_at: String,
    pub willdeep_version: &'static str,
    pub scope: ScopeDescription,
    pub summary: Summary,
    pub sessions: Vec<SessionAudit>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ScopeDescription {
    pub session: Option<String>,
    pub workspace: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct Summary {
    pub sessions: usize,
    pub approvals: usize,
    pub approvals_by_source: BTreeMap<String, usize>,
    /// 没有 `session_id`、只能按时间窗归入的旧记录数。它们可能属于同一时段的
    /// 另一个会话，报告里单独点名，不和有键的混在一起。
    pub approvals_matched_by_time_window: usize,
    pub approvals_unparsable: usize,
    pub human_decisions: HumanDecisions,
    pub hook_denials: usize,
    pub verifications: VerificationCounts,
    pub subagent_verdicts: VerdictCounts,
    pub changes: usize,
    pub files_changed: usize,
    pub reviews: usize,
    pub reverts: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct HumanDecisions {
    pub allow_once: usize,
    pub always_allow: usize,
    pub deny: usize,
    pub answered: usize,
    pub pending: usize,
    pub cancelled: usize,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct VerificationCounts {
    pub passed: usize,
    pub failed: usize,
    /// 超时或没启动起来：既不是过也不是不过。
    pub other: usize,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct VerdictCounts {
    pub passed: usize,
    pub failed: usize,
    /// 没有 verifier 的运行。「没验证」和「没通过」是两件事。
    pub unverified: usize,
}

#[derive(Debug, Serialize)]
pub(crate) struct SessionAudit {
    pub id: Uuid,
    pub title: String,
    pub workspace: String,
    pub model: Option<String>,
    pub profile: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub messages: usize,
    pub status: Option<String>,
    pub turns: Option<usize>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub approvals: Vec<ApprovalEntry>,
    pub interactions: Vec<InteractionEntry>,
    pub hook_denials: Vec<HookDenial>,
    pub verification: VerificationAudit,
    pub subagents: Vec<SubagentEntry>,
    pub changes: Vec<ChangeEntry>,
    pub files_changed: Vec<String>,
    pub reviews: Vec<ReviewEntry>,
    pub reverts: Vec<RevertEntry>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ApprovalEntry {
    pub at: String,
    pub source: String,
    pub detail: String,
    pub command: String,
    pub matched_by: &'static str,
}

#[derive(Debug, Serialize)]
pub(crate) struct InteractionEntry {
    pub task_id: Uuid,
    pub kind: &'static str,
    pub description: String,
    pub status: String,
    pub resolution: Option<&'static str>,
    pub created_at: String,
    pub resolved_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct HookDenial {
    pub hook: String,
    pub tool: Option<String>,
    pub reason: String,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct VerificationAudit {
    pub required: Vec<String>,
    pub baseline: Option<String>,
    pub evidence: Vec<EvidenceEntry>,
    pub snapshot_bound: Vec<SnapshotVerification>,
}

#[derive(Debug, Serialize)]
pub(crate) struct EvidenceEntry {
    pub command: String,
    pub status: String,
    pub snapshot_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SnapshotVerification {
    pub at: String,
    pub snapshot_id: String,
    pub command: String,
    pub exit_code: Option<i32>,
    pub outcome: String,
    pub summary: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct SubagentEntry {
    pub id: Uuid,
    pub label: Option<String>,
    pub profile: Option<String>,
    pub status: String,
    pub verifier_passed: Option<bool>,
    pub attempts: Option<u64>,
    pub claims_checked: Option<u64>,
    pub claims_unverifiable: Option<u64>,
    pub repo_commit: Option<String>,
    pub worktree_branch: Option<String>,
    pub worktree_merged: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct ChangeEntry {
    pub at: String,
    pub agent_id: Uuid,
    pub agent: &'static str,
    pub agent_label: Option<String>,
    pub turn_id: Option<Uuid>,
    pub tool: String,
    pub paths: Vec<String>,
    pub after_snapshot_id: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct ReviewEntry {
    pub at: String,
    pub path: String,
    pub decision: String,
    pub note: Option<String>,
    pub snapshot_id: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct RevertEntry {
    pub snapshot_id: String,
    pub recovery_dir: String,
}

struct ApprovalRecord {
    at: u64,
    session_id: Option<Uuid>,
    source: String,
    detail: String,
    command: String,
}

struct Sources {
    approvals: Vec<ApprovalRecord>,
    approvals_unparsable: usize,
    tasks: Vec<RuntimeTask>,
    interactions: Vec<RuntimeInteraction>,
    agents: Vec<StoredAgent>,
    attributions: Vec<DiffAttributionRecord>,
    verifications: Vec<DiffVerificationRecord>,
    reviews: Vec<DiffReviewRecord>,
    recovery_dirs: Vec<String>,
}

impl Sources {
    fn load(home: &Path) -> Result<Self> {
        let paths = DaemonPaths::new(home);
        let (approvals, approvals_unparsable) = load_approvals(&home.join("approvals.jsonl"))?;
        Ok(Self {
            approvals,
            approvals_unparsable,
            tasks: crate::daemon::load_tasks(&paths.tasks)
                .context("read runtime/tasks.json")?
                .into_values()
                .collect(),
            interactions: crate::daemon::load_interactions(&paths.interactions)
                .context("read runtime/interactions.json")?
                .into_values()
                .collect(),
            agents: crate::daemon::load_agents(&paths.agents)
                .context("read runtime/agents.json")?
                .into_values()
                .collect(),
            attributions: diff_review::load_attributions(&diff_review::attribution_store_path(
                home,
            ))
            .context("read runtime/diff-attributions.json")?,
            verifications: diff_review::load_verifications(&diff_review::verification_store_path(
                home,
            ))
            .context("read runtime/diff-verifications.json")?,
            reviews: diff_review::load_reviews(&diff_review::review_store_path(home))
                .context("read runtime/diff-reviews.json")?,
            recovery_dirs: list_directories(&diff_review::recovery_root(home))?,
        })
    }
}

/// 一行一条 JSON；坏行跳过并计数，不让一次写坏让整份审计出不来。
fn load_approvals(path: &Path) -> Result<(Vec<ApprovalRecord>, usize)> {
    if !path.exists() {
        return Ok((Vec::new(), 0));
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut records = Vec::new();
    let mut unparsable = 0;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            unparsable += 1;
            continue;
        };
        let field = |key: &str| {
            value
                .get(key)
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_owned()
        };
        records.push(ApprovalRecord {
            at: value
                .get("at")
                .and_then(|value| value.as_u64())
                .unwrap_or_default(),
            session_id: value
                .get("session_id")
                .and_then(|value| value.as_str())
                .and_then(|value| Uuid::parse_str(value).ok()),
            source: field("source"),
            detail: field("detail"),
            command: field("command"),
        });
    }
    Ok((records, unparsable))
}

fn list_directories(root: &Path) -> Result<Vec<String>> {
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut names = std::fs::read_dir(root)
        .with_context(|| format!("read {}", root.display()))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort();
    Ok(names)
}

pub(crate) fn build_report(home: &Path, scope: &Scope) -> Result<AuditReport> {
    let store = SessionStore::new(home);
    let sessions = select_sessions(&store, scope)?;
    let sources = Sources::load(home)?;
    let audits = sessions
        .iter()
        .map(|session| audit_session(session, &sources))
        .collect::<Vec<_>>();
    let summary = summarize(&audits, sources.approvals_unparsable);
    Ok(AuditReport {
        schema_version: SCHEMA_VERSION,
        generated_at: format_iso8601(now()),
        willdeep_version: env!("CARGO_PKG_VERSION"),
        scope: ScopeDescription {
            session: scope.session.clone(),
            workspace: scope
                .workspace
                .as_ref()
                .map(|path| path.display().to_string()),
            since: scope.since.map(format_iso8601),
            until: scope.until.map(format_iso8601),
        },
        summary,
        sessions: audits,
    })
}

fn select_sessions(store: &SessionStore, scope: &Scope) -> Result<Vec<Session>> {
    if let Some(selector) = &scope.session {
        let session = if selector == "latest" {
            store.latest()?.context("no sessions recorded yet")?
        } else {
            let id = Uuid::parse_str(selector).context("--session must be a UUID or `latest`")?;
            store
                .load(id)
                .with_context(|| format!("load session {id}"))?
        };
        return Ok(vec![session]);
    }
    if scope.workspace.is_none() && scope.since.is_none() && scope.until.is_none() {
        return Ok(store.latest()?.into_iter().collect());
    }
    let workspace = scope.workspace.as_deref().map(canonical);
    let mut sessions = store
        .list()?
        .into_iter()
        .filter(|session| {
            workspace
                .as_ref()
                .is_none_or(|wanted| canonical(&session.workspace) == *wanted)
        })
        .filter(|session| scope.since.is_none_or(|since| session.updated_at >= since))
        .filter(|session| scope.until.is_none_or(|until| session.created_at <= until))
        .collect::<Vec<_>>();
    sessions.sort_by_key(|session| session.created_at);
    Ok(sessions)
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default()
}

fn audit_session(session: &Session, sources: &Sources) -> SessionAudit {
    let window = session.created_at..=session.updated_at.max(session.created_at);
    let session_tasks = sources
        .tasks
        .iter()
        .filter(|task| task.session_id == Some(session.id))
        .collect::<Vec<_>>();
    let task_ids = session_tasks
        .iter()
        .map(|task| task.id)
        .collect::<BTreeSet<_>>();
    let root_agents = session_tasks
        .iter()
        .filter_map(|task| task.agent_id)
        .collect::<BTreeSet<_>>();
    let agents_by_id = sources
        .agents
        .iter()
        .map(|agent| (agent.id, agent))
        .collect::<HashMap<_, _>>();

    let approvals = sources
        .approvals
        .iter()
        .filter_map(|record| {
            let matched_by = match record.session_id {
                Some(id) if id == session.id => MATCHED_BY_SESSION,
                Some(_) => return None,
                None if window.contains(&record.at) => MATCHED_BY_WINDOW,
                None => return None,
            };
            Some(ApprovalEntry {
                at: format_iso8601(record.at),
                source: record.source.clone(),
                detail: record.detail.clone(),
                command: record.command.clone(),
                matched_by,
            })
        })
        .collect::<Vec<_>>();

    let mut interactions = sources
        .interactions
        .iter()
        .filter(|interaction| task_ids.contains(&interaction.task_id))
        .collect::<Vec<_>>();
    interactions.sort_by_key(|interaction| interaction.created_at);
    let interactions = interactions
        .into_iter()
        .map(|interaction| {
            let (kind, description) = match &interaction.kind {
                InteractionKind::Approval { description, .. } => (
                    "approval",
                    willdeep_core::judge::redact_credentials(description),
                ),
                InteractionKind::Question { question, .. } => ("question", question.clone()),
            };
            InteractionEntry {
                task_id: interaction.task_id,
                kind,
                description,
                status: enum_label(&interaction.status),
                resolution: interaction.resolution.as_ref().map(resolution_label),
                created_at: format_iso8601(interaction.created_at),
                resolved_at: interaction.resolved_at.map(format_iso8601),
            }
        })
        .collect();

    let mut attributions = sources
        .attributions
        .iter()
        .filter(|record| record.session_id == Some(session.id))
        .collect::<Vec<_>>();
    attributions.sort_by_key(|record| record.created_at);
    let snapshot_ids = attributions
        .iter()
        .flat_map(|record| {
            [
                record.before_snapshot_id.as_str(),
                record.after_snapshot_id.as_str(),
            ]
        })
        .collect::<BTreeSet<_>>();
    let changes = attributions
        .iter()
        .map(|record| {
            let stored = agents_by_id.get(&record.agent_id).copied();
            let is_root = root_agents.contains(&record.agent_id)
                || stored.is_some_and(|agent| agent.parent_id.is_none());
            ChangeEntry {
                at: format_iso8601(record.created_at),
                agent_id: record.agent_id,
                agent: if is_root { "root" } else { "child" },
                agent_label: stored
                    .and_then(|agent| agent.label.clone().or_else(|| agent.profile.clone())),
                turn_id: record.turn_id,
                tool: record.tool.clone(),
                paths: record.paths.clone(),
                after_snapshot_id: record.after_snapshot_id.clone(),
            }
        })
        .collect::<Vec<_>>();
    let files_changed = changes
        .iter()
        .flat_map(|change| change.paths.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let checkpoint = session.execution_checkpoint.as_ref();
    let evidence_snapshots = checkpoint
        .map(|checkpoint| {
            checkpoint
                .verification_evidence
                .iter()
                .filter_map(|evidence| evidence.snapshot_id.as_deref())
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let mut snapshot_bound = sources
        .verifications
        .iter()
        .filter(|record| {
            snapshot_ids.contains(record.snapshot_id.as_str())
                || evidence_snapshots.contains(record.snapshot_id.as_str())
        })
        .collect::<Vec<_>>();
    snapshot_bound.sort_by_key(|record| record.created_at);
    let verification = VerificationAudit {
        required: checkpoint
            .map(|checkpoint| checkpoint.required_verifications.clone())
            .unwrap_or_default(),
        baseline: checkpoint.and_then(|checkpoint| checkpoint.verification_baseline.clone()),
        evidence: checkpoint
            .map(|checkpoint| {
                checkpoint
                    .verification_evidence
                    .iter()
                    .map(|evidence| EvidenceEntry {
                        command: evidence.command.clone(),
                        status: enum_label(&evidence.status),
                        snapshot_id: evidence.snapshot_id.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        snapshot_bound: snapshot_bound
            .into_iter()
            .map(|record| SnapshotVerification {
                at: format_iso8601(record.created_at),
                snapshot_id: record.snapshot_id.clone(),
                command: record.command.clone(),
                exit_code: record.exit_code,
                outcome: enum_label(&record.outcome),
                summary: record.summary.clone(),
            })
            .collect(),
    };

    let mut subagents = sources
        .agents
        .iter()
        .filter(|agent| task_ids.contains(&agent.task_id) && agent.parent_id.is_some())
        .collect::<Vec<_>>();
    subagents.sort_by_key(|agent| agent.created_at);
    let subagents = subagents
        .into_iter()
        .map(|agent| SubagentEntry {
            id: agent.id,
            label: agent.label.clone(),
            profile: agent.profile.clone(),
            status: enum_label(&agent.status),
            verifier_passed: agent.verifier_passed,
            attempts: agent.attempts,
            claims_checked: agent.claims_checked,
            claims_unverifiable: agent.claims_unverifiable,
            repo_commit: agent.repo_commit.clone(),
            worktree_branch: agent.worktree_branch.clone(),
            worktree_merged: agent.worktree_merged_at.is_some(),
        })
        .collect();

    let mut reviews = sources
        .reviews
        .iter()
        .filter(|record| snapshot_ids.contains(record.snapshot_id.as_str()))
        .collect::<Vec<_>>();
    reviews.sort_by_key(|record| record.created_at);
    let reviews = reviews
        .into_iter()
        .map(|record| ReviewEntry {
            at: format_iso8601(record.created_at),
            path: record.path.clone(),
            decision: enum_label(&record.decision),
            note: record.note.clone(),
            snapshot_id: record.snapshot_id.clone(),
        })
        .collect();

    // 检查点回退的回收区目录带会话 id 而不是快照 id：`rewind-<会话>-<随机>`。
    let rewind_prefix = format!("rewind-{}-", session.id.simple());
    let reverts = sources
        .recovery_dirs
        .iter()
        .filter_map(|directory| {
            if directory.starts_with(&rewind_prefix) {
                return Some(RevertEntry {
                    snapshot_id: "rewind".to_owned(),
                    recovery_dir: directory.clone(),
                });
            }
            snapshot_ids
                .iter()
                .find(|snapshot_id| directory.starts_with(&format!("{snapshot_id}-")))
                .map(|snapshot_id| RevertEntry {
                    snapshot_id: (*snapshot_id).to_owned(),
                    recovery_dir: directory.clone(),
                })
        })
        .collect();

    SessionAudit {
        id: session.id,
        title: session.title.clone(),
        workspace: session.workspace.display().to_string(),
        model: session.model.clone(),
        profile: session.profile.clone(),
        created_at: format_iso8601(session.created_at),
        updated_at: format_iso8601(session.updated_at),
        messages: session.messages.len(),
        status: checkpoint.map(|checkpoint| enum_label(&checkpoint.status)),
        turns: checkpoint.map(|checkpoint| checkpoint.turns),
        input_tokens: checkpoint.map(|checkpoint| checkpoint.input_tokens),
        output_tokens: checkpoint.map(|checkpoint| checkpoint.output_tokens),
        approvals,
        interactions,
        hook_denials: hook_denials(session),
        verification,
        subagents,
        changes,
        files_changed,
        reviews,
        reverts,
    }
}

/// hook 拦截没有自己的日志：拦下的调用以 `<hook-denied hook="…">` 的工具结果回到
/// 模型，也就落在会话记录里。这里把它们捞出来，点名 hook 和被拦的工具。
fn hook_denials(session: &Session) -> Vec<HookDenial> {
    let tool_names = session
        .messages
        .iter()
        .flat_map(|message| message.tool_calls.iter())
        .map(|call| (call.id.as_str(), call.name.as_str()))
        .collect::<HashMap<_, _>>();
    session
        .messages
        .iter()
        .filter(|message| message.role == Role::Tool)
        .filter_map(|message| {
            let start = message.content.find(HOOK_DENIED_OPEN)?;
            let rest = &message.content[start + HOOK_DENIED_OPEN.len()..];
            let name_end = rest.find('"')?;
            let body = rest[name_end + 1..]
                .strip_prefix('>')
                .unwrap_or(&rest[name_end + 1..]);
            let reason = body
                .split(HOOK_DENIED_CLOSE)
                .next()
                .unwrap_or_default()
                .trim();
            Some(HookDenial {
                hook: rest[..name_end].to_owned(),
                tool: message
                    .tool_call_id
                    .as_deref()
                    .and_then(|id| tool_names.get(id))
                    .map(|name| (*name).to_owned()),
                reason: truncate_chars(reason, HOOK_REASON_CHARS),
            })
        })
        .collect()
}

fn truncate_chars(text: &str, limit: usize) -> String {
    let mut chars = text.chars();
    let head = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

fn enum_label<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => text,
        Ok(other) => other.to_string(),
        Err(_) => "unknown".to_owned(),
    }
}

fn resolution_label(resolution: &InteractionResolution) -> &'static str {
    match resolution {
        InteractionResolution::AllowOnce => "allow_once",
        InteractionResolution::Deny => "deny",
        InteractionResolution::AlwaysAllow => "always_allow",
        InteractionResolution::Answer(_) => "answer",
    }
}

fn summarize(audits: &[SessionAudit], approvals_unparsable: usize) -> Summary {
    let mut summary = Summary {
        sessions: audits.len(),
        approvals_unparsable,
        ..Default::default()
    };
    for audit in audits {
        summary.approvals += audit.approvals.len();
        for approval in &audit.approvals {
            *summary
                .approvals_by_source
                .entry(approval.source.clone())
                .or_default() += 1;
            if approval.matched_by == MATCHED_BY_WINDOW {
                summary.approvals_matched_by_time_window += 1;
            }
        }
        for interaction in &audit.interactions {
            let decisions = &mut summary.human_decisions;
            match (interaction.status.as_str(), interaction.resolution) {
                ("pending", _) => decisions.pending += 1,
                ("cancelled", _) => decisions.cancelled += 1,
                (_, Some("allow_once")) => decisions.allow_once += 1,
                (_, Some("always_allow")) => decisions.always_allow += 1,
                (_, Some("deny")) => decisions.deny += 1,
                (_, Some("answer")) => decisions.answered += 1,
                _ => {}
            }
        }
        summary.hook_denials += audit.hook_denials.len();
        // 检查点里的证据和快照级记录常常是同一次验证的两份记录，按
        // （快照, 命令）去重后再计数，免得一条命令算两遍。
        let mut counted = BTreeSet::new();
        for evidence in &audit.verification.evidence {
            counted.insert((evidence.snapshot_id.clone(), evidence.command.clone()));
            count_verification(&mut summary.verifications, &evidence.status);
        }
        for record in &audit.verification.snapshot_bound {
            if counted.insert((Some(record.snapshot_id.clone()), record.command.clone())) {
                count_verification(&mut summary.verifications, &record.outcome);
            }
        }
        for subagent in &audit.subagents {
            match subagent.verifier_passed {
                Some(true) => summary.subagent_verdicts.passed += 1,
                Some(false) => summary.subagent_verdicts.failed += 1,
                None => summary.subagent_verdicts.unverified += 1,
            }
        }
        summary.changes += audit.changes.len();
        summary.files_changed += audit.files_changed.len();
        summary.reviews += audit.reviews.len();
        summary.reverts += audit.reverts.len();
        summary.input_tokens += audit.input_tokens.unwrap_or_default();
        summary.output_tokens += audit.output_tokens.unwrap_or_default();
    }
    summary
}

fn count_verification(counts: &mut VerificationCounts, status: &str) {
    match status {
        "passed" => counts.passed += 1,
        "failed" => counts.failed += 1,
        _ => counts.other += 1,
    }
}

pub(crate) fn render_markdown(report: &AuditReport, language: Language) -> String {
    let t = |zh: &'static str, en: &'static str, ja: &'static str| language.text(zh, en, ja);
    let none = t("（无）", "(none)", "（なし）");
    let mut out = String::new();
    out.push_str(&format!(
        "# {}\n\n",
        t(
            "WillDeep 审计报告",
            "WillDeep audit report",
            "WillDeep 監査レポート"
        )
    ));
    let mut scope = Vec::new();
    if let Some(session) = &report.scope.session {
        scope.push(format!(
            "{} `{session}`",
            t("会话", "session", "セッション")
        ));
    }
    if let Some(workspace) = &report.scope.workspace {
        scope.push(format!(
            "{} `{workspace}`",
            t("工作区", "workspace", "ワークスペース")
        ));
    }
    if let Some(since) = &report.scope.since {
        scope.push(format!("{} {since}", t("起", "since", "以降")));
    }
    if let Some(until) = &report.scope.until {
        scope.push(format!("{} {until}", t("止", "until", "まで")));
    }
    if scope.is_empty() {
        scope.push(t("最近一个会话", "latest session", "最新のセッション").to_owned());
    }
    out.push_str(&format!(
        "{} {} · willdeep {} · {}: {}\n\n",
        t("生成于", "Generated", "生成"),
        report.generated_at,
        report.willdeep_version,
        t("范围", "scope", "範囲"),
        scope.join(" · ")
    ));

    let summary = &report.summary;
    out.push_str(&format!("## {}\n\n", t("汇总", "Summary", "概要")));
    out.push_str(&format!(
        "| {} | {} |\n|---|---|\n",
        t("指标", "Metric", "指標"),
        t("值", "Value", "値")
    ));
    row(
        &mut out,
        t("会话", "Sessions", "セッション"),
        summary.sessions.to_string(),
    );
    let by_source = if summary.approvals_by_source.is_empty() {
        none.to_owned()
    } else {
        summary
            .approvals_by_source
            .iter()
            .map(|(source, count)| format!("{source} {count}"))
            .collect::<Vec<_>>()
            .join(" · ")
    };
    row(
        &mut out,
        t(
            "审批放行（按来源）",
            "Approvals (by source)",
            "承認（ソース別）",
        ),
        format!("{}（{by_source}）", summary.approvals),
    );
    row(
        &mut out,
        t(
            "其中按时间窗归入的旧记录",
            "…matched by time window (records without session_id)",
            "…時間枠で紐付けた旧記録",
        ),
        summary.approvals_matched_by_time_window.to_string(),
    );
    if summary.approvals_unparsable > 0 {
        row(
            &mut out,
            t(
                "无法解析的审批记录行",
                "Unparsable approval lines",
                "解析できない承認行",
            ),
            summary.approvals_unparsable.to_string(),
        );
    }
    let decisions = &summary.human_decisions;
    row(
        &mut out,
        t("人工裁决", "Human decisions", "人による裁定"),
        format!(
            "{} {} · {} {} · {} {} · {} {} · {} {}",
            t("允许一次", "allow once", "一度許可"),
            decisions.allow_once,
            t("始终允许", "always allow", "常に許可"),
            decisions.always_allow,
            t("拒绝", "deny", "拒否"),
            decisions.deny,
            t("回答", "answered", "回答"),
            decisions.answered,
            t("未决", "pending", "未決"),
            decisions.pending
        ),
    );
    row(
        &mut out,
        t("hook 拦截", "Hook denials", "hook による拒否"),
        summary.hook_denials.to_string(),
    );
    row(
        &mut out,
        t("验证命令", "Verification commands", "検証コマンド"),
        format!(
            "{} {} · {} {} · {} {}",
            t("通过", "passed", "成功"),
            summary.verifications.passed,
            t("失败", "failed", "失敗"),
            summary.verifications.failed,
            t("其它", "other", "その他"),
            summary.verifications.other
        ),
    );
    row(
        &mut out,
        t("子 Agent 裁决", "Subagent verdicts", "サブエージェント判定"),
        format!(
            "{} {} · {} {} · {} {}",
            t("通过", "passed", "成功"),
            summary.subagent_verdicts.passed,
            t("失败", "failed", "失敗"),
            summary.subagent_verdicts.failed,
            t("未验证", "unverified", "未検証"),
            summary.subagent_verdicts.unverified
        ),
    );
    row(
        &mut out,
        t("改动", "Changes", "変更"),
        format!(
            "{} {} · {} {}",
            summary.changes,
            t("次工具调用", "tool calls", "回のツール呼び出し"),
            summary.files_changed,
            t("个文件", "files", "ファイル")
        ),
    );
    row(
        &mut out,
        t(
            "审阅 / 回滚",
            "Reviews / reverts",
            "レビュー / ロールバック",
        ),
        format!("{} / {}", summary.reviews, summary.reverts),
    );
    row(
        &mut out,
        t(
            "token（输入 / 输出）",
            "Tokens (input / output)",
            "トークン（入力 / 出力）",
        ),
        format!("{} / {}", summary.input_tokens, summary.output_tokens),
    );
    out.push('\n');

    if report.sessions.is_empty() {
        out.push_str(&format!(
            "{}\n",
            t(
                "范围内没有会话。",
                "No sessions in scope.",
                "範囲内にセッションはありません。"
            )
        ));
    }
    for audit in &report.sessions {
        render_session(&mut out, audit, language, none);
    }
    out
}

fn render_session(out: &mut String, audit: &SessionAudit, language: Language, none: &str) {
    let t = |zh: &'static str, en: &'static str, ja: &'static str| language.text(zh, en, ja);
    let dash = "-";
    out.push_str(&format!("## {}（{}）\n\n", cell(&audit.title), audit.id));
    out.push_str(&format!(
        "{} `{}` · {} {} · {} {} · {} {} · {} {} · {} {} · {} {} · token {} / {}\n\n",
        t("工作区", "workspace", "ワークスペース"),
        audit.workspace,
        t("模型", "model", "モデル"),
        audit.model.as_deref().unwrap_or(dash),
        t("创建", "created", "作成"),
        audit.created_at,
        t("更新", "updated", "更新"),
        audit.updated_at,
        t("消息", "messages", "メッセージ"),
        audit.messages,
        t("状态", "status", "状態"),
        audit.status.as_deref().unwrap_or(dash),
        t("轮次", "turns", "ターン"),
        audit
            .turns
            .map(|turns| turns.to_string())
            .unwrap_or_else(|| dash.to_owned()),
        audit
            .input_tokens
            .map(|tokens| tokens.to_string())
            .unwrap_or_else(|| dash.to_owned()),
        audit
            .output_tokens
            .map(|tokens| tokens.to_string())
            .unwrap_or_else(|| dash.to_owned()),
    ));

    section(
        out,
        &format!(
            "{}（{}）",
            t("审批放行", "Approvals", "承認"),
            audit.approvals.len()
        ),
        &[
            t("时间", "Time", "時刻"),
            t("来源", "Source", "ソース"),
            t("命令", "Command", "コマンド"),
            t("原因", "Reason", "理由"),
            t("归入方式", "Matched by", "紐付け"),
        ],
        audit
            .approvals
            .iter()
            .map(|entry| {
                vec![
                    entry.at.clone(),
                    entry.source.clone(),
                    format!("`{}`", cell(&entry.command)),
                    cell(&entry.detail),
                    entry.matched_by.to_owned(),
                ]
            })
            .collect(),
        none,
    );
    section(
        out,
        &format!(
            "{}（{}）",
            t("人工裁决", "Human decisions", "人による裁定"),
            audit.interactions.len()
        ),
        &[
            t("时间", "Time", "時刻"),
            t("类型", "Kind", "種類"),
            t("内容", "Description", "内容"),
            t("裁决", "Resolution", "裁定"),
            t("状态", "Status", "状態"),
        ],
        audit
            .interactions
            .iter()
            .map(|entry| {
                vec![
                    entry.created_at.clone(),
                    entry.kind.to_owned(),
                    cell(&entry.description),
                    entry.resolution.unwrap_or(dash).to_owned(),
                    entry.status.clone(),
                ]
            })
            .collect(),
        none,
    );
    section(
        out,
        &format!(
            "{}（{}）",
            t("hook 拦截", "Hook denials", "hook による拒否"),
            audit.hook_denials.len()
        ),
        &[
            "hook",
            t("工具", "Tool", "ツール"),
            t("理由", "Reason", "理由"),
        ],
        audit
            .hook_denials
            .iter()
            .map(|entry| {
                vec![
                    entry.hook.clone(),
                    entry.tool.clone().unwrap_or_else(|| dash.to_owned()),
                    cell(&entry.reason),
                ]
            })
            .collect(),
        none,
    );

    out.push_str(&format!(
        "### {}\n\n",
        t("验证证据", "Verification evidence", "検証の証拠")
    ));
    out.push_str(&format!(
        "{}: {} · {}: {}\n\n",
        t("要求的验证", "Required", "必須の検証"),
        if audit.verification.required.is_empty() {
            none.to_owned()
        } else {
            audit
                .verification
                .required
                .iter()
                .map(|command| format!("`{}`", cell(command)))
                .collect::<Vec<_>>()
                .join(", ")
        },
        t("基线", "baseline", "基準"),
        audit.verification.baseline.as_deref().unwrap_or(dash)
    ));
    table(
        out,
        &[
            t("命令", "Command", "コマンド"),
            t("状态", "Status", "状態"),
            t("快照", "Snapshot", "スナップショット"),
        ],
        audit
            .verification
            .evidence
            .iter()
            .map(|entry| {
                vec![
                    format!("`{}`", cell(&entry.command)),
                    entry.status.clone(),
                    entry.snapshot_id.clone().unwrap_or_else(|| dash.to_owned()),
                ]
            })
            .collect(),
        none,
    );
    if !audit.verification.snapshot_bound.is_empty() {
        out.push_str(&format!(
            "{}:\n\n",
            t(
                "绑定到快照的验证",
                "Snapshot-bound verifications",
                "スナップショットに紐付く検証"
            )
        ));
        table(
            out,
            &[
                t("时间", "Time", "時刻"),
                t("快照", "Snapshot", "スナップショット"),
                t("命令", "Command", "コマンド"),
                t("退出码", "Exit", "終了コード"),
                t("结果", "Outcome", "結果"),
                t("摘要", "Summary", "要約"),
            ],
            audit
                .verification
                .snapshot_bound
                .iter()
                .map(|entry| {
                    vec![
                        entry.at.clone(),
                        entry.snapshot_id.clone(),
                        format!("`{}`", cell(&entry.command)),
                        entry
                            .exit_code
                            .map(|code| code.to_string())
                            .unwrap_or_else(|| dash.to_owned()),
                        entry.outcome.clone(),
                        cell(&entry.summary),
                    ]
                })
                .collect(),
            none,
        );
    }

    section(
        out,
        &format!(
            "{}（{}）",
            t("子 Agent", "Subagents", "サブエージェント"),
            audit.subagents.len()
        ),
        &[
            "Agent",
            t("标签 / 档案", "Label / profile", "ラベル / プロファイル"),
            t("状态", "Status", "状態"),
            "verifier",
            t("尝试", "Attempts", "試行"),
            t("引用核对", "Citations", "引用確認"),
            t("起点 commit", "Base commit", "起点コミット"),
            "worktree",
        ],
        audit
            .subagents
            .iter()
            .map(|entry| {
                vec![
                    entry.id.to_string(),
                    format!(
                        "{} / {}",
                        entry.label.as_deref().unwrap_or(dash),
                        entry.profile.as_deref().unwrap_or(dash)
                    ),
                    entry.status.clone(),
                    match entry.verifier_passed {
                        Some(true) => t("通过", "passed", "成功"),
                        Some(false) => t("失败", "failed", "失敗"),
                        None => t("未验证", "unverified", "未検証"),
                    }
                    .to_owned(),
                    entry
                        .attempts
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| dash.to_owned()),
                    match (entry.claims_checked, entry.claims_unverifiable) {
                        (Some(checked), Some(bad)) => {
                            format!("{} / {checked}", checked - bad.min(checked))
                        }
                        _ => dash.to_owned(),
                    },
                    entry.repo_commit.as_deref().unwrap_or(dash).to_owned(),
                    match (&entry.worktree_branch, entry.worktree_merged) {
                        (Some(branch), true) => {
                            format!("{branch}（{}）", t("已合并", "merged", "マージ済み"))
                        }
                        (Some(branch), false) => branch.clone(),
                        (None, _) => dash.to_owned(),
                    },
                ]
            })
            .collect(),
        none,
    );

    section(
        out,
        &format!(
            "{}（{} {} · {} {}）",
            t("改动归属", "Change attribution", "変更の帰属"),
            audit.changes.len(),
            t("次", "calls", "回"),
            audit.files_changed.len(),
            t("个文件", "files", "ファイル")
        ),
        &[
            t("时间", "Time", "時刻"),
            "Agent",
            t("工具", "Tool", "ツール"),
            t("文件", "Files", "ファイル"),
            t("快照", "Snapshot", "スナップショット"),
        ],
        audit
            .changes
            .iter()
            .map(|entry| {
                vec![
                    entry.at.clone(),
                    match &entry.agent_label {
                        Some(label) => format!("{} · {}", entry.agent, cell(label)),
                        None => entry.agent.to_owned(),
                    },
                    entry.tool.clone(),
                    entry
                        .paths
                        .iter()
                        .map(|path| format!("`{}`", cell(path)))
                        .collect::<Vec<_>>()
                        .join(", "),
                    entry.after_snapshot_id.clone(),
                ]
            })
            .collect(),
        none,
    );
    if !audit.reviews.is_empty() {
        out.push_str(&format!("{}:\n\n", t("审阅", "Reviews", "レビュー")));
        table(
            out,
            &[
                t("时间", "Time", "時刻"),
                t("文件", "File", "ファイル"),
                t("裁定", "Decision", "裁定"),
                t("备注", "Note", "備考"),
                t("快照", "Snapshot", "スナップショット"),
            ],
            audit
                .reviews
                .iter()
                .map(|entry| {
                    vec![
                        entry.at.clone(),
                        format!("`{}`", cell(&entry.path)),
                        entry.decision.clone(),
                        entry
                            .note
                            .as_deref()
                            .map(cell)
                            .unwrap_or_else(|| dash.to_owned()),
                        entry.snapshot_id.clone(),
                    ]
                })
                .collect(),
            none,
        );
    }
    if !audit.reverts.is_empty() {
        out.push_str(&format!(
            "{}: {}\n\n",
            t(
                "回滚过的快照（回收区目录）",
                "Reverted snapshots (recovery directories)",
                "ロールバック済みスナップショット（回収ディレクトリ）"
            ),
            audit
                .reverts
                .iter()
                .map(|entry| format!("{} → `{}`", entry.snapshot_id, entry.recovery_dir))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
}

fn section(out: &mut String, heading: &str, header: &[&str], rows: Vec<Vec<String>>, none: &str) {
    out.push_str(&format!("### {heading}\n\n"));
    table(out, header, rows, none);
}

fn table(out: &mut String, header: &[&str], rows: Vec<Vec<String>>, none: &str) {
    if rows.is_empty() {
        out.push_str(&format!("{none}\n\n"));
        return;
    }
    out.push_str(&format!("| {} |\n", header.join(" | ")));
    out.push_str(&format!("|{}\n", "---|".repeat(header.len())));
    for cells in rows {
        out.push_str(&format!("| {} |\n", cells.join(" | ")));
    }
    out.push('\n');
}

fn row(out: &mut String, label: &str, value: String) {
    out.push_str(&format!("| {label} | {value} |\n"));
}

/// 表格单元格：竖线要转义，换行折成空格，否则一条多行原因就把表撕开了。
fn cell(text: &str) -> String {
    text.replace('|', "\\|")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::InteractionStatus;
    use crate::daemon::diff_review::{AttributionConfidence, ReviewDecision, VerificationOutcome};
    use willdeep_core::checkpoint::{CheckpointMetadata, CheckpointStatus, VerificationEvidence};
    use willdeep_core::tools::VerificationStatus;
    use willdeep_core::types::{Message, ToolCall};

    struct Home(PathBuf);

    impl Home {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("willdeep-audit-{}", Uuid::new_v4()));
            std::fs::create_dir_all(path.join("runtime")).unwrap();
            Self(path)
        }

        fn write(&self, relative: &str, text: &str) {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const SECRET: &str = "sk-secret-123456789";

    fn stored_agent_json(
        id: Uuid,
        parent: Option<Uuid>,
        task: Uuid,
        workspace: &Path,
    ) -> serde_json::Value {
        serde_json::json!({
            "id": id, "parent_id": parent, "task_id": task, "label": parent.map(|_| "reviewer"),
            "workspace": workspace, "profile": parent.map(|_| "reviewer"), "status": "completed",
            "current_turn": 2, "current_tool": null, "input_tokens": 10, "output_tokens": 5, "total_tokens": 15,
            "verifier_passed": parent.map(|_| true), "attempts": parent.map(|_| 1),
            "claims_checked": parent.map(|_| 3), "claims_unverifiable": parent.map(|_| 1),
            "repo_commit": parent.map(|_| "abc1234"), "worktree_branch": parent.map(|_| "willdeep/reviewer"),
            "created_at": 20, "updated_at": 30, "completed_at": 30, "error": null
        })
    }

    /// 铺一份完整的家目录：一个会话，四路来源各一条本会话的记录，外加一条
    /// 别的会话的、一条时间窗外的，用来证明过滤是对的。
    fn seeded_home() -> (Home, Session, PathBuf) {
        let home = Home::new();
        let workspace = home.0.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let store = SessionStore::new(&home.0);
        let mut session = Session::new(workspace.clone(), None, "修一下登录接口");
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "run_command".to_owned(),
            arguments: "{}".to_owned(),
        };
        session.messages = vec![
            Message::user("修一下登录接口"),
            Message::assistant("先看看", vec![call.clone()]),
            Message::tool(
                &call,
                "tool error: <hook-denied hook=\"no-rm\">\n不许删东西\n第二行\n</hook-denied>",
            ),
            Message::assistant("好的", Vec::new()),
        ];
        session.execution_checkpoint = Some(CheckpointMetadata {
            required_verifications: vec!["cargo test".to_owned()],
            verification_evidence: vec![VerificationEvidence {
                snapshot_id: Some("snap-b".to_owned()),
                command: "cargo test".to_owned(),
                status: VerificationStatus::Passed,
            }],
            verification_baseline: None,
            status: CheckpointStatus::Completed,
            turns: 3,
            input_tokens: 100,
            output_tokens: 50,
            pending_call_ids: Vec::new(),
        });
        store.save(&mut session).unwrap();
        let session = store.load(session.id).unwrap();

        let task = Uuid::new_v4();
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let other_session = Uuid::new_v4();
        home.write(
            "approvals.jsonl",
            &format!(
                "{}\n{}\n{}\n{}\nnot json\n",
                serde_json::json!({"at": session.created_at, "session_id": session.id, "source": "static", "detail": "static rule", "command": "ls"}),
                serde_json::json!({"at": session.created_at, "source": "judge", "detail": "AI review (m): ok", "command": "cargo test"}),
                serde_json::json!({"at": 1, "source": "user", "detail": "old", "command": "rm -rf x"}),
                serde_json::json!({"at": session.created_at, "session_id": other_session, "source": "user", "detail": "other", "command": "git push"}),
            ),
        );
        // Runtime 自己的任务记录大半字段是私有的，按它落盘的样子写 JSON。
        let tasks = serde_json::json!([{
            "id": task, "session_id": session.id, "turn_id": null, "agent_id": root,
            "event_start_sequence": 0, "status": "completed", "workspace": workspace,
            "profile": null, "model": null, "prompt_excerpt": null, "origin_client": null,
            "pid": null, "created_at": 10, "started_at": null, "completed_at": 11,
            "exit_code": 0, "failure_domain": null, "error": null
        }]);
        home.write("runtime/tasks.json", &tasks.to_string());
        let interactions = vec![RuntimeInteraction {
            id: Uuid::new_v4(),
            task_id: task,
            kind: InteractionKind::Approval {
                description: format!(
                    "curl -H 'Authorization: Bearer {SECRET}' https://api.invalid"
                ),
                always_allow_available: true,
            },
            status: InteractionStatus::Resolved,
            resolution: Some(InteractionResolution::Deny),
            created_at: 11,
            resolved_at: Some(12),
        }];
        home.write(
            "runtime/interactions.json",
            &serde_json::to_string(&interactions).unwrap(),
        );
        home.write(
            "runtime/agents.json",
            &serde_json::to_string(&vec![
                stored_agent_json(root, None, task, &workspace),
                stored_agent_json(child, Some(root), task, &workspace),
            ])
            .unwrap(),
        );
        let attribution = |session_id: Uuid, after: &str, path: &str| DiffAttributionRecord {
            id: Uuid::new_v4(),
            before_snapshot_id: "snap-a".to_owned(),
            after_snapshot_id: after.to_owned(),
            workspace: workspace.clone(),
            session_id: Some(session_id),
            turn_id: None,
            task_id: task,
            agent_id: root,
            tool: "edit_file".to_owned(),
            paths: vec![path.to_owned()],
            confidence: AttributionConfidence::ToolWindow,
            created_at: 15,
        };
        home.write(
            "runtime/diff-attributions.json",
            &serde_json::to_string(&vec![
                attribution(session.id, "snap-b", "src/lib.rs"),
                attribution(other_session, "snap-z", "other.rs"),
            ])
            .unwrap(),
        );
        let verification = |snapshot: &str| DiffVerificationRecord {
            id: Uuid::new_v4(),
            snapshot_id: snapshot.to_owned(),
            workspace: workspace.clone(),
            command: "cargo test".to_owned(),
            exit_code: Some(0),
            outcome: VerificationOutcome::Passed,
            summary: "ok".to_owned(),
            created_at: 16,
        };
        home.write(
            "runtime/diff-verifications.json",
            &serde_json::to_string(&vec![verification("snap-b"), verification("snap-z")]).unwrap(),
        );
        let reviews = vec![DiffReviewRecord {
            id: Uuid::new_v4(),
            snapshot_id: "snap-b".to_owned(),
            workspace: workspace.clone(),
            path: "src/lib.rs".to_owned(),
            decision: ReviewDecision::Accepted,
            note: Some("看过了".to_owned()),
            created_at: 17,
        }];
        home.write(
            "runtime/diff-reviews.json",
            &serde_json::to_string(&reviews).unwrap(),
        );
        std::fs::create_dir_all(home.0.join("runtime/recovery/snap-b-deadbeef")).unwrap();
        std::fs::create_dir_all(home.0.join("runtime/recovery/snap-z-cafe")).unwrap();
        // 检查点回退的回收区按会话 id 命名：本会话的算，别的会话的不算。
        std::fs::create_dir_all(home.0.join(format!(
            "runtime/recovery/rewind-{}-feed",
            session.id.simple()
        )))
        .unwrap();
        std::fs::create_dir_all(home.0.join(format!(
            "runtime/recovery/rewind-{}-beef",
            Uuid::new_v4().simple()
        )))
        .unwrap();
        (home, session, workspace)
    }

    #[test]
    fn a_session_report_joins_all_sources_and_keeps_other_sessions_out() {
        let (home, session, _workspace) = seeded_home();
        let report = build_report(
            &home.0,
            &Scope {
                session: Some(session.id.to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(report.schema_version, SCHEMA_VERSION);
        assert_eq!(report.sessions.len(), 1);
        let audit = &report.sessions[0];

        let approvals = audit
            .approvals
            .iter()
            .map(|entry| (entry.source.as_str(), entry.matched_by))
            .collect::<Vec<_>>();
        assert_eq!(
            approvals,
            vec![("static", MATCHED_BY_SESSION), ("judge", MATCHED_BY_WINDOW)]
        );
        assert_eq!(report.summary.approvals_matched_by_time_window, 1);
        assert_eq!(report.summary.approvals_unparsable, 1);

        assert_eq!(audit.interactions.len(), 1);
        assert_eq!(audit.interactions[0].kind, "approval");
        assert_eq!(audit.interactions[0].resolution, Some("deny"));
        assert!(!audit.interactions[0].description.contains(SECRET));
        assert_eq!(report.summary.human_decisions.deny, 1);

        assert_eq!(audit.hook_denials.len(), 1);
        assert_eq!(audit.hook_denials[0].hook, "no-rm");
        assert_eq!(audit.hook_denials[0].tool.as_deref(), Some("run_command"));
        assert_eq!(audit.hook_denials[0].reason, "不许删东西\n第二行");

        assert_eq!(audit.verification.required, vec!["cargo test"]);
        assert_eq!(audit.verification.evidence.len(), 1);
        assert_eq!(audit.verification.snapshot_bound.len(), 1);
        assert_eq!(audit.verification.snapshot_bound[0].snapshot_id, "snap-b");
        // 检查点证据与快照级记录是同一次验证，只算一次。
        assert_eq!(report.summary.verifications.passed, 1);

        assert_eq!(
            audit.subagents.len(),
            1,
            "只列子 Agent，根 Agent 就是会话本身"
        );
        assert_eq!(audit.subagents[0].verifier_passed, Some(true));
        assert_eq!(report.summary.subagent_verdicts.passed, 1);

        assert_eq!(audit.changes.len(), 1);
        assert_eq!(audit.changes[0].agent, "root");
        assert_eq!(audit.files_changed, vec!["src/lib.rs"]);
        assert_eq!(audit.reviews.len(), 1);
        assert_eq!(
            audit.reverts.len(),
            2,
            "one snapshot revert plus this session's rewind"
        );
        assert!(
            audit
                .reverts
                .iter()
                .any(|entry| entry.snapshot_id == "rewind" && entry.recovery_dir.ends_with("-feed")),
            "{:?}",
            audit.reverts
        );
        assert!(
            audit
                .reverts
                .iter()
                .any(|entry| entry.snapshot_id == "snap-b"
                    && entry.recovery_dir == "snap-b-deadbeef"),
            "{:?}",
            audit.reverts
        );
        assert_eq!(audit.input_tokens, Some(100));
        assert_eq!(report.summary.input_tokens, 100);
        assert_eq!(audit.status.as_deref(), Some("completed"));
    }

    #[test]
    fn markdown_and_json_carry_no_credentials_and_render_in_every_language() {
        let (home, session, _workspace) = seeded_home();
        let report = build_report(
            &home.0,
            &Scope {
                session: Some(session.id.to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains(SECRET));
        assert!(json.contains("\"schema_version\":1"));
        for language in [Language::ZhCn, Language::En, Language::Ja] {
            let markdown = render_markdown(&report, language);
            assert!(!markdown.contains(SECRET));
            assert!(markdown.contains("snap-b-deadbeef"));
            assert!(markdown.contains("`src/lib.rs`"));
            assert!(markdown.contains("no-rm"));
        }
        let zh = render_markdown(&report, Language::ZhCn);
        assert!(zh.contains("# WillDeep 审计报告"));
        assert!(zh.contains("| 审批放行（按来源） | 2（judge 1 · static 1） |"));
        // 多行的 hook 理由折成一行，表格不被撕开。
        assert!(zh.contains("| no-rm | run_command | 不许删东西 第二行 |"));
    }

    #[test]
    fn workspace_and_time_scopes_filter_sessions() {
        let (home, session, workspace) = seeded_home();
        let store = SessionStore::new(&home.0);
        let mut elsewhere = Session::new(home.0.join("elsewhere"), None, "别的活");
        store.save(&mut elsewhere).unwrap();

        let by_workspace = build_report(
            &home.0,
            &Scope {
                workspace: Some(workspace.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            by_workspace
                .sessions
                .iter()
                .map(|audit| audit.id)
                .collect::<Vec<_>>(),
            vec![session.id]
        );

        let everything_recent = build_report(
            &home.0,
            &Scope {
                since: Some(session.created_at.saturating_sub(60)),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(everything_recent.sessions.len(), 2);

        let nothing = build_report(
            &home.0,
            &Scope {
                until: Some(10),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(nothing.sessions.is_empty());
        assert_eq!(nothing.summary.sessions, 0);
        assert!(render_markdown(&nothing, Language::En).contains("No sessions in scope."));

        let latest = build_report(&home.0, &Scope::default()).unwrap();
        assert_eq!(latest.sessions.len(), 1);
    }

    #[test]
    fn time_arguments_accept_dates_and_unix_timestamps_only() {
        assert_eq!(parse_time("2026-09-20").unwrap(), 1_789_862_400);
        assert_eq!(parse_time("1789862400").unwrap(), 1_789_862_400);
        assert!(parse_time("last week").is_err());
    }

    #[test]
    fn an_empty_home_exports_an_empty_report() {
        let home = Home::new();
        let report = build_report(&home.0, &Scope::default()).unwrap();
        assert!(report.sessions.is_empty());
        assert_eq!(report.summary.approvals, 0);
        assert!(
            build_report(
                &home.0,
                &Scope {
                    session: Some("latest".to_owned()),
                    ..Default::default()
                }
            )
            .is_err(),
            "点名 latest 而一个会话都没有，应当报错而不是空报告"
        );
    }
}
