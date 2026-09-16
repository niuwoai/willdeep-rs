use std::collections::{BTreeSet, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use futures_util::StreamExt;
use globset::Glob;
use ignore::WalkBuilder;
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::background::{
    BackgroundTaskKind, BackgroundTaskRegistry, BackgroundTaskStatus, TaskResult,
};
use crate::hooks::{HookEvent, HookPayload, HookRegistry};
use crate::judge::{JudgeRequest, JudgeVerdict, SafetyJudge};
use crate::safety::CommandSafety;
use crate::sandbox::{SandboxPolicy, SandboxSpec};
use crate::types::{ToolCall, ToolDefinition};
use crate::{McpRegistry, SkillCatalog};

const DEFAULT_MAX_RESULTS: usize = 60;
const MAX_RESULTS: usize = 200;
const DEFAULT_READ_BYTES: usize = 64 * 1024;
const MAX_READ_BYTES: usize = 256 * 1024;
const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 60;
const MAX_COMMAND_TIMEOUT_SECS: u64 = 600;
const MAX_COMMAND_OUTPUT_BYTES: usize = 128 * 1024;
const MAX_SUPERVISOR_REQUEST_BYTES: usize = 256 * 1024;
const BACKGROUND_SUPERVISOR_ENV: &str = "WILLDEEP_INTERNAL_BACKGROUND_SUPERVISOR";
const MAX_WEB_RESPONSE_BYTES: usize = 3 * 1024 * 1024;
const MAX_WEB_REQUEST_BYTES: usize = 1024 * 1024;
const DEFAULT_WEB_POST_CONTENT_TYPE: &str = "application/json";
const MAX_VERIFICATION_SUMMARY_BYTES: usize = 8 * 1024;
/// Ceiling on how many files one writing subagent may claim. Sixteen files
/// cover a bounded feature slice while keeping the write set reviewable; a
/// larger change should still be split by the parent.
const MAX_SUBAGENT_WRITE_TARGETS: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandVerification {
    pub snapshot_id: Option<String>,
    pub command: String,
    pub exit_code: Option<i32>,
    pub status: VerificationStatus,
    pub summary: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    Passed,
    Failed,
    TimedOut,
    LaunchFailed,
}

type VerificationReporter = Arc<dyn Fn(CommandVerification) + Send + Sync>;
type VerificationSnapshot = Arc<dyn Fn() -> Result<Option<String>, String> + Send + Sync>;

/// Why a command ran without an approval card — or why it needed one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalSource {
    /// The static classifier proved the command read-only or bounded.
    StaticAllowlist,
    /// The AI judge returned YES for this exact action.
    Judge,
    /// A rule the operator previously chose to always allow.
    AlwaysAllowList,
    /// The user was asked.
    User,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalTrace {
    pub command: String,
    pub source: ApprovalSource,
    /// Short, user-facing explanation ("static allowlist: read-only",
    /// "judge unavailable: connection refused").
    pub detail: String,
}

type ApprovalReporter = Arc<dyn Fn(ApprovalTrace) + Send + Sync>;
const DEFAULT_WEB_MAX_CHARS: usize = 20_000;
const MAX_WEB_MAX_CHARS: usize = 100_000;
const MAX_WEB_REDIRECTS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalMode {
    ReadOnly,
    Strict,
    Smart,
    WorkspaceAccess,
}

#[derive(Clone, Debug)]
pub struct WebToolConfig {
    pub some_im_base_url: String,
    pub api_key: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalDecision {
    AllowOnce,
    Deny,
    AlwaysAllow,
}

#[derive(Clone, Debug)]
pub struct UserQuestion {
    pub question: String,
    pub options: Vec<String>,
    pub multi_select: bool,
}

#[async_trait]
pub trait Approver: Send + Sync {
    async fn approve(&self, description: &str, always_allow_available: bool) -> ApprovalDecision;
    async fn ask_user(&self, _question: UserQuestion) -> Option<String> {
        None
    }
}

struct DenyApprover;

#[async_trait]
impl Approver for DenyApprover {
    async fn approve(&self, _description: &str, _always_allow_available: bool) -> ApprovalDecision {
        ApprovalDecision::Deny
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("unknown tool: {0}")]
    UnknownTool(String),
    #[error("invalid arguments for {tool}: {source}")]
    InvalidArguments {
        tool: String,
        source: serde_json::Error,
    },
    #[error("path escapes the workspace: {0}")]
    OutsideWorkspace(String),
    #[error("approval denied: {0}")]
    ApprovalDenied(String),
    #[error("read-only Workspace policy blocks tool: {0}")]
    ReadOnlyPolicy(String),
    /// 被生命周期 hook 拦下。理由由 hook 的 stderr 提供，已点名是哪一条。
    #[error("{0}")]
    HookDenied(String),
    #[error("file already exists: {0}")]
    FileAlreadyExists(String),
    #[error("exact edit text was not found in {0}")]
    EditTextNotFound(String),
    #[error(
        "exact edit text appears {count} times in {path}; provide more context or set replace_all"
    )]
    EditTextNotUnique { path: String, count: usize },
    #[error("old_string and new_string must differ")]
    IdenticalEdit,
    #[error("invalid regular expression: {0}")]
    InvalidRegex(String),
    #[error("invalid filename glob: {0}")]
    InvalidGlob(String),
    #[error("filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("command timed out after {seconds} seconds\n{output}")]
    CommandTimeout { seconds: u64, output: String },
    #[error("network operation failed: {0}")]
    Network(String),
    #[error(transparent)]
    Skill(#[from] crate::skills::SkillError),
    #[error(transparent)]
    Mcp(#[from] crate::mcp::McpError),
}

pub struct ToolRegistry {
    workspace: PathBuf,
    approval_mode: ApprovalMode,
    approver: Arc<dyn Approver>,
    skills: Arc<SkillCatalog>,
    mcp: Arc<McpRegistry>,
    web: Option<WebToolConfig>,
    background: Arc<BackgroundTaskRegistry>,
    /// 脱离父进程的后台作业。挂上它之后，显式 `run_in_background` 的命令不再
    /// 随 Harness 一起死：升级、重启、退出都不影响，回来按记录取结果。
    detached_jobs: Option<Arc<crate::detached_job::DetachedJobStore>>,
    allowed_tools: Option<HashSet<String>>,
    /// Every path a subagent is allowed to write. `None` means the registry
    /// is not write-scoped at all (the main agent); a set means writes are
    /// confined to exactly these canonical paths. The single-file `editor`
    /// profile is the set-of-one special case — one gate, not two.
    write_targets: Option<BTreeSet<PathBuf>>,
    read_only_git_shell: bool,
    /// Exact command lines a write-scoped worker may run. `None` leaves
    /// `run_command` under the ordinary approval chain; a set means only
    /// these literal commands (its verifier) are runnable at all.
    command_allowlist: Option<HashSet<String>>,
    /// Child-worker shell policy: static safe commands pass, the ambiguous
    /// middle goes to the AI judge, and denial/unavailability is returned to
    /// the parent instead of trying to show UI from the child context.
    reviewed_subagent_shell: bool,
    /// Commands authorized verbatim by the parent after a human approval.
    /// Nothing derived from or decorated around these strings is authorized.
    preapproved_commands: HashSet<String>,
    /// Byte cap on tool payloads handed back to the model. `None` keeps the
    /// per-tool defaults the main agent has always used; `Some` is the
    /// small-context worker budget — a 128 KB test log would eat a 32K
    /// window whole, and so would one 256 KB file read.
    tool_output_limit: Option<usize>,
    /// Append a delegation hint when a test/build command fails. Only the
    /// main agent gets these — a subagent cannot spawn anything, so the hint
    /// would be an instruction it has no way to follow.
    delegation_hints: bool,
    always_allowed: Arc<Mutex<HashSet<String>>>,
    always_allow_path: Option<PathBuf>,
    verification_reporter: Option<VerificationReporter>,
    verification_snapshot: Option<VerificationSnapshot>,
    verification_records: Arc<Mutex<verification::EvidenceRecords>>,
    safety_judge: Option<Arc<dyn SafetyJudge>>,
    task_context: Arc<Mutex<String>>,
    approval_reporter: Option<ApprovalReporter>,
    /// OS 级写入围栏。默认 `Off`：审批闸门判的是「模型请求做什么」，这一层
    /// 判的是「进程实际能做什么」，两者互补而不互替。
    sandbox: SandboxSpec,
    /// 生命周期挂钩。默认空：没配 hook 的用户不该为此付任何成本。
    hooks: HookRegistry,
    /// 只用于 hook 事件的溯源字段，不参与任何判定。
    session_id: Option<String>,
    output_store: crate::tool_output::ToolOutputStore,
}

impl ToolRegistry {
    pub fn new(
        workspace: impl AsRef<Path>,
        approval_mode: ApprovalMode,
    ) -> Result<Self, ToolError> {
        let workspace = workspace.as_ref().canonicalize()?;
        if !workspace.is_dir() {
            return Err(ToolError::Io(std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                "workspace is not a directory",
            )));
        }
        Ok(Self {
            output_store: crate::tool_output::ToolOutputStore::new(
                &std::env::temp_dir().join("willdeep-tool-outputs"),
                &workspace,
            ),
            workspace,
            approval_mode,
            approver: Arc::new(DenyApprover),
            skills: Arc::new(SkillCatalog::default()),
            mcp: Arc::new(McpRegistry::default()),
            web: None,
            background: Arc::new(BackgroundTaskRegistry::default()),
            allowed_tools: None,
            write_targets: None,
            detached_jobs: None,
            read_only_git_shell: false,
            command_allowlist: None,
            reviewed_subagent_shell: false,
            preapproved_commands: HashSet::new(),
            tool_output_limit: None,
            delegation_hints: false,
            always_allowed: Arc::new(Mutex::new(HashSet::new())),
            always_allow_path: None,
            verification_reporter: None,
            verification_snapshot: None,
            verification_records: Arc::new(Mutex::new(verification::EvidenceRecords::default())),
            safety_judge: None,
            task_context: Arc::new(Mutex::new(String::new())),
            approval_reporter: None,
            sandbox: SandboxSpec::new(SandboxPolicy::Off, []),
            hooks: HookRegistry::default(),
            session_id: None,
        }
        // Completion evidence belongs to the executor, even when its caller
        // does not subscribe to external verification reports.
        .with_verification_reporter(|_| {}))
    }

    /// 注册生命周期挂钩。与审批闸门是两回事：闸门问的是用户，hook 问的是
    /// 用户**事先配好的程序**——审计留痕和 CI 门禁要的是后者。
    pub fn with_hooks(mut self, hooks: HookRegistry) -> Self {
        self.hooks = hooks;
        self
    }

    pub(crate) fn allows_parallel_read(&self, call: &ToolCall) -> bool {
        self.hooks.is_empty()
            && matches!(
                call.name.as_str(),
                "read_file" | "list_directory" | "search_files" | "grep_files" | "read_tool_output"
            )
    }

    /// 带上会话标识，hook 的审计记录里才对得上是哪一次会话。
    pub fn with_session_id(mut self, session_id: Option<String>) -> Self {
        self.session_id = session_id;
        self
    }

    fn hook_payload(&self, event: HookEvent, call: &ToolCall) -> HookPayload {
        HookPayload::new(event)
            .with_tool(call.name.clone(), &call.arguments)
            .with_session(
                self.session_id.clone(),
                Some(self.workspace.display().to_string()),
            )
    }

    /// 套上 OS 级写入围栏。不传等于不套——这一层是加固，不是前提，
    /// 关掉它其余三道闸门照常工作。
    pub fn with_sandbox(mut self, sandbox: SandboxSpec) -> Self {
        self.sandbox = sandbox;
        self
    }

    /// Attach the AI judge consulted for commands the static classifier
    /// cannot decide. Without one, those commands go straight to the user.
    pub fn with_safety_judge(mut self, judge: Arc<dyn SafetyJudge>) -> Self {
        self.safety_judge = Some(judge);
        self
    }

    /// Observe every automatic approval decision (static allow, judge
    /// allow, escalation to the user) so the UI can explain itself.
    pub fn with_approval_reporter<F>(mut self, reporter: F) -> Self
    where
        F: Fn(ApprovalTrace) + Send + Sync + 'static,
    {
        self.approval_reporter = Some(Arc::new(reporter));
        self
    }

    /// The operator's current goal, handed to the judge as inert context.
    /// Set once per user turn; never used to widen a static rule.
    pub fn set_task_context(&self, value: &str) {
        let mut context = self.task_context.lock().expect("task context");
        context.clear();
        context.push_str(value.trim());
    }

    pub fn with_approver(mut self, approver: Arc<dyn Approver>) -> Self {
        self.approver = approver;
        self
    }

    pub fn with_skills(mut self, skills: Arc<SkillCatalog>) -> Self {
        self.skills = skills;
        self
    }
    /// 让显式后台命令脱离父进程，结果落盘。
    pub fn with_detached_jobs(mut self, store: Arc<crate::detached_job::DetachedJobStore>) -> Self {
        self.detached_jobs = Some(store);
        self
    }

    pub fn with_mcp(mut self, mcp: Arc<McpRegistry>) -> Self {
        self.mcp = mcp;
        self
    }
    pub fn with_web_tools(mut self, config: Option<WebToolConfig>) -> Self {
        self.web = config;
        self
    }
    pub fn with_background_tasks(mut self, registry: Arc<BackgroundTaskRegistry>) -> Self {
        self.background = registry;
        self
    }
    pub fn with_verification_reporter<F>(mut self, reporter: F) -> Self
    where
        F: Fn(CommandVerification) + Send + Sync + 'static,
    {
        let records = self.verification_records.clone();
        self.verification_reporter = Some(Arc::new(move |record| {
            let mut records = records.lock().unwrap_or_else(|error| error.into_inner());
            records.record(record.clone());
            drop(records);
            reporter(record);
        }));
        self
    }
    pub fn with_verification_snapshot<F>(self, capture: F) -> Self
    where
        F: Fn() -> Option<String> + Send + Sync + 'static,
    {
        self.with_fallible_verification_snapshot(move || Ok(capture()))
    }
    /// `Ok(None)` means snapshots are unsupported; `Err` means capture failed.
    pub fn with_fallible_verification_snapshot<F>(mut self, capture: F) -> Self
    where
        F: Fn() -> Result<Option<String>, String> + Send + Sync + 'static,
    {
        self.verification_snapshot = Some(Arc::new(capture));
        self
    }
    pub fn with_always_allow_store(mut self, path: PathBuf) -> Result<Self, ToolError> {
        #[cfg(unix)]
        if path.exists() {
            use std::os::unix::fs::PermissionsExt;
            if std::fs::metadata(&path)?.permissions().mode() & 0o077 != 0 {
                return Err(ToolError::Io(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "always-allow store must have 0600 permissions",
                )));
            }
        }
        let stored = if path.exists() {
            serde_json::from_str::<Vec<String>>(&std::fs::read_to_string(&path)?).map_err(
                |error| ToolError::Network(format!("invalid always-allow store: {error}")),
            )?
        } else {
            Vec::new()
        };
        // Rules minted before the credential guard can still be sitting in the
        // file with a secret inside them. Drop them on load and rewrite, so the
        // exposure ends at the next start rather than waiting for someone to
        // read a file whose whole purpose is to stop being read. They are dead
        // weight regardless: `command_signature` no longer mints a signature
        // that could match them.
        let (kept, dropped): (Vec<String>, Vec<String>) = stored
            .into_iter()
            .partition(|rule| !rule_carries_credentials(rule));
        self.always_allowed = Arc::new(Mutex::new(kept.into_iter().collect()));
        self.always_allow_path = Some(path);
        if !dropped.is_empty() {
            self.persist_always_allowed()?;
        }
        Ok(self)
    }
    pub fn with_allowed_tools(mut self, names: impl IntoIterator<Item = String>) -> Self {
        self.allowed_tools = Some(names.into_iter().collect());
        self
    }
    /// Confine writes to exactly these files.
    ///
    /// The targets are canonicalized here because the check they feed compares
    /// them against a canonicalized edit path. One symlinked component
    /// anywhere above the workspace — `/tmp`, `/var` on macOS, a symlinked
    /// checkout — and the two spellings never match: every edit the worker
    /// sends is refused as "outside the workspace" even though the file is
    /// the approved one, and the refusal names a path identical to the one it
    /// just asked for. A worker cannot argue its way out of that, so it burns
    /// its whole turn budget re-sending a correct patch.
    pub fn with_write_targets(mut self, targets: Option<BTreeSet<PathBuf>>) -> Self {
        self.write_targets = targets.map(|targets| {
            targets
                .into_iter()
                .map(|target| target.canonicalize().unwrap_or(target))
                .collect()
        });
        self
    }

    /// Allow read-only `git` and nothing else.
    ///
    /// The regression-archaeology trade has to compose its own queries —
    /// `git log -p`, `git show <sha>`, `git diff <a> <b>` — so a literal
    /// allowlist cannot express what it needs, and the fixed git tools cannot
    /// compare two commits at all. The rule is therefore a shape: the command
    /// head must be `git`, and it must still pass the same static read-only
    /// classification as any other command. Nothing else runs, and there is
    /// no judge to appeal to.
    pub fn with_read_only_git_shell(mut self, enabled: bool) -> Self {
        self.read_only_git_shell = enabled;
        self
    }

    /// Confine `run_command` to exactly these literal command lines.
    pub fn with_command_allowlist(mut self, commands: Option<HashSet<String>>) -> Self {
        self.command_allowlist = commands;
        self
    }

    pub fn with_reviewed_subagent_shell(mut self, enabled: bool) -> Self {
        self.reviewed_subagent_shell = enabled;
        self
    }

    pub fn with_preapproved_commands(mut self, commands: impl IntoIterator<Item = String>) -> Self {
        self.preapproved_commands = commands
            .into_iter()
            .map(|command| command.trim().to_owned())
            .filter(|command| !command.is_empty())
            .collect();
        self
    }

    /// Cap every tool payload this registry returns. Values below 1 KB are
    /// raised to 1 KB: a cap that truncates the failing assertion itself
    /// defeats the point of showing the output at all.
    pub fn with_tool_output_limit(mut self, limit: usize) -> Self {
        self.tool_output_limit = Some(limit.clamp(1_024, MAX_COMMAND_OUTPUT_BYTES));
        self
    }

    fn command_output_limit(&self) -> usize {
        self.tool_output_limit.unwrap_or(MAX_COMMAND_OUTPUT_BYTES)
    }

    fn read_bytes_limit(&self) -> usize {
        self.tool_output_limit.unwrap_or(MAX_READ_BYTES)
    }

    /// Append the "this failure is delegable" hint to failing test/build
    /// commands. Main agent only.
    pub fn with_delegation_hints(mut self, enabled: bool) -> Self {
        self.delegation_hints = enabled;
        self
    }

    /// Resolve a subagent's declared write scope before it starts. Existing
    /// files are canonicalized and new files keep their validated workspace
    /// path. Smart/workspace-write mode inherits the main Agent's normal
    /// workspace write permission; strict mode still presents the whole set
    /// on one approval card.
    pub async fn approve_subagent_write_set(
        &self,
        requested: &[String],
    ) -> Result<BTreeSet<PathBuf>, ToolError> {
        if self.approval_mode == ApprovalMode::ReadOnly {
            return Err(ToolError::ReadOnlyPolicy("writing subagent".to_owned()));
        }
        if requested.is_empty() {
            return Err(ToolError::OutsideWorkspace(
                "a writing subagent needs at least one target file".to_owned(),
            ));
        }
        if requested.len() > MAX_SUBAGENT_WRITE_TARGETS {
            return Err(ToolError::OutsideWorkspace(format!(
                "a writing subagent may claim at most {MAX_SUBAGENT_WRITE_TARGETS} files, got {}",
                requested.len()
            )));
        }
        let mut targets = BTreeSet::new();
        for path in requested {
            let candidate = self.workspace.join(path);
            let target = if candidate.exists() {
                let target = self.resolve_existing(path)?;
                if !target.is_file() {
                    return Err(ToolError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("subagent write target is not a file: {path}"),
                    )));
                }
                target
            } else {
                self.resolve_new(path)?
            };
            targets.insert(target);
        }
        let listing = targets
            .iter()
            .map(|target| format!("  - {}", display_relative(&self.workspace, target)))
            .collect::<Vec<_>>()
            .join("\n");
        self.require_approval(
            &format!(
                "allow subagent to modify exactly these {} file(s):\n{listing}",
                targets.len()
            ),
            true,
        )
        .await?;
        Ok(targets)
    }

    /// Ask the human about one exact command a child could not authorize by
    /// static policy or AI review. The returned string is the capability:
    /// the child accepts only a byte-for-byte match after trimming the outer
    /// whitespace, and receives no remembered or wildcard authority.
    pub async fn approve_subagent_command(&self, command: &str) -> Result<String, ToolError> {
        if self.approval_mode == ApprovalMode::ReadOnly {
            return Err(ToolError::ReadOnlyPolicy(
                "subagent target_command".to_owned(),
            ));
        }
        let command = command.trim();
        if command.is_empty()
            || command.len() > 16 * 1024
            || command
                .chars()
                .any(|character| character == '\0' || matches!(character, '\n' | '\r'))
        {
            return Err(ToolError::ApprovalDenied(
                "target_command must contain 1 to 16384 bytes on one line".to_owned(),
            ));
        }
        self.require_approval(
            &format!("allow ops_runner subagent to run this exact command once:\n{command}"),
            false,
        )
        .await?;
        Ok(command.to_owned())
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub fn with_output_store(mut self, root: &Path) -> Self {
        self.output_store = crate::tool_output::ToolOutputStore::new(root, &self.workspace);
        self
    }

    pub(crate) fn archive_output(&self, text: &str) -> Result<String, ToolError> {
        Ok(self.output_store.save(text)?)
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        let mut tools = vec![
            definition(
                "read_tool_output",
                "Read a page of an archived tool result by its opaque id. Offsets and limits count Unicode characters; maximum 8000 characters per page.",
                json!({"type":"object","properties":{"id":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":8000}},"required":["id"],"additionalProperties":false}),
            ),
            definition(
                "list_skills",
                "List installed WillDeep/Codex-compatible skills. Read a relevant skill before applying it.",
                json!({"type":"object","properties":{"query":{"type":"string"}},"additionalProperties":false}),
            ),
            definition(
                "read_skill",
                "Read an installed SKILL.md or a safe resource inside that skill directory.",
                json!({"type":"object","properties":{"name":{"type":"string"},"resource":{"type":"string"}},"required":["name"],"additionalProperties":false}),
            ),
            definition(
                "search_files",
                "Search workspace files for a literal text query and return matching lines with paths and line numbers. Read-only.",
                json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "Literal text to search for. Case-insensitive."},
                        "max_results": {"type": "integer", "description": "Maximum matches. Defaults to 60, capped at 200."}
                    },
                    "required": ["query"], "additionalProperties": false
                }),
            ),
            definition(
                "grep_files",
                "Search workspace files with a regular expression and return matching lines with paths and line numbers. Read-only.",
                json!({
                    "type": "object",
                    "properties": {
                        "pattern": {"type": "string", "description": "Regular expression pattern."},
                        "path": {"type": "string", "description": "Optional workspace-relative directory."},
                        "include": {"type": "string", "description": "Optional filename glob such as *.rs."},
                        "case_sensitive": {"type": "boolean", "description": "Defaults to false."},
                        "max_results": {"type": "integer", "description": "Defaults to 60, capped at 200."}
                    },
                    "required": ["pattern"], "additionalProperties": false
                }),
            ),
            definition(
                "read_file",
                "Read a UTF-8 text file inside the workspace. Returns line-numbered content and a continuation hint when truncated.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Workspace-relative path."},
                        "offset": {"type": "integer", "minimum": 1, "description": "1-based starting line."},
                        "limit": {"type": "integer", "minimum": 1, "description": "Maximum lines."},
                        "max_bytes": {"type": "integer", "minimum": 1, "description": "Defaults to 64KB, capped at 256KB."}
                    },
                    "required": ["path"], "additionalProperties": false
                }),
            ),
            definition(
                "list_directory",
                "List the immediate entries of a workspace directory with their type. Read-only.",
                json!({
                    "type": "object",
                    "properties": {"path": {"type": "string", "description": "Empty means workspace root."}},
                    "additionalProperties": false
                }),
            ),
            definition(
                "git_status",
                "Return the current Git branch and porcelain status for the workspace. Read-only.",
                json!({"type": "object", "properties": {}, "additionalProperties": false}),
            ),
            definition(
                "git_diff",
                "Return the workspace diff, optionally for one path. Read-only.",
                json!({"type":"object","properties":{"path":{"type":"string"},"staged":{"type":"boolean"},"stat_only":{"type":"boolean"}},"additionalProperties":false}),
            ),
            definition(
                "git_log",
                "Return bounded commit history, optionally restricted to one workspace path. Read-only.",
                json!({"type":"object","properties":{"path":{"type":"string"},"max_count":{"type":"integer","minimum":1,"maximum":100},"author":{"type":"string"},"since":{"type":"string","description":"Git date expression such as 2 weeks ago or 2026-01-01."}},"additionalProperties":false}),
            ),
            definition(
                "git_blame",
                "Return bounded line attribution for one workspace file. Read-only.",
                json!({"type":"object","properties":{"path":{"type":"string"},"start_line":{"type":"integer","minimum":1},"end_line":{"type":"integer","minimum":1}},"required":["path"],"additionalProperties":false}),
            ),
            definition(
                "list_worktrees",
                "List Git worktrees with path, HEAD, branch, detached and prunable state. Read-only.",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
            definition(
                "create_worktree",
                "Create a Git worktree for a new branch under ~/.willdeep/worktrees. Requires approval.",
                json!({"type":"object","properties":{"branch":{"type":"string"}},"required":["branch"],"additionalProperties":false}),
            ),
            definition(
                "get_job_output",
                "Read the status and recent output of a background shell job or subagent. Returns the last 200 lines by default; the full log stays at output_path for grep.",
                json!({"type":"object","properties":{"job_id":{"type":"string"},"tail_lines":{"type":"integer","minimum":1,"maximum":2000,"description":"Lines from the end of each stream. Defaults to 200."}},"required":["job_id"],"additionalProperties":false}),
            ),
            definition(
                "kill_job",
                "Request cancellation of a running background shell job or subagent.",
                json!({"type":"object","properties":{"job_id":{"type":"string"}},"required":["job_id"],"additionalProperties":false}),
            ),
            definition(
                "list_agent_recoveries",
                "List saved foreground children owned by this parent session, including completed reports. Use after an interrupted spawn_agent instead of starting a replacement. Follow next_after_id to retrieve later pages.",
                json!({"type":"object","properties":{"after_id":{"type":"string","format":"uuid"}},"additionalProperties":false}),
            ),
            definition(
                "resume_agent",
                "Resume a saved foreground child from list_agent_recoveries using its original task, approved files and worktree. Accepts no task or permission overrides. If a completed report was saved, return it without repeating tools or verification. Cannot start a new child or resume another parent's child.",
                json!({"type":"object","properties":{"agent_id":{"type":"string","format":"uuid"}},"required":["agent_id"],"additionalProperties":false}),
            ),
            definition(
                "spawn_agent",
                format!(
                    "Delegate a self-contained task to an isolated child. {} Expert requires a runtime-validated escalation ticket after smaller tiers were attempted. Children cannot spawn agents or show approval UI. Commands use static safety rules, then AI review for non-sensitive ambiguity; declined commands can be returned to the parent for exact target_command approval. Pass task with known facts, read/write files and a verifier.",
                    crate::subagent::public_trade_contract()
                ),
                json!({"type":"object","properties":{
                    "prompt":{"type":"string","description":"Free-text instruction. Still required when `task` is present; keep it to what the packet does not already say."},
                    "label":{"type":"string"},
                    "profile":{"type":"string","enum":crate::subagent::PUBLIC_SUBAGENT_IDS,"description":"The responsibility. Independent of worker_tier."},
                    "worker_tier":{"type":"string","enum":["standard","advanced","expert"],"description":"Model tier. Defaults to standard; expert requires the escalation ticket."},
                    "run_in_background":{"type":"boolean"},
                    "target_file":{"type":"string","description":"Single write target for the editor profile."},
                    "target_command":{"type":"string","description":"Exact command returned by a denied/unavailable child review. Valid only with ops_runner; the parent asks the human for one-time approval and authorizes no decorated or substituted command."},
                    "escalation":{"type":"object","description":"Required admission ticket for worker_tier=expert. The runtime cross-checks attempted_profiles against observed lower-tier work before spending the most expensive tier.","properties":{
                        "reason":{"type":"string","description":"Concrete reason the standard tier could not finish."},
                        "attempted_profiles":{"type":"array","items":{"type":"string"},"minItems":1,"description":"Lower-tier profiles already attempted in this harness."},
                        "context_evidence":{"type":"string","description":"Measured evidence that the task exceeds smaller windows or cannot be sharded."},
                        "why_not_decompose":{"type":"string","description":"Why independent worker packets cannot solve the task."}
                    },"required":["reason","attempted_profiles","context_evidence","why_not_decompose"],"additionalProperties":false},
                    "task":{"type":"object","description":"Structured task packet. Compiling one is your job, not the worker's.","properties":{
                        "goal":{"type":"string","description":"One sentence: what done looks like."},
                        "skill":{"type":"string","description":"Installed skill whose body the runtime inlines for the worker. Use for tier=worker skills instead of pasting their steps yourself."},
                        "digest_oversized":{"type":"boolean","description":"When a relevant file exceeds the worker's inline budget, digest it through the worker's own cheap model (chunked summaries, identifiers verbatim) instead of omitting it. Costs extra model calls."},
                        "read_files":{"type":"array","items":{"type":"string"},"description":"Workspace-relative context files. They are inlined but never become writable merely by being readable."},
                        "write_files":{"type":"array","items":{"type":"string"},"description":"Exact workspace-relative write allowlist for writing profiles, approved as one set. These files are also inlined when readable."},
                        "relevant_files":{"type":"array","items":{"type":"string"},"description":"Deprecated combined read/write set retained for old callers. New packets should use read_files and write_files."},
                        "known_facts":{"type":"array","items":{"type":"string"},"description":"What you already established: failing assertion text, the commit that broke it, values observed."},
                        "constraints":{"type":"array","items":{"type":"string"},"description":"What the worker must not do (public API to keep, files to leave alone)."},
                        "verifier":{"type":"object","properties":{"command":{"type":"string"},"expected_exit_code":{"type":"integer"}},"required":["command"],"additionalProperties":false,"description":"Command the runtime runs to decide done. The worker never grades itself."},
                        "max_attempts":{"type":"integer","minimum":1,"maximum":6}
                    },"required":["goal"],"additionalProperties":false}
                },"required":["prompt"],"additionalProperties":false}),
            ),
            definition(
                "ask_user",
                "Ask the user a necessary clarifying question and wait for their answer. Provide options when the valid choices are known; the user may still type another answer.",
                json!({"type":"object","properties":{"question":{"type":"string"},"options":{"type":"array","items":{"type":"string"}},"multi_select":{"type":"boolean"}},"required":["question"],"additionalProperties":false}),
            ),
            definition(
                "web_search",
                "Search the public web through the configured some.im managed search. Network access requires approval.",
                json!({"type":"object","properties":{"query":{"type":"string"},"count":{"type":"integer","minimum":1,"maximum":20}},"required":["query"],"additionalProperties":false}),
            ),
            definition(
                "web_fetch",
                "Fetch a public HTTP(S) URL and return readable text. GET is the default; POST sends `body` and always requires approval, which the user may remember for every POST to the same registrable domain. Same-host GET redirects are followed automatically, POST redirects are never followed. Private, loopback and link-local targets are refused.",
                json!({"type":"object","properties":{"url":{"type":"string"},"method":{"type":"string","enum":["GET","POST"],"description":"Defaults to GET."},"body":{"type":"string","description":"Request body; POST only."},"content_type":{"type":"string","description":"Content-Type of the POST body. Defaults to application/json."},"max_chars":{"type":"integer","minimum":1,"maximum":100000}},"required":["url"],"additionalProperties":false}),
            ),
            definition(
                "run_command",
                "Run a shell command in the workspace root and return exit code, stdout, and stderr. Requires approval.",
                json!({
                    "type": "object",
                    "properties": {
                        "command": {"type": "string", "description": "Shell command line."},
                        "timeout_seconds": {"type": "integer", "minimum": 1, "maximum": 600},
                        "label": {"type": "string", "description": "Optional concise action label; never include secrets."},
                        "run_in_background": {"type": "boolean", "description": "Return a job handle immediately and keep working. A completion notice is delivered automatically; do not poll or sleep."}
                    },
                    "required": ["command"], "additionalProperties": false
                }),
            ),
            definition(
                "create_file",
                "Create a brand-new UTF-8 file inside the workspace. Fails if the path exists. Requires approval.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Workspace-relative new file path."},
                        "content": {"type": "string", "description": "Full literal file content."}
                    },
                    "required": ["path", "content"], "additionalProperties": false
                }),
            ),
            definition(
                "edit_file",
                "Edit an existing UTF-8 file by exact string replacement. old_string must be unique unless replace_all is true. Requires approval.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Workspace-relative existing file path."},
                        "old_string": {"type": "string", "description": "Exact text currently in the file."},
                        "new_string": {"type": "string", "description": "Literal replacement text."},
                        "replace_all": {"type": "boolean", "description": "Replace every occurrence. Defaults to false."}
                    },
                    "required": ["path", "old_string", "new_string"], "additionalProperties": false
                }),
            ),
        ];
        if !self.mcp.is_empty() {
            tools.extend([
                definition(
                    "list_mcp_tools",
                    "Search connected MCP tools on demand. Returns matching names, descriptions and input schemas without charging every turn for the full catalog.",
                    json!({"type":"object","properties":{"query":{"type":"string"},"max_results":{"type":"integer","minimum":1,"maximum":20}},"additionalProperties":false}),
                ),
                definition(
                    "call_mcp_tool",
                    "Call one MCP tool found through list_mcp_tools. External side effects require approval.",
                    json!({"type":"object","properties":{"name":{"type":"string","description":"Exact namespaced name returned by list_mcp_tools, for example mcp__github__get_issue."},"arguments":{"type":"object"}},"required":["name","arguments"],"additionalProperties":false}),
                ),
            ]);
        }
        if let Some(allowed) = &self.allowed_tools {
            tools.retain(|tool| allowed.contains(&tool.name) || tool.name == "read_tool_output");
        }
        tools
    }

    pub async fn execute(&self, call: &ToolCall) -> Result<String, ToolError> {
        if self.approval_mode == ApprovalMode::ReadOnly
            && (matches!(
                call.name.as_str(),
                "run_command" | "create_file" | "edit_file" | "create_worktree" | "call_mcp_tool"
            ) || self.mcp.handles(&call.name))
        {
            return Err(ToolError::ReadOnlyPolicy(call.name.clone()));
        }
        // hook 在这里拦，是因为这是「工具即将执行」唯一的收口。放在各个工具
        // 内部就得逐个记得加，而漏掉的那个恰好会是出事的那个。
        if !self.hooks.is_empty() {
            let outcome = self
                .hooks
                .fire(
                    HookEvent::PreTool,
                    &self.hook_payload(HookEvent::PreTool, call),
                )
                .await;
            if let Some(message) = outcome.message() {
                return Err(ToolError::HookDenied(message));
            }
        }
        let result = self.dispatch(call).await;
        if !self.hooks.is_empty() {
            let payload = self
                .hook_payload(HookEvent::PostTool, call)
                .with_outcome(if result.is_ok() { "ok" } else { "error" });
            // 事后 hook 拦不住已经发生的事，结果丢弃。
            let _ = self.hooks.fire(HookEvent::PostTool, &payload).await;
        }
        result
    }

    async fn dispatch(&self, call: &ToolCall) -> Result<String, ToolError> {
        match call.name.as_str() {
            "read_tool_output" => {
                let args: OutputPageArgs = parse(call)?;
                Ok(self.output_store.page(
                    &args.id,
                    args.offset.unwrap_or(0),
                    args.limit.unwrap_or(8000),
                )?)
            }
            "list_skills" => self.list_skills(parse(call)?),
            "read_skill" => self.read_skill(parse(call)?),
            "list_mcp_tools" => {
                let args: ListMcpToolsArgs = parse(call)?;
                Ok(self
                    .mcp
                    .search(args.query.as_deref(), args.max_results.unwrap_or(10)))
            }
            "call_mcp_tool" => {
                let args: CallMcpToolArgs = parse(call)?;
                if !self.mcp.handles(&args.name) {
                    return Err(ToolError::UnknownTool(args.name));
                }
                self.require_rememberable_approval(
                    &format!("call MCP tool: {}", args.name),
                    format!("mcp:{}", args.name),
                )
                .await?;
                Ok(self.mcp.call(&args.name, args.arguments).await?)
            }
            "search_files" => self.search_files(parse(call)?),
            "grep_files" => self.grep_files(parse(call)?),
            "read_file" => self.read_file(parse(call)?).await,
            "list_directory" => self.list_directory(parse(call)?).await,
            "git_status" => self.git_status().await,
            "git_diff" => self.git_diff(parse(call)?).await,
            "git_log" => self.git_log(parse(call)?).await,
            "git_blame" => self.git_blame(parse(call)?).await,
            "list_worktrees" => self.list_worktrees().await,
            "create_worktree" => self.create_worktree(parse(call)?).await,
            "get_job_output" => self.get_job_output(parse(call)?),
            "kill_job" => self.kill_job(parse(call)?).await,
            "ask_user" => self.ask_user(parse(call)?).await,
            "web_search" => self.web_search(parse(call)?).await,
            "web_fetch" => self.web_fetch(parse(call)?).await,
            "run_command" => self.run_command(parse(call)?).await,
            "create_file" => self.create_file(parse(call)?).await,
            "edit_file" => self.edit_file(parse(call)?).await,
            name if self.mcp.handles(name) => {
                self.require_rememberable_approval(
                    &format!("call MCP tool: {name}"),
                    format!("mcp:{name}"),
                )
                .await?;
                let arguments =
                    call.parsed_arguments()
                        .map_err(|source| ToolError::InvalidArguments {
                            tool: call.name.clone(),
                            source,
                        })?;
                Ok(self.mcp.call(name, arguments).await?)
            }
            name => Err(ToolError::UnknownTool(name.to_owned())),
        }
    }

    fn list_skills(&self, args: ListSkillsArgs) -> Result<String, ToolError> {
        let query = args.query.unwrap_or_default().to_ascii_lowercase();
        let lines = self
            .skills
            .list()
            .iter()
            .filter(|s| {
                query.is_empty()
                    || format!("{} {} {}", s.identifier, s.name, s.description)
                        .to_ascii_lowercase()
                        .contains(&query)
            })
            .map(|s| match s.tier {
                Some(tier) => format!(
                    "- {} | name={} | tier={} | {}",
                    s.identifier,
                    s.name,
                    tier.as_str(),
                    s.description
                ),
                None => format!("- {} | name={} | {}", s.identifier, s.name, s.description),
            })
            .collect::<Vec<_>>();
        let mut result = if lines.is_empty() {
            "No installed skills found.".to_owned()
        } else {
            lines.join("\n")
        };
        // Deterministic dispatch trigger, same philosophy as the failed-test
        // hint: visibility must not depend on the model remembering the
        // routing rule. When the listing surfaces worker-tier skills, the
        // recipe rides along — and only for the main agent, because a child
        // cannot spawn and a hint it cannot act on is just noise.
        if self.delegation_hints {
            let workers = self
                .skills
                .list()
                .iter()
                .filter(|s| {
                    s.tier == Some(crate::skills::SkillTier::Worker)
                        && (query.is_empty()
                            || format!("{} {} {}", s.identifier, s.name, s.description)
                                .to_ascii_lowercase()
                                .contains(&query))
                })
                .map(|s| s.identifier.clone())
                .collect::<Vec<_>>();
            if !workers.is_empty() {
                result.push_str(&format!(
                    "\n\n<delegation-hint tier=\"worker\">\nThese skills fit a small-context worker: {}. Instead of running one inline, spawn_agent with a task packet — set task.skill to the skill name (the runtime inlines its body for the worker), task.goal to the outcome, task.read_files to context inputs, task.write_files to the exact write allowlist, and task.verifier.command when the skill names a check. Your window stays free and the run gets a real verdict.\n</delegation-hint>",
                    workers.join(", ")
                ));
            }
        }
        Ok(result)
    }
    fn read_skill(&self, args: ReadSkillArgs) -> Result<String, ToolError> {
        Ok(self.skills.read(&args.name, args.resource.as_deref())?)
    }

    fn search_files(&self, args: SearchArgs) -> Result<String, ToolError> {
        if args.query.is_empty() {
            return Ok("query is empty".to_owned());
        }
        if let Some(output) =
            self.search_with_rg(&args.query, true, true, None, None, args.max_results)
        {
            return Ok(output);
        }
        let query = args.query.to_lowercase();
        self.search(None, None, args.max_results, |line| {
            line.to_lowercase().contains(&query)
        })
    }

    fn grep_files(&self, args: GrepArgs) -> Result<String, ToolError> {
        let regex = RegexBuilder::new(&args.pattern)
            .case_insensitive(!args.case_sensitive.unwrap_or(false))
            .build()
            .map_err(|error| ToolError::InvalidRegex(error.to_string()))?;
        let include = args
            .include
            .as_deref()
            .map(|pattern| {
                Glob::new(pattern)
                    .map(|glob| glob.compile_matcher())
                    .map_err(|error| ToolError::InvalidGlob(error.to_string()))
            })
            .transpose()?;
        if let Some(output) = self.search_with_rg(
            &args.pattern,
            false,
            !args.case_sensitive.unwrap_or(false),
            args.path.as_deref(),
            args.include.as_deref(),
            args.max_results,
        ) {
            return Ok(output);
        }
        self.search(
            args.path.as_deref(),
            include.as_ref(),
            args.max_results,
            |line| regex.is_match(line),
        )
    }

    fn search_with_rg(
        &self,
        pattern: &str,
        fixed_strings: bool,
        ignore_case: bool,
        relative_path: Option<&str>,
        include: Option<&str>,
        max_results: Option<usize>,
    ) -> Option<String> {
        let search_path = match relative_path {
            Some(path) if !path.is_empty() => {
                self.resolve_existing(path).ok()?;
                path
            }
            _ => ".",
        };
        let limit = max_results
            .unwrap_or(DEFAULT_MAX_RESULTS)
            .clamp(1, MAX_RESULTS);
        let mut command = std::process::Command::new("rg");
        command.current_dir(&self.workspace).args([
            "--line-number",
            "--no-heading",
            "--color",
            "never",
        ]);
        if fixed_strings {
            command.arg("--fixed-strings");
        }
        if ignore_case {
            command.arg("--ignore-case");
        }
        if let Some(glob) = include {
            command.args(["--glob", glob]);
        }
        let output = command.args(["--", pattern, search_path]).output().ok()?;
        if !output.status.success() && output.status.code() != Some(1) {
            return None;
        }
        let mut lines = String::from_utf8_lossy(&output.stdout)
            .lines()
            .take(limit)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if lines.len() == limit && String::from_utf8_lossy(&output.stdout).lines().count() > limit {
            lines.push(format!("... truncated at {limit} results"));
        }
        Some(if lines.is_empty() {
            "No matches found.".to_owned()
        } else {
            lines.join("\n")
        })
    }

    fn search<F>(
        &self,
        relative_path: Option<&str>,
        include: Option<&globset::GlobMatcher>,
        max_results: Option<usize>,
        matches: F,
    ) -> Result<String, ToolError>
    where
        F: Fn(&str) -> bool,
    {
        let root = match relative_path {
            Some(path) if !path.is_empty() => self.resolve_existing(path)?,
            _ => self.workspace.clone(),
        };
        let limit = max_results
            .unwrap_or(DEFAULT_MAX_RESULTS)
            .clamp(1, MAX_RESULTS);
        let mut output = Vec::new();
        for entry in WalkBuilder::new(root).standard_filters(true).build() {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if include
                .is_some_and(|matcher| !matcher.is_match(path.file_name().unwrap_or_default()))
            {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(path) else {
                continue;
            };
            for (index, line) in content.lines().enumerate() {
                if matches(line) {
                    output.push(format!(
                        "{}:{}:{}",
                        display_relative(&self.workspace, path),
                        index + 1,
                        truncate_line(line, 500)
                    ));
                    if output.len() >= limit {
                        return Ok(format!(
                            "{}\n[limited to {limit} matches]",
                            output.join("\n")
                        ));
                    }
                }
            }
        }
        Ok(if output.is_empty() {
            "no matches".to_owned()
        } else {
            output.join("\n")
        })
    }

    async fn read_file(&self, args: ReadArgs) -> Result<String, ToolError> {
        let path = self.resolve_existing(&args.path)?;
        let content = tokio::fs::read_to_string(path).await?;
        let offset = args.offset.unwrap_or(1).max(1);
        let limit = args.limit.unwrap_or(usize::MAX);
        let max_bytes = args
            .max_bytes
            .unwrap_or(DEFAULT_READ_BYTES)
            .clamp(1, self.read_bytes_limit());
        let mut output = String::new();
        let mut next_line = None;
        for (index, line) in content.lines().enumerate().skip(offset - 1).take(limit) {
            let row = format!("{:>6}  {}\n", index + 1, line);
            if output.len() + row.len() > max_bytes {
                next_line = Some(index + 1);
                break;
            }
            output.push_str(&row);
        }
        if let Some(line) = next_line {
            output.push_str(&format!("[truncated; continue with offset {line}]\n"));
        }
        Ok(output)
    }

    async fn list_directory(&self, args: ListDirectoryArgs) -> Result<String, ToolError> {
        let path = self.resolve_existing(args.path.as_deref().unwrap_or("."))?;
        let mut reader = tokio::fs::read_dir(path).await?;
        let mut entries = Vec::new();
        while let Some(entry) = reader.next_entry().await? {
            let kind = if entry.file_type().await?.is_dir() {
                "directory"
            } else {
                "file"
            };
            entries.push(format!("{kind}\t{}", entry.file_name().to_string_lossy()));
        }
        entries.sort();
        Ok(entries.join("\n"))
    }

    async fn git_status(&self) -> Result<String, ToolError> {
        let branch = Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(&self.workspace)
            .output()
            .await?;
        let status = Command::new("git")
            .args(["status", "--short"])
            .current_dir(&self.workspace)
            .output()
            .await?;
        Ok(format!(
            "branch: {}\nstatus:\n{}",
            String::from_utf8_lossy(&branch.stdout).trim(),
            String::from_utf8_lossy(&status.stdout)
        ))
    }

    async fn git_diff(&self, args: GitDiffArgs) -> Result<String, ToolError> {
        let mut command = Command::new("git");
        command.arg("diff");
        if args.staged.unwrap_or(false) {
            command.arg("--cached");
        }
        if args.stat_only.unwrap_or(false) {
            command.arg("--stat");
        }
        if let Some(path) = args.path.as_deref() {
            self.resolve_existing(path)?;
            command.args(["--", path]);
        }
        let output = command.current_dir(&self.workspace).output().await?;
        if !output.status.success() {
            return Err(ToolError::Io(std::io::Error::other(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            )));
        }
        Ok(truncate_bytes(
            String::from_utf8_lossy(&output.stdout).into_owned(),
            self.command_output_limit(),
        ))
    }

    async fn git_log(&self, args: GitLogArgs) -> Result<String, ToolError> {
        let mut command = Command::new("git");
        command.args([
            "log",
            "--no-color",
            "--date=iso-strict",
            "--format=%H%x09%an%x09%aI%x09%s",
        ]);
        command.arg(format!(
            "--max-count={}",
            args.max_count.unwrap_or(20).clamp(1, 100)
        ));
        if let Some(author) = args
            .author
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            command.arg(format!("--author={author}"));
        }
        if let Some(since) = args
            .since
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            command.arg(format!("--since={since}"));
        }
        if let Some(path) = args.path.as_deref() {
            self.resolve_existing(path)?;
            command.args(["--", path]);
        }
        let output = command.current_dir(&self.workspace).output().await?;
        git_output(output, self.command_output_limit())
    }

    async fn git_blame(&self, args: GitBlameArgs) -> Result<String, ToolError> {
        self.resolve_existing(&args.path)?;
        let start = args.start_line.unwrap_or(1);
        let end = args.end_line;
        if start == 0 || end.is_some_and(|end| end < start || end.saturating_sub(start) >= 2_000) {
            return Err(ToolError::Io(std::io::Error::other(
                "git_blame lines must be a 1-based ordered range of at most 2000 lines",
            )));
        }
        let range = end.map_or_else(|| format!("{start},+200"), |end| format!("{start},{end}"));
        let output = Command::new("git")
            .args([
                "-c",
                "color.ui=false",
                "blame",
                "--date=iso-strict",
                "-L",
                &range,
                "--",
                &args.path,
            ])
            .current_dir(&self.workspace)
            .output()
            .await?;
        git_output(output, self.command_output_limit())
    }

    async fn list_worktrees(&self) -> Result<String, ToolError> {
        let output = Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(&self.workspace)
            .output()
            .await?;
        if !output.status.success() {
            return Err(ToolError::Io(std::io::Error::other(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    async fn create_worktree(&self, args: CreateWorktreeArgs) -> Result<String, ToolError> {
        let branch = sanitize_branch(&args.branch)?;
        self.require_approval(
            &format!("create Git worktree for new branch: {branch}"),
            false,
        )
        .await?;
        let repository = Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(&self.workspace)
            .output()
            .await?;
        if !repository.status.success() {
            return Err(ToolError::Io(std::io::Error::other(
                "workspace is not a Git repository",
            )));
        }
        let repository = PathBuf::from(String::from_utf8_lossy(&repository.stdout).trim());
        let home = std::env::var_os("WILLDEEP_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|value| PathBuf::from(value).join(".willdeep"))
            })
            .or_else(|| {
                std::env::var_os("USERPROFILE").map(|value| PathBuf::from(value).join(".willdeep"))
            })
            .ok_or_else(|| ToolError::Io(std::io::Error::other("cannot locate WillDeep home")))?;
        let repo_name = repository
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("repo");
        let target = home.join("worktrees").join(repo_name).join(&branch);
        tokio::fs::create_dir_all(target.parent().expect("worktree parent")).await?;
        let output = Command::new("git")
            .args(["worktree", "add", "-b", &branch])
            .arg(&target)
            .current_dir(&repository)
            .output()
            .await?;
        if !output.status.success() {
            return Err(ToolError::Io(std::io::Error::other(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            )));
        }
        Ok(format!(
            "created worktree {} on branch {branch}",
            target.display()
        ))
    }

    async fn web_search(&self, args: WebSearchArgs) -> Result<String, ToolError> {
        let config = self.web.as_ref().ok_or_else(|| {
            ToolError::Network("web_search requires a some.im provider".to_owned())
        })?;
        let query = args.query.trim();
        if query.is_empty() {
            return Err(ToolError::Network("search query is empty".to_owned()));
        }
        self.require_network_read_approval(&format!("search the public web for: {query}"))
            .await?;
        let mut endpoint = reqwest::Url::parse(&config.some_im_base_url)
            .map_err(|error| ToolError::Network(format!("invalid some.im API base: {error}")))?;
        endpoint.set_path("/api/v1/customer/web-search");
        endpoint.set_query(None);
        let response = web_client()?
            .post(endpoint)
            .bearer_auth(&config.api_key)
            .json(&json!({
                "query": query,
                "count": args.count.unwrap_or(8).clamp(1, 20),
                "provider": "auto"
            }))
            .send()
            .await
            .map_err(|error| ToolError::Network(error.to_string()))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| ToolError::Network(error.to_string()))?;
        if !status.is_success() {
            return Err(ToolError::Network(format!(
                "some.im web search returned HTTP {status}: {}",
                truncate_line(&body, 500)
            )));
        }
        format_search_results(&body)
    }

    async fn web_fetch(&self, args: WebFetchArgs) -> Result<String, ToolError> {
        let method = parse_web_method(args.method.as_deref())?;
        let url = reqwest::Url::parse(args.url.trim())
            .map_err(|error| ToolError::Network(format!("invalid URL: {error}")))?;
        validate_public_url(&url).await?;
        let response = match method {
            WebMethod::Get => {
                if args.body.is_some() {
                    return Err(ToolError::Network(
                        "a request body requires method \"POST\"".to_owned(),
                    ));
                }
                self.require_network_read_approval(&format!("fetch public URL: {url}"))
                    .await?;
                self.web_get(url).await?
            }
            WebMethod::Post => {
                let body = args.body.unwrap_or_default();
                if body.len() > MAX_WEB_REQUEST_BYTES {
                    return Err(ToolError::Network(format!(
                        "request body exceeds the {} KiB limit",
                        MAX_WEB_REQUEST_BYTES / 1024
                    )));
                }
                let content_type = args
                    .content_type
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or(DEFAULT_WEB_POST_CONTENT_TYPE)
                    .to_owned();
                self.require_web_post_approval(&url, body.len(), &content_type)
                    .await?;
                self.web_post(url, body, &content_type).await?
            }
        };
        if !response.status().is_success() {
            let status = response.status();
            if method == WebMethod::Post {
                // POST 的失败正文往往就是服务端给的错误说明，吞掉它等于让模型
                // 对着一个裸状态码猜。
                let body = read_web_response(response).await.unwrap_or_default();
                return Err(ToolError::Network(format!(
                    "web server returned HTTP {status}: {}",
                    truncate_line(&String::from_utf8_lossy(&body), 500)
                )));
            }
            return Err(ToolError::Network(format!(
                "web server returned HTTP {status}"
            )));
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_WEB_RESPONSE_BYTES as u64)
        {
            return Err(ToolError::Network(
                "response exceeds the 3 MiB limit".to_owned(),
            ));
        }
        let is_html = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.to_ascii_lowercase().contains("html"));
        let bytes = read_web_response(response).await?;
        let raw = String::from_utf8_lossy(&bytes);
        let text = if is_html {
            html_to_text(&raw)
        } else {
            raw.into_owned()
        };
        let limit = args
            .max_chars
            .unwrap_or(DEFAULT_WEB_MAX_CHARS)
            .clamp(1, MAX_WEB_MAX_CHARS);
        Ok(truncate_chars(&text, limit))
    }

    async fn web_get(&self, mut url: reqwest::Url) -> Result<reqwest::Response, ToolError> {
        let client = web_client()?;
        let mut redirects = 0;
        let mut visited = HashSet::new();
        loop {
            if !visited.insert(redirect_key(&url)) {
                return Err(ToolError::Network("redirect loop detected".to_owned()));
            }
            let response = client
                .get(url.clone())
                .send()
                .await
                .map_err(|error| ToolError::Network(error.to_string()))?;
            if !response.status().is_redirection() {
                return Ok(response);
            }
            if redirects >= MAX_WEB_REDIRECTS {
                return Err(ToolError::Network(format!(
                    "redirect limit exceeded ({MAX_WEB_REDIRECTS})"
                )));
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| {
                    ToolError::Network("redirect response has no valid Location header".to_owned())
                })?;
            let next = url
                .join(location)
                .map_err(|error| ToolError::Network(format!("invalid redirect target: {error}")))?;
            validate_public_url(&next).await?;
            if url.scheme() == "https" && next.scheme() == "http" {
                return Err(ToolError::Network(
                    "HTTPS to HTTP redirect downgrade is refused".to_owned(),
                ));
            }
            if !same_hostname(&url, &next) {
                self.require_network_read_approval(&format!(
                    "redirect web_fetch from {url} to different host: {next}"
                ))
                .await?;
            }
            url = next;
            redirects += 1;
        }
    }

    /// POST 不跟随重定向。用户批准的是「向这个地址写」，而重定向后的目标是
    /// 另一个端点、可能还是另一个域名，跟着跳就等于拿旧批准去写新地方。把状
    /// 态码和 Location 原样交回去，让模型拿新地址重新申请一次。
    async fn web_post(
        &self,
        url: reqwest::Url,
        body: String,
        content_type: &str,
    ) -> Result<reqwest::Response, ToolError> {
        let content_type = reqwest::header::HeaderValue::from_str(content_type)
            .map_err(|error| ToolError::Network(format!("invalid content type: {error}")))?;
        let response = web_client()?
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(body)
            .send()
            .await
            .map_err(|error| ToolError::Network(error.to_string()))?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("(no Location header)");
            return Err(ToolError::Network(format!(
                "POST redirects are not followed (HTTP {} to {location}); re-issue the request against the final URL",
                response.status()
            )));
        }
        Ok(response)
    }

    async fn run_command(&self, args: CommandArgs) -> Result<String, ToolError> {
        let description = args
            .label
            .as_deref()
            .filter(|label| !label.trim().is_empty())
            .map(|label| format!("{label}\ncommand: {}", args.command))
            .unwrap_or_else(|| args.command.clone());
        self.gate_command(&args.command, &description).await?;
        let timeout = args
            .timeout_seconds
            .unwrap_or(DEFAULT_COMMAND_TIMEOUT_SECS)
            .clamp(1, MAX_COMMAND_TIMEOUT_SECS);
        if args.run_in_background.unwrap_or(false) {
            // 脱离模式：进程自成进程组，输出与退出码落盘。父进程升级或退出
            // 都不影响它，回来只取结果，不重跑。
            if let Some(jobs) = self.detached_jobs.clone() {
                let job = jobs
                    .spawn_with_policy(
                        &args.command,
                        &description,
                        &self.workspace,
                        &self.sandbox,
                        timeout,
                    )
                    .map_err(ToolError::Io)?;
                return Ok(format!(
                    "Background job started: {} (pid {}). It survives a Runtime restart; read it with get_job_output.",
                    job.id, job.pid
                ));
            }
            let command = args.command;
            let workspace = self.workspace.clone();
            let sandbox = self.sandbox.clone();
            let verification_reporter = self.verification_reporter.clone();
            let verification_snapshot = self.verification_snapshot.clone();
            let id = self.background.start_retriable(
                BackgroundTaskKind::Shell,
                description,
                move || {
                    let command = command.clone();
                    let workspace = workspace.clone();
                    let sandbox = sandbox.clone();
                    let verification_reporter = verification_reporter.clone();
                    let verification_snapshot = verification_snapshot.clone();
                    async move {
                        let snapshot_id =
                            capture_verification_snapshot(verification_snapshot.as_ref(), &command);
                        let mut result =
                            run_background_shell(command.clone(), workspace, timeout, sandbox)
                                .await;
                        let status = verification::finish_verification(
                            verification_snapshot.as_ref(),
                            &command,
                            &snapshot_id,
                            verification_status(&result.status),
                            &mut result.output,
                        );
                        report_verification(
                            verification_reporter.as_ref(),
                            &command,
                            result.exit_code,
                            status,
                            &result.output,
                            snapshot_id,
                        );
                        result
                    }
                },
            );
            return Ok(format!(
                "Background task started: {id}. Completion will be delivered automatically; use get_job_output for details."
            ));
        }
        let snapshot_id =
            capture_verification_snapshot(self.verification_snapshot.as_ref(), &args.command);
        let output = match crate::execution::run_capture(
            &args.command,
            &self.workspace,
            &self.sandbox,
            std::time::Duration::from_secs(timeout),
            self.command_output_limit(),
        )
        .await
        {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                report_verification(
                    self.verification_reporter.as_ref(),
                    &args.command,
                    None,
                    VerificationStatus::TimedOut,
                    &error.to_string(),
                    snapshot_id,
                );
                return Err(ToolError::CommandTimeout {
                    seconds: timeout,
                    output: error.to_string(),
                });
            }
            Err(error) => {
                report_verification(
                    self.verification_reporter.as_ref(),
                    &args.command,
                    None,
                    VerificationStatus::Failed,
                    &error.to_string(),
                    snapshot_id,
                );
                return Err(error.into());
            }
        };
        let mut text = format!(
            "exit_code: {}\nstdout:\n{}\nstderr:\n{}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let status = verification::finish_verification(
            self.verification_snapshot.as_ref(),
            &args.command,
            &snapshot_id,
            if output.status.success() {
                VerificationStatus::Passed
            } else {
                VerificationStatus::Failed
            },
            &mut text,
        );
        report_verification(
            self.verification_reporter.as_ref(),
            &args.command,
            output.status.code(),
            status,
            &text,
            snapshot_id,
        );
        let mut text = truncate_bytes(text, self.command_output_limit());
        // 把「命令自己错了」和「命令被围栏拦了」分开说。不分开的话，用户看到的
        // 是一句 `Operation not permitted`，然后花二十分钟怀疑自己的代码。
        if self.sandbox.policy.is_enforcing()
            && !output.status.success()
            && crate::sandbox::looks_like_denial(&text)
        {
            text.push_str(&sandbox_denial_hint(&self.sandbox));
        }
        if self.delegation_hints
            && !output.status.success()
            && let Some(profile) = delegable_failure_profile(&args.command)
        {
            text.push_str(&delegation_hint(profile, &args.command));
        }
        Ok(text)
    }

    async fn create_file(&self, args: CreateArgs) -> Result<String, ToolError> {
        self.require_write_target(&args.path, true)?;
        self.require_approval(&format!("create file: {}", args.path), true)
            .await?;
        let path = self.resolve_new(&args.path)?;
        if path.exists() {
            return Err(ToolError::FileAlreadyExists(args.path));
        }
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut options = tokio::fs::OpenOptions::new();
        let mut file = options.write(true).create_new(true).open(&path).await?;
        file.write_all(args.content.as_bytes()).await?;
        file.flush().await?;
        Ok(format!(
            "created {} ({} bytes)",
            args.path,
            args.content.len()
        ))
    }

    async fn edit_file(&self, args: EditArgs) -> Result<String, ToolError> {
        self.require_write_target(&args.path, false)?;
        if args.old_string == args.new_string {
            return Err(ToolError::IdenticalEdit);
        }
        self.require_approval(&format!("edit file: {}", args.path), true)
            .await?;
        let path = self.resolve_existing(&args.path)?;
        let content = tokio::fs::read_to_string(&path).await?;
        let count = content.matches(&args.old_string).count();
        if count == 0 {
            return Err(ToolError::EditTextNotFound(args.path));
        }
        if count > 1 && !args.replace_all.unwrap_or(false) {
            return Err(ToolError::EditTextNotUnique {
                path: args.path,
                count,
            });
        }
        let updated = if args.replace_all.unwrap_or(false) {
            content.replace(&args.old_string, &args.new_string)
        } else {
            content.replacen(&args.old_string, &args.new_string, 1)
        };
        atomic_write(&path, updated.as_bytes()).await?;
        Ok(format!("edited {} ({count} replacement(s))", args.path))
    }

    /// Two-tier approval gate for shell commands.
    ///
    /// `Strict` asks about everything, as advertised. `ReadOnly` never gets
    /// here (write tools are refused earlier and commands are gated the same
    /// as `Strict`). `Smart` and `WorkspaceAccess` run the static classifier
    /// first — read-only and bounded commands just run — then consult the AI
    /// judge for the ambiguous middle, and only escalate to the user when
    /// both tiers decline.
    async fn gate_command(&self, command: &str, description: &str) -> Result<(), ToolError> {
        let trimmed = command.trim();
        let escalate = |registry: &Self, detail: String| {
            registry.report_approval(command, ApprovalSource::User, detail);
        };
        if self.preapproved_commands.contains(trimmed) {
            self.report_approval(
                command,
                ApprovalSource::User,
                "parent relayed one-time human approval for this exact command".to_owned(),
            );
            return Ok(());
        }
        // A worker with an allowlist runs its verifier and nothing else. This
        // gate is first because it is the narrowest: no approval mode, static
        // rule or judge verdict can widen a worker past the exact command its
        // dispatcher declared.
        if self.read_only_git_shell {
            let is_git = trimmed == "git" || trimmed.starts_with("git ");
            if !is_git || crate::safety::classify(trimmed) != CommandSafety::AlwaysSafe {
                return Err(ToolError::ApprovalDenied(format!(
                    "this subagent may only run read-only git commands, not: {command}"
                )));
            }
            return Ok(());
        }
        if let Some(allowed) = &self.command_allowlist {
            if allowed.contains(trimmed) {
                return Ok(());
            }
            // Name the command it *may* run. The live-fire range showed the
            // typical near-miss is a decorated verifier — `cargo build 2>&1`
            // instead of `cargo build` — and a refusal that only says "not
            // that" invites the worker to guess again, one turn per guess.
            let allowed_list = {
                let mut allowed = allowed.iter().cloned().collect::<Vec<_>>();
                allowed.sort();
                allowed.join(", ")
            };
            return Err(ToolError::ApprovalDenied(format!(
                "this subagent may only run its declared verifier command verbatim ({allowed_list}), not: {command}"
            )));
        }
        if self.reviewed_subagent_shell {
            if child_command_is_sensitive(trimmed) {
                return Err(reviewed_subagent_denial(
                    command,
                    "credential-sensitive command; AI review was bypassed",
                ));
            }
            match crate::safety::classify(trimmed) {
                CommandSafety::AlwaysSafe => {
                    self.report_approval(
                        command,
                        ApprovalSource::StaticAllowlist,
                        "subagent static rule: read-only or bounded command".to_owned(),
                    );
                    return Ok(());
                }
                CommandSafety::AlwaysDangerous => {
                    return Err(reviewed_subagent_denial(
                        command,
                        "destructive command shape; AI review was bypassed",
                    ));
                }
                CommandSafety::NeedsJudgment => {}
            }
            let Some(judge) = &self.safety_judge else {
                return Err(reviewed_subagent_denial(
                    command,
                    "no AI safety judge is configured",
                ));
            };
            let task_context = self.task_context.lock().expect("task context").clone();
            let verdict = judge
                .judge(JudgeRequest {
                    tool: "subagent_run_command".to_owned(),
                    command: command.to_owned(),
                    task_context,
                })
                .await;
            return match verdict {
                JudgeVerdict::Allow => {
                    self.report_approval(
                        command,
                        ApprovalSource::Judge,
                        format!(
                            "subagent AI review ({}): bounded and consistent with the delegated task",
                            judge.model()
                        ),
                    );
                    Ok(())
                }
                JudgeVerdict::Deny => Err(reviewed_subagent_denial(
                    command,
                    &format!("AI safety judge ({}) declined", judge.model()),
                )),
                JudgeVerdict::Unavailable(reason) => Err(reviewed_subagent_denial(
                    command,
                    &format!("AI safety judge ({}) unavailable: {reason}", judge.model()),
                )),
            };
        }
        if matches!(
            self.approval_mode,
            ApprovalMode::Strict | ApprovalMode::ReadOnly
        ) {
            escalate(self, "strict approval mode".to_owned());
            return self.ask_for_command(command, description).await;
        }
        if let Some(signature) = command_signature(command)
            && self
                .always_allowed
                .lock()
                .expect("always allow rules")
                .contains(&signature)
        {
            self.report_approval(
                command,
                ApprovalSource::AlwaysAllowList,
                "operator marked this exact command always-allowed".to_owned(),
            );
            return Ok(());
        }

        let allow_workspace_create = self.approval_mode != ApprovalMode::ReadOnly;
        match crate::safety::classify_with_workspace_write(command, allow_workspace_create) {
            CommandSafety::AlwaysSafe => {
                self.report_approval(
                    command,
                    ApprovalSource::StaticAllowlist,
                    "static rule: read-only or bounded workspace command".to_owned(),
                );
                return Ok(());
            }
            CommandSafety::AlwaysDangerous => {
                // Destructive shapes never reach the judge — a model must not
                // be able to talk its way into `rm -rf`.
                escalate(
                    self,
                    "static rule: destructive shape, judge bypassed".to_owned(),
                );
                return self.ask_for_command(command, description).await;
            }
            CommandSafety::NeedsJudgment => {}
        }

        let Some(judge) = &self.safety_judge else {
            escalate(self, "no AI judge configured".to_owned());
            return self.ask_for_command(command, description).await;
        };
        let task_context = self.task_context.lock().expect("task context").clone();
        let verdict = judge
            .judge(JudgeRequest {
                tool: "run_command".to_owned(),
                command: command.to_owned(),
                task_context,
            })
            .await;
        // The judge model goes into every trace, not just the failures: an
        // operator comparing "why does the CLI ask more than the app" needs to
        // see which model answered, and a silent model swap is otherwise
        // invisible in the audit trail.
        let model = judge.model();
        match verdict {
            JudgeVerdict::Allow => {
                self.report_approval(
                    command,
                    ApprovalSource::Judge,
                    format!("AI review ({model}): bounded and consistent with the current task"),
                );
                Ok(())
            }
            JudgeVerdict::Deny => {
                escalate(self, format!("AI review ({model}) declined"));
                self.ask_for_command(command, description).await
            }
            JudgeVerdict::Unavailable(reason) => {
                escalate(self, format!("AI review ({model}) unavailable: {reason}"));
                self.ask_for_command(command, description).await
            }
        }
    }

    async fn ask_for_command(&self, command: &str, description: &str) -> Result<(), ToolError> {
        match command_signature(command) {
            Some(signature) => {
                self.require_rememberable_approval(
                    &format!("run command: {description}"),
                    signature,
                )
                .await
            }
            None => {
                self.require_approval(&format!("run command: {description}"), false)
                    .await
            }
        }
    }

    fn report_approval(&self, command: &str, source: ApprovalSource, detail: String) {
        let Some(reporter) = &self.approval_reporter else {
            return;
        };
        reporter(ApprovalTrace {
            command: command.to_owned(),
            source,
            detail,
        });
    }

    /// POST 是对外写操作，所有审批模式都要过一遍，`read-only` 策略直接拒。
    ///
    /// 「始终允许」按注册域名收敛，而不是像 shell 命令那样逐字记：POST 的 URL
    /// 常带一次性 id、body 每次都不同，逐字规则下一次就对不上，等于没有。规则
    /// 里只有域名，body 中的密钥不会被写进 always-allow.json。
    async fn require_web_post_approval(
        &self,
        url: &reqwest::Url,
        body_bytes: usize,
        content_type: &str,
    ) -> Result<(), ToolError> {
        if self.approval_mode == ApprovalMode::ReadOnly {
            return Err(ToolError::ReadOnlyPolicy(format!("POST to {url}")));
        }
        let domain = registrable_domain(url);
        let description = format!(
            "POST to {url}\nbody: {body_bytes} B ({content_type})\nAlways allow scope: every POST to {domain}"
        );
        self.require_rememberable_approval(
            &description,
            format!("{WEB_POST_SIGNATURE_PREFIX}{domain}"),
        )
        .await
    }

    /// 只读的公网抓取（web_fetch / web_search）不改动本地状态，SSRF 目标已被
    /// `validate_public_url` 拦下，所以只有 Strict 模式才逐次询问。
    async fn require_network_read_approval(&self, description: &str) -> Result<(), ToolError> {
        if self.approval_mode != ApprovalMode::Strict {
            return Ok(());
        }
        self.require_approval(description, false).await
    }

    async fn require_approval(
        &self,
        description: &str,
        workspace_write: bool,
    ) -> Result<(), ToolError> {
        let workspace_write_allowed = workspace_write
            && matches!(
                self.approval_mode,
                ApprovalMode::Smart | ApprovalMode::WorkspaceAccess
            );
        if workspace_write_allowed {
            return Ok(());
        }
        match self.approver.approve(description, false).await {
            ApprovalDecision::AllowOnce | ApprovalDecision::AlwaysAllow => Ok(()),
            ApprovalDecision::Deny => Err(ToolError::ApprovalDenied(description.to_owned())),
        }
    }

    pub(crate) async fn approve_uncertain_replay(&self, call: &ToolCall) -> Result<(), ToolError> {
        let description = format!(
            "Retry {} with the same arguments as an interrupted call whose effects are unknown? This may repeat an external or file side effect. Approval applies only to this attempt.",
            call.name
        );
        match self.approver.approve(&description, false).await {
            ApprovalDecision::AllowOnce => Ok(()),
            _ => Err(ToolError::ApprovalDenied(
                "uncertain replay requires one-time approval".into(),
            )),
        }
    }

    async fn require_rememberable_approval(
        &self,
        description: &str,
        signature: String,
    ) -> Result<(), ToolError> {
        if self
            .always_allowed
            .lock()
            .expect("always allow rules")
            .contains(&signature)
        {
            return Ok(());
        }
        match self.approver.approve(description, true).await {
            ApprovalDecision::AllowOnce => Ok(()),
            ApprovalDecision::AlwaysAllow => {
                self.always_allowed
                    .lock()
                    .expect("always allow rules")
                    .insert(signature);
                self.persist_always_allowed()?;
                Ok(())
            }
            ApprovalDecision::Deny => Err(ToolError::ApprovalDenied(description.to_owned())),
        }
    }

    fn persist_always_allowed(&self) -> Result<(), ToolError> {
        let Some(path) = &self.always_allow_path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut rules = self
            .always_allowed
            .lock()
            .expect("always allow rules")
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        rules.sort();
        let bytes = serde_json::to_vec_pretty(&rules)
            .map_err(|error| ToolError::Network(error.to_string()))?;
        let mut options = std::fs::OpenOptions::new();
        options.create(true).write(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        let mut file = options.open(path)?;
        file.write_all(&bytes)?;
        file.flush()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    async fn ask_user(&self, args: AskUserArgs) -> Result<String, ToolError> {
        let question = args.question.trim();
        if question.is_empty() {
            return Err(ToolError::Network("ask_user question is empty".to_owned()));
        }
        let mut options = args
            .options
            .unwrap_or_default()
            .into_iter()
            .map(|value| truncate_line(value.trim(), 500))
            .filter(|value| !value.is_empty())
            .take(12)
            .collect::<Vec<_>>();
        options.dedup();
        self.approver
            .ask_user(UserQuestion {
                question: truncate_line(question, 2_000),
                options,
                multi_select: args.multi_select.unwrap_or(false),
            })
            .await
            .map(|answer| format!("<user_answer>{}</user_answer>", escape_user_answer(&answer)))
            .ok_or_else(|| ToolError::ApprovalDenied("user skipped ask_user".to_owned()))
    }

    fn require_write_target(&self, requested: &str, allow_new: bool) -> Result<(), ToolError> {
        let Some(targets) = &self.write_targets else {
            return Ok(());
        };
        let resolved = if allow_new {
            self.resolve_new(requested)?
        } else {
            self.resolve_existing(requested)?
        };
        if targets.contains(&resolved) {
            return Ok(());
        }
        let allowed = targets
            .iter()
            .map(|target| display_relative(&self.workspace, target))
            .collect::<Vec<_>>()
            .join(", ");
        Err(ToolError::OutsideWorkspace(format!(
            "subagent may only edit {allowed}; to widen the scope, report back to the parent agent and ask to be dispatched again with the file in its task packet"
        )))
    }

    fn get_job_output(&self, args: JobOutputArgs) -> Result<String, ToolError> {
        if let Some(output) = self.background.output(
            &args.job_id,
            args.tail_lines
                .unwrap_or(crate::detached_job::DEFAULT_TAIL_LINES)
                .clamp(1, 2_000),
        ) {
            return Ok(output);
        }
        // 进程内那份找不到就问落盘的那份：脱离作业活得比 Harness 久，重启之后
        // 它只存在于磁盘上。格式见后台任务合同 v1。
        if let Some(jobs) = &self.detached_jobs
            && let Some(job) = jobs.get(&args.job_id)
        {
            return Ok(jobs.render_output(&job, args.tail_lines));
        }
        Err(ToolError::Network(format!(
            "background task not found: {}",
            args.job_id
        )))
    }

    async fn kill_job(&self, args: JobIDArgs) -> Result<String, ToolError> {
        self.require_approval(&format!("cancel background task: {}", args.job_id), false)
            .await?;
        if self.background.kill(&args.job_id) {
            return Ok(format!("kill requested for {}", args.job_id));
        }
        // run_in_background 的命令落在脱离作业里，进程内注册表查不到它。
        let detached = match &self.detached_jobs {
            Some(jobs) => jobs.kill(&args.job_id).map_err(ToolError::Io)?,
            None => crate::detached_job::KillOutcome::NotFound,
        };
        match detached {
            crate::detached_job::KillOutcome::Signalled => {
                Ok(format!("kill requested for {}", args.job_id))
            }
            crate::detached_job::KillOutcome::NotRunning => Err(ToolError::Network(format!(
                "background job already finished: {}; read it with get_job_output",
                args.job_id
            ))),
            crate::detached_job::KillOutcome::NotFound => Err(ToolError::Network(format!(
                "running background task not found: {}",
                args.job_id
            ))),
        }
    }

    fn resolve_existing(&self, requested: &str) -> Result<PathBuf, ToolError> {
        validate_workspace_relative(requested)?;
        let resolved = self.workspace.join(requested).canonicalize()?;
        self.ensure_inside(resolved, requested)
    }

    fn resolve_new(&self, requested: &str) -> Result<PathBuf, ToolError> {
        validate_workspace_relative(requested)?;
        let candidate = self.workspace.join(requested);
        if candidate.is_absolute() && !candidate.starts_with(&self.workspace) {
            return Err(ToolError::OutsideWorkspace(requested.to_owned()));
        }
        let mut ancestor = candidate.parent().unwrap_or(&self.workspace);
        while !ancestor.exists() {
            ancestor = ancestor
                .parent()
                .ok_or_else(|| ToolError::OutsideWorkspace(requested.to_owned()))?;
        }
        let canonical = ancestor.canonicalize()?;
        if !canonical.starts_with(&self.workspace) {
            return Err(ToolError::OutsideWorkspace(requested.to_owned()));
        }
        Ok(candidate)
    }

    fn ensure_inside(&self, path: PathBuf, requested: &str) -> Result<PathBuf, ToolError> {
        if path.starts_with(&self.workspace) {
            Ok(path)
        } else {
            Err(ToolError::OutsideWorkspace(requested.to_owned()))
        }
    }
}

mod background_shell;
mod verification;
use background_shell::run_background_shell;
pub use background_shell::run_background_supervisor;
use verification::{capture_verification_snapshot, report_verification, verification_status};

fn validate_workspace_relative(requested: &str) -> Result<(), ToolError> {
    let path = Path::new(requested);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(ToolError::OutsideWorkspace(requested.to_owned()));
    }
    Ok(())
}

