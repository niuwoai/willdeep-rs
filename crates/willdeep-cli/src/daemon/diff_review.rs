use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Component;

use super::*;

const MAX_DIFF_BYTES: usize = 512 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
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
    let response = client()
        .get(format!("http://{}/v1/diffs", state.address))
        .header(TOKEN_HEADER, &state.token)
        .query(&[("workspace", workspace.display().to_string())])
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("Runtime rejected Diff snapshot: {}", response.text().await?);
    }
    Ok(response.json().await?)
}

pub(crate) async fn remote_content(
    home: &Path,
    workspace: &Path,
    snapshot_id: &str,
    path: &str,
    area: DiffArea,
) -> Result<String> {
    let state = ensure_running(home).await?;
    let response = client()
        .get(format!(
            "http://{}/v1/diffs/{snapshot_id}/content",
            state.address
        ))
        .header(TOKEN_HEADER, &state.token)
        .query(&[
            ("workspace", workspace.display().to_string()),
            ("path", path.to_owned()),
            ("area", area_name(area).to_owned()),
        ])
        .send()
        .await?;
    if response.status() == StatusCode::CONFLICT {
        bail!("Diff snapshot changed; reopen /diff before reviewing");
    }
    if !response.status().is_success() {
        bail!("Runtime rejected Diff content: {}", response.text().await?);
    }
    let value: serde_json::Value = response.json().await?;
    Ok(value
        .get("content")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_owned())
}

pub(crate) async fn remote_review(
    home: &Path,
    snapshot_id: &str,
    request: &ReviewRequest,
) -> Result<DiffReviewRecord> {
    let state = ensure_running(home).await?;
    let response = client()
        .post(format!(
            "http://{}/v1/diffs/{snapshot_id}/reviews",
            state.address
        ))
        .header(TOKEN_HEADER, &state.token)
        .json(request)
        .send()
        .await?;
    if response.status() == StatusCode::CONFLICT {
        bail!("Diff snapshot changed; reopen /diff before reviewing");
    }
    if !response.status().is_success() {
        bail!("Runtime rejected Diff review: {}", response.text().await?);
    }
    Ok(response.json().await?)
}

pub(crate) async fn remote_reviews(
    home: &Path,
    workspace: &Path,
    snapshot_id: &str,
) -> Result<Vec<DiffReviewRecord>> {
    let state = ensure_running(home).await?;
    let response = client()
        .get(format!(
            "http://{}/v1/diffs/{snapshot_id}/reviews",
            state.address
        ))
        .header(TOKEN_HEADER, &state.token)
        .query(&[("workspace", workspace.display().to_string())])
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("Runtime rejected Diff reviews: {}", response.text().await?);
    }
    Ok(response.json().await?)
}

pub(crate) async fn remote_verifications(
    home: &Path,
    workspace: &Path,
    snapshot_id: &str,
) -> Result<Vec<DiffVerificationRecord>> {
    let state = ensure_running(home).await?;
    let response = client()
        .get(format!(
            "http://{}/v1/diffs/{snapshot_id}/verifications",
            state.address
        ))
        .header(TOKEN_HEADER, &state.token)
        .query(&[("workspace", workspace.display().to_string())])
        .send()
        .await?;
    if !response.status().is_success() {
        bail!(
            "Runtime rejected Diff verifications: {}",
            response.text().await?
        );
    }
    Ok(response.json().await?)
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
    let mut query = vec![
        ("workspace", workspace.display().to_string()),
        ("message", message.to_owned()),
        ("remote", remote.to_owned()),
    ];
    if let Some(tag) = tag {
        query.push(("tag", tag.to_owned()));
    }
    let response = client()
        .get(format!(
            "http://{}/v1/diffs/{snapshot_id}/commit-preview",
            state.address
        ))
        .header(TOKEN_HEADER, &state.token)
        .query(&query)
        .send()
        .await?;
    if response.status() == StatusCode::CONFLICT {
        bail!("Diff snapshot changed; reopen /diff before commit preview");
    }
    if !response.status().is_success() {
        bail!(
            "Runtime rejected Commit Preview: {}",
            response.text().await?
        );
    }
    Ok(response.json().await?)
}

pub(crate) async fn remote_revert(
    home: &Path,
    snapshot_id: &str,
    request: &RevertRequest,
) -> Result<RevertResult> {
    let state = ensure_running(home).await?;
    let response = client()
        .post(format!(
            "http://{}/v1/diffs/{snapshot_id}/revert",
            state.address
        ))
        .header(TOKEN_HEADER, &state.token)
        .json(request)
        .send()
        .await?;
    if response.status() == StatusCode::CONFLICT {
        bail!("Diff snapshot changed; reopen /diff before reverting");
    }
    if !response.status().is_success() {
        bail!("Runtime rejected Diff revert: {}", response.text().await?);
    }
    Ok(response.json().await?)
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
    let response = client()
        .post(format!(
            "http://{}/v1/diffs/{}/verifications",
            state.address, snapshot_id
        ))
        .header(TOKEN_HEADER, &state.token)
        .json(&VerificationRequest {
            workspace: workspace.to_path_buf(),
            command: verification.command,
            exit_code: verification.exit_code,
            outcome,
            summary: verification.summary,
        })
        .send()
        .await?;
    if response.status() == StatusCode::CONFLICT {
        bail!("Workspace changed before verification could be bound to its Diff");
    }
    if !response.status().is_success() {
        bail!(
            "Runtime rejected Diff verification: {}",
            response.text().await?
        );
    }
    Ok(response.json().await?)
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
    if session_allowed || task_allowed {
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
}
