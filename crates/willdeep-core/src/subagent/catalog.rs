//! Dispatch: which trade runs, whether it is allowed to run at all, and what
//! it is handed. The circuit breaker, the write-set approval, the verifier
//! safety gate, the worktree policy and the background lifecycle all live
//! here; the actual execution is [`super::runner`]'s job.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::agent::{
    AgentError, AgentEvent, AgentInstructionInbox, EventSink, SubagentLifecycleStatus,
};
use crate::background::{
    BackgroundTaskKind, BackgroundTaskRegistry, BackgroundTaskStatus, TaskResult,
};
use crate::judge::{JudgeRequest, JudgeVerdict, SafetyJudge};
use crate::provider::Provider;
use crate::safety::CommandSafety;
use crate::subagent_worktree::{
    PreparedSubagentWorkspace, SubagentWorktreeManager, SubagentWorktreePolicy,
    worktree_result_note,
};
use crate::tools::ToolError;

use super::runner::{SubagentRun, run_subagent};
use super::text::bounded_report;
use super::types::{
    PUBLIC_SUBAGENT_IDS, SpawnAgentArgs, SubagentProfile, SubagentWriteScope, TaskVerifier,
    public_profile_id,
};

/// Hard ceiling on the attempt budget a task packet may ask for.
const MAX_ATTEMPTS_CEILING: usize = 6;

#[derive(Clone)]
pub struct SubagentCatalog {
    workspace: PathBuf,
    profiles: BTreeMap<String, SubagentProfile>,
    background: Arc<BackgroundTaskRegistry>,
    sink: Arc<dyn EventSink>,
    failures: Arc<Mutex<BTreeMap<String, usize>>>,
    model_overrides: Arc<Mutex<BTreeMap<uuid::Uuid, String>>>,
    worktrees: Option<SubagentWorktreeManager>,
    /// Files currently claimed by a running writing worker. Two workers whose
    /// declared sets intersect would race on the same lines; the second one
    /// is refused with the conflict named, and the parent decides whether to
    /// wait or to re-cut the work.
    claimed_files: Arc<Mutex<BTreeSet<PathBuf>>>,
    /// Judge consulted for verifier commands the static classifier cannot
    /// decide. A subagent has no approval UI, so an undecidable command with
    /// no judge is refused rather than silently run.
    safety_judge: Option<Arc<dyn SafetyJudge>>,
    /// Skill library used to resolve `task.skill` at dispatch time.
    skills: Option<Arc<crate::skills::SkillCatalog>>,
    /// 每一档兑现成哪个 provider。准入在上一层（[`crate::WorkerTier::requires_admission`]），
    /// 这里只负责兑现：档位说「这活儿值得贵一次」，绑定说「贵成什么样」。
    ///
    /// 没配的档不换模型，只放宽预算——票据白拿不到东西，好过静默降级到一个
    /// 谁也没指定的模型。
    tier_bindings: BTreeMap<crate::WorkerTier, TierBinding>,
}

/// 一个档位兑现出来的模型。
#[derive(Clone)]
pub struct TierBinding {
    pub provider: Arc<dyn Provider>,
    /// 展示与遥测用的模型名；`None` 表示沿用 provider 自己配的那个。
    pub model: Option<String>,
    /// 这一档的上下文预算下限。
    pub window: u64,
    /// 这个模型是否由网关托管职责提示词（见 [`crate::worker_tier::hosts_job_prompt`]）。
    pub hosted_job_prompt: bool,
}

struct SubagentNoopSink;

#[async_trait::async_trait]
impl EventSink for SubagentNoopSink {
    async fn emit(&self, _event: AgentEvent) {}
}

impl SubagentCatalog {
    pub fn new(
        workspace: impl AsRef<Path>,
        profiles: Vec<SubagentProfile>,
        background: Arc<BackgroundTaskRegistry>,
    ) -> Self {
        Self {
            workspace: workspace.as_ref().to_path_buf(),
            profiles: profiles
                .into_iter()
                .map(|profile| (profile.id.clone(), profile))
                .collect(),
            background,
            sink: Arc::new(SubagentNoopSink),
            failures: Arc::new(Mutex::new(BTreeMap::new())),
            model_overrides: Arc::new(Mutex::new(BTreeMap::new())),
            worktrees: None,
            claimed_files: Arc::new(Mutex::new(BTreeSet::new())),
            safety_judge: None,
            skills: None,
            tier_bindings: BTreeMap::new(),
        }
    }

    /// Attach the skill library so a task packet can name a skill and have
    /// its body inlined into the worker's opening message.
    pub fn with_skills(mut self, skills: Arc<crate::skills::SkillCatalog>) -> Self {
        self.skills = Some(skills);
        self
    }

    /// 给某一档绑定兑现用的 provider。
    ///
    /// 三档都可绑：基础档默认沿用工种自己的模型，进阶与专家档由 harness 按
    /// 网关默认表或用户的 `[worker_tiers.*]` 配置填进来。
    pub fn with_tier_binding(mut self, tier: crate::WorkerTier, binding: TierBinding) -> Self {
        self.tier_bindings.insert(tier, binding);
        self
    }

    /// 这一档兑现成什么。没绑就是 `None`。
    pub fn tier_binding(&self, tier: crate::WorkerTier) -> Option<&TierBinding> {
        self.tier_bindings.get(&tier)
    }

    pub fn with_event_sink(mut self, sink: Arc<dyn EventSink>) -> Self {
        self.sink = sink;
        self
    }

