use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;

use crate::agent::{Agent, AgentConfig, AgentError};
use crate::background::{
    BackgroundTaskKind, BackgroundTaskRegistry, BackgroundTaskStatus, TaskResult,
};
use crate::provider::Provider;
use crate::tools::{ApprovalMode, ToolRegistry};

#[derive(Clone)]
pub struct SubagentProfile {
    pub id: String,
    pub purpose: String,
    pub provider: Arc<dyn Provider>,
    pub tool_names: Vec<String>,
    pub capability_prompt: String,
    pub max_turns: usize,
    pub context_window: u64,
    pub requires_write_target: bool,
}

#[derive(Clone)]
pub struct SubagentCatalog {
    workspace: PathBuf,
    profiles: BTreeMap<String, SubagentProfile>,
    background: Arc<BackgroundTaskRegistry>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SpawnAgentArgs {
    pub prompt: String,
    pub label: Option<String>,
    pub profile: Option<String>,
    pub run_in_background: Option<bool>,
    pub target_file: Option<String>,
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
        }
    }

    pub fn description(&self) -> String {
        self.profiles
            .values()
            .map(|profile| format!("`{}` — {}", profile.id, profile.purpose))
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub(crate) fn needs_write_approval(&self, id: Option<&str>) -> bool {
        self.profile(id)
            .is_some_and(|profile| profile.requires_write_target)
    }

    fn profile(&self, id: Option<&str>) -> Option<&SubagentProfile> {
        let id = id.unwrap_or("deep").trim().to_ascii_lowercase();
        self.profiles.get(&id).or_else(|| self.profiles.get("deep"))
    }

    pub(crate) async fn run(
        &self,
        args: SpawnAgentArgs,
        approved_target: Option<PathBuf>,
    ) -> Result<String, AgentError> {
        let profile = self
            .profile(args.profile.as_deref())
            .ok_or_else(|| AgentError::Subagent("no subagent profiles configured".to_owned()))?
            .clone();
        if profile.requires_write_target && approved_target.is_none() {
            return Err(AgentError::Subagent(
                "editor profile requires an approved target_file".to_owned(),
            ));
        }
        let label = args
            .label
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| args.prompt.chars().take(48).collect());
        let workspace = self.workspace.clone();
        let prompt = args.prompt;
        let profile_id = profile.id.clone();
        if args.run_in_background.unwrap_or(false) {
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
            let id = self.background.start_retriable(
                BackgroundTaskKind::Subagent,
                format!("{label} · {profile_id}"),
                move || {
                    let workspace = workspace.clone();
                    let profile = profile.clone();
                    let prompt = prompt.clone();
                    let approved_target = approved_target.clone();
                    async move {
                        match run_profile(workspace, profile, prompt, approved_target).await {
                            Ok(report) => TaskResult {
                                status: BackgroundTaskStatus::Completed,
                                exit_code: Some(0),
                                output: report,
                            },
                            Err(error) => TaskResult {
                                status: BackgroundTaskStatus::Failed,
                                exit_code: Some(1),
                                output: error.to_string(),
                            },
                        }
                    }
                },
            );
            Ok(format!(
                "Subagent started: {id}. Its report will be delivered automatically to the main harness."
            ))
        } else {
            run_profile(workspace, profile, prompt, approved_target).await
        }
    }
}

async fn run_profile(
    workspace: PathBuf,
    profile: SubagentProfile,
    prompt: String,
    approved_target: Option<PathBuf>,
) -> Result<String, AgentError> {
    let approval = if approved_target.is_some() {
        ApprovalMode::WorkspaceAccess
    } else {
        ApprovalMode::Strict
    };
    let tools = ToolRegistry::new(&workspace, approval)?
        .with_allowed_tools(profile.tool_names.clone())
        .with_write_target(approved_target);
    let system_prompt = format!(
        "You are a WillDeep subagent working in {}. You do not see the parent conversation, cannot ask the user, and cannot spawn another agent. Your final response is the report returned to the parent.\n\n{}",
        workspace.display(),
        profile.capability_prompt
    );
    let agent = Agent::new(
        profile.provider,
        tools,
        AgentConfig {
            max_turns: profile.max_turns,
            system_prompt,
            context_window: profile.context_window,
        },
    );
    Ok(Box::pin(agent.run(prompt)).await?.final_text)
}