async fn atomic_write(path: &Path, content: &[u8]) -> Result<(), std::io::Error> {
    let permissions = tokio::fs::metadata(path).await?.permissions();
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("file");
    let temporary = path.with_file_name(format!(
        ".{file_name}.willdeep-tmp-{}",
        uuid::Uuid::new_v4()
    ));
    let result = async {
        let mut file = tokio::fs::File::create(&temporary).await?;
        file.write_all(content).await?;
        file.flush().await?;
        drop(file);
        tokio::fs::set_permissions(&temporary, permissions).await?;
        tokio::fs::rename(&temporary, path).await
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    result
}

fn parse<T: for<'de> Deserialize<'de>>(call: &ToolCall) -> Result<T, ToolError> {
    serde_json::from_str(&call.arguments).map_err(|source| ToolError::InvalidArguments {
        tool: call.name.clone(),
        source,
    })
}

fn definition(
    name: impl Into<String>,
    description: impl Into<String>,
    parameters: serde_json::Value,
) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        description: description.into(),
        parameters,
    }
}

fn display_relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn truncate_line(line: &str, max_chars: usize) -> String {
    let mut value = line.chars().take(max_chars).collect::<String>();
    if line.chars().count() > max_chars {
        value.push('…');
    }
    value
}

const COMMAND_SIGNATURE_PREFIX: &str = "command-exact:";
const WEB_POST_SIGNATURE_PREFIX: &str = "web-post:";

