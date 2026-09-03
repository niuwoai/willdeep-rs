//! Execution of one dispatched worker: the attempt loop, the isolated child
//! agent, the verifier the runtime — not the model — uses to decide done, and
//! the file claims that stop two writers from racing on the same lines.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::agent::{Agent, AgentConfig, AgentError, AgentEvent, AgentInstructionInbox, EventSink};
use crate::judge::SafetyJudge;
use crate::tools::{ApprovalMode, ToolRegistry};

use super::audit::{CitationAudit, audit_citations};
use super::brief::compose_brief;
use super::text::bounded;
use super::types::{SubagentProfile, SubagentShell, TaskPacket, TaskVerifier};

/// Bytes of digested verifier output fed back to a worker between attempts.
const MAX_VERIFIER_DIGEST_BYTES: usize = 6 * 1024;

/// Seconds a verifier command may run before it counts as a failed attempt.
const VERIFIER_TIMEOUT_SECONDS: u64 = 900;

/// The last failing verdict of a verified run, kept so the failure report can
/// name the command, the attempt count and the real output.
#[derive(Clone, Debug, PartialEq, Eq)]
struct VerifierOutcome {
    command: String,
    attempts: usize,
    last_digest: String,
}

struct ChildEventSink {
    id: uuid::Uuid,
    parent: Arc<dyn EventSink>,
}

#[async_trait::async_trait]
impl EventSink for ChildEventSink {
    async fn emit(&self, event: AgentEvent) {
        let event = match event {
            AgentEvent::TurnStarted { turn } => {
                Some(AgentEvent::SubagentTurnStarted { id: self.id, turn })
            }
            AgentEvent::ToolRequested(call) => Some(AgentEvent::SubagentToolRequested {
                id: self.id,
                name: call.name,
            }),
            AgentEvent::ToolCompleted { call, is_error, .. } => {
                Some(AgentEvent::SubagentToolCompleted {
                    id: self.id,
                    name: call.name,
                    is_error,
                })
            }
            AgentEvent::Usage(usage) => Some(AgentEvent::SubagentUsage { id: self.id, usage }),
            _ => None,
        };
        if let Some(event) = event {
            self.parent.emit(event).await;
        }
    }
}

/// 会改动工作区的工具。没有已批准写集合时，它们不进 Worker 的工具面。
const WRITE_TOOLS: &[&str] = &["create_file", "edit_file", "create_worktree"];

/// Files one running worker has claimed. Released on drop, so a panic, a
/// timeout or a cancelled run never leaves a file locked behind it.
struct FileClaim {
    claimed: Arc<Mutex<BTreeSet<PathBuf>>>,
    files: BTreeSet<PathBuf>,
}

/// 让 catalog 的测试够得着这道锁：并发上限与写冲突是两条独立的门，测试要能
/// 分别按一按，才说得清哪条在起作用。
#[cfg(test)]
pub(super) fn acquire_file_claim_for_test(
    claimed: &Arc<Mutex<BTreeSet<PathBuf>>>,
    files: &BTreeSet<PathBuf>,
) -> Result<Option<impl Sized>, AgentError> {
    FileClaim::acquire(claimed, files)
}