pub fn builtin_profiles(
    parent: Arc<dyn Provider>,
    cheap: Arc<dyn Provider>,
    context_window: u64,
) -> Vec<SubagentProfile> {
    vec![
        profile(
            cheap.clone(),
            context_window,
            ProfileSpec {
                id: "scout",
                purpose: "Locate files, symbols and call sites quickly; no shell or writes.",
                tools: &["search_files", "grep_files", "list_directory", "read_file"],
                prompt: "Your trade is LOCATION. Report exact paths and line numbers; do not redesign.",
                max_turns: 8,
                requires_write_target: false,
            },
        ),
        profile(
            cheap.clone(),
            context_window,
            ProfileSpec {
                id: "reader",
                purpose: "Read and summarize long files or documentation; no shell or writes.",
                tools: &["read_file", "list_directory", "search_files"],
                prompt: "Your trade is READING. Answer with specific evidence and say which parts you read.",
                max_turns: 8,
                requires_write_target: false,
            },
        ),
        profile(
            parent,
            context_window,
            ProfileSpec {
                id: "deep",
                purpose: "Complex investigation across files and repository state.",
                tools: &[
                    "search_files",
                    "grep_files",
                    "read_file",
                    "list_directory",
                    "git_status",
                ],
                prompt: "Your trade is INVESTIGATION. Follow evidence across files and state what you could not confirm.",
                max_turns: 12,
                requires_write_target: false,
            },
        ),
        profile(
            cheap,
            context_window,
            ProfileSpec {
                id: "editor",
                purpose: "Edit exactly one separately approved target_file.",
                tools: &["read_file", "edit_file"],
                prompt: "Your trade is EDITING EXACTLY ONE FILE. Read it first, make a minimal exact edit, and touch no other path.",
                max_turns: 6,
                requires_write_target: true,
            },
        ),
    ]
}

struct ProfileSpec<'a> {
    id: &'a str,
    purpose: &'a str,
    tools: &'a [&'a str],
    prompt: &'a str,
    max_turns: usize,
    requires_write_target: bool,
}

fn profile(
    provider: Arc<dyn Provider>,
    context_window: u64,
    spec: ProfileSpec<'_>,
) -> SubagentProfile {
    SubagentProfile {
        id: spec.id.to_owned(),
        purpose: spec.purpose.to_owned(),
        provider,
        tool_names: spec.tools.iter().map(|value| (*value).to_owned()).collect(),
        capability_prompt: spec.prompt.to_owned(),
        max_turns: spec.max_turns,
        context_window,
        requires_write_target: spec.requires_write_target,
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;

    use super::*;
    use crate::provider::ProviderError;
    use crate::types::{Completion, Message, ToolDefinition};

    struct ReportProvider;
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

    fn fixture() -> (SubagentCatalog, PathBuf) {
        let root = std::env::temp_dir().join(format!("willdeep-subagent-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("workspace");
        let provider: Arc<dyn Provider> = Arc::new(ReportProvider);
        let background = Arc::new(BackgroundTaskRegistry::default());
        (
            SubagentCatalog::new(
                &root,
                builtin_profiles(provider.clone(), provider, 128_000),
                background,
            ),
            root,
        )
    }

    #[tokio::test]
    async fn foreground_profile_runs_in_isolated_non_nested_agent() {
        let (catalog, root) = fixture();
        let report = catalog
            .run(
                SpawnAgentArgs {
                    prompt: "inspect".to_owned(),
                    label: None,
                    profile: Some("scout".to_owned()),
                    run_in_background: Some(false),
                    target_file: None,
                },
                None,
            )
            .await
            .expect("run");
        assert_eq!(report, "subagent report");
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
                },
                None,
            )
            .await;
        assert!(matches!(result, Err(AgentError::Subagent(_))));
        std::fs::remove_dir_all(root).expect("cleanup");
    }
}