fn command_signature(command: &str) -> Option<String> {
    if command
        .chars()
        .any(|value| matches!(value, '|' | '&' | ';' | '>' | '<' | '`' | '\n' | '\r'))
    {
        return None;
    }
    let normalized = command.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() || command_carries_credentials(&normalized) {
        return None;
    }
    Some(format!("{COMMAND_SIGNATURE_PREFIX}{normalized}"))
}

/// A remembered rule is the command verbatim, so a command carrying an inline
/// credential would park that secret in `always-allow.json` until someone
/// notices — and nobody audits a file whose whole job is to stop asking. Such
/// commands stay one-shot approvals: the operator can still run them, they
/// just never become a stored rule.
///
/// Reuses the judge's redactor rather than a second marker list, so both
/// paths recognise the same shapes and cannot drift apart. `command` must
/// already be whitespace-normalized, since the redactor normalizes too.
fn command_carries_credentials(command: &str) -> bool {
    crate::judge::redact_credentials(command) != command
}

pub(crate) fn child_command_is_sensitive(command: &str) -> bool {
    if command_carries_credentials(command) {
        return true;
    }
    let normalized = command.to_ascii_lowercase();
    const SENSITIVE_PATHS: &[&str] = &[
        "~/.ssh",
        "/.ssh/",
        "/.gnupg",
        "/.aws",
        "/.config/gh",
        "/library/keychains",
        ".env",
        "id_rsa",
        "id_ed25519",
        ".aws/credentials",
        ".kube/config",
        ".docker/config.json",
        "/etc/shadow",
        "private_key",
        "private key",
        ".pem",
        ".p12",
        ".pfx",
    ];
    if SENSITIVE_PATHS
        .iter()
        .any(|marker| normalized.contains(marker))
    {
        return true;
    }
    let head = normalized.split_whitespace().next().unwrap_or_default();
    matches!(
        head,
        "env" | "printenv" | "set" | "security" | "op" | "pass" | "gpg" | "ssh-add"
    ) || normalized.contains("security find-generic-password")
        || normalized.contains("security find-internet-password")
}

