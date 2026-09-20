//! 工作区检查点：每个 Runtime Turn 开始前给工作树拍一张内容快照，回退时按它恢复文件。
//!
//! Diff 快照只存指纹（哪些文件动了），`diff.revert` 只会回到 git HEAD；「回到第 N 步」
//! 要的是**那一步当时的内容**，所以这里真的存内容。存法是一个**私有的影子 git 仓库**
//! （`$WILLDEEP_HOME/runtime/checkpoints/<工作区哈希>/`）：
//!
//! - 对象库通过 `objects/info/alternates` 借用工作区自己的 `.git/objects`，没改过的
//!   文件一个字节都不重复存，只有新内容才写进影子仓库；
//! - 引用放在 `refs/willdeep/<会话>/<轮次>`，全在影子仓库里，用户的 `git log --all`、
//!   `git status`、索引和 HEAD 一概不受影响；
//! - 每份快照是一个没有父提交的独立 commit：回退只认「那一刻的树」，不认历史。
//!
//! 恢复走「先备份、后覆盖」：被覆盖或删除的当前文件先原样进 `runtime/recovery/`（与
//! 安全撤销同一个回收区，审计导出能看见），同时把回退前的整棵树也拍成一份快照，
//! 一次回退本身也是可回退的。
//!
//! 不是 git 仓库的工作区没有检查点：回退只能回对话，不能回文件，调用方要把这话说给人听。

use std::collections::BTreeSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// 关掉检查点的环境变量：`0` / `false` / `off` 即关。默认开。
pub(crate) const ENV_TOGGLE: &str = "WILLDEEP_WORKSPACE_CHECKPOINTS";

/// 未跟踪文件超过这个数就只拍已跟踪文件。GOCACHE 之类指进仓库的缓存目录会把
/// 未跟踪列表摊成上万个文件，为它们算哈希不值，也不该让一轮开始等上半分钟。
const MAX_UNTRACKED_FILES: usize = 2000;

/// 单个未跟踪文件超过这个大小就不进快照：大二进制多半是构建产物，回退也不该碰它。
const MAX_UNTRACKED_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// 每个工作区最多留多少份检查点；超了删最老的引用。对象本身不主动清，
/// 需要时对影子仓库跑 `git gc --prune=now`。
const MAX_CHECKPOINTS_PER_WORKSPACE: usize = 200;

const REF_NAMESPACE: &str = "refs/willdeep";

/// 一份工作区检查点。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct WorkspaceCheckpoint {
    /// 影子仓库里的 commit id。
    pub commit: String,
    /// 进了快照的文件数。
    pub files: usize,
    /// 未跟踪文件太多，这份快照只有已跟踪文件。
    pub untracked_skipped: bool,
}

/// 一次按检查点恢复的结果。路径都相对工作区。
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RestoreOutcome {
    /// 恢复到的检查点。
    pub checkpoint: String,
    /// 恢复前的工作树快照：回退错了，可以按它再恢复一次。
    pub before_checkpoint: String,
    /// 内容被改回检查点版本的文件（含被删后重建的）。
    pub restored: Vec<String>,
    /// 检查点里没有、现在有的文件：挪进回收区。
    pub removed: Vec<String>,
    /// 没法恢复的条目（子模块 gitlink）。
    pub skipped: Vec<String>,
    /// 被覆盖或删除的当前文件的原件所在。没动任何文件时为 `None`。
    pub recovery_path: Option<PathBuf>,
}

pub(crate) fn enabled() -> bool {
    !matches!(
        std::env::var(ENV_TOGGLE)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "0" | "false" | "off" | "no"
    )
}

pub(crate) fn checkpoints_root(home: &Path) -> PathBuf {
    home.join("runtime/checkpoints")
}

fn session_ref_prefix(session_id: uuid::Uuid) -> String {
    format!("{REF_NAMESPACE}/{}", session_id.simple())
}

fn turn_ref(session_id: uuid::Uuid, turn_id: uuid::Uuid) -> String {
    format!("{}/{}", session_ref_prefix(session_id), turn_id.simple())
}

/// 工作区自己的 git 目录（worktree 检出时是公共目录），不是仓库就报错。
fn workspace_git_objects(workspace: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(workspace)
        .stderr(Stdio::null())
        .output()
        .context("run git rev-parse")?;
    if !output.status.success() {
        bail!("workspace is not a git repository");
    }
    let common = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let common = if common.is_absolute() {
        common
    } else {
        workspace.join(common)
    };
    Ok(common.join("objects"))
}