    /// Attach the judge that reviews verifier commands the static classifier
    /// cannot decide. Without one, those commands are refused.
    pub fn with_safety_judge(mut self, judge: Arc<dyn SafetyJudge>) -> Self {
        self.safety_judge = Some(judge);
        self
    }

    pub fn with_worktree_root(mut self, root: impl AsRef<Path>) -> Self {
        self.worktrees = Some(SubagentWorktreeManager::new(root.as_ref().to_path_buf()));
        self
    }

    pub fn description(&self) -> String {
        PUBLIC_SUBAGENT_IDS
            .iter()
            .filter_map(|id| self.profiles.get(*id))
            .map(|profile| format!("`{}` — {}", profile.id, profile.purpose))
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn retry_background_agent(
        &self,
        agent_id: uuid::Uuid,
        model: Option<&str>,
    ) -> Result<Option<String>, AgentError> {
        let model = model.map(str::trim).filter(|model| !model.is_empty());
        if model.is_some_and(|model| model.len() > 256) {
            return Err(AgentError::Subagent(
                "model override must contain 1 to 256 bytes".to_owned(),
            ));
        }
        let previous = if let Some(model) = model {
            self.model_overrides
                .lock()
                .expect("subagent model overrides")
                .insert(agent_id, model.to_owned())
        } else {
            None
        };
        let retried = self.background.retry_agent(agent_id);
        if retried.is_none() && model.is_some() {
            let mut overrides = self
                .model_overrides
                .lock()
                .expect("subagent model overrides");
            if let Some(previous) = previous {
                overrides.insert(agent_id, previous);
            } else {
                overrides.remove(&agent_id);
            }
        }
        Ok(retried)
    }

    pub(crate) fn write_scope(&self, id: Option<&str>) -> SubagentWriteScope {
        self.profile(id)
            .map_or(SubagentWriteScope::None, |profile| profile.write_scope)
    }

    pub(crate) fn has_profile(&self, id: &str) -> bool {
        self.resolve_profile_id(id).is_some()
    }

    /// 把一个可能是旧名的工种解析成目录里真实存在的那个。
    ///
    /// 直接命中优先——内部工种（`scout`、`editor` 等）仍然可以精确点名；
    /// 命不中再走公开名映射，让 `reader`、`judge`、`deep` 这些改名前的写法
    /// 继续可用。别人保存的流程不该因为我们换了个称呼就断掉。
    fn resolve_profile_id(&self, id: &str) -> Option<String> {
        let id = id.trim().to_ascii_lowercase();
        if self.profiles.contains_key(&id) {
            return Some(id);
        }
        public_profile_id(&id)
            .map(str::to_owned)
            .filter(|resolved| self.profiles.contains_key(resolved))
    }

    fn profile(&self, id: Option<&str>) -> Option<&SubagentProfile> {
        let resolved = self.resolve_profile_id(id.unwrap_or("generalist"))?;
        self.profiles.get(&resolved)
    }

    pub(crate) async fn run(
        &self,
        args: SpawnAgentArgs,
        approved_targets: Option<BTreeSet<PathBuf>>,
    ) -> Result<String, AgentError> {
        if args.target_command.is_some() {
            return Err(AgentError::Subagent(
                "target_command requires exact parent authorization".to_owned(),
            ));
        }
        self.run_with_id(uuid::Uuid::new_v4(), args, approved_targets, None)
            .await
    }

    pub(crate) async fn run_authorized(
        &self,
        args: SpawnAgentArgs,
        approved_targets: Option<BTreeSet<PathBuf>>,
        approved_command: Option<String>,
    ) -> Result<String, AgentError> {
        self.run_with_id(
            uuid::Uuid::new_v4(),
            args,
            approved_targets,
            approved_command,
        )
        .await
    }

    pub async fn spawn_external_read_only(
        &self,
        id: uuid::Uuid,
        prompt: String,
        label: Option<String>,
        profile: Option<String>,
    ) -> Result<(), AgentError> {
        let profile_id = profile
            .as_deref()
            .unwrap_or("deep")
            .trim()
            .to_ascii_lowercase();
        if crate::WorkerTier::parse(&profile_id).is_some_and(|tier| tier.requires_admission()) {
            return Err(AgentError::Subagent(
                "external expert-tier spawn is disabled; the parent Agent must provide a runtime-validated escalation ticket"
                    .to_owned(),
            ));
        }
        let selected = self.profiles.get(&profile_id).ok_or_else(|| {
            AgentError::Subagent(format!("subagent profile not found: {profile_id}"))
        })?;
        const READ_ONLY_TOOLS: &[&str] = &[
            "search_files",
            "grep_files",
            "list_directory",
            "read_file",
            "git_status",
            "git_diff",
            "git_log",
            "git_blame",
        ];
        if selected.write_scope.writes()
            || selected
                .tool_names
                .iter()
                .any(|name| !READ_ONLY_TOOLS.contains(&name.as_str()))
        {
            return Err(AgentError::Subagent(format!(
                "profile {profile_id} is not eligible for external read-only spawn"
            )));
        }
        self.run_with_id(
            id,
            SpawnAgentArgs {
                prompt,
                label,
                profile: Some(profile_id),
                run_in_background: Some(true),
                ..SpawnAgentArgs::default()
            },
            None,
            None,
        )
        .await?;
        Ok(())
    }

    async fn run_with_id(
        &self,
        agent_id: uuid::Uuid,
        args: SpawnAgentArgs,
        approved_targets: Option<BTreeSet<PathBuf>>,
        approved_command: Option<String>,
    ) -> Result<String, AgentError> {
        let mut profile = self
            .profile(args.profile.as_deref())
            .ok_or_else(|| AgentError::Subagent("no subagent profiles configured".to_owned()))?
            .clone();
        // 职责给提示词、工具和写入边界，档位给模型和上下文预算。
        //
        // 预算只放宽不收窄：工种自己声明的窗口是它完成职责的下限，implementer
        // 的 256K 不该因为跑在基础档上被砍掉。换模型则走档位绑定——专家档已经
        // 在 agent 层过了票据与预算，到这里只负责兑现。
        if let Some(tier) = args
            .worker_tier
            .as_deref()
            .and_then(crate::WorkerTier::parse)
        {
            profile.context_window = profile.context_window.max(tier.context_budget());
            if let Some(binding) = self.tier_bindings.get(&tier) {
                profile.provider = binding.provider.clone();
                profile.model = binding.model.clone();
                profile.context_window = profile.context_window.max(binding.window);
                // 提示词的归属跟着模型走，不跟着工种走。工种默认绑在托管别名上、
                // 档位又把它换成别的模型时，这份提示词必须重新由客户端发送，
                // 否则 Worker 只剩边界段落、不知道自己是干什么的。
                profile.hosted_job_prompt = binding.hosted_job_prompt;
            }
        }
        let requested_command = args
            .target_command
            .as_deref()
            .map(str::trim)
            .filter(|command| !command.is_empty());
        if requested_command.is_some() && profile.id != "ops_runner" {
            return Err(AgentError::Subagent(
                "target_command is only supported by the ops_runner profile".to_owned(),
            ));
        }
        if requested_command != approved_command.as_deref() {
            return Err(AgentError::Subagent(
                "target_command did not receive matching parent authorization".to_owned(),
            ));
        }
        let failure_count = self
            .failures
            .lock()
            .map_err(|_| {
                AgentError::Subagent("subagent failure tracker is unavailable".to_owned())
            })?
            .get(&profile.id)
            .copied()
            .unwrap_or(0);
        if failure_count >= profile.max_consecutive_failures {
            return Err(AgentError::Subagent(format!(
                "profile {} circuit is open after {} consecutive failures",
                profile.id, failure_count
            )));
        }
        if profile.write_scope.writes() && approved_targets.as_ref().is_none_or(BTreeSet::is_empty)
        {
            return Err(AgentError::Subagent(format!(
                "profile {} may write, so it requires an approved file set: pass target_file, or task.write_files for a file-set profile",
                profile.id
            )));
        }
        let task = args.task;
        let verifier = match task.as_ref().and_then(|task| task.verifier.clone()) {
            Some(verifier) => Some(self.gate_verifier_command(verifier).await?),
            None => None,
        };
        // A worker that can run commands is a worker whose whole design is
        // "fix, then prove it". Without a verifier it has nothing to run and
        // nothing to prove, and its report would be exactly the unverified
        // self-assessment this profile exists to replace.
        if verifier.is_none() && profile.shell.requires_verifier() {
            return Err(AgentError::Subagent(format!(
                "profile {} is a verified worker and requires task.verifier.command",
                profile.id
            )));
        }
        let max_attempts = task
            .as_ref()
            .and_then(|task| task.max_attempts)
            .unwrap_or(profile.max_attempts)
            .clamp(1, MAX_ATTEMPTS_CEILING);
        let label = args
            .label
            .filter(|value| !value.trim().is_empty())
            .or_else(|| task.as_ref().map(|task| task.goal.clone()))
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| args.prompt.chars().take(48).collect());
        let prompt = args.prompt;
        let profile_id = profile.id.clone();
        let background = args.run_in_background.unwrap_or(false);
        if background {
            let running = self
                .background
                .snapshots()
                .into_iter()
                .filter(|task| {
                    task.kind == BackgroundTaskKind::Subagent
                        && task.status == BackgroundTaskStatus::Running
                })
                .count();
            if running >= 3 {
                return Err(AgentError::Subagent(
                    "at most 3 background subagents may run concurrently".to_owned(),
                ));
            }
        }
        let prepared = self.prepare_workspace(agent_id, profile.worktree).await?;
        let approved_targets = remap_approved_targets(approved_targets, &prepared)?;
        let run = SubagentRun {
            workspace: prepared.workspace.clone(),
            profile: profile.clone(),
            prompt: prompt.clone(),
            task,
            approved_targets,
            verifier,
            max_attempts,
            claimed_files: self.claimed_files.clone(),
            skills: self.skills.clone(),
            safety_judge: self.safety_judge.clone(),
            approved_command,
        };
        if background {
            let runner_sink = self.sink.clone();
            let failures = self.failures.clone();
            let lifecycle_sink = self.sink.clone();
            let lifecycle_background = self.background.clone();
            let lifecycle_profile = profile_id.clone();
            let lifecycle_model = profile.model.clone();
            let lifecycle_label = label.clone();
            let lifecycle_max_turns = profile.max_turns;
            let lifecycle_token_budget = profile.token_budget;
            let lifecycle_timeout_seconds = profile.timeout_seconds;
            let lifecycle_workspace = prepared.workspace.clone();
            let lifecycle_root_workspace = prepared.root_workspace.clone();
            let lifecycle_worktree_branch = prepared.branch.clone();
            let lifecycle_dedicated_worktree = prepared.dedicated;
            let runner_model_overrides = self.model_overrides.clone();
            let lifecycle_model_overrides = self.model_overrides.clone();
            let instruction_inbox = Arc::new(AgentInstructionInbox::default());
            let runner_instruction_inbox = instruction_inbox.clone();
            let runner_prepared = prepared.clone();
            let id = self.background.start_retriable_with_lifecycle(
                agent_id,
                BackgroundTaskKind::Subagent,
                format!("{label} · {profile_id}"),
                instruction_inbox,
                move || {
                    let mut run = run.clone();
                    let profile_id = run.profile.id.clone();
                    let sink = runner_sink.clone();
                    let failures = failures.clone();
                    let instruction_inbox = runner_instruction_inbox.clone();
                    let prepared = runner_prepared.clone();
                    let model_overrides = runner_model_overrides.clone();
                    async move {
                        if let Some(model) = model_overrides
                            .lock()
                            .expect("subagent model overrides")
                            .get(&agent_id)
                            .cloned()
                        {
                            run.profile.provider = match run.profile.provider.with_model(&model) {
                                Ok(provider) => provider,
                                Err(error) => return subagent_task_result(Err(error.into())),
                            };
                            run.profile.model = Some(model);
                        }
                        let result =
                            run_subagent(run, sink.clone(), agent_id, Some(instruction_inbox))
                                .await;
                        let result = attach_worktree_report(result, &prepared).await;
                        record_profile_result(&failures, &profile_id, &result);
                        subagent_task_result(result)
                    }
                },
                move |snapshot| {
                    let sink = lifecycle_sink.clone();
                    let background = lifecycle_background.clone();
                    let profile = lifecycle_profile.clone();
                    let model = lifecycle_model_overrides
                        .lock()
                        .expect("subagent model overrides")
                        .get(&agent_id)
                        .cloned()
                        .or_else(|| lifecycle_model.clone());
                    let label = lifecycle_label.clone();
                    let workspace = lifecycle_workspace.clone();
                    let root_workspace = lifecycle_root_workspace.clone();
                    let worktree_branch = lifecycle_worktree_branch.clone();
                    async move {
                        if snapshot.status == BackgroundTaskStatus::Running {
                            sink.emit(AgentEvent::SubagentStarted {
                                id: agent_id,
                                profile,
                                model,
                                label,
                                background: true,
                                max_turns: lifecycle_max_turns,
                                token_budget: lifecycle_token_budget,
                                timeout_seconds: lifecycle_timeout_seconds,
                                workspace,
                                root_workspace,
                                worktree_branch,
                                dedicated_worktree: lifecycle_dedicated_worktree,
                            })
                            .await;
                        } else {
                            sink.emit(AgentEvent::SubagentCompleted {
                                id: agent_id,
                                status: background_lifecycle_status(&snapshot.status),
                                report: background.output(&snapshot.id, 200),
                            })
                            .await;
                        }
                    }
                },
            );
            Ok(format!(
                "Subagent started: agent_id={agent_id}, background_task={id}. Its report will be delivered automatically to the main harness."
            ))
        } else {
            self.sink
                .emit(AgentEvent::SubagentStarted {
                    id: agent_id,
                    profile: profile_id.clone(),
                    model: profile.model.clone(),
                    label,
                    background: false,
                    max_turns: profile.max_turns,
                    token_budget: profile.token_budget,
                    timeout_seconds: profile.timeout_seconds,
                    workspace: prepared.workspace.clone(),
                    root_workspace: prepared.root_workspace.clone(),
                    worktree_branch: prepared.branch.clone(),
                    dedicated_worktree: prepared.dedicated,
                })
                .await;
            let result = run_subagent(run, self.sink.clone(), agent_id, None).await;
            let result = attach_worktree_report(result, &prepared).await;
            record_profile_result(&self.failures, &profile_id, &result);
            self.sink
                .emit(AgentEvent::SubagentCompleted {
                    id: agent_id,
                    status: subagent_lifecycle_status(&result),
                    report: result
                        .as_ref()
                        .ok()
                        .cloned()
                        .or_else(|| result.as_ref().err().map(ToString::to_string))
                        .map(bounded_report),
                })
                .await;
            result
        }
    }

