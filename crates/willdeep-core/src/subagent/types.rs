//! The dispatch contract: what a trade is allowed to do, what the parent
//! hands it, and what the tool call carries. Everything here is data — no
//! scheduling, no execution — so both the catalog and the runner can depend
//! on it without depending on each other.

use std::sync::Arc;

use serde::Deserialize;

use crate::provider::Provider;
use crate::routing::EscalationTicket;
use crate::subagent_worktree::SubagentWorktreePolicy;

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
    pub(super) fn requires_verifier(self) -> bool {
        matches!(self, Self::VerifierOnly | Self::ReviewedVerifierOnly)
    }

    pub(super) fn uses_intelligent_review(self) -> bool {
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
    pub(super) fn inline_files(&self) -> Vec<String> {
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
    pub(super) fn expected_exit_code(&self) -> i32 {
        self.expected_exit_code.unwrap_or(0)
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
