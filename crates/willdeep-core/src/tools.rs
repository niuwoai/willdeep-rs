use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use globset::Glob;
use ignore::WalkBuilder;
use regex::RegexBuilder;
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::background::{
    BackgroundTaskKind, BackgroundTaskRegistry, BackgroundTaskStatus, TaskResult,
};
use crate::types::{ToolCall, ToolDefinition};
use crate::{McpRegistry, SkillCatalog};

const DEFAULT_MAX_RESULTS: usize = 60;
const MAX_RESULTS: usize = 200;
const DEFAULT_READ_BYTES: usize = 64 * 1024;
const MAX_READ_BYTES: usize = 256 * 1024;
const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 60;
const MAX_COMMAND_TIMEOUT_SECS: u64 = 600;
const MAX_COMMAND_OUTPUT_BYTES: usize = 128 * 1024;
const MAX_WEB_RESPONSE_BYTES: usize = 3 * 1024 * 1024;
const DEFAULT_WEB_MAX_CHARS: usize = 20_000;
const MAX_WEB_MAX_CHARS: usize = 100_000;
const MAX_WEB_REDIRECTS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalMode {
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
        })
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
        match call.name.as_str() {
            "list_skills" => self.list_skills(parse(call)?),
            "read_skill" => self.read_skill(parse(call)?),
            "search_files" => self.search_files(parse(call)?),
            "grep_files" => self.grep_files(parse(call)?),
            "read_file" => self.read_file(parse(call)?).await,
            "list_directory" => self.list_directory(parse(call)?).await,
            "git_status" => self.git_status().await,
            "git_diff" => self.git_diff(parse(call)?).await,
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
        let response = loop {
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
        let bytes = response
            .bytes()
            .await
            .map_err(|error| ToolError::Network(error.to_string()))?;
        if bytes.len() > MAX_WEB_RESPONSE_BYTES {
            return Err(ToolError::Network(
                "response exceeds the 3 MiB limit".to_owned(),
            ));
        }
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
        if let Some(signature) = command_signature(&args.command) {
            self.require_rememberable_approval(&format!("run command: {description}"), signature)
                .await?;
        } else {
            self.require_approval(&format!("run command: {description}"), false)
                .await?;
        }
        let timeout = args
            .timeout_seconds
            .unwrap_or(DEFAULT_COMMAND_TIMEOUT_SECS)
            .clamp(1, MAX_COMMAND_TIMEOUT_SECS);
        if args.run_in_background.unwrap_or(false) {
            let command = args.command;
            let workspace = self.workspace.clone();
            let id = self
                .background
                .start(BackgroundTaskKind::Shell, description, async move {
                    let mut process = platform_shell(&command);
                    process
                        .current_dir(workspace)
                        .stdin(Stdio::null())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .kill_on_drop(true);
                    let output =
                        match tokio::time::timeout(std::time::Duration::from_secs(timeout), async {
                            process.spawn()?.wait_with_output().await
                        })
                        .await
                        {
                            Ok(Ok(output)) => output,
                            Ok(Err(error)) => {
                                return TaskResult {
                                    status: BackgroundTaskStatus::LaunchFailed,
                                    exit_code: Some(-1),
                                    output: error.to_string(),
                                };
                            }
                            Err(_) => {
                                return TaskResult {
                                    status: BackgroundTaskStatus::TimedOut,
                                    exit_code: None,
                                    output: format!("command timed out after {timeout} seconds"),
                                };
                            }
                        };
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
                });
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
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(timeout),
            child.wait_with_output(),
        )
        .await
        .map_err(|_| ToolError::CommandTimeout(timeout))??;
        Ok(truncate_bytes(format!(
            "exit_code: {}\nstdout:\n{}\nstderr:\n{}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )))
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
    async fn smart_allows_workspace_edit_but_not_shell() {
        let root = workspace("smart");
        std::fs::write(root.join("file.txt"), "before").expect("fixture");
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
        let command = registry
            .run_command(CommandArgs {
                command: "printf should-not-run".to_owned(),
                timeout_seconds: None,
                label: None,
                run_in_background: None,
            })
            .await;

        assert!(matches!(command, Err(ToolError::ApprovalDenied(_))));
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
}
