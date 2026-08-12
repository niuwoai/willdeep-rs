use std::path::{Path, PathBuf};

use tokio::process::Command;

use crate::agent::AgentError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubagentWorktreePolicy {
    Shared,
    Dedicated,
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedSubagentWorkspace {
    pub workspace: PathBuf,
    pub root_workspace: PathBuf,
    pub branch: Option<String>,
    pub dedicated: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct SubagentWorktreeManager {
    root: PathBuf,
}

pub(crate) async fn worktree_result_note(
    prepared: &PreparedSubagentWorkspace,
) -> Result<Option<String>, AgentError> {
    if !prepared.dedicated {
        return Ok(None);
    }
    let status = git_stdout(&prepared.workspace, &["status", "--short"]).await?;
    let branch = prepared.branch.as_deref().unwrap_or("unknown");
    let status = bounded_status(status.trim(), 16 * 1024);
    Ok(Some(format!(
        "[WillDeep Worktree]\nworkspace: {}\nbranch: {}\nchanges:\n{}",
        prepared.workspace.display(),
        branch,
        if status.is_empty() { "(none)" } else { &status }
    )))
}

fn bounded_status(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}\n[status truncated]", &value[..boundary])
}

impl SubagentWorktreeManager {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub(crate) async fn prepare(
        &self,
        workspace: &Path,
        agent_id: uuid::Uuid,
        policy: SubagentWorktreePolicy,
    ) -> Result<PreparedSubagentWorkspace, AgentError> {
        if policy == SubagentWorktreePolicy::Shared {
            return Ok(shared_workspace(workspace));
        }

        let repository = git_stdout(workspace, &["rev-parse", "--show-toplevel"]).await?;
        let repository = PathBuf::from(repository.trim());
        let canonical_workspace = workspace.canonicalize().map_err(|error| {
            AgentError::Subagent(format!(
                "resolve subagent workspace {}: {error}",
                workspace.display()
            ))
        })?;
        let canonical_repository = repository.canonicalize().map_err(|error| {
            AgentError::Subagent(format!(
                "resolve Git repository {}: {error}",
                repository.display()
            ))
        })?;
        if !canonical_workspace.starts_with(&canonical_repository) {
            return Err(AgentError::Subagent(format!(
                "workspace {} is outside Git repository {}",
                canonical_workspace.display(),
                canonical_repository.display()
            )));
        }

        let short_id = agent_id.simple().to_string();
        let branch = format!("willdeep/agent-{}", &short_id[..12]);
        let target = self.root.join(agent_id.to_string());
        tokio::fs::create_dir_all(&self.root)
            .await
            .map_err(|error| {
                AgentError::Subagent(format!(
                    "create subagent worktree root {}: {error}",
                    self.root.display()
                ))
            })?;
        let output = Command::new("git")
            .args(["worktree", "add", "-b", &branch])
            .arg(&target)
            .current_dir(&canonical_repository)
            .output()
            .await
            .map_err(|error| AgentError::Subagent(format!("launch git worktree add: {error}")))?;
        if !output.status.success() {
            return Err(AgentError::Subagent(format!(
                "create dedicated subagent worktree: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }

        let relative_workspace = canonical_workspace
            .strip_prefix(&canonical_repository)
            .expect("workspace containment checked");
        Ok(PreparedSubagentWorkspace {
            workspace: target.join(relative_workspace),
            root_workspace: canonical_workspace,
            branch: Some(branch),
            dedicated: true,
        })
    }
}

fn shared_workspace(workspace: &Path) -> PreparedSubagentWorkspace {
    PreparedSubagentWorkspace {
        workspace: workspace.to_path_buf(),
        root_workspace: workspace.to_path_buf(),
        branch: None,
        dedicated: false,
    }
}

async fn git_stdout(workspace: &Path, args: &[&str]) -> Result<String, AgentError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .await
        .map_err(|error| AgentError::Subagent(format!("launch git: {error}")))?;
    if !output.status.success() {
        return Err(AgentError::Subagent(format!(
            "dedicated subagent worktree requires a Git repository: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use std::process::Command as StdCommand;

    use super::*;

    #[tokio::test]
    async fn dedicated_workspace_creates_isolated_branch_and_path() {
        let root = std::env::temp_dir().join(format!(
            "willdeep-subagent-worktree-{}",
            uuid::Uuid::new_v4()
        ));
        let repository = root.join("repository");
        let worktrees = root.join("managed");
        std::fs::create_dir_all(&repository).expect("repository");
        git(&repository, &["init"]);
        std::fs::write(repository.join("tracked.txt"), "root\n").expect("tracked file");
        git(&repository, &["add", "tracked.txt"]);
        git(
            &repository,
            &[
                "-c",
                "user.name=WillDeep Test",
                "-c",
                "user.email=willdeep@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );

        let agent_id = uuid::Uuid::new_v4();
        let prepared = SubagentWorktreeManager::new(worktrees)
            .prepare(&repository, agent_id, SubagentWorktreePolicy::Dedicated)
            .await
            .expect("prepare dedicated worktree");
        assert!(prepared.dedicated);
        assert_eq!(prepared.root_workspace, repository.canonicalize().unwrap());
        assert!(prepared.workspace.join("tracked.txt").is_file());
        assert_eq!(
            prepared.branch.as_deref(),
            Some(format!("willdeep/agent-{}", &agent_id.simple().to_string()[..12]).as_str())
        );

        git(
            &repository,
            &[
                "worktree",
                "remove",
                "--force",
                prepared.workspace.to_str().unwrap(),
            ],
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[tokio::test]
    async fn two_dedicated_worktrees_isolate_parallel_edits_from_each_other_and_root() {
        let root = std::env::temp_dir().join(format!(
            "willdeep-parallel-worktrees-{}",
            uuid::Uuid::new_v4()
        ));
        let repository = root.join("repository");
        std::fs::create_dir_all(&repository).expect("repository");
        git(&repository, &["init"]);
        std::fs::write(repository.join("tracked.txt"), "root\n").expect("tracked file");
        git(&repository, &["add", "tracked.txt"]);
        git(
            &repository,
            &[
                "-c",
                "user.name=WillDeep Test",
                "-c",
                "user.email=willdeep@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        let manager = SubagentWorktreeManager::new(root.join("managed"));
        let first = manager
            .prepare(
                &repository,
                uuid::Uuid::new_v4(),
                SubagentWorktreePolicy::Dedicated,
            )
            .await
            .unwrap();
        let second = manager
            .prepare(
                &repository,
                uuid::Uuid::new_v4(),
                SubagentWorktreePolicy::Dedicated,
            )
            .await
            .unwrap();
        std::fs::write(first.workspace.join("tracked.txt"), "first\n").unwrap();
        std::fs::write(second.workspace.join("tracked.txt"), "second\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(repository.join("tracked.txt")).unwrap(),
            "root\n"
        );
        assert_eq!(
            std::fs::read_to_string(first.workspace.join("tracked.txt")).unwrap(),
            "first\n"
        );
        assert_eq!(
            std::fs::read_to_string(second.workspace.join("tracked.txt")).unwrap(),
            "second\n"
        );
        for prepared in [first, second] {
            git(
                &repository,
                &[
                    "worktree",
                    "remove",
                    "--force",
                    prepared.workspace.to_str().unwrap(),
                ],
            );
        }
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    fn git(repository: &Path, args: &[&str]) {
        let output = StdCommand::new("git")
            .args(args)
            .current_dir(repository)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