fn reviewed_subagent_denial(command: &str, reason: &str) -> ToolError {
    ToolError::ApprovalDenied(format!(
        "subagent command was not authorized ({reason}). Report this exact command to the parent: {command}\nThe parent may ask the human, then respawn profile=\"ops_runner\" with target_command set to the identical command. Do not decorate, rewrite, or substitute it."
    ))
}

fn rule_carries_credentials(rule: &str) -> bool {
    command_carries_credentials(
        rule.strip_prefix(COMMAND_SIGNATURE_PREFIX)
            .unwrap_or(rule)
            .trim(),
    )
}

fn contains_sensitive_command(command: &str) -> bool {
    let uppercase = command.to_ascii_uppercase();
    ["API_KEY", "TOKEN=", "SECRET=", "PASSWORD=", "AUTHORIZATION"]
        .iter()
        .any(|marker| uppercase.contains(marker))
}

fn truncate_utf8_bytes(mut value: String, limit: usize) -> String {
    if value.len() <= limit {
        return value;
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value
}

fn escape_user_answer(answer: &str) -> String {
    answer
        .trim()
        .chars()
        .take(8_000)
        .collect::<String>()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn web_client() -> Result<reqwest::Client, ToolError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|error| ToolError::Network(error.to_string()))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WebMethod {
    Get,
    Post,
}

fn parse_web_method(raw: Option<&str>) -> Result<WebMethod, ToolError> {
    match raw.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(WebMethod::Get),
        Some(value) if value.eq_ignore_ascii_case("get") => Ok(WebMethod::Get),
        Some(value) if value.eq_ignore_ascii_case("post") => Ok(WebMethod::Post),
        Some(value) => Err(ToolError::Network(format!(
            "unsupported HTTP method: {value}; web_fetch supports GET and POST"
        ))),
    }
}