/// 影子仓库路径；不存在就初始化，并把 alternates 指向工作区的对象库。
fn shadow_repo(home: &Path, workspace: &Path) -> Result<PathBuf> {
    let canonical = workspace
        .canonicalize()
        .with_context(|| format!("canonicalize workspace {}", workspace.display()))?;
    let objects = workspace_git_objects(&canonical)?;
    let mut hasher = DefaultHasher::new();
    canonical.hash(&mut hasher);
    let shadow = checkpoints_root(home).join(format!("{:016x}", hasher.finish()));
    if !shadow.join("HEAD").exists() {
        std::fs::create_dir_all(&shadow)?;
        let status = Command::new("git")
            .args(["init", "--bare", "--quiet"])
            .arg(&shadow)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("run git init for the checkpoint repository")?;
        if !status.success() {
            bail!("initialise checkpoint repository at {}", shadow.display());
        }
    }
    let info = shadow.join("objects/info");
    std::fs::create_dir_all(&info)?;
    let alternates = info.join("alternates");
    let wanted = format!("{}\n", objects.display());
    if std::fs::read_to_string(&alternates).ok().as_deref() != Some(wanted.as_str()) {
        std::fs::write(&alternates, wanted)?;
    }
    // 工作区那边的用户身份、钩子、签名配置都与快照无关；只认这里的。
    let config = shadow.join("config");
    let current = std::fs::read_to_string(&config).unwrap_or_default();
    if !current.contains("[willdeep]") {
        std::fs::write(
            &config,
            format!(
                "{current}[willdeep]\n\tcheckpoints = true\n[gc]\n\tauto = 0\n[core]\n\thooksPath = /dev/null\n"
            ),
        )?;
    }
    Ok(shadow)
}

/// 在影子仓库里跑一条 git，工作树指向工作区。
fn shadow_git(
    shadow: &Path,
    workspace: &Path,
    index: Option<&Path>,
    args: &[&str],
) -> Result<Vec<u8>> {
    shadow_git_with_stdin(shadow, workspace, index, args, None)
}