    /// Decide whether a verifier command may run unattended.
    ///
    /// An honest verifier writes — a build produces artifacts, a test writes
    /// fixtures — so "read-only only" is the wrong gate: it would push every
    /// real verifier back to "trust the model's own claim", which is worse
    /// than running the command. What cannot be given up is that *something*
    /// judges it. Statically read-only commands pass; a destructive shape is
    /// refused outright; everything in between goes to the same judge the
    /// main agent's `run_command` uses. With no judge configured there is no
    /// one to ask and no approval card to show, so the answer is no.
    async fn gate_verifier_command(
        &self,
        verifier: TaskVerifier,
    ) -> Result<TaskVerifier, AgentError> {
        let command = verifier.command.trim();
        if command.is_empty() {
            return Err(AgentError::Subagent(
                "verifier.command cannot be empty".to_owned(),
            ));
        }
        if crate::tools::child_command_is_sensitive(command) {
            return Err(AgentError::Subagent(format!(
                "verifier command is credential-sensitive and will not be sent to the AI judge: {command}. The parent may request exact human authorization through ops_runner target_command instead"
            )));
        }
        match crate::safety::classify(command) {
            CommandSafety::AlwaysSafe => return Ok(verifier),
            CommandSafety::AlwaysDangerous => {
                return Err(AgentError::Subagent(format!(
                    "verifier command has a destructive shape and will not be run unattended: {command}"
                )));
            }
            CommandSafety::NeedsJudgment => {}
        }
        let Some(judge) = &self.safety_judge else {
            return Err(AgentError::Subagent(format!(
                "verifier command needs review but no safety judge is configured, and a subagent has no approval card to show: {command}"
            )));
        };
        match judge
            .judge(JudgeRequest {
                tool: "subagent_verifier".to_owned(),
                command: command.to_owned(),
                task_context: "run an unattended verifier for a delegated coding task".to_owned(),
            })
            .await
        {
            JudgeVerdict::Allow => Ok(verifier),
            JudgeVerdict::Deny => Err(AgentError::Subagent(format!(
                "the safety judge ({}) refused this verifier command: {command}. The parent may request exact human authorization through ops_runner target_command",
                judge.model()
            ))),
            JudgeVerdict::Unavailable(reason) => Err(AgentError::Subagent(format!(
                "the safety judge ({}) could not review this verifier command ({reason}): {command}. The parent may request exact human authorization through ops_runner target_command",
                judge.model()
            ))),
        }
    }

