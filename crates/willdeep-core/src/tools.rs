use std::collections::{BTreeSet, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use futures_util::StreamExt;
use globset::Glob;
use ignore::WalkBuilder;
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
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
    pub command: String,
    pub exit_code: Option<i32>,
    pub status: VerificationStatus,
    pub summary: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerificationStatus {
    Passed,
    Failed,
    TimedOut,
    LaunchFailed,
}

type VerificationReporter = Arc<dyn Fn(CommandVerification) + Send + Sync>;

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
    #[error("command timed out after {0} seconds")]
    CommandTimeout(u64),
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
            safety_judge: None,
            task_context: Arc::new(Mutex::new(String::new())),
            approval_reporter: None,
            sandbox: SandboxSpec::new(SandboxPolicy::Off, []),
            hooks: HookRegistry::default(),
            session_id: None,
        })
    }

    /// 注册生命周期挂钩。与审批闸门是两回事：闸门问的是用户，hook 问的是
    /// 用户**事先配好的程序**——审计留痕和 CI 门禁要的是后者。
    pub fn with_hooks(mut self, hooks: HookRegistry) -> Self {
        self.hooks = hooks;
        self
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

    /// 建一条 shell 命令，能套围栏就套。套不上（平台不支持、这一档不需要）
    /// 就退回裸 shell —— 退回是静默的，但 [`crate::sandbox::available`] 让
    /// 上层能查出「这台机器上根本没有围栏」，不至于以为自己有。
    fn shell_command(&self, command: &str) -> Command {
        match self.sandbox.command_line(SHELL_PROGRAM, command) {
            Some(argv) => {
                let mut process = Command::new(&argv[0]);
                process.args(&argv[1..]);
                process
            }
            None => platform_shell(command),
        }
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
        self.verification_reporter = Some(Arc::new(reporter));
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

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        let mut tools = vec![
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
                "Read the captured output of a background shell job or subagent.",
                json!({"type":"object","properties":{"job_id":{"type":"string"},"tail_lines":{"type":"integer","minimum":1,"maximum":2000}},"required":["job_id"],"additionalProperties":false}),
            ),
            definition(
                "kill_job",
                "Request cancellation of a running background shell job or subagent.",
                json!({"type":"object","properties":{"job_id":{"type":"string"}},"required":["job_id"],"additionalProperties":false}),
            ),
            definition(
                "spawn_agent",
                "Delegate a self-contained task to an isolated child. Responsibility and model are separate choices. The public trades are generalist (investigation across files and repository state), implementer (bounded coding), tester (tests and verification), reviewer (independent correctness/safety review), and ops_runner (bounded command execution); legacy specialist IDs remain internally compatible but are not public choices. worker_tier picks the model: standard by default, advanced for harder reasoning, expert only after smaller tiers were attempted and a runtime-validated escalation ticket explains why decomposition cannot work. Children cannot spawn agents or show approval UI. A command-capable child uses static safety rules first and AI review only for the ambiguous, non-sensitive middle. If review declines or is unavailable, it returns the exact command; the parent may respawn ops_runner with target_command, which requests one-time human approval for that identical command. Pass task whenever possible so the worker receives known facts, exact read/write files, and a verifier instead of rediscovering them.",
                json!({"type":"object","properties":{
                    "prompt":{"type":"string","description":"Free-text instruction. Still required when `task` is present; keep it to what the packet does not already say."},
                    "label":{"type":"string"},
                    "profile":{"type":"string","enum":["generalist","implementer","tester","reviewer","ops_runner"],"description":"The responsibility. Independent of worker_tier."},
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
                        "run_in_background": {"type": "boolean", "description": "Return a job handle immediately. Completion is delivered back to the main harness."}
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
            tools.retain(|tool| allowed.contains(&tool.name));
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
                    .spawn(&args.command, &description, &self.workspace)
                    .map_err(ToolError::Io)?;
                return Ok(format!(
                    "Background job started: {} (pid {}). It survives a Runtime restart; read it with get_job_output.",
                    job.id, job.pid
                ));
            }
            let command = args.command;
            let workspace = self.workspace.clone();
            let verification_reporter = self.verification_reporter.clone();
            let id = self.background.start_retriable(
                BackgroundTaskKind::Shell,
                description,
                move || {
                    let command = command.clone();
                    let workspace = workspace.clone();
                    let verification_reporter = verification_reporter.clone();
                    async move {
                        let result =
                            run_background_shell(command.clone(), workspace, timeout).await;
                        report_verification(
                            verification_reporter.as_ref(),
                            &command,
                            result.exit_code,
                            verification_status(&result.status),
                            &result.output,
                        );
                        result
                    }
                },
            );
            return Ok(format!(
                "Background task started: {id}. Completion will be delivered automatically; use get_job_output for details."
            ));
        }
        let mut command = self.shell_command(&args.command);
        command
            .current_dir(&self.workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = command.spawn()?;
        let output = match tokio::time::timeout(
            std::time::Duration::from_secs(timeout),
            child.wait_with_output(),
        )
        .await
        {
            Ok(output) => output?,
            Err(_) => {
                report_verification(
                    self.verification_reporter.as_ref(),
                    &args.command,
                    None,
                    VerificationStatus::TimedOut,
                    &format!("command timed out after {timeout} seconds"),
                );
                return Err(ToolError::CommandTimeout(timeout));
            }
        };
        let text = format!(
            "exit_code: {}\nstdout:\n{}\nstderr:\n{}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        report_verification(
            self.verification_reporter.as_ref(),
            &args.command,
            output.status.code(),
            if output.status.success() {
                VerificationStatus::Passed
            } else {
                VerificationStatus::Failed
            },
            &text,
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
        if let Some(output) = self
            .background
            .output(&args.job_id, args.tail_lines.unwrap_or(200).clamp(1, 2_000))
        {
            return Ok(output);
        }
        // 进程内那份找不到就问落盘的那份：脱离作业活得比 Harness 久，重启之后
        // 它只存在于磁盘上。
        if let Some(jobs) = &self.detached_jobs
            && let Some(job) = jobs.get(&args.job_id)
        {
            let report = jobs.report(&job);
            let status = match report.state {
                crate::detached_job::JobState::Running => format!("running (pid {})", job.pid),
                crate::detached_job::JobState::Finished { exit_code } => {
                    format!("finished with exit code {exit_code}")
                }
                // 「不知道」和「失败」不是一回事：失败有退出码。
                crate::detached_job::JobState::Vanished => {
                    "process is gone and left no exit code".to_owned()
                }
            };
            return Ok(format!("{}: {status}\n{}", job.id, report.output));
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
            Ok(format!("kill requested for {}", args.job_id))
        } else {
            Err(ToolError::Network(format!(
                "running background task not found: {}",
                args.job_id
            )))
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

#[derive(Debug, Serialize, Deserialize)]
struct BackgroundSupervisorRequest {
    command: String,
    workspace: PathBuf,
    timeout_seconds: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct BackgroundSupervisorResult {
    status: BackgroundTaskStatus,
    exit_code: Option<i32>,
    output: String,
}

#[cfg(not(test))]
async fn run_background_shell(
    command: String,
    workspace: PathBuf,
    timeout_seconds: u64,
) -> TaskResult {
    match run_supervised_background_shell(command, workspace, timeout_seconds).await {
        Ok(result) => TaskResult {
            status: result.status,
            exit_code: result.exit_code,
            output: result.output,
        },
        Err(error) => TaskResult {
            status: BackgroundTaskStatus::LaunchFailed,
            exit_code: Some(-1),
            output: format!("background supervisor failed: {error}"),
        },
    }
}

#[cfg(test)]
async fn run_background_shell(
    command: String,
    workspace: PathBuf,
    timeout_seconds: u64,
) -> TaskResult {
    let mut process = platform_shell(&command);
    process
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    match tokio::time::timeout(std::time::Duration::from_secs(timeout_seconds), async {
        process.spawn()?.wait_with_output().await
    })
    .await
    {
        Ok(Ok(output)) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            TaskResult {
                status: if output.status.success() {
                    BackgroundTaskStatus::Completed
                } else {
                    BackgroundTaskStatus::Failed
                },
                exit_code: output.status.code(),
                output: text,
            }
        }
        Ok(Err(error)) => TaskResult {
            status: BackgroundTaskStatus::LaunchFailed,
            exit_code: Some(-1),
            output: error.to_string(),
        },
        Err(_) => TaskResult {
            status: BackgroundTaskStatus::TimedOut,
            exit_code: None,
            output: format!("command timed out after {timeout_seconds} seconds"),
        },
    }
}

#[cfg(not(test))]
async fn run_supervised_background_shell(
    command: String,
    workspace: PathBuf,
    timeout_seconds: u64,
) -> anyhow::Result<BackgroundSupervisorResult> {
    let request = BackgroundSupervisorRequest {
        command,
        workspace,
        timeout_seconds,
    };
    let payload = serde_json::to_vec(&request)?;
    anyhow::ensure!(
        payload.len() <= MAX_SUPERVISOR_REQUEST_BYTES,
        "background supervisor request is too large"
    );
    let executable = std::env::current_exe()?;
    let mut process = Command::new(executable);
    process
        .args(["daemon", "background-supervisor"])
        .env(BACKGROUND_SUPERVISOR_ENV, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false);
    let mut child = process.spawn()?;
    let mut liveness = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("background supervisor stdin is unavailable"))?;
    let length = u32::try_from(payload.len())?.to_be_bytes();
    liveness.write_all(&length).await?;
    liveness.write_all(&payload).await?;
    liveness.flush().await?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("background supervisor stdout is unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("background supervisor stderr is unavailable"))?;
    let stdout_task = tokio::spawn(read_bounded(stdout, MAX_COMMAND_OUTPUT_BYTES));
    let stderr_task = tokio::spawn(read_bounded(stderr, MAX_COMMAND_OUTPUT_BYTES));
    let status = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_seconds.saturating_add(10)),
        child.wait(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("background supervisor did not stop after its deadline"))??;
    drop(liveness);
    let stdout = stdout_task.await??;
    let stderr = stderr_task.await??;
    anyhow::ensure!(
        status.success(),
        "background supervisor exited with {:?}: {}",
        status.code(),
        String::from_utf8_lossy(&stderr).trim()
    );
    serde_json::from_slice(&stdout)
        .map_err(|error| anyhow::anyhow!("decode background supervisor result: {error}"))
}

pub async fn run_background_supervisor() -> anyhow::Result<()> {
    anyhow::ensure!(
        std::env::var(BACKGROUND_SUPERVISOR_ENV).as_deref() == Ok("1"),
        "background supervisor is an internal command"
    );
    let mut input = tokio::io::stdin();
    let mut length = [0_u8; 4];
    input.read_exact(&mut length).await?;
    let length = u32::from_be_bytes(length) as usize;
    anyhow::ensure!(
        length <= MAX_SUPERVISOR_REQUEST_BYTES,
        "background supervisor request is too large"
    );
    let mut payload = vec![0_u8; length];
    input.read_exact(&mut payload).await?;
    let request: BackgroundSupervisorRequest = serde_json::from_slice(&payload)?;
    anyhow::ensure!(
        !request.command.trim().is_empty(),
        "background command is empty"
    );
    anyhow::ensure!(
        request.timeout_seconds > 0 && request.timeout_seconds <= MAX_COMMAND_TIMEOUT_SECS,
        "background command timeout is invalid"
    );
    let workspace = request.workspace.canonicalize()?;
    anyhow::ensure!(
        workspace.is_dir(),
        "background Workspace is not a directory"
    );

    let mut shell = platform_shell(&request.command);
    configure_background_process(&mut shell);
    shell
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut shell = shell.spawn()?;
    let shell_stdout = shell
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("background shell stdout is unavailable"))?;
    let shell_stderr = shell
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("background shell stderr is unavailable"))?;
    let stdout_task = tokio::spawn(read_bounded(shell_stdout, MAX_COMMAND_OUTPUT_BYTES));
    let stderr_task = tokio::spawn(read_bounded(shell_stderr, MAX_COMMAND_OUTPUT_BYTES));
    drop(input);
    let mut parent_disconnect = watch_parent_disconnect()?;
    let (status, exit_code) = tokio::select! {
        status = shell.wait() => {
            let status = status?;
            let kind = if status.success() {
                BackgroundTaskStatus::Completed
            } else {
                BackgroundTaskStatus::Failed
            };
            (kind, status.code())
        }
        parent = &mut parent_disconnect => {
            parent.map_err(|_| anyhow::anyhow!("background parent watcher stopped"))??;
            terminate_background_process(&mut shell).await;
            (BackgroundTaskStatus::Killed, None)
        }
        _ = tokio::time::sleep(std::time::Duration::from_secs(request.timeout_seconds)) => {
            terminate_background_process(&mut shell).await;
            (BackgroundTaskStatus::TimedOut, None)
        }
    };
    let mut output = String::from_utf8_lossy(&stdout_task.await??).into_owned();
    output.push_str(&String::from_utf8_lossy(&stderr_task.await??));
    let output = truncate_bytes(output, MAX_COMMAND_OUTPUT_BYTES);
    let result = BackgroundSupervisorResult {
        status,
        exit_code,
        output,
    };
    let mut stdout = tokio::io::stdout();
    stdout.write_all(&serde_json::to_vec(&result)?).await?;
    stdout.flush().await?;
    Ok(())
}

fn watch_parent_disconnect() -> anyhow::Result<tokio::sync::oneshot::Receiver<std::io::Result<()>>>
{
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("willdeep-parent-watch".to_owned())
        .spawn(move || {
            let mut input = std::io::stdin();
            let mut buffer = [0_u8; 64];
            let result = loop {
                match std::io::Read::read(&mut input, &mut buffer) {
                    Ok(0) => break Ok(()),
                    Ok(_) => {}
                    Err(error) => break Err(error),
                }
            };
            let _ = sender.send(result);
        })?;
    Ok(receiver)
}

#[cfg(unix)]
fn configure_background_process(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.as_std_mut().process_group(0);
}

#[cfg(windows)]
fn configure_background_process(_command: &mut Command) {}

async fn terminate_background_process(process: &mut tokio::process::Child) {
    #[cfg(unix)]
    {
        // `try_wait == None` means the group leader is still an unreaped live
        // child, so its PID cannot be reused between this check and killpg.
        if process.try_wait().ok().flatten().is_none()
            && let Some(pid) = process.id()
            && let Ok(group) = i32::try_from(pid)
        {
            // SAFETY: the child was placed in a fresh process group before
            // spawn and remains unreaped above; a negative PID targets only
            // that owned group rather than an unrelated process.
            unsafe {
                libc::kill(-group, libc::SIGKILL);
            }
        }
    }
    let _ = process.kill().await;
    let _ = process.wait().await;
}

async fn read_bounded<R: AsyncRead + Unpin>(
    mut reader: R,
    limit: usize,
) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::with_capacity(limit.min(8 * 1024));
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            return Ok(output);
        }
        let remaining = limit.saturating_sub(output.len());
        output.extend_from_slice(&chunk[..read.min(remaining)]);
    }
}

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

fn verification_status(status: &BackgroundTaskStatus) -> VerificationStatus {
    match status {
        BackgroundTaskStatus::Completed => VerificationStatus::Passed,
        BackgroundTaskStatus::TimedOut => VerificationStatus::TimedOut,
        BackgroundTaskStatus::LaunchFailed => VerificationStatus::LaunchFailed,
        BackgroundTaskStatus::Failed
        | BackgroundTaskStatus::Killed
        | BackgroundTaskStatus::Blocked
        | BackgroundTaskStatus::Running => VerificationStatus::Failed,
    }
}

fn report_verification(
    reporter: Option<&VerificationReporter>,
    command: &str,
    exit_code: Option<i32>,
    status: VerificationStatus,
    output: &str,
) {
    let Some(reporter) = reporter else {
        return;
    };
    if !is_verification_command(command) || contains_sensitive_command(command) {
        return;
    }
    let summary = output
        .lines()
        .rev()
        .take(40)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    reporter(CommandVerification {
        command: command.trim().to_owned(),
        exit_code,
        status,
        summary: truncate_utf8_bytes(summary, MAX_VERIFICATION_SUMMARY_BYTES),
    });
}

fn is_verification_command(command: &str) -> bool {
    let normalized = command.trim_start().to_ascii_lowercase();
    [
        "cargo test",
        "cargo nextest",
        "go test",
        "pytest",
        "python -m pytest",
        "python3 -m pytest",
        "ruby test",
        "bundle exec rspec",
        "bundle exec rake test",
        "swift test",
        "xcodebuild test",
        "yarn test",
        "yarn run test",
        "npm test",
        "npm run test",
        "pnpm test",
        "pnpm run test",
        "dotnet test",
        "mvn test",
        "mvn verify",
        "gradle test",
        "./gradlew test",
        "make test",
    ]
    .iter()
    .any(|prefix| {
        normalized == *prefix
            || normalized
                .strip_prefix(prefix)
                .is_some_and(|tail| tail.starts_with(char::is_whitespace))
    })
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
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct AllowApprover;
    #[async_trait]
    impl Approver for AllowApprover {
        async fn approve(
            &self,
            _description: &str,
            _always_allow_available: bool,
        ) -> ApprovalDecision {
            ApprovalDecision::AllowOnce
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn mcp_schemas_are_loaded_on_demand_through_two_fixed_tools() {
        let script = r#"read init
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":"1"}}}'
read initialized
read list
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"Echo text","inputSchema":{"type":"object","properties":{"text":{"type":"string"}}}}]}}'
read call
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"pong"}]}}'
"#;
        let mut configs = BTreeMap::new();
        configs.insert(
            "mock".to_owned(),
            crate::mcp::McpServerConfig {
                command: "/bin/sh".to_owned(),
                args: vec!["-c".to_owned(), script.to_owned()],
                env: BTreeMap::new(),
                startup_timeout_seconds: 5,
                enabled: true,
            },
        );
        let mcp = Arc::new(McpRegistry::connect(&configs).await.expect("connect MCP"));
        let root =
            std::env::temp_dir().join(format!("willdeep-mcp-tools-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("workspace");
        let registry = ToolRegistry::new(&root, ApprovalMode::Strict)
            .expect("registry")
            .with_mcp(mcp)
            .with_approver(Arc::new(AllowApprover));
        let names = registry
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>();
        assert!(names.contains(&"list_mcp_tools".to_owned()));
        assert!(names.contains(&"call_mcp_tool".to_owned()));
        assert!(!names.iter().any(|name| name.starts_with("mcp__")));

        let listed = registry
            .execute(&ToolCall {
                id: "list".to_owned(),
                name: "list_mcp_tools".to_owned(),
                arguments: json!({"query":"echo"}).to_string(),
            })
            .await
            .expect("search MCP tools");
        assert!(listed.contains("mcp__mock__echo"));
        assert!(listed.contains("parameters"));
        let called = registry
            .execute(&ToolCall {
                id: "call".to_owned(),
                name: "call_mcp_tool".to_owned(),
                arguments: json!({"name":"mcp__mock__echo","arguments":{"text":"ping"}})
                    .to_string(),
            })
            .await
            .expect("call MCP tool");
        assert!(called.contains("pong"));
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    struct AlwaysApprover(AtomicUsize);
    #[async_trait]
    impl Approver for AlwaysApprover {
        async fn approve(&self, _description: &str, available: bool) -> ApprovalDecision {
            self.0.fetch_add(1, Ordering::SeqCst);
            if available {
                ApprovalDecision::AlwaysAllow
            } else {
                ApprovalDecision::AllowOnce
            }
        }
    }

    struct AnswerApprover;
    #[async_trait]
    impl Approver for AnswerApprover {
        async fn approve(&self, _description: &str, _available: bool) -> ApprovalDecision {
            ApprovalDecision::Deny
        }
        async fn ask_user(&self, question: UserQuestion) -> Option<String> {
            assert_eq!(question.options, vec!["Rust", "Go"]);
            Some("Other <custom>".to_owned())
        }
    }

    fn workspace(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("willdeep-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("create fixture workspace");
        path
    }

    fn git(root: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("run git fixture command");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    async fn read_file_matches_swift_line_number_contract() {
        let root = workspace("read");
        std::fs::write(root.join("file.txt"), "one\ntwo\nthree\n").expect("fixture");
        let registry = ToolRegistry::new(&root, ApprovalMode::Strict).expect("registry");
        let output = registry
            .read_file(ReadArgs {
                path: "file.txt".to_owned(),
                offset: Some(2),
                limit: Some(2),
                max_bytes: None,
            })
            .await
            .expect("read");
        assert_eq!(output, "     2  two\n     3  three\n");
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[tokio::test]
    async fn git_history_tools_are_bounded_read_only_and_path_scoped() {
        let root = workspace("git-history");
        git(&root, &["init"]);
        git(&root, &["config", "user.name", "WillDeep Test"]);
        git(&root, &["config", "user.email", "test@example.invalid"]);
        std::fs::write(root.join("history.txt"), "first\nsecond\n").expect("first fixture");
        git(&root, &["add", "history.txt"]);
        git(&root, &["commit", "-m", "initial history"]);
        std::fs::write(root.join("history.txt"), "first\nchanged\n").expect("second fixture");
        git(&root, &["add", "history.txt"]);
        git(&root, &["commit", "-m", "update history"]);

        let registry = ToolRegistry::new(&root, ApprovalMode::Strict).expect("registry");
        let log = registry
            .git_log(GitLogArgs {
                path: Some("history.txt".to_owned()),
                max_count: Some(1),
                author: Some("WillDeep Test".to_owned()),
                since: None,
            })
            .await
            .expect("git log");
        assert!(log.contains("update history"));
        assert!(!log.contains("initial history"));
        assert_eq!(log.lines().count(), 1);

        let blame = registry
            .git_blame(GitBlameArgs {
                path: "history.txt".to_owned(),
                start_line: None,
                end_line: None,
            })
            .await
            .expect("git blame");
        assert!(blame.contains("WillDeep Test"));
        assert!(blame.contains("first"));
        assert!(blame.contains("changed"));

        let invalid_range = registry
            .git_blame(GitBlameArgs {
                path: "history.txt".to_owned(),
                start_line: Some(3),
                end_line: Some(2),
            })
            .await;
        assert!(invalid_range.is_err());
        let escape = registry
            .git_blame(GitBlameArgs {
                path: "../../etc/passwd".to_owned(),
                start_line: None,
                end_line: None,
            })
            .await;
        assert!(matches!(escape, Err(ToolError::OutsideWorkspace(_))));
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn existing_symlink_escape_is_rejected() {
        let root = workspace("escape");
        let registry = ToolRegistry::new(&root, ApprovalMode::Strict).expect("registry");
        let result = registry.resolve_existing("../../etc/passwd");
        assert!(matches!(result, Err(ToolError::OutsideWorkspace(_))));
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[tokio::test]
    async fn workspace_access_applies_exact_edit() {
        let root = workspace("edit");
        std::fs::write(root.join("file.txt"), "alpha beta").expect("fixture");
        let registry = ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess).expect("registry");
        registry
            .edit_file(EditArgs {
                path: "file.txt".to_owned(),
                old_string: "beta".to_owned(),
                new_string: "gamma".to_owned(),
                replace_all: None,
            })
            .await
            .expect("edit");
        assert_eq!(
            std::fs::read_to_string(root.join("file.txt")).expect("read"),
            "alpha gamma"
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[tokio::test]
    async fn a_blocking_hook_stops_the_tool_before_it_runs() {
        // 引擎能拦是一回事，接没接上工具分发是另一回事。这条钉的是后者。
        let root = workspace("hook-gate");
        let target = root.join("file.txt");
        std::fs::write(&target, "unchanged").expect("fixture");
        let registry = ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess)
            .expect("registry")
            .with_hooks(crate::hooks::HookRegistry::new(vec![crate::hooks::Hook {
                name: "change-ticket".to_owned(),
                event: crate::hooks::HookEvent::PreTool,
                command: "echo '缺少变更单编号' >&2; exit 1".to_owned(),
                blocking: true,
                timeout: std::time::Duration::from_secs(5),
                on_error: crate::hooks::HookFailure::Deny,
            }]));

        let result = registry
            .execute(&ToolCall {
                id: "write".to_owned(),
                name: "edit_file".to_owned(),
                arguments: serde_json::json!({
                    "path": "file.txt",
                    "old_string": "unchanged",
                    "new_string": "changed"
                })
                .to_string(),
            })
            .await;

        let Err(ToolError::HookDenied(message)) = result else {
            panic!("hook 应当拦下这次调用：{result:?}");
        };
        assert!(message.contains("change-ticket"), "{message}");
        assert!(message.contains("缺少变更单编号"), "{message}");
        // 拦住的意思是文件没被动过，不是"改完了再报个错"。
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "unchanged");
    }

    #[tokio::test]
    async fn an_observer_hook_sees_the_call_without_blocking_it() {
        let root = workspace("hook-audit");
        std::fs::write(root.join("file.txt"), "unchanged").expect("fixture");
        let log = root.join("audit.log");
        let registry = ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess)
            .expect("registry")
            .with_hooks(crate::hooks::HookRegistry::new(vec![crate::hooks::Hook {
                name: "audit".to_owned(),
                event: crate::hooks::HookEvent::PreTool,
                command: format!("cat >> {}", log.display()),
                blocking: false,
                timeout: std::time::Duration::from_secs(5),
                on_error: crate::hooks::HookFailure::Deny,
            }]));

        let result = registry
            .execute(&ToolCall {
                id: "edit".to_owned(),
                name: "edit_file".to_owned(),
                arguments: serde_json::json!({
                    "path": "file.txt",
                    "old_string": "unchanged",
                    "new_string": "changed"
                })
                .to_string(),
            })
            .await;

        assert!(result.is_ok(), "{result:?}");
        let recorded = std::fs::read_to_string(&log).expect("审计 hook 应当收到事件");
        assert!(recorded.contains("\"event\":\"pre_tool\""), "{recorded}");
        assert!(recorded.contains("edit_file"), "{recorded}");
    }

    #[tokio::test]
    async fn read_only_policy_blocks_write_capable_tools_before_approval() {
        let root = workspace("read-only");
        std::fs::write(root.join("file.txt"), "unchanged").expect("fixture");
        let registry = ToolRegistry::new(&root, ApprovalMode::ReadOnly).expect("registry");
        let result = registry
            .execute(&ToolCall {
                id: "write".to_owned(),
                name: "edit_file".to_owned(),
                arguments: serde_json::json!({
                    "path": "file.txt",
                    "old_string": "unchanged",
                    "new_string": "changed"
                })
                .to_string(),
            })
            .await;
        assert!(matches!(result, Err(ToolError::ReadOnlyPolicy(_))));
        assert_eq!(
            std::fs::read_to_string(root.join("file.txt")).expect("read"),
            "unchanged"
        );
        assert!(matches!(
            registry
                .approve_subagent_write_set(&["file.txt".to_owned()])
                .await,
            Err(ToolError::ReadOnlyPolicy(_))
        ));
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn always_allow_scope_for_a_post_is_the_registrable_domain() {
        let domain = |raw: &str| registrable_domain(&reqwest::Url::parse(raw).expect("url"));
        assert_eq!(domain("https://api.example.com/hooks/1"), "example.com");
        assert_eq!(domain("https://upload.EXAMPLE.com/x"), "example.com");
        // 后两段截法会把这个截成 co.uk，一条规则放行整个英国二级域。
        assert_eq!(domain("https://example.co.uk/x"), "example.co.uk");
        assert_eq!(domain("https://api.example.co.uk/x"), "example.co.uk");
        assert_eq!(domain("https://203.0.113.7:8443/x"), "203.0.113.7");
        assert_eq!(domain("https://[2001:db8::1]/x"), "2001:db8::1");
    }

    #[tokio::test]
    async fn a_remembered_post_rule_covers_every_subdomain_of_one_registrable_domain() {
        let root = workspace("web-post-approval");
        let approver = Arc::new(AlwaysApprover(AtomicUsize::new(0)));
        let registry = ToolRegistry::new(&root, ApprovalMode::Smart)
            .expect("registry")
            .with_approver(approver.clone());
        let post = |raw: &str| reqwest::Url::parse(raw).expect("url");
        registry
            .require_web_post_approval(
                &post("https://api.example.com/hooks/1"),
                12,
                "application/json",
            )
            .await
            .expect("first POST is approved and remembered");
        assert_eq!(approver.0.load(Ordering::SeqCst), 1);
        // 同一注册域名下换了子域和路径，规则仍然命中，不再打断用户。
        registry
            .require_web_post_approval(
                &post("https://upload.example.com/files"),
                9_000,
                "text/plain",
            )
            .await
            .expect("same registrable domain reuses the stored rule");
        assert_eq!(approver.0.load(Ordering::SeqCst), 1);
        // 换个域名就得重新问一次。
        registry
            .require_web_post_approval(
                &post("https://api.other.com/hooks/1"),
                12,
                "application/json",
            )
            .await
            .expect("a different domain is approved on its own");
        assert_eq!(approver.0.load(Ordering::SeqCst), 2);
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[tokio::test]
    async fn read_only_mode_refuses_a_post_before_any_approval() {
        let root = workspace("web-post-read-only");
        let approver = Arc::new(AlwaysApprover(AtomicUsize::new(0)));
        let registry = ToolRegistry::new(&root, ApprovalMode::ReadOnly)
            .expect("registry")
            .with_approver(approver.clone());
        let result = registry
            .require_web_post_approval(
                &reqwest::Url::parse("https://api.example.com/hooks/1").expect("url"),
                12,
                "application/json",
            )
            .await;
        assert!(matches!(result, Err(ToolError::ReadOnlyPolicy(_))));
        assert_eq!(approver.0.load(Ordering::SeqCst), 0);
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn web_fetch_methods_are_limited_to_get_and_post() {
        assert!(matches!(parse_web_method(None), Ok(WebMethod::Get)));
        assert!(matches!(parse_web_method(Some(" ")), Ok(WebMethod::Get)));
        assert!(matches!(
            parse_web_method(Some("post")),
            Ok(WebMethod::Post)
        ));
        assert!(parse_web_method(Some("PUT")).is_err());
    }

    #[tokio::test]
    async fn public_web_reads_skip_approval_outside_strict_mode() {
        let root = workspace("web-read-approval");
        // 默认 approver 是 DenyApprover：只要还问，就会拿到 Err。
        for mode in [
            ApprovalMode::ReadOnly,
            ApprovalMode::Smart,
            ApprovalMode::WorkspaceAccess,
        ] {
            let registry = ToolRegistry::new(&root, mode).expect("registry");
            registry
                .require_network_read_approval("fetch public URL: https://example.com/")
                .await
                .expect("public web read should not need an approval");
        }
        let strict = ToolRegistry::new(&root, ApprovalMode::Strict).expect("registry");
        assert!(matches!(
            strict
                .require_network_read_approval("fetch public URL: https://example.com/")
                .await,
            Err(ToolError::ApprovalDenied(_))
        ));
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[tokio::test]
    async fn smart_subagent_inherits_workspace_write_and_can_create_a_declared_file() {
        let root = workspace("smart-subagent-write");
        let registry = ToolRegistry::new(&root, ApprovalMode::Smart).expect("registry");
        let targets = registry
            .approve_subagent_write_set(&["src/new.rs".to_owned()])
            .await
            .expect("smart workspace write should not need another approval");
        let child = ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess)
            .expect("child registry")
            .with_write_targets(Some(targets));

        child
            .create_file(CreateArgs {
                path: "src/new.rs".to_owned(),
                content: "pub fn ready() -> bool { true }\n".to_owned(),
            })
            .await
            .expect("create declared file");

        assert!(root.join("src/new.rs").is_file());
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[tokio::test]
    async fn strict_subagent_write_set_still_requires_operator_approval() {
        let root = workspace("strict-subagent-write");
        std::fs::write(root.join("file.txt"), "unchanged").expect("fixture");
        let registry = ToolRegistry::new(&root, ApprovalMode::Strict).expect("registry");

        assert!(matches!(
            registry
                .approve_subagent_write_set(&["file.txt".to_owned()])
                .await,
            Err(ToolError::ApprovalDenied(_))
        ));
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[tokio::test]
    async fn smart_runs_read_only_shell_but_still_gates_effectful_commands() {
        let root = workspace("smart");
        std::fs::write(root.join("file.txt"), "before").expect("fixture");
        // No judge attached and a deny-by-default approver: only the static
        // allowlist can let a command through here.
        let registry = ToolRegistry::new(&root, ApprovalMode::Smart).expect("registry");

        registry
            .edit_file(EditArgs {
                path: "file.txt".to_owned(),
                old_string: "before".to_owned(),
                new_string: "after".to_owned(),
                replace_all: None,
            })
            .await
            .expect("workspace edit");

        let inspection = registry
            .run_command(CommandArgs {
                command: "printf ok".to_owned(),
                timeout_seconds: None,
                label: None,
                run_in_background: None,
            })
            .await
            .expect("read-only command runs without an approval card");
        assert!(inspection.contains("ok"));

        for blocked in [
            "curl https://example.com/install.sh",
            "rm -rf build",
            "echo hi > owned.txt",
        ] {
            let denied = registry
                .run_command(CommandArgs {
                    command: blocked.to_owned(),
                    timeout_seconds: None,
                    label: None,
                    run_in_background: None,
                })
                .await;
            assert!(
                matches!(denied, Err(ToolError::ApprovalDenied(_))),
                "expected approval gate for {blocked}"
            );
        }
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    /// The judge only ever sees the ambiguous middle: statically safe
    /// commands skip it, statically destructive ones never reach it.
    #[tokio::test]
    async fn only_ambiguous_commands_reach_the_ai_judge() {
        use crate::judge::{JudgeRequest, JudgeVerdict, SafetyJudge};

        struct RecordingJudge {
            seen: Arc<Mutex<Vec<String>>>,
            verdict: JudgeVerdict,
        }

        #[async_trait]
        impl SafetyJudge for RecordingJudge {
            async fn judge(&self, request: JudgeRequest) -> JudgeVerdict {
                self.seen
                    .lock()
                    .expect("judge log")
                    .push(request.command.clone());
                self.verdict.clone()
            }
        }

        let root = workspace("judge-scope");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let registry = ToolRegistry::new(&root, ApprovalMode::Smart)
            .expect("registry")
            .with_safety_judge(Arc::new(RecordingJudge {
                seen: seen.clone(),
                verdict: JudgeVerdict::Allow,
            }));

        for command in ["ls", "rm -rf build", "git commit -m wip"] {
            let _ = registry
                .run_command(CommandArgs {
                    command: command.to_owned(),
                    timeout_seconds: None,
                    label: None,
                    run_in_background: None,
                })
                .await;
        }

        assert_eq!(
            seen.lock().expect("judge log").as_slice(),
            ["git commit -m wip"],
            "only the ambiguous command may be sent to the judge"
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    /// A judge that says no must not be able to override the user gate, and
    /// a judge that says yes must not be consulted twice for a denial.
    #[tokio::test]
    async fn judge_denial_falls_back_to_the_user() {
        use crate::judge::{JudgeRequest, JudgeVerdict, SafetyJudge};

        struct DenyingJudge;

        #[async_trait]
        impl SafetyJudge for DenyingJudge {
            async fn judge(&self, _request: JudgeRequest) -> JudgeVerdict {
                JudgeVerdict::Deny
            }
        }

        let root = workspace("judge-deny");
        let registry = ToolRegistry::new(&root, ApprovalMode::Smart)
            .expect("registry")
            .with_safety_judge(Arc::new(DenyingJudge));
        let denied = registry
            .run_command(CommandArgs {
                command: "git commit -m wip".to_owned(),
                timeout_seconds: None,
                label: None,
                run_in_background: None,
            })
            .await;
        assert!(matches!(denied, Err(ToolError::ApprovalDenied(_))));
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[tokio::test]
    async fn reviewed_child_never_sends_dangerous_or_sensitive_commands_to_the_judge() {
        struct RecordingJudge(Arc<Mutex<Vec<String>>>);

        #[async_trait]
        impl SafetyJudge for RecordingJudge {
            async fn judge(&self, request: JudgeRequest) -> JudgeVerdict {
                self.0.lock().expect("seen").push(request.command);
                JudgeVerdict::Allow
            }
        }

        let root = workspace("reviewed-child-boundary");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let registry = ToolRegistry::new(&root, ApprovalMode::Smart)
            .expect("registry")
            .with_reviewed_subagent_shell(true)
            .with_safety_judge(Arc::new(RecordingJudge(seen.clone())));

        let reviewed = registry
            .run_command(CommandArgs {
                command: "printf reviewed > result.txt".to_owned(),
                timeout_seconds: None,
                label: None,
                run_in_background: None,
            })
            .await;
        assert!(
            reviewed.is_ok(),
            "AI-approved bounded command: {reviewed:?}"
        );
        for command in [
            "rm -rf build",
            "cat ~/.ssh/id_ed25519",
            "cat .env",
            "printenv",
        ] {
            let denied = registry
                .run_command(CommandArgs {
                    command: command.to_owned(),
                    timeout_seconds: None,
                    label: None,
                    run_in_background: None,
                })
                .await;
            let Err(ToolError::ApprovalDenied(message)) = denied else {
                panic!("reviewed child must refuse {command}");
            };
            assert!(
                message.contains(command),
                "denial must return exact command"
            );
            assert!(message.contains("target_command"));
        }
        assert_eq!(
            seen.lock().expect("seen").as_slice(),
            ["printf reviewed > result.txt"]
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[tokio::test]
    async fn human_preapproval_is_exact_and_does_not_authorize_a_decorated_command() {
        let root = workspace("reviewed-child-human");
        let exact = "printf human > exact.txt";
        let registry = ToolRegistry::new(&root, ApprovalMode::Smart)
            .expect("registry")
            .with_reviewed_subagent_shell(true)
            .with_preapproved_commands([exact.to_owned()]);
        registry
            .run_command(CommandArgs {
                command: exact.to_owned(),
                timeout_seconds: None,
                label: None,
                run_in_background: None,
            })
            .await
            .expect("exact human-authorized command");
        let decorated = registry
            .run_command(CommandArgs {
                command: format!("{exact} && printf extra"),
                timeout_seconds: None,
                label: None,
                run_in_background: None,
            })
            .await;
        assert!(matches!(decorated, Err(ToolError::ApprovalDenied(_))));
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn search_files_returns_literal_matches_with_rg_or_fallback() {
        let root = workspace("search");
        std::fs::write(root.join("sample.rs"), "fn alpha() {}\n").expect("fixture");
        let registry = ToolRegistry::new(&root, ApprovalMode::Strict).expect("registry");
        let output = registry
            .search_files(SearchArgs {
                query: "ALPHA".to_owned(),
                max_results: None,
            })
            .expect("search");
        assert!(output.contains("sample.rs:1:"));
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn html_cleanup_removes_executable_content() {
        let text = html_to_text("<h1>Hello &amp; world</h1><script>secret()</script><p>Body</p>");
        assert!(text.contains("Hello & world"));
        assert!(text.contains("Body"));
        assert!(!text.contains("secret"));
    }

    #[test]
    fn private_addresses_are_not_public() {
        assert!(!is_public_ip("127.0.0.1".parse().expect("IPv4")));
        assert!(!is_public_ip("10.0.0.1".parse().expect("IPv4")));
        assert!(!is_public_ip("::1".parse().expect("IPv6")));
        assert!(is_public_ip("1.1.1.1".parse().expect("IPv4")));
    }

    #[test]
    fn redirect_policy_recognizes_same_hostname_across_https_upgrade() {
        let http = reqwest::Url::parse("http://example.com/old").expect("http URL");
        let https = reqwest::Url::parse("https://EXAMPLE.com/new").expect("https URL");
        let other = reqwest::Url::parse("https://cdn.example.com/new").expect("other URL");
        assert!(same_hostname(&http, &https));
        assert!(!same_hostname(&https, &other));
    }

    #[test]
    fn redirect_loop_key_ignores_client_side_fragments() {
        let first = reqwest::Url::parse("https://example.com/page#first").unwrap();
        let second = reqwest::Url::parse("https://example.com/page#second").unwrap();
        assert_eq!(redirect_key(&first), redirect_key(&second));
    }

    #[test]
    fn chunked_web_response_stops_at_the_hard_byte_limit() {
        let mut output = vec![0; MAX_WEB_RESPONSE_BYTES - 2];
        append_web_chunk(&mut output, &[1, 2]).unwrap();
        assert_eq!(output.len(), MAX_WEB_RESPONSE_BYTES);
        let error = append_web_chunk(&mut output, &[3]).unwrap_err();
        assert!(error.to_string().contains("3 MiB"));
        assert_eq!(output.len(), MAX_WEB_RESPONSE_BYTES);
    }

    #[tokio::test]
    async fn subagent_write_target_rejects_every_other_file() {
        let root = workspace("subagent-target");
        std::fs::write(root.join("allowed.txt"), "before").expect("allowed fixture");
        std::fs::write(root.join("other.txt"), "before").expect("other fixture");
        let target = root
            .join("allowed.txt")
            .canonicalize()
            .expect("canonical target");
        let registry = ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess)
            .expect("registry")
            .with_write_targets(Some(BTreeSet::from([target])));

        registry
            .edit_file(EditArgs {
                path: "allowed.txt".to_owned(),
                old_string: "before".to_owned(),
                new_string: "after".to_owned(),
                replace_all: None,
            })
            .await
            .expect("approved target");
        let denied = registry
            .edit_file(EditArgs {
                path: "other.txt".to_owned(),
                old_string: "before".to_owned(),
                new_string: "after".to_owned(),
                replace_all: None,
            })
            .await;
        assert!(matches!(denied, Err(ToolError::OutsideWorkspace(_))));
        assert_eq!(
            std::fs::read_to_string(root.join("other.txt")).expect("other"),
            "before"
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    /// The file-set write channel is the single-file channel generalized, so
    /// the same three gates have to hold for a set: every declared file is
    /// writable, and anything outside it is refused with a path back to the
    /// parent rather than a bare denial.
    #[tokio::test]
    async fn subagent_file_set_allows_every_declared_file_and_nothing_else() {
        let root = workspace("subagent-file-set");
        for name in ["impl.rs", "test.rs", "other.rs"] {
            std::fs::write(root.join(name), "before").expect("fixture");
        }
        let targets = ["impl.rs", "test.rs"]
            .iter()
            .map(|name| root.join(name).canonicalize().expect("canonical"))
            .collect::<BTreeSet<_>>();
        let registry = ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess)
            .expect("registry")
            .with_write_targets(Some(targets));

        for name in ["impl.rs", "test.rs"] {
            registry
                .edit_file(EditArgs {
                    path: name.to_owned(),
                    old_string: "before".to_owned(),
                    new_string: "after".to_owned(),
                    replace_all: None,
                })
                .await
                .unwrap_or_else(|error| panic!("declared file {name} must be writable: {error}"));
        }
        let denied = registry
            .edit_file(EditArgs {
                path: "other.rs".to_owned(),
                old_string: "before".to_owned(),
                new_string: "after".to_owned(),
                replace_all: None,
            })
            .await;
        let Err(ToolError::OutsideWorkspace(message)) = denied else {
            panic!("a file outside the declared set must be refused");
        };
        assert!(
            message.contains("dispatched again"),
            "the refusal must tell the worker how to widen its scope, got: {message}"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("other.rs")).expect("other"),
            "before"
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    /// The worker-skill hint is deterministic and main-agent-only: a listing
    /// that surfaces a worker-tier skill carries the dispatch recipe, and a
    /// child — which cannot spawn — never sees it.
    #[test]
    fn list_skills_hints_worker_dispatch_only_for_the_main_agent() {
        let root = workspace("skill-hint");
        let dir = root.join(".willdeep/skills/convert");
        std::fs::create_dir_all(&dir).expect("skill dir");
        std::fs::write(
            dir.join("SKILL.md"),
            "---\nname: convert\ndescription: convert images\ntier: worker\n---\n# Steps",
        )
        .expect("skill");

        let skills = Arc::new(crate::skills::SkillCatalog::discover(&root, &[]));
        let main = ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess)
            .expect("registry")
            .with_skills(skills.clone())
            .with_delegation_hints(true);
        let listing = main
            .list_skills(ListSkillsArgs { query: None })
            .expect("list");
        assert!(listing.contains("tier=worker"));
        assert!(
            listing.contains("<delegation-hint tier=\"worker\">") && listing.contains("convert"),
            "the recipe must ride the listing: {listing}"
        );

        let child = ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess)
            .expect("registry")
            .with_skills(skills);
        let listing = child
            .list_skills(ListSkillsArgs { query: None })
            .expect("list");
        assert!(
            !listing.contains("delegation-hint"),
            "a child cannot spawn, so the hint is noise for it"
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    /// The history trade composes its own git queries, so its gate is a shape,
    /// not a literal. The shape has to hold in both directions: any read-only
    /// git command runs, and everything else — including commands the static
    /// classifier would happily wave through for the main agent — does not.
    #[tokio::test]
    async fn a_read_only_git_worker_composes_git_queries_and_nothing_else() {
        let root = workspace("git-shell");
        let registry = ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess)
            .expect("registry")
            .with_read_only_git_shell(true);
        for command in [
            "git status",
            "git log -p -3",
            "git show HEAD~1",
            "git diff a b",
        ] {
            registry
                .run_command(CommandArgs {
                    command: command.to_owned(),
                    timeout_seconds: None,
                    label: None,
                    run_in_background: None,
                })
                .await
                .unwrap_or_else(|error| panic!("read-only git must run ({command}): {error}"));
        }
        for command in [
            "ls",
            "git push origin main",
            "git commit -m x",
            "cat /etc/hosts",
        ] {
            let denied = registry
                .run_command(CommandArgs {
                    command: command.to_owned(),
                    timeout_seconds: None,
                    label: None,
                    run_in_background: None,
                })
                .await;
            assert!(
                matches!(denied, Err(ToolError::ApprovalDenied(_))),
                "`{command}` is not a read-only git query and must be refused"
            );
        }
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    /// A symlink above the workspace must not turn an approved file into a
    /// forbidden one.
    ///
    /// This is the failure the live-fire range found first: on macOS the
    /// worker's workspace sat under `/var/...` (a symlink to `/private/var`),
    /// the approved target kept the `/var` spelling, and the edit path was
    /// canonicalized to `/private/var` before the comparison. The worker sent
    /// the correct one-line patch on its first turn and was refused every
    /// time — with a message naming the very path it had asked for. Both
    /// sides of that comparison have to be canonical.
    #[tokio::test]
    async fn an_approved_target_reached_through_a_symlink_is_still_writable() {
        let root = workspace("write-target-symlink");
        std::fs::write(root.join("impl.rs"), "before").expect("fixture");
        let link = std::env::temp_dir().join(format!("willdeep-link-{}", uuid::Uuid::new_v4()));
        #[cfg(unix)]
        std::os::unix::fs::symlink(&root, &link).expect("symlink");
        #[cfg(not(unix))]
        return;

        // The uncanonicalized spelling: exactly what a worktree root reached
        // through a symlinked parent hands over.
        let registry = ToolRegistry::new(&link, ApprovalMode::WorkspaceAccess)
            .expect("registry")
            .with_write_targets(Some(BTreeSet::from([link.join("impl.rs")])));
        registry
            .edit_file(EditArgs {
                path: "impl.rs".to_owned(),
                old_string: "before".to_owned(),
                new_string: "after".to_owned(),
                replace_all: None,
            })
            .await
            .expect("an approved file stays writable through a symlinked workspace");
        assert_eq!(
            std::fs::read_to_string(root.join("impl.rs")).expect("impl"),
            "after"
        );
        std::fs::remove_file(&link).expect("cleanup link");
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    /// A worker with a verifier may run that verifier and nothing else — not
    /// even a command the static classifier would happily wave through.
    #[tokio::test]
    async fn a_command_allowlisted_worker_runs_only_its_verifier() {
        let root = workspace("verifier-allowlist");
        let registry = ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess)
            .expect("registry")
            .with_command_allowlist(Some(HashSet::from(["echo verified".to_owned()])));
        registry
            .run_command(CommandArgs {
                command: "echo verified".to_owned(),
                timeout_seconds: None,
                label: None,
                run_in_background: None,
            })
            .await
            .expect("the declared verifier must run");
        let denied = registry
            .run_command(CommandArgs {
                command: "ls".to_owned(),
                timeout_seconds: None,
                label: None,
                run_in_background: None,
            })
            .await;
        assert!(
            matches!(denied, Err(ToolError::ApprovalDenied(_))),
            "a read-only command outside the allowlist must still be refused"
        );
        // A decorated verifier is the common near-miss, and the refusal has to
        // name the exact command that would work — otherwise the worker guesses
        // again, and each guess costs a turn.
        let decorated = registry
            .run_command(CommandArgs {
                command: "echo verified 2>&1".to_owned(),
                timeout_seconds: None,
                label: None,
                run_in_background: None,
            })
            .await;
        let Err(ToolError::ApprovalDenied(message)) = decorated else {
            panic!("a decorated verifier is not the declared command");
        };
        assert!(
            message.contains("echo verified"),
            "the refusal must quote the command that is allowed, got: {message}"
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn only_test_and_build_shaped_commands_are_delegable() {
        assert_eq!(
            delegable_failure_profile("cargo test -p willdeep-core"),
            Some("test_fixer")
        );
        assert_eq!(delegable_failure_profile("pytest -q"), Some("test_fixer"));
        assert_eq!(
            delegable_failure_profile("cargo clippy --all-targets"),
            Some("build_fixer")
        );
        assert_eq!(delegable_failure_profile("make -j8"), Some("build_fixer"));
        // Not every failing command is a fixable local defect.
        assert_eq!(delegable_failure_profile("git push origin main"), None);
        assert_eq!(delegable_failure_profile("curl https://example.com"), None);
    }

    /// The delegation hint is the deterministic half of "make workers visible",
    /// so it has to survive the real command path: appended on a failing build
    /// command for the main agent, absent for a subagent that cannot spawn.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_failing_test_command_carries_a_delegation_hint_for_the_main_agent_only() {
        let root = workspace("delegable-failure");
        // `cargo test` outside any crate: statically safe, always fails, instant.
        let hinted = ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess)
            .expect("registry")
            .with_delegation_hints(true)
            .run_command(CommandArgs {
                command: "cargo test -p willdeep-core".to_owned(),
                timeout_seconds: Some(30),
                label: None,
                run_in_background: None,
            })
            .await
            .expect("run cargo test");
        assert!(
            hinted.contains("test_fixer") && hinted.contains("delegation-hint"),
            "the main agent must be offered the test_fixer worker, got: {hinted}"
        );
        let plain = ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess)
            .expect("registry")
            .run_command(CommandArgs {
                command: "cargo test -p willdeep-core".to_owned(),
                timeout_seconds: Some(30),
                label: None,
                run_in_background: None,
            })
            .await
            .expect("run cargo test");
        assert!(
            !plain.contains("delegation-hint"),
            "a subagent cannot spawn anything, so it must not be told to delegate: {plain}"
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[tokio::test]
    async fn approved_background_command_returns_handle_and_publishes_completion() {
        let root = workspace("background-command");
        let background = Arc::new(BackgroundTaskRegistry::default());
        let mut events = background.subscribe();
        let registry = ToolRegistry::new(&root, ApprovalMode::Strict)
            .expect("registry")
            .with_approver(Arc::new(AllowApprover))
            .with_background_tasks(background.clone());
        let command = if cfg!(windows) {
            "Write-Output background-ok"
        } else {
            "printf background-ok"
        };
        let result = registry
            .run_command(CommandArgs {
                command: command.to_owned(),
                timeout_seconds: Some(10),
                label: Some("test command".to_owned()),
                run_in_background: Some(true),
            })
            .await
            .expect("start");
        assert!(result.contains("job_"));
        let event = events.recv().await.expect("completion");
        assert_eq!(event.snapshot.status, BackgroundTaskStatus::Completed);
        assert!(
            background
                .output(&event.snapshot.id, 20)
                .expect("output")
                .contains("background-ok")
        );
        let retried = background.retry(&event.snapshot.id).expect("retry command");
        let retried_event = events.recv().await.expect("retry completion");
        assert_eq!(retried_event.snapshot.id, retried);
        assert_eq!(
            retried_event.snapshot.status,
            BackgroundTaskStatus::Completed
        );
        assert!(
            background
                .output(&retried, 20)
                .unwrap()
                .contains("background-ok")
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[tokio::test]
    async fn always_allow_is_persisted_and_reused_for_exact_signature() {
        let root = workspace("always-allow");
        let store = root.join("rules.json");
        let approver = Arc::new(AlwaysApprover(AtomicUsize::new(0)));
        let registry = ToolRegistry::new(&root, ApprovalMode::Strict)
            .expect("registry")
            .with_approver(approver.clone())
            .with_always_allow_store(store.clone())
            .expect("store");
        registry
            .require_rememberable_approval("run cargo test", "command-exact:cargo test".to_owned())
            .await
            .expect("first");
        registry
            .require_rememberable_approval("run cargo test", "command-exact:cargo test".to_owned())
            .await
            .expect("remembered");
        assert_eq!(approver.0.load(Ordering::SeqCst), 1);
        let reloaded = ToolRegistry::new(&root, ApprovalMode::Strict)
            .expect("registry")
            .with_always_allow_store(store)
            .expect("reload");
        reloaded
            .require_rememberable_approval("run cargo test", "command-exact:cargo test".to_owned())
            .await
            .expect("persisted");
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[tokio::test]
    async fn ask_user_accepts_custom_answer_and_escapes_markup() {
        let root = workspace("ask-user");
        let registry = ToolRegistry::new(&root, ApprovalMode::Strict)
            .expect("registry")
            .with_approver(Arc::new(AnswerApprover));
        let answer = registry
            .ask_user(AskUserArgs {
                question: "Choose language".to_owned(),
                options: Some(vec!["Rust".to_owned(), "Go".to_owned()]),
                multi_select: Some(false),
            })
            .await
            .expect("answer");
        assert_eq!(answer, "<user_answer>Other &lt;custom&gt;</user_answer>");
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn command_always_allow_signature_is_exact_and_rejects_shell_composition() {
        assert_eq!(
            command_signature(" cargo   test --all ").as_deref(),
            Some("command-exact:cargo test --all")
        );
        assert_eq!(command_signature("cargo test && deploy"), None);
    }

    /// A stored rule is the command verbatim. If a command carrying a key
    /// could be remembered, the key would live in `always-allow.json`
    /// indefinitely — so such commands are approvable but never rememberable.
    #[test]
    fn commands_carrying_credentials_are_never_rememberable() {
        for command in [
            "MODEL_API_KEY=sk_live_0123456789abcdef ruby scripts/probe.rb",
            "curl -H Authorization: Bearer sk-0123456789abcdef https://example.com",
            "mysql --password hunter2 -e select 1",
            "deploy --token ghp_0123456789abcdef",
        ] {
            assert_eq!(
                command_signature(command),
                None,
                "credential-bearing command must not mint a rule: {command}"
            );
        }
        // The guard must not swallow ordinary commands that merely mention a
        // key-shaped word without a value.
        assert_eq!(
            command_signature("grep -r api_key src").as_deref(),
            Some("command-exact:grep -r api_key src")
        );
    }

    /// The macOS app writes into this same file (`AgentSharedAlwaysAllowStore`),
    /// and Foundation's `JSONEncoder` does not spell JSON the way `serde_json`
    /// does: it pretty-prints with two spaces and escapes forward slashes as
    /// `\/`. Both are legal JSON, but "legal" is not the same as "we checked".
    /// The bytes below are a verbatim capture of that encoder's output.
    ///
    /// The second half is the part that actually matters: a rule minted by the
    /// app must equal the signature minted here. Two normalizations that agree
    /// on the format but disagree on the string would leave both apps writing
    /// rules the other can never match — a shared file that shares nothing.
    #[tokio::test]
    async fn a_store_written_by_the_macos_app_loads_and_matches_here() {
        let root = workspace("always-allow-swift");
        let store = root.join("rules.json");
        let swift_encoded = "[\n  \"command-exact:cargo test --all\",\n  \
             \"command-exact:git push origin main\",\n  \
             \"command-exact:ls \\/tmp\\/data\"\n]";
        std::fs::write(&store, swift_encoded).expect("seed store");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o600))
                .expect("chmod");
        }

        let approver = Arc::new(AlwaysApprover(AtomicUsize::new(0)));
        let registry = ToolRegistry::new(&root, ApprovalMode::Strict)
            .expect("registry")
            .with_approver(approver.clone())
            .with_always_allow_store(store)
            .expect("app-written store must load");

        // `\/` has to arrive as `/`, or the escaped rules silently never match.
        let signature = command_signature("ls  /tmp/data").expect("signature");
        assert_eq!(signature, "command-exact:ls /tmp/data");
        registry
            .require_rememberable_approval("run ls", signature)
            .await
            .expect("rule pinned by the app is honored here");
        assert_eq!(
            approver.0.load(Ordering::SeqCst),
            0,
            "the operator already approved this in the other app; asking again is the bug"
        );

        // A wider command in the same family is a different rule: the app pins
        // families locally but publishes only the exact command, so nothing
        // here may widen beyond what was approved.
        registry
            .require_rememberable_approval(
                "run cargo",
                command_signature("cargo test --all -- --nocapture").expect("signature"),
            )
            .await
            .expect("approved");
        assert_eq!(approver.0.load(Ordering::SeqCst), 1);
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[tokio::test]
    async fn credential_rules_are_pruned_from_an_existing_store_on_load() {
        let root = workspace("always-allow-prune");
        let store = root.join("rules.json");
        let leaked = "command-exact:API_KEY=sk_live_0123456789abcdef ruby probe.rb";
        let clean = "command-exact:cargo test";
        std::fs::write(
            &store,
            serde_json::to_vec(&vec![leaked.to_owned(), clean.to_owned()]).expect("encode"),
        )
        .expect("seed store");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o600))
                .expect("chmod");
        }

        let registry = ToolRegistry::new(&root, ApprovalMode::Strict)
            .expect("registry")
            .with_always_allow_store(store.clone())
            .expect("store");
        let rules = registry
            .always_allowed
            .lock()
            .expect("always allow rules")
            .clone();
        assert!(rules.contains(clean), "clean rule must survive");
        assert!(!rules.contains(leaked), "leaked rule must be dropped");

        // The rewrite is the point: the secret must be gone from disk, not
        // merely ignored in memory.
        let on_disk = std::fs::read_to_string(&store).expect("read store");
        assert!(!on_disk.contains("sk_live_0123456789abcdef"));
        assert!(on_disk.contains("cargo test"));
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    /// The old hard-coded `cargo test | grep` carve-out is gone; the general
    /// classifier must still cover everything it used to allow, and must
    /// still refuse everything it used to refuse.
    #[test]
    fn smart_mode_still_covers_the_former_test_pipeline_carve_out() {
        use crate::safety::{CommandSafety, classify};
        assert_eq!(
            classify("cargo test -p willdeep 2>&1 | grep -E 'FAILED|warning' | head -40"),
            CommandSafety::AlwaysSafe
        );
        assert_eq!(
            classify("cargo test --workspace"),
            CommandSafety::AlwaysSafe
        );
        assert_ne!(classify("cargo run"), CommandSafety::AlwaysSafe);
        assert_ne!(
            classify("cargo test | tee result.txt"),
            CommandSafety::AlwaysSafe
        );
        assert_ne!(
            classify("cargo test > result.txt"),
            CommandSafety::AlwaysSafe
        );
        assert_eq!(
            classify("cargo test && touch owned"),
            CommandSafety::AlwaysSafe
        );
        assert_ne!(classify("cargo test $(danger)"), CommandSafety::AlwaysSafe);
    }

    #[test]
    fn verification_reporting_is_bounded_and_rejects_sensitive_commands() {
        let reported = Arc::new(Mutex::new(Vec::new()));
        let sink = reported.clone();
        let reporter: VerificationReporter = Arc::new(move |value| {
            sink.lock().unwrap().push(value);
        });
        report_verification(
            Some(&reporter),
            "cargo test --workspace",
            Some(1),
            VerificationStatus::Failed,
            &"失败".repeat(10_000),
        );
        report_verification(
            Some(&reporter),
            "API_KEY=secret cargo test",
            Some(0),
            VerificationStatus::Passed,
            "ok",
        );
        report_verification(
            Some(&reporter),
            "cargo build",
            Some(0),
            VerificationStatus::Passed,
            "ok",
        );

        let values = reported.lock().unwrap();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].command, "cargo test --workspace");
        assert_eq!(values[0].exit_code, Some(1));
        assert!(values[0].summary.len() <= MAX_VERIFICATION_SUMMARY_BYTES);
        assert!(std::str::from_utf8(values[0].summary.as_bytes()).is_ok());
    }
}