/// 「主域名」按公共后缀表取：`api.example.com` 和 `upload.example.com` 归到同
/// 一条 `example.com` 规则，而 `example.co.uk` 整体就是一个注册域名。机械地取
/// 后两段会把它截成 `co.uk`，那条规则等于放行整个英国二级域。IP 直连没有域名
/// 可归并，按字面量各自成规则。
fn registrable_domain(url: &reqwest::Url) -> String {
    let Some(host) = url.host_str() else {
        return url.as_str().to_ascii_lowercase();
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.parse::<IpAddr>().is_ok() {
        return host.to_ascii_lowercase();
    }
    psl::domain_str(host).unwrap_or(host).to_ascii_lowercase()
}

fn same_hostname(left: &reqwest::Url, right: &reqwest::Url) -> bool {
    left.host_str()
        .zip(right.host_str())
        .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right))
}

fn redirect_key(url: &reqwest::Url) -> String {
    let mut normalized = url.clone();
    normalized.set_fragment(None);
    normalized.to_string()
}

async fn read_web_response(response: reqwest::Response) -> Result<Vec<u8>, ToolError> {
    let mut output = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| ToolError::Network(error.to_string()))?;
        append_web_chunk(&mut output, &chunk)?;
    }
    Ok(output)
}

fn append_web_chunk(output: &mut Vec<u8>, chunk: &[u8]) -> Result<(), ToolError> {
    if output.len().saturating_add(chunk.len()) > MAX_WEB_RESPONSE_BYTES {
        return Err(ToolError::Network(
            "response exceeds the 3 MiB limit".to_owned(),
        ));
    }
    output.extend_from_slice(chunk);
    Ok(())
}

