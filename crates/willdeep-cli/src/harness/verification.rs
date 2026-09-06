use std::path::Path;
use std::process::Command;

/// Absence of a Git repository is a supported workspace mode. Every other
/// discovery or snapshot error must remain distinguishable from that mode.
pub(super) fn snapshot(workspace: &Path) -> Result<Option<String>, String> {
    let discovery = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .env("LC_ALL", "C")
        .current_dir(workspace)
        .output()
        .map_err(|_| "Git workspace discovery could not be started".to_owned())?;
    if !discovery.status.success() {
        if String::from_utf8_lossy(&discovery.stderr)
            .starts_with("fatal: not a git repository (or any of the parent directories): .git")
        {
            return Ok(None);
        }
        return Err("Git workspace discovery failed".into());
    }
    crate::daemon::diff_review::snapshot(workspace)
        .map(|snapshot| Some(snapshot.id))
        .map_err(|_| "Git verification snapshot could not be captured".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_distinguish_plain_workspaces_repositories_and_broken_git_metadata() {
        let root =
            std::env::temp_dir().join(format!("willdeep-snapshot-mode-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(snapshot(&root).unwrap(), None);
        let init = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(init.status.success());
        assert!(snapshot(&root).unwrap().is_some());
        std::fs::rename(root.join(".git"), root.join("saved-git")).unwrap();
        std::fs::write(root.join(".git"), "gitdir: missing-directory\n").unwrap();
        assert!(snapshot(&root).is_err());
        std::fs::remove_dir_all(&root).unwrap();
        assert!(snapshot(&root).is_err());
    }
}
