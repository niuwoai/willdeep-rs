use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use globset::Glob;
use ignore::WalkBuilder;
use regex::RegexBuilder;
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::types::{ToolCall, ToolDefinition};
use crate::{McpRegistry, SkillCatalog};

const DEFAULT_MAX_RESULTS: usize = 60;
const MAX_RESULTS: usize = 200;
const DEFAULT_READ_BYTES: usize = 64 * 1024;
const MAX_READ_BYTES: usize = 256 * 1024;
const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 60;
const MAX_COMMAND_TIMEOUT_SECS: u64 = 600;
const MAX_COMMAND_OUTPUT_BYTES: usize = 128 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalMode {
    Strict,
    Smart,
    WorkspaceAccess,
}

#[async_trait]
pub trait Approver: Send + Sync {
    async fn approve(&self, description: &str) -> bool;
}

struct DenyApprover;

#[async_trait]
impl Approver for DenyApprover {
    async fn approve(&self, _description: &str) -> bool {
        false
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
                "run_command",
                "Run a shell command in the workspace root and return exit code, stdout, and stderr. Requires approval.",
                json!({
                    "type": "object",
                    "properties": {
                        "command": {"type": "string", "description": "Shell command line."},
                        "timeout_seconds": {"type": "integer", "minimum": 1, "maximum": 600},
                        "label": {"type": "string", "description": "Optional concise action label; never include secrets."}
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
            "run_command" => self.run_command(parse(call)?).await,
            "create_file" => self.create_file(parse(call)?).await,
            "edit_file" => self.edit_file(parse(call)?).await,
            name if self.mcp.handles(name) => {
                self.require_approval(&format!("call MCP tool: {name}"), false)
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
        self.search(
            args.path.as_deref(),
            include.as_ref(),
            args.max_results,
            |line| regex.is_match(line),
        )
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

    async fn run_command(&self, args: CommandArgs) -> Result<String, ToolError> {
        let description = args
            .label
            .as_deref()
            .filter(|label| !label.trim().is_empty())
            .map(|label| format!("{label}\ncommand: {}", args.command))
            .unwrap_or_else(|| args.command.clone());
        self.require_approval(&format!("run command: {description}"), false)
            .await?;
        let timeout = args
            .timeout_seconds
            .unwrap_or(DEFAULT_COMMAND_TIMEOUT_SECS)
            .clamp(1, MAX_COMMAND_TIMEOUT_SECS);
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
        if workspace_write_allowed || self.approver.approve(description).await {
            Ok(())
        } else {
            Err(ToolError::ApprovalDenied(description.to_owned()))
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
            })
            .await;

        assert!(matches!(command, Err(ToolError::ApprovalDenied(_))));
        std::fs::remove_dir_all(root).expect("cleanup");
    }
}
