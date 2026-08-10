use std::collections::BTreeSet;
use std::process::Command;

use super::agent_store::RuntimeAgent;
use super::*;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ManagedWorktreeState {
    Active,
    Reviewable,
    Merged,
    Clean,
    Quarantined,
    Missing,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ManagedWorktreeAudit {
    pub agent_id: Option<uuid::Uuid>,
    pub path: PathBuf,
    pub branch: Option<String>,
    pub state: ManagedWorktreeState,
    pub child_snapshot_id: Option<String>,
    pub changed_files: usize,
    pub quarantine_allowed: bool,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct WorktreeQuarantineRequest {
    pub child_snapshot_id: String,
    pub confirm: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct WorktreeQuarantineResult {
    pub agent_id: uuid::Uuid,
    pub original_path: PathBuf,
    pub quarantine_path: PathBuf,
    pub branch_retained: Option<String>,
}

pub(crate) async fn audit_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<ManagedWorktreeAudit>>, StatusCode> {
    authorize(&state, &headers)?;
    audit(&state.home, &state.tasks.agents)
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub(crate) async fn quarantine_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<uuid::Uuid>,
    Json(request): Json<WorktreeQuarantineRequest>,
) -> Result<Json<WorktreeQuarantineResult>, StatusCode> {
    authorize(&state, &headers)?;
    if !request.confirm {
        return Err(StatusCode::BAD_REQUEST);
    }
    quarantine(
        &state.home,
        &state.tasks.agents,
        id,
        &request.child_snapshot_id,
    )
    .map(Json)
    .map_err(quarantine_error_status)
}

pub(super) fn unified_audit(
    state: &ServerState,
) -> Result<Vec<willdeep_runtime_protocol::RuntimeWorktreeAudit>, StatusCode> {
    audit(&state.home, &state.tasks.agents)
        .map(|entries| entries.into_iter().map(public_audit).collect())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub(super) fn unified_quarantine(
    state: &ServerState,
    params: willdeep_runtime_protocol::WorktreeQuarantineParams,
) -> Result<willdeep_runtime_protocol::RuntimeWorktreeQuarantineResult, StatusCode> {
    if !params.confirm {
        return Err(StatusCode::BAD_REQUEST);
    }
    quarantine(
        &state.home,
        &state.tasks.agents,
        params.agent_id,
        &params.child_snapshot_id,
    )
    .map(
        |result| willdeep_runtime_protocol::RuntimeWorktreeQuarantineResult {
            agent_id: result.agent_id,
            original_path: Some(result.original_path.display().to_string()),
            quarantine_path: Some(result.quarantine_path.display().to_string()),
            branch_retained: result.branch_retained,
        },
    )
    .map_err(quarantine_error_status)
}

fn public_audit(audit: ManagedWorktreeAudit) -> willdeep_runtime_protocol::RuntimeWorktreeAudit {
    willdeep_runtime_protocol::RuntimeWorktreeAudit {
        agent_id: audit.agent_id,
        path: Some(audit.path.display().to_string()),
        branch: audit.branch,
        state: match audit.state {
            ManagedWorktreeState::Active => willdeep_runtime_protocol::ManagedWorktreeState::Active,
            ManagedWorktreeState::Reviewable => {
                willdeep_runtime_protocol::ManagedWorktreeState::Reviewable
            }
            ManagedWorktreeState::Merged => willdeep_runtime_protocol::ManagedWorktreeState::Merged,
            ManagedWorktreeState::Clean => willdeep_runtime_protocol::ManagedWorktreeState::Clean,
            ManagedWorktreeState::Quarantined => {
                willdeep_runtime_protocol::ManagedWorktreeState::Quarantined
            }
            ManagedWorktreeState::Missing => {
                willdeep_runtime_protocol::ManagedWorktreeState::Missing
            }
            ManagedWorktreeState::Unknown => {
                willdeep_runtime_protocol::ManagedWorktreeState::Unknown
            }
        },
        child_snapshot_id: audit.child_snapshot_id,
        changed_files: audit.changed_files,
        quarantine_allowed: audit.quarantine_allowed,
        reason: audit.reason,
    }
}

pub(crate) async fn audit_cli(home: &Path) -> Result<()> {
    let state = ensure_running(home).await?;
    let entries = runtime_client(&state)?
        .audit_worktrees()
        .await?
        .into_result()?;
    for entry in entries {
        println!(
            "{}\t{:?}\tfiles={}\tquarantine={}\tsnapshot={}\t{}\t{}",
            entry
                .agent_id
                .map_or_else(|| "—".to_owned(), |id| id.to_string()),
            entry.state,
            entry.changed_files,
            entry.quarantine_allowed,
            entry.child_snapshot_id.as_deref().unwrap_or("—"),
            entry.path.as_deref().unwrap_or("—"),
            entry.reason
        );
    }
    Ok(())
}

pub(crate) async fn quarantine_cli(
    home: &Path,
    id: uuid::Uuid,
    child_snapshot_id: String,
    yes: bool,
) -> Result<()> {
    if !yes {
        bail!(
            "quarantine requires explicit --yes and the exact child snapshot ID from worktrees-audit"
        );
    }
    let state = ensure_running(home).await?;
    let result = (runtime_client(&state)?
        .quarantine_worktree(
            &willdeep_runtime_protocol::WorktreeQuarantineParams {
                agent_id: id,
                child_snapshot_id,
                confirm: true,
            },
            uuid::Uuid::new_v4(),
        )
        .await?)
        .into_result()?;
    println!(
        "quarantined\tagent={}\tfrom={}\tto={}\tbranch_retained={}",
        result.agent_id,
        result.original_path.as_deref().unwrap_or("—"),
        result.quarantine_path.as_deref().unwrap_or("—"),
        result.branch_retained.as_deref().unwrap_or("—")
    );
    Ok(())
}

fn audit(home: &Path, agents: &AgentStore) -> Result<Vec<ManagedWorktreeAudit>> {
    let managed_root = managed_root(home);
    let known = agents
        .list()?
        .into_iter()
        .filter(|agent| agent.dedicated_worktree)
        .collect::<Vec<_>>();
    let known_paths = known
        .iter()
        .map(|agent| agent.workspace.clone())
        .collect::<BTreeSet<_>>();
    let mut entries = known
        .iter()
        .map(|agent| audit_agent(&managed_root, agent))
        .collect::<Result<Vec<_>>>()?;
    if managed_root.is_dir() {
        for item in std::fs::read_dir(&managed_root)? {
            let path = item?.path();
            if path.is_dir() && !known_paths.contains(&path) {
                entries.push(ManagedWorktreeAudit {
                    agent_id: path
                        .file_name()
                        .and_then(|value| value.to_str())
                        .and_then(|value| value.parse().ok()),
                    path,
                    branch: None,
                    state: ManagedWorktreeState::Unknown,
                    child_snapshot_id: None,
                    changed_files: 0,
                    quarantine_allowed: false,
                    reason: "managed directory has no matching Runtime Agent; inspect manually"
                        .to_owned(),
                });
            }
        }
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(entries)
}

fn audit_agent(managed_root: &Path, agent: &RuntimeAgent) -> Result<ManagedWorktreeAudit> {
    if agent.worktree_quarantined_at.is_some() {
        return Ok(entry(
            agent,
            ManagedWorktreeState::Quarantined,
            None,
            0,
            false,
            "Worktree and all contents are retained in Runtime Recovery",
        ));
    }
    if !agent.workspace.exists() {
        return Ok(entry(
            agent,
            ManagedWorktreeState::Missing,
            None,
            0,
            false,
            "recorded path is missing; no automatic repair or deletion",
        ));
    }
    if !is_direct_child(managed_root, &agent.workspace)? {
        return Ok(entry(
            agent,
            ManagedWorktreeState::Unknown,
            None,
            0,
            false,
            "path is outside the WillDeep managed Worktree root",
        ));
    }
    let snapshot = diff_review::snapshot(&agent.workspace)?;
    if is_active(agent.status) {
        return Ok(entry(
            agent,
            ManagedWorktreeState::Active,
            Some(snapshot.id),
            snapshot.files.len(),
            false,
            "agent is still active",
        ));
    }
    if snapshot.files.is_empty() {
        return Ok(entry(
            agent,
            ManagedWorktreeState::Clean,
            Some(snapshot.id),
            0,
            true,
            "terminal Worktree is clean and may be moved to Recovery",
        ));
    }
    let merged_exactly = agent
        .worktree_merged_child_snapshot_id
        .as_deref()
        .is_some_and(|id| id == snapshot.id);
    let safe_dirty = merged_exactly
        && !snapshot.has_conflicts
        && !snapshot
            .files
            .iter()
            .any(|file| file.kind == diff_review::DiffFileKind::Untracked);
    if safe_dirty {
        Ok(entry(
            agent,
            ManagedWorktreeState::Merged,
            Some(snapshot.id),
            snapshot.files.len(),
            true,
            "exact Child snapshot was merged; full Worktree may be moved to Recovery",
        ))
    } else {
        Ok(entry(
            agent,
            ManagedWorktreeState::Reviewable,
            Some(snapshot.id),
            snapshot.files.len(),
            false,
            "unmerged, changed, conflicted, or untracked content must be reviewed",
        ))
    }
}

fn quarantine(
    home: &Path,
    agents: &AgentStore,
    id: uuid::Uuid,
    expected_snapshot_id: &str,
) -> Result<WorktreeQuarantineResult> {
    let agent = agents.get(id)?.context("Runtime agent not found")?;
    let audited = audit_agent(&managed_root(home), &agent)?;
    if !audited.quarantine_allowed {
        bail!("Worktree quarantine is blocked: {}", audited.reason);
    }
    if audited.child_snapshot_id.as_deref() != Some(expected_snapshot_id) {
        bail!("Child snapshot changed; run worktrees-audit again");
    }
    let root_workspace = agent
        .root_workspace
        .as_deref()
        .context("agent is missing root Workspace")?;
    let recovery_parent = home
        .join("recovery")
        .join("worktrees")
        .join(agent.id.to_string())
        .join(format!("{}-{}", now(), uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&recovery_parent)?;
    let quarantine_path = recovery_parent.join("worktree");
    move_worktree(root_workspace, &agent.workspace, &quarantine_path)?;
    if let Err(error) = agents.mark_worktree_quarantined(id, quarantine_path.clone()) {
        let rollback = move_worktree(root_workspace, &quarantine_path, &agent.workspace);
        return Err(match rollback {
            Ok(()) => error.context("persist quarantine state; Worktree move was rolled back"),
            Err(rollback) => error.context(format!(
                "persist quarantine state and rollback Worktree move: {rollback:#}"
            )),
        });
    }
    write_json_atomic(
        &recovery_parent.join("metadata.json"),
        &serde_json::json!({
            "agent_id": agent.id,
            "original_path": agent.workspace,
            "quarantine_path": quarantine_path,
            "branch": agent.worktree_branch,
            "child_snapshot_id": expected_snapshot_id,
            "review_id": agent.worktree_merged_review_id,
            "created_at": now()
        }),
    )?;
    Ok(WorktreeQuarantineResult {
        agent_id: id,
        original_path: agent.workspace,
        quarantine_path,
        branch_retained: agent.worktree_branch,
    })
}

fn move_worktree(repository: &Path, source: &Path, target: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["worktree", "move"])
        .arg(source)
        .arg(target)
        .current_dir(repository)
        .output()?;
    if !output.status.success() {
        bail!(
            "git worktree move failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn entry(
    agent: &RuntimeAgent,
    state: ManagedWorktreeState,
    child_snapshot_id: Option<String>,
    changed_files: usize,
    quarantine_allowed: bool,
    reason: &str,
) -> ManagedWorktreeAudit {
    ManagedWorktreeAudit {
        agent_id: Some(agent.id),
        path: agent.workspace.clone(),
        branch: agent.worktree_branch.clone(),
        state,
        child_snapshot_id,
        changed_files,
        quarantine_allowed,
        reason: reason.to_owned(),
    }
}

fn managed_root(home: &Path) -> PathBuf {
    home.join("worktrees").join("subagents")
}

fn is_direct_child(root: &Path, candidate: &Path) -> Result<bool> {
    let root = root.canonicalize()?;
    let candidate = candidate.canonicalize()?;
    Ok(candidate.parent() == Some(root.as_path()))
}

fn is_active(status: RuntimeAgentStatus) -> bool {
    matches!(
        status,
        RuntimeAgentStatus::Queued
            | RuntimeAgentStatus::Running
            | RuntimeAgentStatus::WaitingApproval
            | RuntimeAgentStatus::WaitingAnswer
    )
}

fn quarantine_error_status(error: anyhow::Error) -> StatusCode {
    let message = error.to_string();
    if message.contains("not found") {
        StatusCode::NOT_FOUND
    } else if message.contains("blocked") || message.contains("changed") {
        StatusCode::CONFLICT
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_terminal_worktree_moves_to_recovery_without_deletion() {
        let fixture = Fixture::new();
        let entries = audit(&fixture.home, &fixture.agents).unwrap();
        let entry = entries
            .iter()
            .find(|entry| entry.agent_id == Some(fixture.agent_id))
            .unwrap();
        assert_eq!(entry.state, ManagedWorktreeState::Clean);
        assert!(entry.quarantine_allowed);
        let snapshot = entry.child_snapshot_id.clone().unwrap();

        let result = quarantine(&fixture.home, &fixture.agents, fixture.agent_id, &snapshot)
            .expect("quarantine");
        assert!(!result.original_path.exists());
        assert!(result.quarantine_path.join("tracked.txt").is_file());
        assert_eq!(
            std::fs::read_to_string(result.quarantine_path.join("tracked.txt")).unwrap(),
            "base\n"
        );
        assert!(result.branch_retained.is_some());
        let stored = fixture.agents.get(fixture.agent_id).unwrap().unwrap();
        assert_eq!(stored.workspace, result.quarantine_path);
        assert!(stored.worktree_quarantined_at.is_some());
    }

    #[test]
    fn changed_unmerged_and_unknown_directories_are_never_quarantinable() {
        let fixture = Fixture::new();
        std::fs::write(fixture.worktree.join("tracked.txt"), "unmerged\n").unwrap();
        let unknown = fixture
            .home
            .join("worktrees/subagents")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&unknown).unwrap();
        let entries = audit(&fixture.home, &fixture.agents).unwrap();
        assert!(entries.iter().any(|entry| {
            entry.agent_id == Some(fixture.agent_id)
                && entry.state == ManagedWorktreeState::Reviewable
                && !entry.quarantine_allowed
        }));
        assert!(entries.iter().any(|entry| {
            entry.path == unknown
                && entry.state == ManagedWorktreeState::Unknown
                && !entry.quarantine_allowed
        }));
    }

    struct Fixture {
        root: PathBuf,
        home: PathBuf,
        worktree: PathBuf,
        agent_id: uuid::Uuid,
        agents: AgentStore,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "willdeep-worktree-maintenance-{}",
                uuid::Uuid::new_v4()
            ));
            let home = root.join("home");
            let repository = root.join("repository");
            let agent_id = uuid::Uuid::new_v4();
            let worktree = home.join("worktrees/subagents").join(agent_id.to_string());
            std::fs::create_dir_all(&repository).unwrap();
            std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
            git(&repository, &["init"]);
            std::fs::write(repository.join("tracked.txt"), "base\n").unwrap();
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
            git(
                &repository,
                &[
                    "worktree",
                    "add",
                    "-b",
                    "willdeep/agent-maintenance-test",
                    worktree.to_str().unwrap(),
                ],
            );
            let agents = AgentStore::open(home.join("agents.json")).unwrap();
            let task_id = uuid::Uuid::new_v4();
            agents
                .ensure_root(
                    task_id,
                    repository.clone(),
                    None,
                    RuntimeAgentStatus::Running,
                )
                .unwrap();
            agents
                .apply_harness_event(
                    task_id,
                    &serde_json::json!({
                        "type": "subagent_started",
                        "id": agent_id,
                        "profile": "editor",
                        "label": "maintenance test",
                        "background": true,
                        "workspace": worktree,
                        "root_workspace": repository,
                        "worktree_branch": "willdeep/agent-maintenance-test",
                        "dedicated_worktree": true
                    })
                    .to_string(),
                )
                .unwrap();
            agents
                .apply_harness_event(
                    task_id,
                    &serde_json::json!({
                        "type": "subagent_completed",
                        "id": agent_id,
                        "status": "completed"
                    })
                    .to_string(),
                )
                .unwrap();
            Self {
                root,
                home,
                worktree,
                agent_id,
                agents,
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn git(repository: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(repository)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