/// Validate that an HTTP(S) URL resolves exclusively to public addresses.
///
/// Kept as a shared boundary for both agent web tools and user-triggered TUI
/// media downloads so SSRF rules cannot drift between the two call sites.
pub async fn validate_public_url(url: &reqwest::Url) -> Result<(), ToolError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ToolError::Network(
            "only HTTP(S) URLs are supported".to_owned(),
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| ToolError::Network("URL has no host".to_owned()))?;
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        return Err(ToolError::Network(
            "loopback targets are refused".to_owned(),
        ));
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| ToolError::Network("URL has no usable port".to_owned()))?;
    let addresses = tokio::net::lookup_host((host, port))
        .await
        .map_err(|error| ToolError::Network(format!("cannot resolve host: {error}")))?;
    let mut found = false;
    for address in addresses {
        found = true;
        if !is_public_ip(address.ip()) {
            return Err(ToolError::Network(format!(
                "private, loopback, or link-local target is refused: {}",
                address.ip()
            )));
        }
    }
    if !found {
        return Err(ToolError::Network(
            "host resolved to no addresses".to_owned(),
        ));
    }
    Ok(())
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(value) => is_public_ipv4(value),
        IpAddr::V6(value) => is_public_ipv6(value),
    }
}

fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || octets[0] == 0
        || octets[0] >= 224
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
        || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
        || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113))
}

