//! Delegation metrics over the agent records the Runtime holds.
//!
//! One computation path feeds three readers: the human `agent-metrics` text,
//! `agent-metrics --json`, and the weekly publish script that turns the JSON
//! into a trend in the docs. Two paths would drift, and the day they disagree
//! nothing reports it — both would look self-consistent.
//!
//! Every rate carries its denominator, and a rate with no denominator is
//! `None` (printed `-`, serialised `null`) rather than a reassuring 0%:
//! "nothing was verified" and "nothing passed" are different facts, and a
//! metric that cannot tell them apart is worse than no metric.

use anyhow::{Result, bail};
use serde::Serialize;
use willdeep_runtime_protocol::RuntimeAgent;

/// Profiles that exist to take work off the parent model. `deep` is not one
/// of them: it runs the parent model by design, so counting it as delegation
/// would make the coverage number flatter itself.
pub(crate) const WORKER_PROFILES: &[&str] = &[
    "scout",
    "reader",
    "log_inspector",
    "git_detective",
    "editor",
    "test_fixer",
    "build_fixer",
];

/// The profile that runs the parent model on a child budget.
const DEEP_PROFILE: &str = "deep";
/// The general-purpose child profile: neither narrow worker nor deep.
const STANDARD_PROFILE: &str = "implementer";

/// Targets from the delegation design. They are printed next to every rate
/// so a number never travels without the bar it is measured against; the
/// publish script alarms on the same constants.
pub(crate) const DEEP_SHARE_TARGET_MAX: f64 = 5.0;
pub(crate) const SKILL_COVERAGE_TARGET_MIN: f64 = 50.0;
pub(crate) const WORKER_VERIFIED_SUCCESS_TARGET_MIN: f64 = 85.0;
pub(crate) const ESCALATION_RATE_TARGET_MAX: f64 = 15.0;

const SECONDS_PER_HOUR: u64 = 60 * 60;
const SECONDS_PER_DAY: u64 = 24 * SECONDS_PER_HOUR;
const SECONDS_PER_WEEK: u64 = 7 * SECONDS_PER_DAY;

/// Delegation and model-tier numbers over one set of child agent records.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct AgentMetrics {
    /// Inclusive lower bound on `created_at` (unix seconds). `None` means
    /// every record the Runtime still holds.
    pub since: Option<u64>,
    /// Child runs counted (root agents are never delegation).
    pub children: usize,
    /// Runs on a narrow worker profile.
    pub workers: usize,
    /// Runs on the standard `implementer` profile.
    pub standard: usize,
    /// Runs on the `deep` profile.
    pub deep: usize,
    /// Runs that had a verifier, whichever way it went.
    pub verified_runs: usize,
    /// Verified runs whose verifier passed.
    pub passed: usize,
    /// Runs without a verifier: nothing was proved either way.
    pub unverified_runs: usize,
    /// Attempts summed over verified runs.
    pub attempts: u64,
    /// Citations spot-checked in report-only runs.
    pub claims_checked: u64,
    /// Checked citations that pointed at nothing.
    pub claims_unverifiable: u64,
    /// deep / children, percent.
    pub deep_share: Option<f64>,
    /// workers / children, percent.
    pub skill_coverage: Option<f64>,
    /// passed / verified_runs, percent.
    pub worker_verified_success: Option<f64>,
    /// (verified_runs - passed) / verified_runs, percent: runs that exhausted
    /// their attempts and need a bigger model.
    pub escalation_rate: Option<f64>,
    /// (claims_checked - claims_unverifiable) / claims_checked, percent.
    pub citation_accuracy: Option<f64>,
    /// attempts / verified_runs.
    pub attempts_per_verified_run: Option<f64>,
}

