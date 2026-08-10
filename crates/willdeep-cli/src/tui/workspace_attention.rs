use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;

use willdeep_core::{AttentionItem, AttentionSource, RuntimeStatus};

pub(super) fn workspace_attention(workspace: &Path) -> Vec<AttentionItem> {
    let output = std::process::Command::new("git")
        .args(["status", "--porcelain=v1", "--untracked-files=normal"])
        .current_dir(workspace)
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let porcelain = String::from_utf8_lossy(&output.stdout);
    let signature = workspace_signature(workspace, &porcelain);
    workspace_attention_from_porcelain_with_signature(&porcelain, signature)
}

#[cfg(test)]
pub(super) fn workspace_attention_from_porcelain(value: &str) -> Vec<AttentionItem> {
    workspace_attention_from_porcelain_with_signature(value, 0)
}

fn workspace_attention_from_porcelain_with_signature(
    value: &str,
    signature: u64,
) -> Vec<AttentionItem> {
    let lines = value
        .lines()
        .filter(|line| line.len() >= 3)
        .collect::<Vec<_>>();
    let (conflicts, reviewable): (Vec<_>, Vec<_>) =
        lines.into_iter().partition(|line| is_unmerged(&line[..2]));
    let mut items = Vec::new();
    if !conflicts.is_empty() {
        items.push(AttentionItem {
            id: fingerprint("worktree-conflict", &conflicts, signature),
            source: AttentionSource::Worktree,
            title: format!("{} unresolved Git conflict(s)", conflicts.len()),
            detail: summarize(&conflicts),
            status: RuntimeStatus::Blocked,
            elapsed_millis: None,
        });
    }
    if !reviewable.is_empty() {
        items.push(AttentionItem {
            id: fingerprint("diff-review", &reviewable, signature),
            source: AttentionSource::DiffReview,
            title: format!("{} changed file(s) ready for review", reviewable.len()),
            detail: summarize(&reviewable),
            status: RuntimeStatus::WaitingApproval,
            elapsed_millis: None,
        });
    }
    items
}

fn is_unmerged(code: &str) -> bool {
    matches!(code, "DD" | "AU" | "UD" | "UA" | "DU" | "AA" | "UU")
}

fn fingerprint(prefix: &str, lines: &[&str], signature: u64) -> String {
    let mut hasher = DefaultHasher::new();
    lines.hash(&mut hasher);
    signature.hash(&mut hasher);
    format!("{prefix}:{:016x}", hasher.finish())
}

fn workspace_signature(workspace: &Path, porcelain: &str) -> u64 {
    const MAX_UNTRACKED_BYTES: usize = 1024 * 1024;
    let mut hasher = DefaultHasher::new();
    porcelain.hash(&mut hasher);
    for args in [
        ["diff", "--no-ext-diff", "--binary"].as_slice(),
        ["diff", "--cached", "--no-ext-diff", "--binary"].as_slice(),
    ] {
        if let Ok(output) = std::process::Command::new("git")
            .args(args)
            .current_dir(workspace)
            .output()
        {
            output.stdout.hash(&mut hasher);
        }
    }
    let mut remaining = MAX_UNTRACKED_BYTES;
    if let Ok(output) = std::process::Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard", "-z"])
        .current_dir(workspace)
        .output()
    {
        for relative in output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
        {
            relative.hash(&mut hasher);
            if remaining == 0 {
                break;
            }
            let path = workspace.join(String::from_utf8_lossy(relative).as_ref());
            if let Ok(data) = std::fs::read(path) {
                let take = data.len().min(remaining);
                data[..take].hash(&mut hasher);
                remaining -= take;
            }
        }
    }
    hasher.finish()
}

fn summarize(lines: &[&str]) -> String {
    lines
        .iter()
        .take(8)
        .map(|line| line.trim().to_owned())
        .collect::<Vec<_>>()
        .join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separates_conflicts_from_reviewable_changes() {
        let items = workspace_attention_from_porcelain(
            "UU src/conflicted.rs\n M src/changed.rs\n?? docs/new.md\n",
        );
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].source, AttentionSource::Worktree);
        assert_eq!(items[0].status, RuntimeStatus::Blocked);
        assert_eq!(items[1].source, AttentionSource::DiffReview);
        assert_eq!(items[1].status, RuntimeStatus::WaitingApproval);
    }

    #[test]
    fn content_fingerprint_changes_with_the_diff() {
        let first = workspace_attention_from_porcelain_with_signature(" M src/a.rs\n", 1);
        let second = workspace_attention_from_porcelain_with_signature(" M src/a.rs\n", 2);
        assert_ne!(first[0].id, second[0].id);
        assert!(workspace_attention_from_porcelain("").is_empty());
    }
}