    async fn prepare_workspace(
        &self,
        agent_id: uuid::Uuid,
        policy: SubagentWorktreePolicy,
    ) -> Result<PreparedSubagentWorkspace, AgentError> {
        if policy == SubagentWorktreePolicy::Shared {
            return Ok(PreparedSubagentWorkspace {
                workspace: self.workspace.clone(),
                root_workspace: self.workspace.clone(),
                branch: None,
                dedicated: false,
            });
        }
        let manager = self.worktrees.as_ref().ok_or_else(|| {
            AgentError::Subagent("dedicated subagent worktree root is not configured".to_owned())
        })?;
        manager.prepare(&self.workspace, agent_id, policy).await
    }
}

async fn attach_worktree_report(
    result: Result<String, AgentError>,
    prepared: &PreparedSubagentWorkspace,
) -> Result<String, AgentError> {
    let report = result?;
    let Some(note) = worktree_result_note(prepared).await? else {
        return Ok(report);
    };
    Ok(format!("{report}\n\n{note}"))
}

fn remap_approved_targets(
    targets: Option<BTreeSet<PathBuf>>,
    prepared: &PreparedSubagentWorkspace,
) -> Result<Option<BTreeSet<PathBuf>>, AgentError> {
    let Some(targets) = targets else {
        return Ok(None);
    };
    if !prepared.dedicated {
        return Ok(Some(targets));
    }
    targets
        .into_iter()
        .map(|target| {
            let relative = target.strip_prefix(&prepared.root_workspace).map_err(|_| {
                AgentError::Subagent(format!(
                    "approved write target {} is outside root workspace {}",
                    target.display(),
                    prepared.root_workspace.display()
                ))
            })?;
            Ok(prepared.workspace.join(relative))
        })
        .collect::<Result<BTreeSet<_>, AgentError>>()
        .map(Some)
}

