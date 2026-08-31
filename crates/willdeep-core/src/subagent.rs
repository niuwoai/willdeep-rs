use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use serde::Deserialize;

use crate::agent::{
    Agent, AgentConfig, AgentError, AgentEvent, AgentInstructionInbox, EventSink,
    SubagentLifecycleStatus,
};
use crate::background::{
    BackgroundTaskKind, BackgroundTaskRegistry, BackgroundTaskStatus, TaskResult,
};
use crate::judge::{JudgeRequest, JudgeVerdict, SafetyJudge};
use crate::provider::Provider;
use crate::routing::EscalationTicket;
use crate::safety::CommandSafety;
use crate::subagent_worktree::{
    PreparedSubagentWorkspace, SubagentWorktreeManager, SubagentWorktreePolicy,
    worktree_result_note,
};
use crate::tools::{ApprovalMode, ToolError, ToolRegistry};

/// Never inline more than this, whatever the window says.
const MAX_INLINE_BYTES: usize = 96 * 1024;

/// Bytes of relevant-file content a worker's first message may carry, as a
/// function of its window. Roughly three quarters of a token of budget per
/// token of window (≈1 token per 3 bytes of source), which lands a 32K worker
/// at 24 KB and a 64K worker at 48 KB — the largest single item it starts
/// with, and still leaving half the window for tool round trips and output.
fn inline_budget(context_window: u64) -> usize {
    usize::try_from(context_window.saturating_mul(3) / 4)
        .unwrap_or(MAX_INLINE_BYTES)
        .min(MAX_INLINE_BYTES)
}

/// Bytes of digested verifier output fed back to a worker between attempts.
const MAX_VERIFIER_DIGEST_BYTES: usize = 6 * 1024;

/// Attempts a verified run makes before it gives up and reports failure.
const DEFAULT_MAX_ATTEMPTS: usize = 3;
const MAX_ATTEMPTS_CEILING: usize = 6;

/// Seconds a verifier command may run before it counts as a failed attempt.
const VERIFIER_TIMEOUT_SECONDS: u64 = 900;

/// What a trade may put through `run_command`.
///
/// A worker runs unattended: there is no approval card to show, so the shell
/// it gets has to be decided at dispatch time and never widened at runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubagentShell {
    /// No shell at all.
    None,
    /// Exactly the verifier command declared in the task packet, verbatim.
    VerifierOnly,
    /// No shell unless the task packet declares a verifier; when it does,
    /// only that exact command is available.
    VerifierOptional,
    /// Read-only `git` and nothing else. History questions — which commit
    /// introduced this, what did that commit change, how do two refs differ —
    /// cannot be answered by a fixed set of pre-baked git tools: the whole
    /// job is composing the query. Both halves of the rule matter: the head
    /// must be `git`, and the command must still pass the same static
    /// read-only classification every other command passes.
    ReadOnlyGit,
    /// Static read-only/bounded commands run directly. Ambiguous commands
    /// are reviewed by the configured safety judge in this worker's task
    /// context. Destructive or credential-sensitive commands are refused
    /// before the judge sees them.
    Reviewed,
    /// A reviewed shell that still requires a verifier in the task packet.
    ReviewedVerifierOnly,
    /// A reviewed shell whose verifier is optional.
    ReviewedVerifierOptional,
}

impl SubagentShell {
    fn requires_verifier(self) -> bool {
        matches!(self, Self::VerifierOnly | Self::ReviewedVerifierOnly)
    }

    fn uses_intelligent_review(self) -> bool {
        matches!(
            self,
            Self::Reviewed | Self::ReviewedVerifierOnly | Self::ReviewedVerifierOptional
        )
    }
}

/// Public trades shown to people and advertised in the tool schema.
///
/// Five responsibilities, mirroring macOS Xedit's `AgentWorkerRole`: 调查 /
/// 实现 / 验证 / 审查 / 运维. Legacy specialist IDs remain executable for
/// automatic routing and saved flows.
///
/// `deep` is deliberately absent. It used to be both a responsibility
/// ("complex investigation") and a model choice ("run the parent's expensive
/// model"); those are now separate axes — the responsibility is `generalist`,
/// the model is [`crate::WorkerTier::Expert`]. A trade list that doubles as a
/// price list makes every new responsibility cost a new model binding.
pub const PUBLIC_SUBAGENT_IDS: [&str; 5] = [
    "generalist",
    "implementer",
    "tester",
    "reviewer",
    "ops_runner",
];

pub fn public_profile_id(id: &str) -> Option<&'static str> {
    match id.trim().to_ascii_lowercase().as_str() {
        "scout" | "reader" | "generalist" | "log_inspector" | "git_detective" | "deep" => {
            Some("generalist")
        }
        "editor" | "implementer" | "test_fixer" | "build_fixer" => Some("implementer"),
        "tester" | "small_reviewer" => Some("tester"),
        "ops_runner" | "terminal_operator" => Some("ops_runner"),
        "security_guard" | "judge" | "reviewer" => Some("reviewer"),
        _ => None,
    }
}

/// How a profile is allowed to write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubagentWriteScope {
    /// Read-only worker.
    None,
    /// Exactly one separately approved file (`editor`).
    SingleFile,
    /// The set of files declared in the task packet, approved as one set.
    FileSet,
}

impl SubagentWriteScope {
    pub fn writes(self) -> bool {
        !matches!(self, Self::None)
    }
}

#[derive(Clone)]
pub struct SubagentProfile {
    pub id: String,
    pub purpose: String,
    pub model: Option<String>,
    pub provider: Arc<dyn Provider>,
    pub tool_names: Vec<String>,
    pub capability_prompt: String,
    pub max_turns: usize,
    pub context_window: u64,
    pub token_budget: Option<u64>,
    pub timeout_seconds: Option<u64>,
    pub max_consecutive_failures: usize,
    pub write_scope: SubagentWriteScope,
    pub worktree: SubagentWorktreePolicy,
    /// Byte cap on this profile's tool payloads. `None` keeps the main
    /// agent's generous defaults; small-window workers set it explicitly.
    pub tool_output_limit: Option<usize>,
    /// Attempt budget for verified runs when the packet does not override it.
    pub max_attempts: usize,
    /// What this trade may run in a shell.
    pub shell: SubagentShell,
    /// The relay prepends this trade's job prompt (`someim-32b-<trade>`), so
    /// the client must not send its own copy. The boundary paragraph — no
    /// user, no nesting, the report *is* the return value — is always sent:
    /// the server owns the trade, the client owns the边界.
    pub hosted_job_prompt: bool,
}

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

