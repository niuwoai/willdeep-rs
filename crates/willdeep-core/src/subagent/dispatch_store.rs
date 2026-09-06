//! Immutable dispatch records. Credentials and live provider objects are never serialized.
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::types::SpawnAgentArgs;
use crate::AgentError;
use crate::subagent_worktree::PreparedSubagentWorkspace;

const MAX_RECORD_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct DispatchRecord {
    pub version: u32,
    pub id: uuid::Uuid,
    pub parent_session: Option<uuid::Uuid>,
    pub args: SpawnAgentArgs,
    pub approved_targets: Option<BTreeSet<PathBuf>>,
    pub approved_command: Option<String>,
    pub model: Option<String>,
    pub prepared: PreparedSubagentWorkspace,
}

fn failure(error: impl std::fmt::Display) -> AgentError {
    AgentError::Checkpoint(format!("worker dispatch record: {error}"))
}

async fn git_value(workspace: &Path, args: &[&str]) -> Result<String, AgentError> {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .await
        .map_err(failure)?;
    if !output.status.success() {
        return Err(failure("cannot validate restored Git worktree"));
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(failure)
}

pub(super) async fn validate_workspace(
    prepared: &PreparedSubagentWorkspace,
    root: &Path,
) -> Result<(), AgentError> {
    let root = root.canonicalize().map_err(failure)?;
    if prepared.root_workspace.canonicalize().map_err(failure)? != root {
        return Err(failure("parent workspace changed"));
    }
    let workspace = prepared.workspace.canonicalize().map_err(failure)?;
    if !prepared.dedicated {
        if workspace != root {
            return Err(failure("shared workspace changed"));
        }
        return Ok(());
    }
    let args = ["rev-parse", "--path-format=absolute", "--git-common-dir"];
    let original = PathBuf::from(git_value(&root, &args).await?)
        .canonicalize()
        .map_err(failure)?;
    let restored = PathBuf::from(git_value(&workspace, &args).await?)
        .canonicalize()
        .map_err(failure)?;
    let branch = git_value(&workspace, &["symbolic-ref", "--short", "HEAD"]).await?;
    if original != restored || prepared.branch.as_deref() != Some(branch.as_str()) {
        return Err(failure("restored worktree repository or branch changed"));
    }
    Ok(())
}

pub(super) fn load(home: &Path, id: uuid::Uuid) -> Result<Option<DispatchRecord>, AgentError> {
    let path = home.join("dispatches").join(format!("{id}.json"));
    let Some(bytes) = read_bytes(&path)? else {
        return Ok(None);
    };
    let record: DispatchRecord = serde_json::from_slice(&bytes).map_err(failure)?;
    if record.version != 1 || record.id != id {
        return Err(failure("identity or version mismatch"));
    }
    Ok(Some(record))
}

fn read_bytes(path: &Path) -> Result<Option<Vec<u8>>, AgentError> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(failure(error)),
    };
    if !file.metadata().map_err(failure)?.is_file() {
        return Err(failure("not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(failure)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(failure("record exceeds size limit"));
    }
    Ok(Some(bytes))
}

pub(super) fn save(home: &Path, record: &DispatchRecord) -> Result<(), AgentError> {
    let directory = home.join("dispatches");
    let filename = format!("{}.json", record.id);
    write_once(
        &directory,
        &filename,
        &serde_json::to_vec(record).map_err(failure)?,
    )?;
    if record.args.run_in_background != Some(true)
        && let Some(parent) = record.parent_session
    {
        write_once(
            &directory.join("parents").join(parent.to_string()),
            &filename,
            b"",
        )?;
    }
    Ok(())
}

fn write_once(directory: &Path, filename: &str, bytes: &[u8]) -> Result<(), AgentError> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(directory).map_err(failure)?;
    if !std::fs::symlink_metadata(directory)
        .map_err(failure)?
        .is_dir()
    {
        return Err(failure("dispatch directory is not a directory"));
    }
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(failure("record exceeds size limit"));
    }
    let temporary = directory.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary).map_err(failure)?;
        file.write_all(bytes).map_err(failure)?;
        file.sync_all().map_err(failure)?;
        match std::fs::hard_link(&temporary, directory.join(filename)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing = read_bytes(&directory.join(filename))?
                    .ok_or_else(|| failure("record disappeared"))?;
                if existing != bytes {
                    return Err(failure("immutable dispatch differs"));
                }
            }
            Err(error) => return Err(failure(error)),
        }
        #[cfg(unix)]
        std::fs::File::open(directory)
            .and_then(|file| file.sync_all())
            .map_err(failure)?;
        Ok(())
    })();
    if let Err(error) = std::fs::remove_file(&temporary)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        return Err(failure(error));
    }
    result
}

