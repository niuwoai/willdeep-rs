//! Deterministic spot-check of the locations a worker's report names. No
//! model, no network — the one thing a program can still verify about a
//! report-only trade that has no exit code to judge it.

use std::collections::BTreeSet;
use std::path::Path;

/// What a deterministic spot-check made of a report-only run.
///
/// Read-only trades (`scout`, `reader`, `log_inspector`, `git_detective`) have
/// no exit code to judge them, so their telemetry has been a permanent `None`:
/// every run "unverified", forever. But their answers are not unfalsifiable —
/// a location either exists or it does not. This checks the part a program
/// can check: the paths, line numbers and commits the report names.
///
/// It deliberately says nothing about whether the answer is *right*. A worker
/// can cite ten real files and still miss the point; what it can no longer do
/// is invent a path and have that pass unnoticed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CitationAudit {
    pub checked: usize,
    pub unverifiable: Vec<String>,
}

impl CitationAudit {
    pub(super) fn note(&self) -> Option<String> {
        if self.checked == 0 {
            return None;
        }
        if self.unverifiable.is_empty() {
            return Some(format!(
                "<citation-check checked=\"{}\" unverifiable=\"0\" />",
                self.checked
            ));
        }
        Some(format!(
            "<citation-check checked=\"{}\" unverifiable=\"{}\">\nThese cited locations do not exist in the workspace, so anything resting on them is unsupported:\n{}\n</citation-check>",
            self.checked,
            self.unverifiable.len(),
            self.unverifiable
                .iter()
                .map(|claim| format!("- {claim}"))
                .collect::<Vec<_>>()
                .join("\n")
        ))
    }
}

/// Spot-check every `path`, `path:line` and commit hash a report names.
///
/// Cheap and deterministic: no model, no network, a `stat` per path and one
/// `git cat-file` per hash. Anything it cannot classify it leaves alone —
/// counting an unrecognized token as a bad citation would make the metric
/// measure the parser instead of the worker.
pub async fn audit_citations(workspace: &Path, report: &str) -> CitationAudit {
    let mut audit = CitationAudit::default();
    let mut seen = BTreeSet::new();
    for raw in report.split(|ch: char| {
        ch.is_whitespace()
            || matches!(
                ch,
                '`' | '"' | '\'' | '(' | ')' | '[' | ']' | '<' | '>' | ',' | ';'
            )
    }) {
        let token = raw.trim_matches(|ch: char| matches!(ch, '.' | ':' | '*' | '#'));
        if token.is_empty() || !seen.insert(token.to_owned()) {
            continue;
        }
        if let Some((path, line)) = split_path_citation(token) {
            let target = workspace.join(path);
            if !target.is_file() {
                audit.checked += 1;
                audit.unverifiable.push(token.to_owned());
                continue;
            }
            audit.checked += 1;
            let Some(line) = line else { continue };
            // A line number past the end of the file is the same class of
            // error as a path that does not exist: it points at nothing.
            let lines = tokio::fs::read_to_string(&target)
                .await
                .map(|text| text.lines().count())
                .unwrap_or(0);
            if line == 0 || line > lines {
                audit.unverifiable.push(token.to_owned());
            }
        } else if is_commit_hash(token) {
            audit.checked += 1;
            if !commit_exists(workspace, token).await {
                audit.unverifiable.push(token.to_owned());
            }
        }
    }
    audit
}

/// `src/foo.rs:42` → (`src/foo.rs`, Some(42)). Only tokens that look like a
/// workspace-relative file path qualify: a directory separator and an
/// extension, no absolute paths, no URLs.
fn split_path_citation(token: &str) -> Option<(&str, Option<usize>)> {
    let (path, line) = match token.rsplit_once(':') {
        Some((head, tail)) if tail.chars().all(|ch| ch.is_ascii_digit()) && !tail.is_empty() => {
            (head, tail.parse::<usize>().ok())
        }
        _ => (token, None),
    };
    if path.starts_with('/') || path.contains("://") || path.starts_with('-') {
        return None;
    }
    let file_name = path.rsplit('/').next()?;
    if !path.contains('/') || !file_name.contains('.') || file_name.starts_with('.') {
        return None;
    }
    Some((path, line))
}

fn is_commit_hash(token: &str) -> bool {
    (7..=40).contains(&token.len())
        && token.chars().all(|ch| ch.is_ascii_hexdigit())
        && token.chars().any(|ch| ch.is_ascii_digit())
        && token.chars().any(|ch| ch.is_ascii_alphabetic())
}

async fn commit_exists(workspace: &Path, hash: &str) -> bool {
    tokio::process::Command::new("git")
        .args(["cat-file", "-e", &format!("{hash}^{{commit}}")])
        .current_dir(workspace)
        .output()
        .await
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A report-only trade has no exit code, so the one thing a program can
    /// still check is whether the places it named exist. Both directions
    /// matter: an invented path must be caught, and a real citation must not
    /// be flagged — a check that cries wolf gets switched off.
    #[tokio::test]
    async fn a_report_only_run_is_spot_checked_against_the_files_it_cites() {
        let root =
            std::env::temp_dir().join(format!("willdeep-citations-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src")).expect("workspace");
        std::fs::write(root.join("src/real.rs"), "one\ntwo\nthree\n").expect("fixture");

        let clean = audit_citations(
            &root,
            "The handler lives in `src/real.rs:2`, and `src/real.rs` has no other callers.",
        )
        .await;
        assert_eq!(clean.checked, 2, "both citations are checkable");
        assert!(
            clean.unverifiable.is_empty(),
            "a real path and an in-range line must not be flagged: {:?}",
            clean.unverifiable
        );

        let dirty = audit_citations(
            &root,
            "See `src/invented.rs:10` and `src/real.rs:900` for the retry logic.",
        )
        .await;
        assert_eq!(dirty.checked, 2);
        assert_eq!(
            dirty.unverifiable.len(),
            2,
            "a path that does not exist and a line past the end are both citations of nothing: {:?}",
            dirty.unverifiable
        );

        // Prose is not a citation. Counting words the parser merely failed to
        // understand would measure the parser, not the worker.
        let prose = audit_citations(&root, "The retry logic looks correct to me.").await;
        assert_eq!(prose.checked, 0);
        assert!(prose.note().is_none(), "nothing checked, nothing to report");

        std::fs::remove_dir_all(root).expect("cleanup");
    }

    /// A commit hash that does not resolve is the `git_detective` version of
    /// an invented path.
    #[tokio::test]
    async fn a_cited_commit_that_does_not_resolve_is_flagged() {
        let root = std::env::temp_dir().join(format!("willdeep-commits-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("workspace");
        std::fs::write(root.join("file.txt"), "seed").expect("fixture");
        for args in [
            vec!["init", "--quiet", "--initial-branch=main"],
            vec!["add", "."],
            vec![
                "-c",
                "user.name=range",
                "-c",
                "user.email=range@local",
                "commit",
                "--quiet",
                "-m",
                "seed",
            ],
        ] {
            std::process::Command::new("git")
                .args(&args)
                .current_dir(&root)
                .status()
                .expect("git");
        }
        let head = String::from_utf8(
            std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&root)
                .output()
                .expect("head")
                .stdout,
        )
        .expect("utf8");
        let head = head.trim();

        let audit = audit_citations(
            &root,
            &format!("Introduced in {head}, not in 0badc0de1234 as the report claimed."),
        )
        .await;
        assert_eq!(audit.checked, 2, "both hashes are checkable");
        assert_eq!(
            audit.unverifiable,
            vec!["0badc0de1234".to_owned()],
            "only the hash that does not resolve is flagged"
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }
}
