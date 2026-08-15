use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use super::agent_store::RuntimeAgent;
use super::*;

const MAX_WORKTREE_PATCH_BYTES: usize = 2 * 1024 * 1024;
static MERGE_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct WorktreeReview {
    pub id: String,
    pub agent_id: uuid::Uuid,
    pub root_workspace: PathBuf,
    pub worktree: PathBuf,
    pub branch: String,
    pub child_snapshot_id: String,
    pub root_snapshot_id: String,
    pub files: Vec<diff_review::DiffFile>,
    pub additions: u64,
    pub deletions: u64,
    pub patch_bytes: usize,
    pub can_merge: bool,
    pub blockers: Vec<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorktreeMergeDecision {
    Approve,
    Reject,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct WorktreeMergeRequest {
    pub review_id: String,
    pub decision: WorktreeMergeDecision,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct WorktreeMergeResult {
    pub review_id: String,
    pub applied: bool,
    pub root_snapshot_id: String,
}

pub(crate) async fn remote_review(home: &Path, id: uuid::Uuid) -> Result<WorktreeReview> {
    let state = ensure_running(home).await?;
    let response = runtime_client(&state)?
        .worktree_review(&willdeep_runtime_protocol::WorktreeReviewParams { agent_id: id })
        .await?;
    local_review(response.into_result()?)
}

pub(crate) async fn remote_merge(
    home: &Path,
    id: uuid::Uuid,
    review_id: String,
) -> Result<WorktreeMergeResult> {
    let state = ensure_running(home).await?;
    let result = (runtime_client(&state)?
        .merge_worktree(
            &willdeep_runtime_protocol::WorktreeMergeParams {
                agent_id: id,
                review_id,
                decision: willdeep_runtime_protocol::WorktreeMergeDecision::Approve,
            },
            uuid::Uuid::new_v4(),
        )
        .await?)
        .into_result()?;
    Ok(WorktreeMergeResult {
        review_id: result.review_id,
        applied: result.applied,
        root_snapshot_id: result.root_snapshot_id,
    })
}

fn local_review(
    review: willdeep_runtime_protocol::RuntimeWorktreeReview,
) -> Result<WorktreeReview> {
    Ok(WorktreeReview {
        id: review.id,
        agent_id: review.agent_id,
        root_workspace: PathBuf::from(
            review
                .root_workspace
                .context("missing root Workspace path")?,
        ),
        worktree: PathBuf::from(review.worktree.context("missing Worktree path")?),
        branch: review.branch,
        child_snapshot_id: review.child_snapshot_id,
        root_snapshot_id: review.root_snapshot_id,
        files: review.files.into_iter().map(local_file).collect(),
        additions: review.additions,
        deletions: review.deletions,
        patch_bytes: review.patch_bytes,
        can_merge: review.can_merge,
        blockers: review.blockers,
    })
}

fn local_file(file: willdeep_runtime_protocol::DiffFile) -> diff_review::DiffFile {
    diff_review::DiffFile {
        path: file.path,
        old_path: file.old_path,
        kind: match file.kind {
            willdeep_runtime_protocol::DiffFileKind::Added => diff_review::DiffFileKind::Added,
            willdeep_runtime_protocol::DiffFileKind::Modified => {
                diff_review::DiffFileKind::Modified
            }
            willdeep_runtime_protocol::DiffFileKind::Deleted => diff_review::DiffFileKind::Deleted,
            willdeep_runtime_protocol::DiffFileKind::Renamed => diff_review::DiffFileKind::Renamed,
            willdeep_runtime_protocol::DiffFileKind::Copied => diff_review::DiffFileKind::Copied,
            willdeep_runtime_protocol::DiffFileKind::Unmerged => {
                diff_review::DiffFileKind::Unmerged
            }
            willdeep_runtime_protocol::DiffFileKind::Untracked => {
                diff_review::DiffFileKind::Untracked
            }
        },
        staged: file.staged,
        unstaged: file.unstaged,
        binary: file.binary,
        additions: file.additions,
        deletions: file.deletions,
    }
}

pub(crate) async fn review_cli(home: &Path, id: uuid::Uuid) -> Result<()> {
    let review = remote_review(home, id).await?;
    print_review(&review);
    Ok(())
}

pub(crate) async fn merge_cli(
    home: &Path,
    id: uuid::Uuid,
    review_id: String,
    yes: bool,
) -> Result<()> {
    if !yes {
        bail!(
            "merge requires explicit --yes with the exact review ID; run `willdeep daemon agent-worktree-review {id}` first"
        );
    }
    let result = remote_merge(home, id, review_id).await?;
    println!(
        "merged\treview={}\troot_snapshot={}",
        result.review_id, result.root_snapshot_id
    );
    Ok(())
}

fn print_review(review: &WorktreeReview) {
    println!("review\t{}", review.id);
    println!("agent\t{}", review.agent_id);
    println!("branch\t{}", review.branch);
    println!("worktree\t{}", review.worktree.display());
    println!("root\t{}", review.root_workspace.display());
    println!(
        "changes\tfiles={}\t+{}\t-{}\tpatch_bytes={}",
        review.files.len(),
        review.additions,
        review.deletions,
        review.patch_bytes
    );
    println!("can_merge\t{}", review.can_merge);
    for blocker in &review.blockers {
        println!("blocked\t{blocker}");
    }
    for file in &review.files {
        println!("file\t{:?}\t{}", file.kind, file.path);
    }
}

pub(crate) async fn review_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<uuid::Uuid>,
) -> Result<Json<WorktreeReview>, StatusCode> {
    authorize(&state, &headers)?;
    let agent = state
        .tasks
        .agents
        .get(id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    review(&agent).map(Json).map_err(review_error_status)
}

pub(crate) async fn merge_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<uuid::Uuid>,
    Json(request): Json<WorktreeMergeRequest>,
) -> Result<Json<WorktreeMergeResult>, StatusCode> {
    authorize(&state, &headers)?;
    merge(&state, id, request).await.map(Json)
}

async fn merge(
    state: &ServerState,
    id: uuid::Uuid,
    request: WorktreeMergeRequest,
) -> Result<WorktreeMergeResult, StatusCode> {
    let _guard = MERGE_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let agent = state
        .tasks
        .agents
        .get(id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let current = review(&agent).map_err(review_error_status)?;
    if current.id != request.review_id {
        return Err(StatusCode::CONFLICT);
    }
    if request.decision == WorktreeMergeDecision::Reject {
        return Ok(WorktreeMergeResult {
            review_id: current.id,
            applied: false,
            root_snapshot_id: current.root_snapshot_id,
        });
    }
    if !current.can_merge {
        return Err(StatusCode::CONFLICT);
    }
    let patch = tracked_patch(&current.worktree).map_err(review_error_status)?;
    apply_patch(&current.root_workspace, &patch).map_err(review_error_status)?;
    state
        .tasks
        .agents
        .mark_worktree_merged(id, current.id.clone(), current.child_snapshot_id.clone())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let updated = diff_review::snapshot(&current.root_workspace)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let _ = state.events.append(
        "agent.worktree_merged",
        format!(
            "agent_id={} review_id={} root_snapshot_id={}",
            id, current.id, updated.id
        ),
    );
    Ok(WorktreeMergeResult {
        review_id: current.id,
        applied: true,
        root_snapshot_id: updated.id,
    })
}

pub(super) async fn unified_review(
    state: &ServerState,
    params: willdeep_runtime_protocol::WorktreeReviewParams,
) -> Result<willdeep_runtime_protocol::RuntimeWorktreeReview, StatusCode> {
    let agent = state
        .tasks
        .agents
        .get(params.agent_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    review(&agent)
        .map(public_review)
        .map_err(review_error_status)
}

pub(super) async fn unified_merge(
    state: &ServerState,
    params: willdeep_runtime_protocol::WorktreeMergeParams,
) -> Result<willdeep_runtime_protocol::RuntimeWorktreeMergeResult, StatusCode> {
    let decision = match params.decision {
        willdeep_runtime_protocol::WorktreeMergeDecision::Approve => WorktreeMergeDecision::Approve,
        willdeep_runtime_protocol::WorktreeMergeDecision::Reject => WorktreeMergeDecision::Reject,
    };
    merge(
        state,
        params.agent_id,
        WorktreeMergeRequest {
            review_id: params.review_id,
            decision,
        },
    )
    .await
    .map(
        |result| willdeep_runtime_protocol::RuntimeWorktreeMergeResult {
            review_id: result.review_id,
            applied: result.applied,
            root_snapshot_id: result.root_snapshot_id,
        },
    )
}

fn public_review(review: WorktreeReview) -> willdeep_runtime_protocol::RuntimeWorktreeReview {
    willdeep_runtime_protocol::RuntimeWorktreeReview {
        id: review.id,
        agent_id: review.agent_id,
        root_workspace: Some(review.root_workspace.display().to_string()),
        worktree: Some(review.worktree.display().to_string()),
        branch: review.branch,
        child_snapshot_id: review.child_snapshot_id,
        root_snapshot_id: review.root_snapshot_id,
        files: review.files.into_iter().map(public_file).collect(),
        additions: review.additions,
        deletions: review.deletions,
        patch_bytes: review.patch_bytes,
        can_merge: review.can_merge,
        blockers: review.blockers,
    }
}

fn public_file(file: diff_review::DiffFile) -> willdeep_runtime_protocol::DiffFile {
    willdeep_runtime_protocol::DiffFile {
        path: file.path,
        old_path: file.old_path,
        kind: match file.kind {
            diff_review::DiffFileKind::Added => willdeep_runtime_protocol::DiffFileKind::Added,
            diff_review::DiffFileKind::Modified => {
                willdeep_runtime_protocol::DiffFileKind::Modified
            }
            diff_review::DiffFileKind::Deleted => willdeep_runtime_protocol::DiffFileKind::Deleted,
            diff_review::DiffFileKind::Renamed => willdeep_runtime_protocol::DiffFileKind::Renamed,
            diff_review::DiffFileKind::Copied => willdeep_runtime_protocol::DiffFileKind::Copied,
            diff_review::DiffFileKind::Unmerged => {
                willdeep_runtime_protocol::DiffFileKind::Unmerged
            }
            diff_review::DiffFileKind::Untracked => {
                willdeep_runtime_protocol::DiffFileKind::Untracked
            }
        },
        staged: file.staged,
        unstaged: file.unstaged,
        binary: file.binary,
        additions: file.additions,
        deletions: file.deletions,
    }
}

fn review(agent: &RuntimeAgent) -> Result<WorktreeReview> {
    if !agent.dedicated_worktree {
        bail!("agent does not own a dedicated worktree");
    }
    if !matches!(
        agent.status,
        RuntimeAgentStatus::Completed
            | RuntimeAgentStatus::Failed
            | RuntimeAgentStatus::Blocked
            | RuntimeAgentStatus::Cancelled
            | RuntimeAgentStatus::Interrupted
    ) {
        bail!("agent worktree cannot be reviewed while the agent is active");
    }
    let root_workspace = agent
        .root_workspace
        .clone()
        .context("agent is missing its root workspace")?;
    let branch = agent
        .worktree_branch
        .clone()
        .context("agent is missing its worktree branch")?;
    let child = diff_review::snapshot(&agent.workspace)?;
    let root = diff_review::snapshot(&root_workspace)?;
    let patch = tracked_patch(&agent.workspace)?;
    let mut blockers = Vec::new();
    if child.files.is_empty() {
        blockers.push("child worktree has no changes".to_owned());
    }
    if child.has_conflicts {
        blockers.push("child worktree contains unresolved conflicts".to_owned());
    }
    if child
        .files
        .iter()
        .any(|file| file.kind == diff_review::DiffFileKind::Untracked)
    {
        blockers.push("untracked child files require explicit review before merge".to_owned());
    }
    if patch.len() > MAX_WORKTREE_PATCH_BYTES {
        blockers.push(format!(
            "tracked patch exceeds {} bytes",
            MAX_WORKTREE_PATCH_BYTES
        ));
    }
    if patch.is_empty() && !child.files.is_empty() {
        blockers.push("child changes cannot be represented as a tracked Git patch".to_owned());
    }
    if blockers.is_empty()
        && let Err(error) = check_patch(&root_workspace, &patch)
    {
        blockers.push(format!(
            "root workspace conflicts with child patch: {error}"
        ));
    }
    let id = review_id(agent.id, &child.id, &root.id, &patch);
    Ok(WorktreeReview {
        id,
        agent_id: agent.id,
        root_workspace,
        worktree: agent.workspace.clone(),
        branch,
        child_snapshot_id: child.id,
        root_snapshot_id: root.id,
        files: child.files,
        additions: child.additions,
        deletions: child.deletions,
        patch_bytes: patch.len(),
        can_merge: blockers.is_empty(),
        blockers,
    })
}

pub(super) fn tracked_patch(worktree: &Path) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .args(["diff", "--binary", "--full-index", "HEAD", "--"])
        .current_dir(worktree)
        .output()
        .with_context(|| format!("read child patch from {}", worktree.display()))?;
    if !output.status.success() {
        bail!(
            "read child patch: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

fn check_patch(workspace: &Path, patch: &[u8]) -> Result<()> {
    run_git_apply(workspace, patch, true)
}

fn apply_patch(workspace: &Path, patch: &[u8]) -> Result<()> {
    run_git_apply(workspace, patch, false)
}

fn run_git_apply(workspace: &Path, patch: &[u8], check: bool) -> Result<()> {
    let mut command = Command::new("git");
    command.args(["apply", "--whitespace=nowarn"]);
    if check {
        command.arg("--check");
    }
    command
        .arg("-")
        .current_dir(workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().context("launch git apply")?;
    child
        .stdin
        .take()
        .context("open git apply stdin")?
        .write_all(patch)
        .context("write git apply patch")?;
    let output = child.wait_with_output().context("wait for git apply")?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}

fn review_id(agent_id: uuid::Uuid, child: &str, root: &str, patch: &[u8]) -> String {
    let mut hasher = DefaultHasher::new();
    agent_id.hash(&mut hasher);
    child.hash(&mut hasher);
    root.hash(&mut hasher);
    patch.hash(&mut hasher);
    format!("wr-{:016x}", hasher.finish())
}

fn review_error_status(error: anyhow::Error) -> StatusCode {
    let message = error.to_string();
    if message.contains("does not own") || message.contains("missing") {
        StatusCode::UNPROCESSABLE_ENTITY
    } else if message.contains("active") {
        StatusCode::CONFLICT
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_applies_an_exact_tracked_patch() {
        let fixture = Fixture::new();
        std::fs::write(fixture.worktree.join("tracked.txt"), "child\n").unwrap();
        let review = review(&fixture.agent()).expect("review");
        assert!(review.can_merge, "{:?}", review.blockers);
        assert_eq!(review.files.len(), 1);
        let patch = tracked_patch(&fixture.worktree).unwrap();
        apply_patch(&fixture.repository, &patch).expect("apply");
        assert_eq!(
            std::fs::read_to_string(fixture.repository.join("tracked.txt")).unwrap(),
            "child\n"
        );
    }

    #[test]
    fn review_blocks_when_root_changed_the_same_file() {
        let fixture = Fixture::new();
        std::fs::write(fixture.worktree.join("tracked.txt"), "child\n").unwrap();
        let clean_root_review = review(&fixture.agent()).expect("initial review");
        assert!(clean_root_review.can_merge);
        std::fs::write(fixture.repository.join("tracked.txt"), "user\n").unwrap();
        let review = review(&fixture.agent()).expect("review");
        assert_ne!(review.id, clean_root_review.id);
        assert!(!review.can_merge);
        assert!(
            review
                .blockers
                .iter()
                .any(|blocker| blocker.contains("conflicts"))
        );
        assert_eq!(
            std::fs::read_to_string(fixture.repository.join("tracked.txt")).unwrap(),
            "user\n"
        );
    }

    struct Fixture {
        root: PathBuf,
        repository: PathBuf,
        worktree: PathBuf,
        agent_id: uuid::Uuid,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir()
                .join(format!("willdeep-worktree-review-{}", uuid::Uuid::new_v4()));
            let repository = root.join("repository");
            let worktree = root.join("worktree");
            std::fs::create_dir_all(&repository).unwrap();
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
                    "willdeep/agent-test",
                    worktree.to_str().unwrap(),
                ],
            );
            Self {
                root,
                repository,
                worktree,
                agent_id: uuid::Uuid::new_v4(),
            }
        }

        fn agent(&self) -> RuntimeAgent {
            RuntimeAgent {
                id: self.agent_id,
                parent_id: Some(uuid::Uuid::new_v4()),
                task_id: uuid::Uuid::new_v4(),
                label: Some("editor".to_owned()),
                background: true,
                workspace: self.worktree.clone(),
                root_workspace: Some(self.repository.clone()),
                worktree_branch: Some("willdeep/agent-test".to_owned()),
                dedicated_worktree: true,
                worktree_merged_review_id: None,
                worktree_merged_child_snapshot_id: None,
                worktree_merged_at: None,
                worktree_quarantined_at: None,
                profile: Some("editor".to_owned()),
                model: Some("editor-model".to_owned()),
                status: RuntimeAgentStatus::Completed,
                current_turn: 1,
                current_tool: None,
                input_tokens: None,
                output_tokens: None,
                total_tokens: None,
                max_turns: Some(6),
                token_budget: None,
                timeout_seconds: Some(300),
                report: None,
                verifier_command: None,
                verifier_passed: None,
                attempts: None,
                repo_commit: None,
                created_at: 1,
                updated_at: 2,
                completed_at: Some(2),
                error: None,
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = Command::new("git")
                .args(["worktree", "remove", "--force"])
                .arg(&self.worktree)
                .current_dir(&self.repository)
                .output();
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