impl FileClaim {
    fn acquire(
        claimed: &Arc<Mutex<BTreeSet<PathBuf>>>,
        files: &BTreeSet<PathBuf>,
    ) -> Result<Option<Self>, AgentError> {
        if files.is_empty() {
            return Ok(None);
        }
        let mut held = claimed
            .lock()
            .map_err(|_| AgentError::Subagent("subagent file claims are unavailable".to_owned()))?;
        let conflicts = files.intersection(&held).cloned().collect::<Vec<_>>();
        if !conflicts.is_empty() {
            return Err(AgentError::Subagent(format!(
                "another running subagent already claimed {}; wait for it to finish or dispatch this work over different files",
                conflicts
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        held.extend(files.iter().cloned());
        drop(held);
        Ok(Some(Self {
            claimed: claimed.clone(),
            files: files.clone(),
        }))
    }
}

impl Drop for FileClaim {
    fn drop(&mut self) {
        if let Ok(mut held) = self.claimed.lock() {
            for file in &self.files {
                held.remove(file);
            }
        }
    }
}

/// One dispatched worker, fully resolved: everything the runner needs and
/// nothing it has to ask the catalog for again.
#[derive(Clone)]
pub(super) struct SubagentRun {
    pub(super) workspace: PathBuf,
    pub(super) profile: SubagentProfile,
    pub(super) prompt: String,
    pub(super) task: Option<TaskPacket>,
    pub(super) approved_targets: Option<BTreeSet<PathBuf>>,
    pub(super) verifier: Option<TaskVerifier>,
    pub(super) max_attempts: usize,
    pub(super) claimed_files: Arc<Mutex<BTreeSet<PathBuf>>>,
    pub(super) skills: Option<Arc<crate::skills::SkillCatalog>>,
    pub(super) safety_judge: Option<Arc<dyn SafetyJudge>>,
    pub(super) approved_command: Option<String>,
    /// 已连接的 MCP 服务。只有兜底工种拿得到（在 catalog 那边决定），因为窄
    /// 工种的价值就在于范围窄。
    pub(super) mcp: Option<Arc<crate::mcp::McpRegistry>>,
    /// 父会话的 always-allow 存储路径，见
    /// [`SubagentCatalog::with_always_allow_store`](super::catalog::SubagentCatalog::with_always_allow_store)。
    pub(super) always_allow_store: Option<PathBuf>,
}

/// Run a worker to a verdict.
///
/// Without a verifier this is the old behaviour: one pass, the worker's final
/// text is the report. With one, the runtime — not the model — decides done:
/// the command runs after every attempt, a pass ends the run, a failure is
/// digested and handed back as the next attempt's brief, and an exhausted
/// attempt budget is a failure the parent must escalate. A worker that
/// declares itself finished without the verifier ever having run does not get
/// the benefit of the doubt; the runtime runs it anyway.
pub(super) async fn run_subagent(
    run: SubagentRun,
    lifecycle_sink: Arc<dyn EventSink>,
    agent_id: uuid::Uuid,
    instruction_inbox: Option<Arc<AgentInstructionInbox>>,
) -> Result<String, AgentError> {
    let SubagentRun {
        workspace,
        profile,
        prompt,
        task,
        approved_targets,
        verifier,
        max_attempts,
        claimed_files,
        skills,
        safety_judge,
        approved_command,
        mcp,
        always_allow_store,
    } = run;
    let _claim = match &approved_targets {
        Some(targets) => FileClaim::acquire(&claimed_files, targets)?,
        None => None,
    };
    // Read the anchor before the worker touches anything: with the commit the
    // run started from, this record replays.
    let repo_commit = head_commit(&workspace).await;
    let verdict = |passed: Option<bool>, attempts: usize, audit: &CitationAudit| {
        AgentEvent::SubagentVerdict {
            id: agent_id,
            repo_commit: repo_commit.clone(),
            verifier_command: verifier.as_ref().map(|verifier| verifier.command.clone()),
            verifier_passed: passed,
            attempts,
            claims_checked: audit.checked,
            claims_unverifiable: audit.unverifiable.len(),
        }
    };
    let mut brief = compose_brief(
        &prompt,
        task.as_ref(),
        &workspace,
        &profile,
        skills.as_deref(),
    )
    .await;
    let review_goal = task
        .as_ref()
        .map(|task| task.goal.as_str())
        .filter(|goal| !goal.trim().is_empty())
        .unwrap_or(prompt.as_str());
    let command_review_context = bounded(
        crate::judge::redact_credentials(&format!(
            "delegated trade={} purpose={} goal={review_goal}",
            profile.id, profile.purpose
        )),
        2 * 1024,
    );
    let attempts = if verifier.is_some() { max_attempts } else { 1 };
    let mut outcome: Option<VerifierOutcome> = None;

    for attempt in 1..=attempts {
        let report = run_once(
            &workspace,
            &profile,
            &approved_targets,
            verifier.as_ref(),
            brief.clone(),
            lifecycle_sink.clone(),
            agent_id,
            instruction_inbox.clone(),
            safety_judge.clone(),
            approved_command.clone(),
            command_review_context.clone(),
            mcp.clone(),
            always_allow_store.clone(),
        )
        .await?;
        let Some(verifier) = verifier.as_ref() else {
            // No verifier: unverified, which is not the same as failed, and
            // the telemetry has to keep the two apart. What can still be
            // checked without a command is what the report cites.
            let audit = audit_citations(&workspace, &report).await;
            lifecycle_sink.emit(verdict(None, attempt, &audit)).await;
            return Ok(match audit.note() {
                Some(note) => format!("{report}\n\n{note}"),
                None => report,
            });
        };
        let result = run_verifier(&workspace, verifier).await?;
        if result.passed {
            lifecycle_sink
                .emit(verdict(Some(true), attempt, &CitationAudit::default()))
                .await;
            return Ok(format!(
                "{report}\n\n<verifier command={:?} attempts={attempt} verdict=\"passed\" />",
                verifier.command
            ));
        }
        outcome = Some(VerifierOutcome {
            command: verifier.command.clone(),
            attempts: attempt,
            last_digest: result.digest.clone(),
        });
        if attempt < attempts {
            brief = format!(
                "{brief}\n\n<verifier-failure attempt=\"{attempt}\" command={:?} exit_code=\"{}\">\nYour previous attempt did not pass. This is the digested output; the assertions below are verbatim, not paraphrased. Fix the cause, do not weaken the check.\n\n{}\n</verifier-failure>",
                verifier.command, result.exit_code, result.digest
            );
        }
    }

    let outcome = outcome.expect("a verified run records an outcome before exhausting attempts");
    lifecycle_sink
        .emit(verdict(
            Some(false),
            outcome.attempts,
            &CitationAudit::default(),
        ))
        .await;
    Err(AgentError::Subagent(format!(
        "worker did not reach a verified pass: {} failed after {} attempt(s). Escalate — retry this agent with the parent model, or re-dispatch with a wider task packet.\n\n{}",
        outcome.command, outcome.attempts, outcome.last_digest
    )))
}

/// The commit a run started from. Best effort: a workspace that is not a Git
/// repository is a fine place to delegate work, it just cannot be replayed.
async fn head_commit(workspace: &Path) -> Option<String> {
    let output = tokio::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(workspace)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let commit = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (commit.len() == 40 && commit.chars().all(|value| value.is_ascii_hexdigit())).then_some(commit)
}

/// One model pass: a fresh isolated agent, so a failed attempt never leaves
/// its own dead ends in the next attempt's context. What carries over is the
/// digested verifier output, which is the only part worth carrying.
#[allow(clippy::too_many_arguments)]
async fn run_once(
    workspace: &Path,
    profile: &SubagentProfile,
    approved_targets: &Option<BTreeSet<PathBuf>>,
    verifier: Option<&TaskVerifier>,
    brief: String,
    lifecycle_sink: Arc<dyn EventSink>,
    agent_id: uuid::Uuid,
    instruction_inbox: Option<Arc<AgentInstructionInbox>>,
    safety_judge: Option<Arc<dyn SafetyJudge>>,
    approved_command: Option<String>,
    command_review_context: String,
    mcp: Option<Arc<crate::mcp::McpRegistry>>,
    always_allow_store: Option<PathBuf>,
) -> Result<String, AgentError> {
    let approval = if profile.shell.uses_intelligent_review() {
        ApprovalMode::Smart
    } else if approved_targets.is_some() {
        ApprovalMode::WorkspaceAccess
    } else {
        ApprovalMode::Strict
    };
    // 可选写范围的工种没带目标时，写工具当场从工具面上摘掉。
    //
    // 只靠 `with_write_targets(None)` 是不够的：那个 `None` 的意思是「不限制
    // 写到哪」，正是主 Agent 的用法。给一个没声明写集合的 Worker 注册写工具，
    // 等于把整个工作区交给它——**工具变多不等于权限变大**这句话得由这里兑现。
    let has_targets = approved_targets
        .as_ref()
        .is_some_and(|targets| !targets.is_empty());
    let mut allowed = profile.tool_names.clone();
    if !profile.write_scope.writes_this_run(has_targets) {
        allowed.retain(|name| !WRITE_TOOLS.contains(&name.as_str()));
    }
    let mut tools = ToolRegistry::new(workspace, approval)?
        .with_allowed_tools(allowed)
        .with_write_targets(approved_targets.clone());
    if let Some(path) = always_allow_store {
        // 坏掉的规则文件不该让一次派工失败：Worker 退回「什么都要批」，而它
        // 没有审批 UI，于是照常报告哪一步被拦下，由父会话去请人批。
        tools = tools.with_always_allow_store(path)?;
    }
    if let Some(mcp) = mcp {
        // 动态网关：Worker 按需检索工具，而不是每一轮都为整份目录付上下文。
        // 调用仍走各自的审批路径——多一条查询通道不等于多一份权限。
        tools = tools.with_mcp(mcp);
    }
    if let Some(judge) = safety_judge {
        tools = tools.with_safety_judge(judge);
    }
    tools.set_task_context(&command_review_context);
    if let Some(limit) = profile.tool_output_limit {
        tools = tools.with_tool_output_limit(limit);
    }
    // The shell a worker gets is decided here and never widened at runtime.
    // `ReadOnlyGit` is the one policy that is a *shape* rather than a literal:
    // composing the query is the job, so the rule constrains what the command
    // may be, not which command it is.
    tools = match profile.shell {
        SubagentShell::ReadOnlyGit => tools.with_read_only_git_shell(true),
        SubagentShell::None | SubagentShell::VerifierOnly | SubagentShell::VerifierOptional => {
            tools.with_command_allowlist(Some(
                verifier
                    .map(|verifier| HashSet::from([verifier.command.trim().to_owned()]))
                    .unwrap_or_default(),
            ))
        }
        SubagentShell::Reviewed
        | SubagentShell::ReviewedVerifierOnly
        | SubagentShell::ReviewedVerifierOptional => tools
            .with_reviewed_subagent_shell(true)
            .with_preapproved_commands(
                approved_command
                    .map(|command| HashSet::from([command]))
                    .unwrap_or_default(),
            ),
    };
    let boundary = format!(
        "You are a WillDeep subagent working in {}. You do not see the parent conversation, cannot ask the user, and cannot spawn another agent. Your final response is the report returned to the parent. Read-only and bounded commands may pass static checks. Other non-destructive commands may be reviewed by an AI safety judge. Destructive or credential-sensitive commands are outside that judge's authority. If command review is denied or unavailable, report the exact command to the parent; the parent may ask the human and respawn an ops_runner with that exact target_command.",
        workspace.display()
    );
    // A relay-hosted trade already carries its job prompt server-side. Sending
    // the client's copy too would put two descriptions of the same trade in
    // one context — and when they drift, the worker gets to pick.
    let system_prompt = if profile.hosted_job_prompt {
        boundary
    } else {
        format!("{boundary}\n\n{}", profile.capability_prompt)
    };
    let timeout_seconds = profile.timeout_seconds;
    let mut agent = Agent::new(
        profile.provider.clone(),
        tools,
        AgentConfig {
            max_turns: profile.max_turns,
            system_prompt,
            context_window: profile.context_window,
            token_budget: profile.token_budget,
        },
    )
    .with_event_sink(Arc::new(ChildEventSink {
        id: agent_id,
        parent: lifecycle_sink,
    }));
    if let Some(inbox) = instruction_inbox {
        agent = agent.with_instruction_inbox(inbox);
    }
    let run = Box::pin(agent.run(brief));
    let outcome = if let Some(seconds) = timeout_seconds {
        tokio::time::timeout(Duration::from_secs(seconds), run)
            .await
            .map_err(|_| {
                AgentError::Subagent(format!("subagent timed out after {seconds} seconds"))
            })??
    } else {
        run.await?
    };
    Ok(outcome.final_text)
}

struct VerifierResult {
    passed: bool,
    exit_code: i32,
    digest: String,
}

/// Run the verifier as a runtime action: no model turn, no approval card, no
/// interpretation. Its exit code is the whole verdict.
async fn run_verifier(
    workspace: &Path,
    verifier: &TaskVerifier,
) -> Result<VerifierResult, AgentError> {
    let mut command = crate::tools::platform_shell(&verifier.command);
    command
        .current_dir(workspace)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let child = command.spawn().map_err(|error| {
        AgentError::Subagent(format!("verifier command failed to start: {error}"))
    })?;
    let output = tokio::time::timeout(
        Duration::from_secs(VERIFIER_TIMEOUT_SECONDS),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| {
        AgentError::Subagent(format!(
            "verifier command timed out after {VERIFIER_TIMEOUT_SECONDS} seconds: {}",
            verifier.command
        ))
    })?
    .map_err(|error| AgentError::Subagent(format!("verifier command failed: {error}")))?;
    let exit_code = output.status.code().unwrap_or(-1);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(VerifierResult {
        passed: exit_code == verifier.expected_exit_code(),
        exit_code,
        digest: digest_failure_output(&combined),
    })
}

/// Compress a failing build/test log to what a small-window worker can act
/// on: the failure-bearing lines with a little context, plus the tail. Pure
/// string work — no model is spent deciding what a small model gets to read.
fn digest_failure_output(output: &str) -> String {
    const MARKERS: &[&str] = &[
        "error",
        "failed",
        "failure",
        "panicked",
        "assert",
        "expected",
        "exception",
        "traceback",
        "cannot",
        "undefined",
        "unresolved",
        "warning: unused",
    ];
    let lines = output.lines().collect::<Vec<_>>();
    let mut keep = vec![false; lines.len()];
    for (index, line) in lines.iter().enumerate() {
        let lowered = line.to_ascii_lowercase();
        if MARKERS.iter().any(|marker| lowered.contains(marker)) {
            // One line of lead-in, three of follow-on: enough to carry a
            // compiler's "expected X, found Y" or an assertion's diff.
            let start = index.saturating_sub(1);
            let end = (index + 3).min(lines.len().saturating_sub(1));
            for slot in keep.iter_mut().take(end + 1).skip(start) {
                *slot = true;
            }
        }
    }
    let mut focused = Vec::new();
    let mut skipping = false;
    for (index, line) in lines.iter().enumerate() {
        if keep[index] {
            focused.push((*line).to_owned());
            skipping = false;
        } else if !skipping {
            focused.push("…".to_owned());
            skipping = true;
        }
    }
    let tail = lines
        .iter()
        .rev()
        .take(20)
        .rev()
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    let focused = focused.join("\n");
    let digest = if focused.trim_matches(['…', '\n', ' ']).is_empty() {
        tail
    } else {
        format!("{focused}\n\n--- tail ---\n{tail}")
    };
    bounded(digest, MAX_VERIFIER_DIGEST_BYTES)
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;

    use super::*;
    use crate::background::BackgroundTaskRegistry;
    use crate::provider::{Provider, ProviderError};
    use crate::subagent::test_support::{AllowingJudge, CaptureSink, ReportProvider, fixture};
    use crate::subagent::types::SpawnAgentArgs;
    use crate::subagent::{SubagentCatalog, builtin_profiles};
    use crate::types::{Completion, Message, ToolDefinition};

    /// With the job prompt hosted, the client sends the boundary paragraph and
    /// nothing else: two copies of a trade description that can drift is worse
    /// than one, and the boundary is the half the client owns.
    #[tokio::test]
    async fn a_hosted_trade_sends_the_boundary_without_a_second_job_prompt() {
        struct PromptProbe(Arc<Mutex<Vec<String>>>);

        #[async_trait]
        impl Provider for PromptProbe {
            async fn complete(
                &self,
                messages: &[Message],
                _tools: &[ToolDefinition],
            ) -> Result<Completion, ProviderError> {
                self.0
                    .lock()
                    .expect("prompts")
                    .push(messages[0].content.clone());
                Ok(Completion {
                    content: "report".to_owned(),
                    tool_calls: Vec::new(),
                    finish_reason: Some("stop".to_owned()),
                    usage: None,
                })
            }
        }

        let seen = Arc::new(Mutex::new(Vec::new()));
        let provider: Arc<dyn Provider> = Arc::new(PromptProbe(seen.clone()));
        let root = std::env::temp_dir().join(format!("willdeep-hosted-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("workspace");
        let mut profiles = builtin_profiles(provider);
        for profile in &mut profiles {
            profile.hosted_job_prompt = profile.id == "scout";
        }
        let catalog =
            SubagentCatalog::new(&root, profiles, Arc::new(BackgroundTaskRegistry::default()));
        for id in ["scout", "reader"] {
            catalog
                .run(
                    SpawnAgentArgs {
                        prompt: "look".to_owned(),
                        profile: Some(id.to_owned()),
                        run_in_background: Some(false),
                        ..SpawnAgentArgs::default()
                    },
                    None,
                )
                .await
                .expect("run");
        }
        let prompts = seen.lock().expect("prompts").clone();
        assert!(
            prompts[0].contains("cannot spawn another agent") && !prompts[0].contains("LOCATION"),
            "a hosted trade keeps the boundary and drops the client job prompt: {}",
            prompts[0]
        );
        assert!(
            prompts[1].contains("cannot spawn another agent") && prompts[1].contains("READING"),
            "an unhosted trade still gets its job prompt from the client: {}",
            prompts[1]
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    /// The whole point of the verifier loop: a worker's own claim of success
    /// is worth nothing, and a run only ends when the command says so. This
    /// worker claims "done" every time but only actually fixes anything on
    /// its third pass, so the run must take exactly three attempts — never
    /// one on the model's say-so.
    #[tokio::test]
    async fn a_run_ends_when_the_verifier_says_so_not_when_the_worker_claims_done() {
        /// A worker that reports success on every attempt and does the real
        /// work only on the third.
        struct SlowFixer {
            marker: PathBuf,
            attempts: Arc<Mutex<usize>>,
        }

        #[async_trait]
        impl Provider for SlowFixer {
            async fn complete(
                &self,
                _messages: &[Message],
                _tools: &[ToolDefinition],
            ) -> Result<Completion, ProviderError> {
                let mut attempts = self.attempts.lock().unwrap();
                *attempts += 1;
                if *attempts >= 3 {
                    std::fs::write(&self.marker, "fixed").expect("marker");
                }
                Ok(Completion {
                    content: "all done, everything passes".to_owned(),
                    tool_calls: Vec::new(),
                    finish_reason: Some("stop".to_owned()),
                    usage: None,
                })
            }
        }

        let root = std::env::temp_dir().join(format!("willdeep-verify-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("workspace");
        let marker = root.join("fixed.txt");
        let attempts = Arc::new(Mutex::new(0usize));
        let provider: Arc<dyn Provider> = Arc::new(SlowFixer {
            marker: marker.clone(),
            attempts: attempts.clone(),
        });
        let catalog = SubagentCatalog::new(
            &root,
            builtin_profiles(provider),
            Arc::new(BackgroundTaskRegistry::default()),
        );
        let report = catalog
            .run(
                SpawnAgentArgs {
                    prompt: "make it pass".to_owned(),
                    profile: Some("scout".to_owned()),
                    run_in_background: Some(false),
                    task: Some(TaskPacket {
                        goal: "reach a verified pass".to_owned(),
                        verifier: Some(TaskVerifier {
                            command: format!("test -f {}", marker.display()),
                            expected_exit_code: None,
                        }),
                        ..TaskPacket::default()
                    }),
                    ..SpawnAgentArgs::default()
                },
                None,
            )
            .await
            .expect("verified run");
        assert!(
            report.contains("verdict=\"passed\"") && report.contains("attempts=3"),
            "the runtime must keep going until the verifier passes, got: {report}"
        );
        assert_eq!(
            *attempts.lock().unwrap(),
            3,
            "two claimed-but-unverified passes must not have ended the run"
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    /// Telemetry has to keep three outcomes apart: verified, failed
    /// verification, and never verified at all. Collapsing the third into
    /// either of the first two is how a delegation metric starts flattering
    /// itself — every unverified report becomes a success (or a failure) it
    /// never earned.
    #[tokio::test]
    async fn every_run_reports_a_verdict_and_unverified_is_not_a_pass() {
        let (catalog, root) = fixture();
        let sink = Arc::new(CaptureSink::default());
        let catalog = catalog
            .with_event_sink(sink.clone())
            .with_safety_judge(Arc::new(AllowingJudge));

        // 1. No verifier at all.
        catalog
            .run(
                SpawnAgentArgs {
                    prompt: "look around".to_owned(),
                    profile: Some("scout".to_owned()),
                    run_in_background: Some(false),
                    ..SpawnAgentArgs::default()
                },
                None,
            )
            .await
            .expect("unverified run");
        // 2. A verifier that never passes.
        catalog
            .run(
                SpawnAgentArgs {
                    prompt: "fix".to_owned(),
                    profile: Some("scout".to_owned()),
                    run_in_background: Some(false),
                    task: Some(TaskPacket {
                        goal: "never reachable".to_owned(),
                        verifier: Some(TaskVerifier {
                            command: "test -f definitely-not-here".to_owned(),
                            expected_exit_code: None,
                        }),
                        max_attempts: Some(2),
                        ..TaskPacket::default()
                    }),
                    ..SpawnAgentArgs::default()
                },
                None,
            )
            .await
            .expect_err("failing verifier");

        let events = sink.0.lock().unwrap();
        let verdicts = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::SubagentVerdict {
                    verifier_passed,
                    attempts,
                    ..
                } => Some((*verifier_passed, *attempts)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            verdicts,
            vec![(None, 1), (Some(false), 2)],
            "an unverified run reports None, a failed one reports Some(false) with its attempt count"
        );
        drop(events);
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    /// An exhausted attempt budget is a failure the parent has to see — not a
    /// report that quietly reads like success.
    #[tokio::test]
    async fn an_exhausted_attempt_budget_fails_the_run_and_asks_for_escalation() {
        let root = std::env::temp_dir().join(format!("willdeep-verify-x-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("workspace");
        let provider: Arc<dyn Provider> = Arc::new(ReportProvider);
        let catalog = SubagentCatalog::new(
            &root,
            builtin_profiles(provider),
            Arc::new(BackgroundTaskRegistry::default()),
        )
        .with_safety_judge(Arc::new(AllowingJudge));
        let error = catalog
            .run(
                SpawnAgentArgs {
                    prompt: "make it pass".to_owned(),
                    profile: Some("scout".to_owned()),
                    run_in_background: Some(false),
                    task: Some(TaskPacket {
                        goal: "never reachable".to_owned(),
                        verifier: Some(TaskVerifier {
                            command: "echo 'error: still broken' >&2; exit 1".to_owned(),
                            expected_exit_code: None,
                        }),
                        max_attempts: Some(2),
                        ..TaskPacket::default()
                    }),
                    ..SpawnAgentArgs::default()
                },
                None,
            )
            .await
            .expect_err("an unverified run must fail");
        let message = error.to_string();
        assert!(
            message.contains("2 attempt(s)") && message.contains("Escalate"),
            "the failure must name the attempt count and ask for escalation, got: {message}"
        );
        assert!(
            message.contains("still broken"),
            "the failing output must survive into the report, got: {message}"
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    /// Two workers editing the same file would race line-by-line. The second
    /// claim is refused with the conflicting path named, so the parent can
    /// re-cut the work instead of discovering the collision in the diff.
    #[test]
    fn overlapping_file_claims_are_refused_and_released_on_drop() {
        let claimed = Arc::new(Mutex::new(BTreeSet::new()));
        let first = BTreeSet::from([PathBuf::from("/w/a.rs"), PathBuf::from("/w/b.rs")]);
        let second = BTreeSet::from([PathBuf::from("/w/b.rs")]);
        let held = FileClaim::acquire(&claimed, &first)
            .expect("first claim")
            .expect("claim guard");
        let Err(conflict) = FileClaim::acquire(&claimed, &second) else {
            panic!("an overlapping claim must be refused");
        };
        assert!(
            conflict.to_string().contains("b.rs"),
            "the refusal must name the contested file, got: {conflict}"
        );
        drop(held);
        assert!(
            FileClaim::acquire(&claimed, &second).is_ok(),
            "a finished worker must release its files"
        );
    }

    /// The digest is what a small-window worker gets to read of a long log:
    /// the failing assertion verbatim, not a summary of it.
    #[test]
    fn the_failure_digest_keeps_the_assertion_and_drops_the_noise() {
        let mut log = String::new();
        for index in 0..500 {
            log.push_str(&format!("   Compiling crate-{index} v0.1.0\n"));
        }
        log.push_str("thread 'main' panicked at src/lib.rs:42:\nassertion `left == right` failed\n  left: 1\n right: 2\n");
        let digest = digest_failure_output(&log);
        assert!(
            digest.contains("assertion `left == right` failed") && digest.contains("right: 2"),
            "the assertion must survive verbatim: {digest}"
        );
        assert!(
            digest.len() <= MAX_VERIFIER_DIGEST_BYTES + 32,
            "the digest must stay within a worker's budget, got {} bytes",
            digest.len()
        );
        assert!(
            !digest.contains("crate-100"),
            "hundreds of successful compile lines are not evidence: {digest}"
        );
    }
}