fn background_lifecycle_status(status: &BackgroundTaskStatus) -> SubagentLifecycleStatus {
    match status {
        BackgroundTaskStatus::Completed => SubagentLifecycleStatus::Completed,
        BackgroundTaskStatus::Blocked => SubagentLifecycleStatus::Blocked,
        BackgroundTaskStatus::Killed => SubagentLifecycleStatus::Cancelled,
        BackgroundTaskStatus::Running
        | BackgroundTaskStatus::Failed
        | BackgroundTaskStatus::TimedOut
        | BackgroundTaskStatus::LaunchFailed => SubagentLifecycleStatus::Failed,
    }
}

fn subagent_lifecycle_status(result: &Result<String, AgentError>) -> SubagentLifecycleStatus {
    match result {
        Ok(_) => SubagentLifecycleStatus::Completed,
        Err(AgentError::Tool(ToolError::ApprovalDenied(_))) => SubagentLifecycleStatus::Blocked,
        Err(_) => SubagentLifecycleStatus::Failed,
    }
}

fn subagent_task_result(result: Result<String, AgentError>) -> TaskResult {
    match result {
        Ok(report) => TaskResult {
            status: BackgroundTaskStatus::Completed,
            exit_code: Some(0),
            output: report,
        },
        Err(AgentError::Tool(ToolError::ApprovalDenied(message))) => TaskResult {
            status: BackgroundTaskStatus::Blocked,
            exit_code: None,
            output: message,
        },
        Err(error) => TaskResult {
            status: BackgroundTaskStatus::Failed,
            exit_code: Some(1),
            output: error.to_string(),
        },
    }
}