fn is_public_ipv6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] == 0x2001 && segments[1] == 0x0db8))
}

fn html_to_text(html: &str) -> String {
    let scripts = regex::Regex::new(
        r"(?is)<script[^>]*>.*?</script>|<style[^>]*>.*?</style>|<noscript[^>]*>.*?</noscript>",
    )
    .expect("valid HTML cleanup regex")
    .replace_all(html, " ");
    let breaks = regex::Regex::new(r"(?i)</?(p|div|br|li|h[1-6]|tr|section|article)[^>]*>")
        .expect("valid HTML block regex")
        .replace_all(&scripts, "\n");
    let tags = regex::Regex::new(r"(?s)<[^>]+>")
        .expect("valid HTML tag regex")
        .replace_all(&breaks, " ");
    let decoded = tags
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    let whitespace = regex::Regex::new(r"[ \t\r\x0B\x0C]+")
        .expect("valid whitespace regex")
        .replace_all(&decoded, " ");
    regex::Regex::new(r"\n\s*\n+")
        .expect("valid blank line regex")
        .replace_all(&whitespace, "\n\n")
        .trim()
        .to_owned()
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    format!(
        "{}\n[content truncated]",
        value.chars().take(max_chars).collect::<String>()
    )
}

fn format_search_results(body: &str) -> Result<String, ToolError> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|error| ToolError::Network(format!("invalid search response: {error}")))?;
    let data = value.get("data").unwrap_or(&value);
    let items = data
        .get("results")
        .or_else(|| data.get("items"))
        .or_else(|| data.as_array().map(|_| data))
        .and_then(serde_json::Value::as_array);
    let Some(items) = items else {
        return serde_json::to_string_pretty(data)
            .map_err(|error| ToolError::Network(error.to_string()));
    };
    let lines = items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let title = item
                .get("title")
                .and_then(|value| value.as_str())
                .unwrap_or("Untitled");
            let url = item
                .get("url")
                .or_else(|| item.get("link"))
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let snippet = item
                .get("snippet")
                .or_else(|| item.get("content"))
                .or_else(|| item.get("description"))
                .and_then(|value| value.as_str())
                .unwrap_or("");
            format!(
                "{}. {title}\n{url}\n{}",
                index + 1,
                truncate_line(snippet, 1_000)
            )
        })
        .collect::<Vec<_>>();
    Ok(if lines.is_empty() {
        "No search results.".to_owned()
    } else {
        lines.join("\n\n")
    })
}