fn shadow_git_with_stdin(
    shadow: &Path,
    workspace: &Path,
    index: Option<&Path>,
    args: &[&str],
    stdin: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    command
        .arg("--git-dir")
        .arg(shadow)
        .arg("--work-tree")
        .arg(workspace)
        .args(args)
        .current_dir(workspace)
        .env("GIT_AUTHOR_NAME", "willdeep")
        .env("GIT_AUTHOR_EMAIL", "checkpoint@willdeep.local")
        .env("GIT_COMMITTER_NAME", "willdeep")
        .env("GIT_COMMITTER_EMAIL", "checkpoint@willdeep.local")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE");
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    } else {
        command.env_remove("GIT_INDEX_FILE");
    }
    let mut child = command
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn git {}", args.join(" ")))?;
    if let Some(bytes) = stdin {
        use std::io::Write;
        let mut pipe = child.stdin.take().context("git stdin")?;
        pipe.write_all(bytes)?;
        drop(pipe);
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

fn workspace_git_lines(workspace: &Path, args: &[&str]) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .with_context(|| format!("run git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| String::from_utf8_lossy(entry).into_owned())
        .collect())
}

/// 要进快照的文件：已跟踪的全部（丢了的由 `--remove` 摘掉），未跟踪的按上限筛。
fn snapshot_paths(workspace: &Path) -> Result<(Vec<String>, bool)> {
    let tracked = workspace_git_lines(workspace, &["ls-files", "-z"])?;
    let untracked = workspace_git_lines(
        workspace,
        &["ls-files", "-z", "--others", "--exclude-standard"],
    )?;
    let mut paths: BTreeSet<String> = tracked.into_iter().collect();
    let skipped = untracked.len() > MAX_UNTRACKED_FILES;
    if !skipped {
        for path in untracked {
            let Ok(metadata) = std::fs::symlink_metadata(workspace.join(&path)) else {
                continue;
            };
            let file_type = metadata.file_type();
            if file_type.is_symlink()
                || (file_type.is_file() && metadata.len() <= MAX_UNTRACKED_FILE_BYTES)
            {
                paths.insert(path);
            }
        }
    }
    Ok((paths.into_iter().collect(), skipped))
}

/// 把工作树写成影子仓库里的一个 commit，不碰工作区的索引。
fn commit_worktree(
    shadow: &Path,
    workspace: &Path,
    message: &str,
) -> Result<(String, usize, bool)> {
    let (paths, untracked_skipped) = snapshot_paths(workspace)?;
    // 每次一个临时索引：既不碰工作区的索引，两次并发拍照也互不踩。
    let index = shadow.join(format!("index-{}", uuid::Uuid::new_v4().simple()));
    let result = write_commit(shadow, workspace, &index, &paths, message);
    let _ = std::fs::remove_file(&index);
    let commit = result?;
    Ok((commit, paths.len(), untracked_skipped))
}

fn write_commit(
    shadow: &Path,
    workspace: &Path,
    index: &Path,
    paths: &[String],
    message: &str,
) -> Result<String> {
    let mut stdin = Vec::new();
    for path in paths {
        stdin.extend_from_slice(path.as_bytes());
        stdin.push(0);
    }
    shadow_git_with_stdin(
        shadow,
        workspace,
        Some(index),
        &["update-index", "--add", "--remove", "-z", "--stdin"],
        Some(&stdin),
    )?;
    let tree = String::from_utf8_lossy(&shadow_git(
        shadow,
        workspace,
        Some(index),
        &["write-tree"],
    )?)
    .trim()
    .to_owned();
    let commit = shadow_git(
        shadow,
        workspace,
        Some(index),
        &["commit-tree", &tree, "-m", message],
    )?;
    Ok(String::from_utf8_lossy(&commit).trim().to_owned())
}

fn list_refs(shadow: &Path, workspace: &Path, prefix: &str) -> Result<Vec<String>> {
    let output = shadow_git(
        shadow,
        workspace,
        None,
        &[
            "for-each-ref",
            "--sort=creatordate",
            "--format=%(refname)",
            prefix,
        ],
    )?;
    Ok(String::from_utf8_lossy(&output)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

/// 给一个 Runtime Turn 拍检查点。返回的 commit 记在 Turn 上，回退时按它找。
pub(crate) fn capture(
    home: &Path,
    workspace: &Path,
    session_id: uuid::Uuid,
    turn_id: uuid::Uuid,
) -> Result<WorkspaceCheckpoint> {
    let shadow = shadow_repo(home, workspace)?;
    let (commit, files, untracked_skipped) = commit_worktree(
        &shadow,
        workspace,
        &format!("willdeep checkpoint session={session_id} turn={turn_id}"),
    )?;
    shadow_git(
        &shadow,
        workspace,
        None,
        &["update-ref", &turn_ref(session_id, turn_id), &commit],
    )?;
    let refs = list_refs(&shadow, workspace, REF_NAMESPACE)?;
    for stale in refs
        .iter()
        .take(refs.len().saturating_sub(MAX_CHECKPOINTS_PER_WORKSPACE))
    {
        let _ = shadow_git(&shadow, workspace, None, &["update-ref", "-d", stale]);
    }
    Ok(WorkspaceCheckpoint {
        commit,
        files,
        untracked_skipped,
    })
}

pub(crate) async fn capture_blocking(
    home: PathBuf,
    workspace: PathBuf,
    session_id: uuid::Uuid,
    turn_id: uuid::Uuid,
) -> Result<WorkspaceCheckpoint> {
    tokio::task::spawn_blocking(move || capture(&home, &workspace, session_id, turn_id))
        .await
        .context("workspace checkpoint task")?
}

/// 影子仓库里还有没有这个 commit（引用被裁掉后对象仍可能在，但不保证）。
#[cfg(test)]
pub(crate) fn exists(home: &Path, workspace: &Path, commit: &str) -> bool {
    let Ok(shadow) = shadow_repo(home, workspace) else {
        return false;
    };
    shadow_git(
        &shadow,
        workspace,
        None,
        &["cat-file", "-e", &format!("{commit}^{{commit}}")],
    )
    .is_ok()
}

fn safe_relative_path(workspace: &Path, path: &str) -> Result<PathBuf> {
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
        bail!("checkpoint path escapes Workspace: {path}");
    }
    Ok(workspace.join(relative))
}

/// 把当前文件挪进回收区（重命名失败就复制后删除），目录按需建。
fn stash_current(workspace: &Path, recovery_root: &Path, path: &str) -> Result<bool> {
    let source = safe_relative_path(workspace, path)?;
    let Ok(metadata) = std::fs::symlink_metadata(&source) else {
        return Ok(false);
    };
    let destination = recovery_root.join(path);
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::rename(&source, &destination).is_err() {
        if metadata.file_type().is_symlink() {
            let target = std::fs::read_link(&source)?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, &destination)?;
            #[cfg(not(unix))]
            std::fs::write(&destination, target.to_string_lossy().as_bytes())?;
            std::fs::remove_file(&source)?;
        } else {
            std::fs::copy(&source, &destination)?;
            std::fs::remove_file(&source)?;
        }
    }
    Ok(true)
}

/// 从检查点写回一个文件。返回 `false` 表示这一项不能恢复（gitlink）。
fn write_from_checkpoint(
    shadow: &Path,
    workspace: &Path,
    commit: &str,
    path: &str,
) -> Result<bool> {
    let entry = shadow_git(
        shadow,
        workspace,
        None,
        &["ls-tree", "-z", commit, "--", path],
    )?;
    let entry = String::from_utf8_lossy(&entry);
    let entry = entry.trim_end_matches('\0');
    let (meta, _) = entry
        .split_once('\t')
        .with_context(|| format!("checkpoint {commit} has no entry for {path}"))?;
    let mut fields = meta.split(' ');
    let mode = fields.next().unwrap_or_default();
    let kind = fields.next().unwrap_or_default();
    let oid = fields.next().unwrap_or_default();
    if kind != "blob" {
        return Ok(false);
    }
    let bytes = shadow_git(shadow, workspace, None, &["cat-file", "blob", oid])?;
    let destination = safe_relative_path(workspace, path)?;
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::symlink_metadata(&destination).is_ok() {
        std::fs::remove_file(&destination)?;
    }
    if mode == "120000" {
        let target = PathBuf::from(String::from_utf8_lossy(&bytes).into_owned());
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, &destination)?;
        #[cfg(not(unix))]
        std::fs::write(&destination, bytes)?;
        return Ok(true);
    }
    let temporary = destination.with_file_name(format!(
        ".{}.willdeep-restore-{}",
        destination
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::write(&temporary, &bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions =
            std::fs::Permissions::from_mode(if mode == "100755" { 0o755 } else { 0o644 });
        std::fs::set_permissions(&temporary, permissions)?;
    }
    std::fs::rename(&temporary, &destination)?;
    Ok(true)
}

/// 把工作树恢复到 `commit` 那一刻。先把当前状态拍成 `before_checkpoint`，再逐文件
/// 「备份到回收区 → 覆盖 / 删除」。
pub(crate) fn restore(
    home: &Path,
    workspace: &Path,
    session_id: uuid::Uuid,
    commit: &str,
) -> Result<RestoreOutcome> {
    let shadow = shadow_repo(home, workspace)?;
    shadow_git(
        &shadow,
        workspace,
        None,
        &["cat-file", "-e", &format!("{commit}^{{commit}}")],
    )
    .with_context(|| format!("checkpoint {commit} is no longer available"))?;
    let (before, _, _) = commit_worktree(
        &shadow,
        workspace,
        &format!("willdeep before-rewind session={session_id}"),
    )?;
    shadow_git(
        &shadow,
        workspace,
        None,
        &[
            "update-ref",
            &format!(
                "{}/before-rewind-{}",
                session_ref_prefix(session_id),
                uuid::Uuid::new_v4().simple()
            ),
            &before,
        ],
    )?;
    let listing = shadow_git(
        &shadow,
        workspace,
        None,
        &[
            "diff",
            "--no-renames",
            "--name-status",
            "-z",
            commit,
            &before,
        ],
    )?;
    let mut fields = listing
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| String::from_utf8_lossy(entry).into_owned());
    let mut outcome = RestoreOutcome {
        checkpoint: commit.to_owned(),
        before_checkpoint: before,
        ..RestoreOutcome::default()
    };
    let recovery_root = super::diff_review::recovery_root(home).join(format!(
        "rewind-{}-{}",
        session_id.simple(),
        uuid::Uuid::new_v4().simple()
    ));
    let mut stashed = false;
    while let Some(status) = fields.next() {
        let Some(path) = fields.next() else { break };
        match status.chars().next() {
            // 检查点里没有：这是回退掉的那几步新建的，挪进回收区。
            Some('A') => {
                stashed |= stash_current(workspace, &recovery_root, &path)?;
                outcome.removed.push(path);
            }
            // 检查点里有、现在没有或不一样：备份当前的，再写回检查点版本。
            Some('D') | Some('M') | Some('T') => {
                stashed |= stash_current(workspace, &recovery_root, &path)?;
                if write_from_checkpoint(&shadow, workspace, commit, &path)? {
                    outcome.restored.push(path);
                } else {
                    outcome.skipped.push(path);
                }
            }
            _ => outcome.skipped.push(path),
        }
    }
    outcome.recovery_path = stashed.then_some(recovery_root);
    Ok(outcome)
}

pub(crate) async fn restore_blocking(
    home: PathBuf,
    workspace: PathBuf,
    session_id: uuid::Uuid,
    commit: String,
) -> Result<RestoreOutcome> {
    tokio::task::spawn_blocking(move || restore(&home, &workspace, session_id, &commit))
        .await
        .context("workspace restore task")?
}

/// 会话删除时把它的检查点引用一起删掉。对象留给 `git gc`。
pub(crate) fn forget_session(home: &Path, workspace: &Path, session_id: uuid::Uuid) -> Result<()> {
    let Ok(shadow) = shadow_repo(home, workspace) else {
        return Ok(());
    };
    for reference in list_refs(&shadow, workspace, &session_ref_prefix(session_id))? {
        shadow_git(&shadow, workspace, None, &["update-ref", "-d", &reference])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(workspace: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(workspace)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn repo() -> (PathBuf, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("willdeep-checkpoint-{}", uuid::Uuid::new_v4()));
        let workspace = root.join("workspace");
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        git(&workspace, &["init", "--quiet", "-b", "main"]);
        std::fs::write(workspace.join("src/lib.rs"), "fn one() {}\n").unwrap();
        std::fs::write(workspace.join("README.md"), "hello\n").unwrap();
        std::fs::write(workspace.join(".gitignore"), "target/\n").unwrap();
        git(&workspace, &["add", "-A"]);
        git(&workspace, &["commit", "--quiet", "-m", "init"]);
        (root, workspace)
    }

    #[test]
    fn a_checkpoint_captures_tracked_and_untracked_content_without_touching_the_repo() {
        let (root, workspace) = repo();
        let home = root.join("home");
        let session = uuid::Uuid::new_v4();
        let turn = uuid::Uuid::new_v4();
        std::fs::write(workspace.join("src/lib.rs"), "fn one() {}\nfn two() {}\n").unwrap();
        std::fs::write(workspace.join("notes.txt"), "untracked\n").unwrap();
        std::fs::create_dir_all(workspace.join("target")).unwrap();
        std::fs::write(workspace.join("target/out.bin"), "ignored\n").unwrap();
        let head_before = git(&workspace, &["rev-parse", "HEAD"]);
        let status_before = git(&workspace, &["status", "--porcelain"]);

        let checkpoint = capture(&home, &workspace, session, turn).unwrap();
        assert_eq!(
            checkpoint.files, 4,
            "lib.rs, README.md, .gitignore, notes.txt"
        );
        assert!(!checkpoint.untracked_skipped);
        assert!(exists(&home, &workspace, &checkpoint.commit));

        // 用户的仓库一根毫毛都没动：HEAD、状态、引用都和之前一样。
        assert_eq!(git(&workspace, &["rev-parse", "HEAD"]), head_before);
        assert_eq!(git(&workspace, &["status", "--porcelain"]), status_before);
        assert_eq!(git(&workspace, &["for-each-ref", "refs/willdeep"]), "");
        assert!(!workspace.join(".git/index.lock").exists());

        // 影子仓库里能读到当时的内容，忽略的文件没进去。
        let shadow = shadow_repo(&home, &workspace).unwrap();
        let listing = shadow_git(
            &shadow,
            &workspace,
            None,
            &["ls-tree", "-r", "--name-only", &checkpoint.commit],
        )
        .unwrap();
        let listing = String::from_utf8_lossy(&listing);
        assert!(listing.contains("notes.txt"), "{listing}");
        assert!(!listing.contains("target/out.bin"), "{listing}");
        let blob = shadow_git(
            &shadow,
            &workspace,
            None,
            &["show", &format!("{}:src/lib.rs", checkpoint.commit)],
        )
        .unwrap();
        assert_eq!(String::from_utf8_lossy(&blob), "fn one() {}\nfn two() {}\n");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restore_brings_files_back_and_parks_the_current_ones_in_recovery() {
        let (root, workspace) = repo();
        let home = root.join("home");
        let session = uuid::Uuid::new_v4();
        std::fs::write(workspace.join("notes.txt"), "kept\n").unwrap();
        let checkpoint = capture(&home, &workspace, session, uuid::Uuid::new_v4()).unwrap();

        // 之后几步：改一个、删一个、新建一个、新建一个目录里的文件。
        std::fs::write(workspace.join("src/lib.rs"), "fn broken() {}\n").unwrap();
        std::fs::remove_file(workspace.join("README.md")).unwrap();
        std::fs::write(workspace.join("new.rs"), "later\n").unwrap();
        std::fs::create_dir_all(workspace.join("deep/dir")).unwrap();
        std::fs::write(workspace.join("deep/dir/file.txt"), "later\n").unwrap();

        let outcome = restore(&home, &workspace, session, &checkpoint.commit).unwrap();
        assert_eq!(outcome.checkpoint, checkpoint.commit);
        assert!(exists(&home, &workspace, &outcome.before_checkpoint));
        let mut restored = outcome.restored.clone();
        restored.sort();
        assert_eq!(restored, vec!["README.md", "src/lib.rs"]);
        let mut removed = outcome.removed.clone();
        removed.sort();
        assert_eq!(removed, vec!["deep/dir/file.txt", "new.rs"]);
        assert!(outcome.skipped.is_empty());

        assert_eq!(
            std::fs::read_to_string(workspace.join("src/lib.rs")).unwrap(),
            "fn one() {}\n"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("README.md")).unwrap(),
            "hello\n"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("notes.txt")).unwrap(),
            "kept\n"
        );
        assert!(!workspace.join("new.rs").exists());
        assert!(!workspace.join("deep/dir/file.txt").exists());

        // 被盖掉、被删掉的原件都在回收区，一个没丢。
        let recovery = outcome.recovery_path.expect("recovery directory");
        assert!(recovery.starts_with(super::super::diff_review::recovery_root(&home)));
        assert_eq!(
            std::fs::read_to_string(recovery.join("src/lib.rs")).unwrap(),
            "fn broken() {}\n"
        );
        assert_eq!(
            std::fs::read_to_string(recovery.join("new.rs")).unwrap(),
            "later\n"
        );
        assert_eq!(
            std::fs::read_to_string(recovery.join("deep/dir/file.txt")).unwrap(),
            "later\n"
        );
        assert!(
            !recovery.join("README.md").exists(),
            "a file that was already gone has nothing to park"
        );

        // 回退错了还能回来：按 before_checkpoint 再恢复一次。
        let again = restore(&home, &workspace, session, &outcome.before_checkpoint).unwrap();
        assert_eq!(
            std::fs::read_to_string(workspace.join("src/lib.rs")).unwrap(),
            "fn broken() {}\n"
        );
        assert!(!workspace.join("README.md").exists());
        assert_eq!(
            std::fs::read_to_string(workspace.join("new.rs")).unwrap(),
            "later\n"
        );
        assert!(again.removed.contains(&"README.md".to_owned()));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restoring_an_unchanged_tree_touches_nothing() {
        let (root, workspace) = repo();
        let home = root.join("home");
        let session = uuid::Uuid::new_v4();
        let checkpoint = capture(&home, &workspace, session, uuid::Uuid::new_v4()).unwrap();
        let outcome = restore(&home, &workspace, session, &checkpoint.commit).unwrap();
        assert!(outcome.restored.is_empty() && outcome.removed.is_empty());
        assert_eq!(outcome.recovery_path, None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_workspace_without_git_has_no_checkpoints() {
        let root = std::env::temp_dir().join(format!(
            "willdeep-checkpoint-plain-{}",
            uuid::Uuid::new_v4()
        ));
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let error = capture(
            &root.join("home"),
            &workspace,
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("not a git repository"),
            "{error:#}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn forgetting_a_session_drops_its_refs_and_the_toggle_reads_the_environment() {
        let (root, workspace) = repo();
        let home = root.join("home");
        let session = uuid::Uuid::new_v4();
        let other = uuid::Uuid::new_v4();
        capture(&home, &workspace, session, uuid::Uuid::new_v4()).unwrap();
        capture(&home, &workspace, other, uuid::Uuid::new_v4()).unwrap();
        let shadow = shadow_repo(&home, &workspace).unwrap();
        assert_eq!(
            list_refs(&shadow, &workspace, REF_NAMESPACE).unwrap().len(),
            2
        );
        forget_session(&home, &workspace, session).unwrap();
        let remaining = list_refs(&shadow, &workspace, REF_NAMESPACE).unwrap();
        assert_eq!(remaining.len(), 1);
        assert!(remaining[0].contains(&other.simple().to_string()));
        assert!(enabled() || std::env::var(ENV_TOGGLE).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }
}