impl AgentMetrics {
    /// Count the child runs in `agents` created at or after `since`.
    pub(crate) fn compute(agents: &[RuntimeAgent], since: Option<u64>) -> Self {
        let children = agents
            .iter()
            .filter(|agent| agent.parent_id.is_some())
            .filter(|agent| since.is_none_or(|floor| agent.created_at >= floor))
            .collect::<Vec<_>>();
        let profile_count = |wanted: &dyn Fn(&str) -> bool| {
            children
                .iter()
                .filter(|agent| agent.profile.as_deref().is_some_and(wanted))
                .count()
        };
        let workers = profile_count(&|profile| WORKER_PROFILES.contains(&profile));
        let standard = profile_count(&|profile| profile == STANDARD_PROFILE);
        let deep = profile_count(&|profile| profile == DEEP_PROFILE);

        let verified = children
            .iter()
            .filter(|agent| agent.verifier_passed.is_some())
            .collect::<Vec<_>>();
        let passed = verified
            .iter()
            .filter(|agent| agent.verifier_passed == Some(true))
            .count();
        let attempts = verified
            .iter()
            .filter_map(|agent| agent.attempts)
            .sum::<u64>();

        // Report-only trades never earn a verifier verdict, so without the
        // citation numbers they are permanently invisible — and "invisible"
        // reads as "fine". A citation either resolves or it does not.
        let audited = children
            .iter()
            .filter(|agent| agent.claims_checked.is_some_and(|checked| checked > 0))
            .collect::<Vec<_>>();
        let claims_checked = audited
            .iter()
            .filter_map(|agent| agent.claims_checked)
            .sum::<u64>();
        let claims_unverifiable = audited
            .iter()
            .filter_map(|agent| agent.claims_unverifiable)
            .sum::<u64>()
            .min(claims_checked);

        Self {
            since,
            children: children.len(),
            workers,
            standard,
            deep,
            verified_runs: verified.len(),
            passed,
            unverified_runs: children.len() - verified.len(),
            attempts,
            claims_checked,
            claims_unverifiable,
            deep_share: rate(deep as u64, children.len() as u64),
            skill_coverage: rate(workers as u64, children.len() as u64),
            worker_verified_success: rate(passed as u64, verified.len() as u64),
            escalation_rate: rate((verified.len() - passed) as u64, verified.len() as u64),
            citation_accuracy: rate(claims_checked - claims_unverifiable, claims_checked),
            attempts_per_verified_run: (!verified.is_empty())
                .then(|| attempts as f64 / verified.len() as f64),
        }
    }

    /// The tab-separated human report: one metric per line, each rate next to
    /// its denominator and target.
    pub(crate) fn render_text(&self) -> String {
        let mut out = String::new();
        if let Some(since) = self.since {
            out.push_str(&format!(
                "since\t{since}\t(unix seconds; only child runs created at or after this instant)\n"
            ));
        }
        out.push_str(&format!(
            "agents\tchildren={}\tworkers={}\n",
            self.children, self.workers
        ));
        out.push_str(&format!(
            "model_tiers\tworker={}\tstandard={}\tdeep={}\n",
            self.workers, self.standard, self.deep
        ));
        out.push_str(&format!(
            "deep_share\t{}\t(actual deep child runs / all child runs: {}/{}; target <= {DEEP_SHARE_TARGET_MAX:.0}%)\n",
            format_rate(self.deep_share),
            self.deep,
            self.children
        ));
        out.push_str(&format!(
            "skill_coverage\t{}\t(narrow worker runs / all child runs: {}/{}; target >= {SKILL_COVERAGE_TARGET_MIN:.0}%)\n",
            format_rate(self.skill_coverage),
            self.workers,
            self.children
        ));
        out.push_str(&format!(
            "worker_verified_success\t{}\t(verifier passes / runs with a verifier: {}/{}; target >= {WORKER_VERIFIED_SUCCESS_TARGET_MIN:.0}%)\n",
            format_rate(self.worker_verified_success),
            self.passed,
            self.verified_runs
        ));
        out.push_str(&format!(
            "escalation_rate\t{}\t(verified runs that exhausted their attempts and need a bigger model: {}/{}; target <= {ESCALATION_RATE_TARGET_MAX:.0}%)\n",
            format_rate(self.escalation_rate),
            self.verified_runs - self.passed,
            self.verified_runs
        ));
        out.push_str(&format!(
            "citation_accuracy\t{}\t(cited locations that exist / cited locations checked in report-only runs: {}/{})\n",
            format_rate(self.citation_accuracy),
            self.claims_checked - self.claims_unverifiable,
            self.claims_checked
        ));
        out.push_str(&format!(
            "attempts_per_verified_run\t{}\n",
            self.attempts_per_verified_run
                .map_or_else(|| "-".to_owned(), |value| format!("{value:.2}"))
        ));
        out.push_str(&format!(
            "unverified_runs\t{}\t(no verifier was given, so nothing was proved either way)\n",
            self.unverified_runs
        ));
        out
    }
}

/// Percent with one decimal, or `None` when there is nothing to divide by.
fn rate(part: u64, whole: u64) -> Option<f64> {
    (whole > 0).then(|| (part as f64 * 1000.0 / whole as f64).round() / 10.0)
}