fn truncate_bytes(value: String, limit: usize) -> String {
    if value.len() <= limit {
        return value;
    }
    let mut boundary = limit;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}\n[output truncated]", &value[..boundary])
}

fn git_output(output: std::process::Output, limit: usize) -> Result<String, ToolError> {
    if !output.status.success() {
        return Err(ToolError::Io(std::io::Error::other(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        )));
    }
    Ok(truncate_bytes(
        String::from_utf8_lossy(&output.stdout).into_owned(),
        limit,
    ))
}

/// Which worker profile is built for this failing command, if any. Matched on
/// the shape of the command line, not on the failure text: a `cargo test` that
/// fails to compile and one that fails an assertion both land in the same
/// place, and the worker is the one that reads the difference.
fn delegable_failure_profile(command: &str) -> Option<&'static str> {
    let lowered = command.to_ascii_lowercase();
    const TEST_MARKERS: &[&str] = &[
        "cargo test",
        "cargo nextest",
        "go test",
        "npm test",
        "yarn test",
        "pnpm test",
        "pytest",
        "rspec",
        "bundle exec rspec",
        "jest",
        "vitest",
        "xcodebuild test",
        "swift test",
        "gradle test",
        "mvn test",
    ];
    const BUILD_MARKERS: &[&str] = &[
        "cargo build",
        "cargo check",
        "cargo clippy",
        "go build",
        "go vet",
        "tsc",
        "npm run build",
        "yarn build",
        "pnpm build",
        "make",
        "cmake",
        "xcodebuild build",
        "swift build",
        "mypy",
        "ruff",
        "eslint",
    ];
    if TEST_MARKERS.iter().any(|marker| lowered.contains(marker)) {
        return Some("test_fixer");
    }
    if BUILD_MARKERS.iter().any(|marker| lowered.contains(marker)) {
        return Some("build_fixer");
    }
    None
}

/// The hint appended to a delegable failure. Deliberately a suggestion with a
/// ready-made recipe rather than an automatic spawn: the parent still owns the
/// decision, but the cost of delegating drops to one tool call.
fn delegation_hint(profile: &str, command: &str) -> String {
    format!(
        "\n\n<delegation-hint profile=\"{profile}\">\nThis failure is a good fit for the `{profile}` worker. \
Spawn it with a task packet: goal, the relevant files you already know about, the failing assertions as known_facts, \
and verifier.command = {command:?}. The worker fixes and re-verifies on its own; only a verified pass or an \
exhausted attempt budget comes back to you.\n</delegation-hint>"
    )
}

/// 围栏拦下之后贴给模型看的话。写清楚「哪一档、能写哪儿」，模型才有可能
/// 自己改到工作区里去，而不是把同一条越界命令再试三遍。
fn sandbox_denial_hint(sandbox: &SandboxSpec) -> String {
    let roots = if sandbox.writable_roots.is_empty() {
        "（这一档什么都不许写）".to_owned()
    } else {
        sandbox
            .writable_roots
            .iter()
            .map(|root| root.display().to_string())
            .collect::<Vec<_>>()
            .join("、")
    };
    format!(
        "\n\n<sandbox-denied>\n这条命令看起来是被 OS 级写入围栏拦下的，不是命令本身写错了。\n\
当前档位只允许写入：{roots}\n\
把写入目标改到允许范围内，或请用户放宽工作区策略后重试。\n</sandbox-denied>"
    )
}

#[cfg(windows)]
pub(crate) const SHELL_PROGRAM: &str = "powershell.exe";
#[cfg(not(windows))]
pub(crate) const SHELL_PROGRAM: &str = "/bin/sh";

#[cfg(windows)]
pub(crate) fn platform_shell(command: &str) -> Command {
    let mut process = Command::new("powershell.exe");
    process.args(["-NoProfile", "-NonInteractive", "-Command", command]);
    process
}

#[cfg(not(windows))]
pub(crate) fn platform_shell(command: &str) -> Command {
    let mut process = Command::new(SHELL_PROGRAM);
    process.args([crate::sandbox::SHELL_COMMAND_FLAG, command]);
    process
}

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    max_results: Option<usize>,
}

#[derive(Deserialize)]
struct OutputPageArgs {
    id: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct ListSkillsArgs {
    query: Option<String>,
}
#[derive(Deserialize)]
struct ReadSkillArgs {
    name: String,
    resource: Option<String>,
}

#[derive(Deserialize)]
struct ListMcpToolsArgs {
    query: Option<String>,
    max_results: Option<usize>,
}

#[derive(Deserialize)]
struct CallMcpToolArgs {
    name: String,
    arguments: serde_json::Value,
}

#[derive(Deserialize)]
struct GrepArgs {
    pattern: String,
    path: Option<String>,
    include: Option<String>,
    case_sensitive: Option<bool>,
    max_results: Option<usize>,
}

#[derive(Deserialize)]
struct ReadArgs {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
    max_bytes: Option<usize>,
}

#[derive(Deserialize)]
struct ListDirectoryArgs {
    path: Option<String>,
}

#[derive(Deserialize)]
struct CommandArgs {
    command: String,
    timeout_seconds: Option<u64>,
    label: Option<String>,
    run_in_background: Option<bool>,
}

#[derive(Deserialize)]
struct JobOutputArgs {
    job_id: String,
    tail_lines: Option<usize>,
}

#[derive(Deserialize)]
struct JobIDArgs {
    job_id: String,
}

#[derive(Deserialize)]
struct AskUserArgs {
    question: String,
    options: Option<Vec<String>>,
    multi_select: Option<bool>,
}

#[derive(Deserialize)]
struct GitDiffArgs {
    path: Option<String>,
    staged: Option<bool>,
    stat_only: Option<bool>,
}

#[derive(Deserialize)]
struct GitLogArgs {
    path: Option<String>,
    max_count: Option<usize>,
    author: Option<String>,
    since: Option<String>,
}

#[derive(Deserialize)]
struct GitBlameArgs {
    path: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
}

#[derive(Deserialize)]
struct CreateWorktreeArgs {
    branch: String,
}

fn sanitize_branch(value: &str) -> Result<String, ToolError> {
    let value = value.trim();
    if value.is_empty()
        || value.starts_with('-')
        || value.contains("..")
        || value
            .chars()
            .any(|c| c.is_whitespace() || "~^:?*[\\".contains(c))
    {
        return Err(ToolError::Io(std::io::Error::other(
            "invalid Git branch name",
        )));
    }
    Ok(value.to_owned())
}

#[derive(Deserialize)]
struct WebSearchArgs {
    query: String,
    count: Option<usize>,
}

#[derive(Deserialize)]
struct WebFetchArgs {
    url: String,
    method: Option<String>,
    body: Option<String>,
    content_type: Option<String>,
    max_chars: Option<usize>,
}

#[derive(Deserialize)]
struct CreateArgs {
    path: String,
    content: String,
}

#[derive(Deserialize)]
struct EditArgs {
    path: String,
    old_string: String,
    new_string: String,
    replace_all: Option<bool>,
}

#[cfg(test)]
mod tests;