fn record_profile_result(
    failures: &Mutex<BTreeMap<String, usize>>,
    profile: &str,
    result: &Result<String, AgentError>,
) {
    let Ok(mut failures) = failures.lock() else {
        return;
    };
    if result.is_ok() {
        failures.remove(profile);
    } else {
        let count = failures.entry(profile.to_owned()).or_default();
        *count = count.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use async_trait::async_trait;

    use super::*;
    use crate::provider::ProviderError;
    use crate::subagent::builtin_profiles;
    use crate::subagent::test_support::{
        AllowingJudge, CaptureSink, ModelProvider, ReportProvider, fixture,
    };
    use crate::subagent::types::TaskPacket;
    use crate::types::{Completion, Message, ToolDefinition};

    #[test]
    fn approval_denial_marks_background_subagent_blocked() {
        let result = subagent_task_result(Err(AgentError::Tool(ToolError::ApprovalDenied(
            "write access needed".to_owned(),
        ))));
        assert_eq!(result.status, BackgroundTaskStatus::Blocked);
        assert_eq!(result.exit_code, None);
        assert_eq!(result.output, "write access needed");
    }

    #[tokio::test]
    async fn foreground_profile_runs_in_isolated_non_nested_agent() {
        let (catalog, root) = fixture();
        let sink = Arc::new(CaptureSink::default());
        let catalog = catalog.with_event_sink(sink.clone());
        let report = catalog
            .run(
                SpawnAgentArgs {
                    prompt: "inspect".to_owned(),
                    label: None,
                    profile: Some("scout".to_owned()),
                    run_in_background: Some(false),
                    ..SpawnAgentArgs::default()
                },
                None,
            )
            .await
            .expect("run");
        assert_eq!(report, "subagent report");
        let events = sink.0.lock().unwrap();
        let started = events.iter().find_map(|event| match event {
            AgentEvent::SubagentStarted {
                id,
                profile,
                background,
                ..
            } => Some((*id, profile.as_str(), *background)),
            _ => None,
        });
        let completed = events.iter().find_map(|event| match event {
            AgentEvent::SubagentCompleted { id, status, .. } => Some((*id, *status)),
            _ => None,
        });
        assert_eq!(
            started.map(|(_, profile, background)| (profile, background)),
            Some(("scout", false))
        );
        assert_eq!(
            completed,
            started.map(|(id, _, _)| (id, SubagentLifecycleStatus::Completed))
        );
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::SubagentTurnStarted { id, turn: 1 }
                if Some(*id) == started.map(|(id, _, _)| id)
        )));
        drop(events);
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[tokio::test]
    async fn editor_requires_preapproved_target() {
        let (catalog, root) = fixture();
        let result = catalog
            .run(
                SpawnAgentArgs {
                    prompt: "edit".to_owned(),
                    label: None,
                    profile: Some("editor".to_owned()),
                    run_in_background: Some(false),
                    target_file: Some("file.txt".to_owned()),
                    ..SpawnAgentArgs::default()
                },
                None,
            )
            .await;
        assert!(matches!(result, Err(AgentError::Subagent(_))));
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[tokio::test]
    async fn external_spawn_is_background_only_and_rejects_writing_profiles() {
        let (catalog, root) = fixture();
        let sink = Arc::new(CaptureSink::default());
        let catalog = catalog.with_event_sink(sink.clone());
        assert!(
            catalog
                .spawn_external_read_only(
                    uuid::Uuid::new_v4(),
                    "edit".to_owned(),
                    None,
                    Some("editor".to_owned()),
                )
                .await
                .is_err()
        );
        assert!(
            catalog
                .spawn_external_read_only(
                    uuid::Uuid::new_v4(),
                    "inspect everything".to_owned(),
                    None,
                    Some("deep".to_owned()),
                )
                .await
                .is_err()
        );
        assert!(
            catalog
                .spawn_external_read_only(
                    uuid::Uuid::new_v4(),
                    "inspect".to_owned(),
                    None,
                    Some("missing".to_owned()),
                )
                .await
                .is_err()
        );

        let id = uuid::Uuid::new_v4();
        catalog
            .spawn_external_read_only(
                id,
                "inspect".to_owned(),
                Some("external scout".to_owned()),
                Some("scout".to_owned()),
            )
            .await
            .expect("spawn external read-only agent");
        for _ in 0..50 {
            if sink.0.lock().unwrap().iter().any(|event| {
                matches!(event, AgentEvent::SubagentCompleted { id: event_id, .. } if *event_id == id)
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let events = sink.0.lock().unwrap();
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::SubagentStarted {
                id: event_id,
                profile,
                background: true,
                ..
            } if *event_id == id && profile == "scout"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::SubagentCompleted { id: event_id, .. } if *event_id == id
        )));
        drop(events);
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[tokio::test]
    async fn terminal_background_agent_retries_with_a_reconfigured_provider_model() {
        let root =
            std::env::temp_dir().join(format!("willdeep-subagent-model-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("workspace");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let provider: Arc<dyn Provider> = Arc::new(ModelProvider {
            model: "old-model".to_owned(),
            seen: seen.clone(),
        });
        let background = Arc::new(BackgroundTaskRegistry::default());
        let sink = Arc::new(CaptureSink::default());
        let mut profiles = builtin_profiles(provider);
        for profile in &mut profiles {
            profile.model = Some("old-model".to_owned());
        }
        let catalog =
            SubagentCatalog::new(&root, profiles, background).with_event_sink(sink.clone());
        let id = uuid::Uuid::new_v4();
        catalog
            .spawn_external_read_only(
                id,
                "inspect".to_owned(),
                Some("model retry".to_owned()),
                Some("scout".to_owned()),
            )
            .await
            .expect("spawn");
        for _ in 0..50 {
            if seen.lock().unwrap().len() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(seen.lock().unwrap().as_slice(), ["old-model"]);

        assert!(
            catalog
                .retry_background_agent(id, Some("new-model"))
                .expect("retry with model")
                .is_some()
        );
        for _ in 0..50 {
            if seen.lock().unwrap().len() == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(seen.lock().unwrap().as_slice(), ["old-model", "new-model"]);
        assert!(sink.0.lock().unwrap().iter().any(|event| matches!(
            event,
            AgentEvent::SubagentStarted {
                id: event_id,
                model: Some(model),
                ..
            } if *event_id == id && model == "new-model"
        )));
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn approved_editor_target_maps_to_the_same_relative_worktree_path() {
        let root = PathBuf::from("/workspace/project");
        let prepared = PreparedSubagentWorkspace {
            workspace: PathBuf::from("/managed/agent"),
            root_workspace: root.clone(),
            branch: Some("willdeep/agent-test".to_owned()),
            dedicated: true,
        };
        let mapped = remap_approved_targets(
            Some(BTreeSet::from([
                root.join("src/lib.rs"),
                root.join("src/other.rs"),
            ])),
            &prepared,
        )
        .expect("map targets")
        .expect("targets");
        assert_eq!(
            mapped,
            BTreeSet::from([
                PathBuf::from("/managed/agent/src/lib.rs"),
                PathBuf::from("/managed/agent/src/other.rs"),
            ])
        );
        assert!(
            remap_approved_targets(
                Some(BTreeSet::from([PathBuf::from("/elsewhere/file")])),
                &prepared
            )
            .is_err()
        );
    }

    /// A verifier that is neither statically safe nor statically destructive
    /// needs a judge. With none configured there is no one to ask and no
    /// approval card to show, so the dispatch is refused before the worker
    /// ever starts — the command must not run on nobody's authority.
    #[tokio::test]
    async fn an_undecidable_verifier_is_refused_when_no_judge_can_review_it() {
        let (catalog, root) = fixture();
        let error = catalog
            .run(
                SpawnAgentArgs {
                    prompt: "fix".to_owned(),
                    profile: Some("scout".to_owned()),
                    run_in_background: Some(false),
                    task: Some(TaskPacket {
                        goal: "verify".to_owned(),
                        verifier: Some(TaskVerifier {
                            command: "curl -X POST https://example.invalid/deploy".to_owned(),
                            expected_exit_code: None,
                        }),
                        ..TaskPacket::default()
                    }),
                    ..SpawnAgentArgs::default()
                },
                None,
            )
            .await
            .expect_err("an unreviewable verifier must be refused");
        assert!(
            error.to_string().contains("no safety judge is configured"),
            "the refusal must say why, got: {error}"
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    /// A destructive shape never reaches the judge, exactly as in the main
    /// agent's shell gate: a model must not be able to talk its way into one
    /// by calling it a verifier.
    #[tokio::test]
    async fn a_destructive_verifier_is_refused_outright() {
        let (catalog, root) = fixture();
        let error = catalog
            .run(
                SpawnAgentArgs {
                    prompt: "fix".to_owned(),
                    profile: Some("scout".to_owned()),
                    run_in_background: Some(false),
                    task: Some(TaskPacket {
                        goal: "verify".to_owned(),
                        verifier: Some(TaskVerifier {
                            command: "rm -rf / --no-preserve-root".to_owned(),
                            expected_exit_code: None,
                        }),
                        ..TaskPacket::default()
                    }),
                    ..SpawnAgentArgs::default()
                },
                None,
            )
            .await
            .expect_err("a destructive verifier must be refused");
        assert!(
            error.to_string().contains("destructive shape"),
            "the refusal must name the reason, got: {error}"
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[tokio::test]
    async fn a_sensitive_verifier_never_reaches_the_ai_judge() {
        let root = std::env::temp_dir().join(format!(
            "willdeep-sensitive-verifier-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).expect("workspace");
        let catalog = SubagentCatalog::new(
            &root,
            builtin_profiles(Arc::new(ReportProvider) as Arc<dyn Provider>),
            Arc::new(BackgroundTaskRegistry::default()),
        )
        .with_safety_judge(Arc::new(AllowingJudge));
        let error = catalog
            .run(
                SpawnAgentArgs {
                    prompt: "inspect".to_owned(),
                    profile: Some("reader".to_owned()),
                    task: Some(TaskPacket {
                        goal: "verify".to_owned(),
                        verifier: Some(TaskVerifier {
                            command: "cat ~/.ssh/id_ed25519".to_owned(),
                            expected_exit_code: None,
                        }),
                        ..TaskPacket::default()
                    }),
                    ..SpawnAgentArgs::default()
                },
                None,
            )
            .await
            .expect_err("sensitive verifier must be refused before AI review");
        assert!(error.to_string().contains("credential-sensitive"));
        assert!(error.to_string().contains("cat ~/.ssh/id_ed25519"));
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn public_catalog_is_five_responsibilities_while_legacy_profiles_remain_executable() {
        let provider: Arc<dyn Provider> = Arc::new(ReportProvider);
        let catalog = SubagentCatalog::new(
            std::env::temp_dir(),
            builtin_profiles(provider),
            Arc::new(BackgroundTaskRegistry::default()),
        );
        let description = catalog.description();
        for profile in PUBLIC_SUBAGENT_IDS {
            assert!(description.contains(&format!("`{profile}`")));
            assert!(catalog.has_profile(profile));
        }
        for legacy in [
            "scout",
            "editor",
            "test_fixer",
            "build_fixer",
            "log_inspector",
            "git_detective",
        ] {
            assert!(catalog.has_profile(legacy));
            assert!(!description.contains(&format!("`{legacy}`")));
        }
        assert_eq!(public_profile_id("git_detective"), Some("generalist"));
        assert_eq!(public_profile_id("build_fixer"), Some("implementer"));
        assert_eq!(public_profile_id("terminal_operator"), Some("ops_runner"));
        assert_eq!(public_profile_id("security_guard"), Some("reviewer"));
        // 改名前的公开名继续路由，别人保存的流程不该在这次改动里断掉。
        assert_eq!(public_profile_id("reader"), Some("generalist"));
        assert_eq!(public_profile_id("judge"), Some("reviewer"));
        // `deep` 现在是档位而不是工种：它的职责部分落在 generalist 上。
        assert_eq!(public_profile_id("deep"), Some("generalist"));
    }

    /// 档位换模型，职责不变。这条盯着正交化本身：`deep` 曾经既是职责也是
    /// 价格，把它拆开之后，一个便宜的调查工种不能因为换了个名字就白拿父模型。
    #[tokio::test]
    async fn the_expert_tier_swaps_the_model_without_changing_the_responsibility() {
        let cheap: Arc<dyn Provider> = Arc::new(ReportProvider);
        let expert: Arc<dyn Provider> = Arc::new(ReportProvider);
        let catalog = SubagentCatalog::new(
            std::env::temp_dir(),
            builtin_profiles(cheap),
            Arc::new(BackgroundTaskRegistry::default()),
        )
        .with_tier_binding(
            crate::WorkerTier::Expert,
            TierBinding {
                provider: expert,
                model: Some("gpt-5.6-sol".to_owned()),
                window: 200_000,
                hosted_job_prompt: false,
            },
        );

        let generalist = catalog.profile(Some("generalist")).expect("generalist");
        // 默认这一档是便宜的：贵模型要过 agent 层的票据才拿得到。
        assert!(generalist.context_window <= crate::WorkerTier::Standard.context_budget());

        // 旧名仍然解析得到，别人保存的流程不该断。
        assert!(catalog.has_profile("deep"));
        assert!(catalog.has_profile("reader"));
        assert!(catalog.has_profile("judge"));
        assert_eq!(
            catalog.profile(Some("deep")).map(|p| p.id.as_str()),
            Some("generalist")
        );
        assert_eq!(
            catalog.profile(Some("judge")).map(|p| p.id.as_str()),
            Some("reviewer")
        );
    }

    /// 三档都能各绑各的模型，而且真的走到那个 provider 上。
    ///
    /// 只断言绑定存进去了是不够的——它得在派工路径上兑现出来，否则用户在
    /// `[worker_tiers.*]` 里写的东西就是一句空话（这正是 0.51 的毛病）。
    #[tokio::test]
    async fn every_tier_cashes_its_own_binding_at_dispatch() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let base: Arc<dyn Provider> = Arc::new(ModelProvider {
            model: "someim-32b".to_owned(),
            seen: seen.clone(),
        });
        let root = std::env::temp_dir().join(format!("willdeep-tiers-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("workspace");
        let mut catalog = SubagentCatalog::new(
            &root,
            builtin_profiles(base),
            Arc::new(BackgroundTaskRegistry::default()),
        );
        for (tier, model) in [
            (crate::WorkerTier::Advanced, "deepseek-v4-flash"),
            (crate::WorkerTier::Expert, "opus-5"),
        ] {
            catalog = catalog.with_tier_binding(
                tier,
                TierBinding {
                    provider: Arc::new(ModelProvider {
                        model: model.to_owned(),
                        seen: seen.clone(),
                    }),
                    model: Some(model.to_owned()),
                    window: tier.context_budget(),
                    hosted_job_prompt: false,
                },
            );
        }

        for tier in ["standard", "advanced", "expert"] {
            catalog
                .run(
                    SpawnAgentArgs {
                        prompt: "look".to_owned(),
                        profile: Some("generalist".to_owned()),
                        worker_tier: Some(tier.to_owned()),
                        run_in_background: Some(false),
                        ..SpawnAgentArgs::default()
                    },
                    None,
                )
                .await
                .expect("run");
        }

        // 同一个职责，三档三个模型。基础档没绑定，留在工种自己的模型上。
        assert_eq!(
            seen.lock().expect("models").as_slice(),
            ["someim-32b", "deepseek-v4-flash", "opus-5"]
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    /// 档位换掉一个托管工种的模型时，客户端必须把职责提示词补回来。
    ///
    /// 这是最容易漏的一条：工种绑在网关托管的别名上（提示词由服务端 prepend，
    /// 客户端因此不发），档位又把模型换成了别处的 `opus-5`——两边都不发，
    /// Worker 就只剩边界段落，完全不知道自己该干什么。
    #[tokio::test]
    async fn switching_tiers_hands_the_job_prompt_back_to_the_client() {
        struct PromptProbe(Arc<Mutex<Vec<String>>>);
        #[async_trait]
        impl Provider for PromptProbe {
            async fn complete(
                &self,
                messages: &[Message],
                _tools: &[ToolDefinition],
            ) -> Result<Completion, ProviderError> {
                self.0.lock().unwrap().push(messages[0].content.clone());
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
        let root =
            std::env::temp_dir().join(format!("willdeep-tierprompt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("workspace");
        let mut profiles = builtin_profiles(provider.clone());
        for profile in &mut profiles {
            // 模拟一个绑在 `someim-32b-scout` 上的托管工种：提示词由服务端发。
            profile.hosted_job_prompt = profile.id == "scout";
        }
        let catalog =
            SubagentCatalog::new(&root, profiles, Arc::new(BackgroundTaskRegistry::default()))
                .with_tier_binding(
                    crate::WorkerTier::Expert,
                    TierBinding {
                        provider,
                        model: Some("opus-5".to_owned()),
                        window: 200_000,
                        // opus-5 不是托管别名，服务端不会 prepend 任何东西。
                        hosted_job_prompt: false,
                    },
                );

        catalog
            .run(
                SpawnAgentArgs {
                    prompt: "look".to_owned(),
                    profile: Some("scout".to_owned()),
                    worker_tier: Some("expert".to_owned()),
                    run_in_background: Some(false),
                    ..SpawnAgentArgs::default()
                },
                None,
            )
            .await
            .expect("run");

        let prompts = seen.lock().expect("prompts").clone();
        assert!(
            prompts[0].contains("LOCATION"),
            "换掉托管模型之后职责提示词必须由客户端补上，实际收到：{}",
            prompts[0]
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }
}