fn format_rate(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_owned(), |value| format!("{value:.1}%"))
}

/// Turn a `--since` argument into a unix-seconds floor.
///
/// Accepts a relative window (`7d`, `24h`, `2w`), a UTC date (`2026-09-14`,
/// midnight) or a UTC timestamp (`2026-09-14T08:00:00Z`). A bare number is
/// refused: `--since 7` reads as seven of something, and guessing which
/// something is how a weekly report silently becomes an all-time one.
pub(crate) fn parse_since(text: &str, now: u64) -> Result<u64> {
    let text = text.trim();
    if let Some(seconds) = relative_window(text) {
        return Ok(now.saturating_sub(seconds));
    }
    if let Some(instant) = willdeep_core::session::parse_iso8601_utc(text) {
        return Ok(instant);
    }
    bail!(
        "cannot read `{text}` as a window: use `<N>d` / `<N>h` / `<N>w`, a UTC date like 2026-09-14, or a UTC timestamp like 2026-09-14T08:00:00Z"
    )
}

fn relative_window(text: &str) -> Option<u64> {
    let (digits, unit) = text.split_at(text.len().checked_sub(1)?);
    let count: u64 = digits.parse().ok()?;
    let unit_seconds = match unit {
        "h" => SECONDS_PER_HOUR,
        "d" => SECONDS_PER_DAY,
        "w" => SECONDS_PER_WEEK,
        _ => return None,
    };
    count.checked_mul(unit_seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use willdeep_runtime_protocol::AgentStatus;

    struct Run {
        profile: Option<&'static str>,
        child: bool,
        verifier: Option<bool>,
        attempts: Option<u64>,
        claims: Option<(u64, u64)>,
        created_at: u64,
    }

    fn run(profile: &'static str) -> Run {
        Run {
            profile: Some(profile),
            child: true,
            verifier: None,
            attempts: None,
            claims: None,
            created_at: 1_000,
        }
    }

    fn agent(run: Run) -> RuntimeAgent {
        RuntimeAgent {
            id: uuid::Uuid::new_v4(),
            parent_id: run.child.then(uuid::Uuid::new_v4),
            task_id: uuid::Uuid::new_v4(),
            label: None,
            background: true,
            workspace: None,
            root_workspace: None,
            worktree_branch: None,
            dedicated_worktree: false,
            profile: run.profile.map(str::to_owned),
            model: None,
            status: AgentStatus::Completed,
            current_turn: 1,
            current_tool: None,
            retry_wait: None,
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            max_turns: None,
            token_budget: None,
            timeout_seconds: None,
            report: None,
            verifier_passed: run.verifier,
            claims_checked: run.claims.map(|(checked, _)| checked),
            claims_unverifiable: run.claims.map(|(_, bad)| bad),
            attempts: run.attempts,
            repo_commit: None,
            created_at: run.created_at,
            updated_at: run.created_at,
            completed_at: Some(run.created_at + 10),
        }
    }

    #[test]
    fn empty_runtime_reports_no_rates_rather_than_zero_percent() {
        let metrics = AgentMetrics::compute(&[], None);
        assert_eq!(metrics.children, 0);
        assert_eq!(metrics.deep_share, None);
        assert_eq!(metrics.worker_verified_success, None);
        assert_eq!(metrics.citation_accuracy, None);
        assert_eq!(metrics.attempts_per_verified_run, None);
        let text = metrics.render_text();
        assert!(text.contains("deep_share\t-\t"), "{text}");
        assert!(
            text.contains(
                "worker_verified_success\t-\t(verifier passes / runs with a verifier: 0/0"
            ),
            "{text}"
        );
        assert!(text.contains("attempts_per_verified_run\t-\n"), "{text}");
        let json = serde_json::to_value(&metrics).expect("serialise");
        assert_eq!(json["deep_share"], serde_json::Value::Null);
        assert_eq!(json["children"], 0);
    }

    #[test]
    fn tiers_verdicts_and_citations_are_counted_from_child_runs_only() {
        let agents = vec![
            // The root is never delegation, whatever its verdict says.
            agent(Run {
                child: false,
                verifier: Some(true),
                ..run("implementer")
            }),
            agent(Run {
                verifier: Some(true),
                attempts: Some(1),
                ..run("test_fixer")
            }),
            agent(Run {
                verifier: Some(false),
                attempts: Some(3),
                ..run("build_fixer")
            }),
            agent(Run {
                verifier: Some(true),
                attempts: Some(2),
                ..run("implementer")
            }),
            agent(run("deep")),
            agent(Run {
                claims: Some((4, 1)),
                ..run("reader")
            }),
            agent(Run {
                profile: None,
                ..run("reader")
            }),
        ];
        let metrics = AgentMetrics::compute(&agents, None);
        assert_eq!(metrics.children, 6);
        assert_eq!((metrics.workers, metrics.standard, metrics.deep), (3, 1, 1));
        assert_eq!(
            (
                metrics.verified_runs,
                metrics.passed,
                metrics.unverified_runs
            ),
            (3, 2, 3)
        );
        assert_eq!(metrics.attempts, 6);
        assert_eq!(metrics.deep_share, Some(16.7));
        assert_eq!(metrics.skill_coverage, Some(50.0));
        assert_eq!(metrics.worker_verified_success, Some(66.7));
        assert_eq!(metrics.escalation_rate, Some(33.3));
        assert_eq!(
            (metrics.claims_checked, metrics.claims_unverifiable),
            (4, 1)
        );
        assert_eq!(metrics.citation_accuracy, Some(75.0));
        assert_eq!(metrics.attempts_per_verified_run, Some(2.0));
        let text = metrics.render_text();
        assert!(
            text.contains("model_tiers\tworker=3\tstandard=1\tdeep=1\n"),
            "{text}"
        );
        assert!(text.contains("worker_verified_success\t66.7%\t(verifier passes / runs with a verifier: 2/3; target >= 85%)"), "{text}");
        assert!(text.contains("citation_accuracy\t75.0%\t(cited locations that exist / cited locations checked in report-only runs: 3/4)"), "{text}");
        assert!(!text.starts_with("since"), "{text}");
    }

    #[test]
    fn a_window_drops_runs_created_before_it() {
        let agents = vec![
            agent(Run {
                verifier: Some(false),
                attempts: Some(3),
                created_at: 100,
                ..run("editor")
            }),
            agent(Run {
                verifier: Some(true),
                attempts: Some(1),
                created_at: 200,
                ..run("editor")
            }),
            agent(Run {
                verifier: Some(true),
                attempts: Some(1),
                created_at: 300,
                ..run("editor")
            }),
        ];
        let all = AgentMetrics::compute(&agents, None);
        assert_eq!(all.worker_verified_success, Some(66.7));
        let windowed = AgentMetrics::compute(&agents, Some(200));
        assert_eq!(windowed.since, Some(200));
        assert_eq!(windowed.children, 2);
        assert_eq!(windowed.worker_verified_success, Some(100.0));
        assert!(windowed.render_text().starts_with("since\t200\t"));
        let empty = AgentMetrics::compute(&agents, Some(301));
        assert_eq!(empty.children, 0);
        assert_eq!(empty.worker_verified_success, None);
    }

    #[test]
    fn unverifiable_claims_never_exceed_checked_claims() {
        let agents = vec![agent(Run {
            claims: Some((2, 5)),
            ..run("reader")
        })];
        let metrics = AgentMetrics::compute(&agents, None);
        assert_eq!(metrics.claims_unverifiable, 2);
        assert_eq!(metrics.citation_accuracy, Some(0.0));
    }

    #[test]
    fn since_accepts_windows_dates_and_timestamps_but_not_bare_numbers() {
        let now = 1_800_000_000;
        assert_eq!(parse_since("7d", now).unwrap(), now - 7 * SECONDS_PER_DAY);
        assert_eq!(
            parse_since(" 36h ", now).unwrap(),
            now - 36 * SECONDS_PER_HOUR
        );
        assert_eq!(parse_since("2w", now).unwrap(), now - 2 * SECONDS_PER_WEEK);
        assert_eq!(parse_since("2026-09-14", now).unwrap(), 1_789_344_000);
        assert_eq!(
            parse_since("2026-09-14T08:00:00Z", now).unwrap(),
            1_789_344_000 + 8 * SECONDS_PER_HOUR
        );
        assert_eq!(
            parse_since("999999d", 10).unwrap(),
            0,
            "a window before the epoch clamps to zero"
        );
        for bad in ["7", "", "d", "7x", "yesterday", "2026-13-01"] {
            assert!(parse_since(bad, now).is_err(), "`{bad}` must be refused");
        }
    }
}