/// The structured half of a dispatch: what the parent already knows, so the
/// worker does not spend its window rediscovering it.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct TaskPacket {
    pub goal: String,
    /// Skill whose body the runtime inlines into the worker's first message.
    /// Workers have no skill tools of their own — a worker that had to fetch
    /// its own instructions would spend its window doing it — so the parent
    /// names the skill and the runtime does the reading.
    #[serde(default)]
    pub skill: Option<String>,
    #[serde(default)]
    pub read_files: Vec<String>,
    /// Exact write allowlist for writing profiles. Kept separate from
    /// `read_files` so giving a worker enough context never grants it more
    /// authority. When absent, legacy `relevant_files` remains the write set
    /// for backwards compatibility.
    #[serde(default)]
    pub write_files: Vec<String>,
    /// Legacy combined read/write set. New callers should use `read_files`
    /// plus `write_files`; old packets retain their original semantics.
    #[serde(default)]
    pub relevant_files: Vec<String>,
    #[serde(default)]
    pub known_facts: Vec<String>,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub verifier: Option<TaskVerifier>,
    #[serde(default)]
    pub max_attempts: Option<usize>,
    /// Air-gapped degradation: when a relevant file does not fit the inline
    /// budget, digest it through the worker's own cheap model (shard →
    /// per-chunk structured summary) instead of dropping it with an
    /// "omitted" marker. Off by default — it spends model calls, and that is
    /// the dispatcher's decision to make, not the runtime's.
    #[serde(default)]
    pub digest_oversized: Option<bool>,
}

impl TaskPacket {
    fn inline_files(&self) -> Vec<String> {
        let mut files = Vec::new();
        for path in self
            .read_files
            .iter()
            .chain(self.write_files.iter())
            .chain(self.relevant_files.iter())
        {
            if !files.contains(path) {
                files.push(path.clone());
            }
        }
        files
    }

