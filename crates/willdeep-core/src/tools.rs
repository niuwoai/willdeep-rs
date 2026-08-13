use std::collections::HashSet;
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
use crate::judge::{JudgeRequest, JudgeVerdict, SafetyJudge};
use crate::safety::CommandSafety;
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
const MAX_VERIFICATION_SUMMARY_BYTES: usize = 8 * 1024;

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
    allowed_tools: Option<HashSet<String>>,
    write_target: Option<PathBuf>,
    always_allowed: Arc<Mutex<HashSet<String>>>,
    always_allow_path: Option<PathBuf>,
    verification_reporter: Option<VerificationReporter>,
    safety_judge: Option<Arc<dyn SafetyJudge>>,
    task_context: Arc<Mutex<String>>,
    approval_reporter: Option<ApprovalReporter>,
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
            write_target: None,
            always_allowed: Arc::new(Mutex::new(HashSet::new())),
            always_allow_path: None,
            verification_reporter: None,
            safety_judge: None,
            task_context: Arc::new(Mutex::new(String::new())),
            approval_reporter: None,
        })
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
        let rules = if path.exists() {
            serde_json::from_str::<Vec<String>>(&std::fs::read_to_string(&path)?).map_err(
                |error| ToolError::Network(format!("invalid always-allow store: {error}")),
            )?
        } else {
            Vec::new()
        };
        self.always_allowed = Arc::new(Mutex::new(rules.into_iter().collect()));
        self.always_allow_path = Some(path);
        Ok(self)
    }
    pub fn with_allowed_tools(mut self, names: impl IntoIterator<Item = String>) -> Self {
        self.allowed_tools = Some(names.into_iter().collect());
        self
    }
    pub fn with_write_target(mut self, target: Option<PathBuf>) -> Self {
        self.write_target = target;
        self
    }

    pub async fn approve_subagent_editor(&self, requested: &str) -> Result<PathBuf, ToolError> {
        if self.approval_mode == ApprovalMode::ReadOnly {
            return Err(ToolError::ReadOnlyPolicy("editor subagent".to_owned()));
        }
        let target = self.resolve_existing(requested)?;
        if !target.is_file() {
            return Err(ToolError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "editor target is not a file",
            )));
        }
        self.require_approval(
            &format!(
                "allow editor subagent to modify exactly: {}",
                display_relative(&self.workspace, &target)
            ),
            false,
        )
        .await?;
        Ok(target)
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
                "Delegate a self-contained task to a child agent with an isolated context. Profiles: scout (locate), reader (summarize), deep (investigate), editor (one separately approved file). Children cannot spawn more agents.",
                json!({"type":"object","properties":{"prompt":{"type":"string"},"label":{"type":"string"},"profile":{"type":"string","enum":["scout","reader","deep","editor"]},"run_in_background":{"type":"boolean"},"target_file":{"type":"string"}},"required":["prompt"],"additionalProperties":false}),
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
                "Fetch readable text from a public HTTP(S) URL. Safe same-host redirects are followed automatically; cross-host redirects require approval. Private, loopback and link-local targets are refused.",
                json!({"type":"object","properties":{"url":{"type":"string"},"max_chars":{"type":"integer","minimum":1,"maximum":100000}},"required":["url"],"additionalProperties":false}),
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
        tools.extend(self.mcp.definitions());
        if let Some(allowed) = &self.allowed_tools {
            tools.retain(|tool| allowed.contains(&tool.name));
        }
        tools
    }

    pub async fn execute(&self, call: &ToolCall) -> Result<String, ToolError> {
        if self.approval_mode == ApprovalMode::ReadOnly
            && (matches!(
                call.name.as_str(),
                "run_command" | "create_file" | "edit_file" | "create_worktree"
            ) || self.mcp.handles(&call.name))
        {
            return Err(ToolError::ReadOnlyPolicy(call.name.clone()));
        }
        match call.name.as_str() {
            "list_skills" => self.list_skills(parse(call)?),
            "read_skill" => self.read_skill(parse(call)?),
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
            .map(|s| format!("- {} | name={} | {}", s.identifier, s.name, s.description))
            .collect::<Vec<_>>();
        Ok(if lines.is_empty() {
            "No installed skills found.".to_owned()
        } else {
            lines.join("\n")
        })
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
            .clamp(1, MAX_READ_BYTES);
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
        git_output(output)
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
        git_output(output)
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
        self.require_approval(&format!("search the public web for: {query}"), false)
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
        let mut url = reqwest::Url::parse(args.url.trim())
            .map_err(|error| ToolError::Network(format!("invalid URL: {error}")))?;
        validate_public_url(&url).await?;
        self.require_approval(&format!("fetch public URL: {url}"), false)
            .await?;
        let client = web_client()?;
        let mut redirects = 0;
        let mut visited = HashSet::new();
        let response = loop {
            if !visited.insert(redirect_key(&url)) {
                return Err(ToolError::Network("redirect loop detected".to_owned()));
            }
            let response = client
                .get(url.clone())
                .send()
                .await
                .map_err(|error| ToolError::Network(error.to_string()))?;
            if !response.status().is_redirection() {
                break response;
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
                self.require_approval(
                    &format!("redirect web_fetch from {url} to different host: {next}"),
                    false,
                )
                .await?;
            }
            url = next;
            redirects += 1;
        };
        if !response.status().is_success() {
            return Err(ToolError::Network(format!(
                "web server returned HTTP {}",
                response.status()
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
        let mut command = platform_shell(&args.command);
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
        Ok(truncate_bytes(text))
    }

    async fn create_file(&self, args: CreateArgs) -> Result<String, ToolError> {
        self.require_write_target(&args.path)?;
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
        self.require_write_target(&args.path)?;
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
        let escalate = |registry: &Self, detail: String| {
            registry.report_approval(command, ApprovalSource::User, detail);
        };
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
        match verdict {
            JudgeVerdict::Allow => {
                self.report_approval(
                    command,
                    ApprovalSource::Judge,
                    "AI review: bounded and consistent with the current task".to_owned(),
                );
                Ok(())
            }
            JudgeVerdict::Deny => {
                escalate(self, "AI review declined".to_owned());
                self.ask_for_command(command, description).await
            }
            JudgeVerdict::Unavailable(reason) => {
                escalate(self, format!("AI review unavailable: {reason}"));
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

    fn require_write_target(&self, requested: &str) -> Result<(), ToolError> {
        if let Some(target) = &self.write_target {
            let resolved = self.resolve_existing(requested)?;
            if &resolved != target {
                return Err(ToolError::OutsideWorkspace(format!(
                    "subagent may only edit {}",
                    display_relative(&self.workspace, target)
                )));
            }
        }
        Ok(())
    }

    fn get_job_output(&self, args: JobOutputArgs) -> Result<String, ToolError> {
        self.background
            .output(&args.job_id, args.tail_lines.unwrap_or(200).clamp(1, 2_000))
            .ok_or_else(|| {
                ToolError::Network(format!("background task not found: {}", args.job_id))
            })
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
    let output = truncate_bytes(output);
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

fn command_signature(command: &str) -> Option<String> {
    if command
        .chars()
        .any(|value| matches!(value, '|' | '&' | ';' | '>' | '<' | '`' | '\n' | '\r'))
    {
        return None;
    }
    let normalized = command.split_whitespace().collect::<Vec<_>>().join(" ");
    (!normalized.is_empty()).then(|| format!("command-exact:{normalized}"))
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

async fn validate_public_url(url: &reqwest::Url) -> Result<(), ToolError> {
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

fn truncate_bytes(value: String) -> String {
    if value.len() <= MAX_COMMAND_OUTPUT_BYTES {
        return value;
    }
    let mut boundary = MAX_COMMAND_OUTPUT_BYTES;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}\n[output truncated]", &value[..boundary])
}

fn git_output(output: std::process::Output) -> Result<String, ToolError> {
    if !output.status.success() {
        return Err(ToolError::Io(std::io::Error::other(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        )));
    }
    Ok(truncate_bytes(
        String::from_utf8_lossy(&output.stdout).into_owned(),
    ))
}

#[cfg(windows)]
fn platform_shell(command: &str) -> Command {
    let mut process = Command::new("powershell.exe");
    process.args(["-NoProfile", "-NonInteractive", "-Command", command]);
    process
}

#[cfg(not(windows))]
fn platform_shell(command: &str) -> Command {
    let mut process = Command::new("/bin/sh");
    process.args(["-lc", command]);
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
            registry.approve_subagent_editor("file.txt").await,
            Err(ToolError::ReadOnlyPolicy(_))
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
            .with_write_target(Some(target));

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
