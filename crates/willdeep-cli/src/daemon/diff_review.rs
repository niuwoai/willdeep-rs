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

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DiffArea {
    Staged,
    Unstaged,
    #[default]
    Combined,
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

fn file_diff(workspace: &Path, path: &str, area: DiffArea) -> Result<String> {
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
}
