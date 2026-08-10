use std::collections::{BTreeMap, BTreeSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Component;
use std::sync::OnceLock;

use super::*;

const MAX_DIFF_BYTES: usize = 512 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, Hash, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DiffFileKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    Unmerged,
    Untracked,
}

#[derive(Clone, Debug, Serialize, Deserialize, Hash, PartialEq, Eq)]
pub(crate) struct DiffFile {
    pub path: String,
    pub old_path: Option<String>,
    pub kind: DiffFileKind,
    pub staged: bool,
    pub unstaged: bool,
    pub binary: bool,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct DiffSnapshot {
    pub id: String,
    pub workspace: PathBuf,
    pub head: Option<String>,
    pub files: Vec<DiffFile>,
    pub additions: u64,
    pub deletions: u64,
    pub has_conflicts: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AttributionConfidence {
    ToolWindow,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct DiffAttributionRecord {
    pub id: uuid::Uuid,
    pub before_snapshot_id: String,
    pub after_snapshot_id: String,
    pub workspace: PathBuf,
    pub session_id: Option<uuid::Uuid>,
    pub turn_id: Option<uuid::Uuid>,
    pub task_id: uuid::Uuid,
    pub agent_id: uuid::Uuid,
    pub tool: String,
    pub paths: Vec<String>,
    pub confidence: AttributionConfidence,
    pub created_at: u64,
}

pub(crate) struct DiffCapture {
    snapshot: DiffSnapshot,
    fingerprints: BTreeMap<String, u64>,
}

pub(crate) struct AttributionContext {
    pub session_id: Option<uuid::Uuid>,
    pub turn_id: Option<uuid::Uuid>,
    pub task_id: uuid::Uuid,
    pub agent_id: uuid::Uuid,
    pub tool: String,
}

static ATTRIBUTION_STORE_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, clap::ValueEnum, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DiffArea {
    Staged,
    Unstaged,
    #[default]
    Combined,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, clap::ValueEnum, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReviewDecision {
    Accepted,
    Rejected,
    ChangesRequested,
    Reviewed,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct DiffReviewRecord {
    pub id: uuid::Uuid,
    pub snapshot_id: String,
    pub workspace: PathBuf,
    pub path: String,
    pub decision: ReviewDecision,
    pub note: Option<String>,
    pub created_at: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct ReviewRequest {
    pub workspace: PathBuf,
    pub path: String,
    pub decision: ReviewDecision,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct RevertRequest {
    pub workspace: PathBuf,
    pub path: String,
    #[serde(default)]
    pub area: DiffArea,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RevertResult {
    pub previous_snapshot_id: String,
    pub current_snapshot_id: String,
    pub path: String,
    pub recovery_path: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum VerificationOutcome {
    Passed,
    Failed,
    TimedOut,
    LaunchFailed,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct DiffVerificationRecord {
    pub id: uuid::Uuid,
    pub snapshot_id: String,
    pub workspace: PathBuf,
    pub command: String,
    pub exit_code: Option<i32>,
    pub outcome: VerificationOutcome,
    pub summary: String,
    pub created_at: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct VerificationRequest {
    pub workspace: PathBuf,
    pub command: String,
    pub exit_code: Option<i32>,
    pub outcome: VerificationOutcome,
    pub summary: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FindingSeverity {
    Warning,
    Blocker,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct SensitiveFinding {
    pub path: String,
    pub code: String,
    pub severity: FindingSeverity,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct CommitPreview {
    pub snapshot_id: String,
    pub workspace: PathBuf,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub message: String,
    pub staged_files: Vec<String>,
    pub unstaged_files: Vec<String>,
    pub sensitive_findings: Vec<SensitiveFinding>,
    pub remote: String,
    pub push_target: Option<String>,
    pub tag: Option<String>,
    pub blockers: Vec<String>,
    pub requires_confirmation: bool,
}

fn public_file(file: DiffFile) -> willdeep_runtime_protocol::DiffFile {
    use willdeep_runtime_protocol::DiffFileKind as Target;
    willdeep_runtime_protocol::DiffFile {
        path: file.path,
        old_path: file.old_path,
        kind: match file.kind {
            DiffFileKind::Added => Target::Added,
            DiffFileKind::Modified => Target::Modified,
            DiffFileKind::Deleted => Target::Deleted,
            DiffFileKind::Renamed => Target::Renamed,
            DiffFileKind::Copied => Target::Copied,
            DiffFileKind::Unmerged => Target::Unmerged,
            DiffFileKind::Untracked => Target::Untracked,
        },
        staged: file.staged,
        unstaged: file.unstaged,
        binary: file.binary,
        additions: file.additions,
        deletions: file.deletions,
    }
}

fn public_snapshot(snapshot: DiffSnapshot) -> willdeep_runtime_protocol::DiffSnapshot {
    willdeep_runtime_protocol::DiffSnapshot {
        id: snapshot.id,
        workspace: Some(snapshot.workspace.to_string_lossy().into_owned()),
        head: snapshot.head,
        files: snapshot.files.into_iter().map(public_file).collect(),
        additions: snapshot.additions,
        deletions: snapshot.deletions,
        has_conflicts: snapshot.has_conflicts,
    }
}

fn public_area(area: DiffArea) -> willdeep_runtime_protocol::DiffArea {
    match area {
        DiffArea::Staged => willdeep_runtime_protocol::DiffArea::Staged,
        DiffArea::Unstaged => willdeep_runtime_protocol::DiffArea::Unstaged,
        DiffArea::Combined => willdeep_runtime_protocol::DiffArea::Combined,
    }
}

fn local_area(area: willdeep_runtime_protocol::DiffArea) -> DiffArea {
    match area {
        willdeep_runtime_protocol::DiffArea::Staged => DiffArea::Staged,
        willdeep_runtime_protocol::DiffArea::Unstaged => DiffArea::Unstaged,
        willdeep_runtime_protocol::DiffArea::Combined => DiffArea::Combined,
    }
}

fn public_decision(decision: ReviewDecision) -> willdeep_runtime_protocol::ReviewDecision {
    match decision {
        ReviewDecision::Accepted => willdeep_runtime_protocol::ReviewDecision::Accepted,
        ReviewDecision::Rejected => willdeep_runtime_protocol::ReviewDecision::Rejected,
        ReviewDecision::ChangesRequested => {
            willdeep_runtime_protocol::ReviewDecision::ChangesRequested
        }
        ReviewDecision::Reviewed => willdeep_runtime_protocol::ReviewDecision::Reviewed,
    }
}

fn runtime_api_data<T>(response: willdeep_runtime_protocol::ApiResponse<T>) -> Result<T> {
    match response {
        willdeep_runtime_protocol::ApiResponse::Ok { data, .. } => Ok(data),
        willdeep_runtime_protocol::ApiResponse::Error { error, .. } => {
            bail!("Runtime API error: {}", error.message)
        }
    }
}

fn local_snapshot(snapshot: willdeep_runtime_protocol::DiffSnapshot) -> Result<DiffSnapshot> {
    use willdeep_runtime_protocol::DiffFileKind as Source;
    Ok(DiffSnapshot {
        id: snapshot.id,
        workspace: PathBuf::from(
            snapshot
                .workspace
                .context("Runtime omitted Diff workspace")?,
        ),
        head: snapshot.head,
        files: snapshot
            .files
            .into_iter()
            .map(|file| DiffFile {
                path: file.path,
                old_path: file.old_path,
                kind: match file.kind {
                    Source::Added => DiffFileKind::Added,
                    Source::Modified => DiffFileKind::Modified,
                    Source::Deleted => DiffFileKind::Deleted,
                    Source::Renamed => DiffFileKind::Renamed,
                    Source::Copied => DiffFileKind::Copied,
                    Source::Unmerged => DiffFileKind::Unmerged,
                    Source::Untracked => DiffFileKind::Untracked,
                },
                staged: file.staged,
                unstaged: file.unstaged,
                binary: file.binary,
                additions: file.additions,
                deletions: file.deletions,
            })
            .collect(),
        additions: snapshot.additions,
        deletions: snapshot.deletions,
        has_conflicts: snapshot.has_conflicts,
    })
}

fn public_review(record: DiffReviewRecord) -> willdeep_runtime_protocol::DiffReview {
    use willdeep_runtime_protocol::ReviewDecision as Target;
    willdeep_runtime_protocol::DiffReview {
        id: record.id,
        snapshot_id: record.snapshot_id,
        workspace: Some(record.workspace.to_string_lossy().into_owned()),
        path: record.path,
        decision: match record.decision {
            ReviewDecision::Accepted => Target::Accepted,
            ReviewDecision::Rejected => Target::Rejected,
            ReviewDecision::ChangesRequested => Target::ChangesRequested,
            ReviewDecision::Reviewed => Target::Reviewed,
        },
        note: record.note,
        created_at: record.created_at,
    }
}

fn local_review(record: willdeep_runtime_protocol::DiffReview) -> Result<DiffReviewRecord> {
    use willdeep_runtime_protocol::ReviewDecision as Source;
    Ok(DiffReviewRecord {
        id: record.id,
        snapshot_id: record.snapshot_id,
        workspace: PathBuf::from(
            record
                .workspace
                .context("Runtime omitted review workspace")?,
        ),
        path: record.path,
        decision: match record.decision {
            Source::Accepted => ReviewDecision::Accepted,
            Source::Rejected => ReviewDecision::Rejected,
            Source::ChangesRequested => ReviewDecision::ChangesRequested,
            Source::Reviewed => ReviewDecision::Reviewed,
        },
        note: record.note,
        created_at: record.created_at,
    })
}

fn public_verification(
    record: DiffVerificationRecord,
) -> willdeep_runtime_protocol::DiffVerification {
    use willdeep_runtime_protocol::VerificationOutcome as Target;
    willdeep_runtime_protocol::DiffVerification {
        id: record.id,
        snapshot_id: record.snapshot_id,
        workspace: Some(record.workspace.to_string_lossy().into_owned()),
        command: record.command,
        exit_code: record.exit_code,
        outcome: match record.outcome {
            VerificationOutcome::Passed => Target::Passed,
            VerificationOutcome::Failed => Target::Failed,
            VerificationOutcome::TimedOut => Target::TimedOut,
            VerificationOutcome::LaunchFailed => Target::LaunchFailed,
        },
        summary: record.summary,
        created_at: record.created_at,
    }
}

fn local_verification(
    record: willdeep_runtime_protocol::DiffVerification,
) -> Result<DiffVerificationRecord> {
    use willdeep_runtime_protocol::VerificationOutcome as Source;
    Ok(DiffVerificationRecord {
        id: record.id,
        snapshot_id: record.snapshot_id,
        workspace: PathBuf::from(
            record
                .workspace
                .context("Runtime omitted verification workspace")?,
        ),
        command: record.command,
        exit_code: record.exit_code,
        outcome: match record.outcome {
            Source::Passed => VerificationOutcome::Passed,
            Source::Failed => VerificationOutcome::Failed,
            Source::TimedOut => VerificationOutcome::TimedOut,
            Source::LaunchFailed => VerificationOutcome::LaunchFailed,
        },
        summary: record.summary,
        created_at: record.created_at,
    })
}

fn public_attribution(record: DiffAttributionRecord) -> willdeep_runtime_protocol::DiffAttribution {
    willdeep_runtime_protocol::DiffAttribution {
        id: record.id,
        before_snapshot_id: record.before_snapshot_id,
        after_snapshot_id: record.after_snapshot_id,
        workspace: Some(record.workspace.to_string_lossy().into_owned()),
        session_id: record.session_id,
        turn_id: record.turn_id,
        task_id: record.task_id,
        agent_id: record.agent_id,
        tool: record.tool,
        paths: record.paths,
        confidence: willdeep_runtime_protocol::AttributionConfidence::ToolWindow,
        created_at: record.created_at,
    }
}

fn local_attribution(
    record: willdeep_runtime_protocol::DiffAttribution,
) -> Result<DiffAttributionRecord> {
    Ok(DiffAttributionRecord {
        id: record.id,
        before_snapshot_id: record.before_snapshot_id,
        after_snapshot_id: record.after_snapshot_id,
        workspace: PathBuf::from(
            record
                .workspace
                .context("Runtime omitted attribution workspace")?,
        ),
        session_id: record.session_id,
        turn_id: record.turn_id,
        task_id: record.task_id,
        agent_id: record.agent_id,
        tool: record.tool,
        paths: record.paths,
        confidence: AttributionConfidence::ToolWindow,
        created_at: record.created_at,
    })
}

fn public_finding(finding: SensitiveFinding) -> willdeep_runtime_protocol::SensitiveFinding {
    willdeep_runtime_protocol::SensitiveFinding {
        path: finding.path,
        code: finding.code,
        severity: match finding.severity {
            FindingSeverity::Warning => willdeep_runtime_protocol::FindingSeverity::Warning,
            FindingSeverity::Blocker => willdeep_runtime_protocol::FindingSeverity::Blocker,
        },
    }
}

fn local_finding(finding: willdeep_runtime_protocol::SensitiveFinding) -> SensitiveFinding {
    SensitiveFinding {
        path: finding.path,
        code: finding.code,
        severity: match finding.severity {
            willdeep_runtime_protocol::FindingSeverity::Warning => FindingSeverity::Warning,
            willdeep_runtime_protocol::FindingSeverity::Blocker => FindingSeverity::Blocker,
        },
    }
}

fn public_commit_preview(preview: CommitPreview) -> willdeep_runtime_protocol::DiffCommitPreview {
    willdeep_runtime_protocol::DiffCommitPreview {
        snapshot_id: preview.snapshot_id,
        workspace: Some(preview.workspace.to_string_lossy().into_owned()),
        branch: preview.branch,
        head: preview.head,
        message: preview.message,
        staged_files: preview.staged_files,
        unstaged_files: preview.unstaged_files,
        sensitive_findings: preview
            .sensitive_findings
            .into_iter()
            .map(public_finding)
            .collect(),
        remote: preview.remote,
        push_target: preview.push_target,
        tag: preview.tag,
        blockers: preview.blockers,
        requires_confirmation: preview.requires_confirmation,
    }
}

fn local_commit_preview(
    preview: willdeep_runtime_protocol::DiffCommitPreview,
) -> Result<CommitPreview> {
    Ok(CommitPreview {
        snapshot_id: preview.snapshot_id,
        workspace: PathBuf::from(
            preview
                .workspace
                .context("Runtime omitted Commit Preview workspace")?,
        ),
        branch: preview.branch,
        head: preview.head,
        message: preview.message,
        staged_files: preview.staged_files,
        unstaged_files: preview.unstaged_files,
        sensitive_findings: preview
            .sensitive_findings
            .into_iter()
            .map(local_finding)
            .collect(),
        remote: preview.remote,
        push_target: preview.push_target,
        tag: preview.tag,
        blockers: preview.blockers,
        requires_confirmation: preview.requires_confirmation,
    })
}

#[derive(Debug, Deserialize)]
pub(super) struct CommitPreviewQuery {
    workspace: PathBuf,
    message: String,
    #[serde(default = "default_remote")]
    remote: String,
    tag: Option<String>,
}

pub(super) async fn snapshot_cli(home: &Path, workspace: PathBuf) -> Result<()> {
    let state = ensure_running(home).await?;
    let response = client()
        .get(format!("http://{}/v1/diffs", state.address))
        .header(TOKEN_HEADER, &state.token)
        .query(&[("workspace", workspace.display().to_string())])
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("Runtime rejected Diff snapshot: {}", response.text().await?);
    }
    let snapshot: DiffSnapshot = response.json().await?;
    println!(
        "{}\tfiles={}\t+{}\t-{}\tconflicts={}\t{}",
        snapshot.id,
        snapshot.files.len(),
        snapshot.additions,
        snapshot.deletions,
        snapshot.has_conflicts,
        snapshot.workspace.display()
    );
    for file in snapshot.files {
        println!(
            "{:?}\tstaged={}\tunstaged={}\tbinary={}\t+{}\t-{}\t{}",
            file.kind,
            file.staged,
            file.unstaged,
            file.binary,
            file.additions,
            file.deletions,
            file.path
        );
    }
    Ok(())
}

pub(super) async fn content_cli(
    home: &Path,
    workspace: PathBuf,
    snapshot_id: String,
    path: String,
    area: DiffArea,
) -> Result<()> {
    let state = ensure_running(home).await?;
    let response = client()
        .get(format!(
            "http://{}/v1/diffs/{snapshot_id}/content",
            state.address
        ))
        .header(TOKEN_HEADER, &state.token)
        .query(&[
            ("workspace", workspace.display().to_string()),
            ("path", path),
            ("area", area_name(area).to_owned()),
        ])
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("Runtime rejected Diff content: {}", response.text().await?);
    }
    let value: serde_json::Value = response.json().await?;
    println!(
        "{}",
        value
            .get("content")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
    );
    Ok(())
}

pub(super) async fn review_cli(
    home: &Path,
    workspace: PathBuf,
    snapshot_id: String,
    path: String,
    decision: ReviewDecision,
    note: Option<String>,
) -> Result<()> {
    let record = remote_review(
        home,
        &snapshot_id,
        &ReviewRequest {
            workspace,
            path,
            decision,
            note,
        },
    )
    .await?;
    println!("{}\t{:?}\t{}", record.id, record.decision, record.path);
    Ok(())
}

pub(super) async fn revert_cli(
    home: &Path,
    workspace: PathBuf,
    snapshot_id: String,
    path: String,
    area: DiffArea,
) -> Result<()> {
    let result = remote_revert(
        home,
        &snapshot_id,
        &RevertRequest {
            workspace,
            path,
            area,
        },
    )
    .await?;
    println!(
        "{}\t{}\trecovery={}",
        result.current_snapshot_id,
        result.path,
        result
            .recovery_path
            .map_or_else(|| "none".to_owned(), |path| path.display().to_string())
    );
    Ok(())
}

pub(super) async fn verifications_cli(
    home: &Path,
    workspace: PathBuf,
    snapshot_id: String,
) -> Result<()> {
    for record in remote_verifications(home, &workspace, &snapshot_id).await? {
        println!(
            "{:?}\texit={}\t{}\t{}",
            record.outcome,
            record
                .exit_code
                .map_or_else(|| "none".to_owned(), |code| code.to_string()),
            record.command,
            record.summary.lines().last().unwrap_or_default()
        );
    }
    Ok(())
}

pub(super) async fn attributions_cli(
    home: &Path,
    workspace: PathBuf,
    snapshot_id: String,
) -> Result<()> {
    let records = remote_attributions(home, &workspace, &snapshot_id).await?;
    println!("{}", serde_json::to_string_pretty(&records)?);
    Ok(())
}

pub(super) async fn commit_preview_cli(
    home: &Path,
    workspace: PathBuf,
    snapshot_id: String,
    message: String,
    remote: String,
    tag: Option<String>,
) -> Result<()> {
    let preview = remote_commit_preview(
        home,
        &workspace,
        &snapshot_id,
        &message,
        &remote,
        tag.as_deref(),
    )
    .await?;
    println!("{}", serde_json::to_string_pretty(&preview)?);
    Ok(())
}

fn area_name(area: DiffArea) -> &'static str {
    match area {
        DiffArea::Staged => "staged",
        DiffArea::Unstaged => "unstaged",
        DiffArea::Combined => "combined",
    }
}

pub(crate) async fn remote_snapshot(home: &Path, workspace: &Path) -> Result<DiffSnapshot> {
    let state = ensure_running(home).await?;
    let response = runtime_client(&state)?
        .call::<_, willdeep_runtime_protocol::DiffSnapshot>(
            "diff.snapshot",
            &willdeep_runtime_protocol::DiffSnapshotParams {
                workspace: workspace.to_string_lossy().into_owned(),
            },
            None,
        )
        .await?;
    local_snapshot(runtime_api_data(response)?)
}

pub(crate) async fn remote_content(
    home: &Path,
    workspace: &Path,
    snapshot_id: &str,
    path: &str,
    area: DiffArea,
) -> Result<String> {
    let state = ensure_running(home).await?;
    let response = runtime_client(&state)?
        .call::<_, willdeep_runtime_protocol::DiffContent>(
            "diff.content",
            &willdeep_runtime_protocol::DiffContentParams {
                workspace: workspace.to_string_lossy().into_owned(),
                snapshot_id: snapshot_id.to_owned(),
                path: path.to_owned(),
                area: public_area(area),
            },
            None,
        )
        .await?;
    Ok(runtime_api_data(response)?.content)
}

pub(crate) async fn remote_review(
    home: &Path,
    snapshot_id: &str,
    request: &ReviewRequest,
) -> Result<DiffReviewRecord> {
    let state = ensure_running(home).await?;
    let response = runtime_client(&state)?
        .call::<_, willdeep_runtime_protocol::DiffReview>(
            "diff.review",
            &willdeep_runtime_protocol::DiffReviewParams {
                workspace: request.workspace.to_string_lossy().into_owned(),
                snapshot_id: snapshot_id.to_owned(),
                path: request.path.clone(),
                decision: public_decision(request.decision),
                note: request.note.clone(),
            },
            None,
        )
        .await?;
    local_review(runtime_api_data(response)?)
}

pub(crate) async fn remote_reviews(
    home: &Path,
    workspace: &Path,
    snapshot_id: &str,
) -> Result<Vec<DiffReviewRecord>> {
    let state = ensure_running(home).await?;
    let response = runtime_client(&state)?
        .call::<_, Vec<willdeep_runtime_protocol::DiffReview>>(
            "diff.reviews",
            &willdeep_runtime_protocol::DiffSnapshotQueryParams {
                workspace: workspace.to_string_lossy().into_owned(),
                snapshot_id: snapshot_id.to_owned(),
            },
            None,
        )
        .await?;
    runtime_api_data(response)?
        .into_iter()
        .map(local_review)
        .collect()
}

pub(crate) async fn remote_verifications(
    home: &Path,
    workspace: &Path,
    snapshot_id: &str,
) -> Result<Vec<DiffVerificationRecord>> {
    let state = ensure_running(home).await?;
    let response = runtime_client(&state)?
        .call::<_, Vec<willdeep_runtime_protocol::DiffVerification>>(
            "diff.verifications",
            &willdeep_runtime_protocol::DiffSnapshotQueryParams {
                workspace: workspace.to_string_lossy().into_owned(),
                snapshot_id: snapshot_id.to_owned(),
            },
            None,
        )
        .await?;
    runtime_api_data(response)?
        .into_iter()
        .map(local_verification)
        .collect()
}

pub(crate) async fn remote_attributions(
    home: &Path,
    workspace: &Path,
    snapshot_id: &str,
) -> Result<Vec<DiffAttributionRecord>> {
    let state = ensure_running(home).await?;
    let response = runtime_client(&state)?
        .call::<_, Vec<willdeep_runtime_protocol::DiffAttribution>>(
            "diff.attributions",
            &willdeep_runtime_protocol::DiffSnapshotQueryParams {
                workspace: workspace.to_string_lossy().into_owned(),
                snapshot_id: snapshot_id.to_owned(),
            },
            None,
        )
        .await?;
    runtime_api_data(response)?
        .into_iter()
        .map(local_attribution)
        .collect()
}

pub(crate) async fn remote_commit_preview(
    home: &Path,
    workspace: &Path,
    snapshot_id: &str,
    message: &str,
    remote: &str,
    tag: Option<&str>,
) -> Result<CommitPreview> {
    let state = ensure_running(home).await?;
    let response = runtime_client(&state)?
        .call::<_, willdeep_runtime_protocol::DiffCommitPreview>(
            "diff.commit_preview",
            &willdeep_runtime_protocol::DiffCommitPreviewParams {
                workspace: workspace.to_string_lossy().into_owned(),
                snapshot_id: snapshot_id.to_owned(),
                message: message.to_owned(),
                remote: remote.to_owned(),
                tag: tag.map(ToOwned::to_owned),
            },
            None,
        )
        .await?;
    local_commit_preview(runtime_api_data(response)?)
}

pub(crate) async fn remote_revert(
    home: &Path,
    snapshot_id: &str,
    request: &RevertRequest,
) -> Result<RevertResult> {
    let state = ensure_running(home).await?;
    let response = runtime_client(&state)?
        .call::<_, willdeep_runtime_protocol::DiffRevertResult>(
            "diff.revert",
            &willdeep_runtime_protocol::DiffRevertParams {
                workspace: request.workspace.to_string_lossy().into_owned(),
                snapshot_id: snapshot_id.to_owned(),
                path: request.path.clone(),
                area: public_area(request.area),
            },
            None,
        )
        .await?;
    let result = runtime_api_data(response)?;
    Ok(RevertResult {
        previous_snapshot_id: result.previous_snapshot_id,
        current_snapshot_id: result.current_snapshot_id,
        path: result.path,
        recovery_path: result.recovery_path.map(PathBuf::from),
    })
}

pub(crate) async fn remote_record_verification(
    home: &Path,
    workspace: &Path,
    snapshot_id: String,
    verification: willdeep_core::CommandVerification,
) -> Result<DiffVerificationRecord> {
    let state = ensure_running(home).await?;
    let outcome = match verification.status {
        willdeep_core::VerificationStatus::Passed => VerificationOutcome::Passed,
        willdeep_core::VerificationStatus::Failed => VerificationOutcome::Failed,
        willdeep_core::VerificationStatus::TimedOut => VerificationOutcome::TimedOut,
        willdeep_core::VerificationStatus::LaunchFailed => VerificationOutcome::LaunchFailed,
    };
    let response = runtime_client(&state)?
        .call::<_, willdeep_runtime_protocol::DiffVerification>(
            "diff.verification.record",
            &willdeep_runtime_protocol::DiffVerificationParams {
                workspace: workspace.to_string_lossy().into_owned(),
                snapshot_id,
                command: verification.command,
                exit_code: verification.exit_code,
                outcome: match outcome {
                    VerificationOutcome::Passed => {
                        willdeep_runtime_protocol::VerificationOutcome::Passed
                    }
                    VerificationOutcome::Failed => {
                        willdeep_runtime_protocol::VerificationOutcome::Failed
                    }
                    VerificationOutcome::TimedOut => {
                        willdeep_runtime_protocol::VerificationOutcome::TimedOut
                    }
                    VerificationOutcome::LaunchFailed => {
                        willdeep_runtime_protocol::VerificationOutcome::LaunchFailed
                    }
                },
                summary: verification.summary,
            },
            None,
        )
        .await?;
    local_verification(runtime_api_data(response)?)
}

#[derive(Deserialize)]
pub(super) struct DiffQuery {
    workspace: PathBuf,
}

#[derive(Deserialize)]
pub(super) struct DiffContentQuery {
    workspace: PathBuf,
    path: String,
    #[serde(default)]
    area: DiffArea,
}

pub(super) async fn snapshot_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Query(query): Query<DiffQuery>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let workspace = authorized_workspace(&state, &query.workspace).await?;
    snapshot(&workspace)
        .map(Json)
        .map(IntoResponse::into_response)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub(super) async fn content_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(snapshot_id): AxumPath<String>,
    Query(query): Query<DiffContentQuery>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let workspace = authorized_workspace(&state, &query.workspace).await?;
    let current = snapshot(&workspace).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if current.id != snapshot_id {
        return Err(StatusCode::CONFLICT);
    }
    if !current.files.iter().any(|file| file.path == query.path) {
        return Err(StatusCode::NOT_FOUND);
    }
    let content = file_diff(&workspace, &query.path, query.area)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({
        "snapshot_id": current.id,
        "path": query.path,
        "area": query.area,
        "content": content,
    }))
    .into_response())
}

pub(super) async fn reviews_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(snapshot_id): AxumPath<String>,
    Query(query): Query<DiffQuery>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let workspace = authorized_workspace(&state, &query.workspace).await?;
    let _guard = state.diff_review_lock.lock().await;
    let records = load_reviews(&review_store_path(&state.home))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_iter()
        .filter(|record| record.snapshot_id == snapshot_id && record.workspace == workspace)
        .collect::<Vec<_>>();
    Ok(Json(records).into_response())
}

pub(super) async fn review_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(snapshot_id): AxumPath<String>,
    Json(mut request): Json<ReviewRequest>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let workspace = authorized_workspace(&state, &request.workspace).await?;
    let current = exact_snapshot(&workspace, &snapshot_id)?;
    if !current.files.iter().any(|file| file.path == request.path) {
        return Err(StatusCode::NOT_FOUND);
    }
    request.note = normalize_review_note(request.note)?;
    let record = DiffReviewRecord {
        id: uuid::Uuid::new_v4(),
        snapshot_id,
        workspace,
        path: request.path,
        decision: request.decision,
        note: request.note,
        created_at: now(),
    };
    let _guard = state.diff_review_lock.lock().await;
    let path = review_store_path(&state.home);
    let mut records = load_reviews(&path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    records.retain(|existing| {
        !(existing.snapshot_id == record.snapshot_id
            && existing.workspace == record.workspace
            && existing.path == record.path)
    });
    records.push(record.clone());
    write_json_atomic(&path, &records).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((StatusCode::CREATED, Json(record)).into_response())
}

pub(super) async fn revert_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(snapshot_id): AxumPath<String>,
    Json(request): Json<RevertRequest>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let workspace = authorized_workspace(&state, &request.workspace).await?;
    let _guard = state.diff_review_lock.lock().await;
    let current = exact_snapshot(&workspace, &snapshot_id)?;
    let file = current
        .files
        .iter()
        .find(|file| file.path == request.path)
        .ok_or(StatusCode::NOT_FOUND)?;
    if file.kind == DiffFileKind::Unmerged {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    let recovery_path = safe_revert(&state.home, &workspace, file, request.area, &snapshot_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let updated = snapshot(&workspace).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(RevertResult {
        previous_snapshot_id: snapshot_id,
        current_snapshot_id: updated.id,
        path: request.path,
        recovery_path,
    })
    .into_response())
}

pub(super) async fn verifications_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(snapshot_id): AxumPath<String>,
    Query(query): Query<DiffQuery>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let workspace = authorized_workspace(&state, &query.workspace).await?;
    let _guard = state.diff_review_lock.lock().await;
    let records = load_verifications(&verification_store_path(&state.home))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_iter()
        .filter(|record| record.snapshot_id == snapshot_id && record.workspace == workspace)
        .collect::<Vec<_>>();
    Ok(Json(records).into_response())
}

pub(super) async fn attributions_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(snapshot_id): AxumPath<String>,
    Query(query): Query<DiffQuery>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let workspace = authorized_workspace(&state, &query.workspace).await?;
    exact_snapshot(&workspace, &snapshot_id)?;
    let _guard = attribution_store_lock().lock().await;
    let records = attribution_lineage(
        load_attributions(&attribution_store_path(&state.home))
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        &workspace,
        &snapshot_id,
    );
    Ok(Json(records).into_response())
}

pub(super) async fn verification_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(snapshot_id): AxumPath<String>,
    Json(mut request): Json<VerificationRequest>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let workspace = authorized_workspace(&state, &request.workspace).await?;
    exact_snapshot(&workspace, &snapshot_id)?;
    request.command = request.command.trim().to_owned();
    if !is_safe_verification_command(&request.command) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if request.command.len() > 2048 || request.summary.len() > 8192 {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let record = DiffVerificationRecord {
        id: uuid::Uuid::new_v4(),
        snapshot_id,
        workspace,
        command: request.command,
        exit_code: request.exit_code,
        outcome: request.outcome,
        summary: request.summary,
        created_at: now(),
    };
    let _guard = state.diff_review_lock.lock().await;
    let path = verification_store_path(&state.home);
    let mut records = load_verifications(&path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    records.push(record.clone());
    records.drain(..records.len().saturating_sub(500));
    write_json_atomic(&path, &records).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((StatusCode::CREATED, Json(record)).into_response())
}

pub(super) async fn commit_preview_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(snapshot_id): AxumPath<String>,
    Query(query): Query<CommitPreviewQuery>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let workspace = authorized_workspace(&state, &query.workspace).await?;
    let current = exact_snapshot(&workspace, &snapshot_id)?;
    build_commit_preview(&workspace, current, query.message, query.remote, query.tag)
        .map(Json)
        .map(IntoResponse::into_response)
        .map_err(|_| StatusCode::BAD_REQUEST)
}

pub(super) async fn unified_snapshot(
    state: &ServerState,
    params: willdeep_runtime_protocol::DiffSnapshotParams,
) -> Result<willdeep_runtime_protocol::DiffSnapshot, StatusCode> {
    let workspace = authorized_workspace(state, Path::new(&params.workspace)).await?;
    snapshot(&workspace)
        .map(public_snapshot)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub(super) async fn unified_content(
    state: &ServerState,
    params: willdeep_runtime_protocol::DiffContentParams,
) -> Result<willdeep_runtime_protocol::DiffContent, StatusCode> {
    let workspace = authorized_workspace(state, Path::new(&params.workspace)).await?;
    let current = exact_snapshot(&workspace, &params.snapshot_id)?;
    if !current.files.iter().any(|file| file.path == params.path) {
        return Err(StatusCode::NOT_FOUND);
    }
    let area = local_area(params.area);
    let content =
        file_diff(&workspace, &params.path, area).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(willdeep_runtime_protocol::DiffContent {
        snapshot_id: current.id,
        path: params.path,
        area: public_area(area),
        content,
    })
}

pub(super) async fn unified_reviews(
    state: &ServerState,
    params: willdeep_runtime_protocol::DiffSnapshotQueryParams,
) -> Result<Vec<willdeep_runtime_protocol::DiffReview>, StatusCode> {
    let workspace = authorized_workspace(state, Path::new(&params.workspace)).await?;
    let _guard = state.diff_review_lock.lock().await;
    Ok(load_reviews(&review_store_path(&state.home))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_iter()
        .filter(|record| record.snapshot_id == params.snapshot_id && record.workspace == workspace)
        .map(public_review)
        .collect())
}

pub(super) async fn unified_review(
    state: &ServerState,
    params: willdeep_runtime_protocol::DiffReviewParams,
) -> Result<willdeep_runtime_protocol::DiffReview, StatusCode> {
    let workspace = authorized_workspace(state, Path::new(&params.workspace)).await?;
    let current = exact_snapshot(&workspace, &params.snapshot_id)?;
    if !current.files.iter().any(|file| file.path == params.path) {
        return Err(StatusCode::NOT_FOUND);
    }
    let decision = match params.decision {
        willdeep_runtime_protocol::ReviewDecision::Accepted => ReviewDecision::Accepted,
        willdeep_runtime_protocol::ReviewDecision::Rejected => ReviewDecision::Rejected,
        willdeep_runtime_protocol::ReviewDecision::ChangesRequested => {
            ReviewDecision::ChangesRequested
        }
        willdeep_runtime_protocol::ReviewDecision::Reviewed => ReviewDecision::Reviewed,
    };
    let record = DiffReviewRecord {
        id: uuid::Uuid::new_v4(),
        snapshot_id: params.snapshot_id,
        workspace,
        path: params.path,
        decision,
        note: normalize_review_note(params.note)?,
        created_at: now(),
    };
    let _guard = state.diff_review_lock.lock().await;
    let path = review_store_path(&state.home);
    let mut records = load_reviews(&path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    records.retain(|existing| {
        !(existing.snapshot_id == record.snapshot_id
            && existing.workspace == record.workspace
            && existing.path == record.path)
    });
    records.push(record.clone());
    write_json_atomic(&path, &records).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(public_review(record))
}

pub(super) async fn unified_verifications(
    state: &ServerState,
    params: willdeep_runtime_protocol::DiffSnapshotQueryParams,
) -> Result<Vec<willdeep_runtime_protocol::DiffVerification>, StatusCode> {
    let workspace = authorized_workspace(state, Path::new(&params.workspace)).await?;
    let _guard = state.diff_review_lock.lock().await;
    Ok(load_verifications(&verification_store_path(&state.home))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_iter()
        .filter(|record| record.snapshot_id == params.snapshot_id && record.workspace == workspace)
        .map(public_verification)
        .collect())
}

pub(super) async fn unified_record_verification(
    state: &ServerState,
    mut params: willdeep_runtime_protocol::DiffVerificationParams,
) -> Result<willdeep_runtime_protocol::DiffVerification, StatusCode> {
    let workspace = authorized_workspace(state, Path::new(&params.workspace)).await?;
    exact_snapshot(&workspace, &params.snapshot_id)?;
    params.command = params.command.trim().to_owned();
    if !is_safe_verification_command(&params.command) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if params.command.len() > 2048 || params.summary.len() > 8192 {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let record = DiffVerificationRecord {
        id: uuid::Uuid::new_v4(),
        snapshot_id: params.snapshot_id,
        workspace,
        command: params.command,
        exit_code: params.exit_code,
        outcome: match params.outcome {
            willdeep_runtime_protocol::VerificationOutcome::Passed => VerificationOutcome::Passed,
            willdeep_runtime_protocol::VerificationOutcome::Failed => VerificationOutcome::Failed,
            willdeep_runtime_protocol::VerificationOutcome::TimedOut => {
                VerificationOutcome::TimedOut
            }
            willdeep_runtime_protocol::VerificationOutcome::LaunchFailed => {
                VerificationOutcome::LaunchFailed
            }
        },
        summary: params.summary,
        created_at: now(),
    };
    let _guard = state.diff_review_lock.lock().await;
    let path = verification_store_path(&state.home);
    let mut records = load_verifications(&path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    records.push(record.clone());
    records.drain(..records.len().saturating_sub(500));
    write_json_atomic(&path, &records).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(public_verification(record))
}

pub(super) async fn unified_attributions(
    state: &ServerState,
    params: willdeep_runtime_protocol::DiffSnapshotQueryParams,
) -> Result<Vec<willdeep_runtime_protocol::DiffAttribution>, StatusCode> {
    let workspace = authorized_workspace(state, Path::new(&params.workspace)).await?;
    exact_snapshot(&workspace, &params.snapshot_id)?;
    let _guard = attribution_store_lock().lock().await;
    Ok(attribution_lineage(
        load_attributions(&attribution_store_path(&state.home))
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        &workspace,
        &params.snapshot_id,
    )
    .into_iter()
    .map(public_attribution)
    .collect())
}

pub(super) async fn unified_commit_preview(
    state: &ServerState,
    params: willdeep_runtime_protocol::DiffCommitPreviewParams,
) -> Result<willdeep_runtime_protocol::DiffCommitPreview, StatusCode> {
    let workspace = authorized_workspace(state, Path::new(&params.workspace)).await?;
    let current = exact_snapshot(&workspace, &params.snapshot_id)?;
    build_commit_preview(
        &workspace,
        current,
        params.message,
        params.remote,
        params.tag,
    )
    .map(public_commit_preview)
    .map_err(|_| StatusCode::BAD_REQUEST)
}

pub(super) async fn unified_revert(
    state: &ServerState,
    params: willdeep_runtime_protocol::DiffRevertParams,
) -> Result<willdeep_runtime_protocol::DiffRevertResult, StatusCode> {
    let workspace = authorized_workspace(state, Path::new(&params.workspace)).await?;
    let _guard = state.diff_review_lock.lock().await;
    let current = exact_snapshot(&workspace, &params.snapshot_id)?;
    let file = current
        .files
        .iter()
        .find(|file| file.path == params.path)
        .ok_or(StatusCode::NOT_FOUND)?;
    if file.kind == DiffFileKind::Unmerged {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    let recovery_path = safe_revert(
        &state.home,
        &workspace,
        file,
        local_area(params.area),
        &params.snapshot_id,
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let updated = snapshot(&workspace).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(willdeep_runtime_protocol::DiffRevertResult {
        previous_snapshot_id: params.snapshot_id,
        current_snapshot_id: updated.id,
        path: params.path,
        recovery_path: recovery_path.map(|path| path.to_string_lossy().into_owned()),
    })
}

fn exact_snapshot(workspace: &Path, snapshot_id: &str) -> Result<DiffSnapshot, StatusCode> {
    let current = snapshot(workspace).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if current.id != snapshot_id {
        return Err(StatusCode::CONFLICT);
    }
    Ok(current)
}

fn normalize_review_note(note: Option<String>) -> Result<Option<String>, StatusCode> {
    let note = note
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if note.as_ref().is_some_and(|value| value.len() > 4096) {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    Ok(note)
}

fn review_store_path(home: &Path) -> PathBuf {
    home.join("runtime/diff-reviews.json")
}

fn load_reviews(path: &Path) -> Result<Vec<DiffReviewRecord>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

fn verification_store_path(home: &Path) -> PathBuf {
    home.join("runtime/diff-verifications.json")
}

fn attribution_store_lock() -> &'static tokio::sync::Mutex<()> {
    ATTRIBUTION_STORE_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn attribution_store_path(home: &Path) -> PathBuf {
    home.join("runtime/diff-attributions.json")
}

fn load_attributions(path: &Path) -> Result<Vec<DiffAttributionRecord>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

fn attribution_lineage(
    records: Vec<DiffAttributionRecord>,
    workspace: &Path,
    snapshot_id: &str,
) -> Vec<DiffAttributionRecord> {
    let mut frontier = BTreeSet::from([snapshot_id.to_owned()]);
    let mut visited = BTreeSet::new();
    let mut lineage = Vec::new();
    loop {
        let next = records
            .iter()
            .filter(|record| {
                record.workspace == workspace
                    && frontier.contains(&record.after_snapshot_id)
                    && visited.insert(record.id)
            })
            .cloned()
            .collect::<Vec<_>>();
        if next.is_empty() {
            break;
        }
        frontier.clear();
        frontier.extend(next.iter().map(|record| record.before_snapshot_id.clone()));
        lineage.extend(next);
    }
    lineage.reverse();
    lineage
}

fn load_verifications(path: &Path) -> Result<Vec<DiffVerificationRecord>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

fn is_safe_verification_command(command: &str) -> bool {
    let uppercase = command.to_ascii_uppercase();
    if ["API_KEY", "TOKEN=", "SECRET=", "PASSWORD=", "AUTHORIZATION"]
        .iter()
        .any(|marker| uppercase.contains(marker))
    {
        return false;
    }
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

fn default_remote() -> String {
    "origin".to_owned()
}

fn build_commit_preview(
    workspace: &Path,
    snapshot: DiffSnapshot,
    message: String,
    remote: String,
    tag: Option<String>,
) -> Result<CommitPreview> {
    let message = message.trim().to_owned();
    let remote = remote.trim().to_owned();
    if message.len() > 2000 || !valid_git_name(&remote) {
        bail!("invalid commit preview input");
    }
    let tag = tag
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let staged_files = snapshot
        .files
        .iter()
        .filter(|file| file.staged)
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    let unstaged_files = snapshot
        .files
        .iter()
        .filter(|file| file.unstaged)
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    let branch = git(workspace, &["branch", "--show-current"])
        .ok()
        .map(|value| String::from_utf8_lossy(&value).trim().to_owned())
        .filter(|value| !value.is_empty());
    let remote_key = format!("remote.{remote}.url");
    let remote_url = git(workspace, &["config", "--get", &remote_key])
        .ok()
        .map(|value| sanitize_remote_url(String::from_utf8_lossy(&value).trim()))
        .filter(|value| !value.is_empty());
    let sensitive_findings = scan_sensitive_staged_files(workspace, &snapshot.files);
    let mut blockers = Vec::new();
    if message.is_empty() {
        blockers.push("commit message is empty".to_owned());
    }
    if staged_files.is_empty() {
        blockers.push("no staged files".to_owned());
    }
    if snapshot.has_conflicts {
        blockers.push("workspace has unresolved conflicts".to_owned());
    }
    if branch.is_none() {
        blockers.push("detached HEAD has no push branch".to_owned());
    }
    if remote_url.is_none() {
        blockers.push(format!("remote {remote} is not configured"));
    }
    if tag.as_ref().is_some_and(|value| !valid_release_tag(value)) {
        blockers.push("tag is not a valid vMAJOR.MINOR.PATCH release tag".to_owned());
    }
    if sensitive_findings
        .iter()
        .any(|finding| finding.severity == FindingSeverity::Blocker)
    {
        blockers.push("staged changes contain sensitive material".to_owned());
    }
    let push_target = remote_url.as_ref().and_then(|url| {
        branch
            .as_ref()
            .map(|branch| format!("{remote} ({url}) → refs/heads/{branch}"))
    });
    Ok(CommitPreview {
        snapshot_id: snapshot.id,
        workspace: workspace.to_path_buf(),
        branch,
        head: snapshot.head,
        message,
        staged_files,
        unstaged_files,
        sensitive_findings,
        remote,
        push_target,
        tag,
        blockers,
        requires_confirmation: true,
    })
}

fn valid_git_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
}

fn valid_release_tag(value: &str) -> bool {
    let Some(version) = value.strip_prefix('v') else {
        return false;
    };
    let core = version.split_once('-').map_or(version, |(core, _)| core);
    let parts = core.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty() && part.chars().all(|character| character.is_ascii_digit())
        })
        && version
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || ".-".contains(character))
}

fn sanitize_remote_url(value: &str) -> String {
    let without_query = value.split(['?', '#']).next().unwrap_or(value);
    let Some((scheme, rest)) = without_query.split_once("://") else {
        return without_query.to_owned();
    };
    let host_path = rest.rsplit_once('@').map_or(rest, |(_, safe)| safe);
    format!("{scheme}://{host_path}")
}

fn scan_sensitive_staged_files(workspace: &Path, files: &[DiffFile]) -> Vec<SensitiveFinding> {
    let mut findings = Vec::new();
    for file in files.iter().filter(|file| file.staged) {
        let lower_path = file.path.to_ascii_lowercase();
        if sensitive_path(&lower_path) {
            findings.push(SensitiveFinding {
                path: file.path.clone(),
                code: "sensitive_path".to_owned(),
                severity: FindingSeverity::Blocker,
            });
        }
        let spec = format!(":{}", file.path);
        let Ok(content) = git(workspace, &["show", &spec]) else {
            continue;
        };
        if content.len() > 1024 * 1024 || content.contains(&0) {
            continue;
        }
        let text = String::from_utf8_lossy(&content);
        if sensitive_content(&text) {
            findings.push(SensitiveFinding {
                path: file.path.clone(),
                code: "credential_material".to_owned(),
                severity: FindingSeverity::Blocker,
            });
        }
    }
    findings.sort_by(|left, right| left.path.cmp(&right.path).then(left.code.cmp(&right.code)));
    findings.dedup_by(|left, right| left.path == right.path && left.code == right.code);
    findings
}

fn sensitive_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    (name == ".env"
        || (name.starts_with(".env.") && !name.ends_with(".example") && !name.ends_with(".sample")))
        || [".pem", ".key", ".p12", ".pfx"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
        || matches!(
            name,
            "id_rsa" | "id_ed25519" | "credentials" | "credentials.json"
        )
}

fn sensitive_content(value: &str) -> bool {
    let uppercase = value.to_ascii_uppercase();
    if uppercase.contains("-----BEGIN PRIVATE KEY-----")
        || uppercase.contains("-----BEGIN RSA PRIVATE KEY-----")
    {
        return true;
    }
    value.lines().any(|line| {
        let trimmed = line.trim();
        let uppercase = trimmed.to_ascii_uppercase();
        let assignment = [
            "API_KEY",
            "ACCESS_TOKEN",
            "AUTH_TOKEN",
            "PASSWORD",
            "SECRET_KEY",
        ]
        .iter()
        .any(|name| uppercase.starts_with(name));
        assignment
            && (trimmed.contains('=') || trimmed.contains(':'))
            && !trimmed.contains("${")
            && !uppercase.contains("ENV:")
            && !uppercase.contains("EXAMPLE")
            && !uppercase.contains("REPLACE_ME")
    })
}

async fn authorized_workspace(
    state: &ServerState,
    requested: &Path,
) -> Result<PathBuf, StatusCode> {
    let requested = requested
        .canonicalize()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let registered = state
        .workspaces
        .list()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .iter()
        .any(|workspace| workspace.root == requested);
    let session_allowed = state
        .sessions
        .list()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .iter()
        .any(|session| session.workspace == requested);
    let task_allowed = state
        .tasks
        .list()
        .await
        .iter()
        .any(|task| task.workspace == requested);
    if registered || session_allowed || task_allowed {
        Ok(requested)
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

pub(crate) fn snapshot(workspace: &Path) -> Result<DiffSnapshot> {
    let status = git(
        workspace,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    let mut files = parse_status(&status);
    apply_numstat(workspace, false, &mut files)?;
    apply_numstat(workspace, true, &mut files)?;
    enrich_untracked(workspace, &mut files);
    let mut files = files.into_values().collect::<Vec<_>>();
    files.sort_by(|left, right| left.path.cmp(&right.path));
    let additions = files.iter().map(|file| file.additions).sum();
    let deletions = files.iter().map(|file| file.deletions).sum();
    let has_conflicts = files.iter().any(|file| file.kind == DiffFileKind::Unmerged);
    let head = git(workspace, &["rev-parse", "HEAD"])
        .ok()
        .map(|value| String::from_utf8_lossy(&value).trim().to_owned())
        .filter(|value| !value.is_empty());
    Ok(DiffSnapshot {
        id: snapshot_id(workspace, &status)?,
        workspace: workspace.to_path_buf(),
        head,
        files,
        additions,
        deletions,
        has_conflicts,
    })
}

pub(crate) fn capture(workspace: &Path) -> Result<DiffCapture> {
    let snapshot = snapshot(workspace)?;
    let mut fingerprints = BTreeMap::new();
    for file in &snapshot.files {
        let mut hasher = DefaultHasher::new();
        file.hash(&mut hasher);
        git(
            workspace,
            &["diff", "--binary", "--no-ext-diff", "--", &file.path],
        )?
        .hash(&mut hasher);
        git(
            workspace,
            &[
                "diff",
                "--cached",
                "--binary",
                "--no-ext-diff",
                "--",
                &file.path,
            ],
        )?
        .hash(&mut hasher);
        if file.kind == DiffFileKind::Untracked {
            std::fs::read(workspace.join(&file.path))?.hash(&mut hasher);
        }
        fingerprints.insert(file.path.clone(), hasher.finish());
    }
    Ok(DiffCapture {
        snapshot,
        fingerprints,
    })
}

pub(crate) async fn record_tool_attribution(
    home: &Path,
    before: DiffCapture,
    workspace: &Path,
    context: AttributionContext,
) -> Result<Option<DiffAttributionRecord>> {
    let after = capture(workspace)?;
    if before.snapshot.id == after.snapshot.id {
        return Ok(None);
    }
    let mut paths = before
        .fingerprints
        .keys()
        .chain(after.fingerprints.keys())
        .filter(|path| before.fingerprints.get(*path) != after.fingerprints.get(*path))
        .cloned()
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    if paths.is_empty() {
        return Ok(None);
    }
    let record = DiffAttributionRecord {
        id: uuid::Uuid::new_v4(),
        before_snapshot_id: before.snapshot.id,
        after_snapshot_id: after.snapshot.id,
        workspace: workspace.to_path_buf(),
        session_id: context.session_id,
        turn_id: context.turn_id,
        task_id: context.task_id,
        agent_id: context.agent_id,
        tool: context.tool,
        paths,
        confidence: AttributionConfidence::ToolWindow,
        created_at: now(),
    };
    let _guard = attribution_store_lock().lock().await;
    let path = attribution_store_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut records = load_attributions(&path)?;
    records.push(record.clone());
    records.drain(..records.len().saturating_sub(1000));
    write_json_atomic(&path, &records)?;
    Ok(Some(record))
}

pub(crate) fn workspace_change_artifacts(
    home: &Path,
    params: willdeep_runtime_protocol::ListArtifactsParams,
) -> Result<Vec<willdeep_runtime_protocol::RuntimeArtifact>> {
    let limit = params.limit.unwrap_or(200).clamp(1, 1_000);
    let mut records = load_attributions(&attribution_store_path(home))?;
    records.reverse();
    let mut artifacts = records
        .into_iter()
        .filter(|record| {
            params
                .session_id
                .is_none_or(|id| record.session_id == Some(id))
        })
        .filter(|record| params.turn_id.is_none_or(|id| record.turn_id == Some(id)))
        .filter(|record| params.task_id.is_none_or(|id| record.task_id == id))
        .filter(|record| params.agent_id.is_none_or(|id| record.agent_id == id))
        .map(|record| willdeep_runtime_protocol::RuntimeArtifact {
            id: record.id,
            kind: willdeep_runtime_protocol::ArtifactKind::WorkspaceChange,
            session_id: record.session_id,
            turn_id: record.turn_id,
            task_id: record.task_id,
            agent_id: record.agent_id,
            title: format!("{} workspace changes", record.tool),
            source_id: record.after_snapshot_id,
            item_count: record.paths.len(),
            created_at: record.created_at,
        })
        .filter(|artifact| params.kind.is_none_or(|kind| artifact.kind == kind))
        .collect::<Vec<_>>();
    artifacts.truncate(limit);
    Ok(artifacts)
}

fn enrich_untracked(workspace: &Path, files: &mut BTreeMap<String, DiffFile>) {
    for file in files
        .values_mut()
        .filter(|file| file.kind == DiffFileKind::Untracked)
    {
        let Ok(data) = std::fs::read(workspace.join(&file.path)) else {
            continue;
        };
        file.binary = data.contains(&0);
        if !file.binary {
            file.additions = String::from_utf8_lossy(&data).lines().count() as u64;
        }
    }
}

fn parse_status(status: &[u8]) -> BTreeMap<String, DiffFile> {
    let mut records = status
        .split(|byte| *byte == 0)
        .filter(|value| !value.is_empty());
    let mut files = BTreeMap::new();
    while let Some(record) = records.next() {
        if record.len() < 4 {
            continue;
        }
        let x = record[0] as char;
        let y = record[1] as char;
        let path = String::from_utf8_lossy(&record[3..]).into_owned();
        let renamed = matches!(x, 'R' | 'C') || matches!(y, 'R' | 'C');
        let old_path = renamed
            .then(|| records.next())
            .flatten()
            .map(|value| String::from_utf8_lossy(value).into_owned());
        let kind = status_kind(x, y);
        files.insert(
            path.clone(),
            DiffFile {
                path,
                old_path,
                kind,
                staged: x != ' ' && x != '?',
                unstaged: y != ' ' || x == '?',
                binary: false,
                additions: 0,
                deletions: 0,
            },
        );
    }
    files
}

fn status_kind(x: char, y: char) -> DiffFileKind {
    if x == '?' {
        return DiffFileKind::Untracked;
    }
    if matches!(
        (x, y),
        ('D', 'D') | ('A', 'U') | ('U', 'D') | ('U', 'A') | ('D', 'U') | ('A', 'A') | ('U', 'U')
    ) {
        return DiffFileKind::Unmerged;
    }
    let code = if y != ' ' { y } else { x };
    match code {
        'A' => DiffFileKind::Added,
        'D' => DiffFileKind::Deleted,
        'R' => DiffFileKind::Renamed,
        'C' => DiffFileKind::Copied,
        _ => DiffFileKind::Modified,
    }
}

fn apply_numstat(
    workspace: &Path,
    staged: bool,
    files: &mut BTreeMap<String, DiffFile>,
) -> Result<()> {
    let mut args = vec!["diff", "--numstat", "-z", "--no-ext-diff"];
    if staged {
        args.push("--cached");
    }
    let output = git(workspace, &args)?;
    for record in output
        .split(|byte| *byte == 0)
        .filter(|value| !value.is_empty())
    {
        let Some(first_tab) = record.iter().position(|byte| *byte == b'\t') else {
            continue;
        };
        let Some(second_relative) = record[first_tab + 1..]
            .iter()
            .position(|byte| *byte == b'\t')
        else {
            continue;
        };
        let second_tab = first_tab + 1 + second_relative;
        let path = String::from_utf8_lossy(&record[second_tab + 1..]).into_owned();
        let Some(file) = files.get_mut(&path) else {
            continue;
        };
        let additions = &record[..first_tab];
        let deletions = &record[first_tab + 1..second_tab];
        file.binary |= additions == b"-" || deletions == b"-";
        file.additions = file
            .additions
            .saturating_add(String::from_utf8_lossy(additions).parse().unwrap_or(0));
        file.deletions = file
            .deletions
            .saturating_add(String::from_utf8_lossy(deletions).parse().unwrap_or(0));
    }
    Ok(())
}

fn snapshot_id(workspace: &Path, status: &[u8]) -> Result<String> {
    let mut hasher = DefaultHasher::new();
    status.hash(&mut hasher);
    git(workspace, &["diff", "--binary", "--no-ext-diff"])?.hash(&mut hasher);
    git(
        workspace,
        &["diff", "--cached", "--binary", "--no-ext-diff"],
    )?
    .hash(&mut hasher);
    for file in parse_status(status)
        .into_values()
        .filter(|file| file.kind == DiffFileKind::Untracked)
    {
        file.path.hash(&mut hasher);
        let data = std::fs::read(workspace.join(&file.path))?;
        data.hash(&mut hasher);
    }
    Ok(format!("diff-{:016x}", hasher.finish()))
}

fn safe_revert(
    home: &Path,
    workspace: &Path,
    file: &DiffFile,
    area: DiffArea,
    snapshot_id: &str,
) -> Result<Option<PathBuf>> {
    safe_workspace_path(workspace, &file.path)?;
    if let Some(old_path) = &file.old_path {
        safe_workspace_path(workspace, old_path)?;
    }
    let paths = std::iter::once(file.path.as_str())
        .chain(file.old_path.as_deref())
        .collect::<Vec<_>>();
    if matches!(area, DiffArea::Staged | DiffArea::Combined) && file.staged {
        reset_index(workspace, &paths, snapshot(workspace)?.head.is_some())?;
    }
    if area == DiffArea::Staged {
        return Ok(None);
    }

    let recovery_root = home
        .join("runtime/recovery")
        .join(format!("{snapshot_id}-{}", uuid::Uuid::new_v4().simple()));
    let mut recovered = false;
    for path in paths {
        if tracked_in_head(workspace, path) {
            git(
                workspace,
                &["restore", "--worktree", "--source=HEAD", "--", path],
            )?;
            continue;
        }
        let source = safe_workspace_path(workspace, path)?;
        if !source.exists() {
            continue;
        }
        let destination = recovery_root.join(path);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&source, &destination)?;
        recovered = true;
    }
    Ok(recovered.then_some(recovery_root))
}

fn reset_index(workspace: &Path, paths: &[&str], has_head: bool) -> Result<()> {
    let mut args = if has_head {
        vec!["restore", "--staged", "--source=HEAD", "--"]
    } else {
        vec!["rm", "--cached", "--ignore-unmatch", "--"]
    };
    args.extend_from_slice(paths);
    git(workspace, &args).map(|_| ())
}

fn tracked_in_head(workspace: &Path, path: &str) -> bool {
    Command::new("git")
        .args(["cat-file", "-e", &format!("HEAD:{path}")])
        .current_dir(workspace)
        .output()
        .is_ok_and(|output| output.status.success())
}

fn safe_workspace_path(workspace: &Path, path: &str) -> Result<PathBuf> {
    let relative = Path::new(path);
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("Diff path escapes Workspace");
    }
    let root = workspace.canonicalize()?;
    let candidate = root.join(relative);
    let safe_path = candidate.canonicalize().unwrap_or(candidate);
    if !safe_path.starts_with(&root) {
        bail!("Diff path escapes Workspace");
    }
    Ok(safe_path)
}

fn file_diff(workspace: &Path, path: &str, area: DiffArea) -> Result<String> {
    let safe_path = safe_workspace_path(workspace, path)?;
    let mut output = Vec::new();
    if matches!(area, DiffArea::Staged | DiffArea::Combined) {
        output.extend(git(
            workspace,
            &["diff", "--cached", "--no-ext-diff", "--", path],
        )?);
    }
    if matches!(area, DiffArea::Unstaged | DiffArea::Combined) {
        output.extend(git(workspace, &["diff", "--no-ext-diff", "--", path])?);
    }
    if output.is_empty() && safe_path.is_file() {
        let data = std::fs::read(&safe_path)?;
        if data.contains(&0) {
            return Ok("Binary untracked file".to_owned());
        }
        let text = String::from_utf8_lossy(&data);
        output.extend(
            format!(
                "--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{} @@\n",
                text.lines().count()
            )
            .bytes(),
        );
        for line in text.lines() {
            output.extend(format!("+{line}\n").bytes());
        }
    }
    if output.len() > MAX_DIFF_BYTES {
        output.truncate(MAX_DIFF_BYTES);
        while std::str::from_utf8(&output).is_err() {
            output.pop();
        }
        output.extend(b"\n[diff truncated]");
    }
    Ok(String::from_utf8_lossy(&output).into_owned())
}

fn git(workspace: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn snapshot_tracks_staged_unstaged_untracked_and_content_changes() {
        let root = std::env::temp_dir().join(format!("willdeep-diff-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("workspace");
        run_git(&root, &["init"]);
        run_git(&root, &["config", "user.email", "test@willdeep.invalid"]);
        run_git(&root, &["config", "user.name", "WillDeep Test"]);
        std::fs::write(root.join("tracked.txt"), "one\n").expect("seed");
        run_git(&root, &["add", "tracked.txt"]);
        run_git(&root, &["commit", "-m", "seed"]);

        std::fs::write(root.join("tracked.txt"), "one\ntwo\n").expect("modify");
        run_git(&root, &["add", "tracked.txt"]);
        std::fs::write(root.join("new.txt"), "alpha\nbeta\n").expect("untracked");
        let first = snapshot(&root).expect("snapshot");
        assert_eq!(first.files.len(), 2);
        assert_eq!(first.additions, 3);
        assert!(
            first
                .files
                .iter()
                .any(|file| { file.path == "tracked.txt" && file.staged && file.additions == 1 })
        );
        assert!(first.files.iter().any(|file| {
            file.path == "new.txt" && file.kind == DiffFileKind::Untracked && file.additions == 2
        }));
        let content = file_diff(&root, "new.txt", DiffArea::Combined).expect("content");
        assert!(content.contains("+alpha"));

        std::fs::write(root.join("new.txt"), "alpha\nbeta\ngamma\n").expect("change");
        let second = snapshot(&root).expect("changed snapshot");
        assert_ne!(first.id, second.id);
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn file_diff_rejects_paths_outside_workspace() {
        let root = std::env::temp_dir().join(format!("willdeep-diff-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("workspace");
        run_git(&root, &["init"]);

        let error = file_diff(&root, "../outside.txt", DiffArea::Combined)
            .expect_err("parent traversal must fail");
        assert!(error.to_string().contains("escapes Workspace"));
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn safe_revert_requires_exact_content_and_restores_tracked_file() {
        let root = std::env::temp_dir().join(format!("willdeep-revert-{}", uuid::Uuid::new_v4()));
        let home = root.join("home");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        run_git(&workspace, &["init"]);
        run_git(
            &workspace,
            &["config", "user.email", "test@willdeep.invalid"],
        );
        run_git(&workspace, &["config", "user.name", "WillDeep Test"]);
        std::fs::write(workspace.join("tracked.txt"), "original\n").expect("seed");
        run_git(&workspace, &["add", "tracked.txt"]);
        run_git(&workspace, &["commit", "-m", "seed"]);
        std::fs::write(workspace.join("tracked.txt"), "staged\n").expect("staged");
        run_git(&workspace, &["add", "tracked.txt"]);
        std::fs::write(workspace.join("tracked.txt"), "unstaged\n").expect("unstaged");

        let before = snapshot(&workspace).expect("snapshot");
        let file = before
            .files
            .iter()
            .find(|file| file.path == "tracked.txt")
            .expect("tracked change");
        let recovery = safe_revert(&home, &workspace, file, DiffArea::Combined, &before.id)
            .expect("safe revert");

        assert!(recovery.is_none());
        assert_eq!(
            std::fs::read_to_string(workspace.join("tracked.txt")).unwrap(),
            "original\n"
        );
        assert!(snapshot(&workspace).unwrap().files.is_empty());
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn safe_revert_moves_untracked_file_to_recovery() {
        let root = std::env::temp_dir().join(format!("willdeep-revert-{}", uuid::Uuid::new_v4()));
        let home = root.join("home");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        run_git(&workspace, &["init"]);
        std::fs::write(workspace.join("draft.txt"), "recover me\n").expect("draft");

        let before = snapshot(&workspace).expect("snapshot");
        let file = before.files.first().expect("untracked file");
        let recovery = safe_revert(&home, &workspace, file, DiffArea::Combined, &before.id)
            .expect("safe revert")
            .expect("recovery path");

        assert!(!workspace.join("draft.txt").exists());
        assert_eq!(
            std::fs::read_to_string(recovery.join("draft.txt")).unwrap(),
            "recover me\n"
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn exact_snapshot_rejects_changes_made_after_review_opened() {
        let root = std::env::temp_dir().join(format!("willdeep-stale-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("workspace");
        run_git(&root, &["init"]);
        std::fs::write(root.join("draft.txt"), "first\n").expect("first");
        let opened = snapshot(&root).expect("opened snapshot");
        std::fs::write(root.join("draft.txt"), "second\n").expect("changed");

        assert_eq!(
            exact_snapshot(&root, &opened.id).expect_err("stale snapshot must fail"),
            StatusCode::CONFLICT
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn commit_preview_redacts_remote_credentials_and_blocks_sensitive_stage() {
        let root = std::env::temp_dir().join(format!("willdeep-preview-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("workspace");
        run_git(&root, &["init"]);
        run_git(&root, &["config", "user.email", "test@willdeep.invalid"]);
        run_git(&root, &["config", "user.name", "WillDeep Test"]);
        run_git(
            &root,
            &[
                "remote",
                "add",
                "origin",
                "https://token-value@github.com/example/project.git",
            ],
        );
        std::fs::write(root.join(".env"), "API_KEY=real-secret\n").expect("secret");
        run_git(&root, &["add", ".env"]);
        let snapshot = snapshot(&root).expect("snapshot");

        let preview = build_commit_preview(
            &root,
            snapshot,
            "feat: preview".to_owned(),
            "origin".to_owned(),
            Some("v1.2.3-rc1".to_owned()),
        )
        .expect("preview");

        assert!(
            preview
                .push_target
                .as_deref()
                .is_some_and(|target| target.contains("https://github.com/example/project.git"))
        );
        assert!(!preview.push_target.unwrap().contains("token-value"));
        assert!(!preview.sensitive_findings.is_empty());
        assert!(
            preview
                .blockers
                .iter()
                .any(|value| value.contains("sensitive"))
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn release_tag_validation_is_semver_shaped() {
        assert!(valid_release_tag("v1.2.3"));
        assert!(valid_release_tag("v1.2.3-rc4"));
        assert!(!valid_release_tag("1.2.3"));
        assert!(!valid_release_tag("v1.2"));
        assert!(!valid_release_tag("v1.2.3;push"));
    }

    #[tokio::test]
    async fn tool_attribution_records_only_paths_changed_inside_the_tool_window() {
        let root =
            std::env::temp_dir().join(format!("willdeep-attribution-{}", uuid::Uuid::new_v4()));
        let home = root.join("home");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&workspace).expect("workspace");
        run_git(&workspace, &["init"]);
        run_git(
            &workspace,
            &["config", "user.email", "test@willdeep.invalid"],
        );
        run_git(&workspace, &["config", "user.name", "WillDeep Test"]);
        std::fs::write(workspace.join("existing.txt"), "base\n").expect("base");
        run_git(&workspace, &["add", "existing.txt"]);
        run_git(&workspace, &["commit", "-m", "base"]);
        std::fs::write(workspace.join("existing.txt"), "dirty before tool\n")
            .expect("preexisting dirty change");
        let before = capture(&workspace).expect("before capture");
        std::fs::write(workspace.join("created-by-agent.txt"), "new\n")
            .expect("agent-created file");
        let session_id = uuid::Uuid::new_v4();
        let turn_id = uuid::Uuid::new_v4();
        let task_id = uuid::Uuid::new_v4();
        let agent_id = uuid::Uuid::new_v4();

        let record = record_tool_attribution(
            &home,
            before,
            &workspace,
            AttributionContext {
                session_id: Some(session_id),
                turn_id: Some(turn_id),
                task_id,
                agent_id,
                tool: "create_file".to_owned(),
            },
        )
        .await
        .expect("record attribution")
        .expect("changed workspace");

        assert_eq!(record.paths, vec!["created-by-agent.txt"]);
        assert_eq!(record.session_id, Some(session_id));
        assert_eq!(record.turn_id, Some(turn_id));
        assert_eq!(record.task_id, task_id);
        assert_eq!(record.agent_id, agent_id);
        assert_eq!(record.tool, "create_file");
        let second_before = capture(&workspace).expect("second capture");
        std::fs::write(workspace.join("second.txt"), "second\n").expect("second file");
        let second = record_tool_attribution(
            &home,
            second_before,
            &workspace,
            AttributionContext {
                session_id: Some(session_id),
                turn_id: Some(turn_id),
                task_id,
                agent_id,
                tool: "edit_file".to_owned(),
            },
        )
        .await
        .expect("second attribution")
        .expect("second changed workspace");
        let stored = load_attributions(&attribution_store_path(&home)).expect("load records");
        assert_eq!(
            attribution_lineage(stored, &workspace, &second.after_snapshot_id)
                .iter()
                .map(|record| record.paths.clone())
                .collect::<Vec<_>>(),
            vec![vec!["created-by-agent.txt"], vec!["second.txt"]]
        );
        let artifacts = workspace_change_artifacts(
            &home,
            willdeep_runtime_protocol::ListArtifactsParams {
                session_id: Some(session_id),
                ..Default::default()
            },
        )
        .expect("workspace change artifacts");
        assert_eq!(artifacts.len(), 2);
        assert_eq!(artifacts[0].source_id, second.after_snapshot_id);
        assert_eq!(artifacts[0].item_count, 1);
        let public_json = serde_json::to_string(&artifacts).unwrap();
        assert!(!public_json.contains("created-by-agent.txt"));
        assert!(!public_json.contains(workspace.to_string_lossy().as_ref()));
        std::fs::remove_dir_all(root).expect("cleanup");
    }
}