    fn requested_write_files(&self) -> Vec<String> {
        if self.write_files.is_empty() {
            self.relevant_files.clone()
        } else {
            self.write_files.clone()
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct TaskVerifier {
    pub command: String,
    #[serde(default)]
    pub expected_exit_code: Option<i32>,
}

impl TaskVerifier {
    fn expected_exit_code(&self) -> i32 {
        self.expected_exit_code.unwrap_or(0)
    }
}

/// The last failing verdict of a verified run, kept so the failure report can
/// name the command, the attempt count and the real output.
#[derive(Clone, Debug, PartialEq, Eq)]
struct VerifierOutcome {
    command: String,
    attempts: usize,
    last_digest: String,
}

struct SubagentNoopSink;

#[async_trait::async_trait]
impl EventSink for SubagentNoopSink {
    async fn emit(&self, _event: AgentEvent) {}
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

#[derive(Debug, Default, Deserialize)]
pub(crate) struct SpawnAgentArgs {
    pub prompt: String,
    pub label: Option<String>,
    pub profile: Option<String>,
    pub run_in_background: Option<bool>,
    pub target_file: Option<String>,
    /// Exact command requested after an unattended worker reported that AI
    /// review declined or was unavailable. Only `ops_runner` may receive it,
    /// and the parent must separately authorize the identical string.
    pub target_command: Option<String>,
    pub task: Option<TaskPacket>,
    pub escalation: Option<EscalationTicket>,
    /// 模型档位，与职责正交（`standard` / `advanced` / `expert`）。缺省走最便宜
    /// 那档；`expert` 与旧名 `deep` 一样需要升级票据。
    pub worker_tier: Option<String>,
}

impl SpawnAgentArgs {
    /// Paths the parent is asking this worker to be allowed to modify: the
    /// packet's `write_files` for a file-set profile, the single `target_file`
    /// for the editor. Legacy packets fall back to `relevant_files`.
    pub(crate) fn requested_write_targets(&self, scope: SubagentWriteScope) -> Vec<String> {
        match scope {
            SubagentWriteScope::None => Vec::new(),
            SubagentWriteScope::SingleFile => self.target_file.clone().into_iter().collect(),
            SubagentWriteScope::FileSet => {
                let mut files = self
                    .task
                    .as_ref()
                    .map(TaskPacket::requested_write_files)
                    .unwrap_or_default();
                if let Some(target) = &self.target_file
                    && !files.contains(target)
                {
                    files.push(target.clone());
                }
                files
            }
        }
    }
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

/// Files one running worker has claimed. Released on drop, so a panic, a
/// timeout or a cancelled run never leaves a file locked behind it.
struct FileClaim {
    claimed: Arc<Mutex<BTreeSet<PathBuf>>>,
    files: BTreeSet<PathBuf>,
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

/// One dispatched worker, fully resolved: everything the runner needs and
/// nothing it has to ask the catalog for again.
#[derive(Clone)]
struct SubagentRun {
    workspace: PathBuf,
    profile: SubagentProfile,
    prompt: String,
    task: Option<TaskPacket>,
    approved_targets: Option<BTreeSet<PathBuf>>,
    verifier: Option<TaskVerifier>,
    max_attempts: usize,
    claimed_files: Arc<Mutex<BTreeSet<PathBuf>>>,
    skills: Option<Arc<crate::skills::SkillCatalog>>,
    safety_judge: Option<Arc<dyn SafetyJudge>>,
    approved_command: Option<String>,
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
async fn run_subagent(
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

/// What a deterministic spot-check made of a report-only run.
///
/// Read-only trades (`scout`, `reader`, `log_inspector`, `git_detective`) have
/// no exit code to judge them, so their telemetry has been a permanent `None`:
/// every run "unverified", forever. But their answers are not unfalsifiable —
/// a location either exists or it does not. This checks the part a program
/// can check: the paths, line numbers and commits the report names.
///
/// It deliberately says nothing about whether the answer is *right*. A worker
/// can cite ten real files and still miss the point; what it can no longer do
/// is invent a path and have that pass unnoticed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CitationAudit {
    pub checked: usize,
    pub unverifiable: Vec<String>,
}

impl CitationAudit {
    fn note(&self) -> Option<String> {
        if self.checked == 0 {
            return None;
        }
        if self.unverifiable.is_empty() {
            return Some(format!(
                "<citation-check checked=\"{}\" unverifiable=\"0\" />",
                self.checked
            ));
        }
        Some(format!(
            "<citation-check checked=\"{}\" unverifiable=\"{}\">\nThese cited locations do not exist in the workspace, so anything resting on them is unsupported:\n{}\n</citation-check>",
            self.checked,
            self.unverifiable.len(),
            self.unverifiable
                .iter()
                .map(|claim| format!("- {claim}"))
                .collect::<Vec<_>>()
                .join("\n")
        ))
    }
}

/// Spot-check every `path`, `path:line` and commit hash a report names.
///
/// Cheap and deterministic: no model, no network, a `stat` per path and one
/// `git cat-file` per hash. Anything it cannot classify it leaves alone —
/// counting an unrecognized token as a bad citation would make the metric
/// measure the parser instead of the worker.
pub async fn audit_citations(workspace: &Path, report: &str) -> CitationAudit {
    let mut audit = CitationAudit::default();
    let mut seen = BTreeSet::new();
    for raw in report.split(|ch: char| {
        ch.is_whitespace()
            || matches!(
                ch,
                '`' | '"' | '\'' | '(' | ')' | '[' | ']' | '<' | '>' | ',' | ';'
            )
    }) {
        let token = raw.trim_matches(|ch: char| matches!(ch, '.' | ':' | '*' | '#'));
        if token.is_empty() || !seen.insert(token.to_owned()) {
            continue;
        }
        if let Some((path, line)) = split_path_citation(token) {
            let target = workspace.join(path);
            if !target.is_file() {
                audit.checked += 1;
                audit.unverifiable.push(token.to_owned());
                continue;
            }
            audit.checked += 1;
            let Some(line) = line else { continue };
            // A line number past the end of the file is the same class of
            // error as a path that does not exist: it points at nothing.
            let lines = tokio::fs::read_to_string(&target)
                .await
                .map(|text| text.lines().count())
                .unwrap_or(0);
            if line == 0 || line > lines {
                audit.unverifiable.push(token.to_owned());
            }
        } else if is_commit_hash(token) {
            audit.checked += 1;
            if !commit_exists(workspace, token).await {
                audit.unverifiable.push(token.to_owned());
            }
        }
    }
    audit
}

/// `src/foo.rs:42` → (`src/foo.rs`, Some(42)). Only tokens that look like a
/// workspace-relative file path qualify: a directory separator and an
/// extension, no absolute paths, no URLs.
fn split_path_citation(token: &str) -> Option<(&str, Option<usize>)> {
    let (path, line) = match token.rsplit_once(':') {
        Some((head, tail)) if tail.chars().all(|ch| ch.is_ascii_digit()) && !tail.is_empty() => {
            (head, tail.parse::<usize>().ok())
        }
        _ => (token, None),
    };
    if path.starts_with('/') || path.contains("://") || path.starts_with('-') {
        return None;
    }
    let file_name = path.rsplit('/').next()?;
    if !path.contains('/') || !file_name.contains('.') || file_name.starts_with('.') {
        return None;
    }
    Some((path, line))
}

fn is_commit_hash(token: &str) -> bool {
    (7..=40).contains(&token.len())
        && token.chars().all(|ch| ch.is_ascii_hexdigit())
        && token.chars().any(|ch| ch.is_ascii_digit())
        && token.chars().any(|ch| ch.is_ascii_alphabetic())
}

async fn commit_exists(workspace: &Path, hash: &str) -> bool {
    tokio::process::Command::new("git")
        .args(["cat-file", "-e", &format!("{hash}^{{commit}}")])
        .current_dir(workspace)
        .output()
        .await
        .map(|output| output.status.success())
        .unwrap_or(false)
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
) -> Result<String, AgentError> {
    let approval = if profile.shell.uses_intelligent_review() {
        ApprovalMode::Smart
    } else if approved_targets.is_some() {
        ApprovalMode::WorkspaceAccess
    } else {
        ApprovalMode::Strict
    };
    let mut tools = ToolRegistry::new(workspace, approval)?
        .with_allowed_tools(profile.tool_names.clone())
        .with_write_targets(approved_targets.clone());
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

/// Build the worker's first message: the packet the parent compiled, then the
/// free-text instruction. Relevant files are inlined here rather than left for
/// the worker to find — every grep it runs is window it does not get back.
async fn compose_brief(
    prompt: &str,
    task: Option<&TaskPacket>,
    workspace: &Path,
    profile: &SubagentProfile,
    skills: Option<&crate::skills::SkillCatalog>,
) -> String {
    let Some(task) = task else {
        return prompt.to_owned();
    };
    let mut brief = String::new();
    brief.push_str(&format!("<goal>\n{}\n</goal>\n", task.goal.trim()));
    if let Some(name) = task
        .skill
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        // The skill body rides in the opening message like a relevant file:
        // a worker made to fetch its own instructions spends its window on
        // the fetching. Unresolvable skills are named, never silent — the
        // worker must not improvise the procedure it thinks it was given.
        let budget = inline_budget(profile.context_window) / 2;
        match skills.and_then(|catalog| catalog.read(name, None).ok()) {
            Some(body) => brief.push_str(&format!(
                "\n<skill name={name:?}>\n{}\n</skill>\n",
                bounded(body, budget)
            )),
            None => brief.push_str(&format!(
                "\n<skill name={name:?} status=\"unavailable: not installed on this runtime; say so in your report instead of improvising the procedure\" />\n"
            )),
        }
    }
    if !task.known_facts.is_empty() {
        brief.push_str("\n<known-facts>\n");
        for fact in &task.known_facts {
            brief.push_str(&format!("- {}\n", fact.trim()));
        }
        brief.push_str("</known-facts>\n");
    }
    if !task.constraints.is_empty() {
        brief.push_str("\n<constraints>\n");
        for constraint in &task.constraints {
            brief.push_str(&format!("- {}\n", constraint.trim()));
        }
        brief.push_str("</constraints>\n");
    }
    if let Some(verifier) = &task.verifier {
        brief.push_str(&format!(
            "\n<verifier command={:?} expected_exit_code=\"{}\">\nThe runtime runs this after you finish and again after every attempt. You do not decide whether you are done, and claiming success without it changes nothing.\n</verifier>\n",
            verifier.command,
            verifier.expected_exit_code()
        ));
    }
    let inline_files = task.inline_files();
    if !inline_files.is_empty() {
        let budget = inline_budget(profile.context_window);
        brief.push_str(
            &inline_relevant_files(
                workspace,
                &inline_files,
                budget,
                task.digest_oversized.unwrap_or(false),
                profile,
            )
            .await,
        );
    }
    brief.push_str(&format!(
        "\n<instruction>\n{}\n</instruction>\n",
        prompt.trim()
    ));
    brief
}

async fn inline_relevant_files(
    workspace: &Path,
    files: &[String],
    budget: usize,
    digest_oversized: bool,
    profile: &SubagentProfile,
) -> String {
    let mut rendered = String::from("\n<relevant-files>\n");
    let mut spent = 0usize;
    for path in files {
        let full = workspace.join(path);
        let Ok(content) = tokio::fs::read_to_string(&full).await else {
            rendered.push_str(&format!("<file path={path:?} status=\"unreadable\" />\n"));
            continue;
        };
        let remaining = budget.saturating_sub(spent);
        // Air-gapped degradation: material that does not fit the window gets
        // digested through the worker's own cheap model instead of dropped.
        // The digest is honest about what it is — a summary, not the file —
        // and the raw path stays named so the worker can still read slices.
        if digest_oversized && content.len() > remaining.max(1) {
            let digest = digest_material(profile, path, &content, remaining.min(budget / 4)).await;
            spent += digest.len();
            rendered.push_str(&digest);
            continue;
        }
        if remaining == 0 {
            rendered.push_str(&format!(
                "<file path={path:?} status=\"omitted: inline budget exhausted, read it yourself if you need it\" />\n"
            ));
            continue;
        }
        let truncated = content.len() > remaining;
        let slice = bounded(content, remaining);
        spent += slice.len();
        rendered.push_str(&format!(
            "<file path={path:?}{}>\n{slice}\n</file>\n",
            if truncated {
                " status=\"truncated\""
            } else {
                ""
            }
        ));
    }
    rendered.push_str("</relevant-files>\n");
    rendered
}

/// Map-reduce a file that does not fit the inline budget: shard it, summarize
/// each shard on the worker's own cheap model, and inline the digests.
///
/// This is the automated form of the air-gapped degradation ladder in
/// `docs/MODEL_TIERS.md`: when no long-context tier exists, oversized
/// material is sharded and reduced rather than silently dropped. Three
/// disciplines keep it honest:
///
/// - the result is *labeled* a digest, chunk by chunk, so nothing downstream
///   mistakes a summary for the file;
/// - identifiers, signatures and assertions are demanded verbatim — the same
///   rule the verifier failure digest lives by;
/// - a chunk whose summary call fails is reported failed, not skipped: a
///   digest with an unmarked hole reads as complete coverage.
const DIGEST_MAX_CHUNKS: usize = 4;

async fn digest_material(
    profile: &SubagentProfile,
    path: &str,
    content: &str,
    output_budget: usize,
) -> String {
    // Chunk on char boundaries, sized so every chunk fits the worker window
    // with room for the instruction and the reply.
    let chunk_bytes = (inline_budget(profile.context_window) / 2).max(4 * 1024);
    let mut chunks: Vec<&str> = Vec::new();
    let mut rest = content;
    while !rest.is_empty() && chunks.len() < DIGEST_MAX_CHUNKS {
        let mut cut = rest.len().min(chunk_bytes);
        while cut > 0 && !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        let (head, tail) = rest.split_at(cut);
        chunks.push(head);
        rest = tail;
    }
    let uncovered = !rest.is_empty();
    let per_chunk = (output_budget / chunks.len().max(1)).max(512);

    let mut rendered = format!(
        "<file path={path:?} status=\"digested: too large to inline, summarized in {} chunk(s) by {}\">\n",
        chunks.len(),
        profile.model.as_deref().unwrap_or("the worker model")
    );
    for (index, chunk) in chunks.iter().enumerate() {
        let request = crate::types::Message::user(format!(
            "Summarize this chunk ({} of {}) of file {path} for an engineer who cannot read the original. Preserve identifiers, function signatures, error messages and assertions verbatim. Be dense and factual; no advice.\n\n{chunk}",
            index + 1,
            chunks.len()
        ));
        match profile.provider.complete(&[request], &[]).await {
            Ok(completion) => rendered.push_str(&format!(
                "<chunk index=\"{}\">\n{}\n</chunk>\n",
                index + 1,
                bounded(completion.content, per_chunk)
            )),
            Err(error) => rendered.push_str(&format!(
                "<chunk index=\"{}\" status=\"digest failed: {error}\" />\n",
                index + 1
            )),
        }
    }
    if uncovered {
        rendered.push_str(&format!(
            "<uncovered note=\"content beyond {DIGEST_MAX_CHUNKS} chunks was not digested; read {path:?} directly for the remainder\" />\n"
        ));
    }
    rendered.push_str("</file>\n");
    rendered
}

fn bounded(value: String, limit: usize) -> String {
    if value.len() <= limit {
        return value;
    }
    let mut boundary = limit;
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}\n[truncated]", &value[..boundary])
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

fn bounded_report(value: String) -> String {
    const MAX_REPORT_BYTES: usize = 64 * 1024;
    if value.len() <= MAX_REPORT_BYTES {
        return value;
    }
    let mut boundary = MAX_REPORT_BYTES;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}\n[report truncated]", &value[..boundary])
}

/// Context window tiers. A cheap model with a 128K window configured is not
/// the same thing as a cheap model that *works* at 128K: the discipline that
/// makes small models succeed is a small average context, and the only place
/// to enforce it is here. `deep` is the deliberate exception — it runs on the
/// parent model precisely because its work does not fit in a small window.
pub const WORKER_WINDOW_STANDARD: u64 = 32_768;
pub const WORKER_WINDOW_BALANCED: u64 = 49_152;
pub const WORKER_WINDOW_WIDE: u64 = 65_536;
pub const STANDARD_WINDOW: u64 = 262_144;

/// Tool payload caps per tier, in bytes. One 128 KB test log would consume a
/// 32K window several times over, so the cap is part of the tier, not a knob.
const PAYLOAD_LIMIT_STANDARD: usize = 4 * 1024;
const PAYLOAD_LIMIT_BALANCED: usize = 5 * 1024;
const PAYLOAD_LIMIT_WIDE: usize = 6 * 1024;
const PAYLOAD_LIMIT_IMPLEMENTER: usize = 16 * 1024;

/// Prefix of the some.im virtual models that used to host trade job prompts.
/// `someim-32b` is a *tier* name, not a context promise: the model behind it
/// is the same cheap model, and the small-window discipline stays on this
/// side of the wire.
pub const HOSTED_WORKER_MODEL_PREFIX: &str = "someim-32b";

/// The hosted model a trade runs on, or `None` when the trade has no default
/// binding on this gateway.
///
/// Every trade now resolves to the shared base model rather than its own
/// `someim-32b-<trade>` chain. Those per-trade chains existed to prepend a job
/// prompt server-side, but the job prompt travels with the request from
/// [`builtin_profiles`] — keeping both means the worker gets its instructions
/// twice, and it means every new responsibility needs a new chain provisioned
/// on the relay before it can ship.
///
/// Mirrors macOS Xedit's `AgentSubagentModelCompatibility.recommendedModel` —
/// same gateway, same accounts, so the two clients must resolve the same trade
/// to the same model or the same operator gets two different workers depending
/// on which app they opened.
pub fn hosted_worker_model(id: &str) -> Option<String> {
    // `generalist` 合并了 scout / reader / log_inspector / git_detective 四个
    // 基础档窄工种，它在这个网关上的默认自然也是基础档。想要更强的，走
    // WorkerTier::Expert——那一档要票据。
    let base = crate::worker_tier::LEGACY_HOSTED_TRADES.contains(&id) || id == "generalist";
    base.then(|| crate::worker_tier::HOSTED_BASE_MODEL.to_owned())
}

/// Every profile the catalog ships with.
///
/// One provider is enough now: each responsibility declares its own context
/// budget, and the session window belongs to the expert tier — which the
/// catalog receives separately through [`SubagentCatalog::with_expert_tier`]
/// because reaching it costs a ticket.
pub fn builtin_profiles(worker: Arc<dyn Provider>) -> Vec<SubagentProfile> {
    let cheap = worker;
    vec![
        profile(
            cheap.clone(),
            ProfileSpec {
                id: "scout",
                shell: SubagentShell::None,
                purpose: "Locate files, symbols and call sites quickly; no shell or writes.",
                tools: &["search_files", "grep_files", "list_directory", "read_file"],
                prompt: "Your trade is LOCATION. Report exact paths and line numbers; do not redesign.",
                max_turns: 8,
                context_window: WORKER_WINDOW_STANDARD,
                tool_output_limit: Some(PAYLOAD_LIMIT_STANDARD),
                write_scope: SubagentWriteScope::None,
                timeout_seconds: 300,
                worktree: SubagentWorktreePolicy::Shared,
            },
        ),
        profile(
            cheap.clone(),
            ProfileSpec {
                id: "reader",
                shell: SubagentShell::None,
                purpose: "Read and summarize long files or documentation; no shell or writes.",
                tools: &["read_file", "list_directory", "search_files"],
                prompt: "Your trade is READING. Answer with specific evidence and say which parts you read.",
                max_turns: 8,
                context_window: WORKER_WINDOW_BALANCED,
                tool_output_limit: Some(PAYLOAD_LIMIT_BALANCED),
                write_scope: SubagentWriteScope::None,
                timeout_seconds: 300,
                worktree: SubagentWorktreePolicy::Shared,
            },
        ),
        profile(
            cheap.clone(),
            ProfileSpec {
                id: "log_inspector",
                shell: SubagentShell::None,
                purpose: "Explain a failure log or error output and classify the cause; no shell or writes.",
                tools: &["read_file"],
                prompt: "Your trade is READING FAILURE OUTPUT. Quote the failing assertion or error verbatim — never paraphrase it — then name the single most likely cause and the file it lives in. If the output does not support a conclusion, say so instead of guessing.",
                max_turns: 4,
                context_window: WORKER_WINDOW_STANDARD,
                tool_output_limit: Some(PAYLOAD_LIMIT_STANDARD),
                write_scope: SubagentWriteScope::None,
                timeout_seconds: 300,
                worktree: SubagentWorktreePolicy::Shared,
            },
        ),
        profile(
            cheap.clone(),
            ProfileSpec {
                id: "git_detective",
                shell: SubagentShell::Reviewed,
                purpose: "Find when and where a regression was introduced from repository history; read-only.",
                tools: &[
                    "git_log",
                    "git_diff",
                    "git_blame",
                    "git_status",
                    "read_file",
                    "run_command",
                ],
                prompt: "Your trade is REGRESSION ARCHAEOLOGY. Prefer read-only `git` commands — `git log -p`, `git show <sha>`, `git diff <a> <b>`, `git bisect` inspection. Work backwards from the symptom through history and report exact commits, dates and hunks. Name the commit you believe introduced the change, and say plainly when the evidence does not single one out.",
                max_turns: 8,
                context_window: WORKER_WINDOW_STANDARD,
                tool_output_limit: Some(PAYLOAD_LIMIT_STANDARD),
                write_scope: SubagentWriteScope::None,
                timeout_seconds: 300,
                worktree: SubagentWorktreePolicy::Shared,
            },
        ),
        profile(
            cheap.clone(),
            ProfileSpec {
                // 旧名 `deep`：它当年既表示「复杂调查」这个职责，也表示
                // 「用最贵的模型 + 整个会话窗口」这个档位。正交化之后职责留在
                // 这里并**默认走便宜档**——贵模型只能经 WorkerTier::Expert 拿到，
                // 而那一档要票据。否则改个名字就等于白送一次升档。
                id: "generalist",
                shell: SubagentShell::Reviewed,
                purpose: "Complex investigation across files and repository state.",
                tools: &[
                    "search_files",
                    "grep_files",
                    "read_file",
                    "list_directory",
                    "git_status",
                    "run_command",
                ],
                prompt: "Your trade is INVESTIGATION. Follow evidence across files and state what you could not confirm.",
                max_turns: 12,
                // 默认基础档预算。整个会话窗口是专家档兑现的东西，不是这个
                // 职责与生俱来的——否则「调查」这个名字就等于一张免费的贵模型券。
                context_window: crate::WorkerTier::Standard.context_budget(),
                tool_output_limit: Some(PAYLOAD_LIMIT_WIDE),
                write_scope: SubagentWriteScope::None,
                timeout_seconds: 300,
                worktree: SubagentWorktreePolicy::Shared,
            },
        ),
        profile(
            cheap.clone(),
            ProfileSpec {
                id: "editor",
                shell: SubagentShell::None,
                purpose: "Edit exactly one separately approved target_file.",
                tools: &["read_file", "edit_file"],
                prompt: "Your trade is EDITING EXACTLY ONE FILE. Read it first, make a minimal exact edit, and touch no other path.",
                max_turns: 6,
                context_window: WORKER_WINDOW_BALANCED,
                tool_output_limit: Some(PAYLOAD_LIMIT_BALANCED),
                write_scope: SubagentWriteScope::SingleFile,
                timeout_seconds: 300,
                worktree: SubagentWorktreePolicy::Dedicated,
            },
        ),
        profile(
            cheap.clone(),
            ProfileSpec {
                id: "implementer",
                shell: SubagentShell::ReviewedVerifierOptional,
                purpose: "Implement a bounded multi-file change with a deployable 256K model.",
                tools: &[
                    "search_files",
                    "grep_files",
                    "list_directory",
                    "read_file",
                    "create_file",
                    "edit_file",
                    "run_command",
                ],
                prompt: "Your trade is BOUNDED IMPLEMENTATION. Complete the requested multi-file change inside the declared file set. Inspect neighboring code, preserve public behaviour outside the task, and use the verifier when one is supplied. You may create or edit only paths declared in task.write_files (legacy packets use task.relevant_files). Report changed files, verification and remaining uncertainty.",
                max_turns: 18,
                context_window: STANDARD_WINDOW,
                tool_output_limit: Some(PAYLOAD_LIMIT_IMPLEMENTER),
                write_scope: SubagentWriteScope::FileSet,
                timeout_seconds: 1_200,
                worktree: SubagentWorktreePolicy::Dedicated,
            },
        ),
        profile(
            cheap.clone(),
            ProfileSpec {
                id: "test_fixer",
                shell: SubagentShell::ReviewedVerifierOnly,
                purpose: "Drive failing tests back to green across the declared file set; needs a verifier command.",
                tools: &["read_file", "edit_file", "run_command"],
                prompt: "Your trade is MAKING A FAILING TEST PASS. The verifier command is the only judge — run it, read the real failure, fix the cause. Prefer fixing the implementation; change the test only when the test itself encodes the wrong expectation, and say so explicitly in your report. Never delete, skip or weaken a test to make it pass. You may edit only the files declared in your task packet.",
                max_turns: 8,
                context_window: WORKER_WINDOW_WIDE,
                tool_output_limit: Some(PAYLOAD_LIMIT_WIDE),
                write_scope: SubagentWriteScope::FileSet,
                timeout_seconds: 900,
                worktree: SubagentWorktreePolicy::Dedicated,
            },
        ),
        profile(
            cheap.clone(),
            ProfileSpec {
                id: "build_fixer",
                shell: SubagentShell::ReviewedVerifierOnly,
                purpose: "Fix compile, type and lint errors across the declared file set; needs a verifier command.",
                tools: &["read_file", "edit_file", "run_command"],
                prompt: "Your trade is MAKING THE BUILD PASS. Read the compiler or linter diagnostic literally: it usually names the file, line and expected type. Make the smallest change that satisfies it without changing behaviour, and never silence a diagnostic with a suppression unless your task packet asked for one. You may edit only the files declared in your task packet.",
                max_turns: 8,
                context_window: WORKER_WINDOW_BALANCED,
                tool_output_limit: Some(PAYLOAD_LIMIT_BALANCED),
                write_scope: SubagentWriteScope::FileSet,
                timeout_seconds: 900,
                worktree: SubagentWorktreePolicy::Dedicated,
            },
        ),
        profile(
            cheap.clone(),
            ProfileSpec {
                id: "tester",
                shell: SubagentShell::Reviewed,
                purpose: "Test and review behavior without modifying source files.",
                tools: &[
                    "search_files",
                    "grep_files",
                    "list_directory",
                    "read_file",
                    "git_status",
                    "git_diff",
                    "run_command",
                ],
                prompt: "Your trade is TESTING AND REVIEW. Reproduce claims with the narrowest relevant checks, inspect failures literally, and report defects with exact evidence. Do not edit source files. Commands that are not statically safe require contextual AI safety review.",
                max_turns: 18,
                context_window: WORKER_WINDOW_WIDE,
                tool_output_limit: Some(PAYLOAD_LIMIT_WIDE),
                write_scope: SubagentWriteScope::None,
                timeout_seconds: 900,
                worktree: SubagentWorktreePolicy::Shared,
            },
        ),
        profile(
            cheap.clone(),
            ProfileSpec {
                id: "ops_runner",
                shell: SubagentShell::Reviewed,
                purpose: "Run bounded operational commands with static, AI, and exact human safety gates.",
                tools: &[
                    "search_files",
                    "grep_files",
                    "list_directory",
                    "read_file",
                    "git_status",
                    "run_command",
                ],
                prompt: "Your trade is BOUNDED OPERATIONS. Inspect before acting and run only commands needed for the declared task. Static safe commands run directly; other non-destructive commands require AI review. Dangerous or credential-sensitive commands are never delegated to the judge. If review declines or is unavailable, return the exact command so the parent can request one-time human authorization through target_command.",
                max_turns: 32,
                context_window: WORKER_WINDOW_BALANCED,
                tool_output_limit: Some(PAYLOAD_LIMIT_BALANCED),
                write_scope: SubagentWriteScope::None,
                timeout_seconds: 1_200,
                worktree: SubagentWorktreePolicy::Shared,
            },
        ),
        profile(
            cheap,
            ProfileSpec {
                // 旧名 `judge`。改叫 reviewer 是为了与 Xedit 的五职责同名；
                // `judge` 与 `security_guard` 继续路由到这里。
                id: "reviewer",
                shell: SubagentShell::None,
                purpose: "Review correctness, risk, and policy boundaries without shell or writes.",
                tools: &[
                    "search_files",
                    "grep_files",
                    "list_directory",
                    "read_file",
                    "git_status",
                    "git_diff",
                ],
                prompt: "Your trade is INDEPENDENT JUDGMENT. Audit the proposed behavior and evidence, identify concrete correctness or safety risks, and distinguish proven facts from uncertainty. You have no shell and make no changes.",
                max_turns: 12,
                context_window: WORKER_WINDOW_BALANCED,
                tool_output_limit: Some(PAYLOAD_LIMIT_BALANCED),
                write_scope: SubagentWriteScope::None,
                timeout_seconds: 600,
                worktree: SubagentWorktreePolicy::Shared,
            },
        ),
    ]
}

struct ProfileSpec<'a> {
    id: &'a str,
    shell: SubagentShell,
    purpose: &'a str,
    tools: &'a [&'a str],
    prompt: &'a str,
    max_turns: usize,
    context_window: u64,
    tool_output_limit: Option<usize>,
    write_scope: SubagentWriteScope,
    worktree: SubagentWorktreePolicy,
    timeout_seconds: u64,
}

fn profile(provider: Arc<dyn Provider>, spec: ProfileSpec<'_>) -> SubagentProfile {
    SubagentProfile {
        id: spec.id.to_owned(),
        purpose: spec.purpose.to_owned(),
        model: None,
        provider,
        tool_names: spec.tools.iter().map(|value| (*value).to_owned()).collect(),
        capability_prompt: spec.prompt.to_owned(),
        max_turns: spec.max_turns,
        context_window: spec.context_window,
        token_budget: None,
        timeout_seconds: Some(spec.timeout_seconds),
        max_consecutive_failures: 3,
        write_scope: spec.write_scope,
        worktree: spec.worktree,
        tool_output_limit: spec.tool_output_limit,
        max_attempts: DEFAULT_MAX_ATTEMPTS,
        shell: spec.shell,
        hosted_job_prompt: false,
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;

    use super::*;
    use crate::provider::ProviderError;
    use crate::types::{Completion, Message, ToolDefinition};

    struct ReportProvider;

    #[derive(Clone)]
    struct ModelProvider {
        model: String,
        seen: Arc<Mutex<Vec<String>>>,
    }

    #[derive(Default)]
    struct CaptureSink(std::sync::Mutex<Vec<AgentEvent>>);

    #[async_trait]
    impl EventSink for CaptureSink {
        async fn emit(&self, event: AgentEvent) {
            self.0.lock().unwrap().push(event);
        }
    }
    #[async_trait]
    impl Provider for ReportProvider {
        async fn complete(
            &self,
            messages: &[Message],
            tools: &[ToolDefinition],
        ) -> Result<Completion, ProviderError> {
            assert!(messages[0].content.contains("cannot spawn another agent"));
            assert!(tools.iter().all(|tool| tool.name != "spawn_agent"));
            Ok(Completion {
                content: "subagent report".to_owned(),
                tool_calls: Vec::new(),
                finish_reason: Some("stop".to_owned()),
                usage: None,
            })
        }
    }

    #[async_trait]
    impl Provider for ModelProvider {
        fn with_model(&self, model: &str) -> Result<Arc<dyn Provider>, ProviderError> {
            Ok(Arc::new(Self {
                model: model.to_owned(),
                seen: self.seen.clone(),
            }))
        }

        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> Result<Completion, ProviderError> {
            self.seen.lock().unwrap().push(self.model.clone());
            Ok(Completion {
                content: format!("report from {}", self.model),
                tool_calls: Vec::new(),
                finish_reason: Some("stop".to_owned()),
                usage: None,
            })
        }
    }

    /// Stands in for the gateway's `someim-security-guard`: a verifier that
    /// writes (as every honest build or test does) is reviewed, not banned.
    struct AllowingJudge;

    #[async_trait]
    impl SafetyJudge for AllowingJudge {
        async fn judge(&self, _request: JudgeRequest) -> JudgeVerdict {
            JudgeVerdict::Allow
        }

        fn model(&self) -> &str {
            "test-judge"
        }
    }

    fn fixture() -> (SubagentCatalog, PathBuf) {
        let root = std::env::temp_dir().join(format!("willdeep-subagent-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("workspace");
        let provider: Arc<dyn Provider> = Arc::new(ReportProvider);
        let background = Arc::new(BackgroundTaskRegistry::default());
        (
            SubagentCatalog::new(&root, builtin_profiles(provider), background),
            root,
        )
    }

    #[test]
    fn approval_denial_marks_background_subagent_blocked() {
        let result = subagent_task_result(Err(AgentError::Tool(ToolError::ApprovalDenied(
            "write access needed".to_owned(),
        ))));
        assert_eq!(result.status, BackgroundTaskStatus::Blocked);
        assert_eq!(result.exit_code, None);
        assert_eq!(result.output, "write access needed");
    }

    /// A report-only trade has no exit code, so the one thing a program can
    /// still check is whether the places it named exist. Both directions
    /// matter: an invented path must be caught, and a real citation must not
    /// be flagged — a check that cries wolf gets switched off.
    #[tokio::test]
    async fn a_report_only_run_is_spot_checked_against_the_files_it_cites() {
        let root =
            std::env::temp_dir().join(format!("willdeep-citations-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src")).expect("workspace");
        std::fs::write(root.join("src/real.rs"), "one\ntwo\nthree\n").expect("fixture");

        let clean = audit_citations(
            &root,
            "The handler lives in `src/real.rs:2`, and `src/real.rs` has no other callers.",
        )
        .await;
        assert_eq!(clean.checked, 2, "both citations are checkable");
        assert!(
            clean.unverifiable.is_empty(),
            "a real path and an in-range line must not be flagged: {:?}",
            clean.unverifiable
        );

        let dirty = audit_citations(
            &root,
            "See `src/invented.rs:10` and `src/real.rs:900` for the retry logic.",
        )
        .await;
        assert_eq!(dirty.checked, 2);
        assert_eq!(
            dirty.unverifiable.len(),
            2,
            "a path that does not exist and a line past the end are both citations of nothing: {:?}",
            dirty.unverifiable
        );

        // Prose is not a citation. Counting words the parser merely failed to
        // understand would measure the parser, not the worker.
        let prose = audit_citations(&root, "The retry logic looks correct to me.").await;
        assert_eq!(prose.checked, 0);
        assert!(prose.note().is_none(), "nothing checked, nothing to report");

        std::fs::remove_dir_all(root).expect("cleanup");
    }

    /// A commit hash that does not resolve is the `git_detective` version of
    /// an invented path.
    #[tokio::test]
    async fn a_cited_commit_that_does_not_resolve_is_flagged() {
        let root = std::env::temp_dir().join(format!("willdeep-commits-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("workspace");
        std::fs::write(root.join("file.txt"), "seed").expect("fixture");
        for args in [
            vec!["init", "--quiet", "--initial-branch=main"],
            vec!["add", "."],
            vec![
                "-c",
                "user.name=range",
                "-c",
                "user.email=range@local",
                "commit",
                "--quiet",
                "-m",
                "seed",
            ],
        ] {
            std::process::Command::new("git")
                .args(&args)
                .current_dir(&root)
                .status()
                .expect("git");
        }
        let head = String::from_utf8(
            std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&root)
                .output()
                .expect("head")
                .stdout,
        )
        .expect("utf8");
        let head = head.trim();

        let audit = audit_citations(
            &root,
            &format!("Introduced in {head}, not in 0badc0de1234 as the report claimed."),
        )
        .await;
        assert_eq!(audit.checked, 2, "both hashes are checkable");
        assert_eq!(
            audit.unverifiable,
            vec!["0badc0de1234".to_owned()],
            "only the hash that does not resolve is flagged"
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    /// A packet that names a skill gets the skill body inlined by the
    /// runtime; a skill this runtime does not have is named as unavailable —
    /// a worker must never improvise the procedure it thinks it was given.
    #[tokio::test]
    async fn a_named_skill_is_inlined_and_a_missing_one_is_called_out() {
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
                    .unwrap()
                    .push(messages.last().unwrap().content.clone());
                Ok(Completion {
                    content: "report".to_owned(),
                    tool_calls: Vec::new(),
                    finish_reason: Some("stop".to_owned()),
                    usage: None,
                })
            }
        }

        let root = std::env::temp_dir().join(format!("willdeep-skillpkt-{}", uuid::Uuid::new_v4()));
        let skill_dir = root.join(".willdeep/skills/convert");
        std::fs::create_dir_all(&skill_dir).expect("skill dir");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: convert\ndescription: convert images\ntier: worker\n---\n# Steps\nUse sips.",
        )
        .expect("skill");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let provider: Arc<dyn Provider> = Arc::new(PromptProbe(seen.clone()));
        let catalog = SubagentCatalog::new(
            &root,
            builtin_profiles(provider),
            Arc::new(BackgroundTaskRegistry::default()),
        )
        .with_skills(Arc::new(crate::skills::SkillCatalog::discover(&root, &[])));

        for (skill, marker) in [
            ("convert", "Use sips."),
            ("no-such-skill", "status=\"unavailable"),
        ] {
            catalog
                .run(
                    SpawnAgentArgs {
                        prompt: "do it".to_owned(),
                        profile: Some("scout".to_owned()),
                        run_in_background: Some(false),
                        task: Some(TaskPacket {
                            goal: "convert the asset".to_owned(),
                            skill: Some(skill.to_owned()),
                            ..TaskPacket::default()
                        }),
                        ..SpawnAgentArgs::default()
                    },
                    None,
                )
                .await
                .expect("run");
            let prompt = seen.lock().unwrap().last().cloned().unwrap();
            assert!(
                prompt.contains(marker),
                "skill {skill} should surface {marker}: {prompt}"
            );
        }
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    /// Oversized material with digestion on is sharded and summarized by the
    /// worker model, labeled as a digest — never silently dropped, never
    /// passed off as the file itself.
    #[tokio::test]
    async fn oversized_material_is_digested_not_dropped() {
        struct DigestProvider(Arc<Mutex<Vec<String>>>);

        #[async_trait]
        impl Provider for DigestProvider {
            async fn complete(
                &self,
                messages: &[Message],
                _tools: &[ToolDefinition],
            ) -> Result<Completion, ProviderError> {
                let content = messages.last().unwrap().content.clone();
                self.0.lock().unwrap().push(content.clone());
                Ok(Completion {
                    content: if content.contains("Summarize this chunk") {
                        "digest: the assertion `left == 42` fails".to_owned()
                    } else {
                        "report".to_owned()
                    },
                    tool_calls: Vec::new(),
                    finish_reason: Some("stop".to_owned()),
                    usage: None,
                })
            }
        }

        let root = std::env::temp_dir().join(format!("willdeep-digest-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("workspace");
        // Far past any inline budget for a deployable worker profile.
        std::fs::write(root.join("huge.log"), "x".repeat(64 * 1024)).expect("fixture");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let provider: Arc<dyn Provider> = Arc::new(DigestProvider(seen.clone()));
        let catalog = SubagentCatalog::new(
            &root,
            builtin_profiles(provider),
            Arc::new(BackgroundTaskRegistry::default()),
        );
        catalog
            .run(
                SpawnAgentArgs {
                    prompt: "explain".to_owned(),
                    profile: Some("log_inspector".to_owned()),
                    run_in_background: Some(false),
                    task: Some(TaskPacket {
                        goal: "explain the failure".to_owned(),
                        relevant_files: vec!["huge.log".to_owned()],
                        digest_oversized: Some(true),
                        ..TaskPacket::default()
                    }),
                    ..SpawnAgentArgs::default()
                },
                None,
            )
            .await
            .expect("run");
        let prompts = seen.lock().unwrap().clone();
        assert!(
            prompts.iter().any(|p| p.contains("Summarize this chunk")),
            "the digest path must actually call the model"
        );
        let brief = prompts
            .iter()
            .find(|p| p.contains("<relevant-files>"))
            .expect("worker brief");
        assert!(
            brief.contains("status=\"digested"),
            "the digest must be labeled a digest: {brief}"
        );
        assert!(
            brief.contains("left == 42"),
            "chunk digests must reach the brief"
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    /// The trade→model table is shared with the macOS app. One operator, one
    /// gateway: if the two clients disagree here, the same trade quietly runs
    /// on two different models depending on which app was opened.
    /// 七条 per-trade 模型链已经收敛到一个基础档。它们当年存在的理由是
    /// 服务端 prepend 职责提示词，而职责提示词现在随请求走——留着就是双重
    /// 注入，而且每加一种职责都得先在网关上铺一条链才能发版。
    #[test]
    fn every_hosted_trade_resolves_to_the_shared_base_model() {
        for trade in crate::worker_tier::LEGACY_HOSTED_TRADES {
            assert_eq!(
                hosted_worker_model(trade).as_deref(),
                Some("someim-32b"),
                "{trade} should resolve to the shared base tier"
            );
        }
        // generalist 合并了四个基础档窄工种，默认也落在基础档。
        assert_eq!(
            hosted_worker_model("generalist").as_deref(),
            Some("someim-32b")
        );
        // 没有默认绑定的工种仍然返回 None，而不是凭空造一个网关会拒绝的名字：
        // implementer 是日常编码主力，跑的是更强的那一档。
        assert_eq!(hosted_worker_model("implementer"), None);
        assert_eq!(hosted_worker_model("ops_runner"), None);
    }

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

    /// The packet is the worker's whole starting position: goal, facts,
    /// constraints, verifier and the file contents themselves, so it never
    /// spends a turn re-finding what the parent already knew.
    #[tokio::test]
    async fn the_task_packet_inlines_what_the_parent_already_knows() {
        let root = std::env::temp_dir().join(format!("willdeep-brief-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("workspace");
        std::fs::write(root.join("target.rs"), "fn broken() {}\n").expect("fixture");
        let profile = builtin_profiles(Arc::new(ReportProvider) as Arc<dyn Provider>)
            .into_iter()
            .find(|profile| profile.id == "test_fixer")
            .expect("test_fixer profile");
        let brief = compose_brief(
            "fix it",
            Some(&TaskPacket {
                goal: "make testCrossSignOff pass".to_owned(),
                read_files: vec!["target.rs".to_owned(), "missing.rs".to_owned()],
                write_files: vec!["target.rs".to_owned()],
                relevant_files: Vec::new(),
                known_facts: vec!["broke at caba8df7".to_owned()],
                constraints: vec!["do not change the public API".to_owned()],
                verifier: Some(TaskVerifier {
                    command: "cargo test -p core".to_owned(),
                    expected_exit_code: None,
                }),
                max_attempts: None,
                skill: None,
                digest_oversized: None,
            }),
            &root,
            &profile,
            None,
        )
        .await;
        assert!(brief.contains("make testCrossSignOff pass"));
        assert!(brief.contains("broke at caba8df7"));
        assert!(brief.contains("do not change the public API"));
        assert!(brief.contains("cargo test -p core"));
        assert!(
            brief.contains("fn broken() {}"),
            "relevant files must arrive inlined, not as paths to go find: {brief}"
        );
        assert!(
            brief.contains("unreadable"),
            "a file that could not be read must be named as such, not silently dropped: {brief}"
        );
        assert!(brief.contains("<instruction>\nfix it\n</instruction>"));
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

    /// Deployable windows are asserted rather than left to drift: narrow
    /// trades stay within 32K-64K, implementer owns the 256K daily-coding
    /// tier, and `generalist` gets the 128K base budget. Nothing inherits the
    /// session window by default any more — that is what the expert tier
    /// cashes in, and it costs a ticket.
    #[test]
    fn workers_run_in_deployable_windows_with_capped_payloads() {
        let provider: Arc<dyn Provider> = Arc::new(ReportProvider);
        for profile in builtin_profiles(provider) {
            if profile.id == "generalist" {
                assert_eq!(
                    profile.context_window,
                    crate::WorkerTier::Standard.context_budget()
                );
                assert_eq!(profile.tool_output_limit, Some(PAYLOAD_LIMIT_WIDE));
                continue;
            }
            if profile.id == "implementer" {
                assert_eq!(profile.context_window, STANDARD_WINDOW);
                assert_eq!(profile.tool_output_limit, Some(PAYLOAD_LIMIT_IMPLEMENTER));
                continue;
            }
            assert!(
                profile.context_window >= WORKER_WINDOW_STANDARD
                    && profile.context_window <= WORKER_WINDOW_WIDE,
                "{} must run in a 32K-64K worker window, got {}",
                profile.id,
                profile.context_window
            );
            let limit = profile
                .tool_output_limit
                .unwrap_or_else(|| panic!("{} must cap its tool payloads", profile.id));
            assert!(
                (limit as u64) < profile.context_window,
                "{} caps payloads at {limit} bytes, which its {}-token window cannot absorb",
                profile.id,
                profile.context_window
            );
        }
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