#[derive(Deserialize, Serialize)]
struct CompletedReport {
    version: u32,
    id: uuid::Uuid,
    report: String,
}

pub(super) fn save_report(home: &Path, id: uuid::Uuid, report: &str) -> Result<(), AgentError> {
    let record = CompletedReport {
        version: 1,
        id,
        report: report.to_owned(),
    };
    write_once(
        &home.join("dispatches/results"),
        &format!("{id}.json"),
        &serde_json::to_vec(&record).map_err(failure)?,
    )
}

pub(super) fn load_report(home: &Path, id: uuid::Uuid) -> Result<Option<String>, AgentError> {
    let Some(bytes) = read_bytes(&home.join("dispatches/results").join(format!("{id}.json")))?
    else {
        return Ok(None);
    };
    let record: CompletedReport = serde_json::from_slice(&bytes).map_err(failure)?;
    if record.version != 1 || record.id != id {
        return Err(failure("report identity or version mismatch"));
    }
    Ok(Some(record.report))
}

const RECOVERY_PAGE_SIZE: usize = 16;
const RECOVERY_LABEL_CHARS: usize = 128;
const RECOVERY_GOAL_CHARS: usize = 256;

pub(super) fn list_foreground(
    home: &Path,
    parent: uuid::Uuid,
    after: Option<uuid::Uuid>,
) -> Result<serde_json::Value, AgentError> {
    let directory = home.join("dispatches/parents").join(parent.to_string());
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(serde_json::json!({"agents":[],"next_after_id":null}));
        }
        Err(error) => return Err(failure(error)),
    };
    let mut ids = BTreeSet::new();
    for entry in entries {
        let path = entry.map_err(failure)?.path();
        if let Some(id) = path
            .file_stem()
            .and_then(|name| name.to_str())
            .and_then(|name| name.parse::<uuid::Uuid>().ok())
            && after.is_none_or(|after| id > after)
        {
            ids.insert(id);
            // Retain one look-ahead ID, not the entire session inventory.
            // Directory iteration order is unspecified; discard the largest
            // candidate so every page contains the smallest IDs after its cursor.
            if ids.len() > RECOVERY_PAGE_SIZE + 1 {
                ids.pop_last();
            }
        }
    }
    let more = ids.len() > RECOVERY_PAGE_SIZE;
    let mut rows = Vec::new();
    let mut last = None;
    for id in ids.into_iter().take(RECOVERY_PAGE_SIZE) {
        let record = load(home, id)?.ok_or_else(|| failure("indexed dispatch is missing"))?;
        if record.parent_session != Some(parent) || record.args.run_in_background == Some(true) {
            return Err(failure("dispatch index owner mismatch"));
        }
        let goal = record
            .args
            .task
            .as_ref()
            .map(|task| task.goal.as_str())
            .unwrap_or(&record.args.prompt)
            .chars()
            .take(RECOVERY_GOAL_CHARS)
            .collect::<String>();
        rows.push(serde_json::json!({"agent_id":id,"profile":record.args.profile,"goal":goal,"label":record.args.label.map(|label|label.chars().take(RECOVERY_LABEL_CHARS).collect::<String>()),"completed_report_available":load_report(home,id)?.is_some()}));
        last = Some(id);
    }
    Ok(serde_json::json!({"agents":rows,"next_after_id":if more { last } else { None }}))
}

#[cfg(test)]
#[path = "dispatch_store_tests.rs"]
mod tests;
