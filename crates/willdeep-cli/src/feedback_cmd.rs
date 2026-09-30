//! `willdeep feedback`：反馈账本（`docs/FEEDBACK_LEDGER.md`）的跨会话汇总，
//! 以及由确定性规则得出的提示词改进候选（`docs/PROMPT_RSI_DESIGN.md` §8.1）。
//!
//! 只读账本、只出计数与 id：账本里即使存了正文（`store_text`），报告和候选
//! 文件也一个字都不带。本命令不改任何提示词——候选是给离线优化器或人看的
//! 材料，晋升要过设计文档 §11 的门禁。

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Subcommand;
use serde::Serialize;
use uuid::Uuid;

use crate::audit_cmd::{FeedbackRow, classify_followups, load_feedback, parse_time};

/// 同一 `(工具, 错误类别)` 至少这么多次才成为候选。
const TOOL_FAILURE_MIN: usize = 5;
/// 某工种「没有结果」的运行占比达到这个值、且样本够数，就成为候选。
const NO_RESULT_RATE: f64 = 0.20;
const NO_RESULT_MIN_RUNS: usize = 5;
/// 某个建议版本展示够数、采用率却低于这个值。
const ACCEPT_RATE_FLOOR: f64 = 0.15;
const ACCEPT_MIN_SHOWN: usize = 20;
/// 宣告完成被门禁打回的比例（虚报完成）。
const REJECTION_RATE: f64 = 0.30;
const REJECTION_MIN_CLAIMS: usize = 5;
/// 后续输入里算作纠正的比例。
const CORRECTION_RATE: f64 = 0.25;
const CORRECTION_MIN_FOLLOWUPS: usize = 10;
/// 每条候选最多列几个会话 id 作例子。
const MAX_EXAMPLES: usize = 5;
/// 文本报告里工具失败排行的条数。
const TOP_TOOL_FAILURES: usize = 15;
/// 没有版本戳的旧行归到这个键下。
const UNSTAMPED: &str = "unstamped";

#[derive(Clone, Debug, Subcommand)]
pub(crate) enum FeedbackAction {
    /// Print the prompt bundle id of every role in this build (main agent, input suggestions, each worker profile).
    ///
    /// Feedback rows carry the bundle id of the prompt that produced them; a
    /// changed id means that prompt changed.
    Bundles,
    /// Summarize the feedback ledger across sessions and derive prompt improvement candidates.
    ///
    /// Counts and ids only: suggestion, prompt and message text never appear,
    /// even when the ledger stores it.
    Report {
        /// Only rows at or after this time: YYYY-MM-DD, YYYY-MM-DDTHH:MM:SSZ or a Unix timestamp.
        #[arg(long, value_name = "TIME")]
        since: Option<String>,
        /// Emit JSON instead of text.
        #[arg(long)]
        json: bool,
        /// Also write the improvement candidates as JSON to this file.
        #[arg(long, value_name = "PATH")]
        candidates: Option<PathBuf>,
    },
}

pub(crate) fn run(action: FeedbackAction, home: &Path) -> Result<()> {
    match action {
        FeedbackAction::Bundles => {
            for bundle in willdeep_core::prompt_bundle::current_bundles() {
                println!("{bundle}");
            }
            Ok(())
        }
        FeedbackAction::Report {
            since,
            json,
            candidates,
        } => {
            let since_ms = since
                .as_deref()
                .map(parse_time)
                .transpose()?
                .map(|seconds| seconds.saturating_mul(1_000));
            let (rows, unparsable) = load_feedback(&willdeep_core::feedback::feedback_dir(home))?;
            let report = build_report(&rows, unparsable, since, since_ms, now_ms());
            if let Some(path) = candidates {
                let file = CandidateFile {
                    generated_at: report.generated_at.clone(),
                    window: report.window.clone(),
                    candidates: report.candidates.clone(),
                };
                std::fs::write(&path, serde_json::to_vec_pretty(&file)?)
                    .with_context(|| format!("write {}", path.display()))?;
                eprintln!(
                    "{} candidate(s) written to {}",
                    file.candidates.len(),
                    path.display()
                );
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", render_text(&report));
            }
            Ok(())
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

#[derive(Debug, Serialize)]
pub(crate) struct FeedbackReport {
    pub generated_at: String,
    pub window: Window,
    /// 按输入建议的提示词版本。
    pub suggestions: BTreeMap<String, SuggestionStats>,
    /// 按 Worker 工种。
    pub workers: BTreeMap<String, WorkerStats>,
    /// `(工具, 错误类别)` 按次数从多到少。
    pub tool_failures: Vec<ToolFailureStats>,
    pub goals: GoalStats,
    pub corrections: CorrectionStats,
    pub candidates: Vec<Candidate>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Window {
    /// `--since` 原样。
    pub since: Option<String>,
    /// 窗口内最早 / 最晚一行的时间。
    pub first: Option<String>,
    pub last: Option<String>,
    pub rows: usize,
    /// 坏行或不认识的 schema。
    pub unparsable: usize,
    pub sessions: usize,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct SuggestionStats {
    pub shown: usize,
    pub accepted: usize,
    pub dismissed: usize,
    pub sent_verbatim: usize,
    pub sent_edited: usize,
    pub accept_rate: Option<f64>,
    pub verbatim_rate: Option<f64>,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct WorkerStats {
    /// 派工次数（`worker_started`）；没有这类行的旧数据退回按 Worker id 去重。
    pub runs: usize,
    pub tool_failures: usize,
    /// 没收敛就停下的运行（含交回了部分结果的）。
    pub incomplete: usize,
    /// 一个字都没交回来的运行：空报告的未收敛、超时、验证用尽。
    pub no_result: usize,
    pub timed_out: usize,
    pub verifier_exhausted: usize,
    pub no_result_rate: Option<f64>,
    /// 窗口内见过的该工种提示词版本。
    pub bundles: BTreeSet<String>,
    #[serde(skip)]
    no_result_sessions: Vec<Uuid>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ToolFailureStats {
    pub tool: String,
    pub error_class: String,
    pub count: usize,
    pub bundles: BTreeSet<String>,
    pub sessions: Vec<Uuid>,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct GoalStats {
    pub completed: usize,
    pub completion_rejected: usize,
    pub budget_limited: usize,
    /// 被打回 / (完成 + 被打回)。
    pub rejection_rate: Option<f64>,
    #[serde(skip)]
    rejected_sessions: Vec<Uuid>,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct CorrectionStats {
    pub followups: usize,
    pub corrective: usize,
    /// 上一轮以 `completed` 收尾却马上被纠正。
    pub completed_then_corrected: usize,
    pub rate: Option<f64>,
    /// 按主 Agent 提示词版本。
    pub by_bundle: BTreeMap<String, RateCount>,
    /// 按周（周一的日期，UTC）。
    pub weekly: Vec<WeekStats>,
    /// 纠正最集中的会话。
    pub top_sessions: Vec<SessionCount>,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct RateCount {
    pub total: usize,
    pub hits: usize,
    pub rate: Option<f64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct WeekStats {
    pub week: String,
    pub followups: usize,
    pub corrective: usize,
    pub rate: Option<f64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SessionCount {
    pub session_id: Uuid,
    pub corrective: usize,
}

/// 设计文档 §8.2 的一条输入：该改哪段提示词、凭什么。
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Candidate {
    pub target: Target,
    pub signal: String,
    pub evidence: Evidence,
    pub suggestion: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Target {
    /// `main`、`worker:<工种>`、`input_suggestion`、`tool:<工具>`。
    pub role: String,
    pub bundle: Option<String>,
    /// 建议改动的提示词段落。
    pub section: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Evidence {
    pub count: usize,
    pub rate: Option<f64>,
    pub examples: Vec<Uuid>,
}

#[derive(Debug, Serialize)]
struct CandidateFile {
    generated_at: String,
    window: Window,
    candidates: Vec<Candidate>,
}

fn rate(hits: usize, total: usize) -> Option<f64> {
    (total > 0).then(|| hits as f64 / total as f64)
}

fn push_example(examples: &mut Vec<Uuid>, session: Option<Uuid>) {
    if let Some(session) = session
        && examples.len() < MAX_EXAMPLES
        && !examples.contains(&session)
    {
        examples.push(session);
    }
}

fn bundle_key(row: &FeedbackRow) -> String {
    row.prompt_bundle
        .clone()
        .unwrap_or_else(|| UNSTAMPED.to_owned())
}

/// 窗口内出现最多的那个版本（没有戳的不算）。
fn dominant_bundle<'a>(rows: impl Iterator<Item = &'a FeedbackRow>) -> Option<String> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for row in rows {
        if let Some(bundle) = &row.prompt_bundle {
            *counts.entry(bundle).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(bundle, _)| bundle.to_owned())
}

/// Unix 毫秒所在那一周的周一（UTC），`YYYY-MM-DD`。
fn week_of(ts_ms: u64) -> String {
    let days = ts_ms / 86_400_000;
    // 1970-01-01 是周四：往回退到周一。
    let monday = days - (days + 3) % 7;
    willdeep_core::session::format_iso8601(monday * 86_400)
        .get(..10)
        .unwrap_or("unknown")
        .to_owned()
}

pub(crate) fn build_report(
    all_rows: &[FeedbackRow],
    unparsable: usize,
    since: Option<String>,
    since_ms: Option<u64>,
    now_ms: u64,
) -> FeedbackReport {
    let rows: Vec<&FeedbackRow> = all_rows
        .iter()
        .filter(|row| since_ms.is_none_or(|floor| row.ts_ms >= floor))
        .collect();
    let stamps = rows.iter().map(|row| row.ts_ms).filter(|ts| *ts > 0);
    let window = Window {
        since,
        first: stamps
            .clone()
            .min()
            .map(willdeep_core::usage_ledger::format_ts),
        last: stamps.max().map(willdeep_core::usage_ledger::format_ts),
        rows: rows.len(),
        unparsable,
        sessions: rows
            .iter()
            .filter_map(|row| row.session_id)
            .collect::<BTreeSet<_>>()
            .len(),
    };

    let mut suggestions: BTreeMap<String, SuggestionStats> = BTreeMap::new();
    let mut workers: BTreeMap<String, WorkerStats> = BTreeMap::new();
    let mut worker_ids: HashMap<String, BTreeSet<Uuid>> = HashMap::new();
    let mut incomplete_ids: HashMap<String, BTreeSet<Uuid>> = HashMap::new();
    let mut no_result_ids: HashMap<String, BTreeSet<Uuid>> = HashMap::new();
    let mut tools: BTreeMap<(String, String), ToolFailureStats> = BTreeMap::new();
    let mut goals = GoalStats::default();
    let mut sessions: BTreeMap<Uuid, Vec<&FeedbackRow>> = BTreeMap::new();

    for row in &rows {
        if let Some(session) = row.session_id {
            sessions.entry(session).or_default().push(row);
        }
        if row.signal.starts_with("suggestion_") {
            let stats = suggestions.entry(bundle_key(row)).or_default();
            match row.signal.as_str() {
                "suggestion_shown" => stats.shown += 1,
                "suggestion_accepted" => stats.accepted += 1,
                "suggestion_dismissed" => stats.dismissed += 1,
                "suggestion_sent_verbatim" => stats.sent_verbatim += 1,
                "suggestion_sent_edited" => stats.sent_edited += 1,
                _ => {}
            }
        }
        if let Some(profile) = &row.worker_profile {
            let stats = workers.entry(profile.clone()).or_default();
            if let Some(bundle) = &row.prompt_bundle {
                stats.bundles.insert(bundle.clone());
            }
            if let Some(agent) = row.agent_id {
                worker_ids.entry(profile.clone()).or_default().insert(agent);
            }
            let mut no_result = false;
            match row.signal.as_str() {
                "worker_started" => stats.runs += 1,
                "tool_failed" => stats.tool_failures += 1,
                "agent_incomplete" => {
                    if let Some(agent) = row.agent_id {
                        incomplete_ids
                            .entry(profile.clone())
                            .or_default()
                            .insert(agent);
                    } else {
                        stats.incomplete += 1;
                    }
                    no_result = row.report_len == Some(0);
                }
                "worker_timed_out" => {
                    stats.timed_out += 1;
                    no_result = true;
                }
                "worker_verifier_exhausted" => {
                    stats.verifier_exhausted += 1;
                    no_result = true;
                }
                _ => {}
            }
            if no_result {
                push_example(&mut stats.no_result_sessions, row.session_id);
                match row.agent_id {
                    Some(agent) => {
                        no_result_ids
                            .entry(profile.clone())
                            .or_default()
                            .insert(agent);
                    }
                    None => stats.no_result += 1,
                }
            }
        }
        if row.signal == "tool_failed" {
            let tool = row.tool.clone().unwrap_or_else(|| "?".to_owned());
            let class = row.error_class.clone().unwrap_or_else(|| "?".to_owned());
            let stats = tools
                .entry((tool.clone(), class.clone()))
                .or_insert_with(|| ToolFailureStats {
                    tool,
                    error_class: class,
                    count: 0,
                    bundles: BTreeSet::new(),
                    sessions: Vec::new(),
                });
            stats.count += 1;
            if let Some(bundle) = &row.prompt_bundle {
                stats.bundles.insert(bundle.clone());
            }
            push_example(&mut stats.sessions, row.session_id);
        }
        match row.signal.as_str() {
            "goal_completed" => goals.completed += 1,
            "goal_completion_rejected" => {
                goals.completion_rejected += 1;
                push_example(&mut goals.rejected_sessions, row.session_id);
            }
            "goal_budget_limited" => goals.budget_limited += 1,
            _ => {}
        }
    }

    for stats in suggestions.values_mut() {
        stats.accept_rate = rate(stats.accepted, stats.shown);
        stats.verbatim_rate = rate(stats.sent_verbatim, stats.shown);
    }
    for (profile, stats) in &mut workers {
        let distinct = worker_ids.get(profile).map_or(0, BTreeSet::len);
        stats.runs = stats.runs.max(distinct);
        stats.incomplete += incomplete_ids.get(profile).map_or(0, BTreeSet::len);
        stats.no_result += no_result_ids.get(profile).map_or(0, BTreeSet::len);
        stats.no_result_rate = rate(stats.no_result, stats.runs);
    }
    goals.rejection_rate = rate(
        goals.completion_rejected,
        goals.completed + goals.completion_rejected,
    );

    let mut corrections = CorrectionStats::default();
    let mut weeks: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut per_session: Vec<SessionCount> = Vec::new();
    for (session, mut session_rows) in sessions {
        session_rows.sort_by_key(|row| row.ts_ms);
        let mut corrective_here = 0;
        for (index, corrective) in classify_followups(&session_rows) {
            let row = session_rows[index];
            corrections.followups += 1;
            let week = weeks.entry(week_of(row.ts_ms)).or_default();
            week.0 += 1;
            let by_bundle = corrections.by_bundle.entry(bundle_key(row)).or_default();
            by_bundle.total += 1;
            if corrective {
                corrections.corrective += 1;
                corrective_here += 1;
                week.1 += 1;
                by_bundle.hits += 1;
                if row.prev_status.as_deref() == Some("completed") {
                    corrections.completed_then_corrected += 1;
                }
            }
        }
        if corrective_here > 0 {
            per_session.push(SessionCount {
                session_id: session,
                corrective: corrective_here,
            });
        }
    }
    corrections.rate = rate(corrections.corrective, corrections.followups);
    for count in corrections.by_bundle.values_mut() {
        count.rate = rate(count.hits, count.total);
    }
    corrections.weekly = weeks
        .into_iter()
        .map(|(week, (followups, corrective))| WeekStats {
            week,
            followups,
            corrective,
            rate: rate(corrective, followups),
        })
        .collect();
    per_session.sort_by(|a, b| {
        b.corrective
            .cmp(&a.corrective)
            .then(a.session_id.cmp(&b.session_id))
    });
    per_session.truncate(MAX_EXAMPLES);
    corrections.top_sessions = per_session;

    let mut tool_failures: Vec<ToolFailureStats> = tools.into_values().collect();
    tool_failures.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| (&a.tool, &a.error_class).cmp(&(&b.tool, &b.error_class)))
    });

    let mut report = FeedbackReport {
        generated_at: willdeep_core::usage_ledger::format_ts(now_ms),
        window,
        suggestions,
        workers,
        tool_failures,
        goals,
        corrections,
        candidates: Vec::new(),
    };
    report.candidates = candidates(&report, &rows);
    report
}

/// 某类工具失败对应的提示词段落与改法。
fn tool_advice(tool: &str, class: &str) -> (String, String) {
    match class {
        "edit_text_not_found" | "edit_text_not_unique" | "identical_edit" => (
            "tool_rules:edit".to_owned(),
            format!(
                "`{tool}` keeps missing its target ({class}): strengthen the read-before-edit rule — copy old text verbatim from a fresh read and include enough surrounding lines to be unique."
            ),
        ),
        "invalid_arguments" | "unknown_tool" => (
            format!("tool:{tool}:description"),
            format!(
                "`{tool}` is called with arguments it rejects ({class}): make its description and parameter schema state the required fields and formats explicitly."
            ),
        ),
        "outside_workspace" | "read_only_policy" => (
            "boundary".to_owned(),
            format!(
                "`{tool}` keeps hitting the workspace or write-scope boundary ({class}): restate which paths are writable in the boundary section or the task brief."
            ),
        ),
        "command_timeout" => (
            "tool_rules:commands".to_owned(),
            format!(
                "`{tool}` times out repeatedly: steer long-running commands to run_in_background or a narrower scope."
            ),
        ),
        _ => (
            format!("tool:{tool}:description"),
            format!(
                "`{tool}` fails with {class} repeatedly: review its description and the matching tool rule."
            ),
        ),
    }
}

/// 确定性规则，不调模型。阈值见文件头的常量。
fn candidates(report: &FeedbackReport, rows: &[&FeedbackRow]) -> Vec<Candidate> {
    let mut out = Vec::new();
    for failure in &report.tool_failures {
        // 人拒绝审批、hook 拦下是人的决定，不是提示词的毛病。
        if failure.count < TOOL_FAILURE_MIN
            || matches!(
                failure.error_class.as_str(),
                "approval_denied" | "hook_denied"
            )
        {
            continue;
        }
        let (section, suggestion) = tool_advice(&failure.tool, &failure.error_class);
        let bundle = dominant_bundle(rows.iter().copied().filter(|row| {
            row.signal == "tool_failed"
                && row.tool.as_deref() == Some(failure.tool.as_str())
                && row.error_class.as_deref() == Some(failure.error_class.as_str())
        }));
        out.push(Candidate {
            target: Target {
                role: bundle
                    .as_deref()
                    .and_then(|bundle| bundle.split('@').next())
                    .map_or_else(|| format!("tool:{}", failure.tool), str::to_owned),
                bundle,
                section,
            },
            signal: format!("tool_failed:{}/{}", failure.tool, failure.error_class),
            evidence: Evidence {
                count: failure.count,
                rate: None,
                examples: failure.sessions.clone(),
            },
            suggestion,
        });
    }
    for (profile, stats) in &report.workers {
        let Some(no_result_rate) = stats.no_result_rate else {
            continue;
        };
        if stats.runs < NO_RESULT_MIN_RUNS || no_result_rate < NO_RESULT_RATE {
            continue;
        }
        out.push(Candidate {
            target: Target {
                role: format!("worker:{profile}"),
                bundle: stats.bundles.iter().next_back().cloned(),
                section: "capability_prompt".to_owned(),
            },
            signal: "worker_no_result".to_owned(),
            evidence: Evidence {
                count: stats.no_result,
                rate: Some(no_result_rate),
                examples: stats.no_result_sessions.clone(),
            },
            suggestion: format!(
                "{} of {} `{profile}` runs returned nothing ({} timed out, {} exhausted the verifier): tighten the scope the brief hands this profile, or revisit its turn budget.",
                stats.no_result, stats.runs, stats.timed_out, stats.verifier_exhausted
            ),
        });
    }
    for (bundle, stats) in &report.suggestions {
        let Some(accept_rate) = stats.accept_rate else {
            continue;
        };
        if stats.shown < ACCEPT_MIN_SHOWN || accept_rate >= ACCEPT_RATE_FLOOR {
            continue;
        }
        let mut examples = Vec::new();
        for row in rows
            .iter()
            .filter(|row| row.signal == "suggestion_shown" && bundle_key(row) == *bundle)
        {
            push_example(&mut examples, row.session_id);
        }
        out.push(Candidate {
            target: Target {
                role: willdeep_core::prompt_bundle::INPUT_SUGGESTION.to_owned(),
                bundle: (bundle != UNSTAMPED).then(|| bundle.clone()),
                section: "system_prompt".to_owned(),
            },
            signal: "suggestion_low_accept".to_owned(),
            evidence: Evidence {
                count: stats.shown,
                rate: Some(accept_rate),
                examples,
            },
            suggestion: format!(
                "Only {} of {} suggestions were taken with Tab: revise the input suggestion prompt (next-step bias, length, when to answer NONE).",
                stats.accepted, stats.shown
            ),
        });
    }
    let claims = report.goals.completed + report.goals.completion_rejected;
    if let Some(rejection_rate) = report.goals.rejection_rate
        && claims >= REJECTION_MIN_CLAIMS
        && rejection_rate >= REJECTION_RATE
    {
        out.push(Candidate {
            target: Target {
                role: willdeep_core::prompt_bundle::MAIN.to_owned(),
                bundle: dominant_bundle(rows.iter().copied().filter(|row| {
                    row.signal == "goal_completion_rejected" && row.agent_id.is_none()
                })),
                section: "goal_continuation".to_owned(),
            },
            signal: "goal_completion_rejected".to_owned(),
            evidence: Evidence {
                count: report.goals.completion_rejected,
                rate: Some(rejection_rate),
                examples: report.goals.rejected_sessions.clone(),
            },
            suggestion: format!(
                "{} of {claims} completion claims were rejected by the gate: have the goal prompt define acceptance criteria up front and attach evidence to each before claiming done.",
                report.goals.completion_rejected
            ),
        });
    }
    if let Some(correction_rate) = report.corrections.rate
        && report.corrections.followups >= CORRECTION_MIN_FOLLOWUPS
        && correction_rate >= CORRECTION_RATE
    {
        out.push(Candidate {
            target: Target {
                role: willdeep_core::prompt_bundle::MAIN.to_owned(),
                bundle: dominant_bundle(
                    rows.iter()
                        .copied()
                        .filter(|row| row.signal == "user_followup"),
                ),
                section: "review".to_owned(),
            },
            signal: "user_correction".to_owned(),
            evidence: Evidence {
                count: report.corrections.corrective,
                rate: Some(correction_rate),
                examples: report
                    .corrections
                    .top_sessions
                    .iter()
                    .map(|session| session.session_id)
                    .collect(),
            },
            suggestion: format!(
                "{} of {} follow-ups corrected the previous turn ({} right after a completed turn): review the listed sessions to find which instruction the agent keeps getting wrong.",
                report.corrections.corrective,
                report.corrections.followups,
                report.corrections.completed_then_corrected
            ),
        });
    }
    out
}

fn percent(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_owned(), |value| format!("{:.0}%", value * 100.0))
}

pub(crate) fn render_text(report: &FeedbackReport) -> String {
    let mut out = String::new();
    let window = &report.window;
    out.push_str(&format!(
        "Feedback report ({} rows, {} sessions, {} unparsable)\n",
        window.rows, window.sessions, window.unparsable
    ));
    if let (Some(first), Some(last)) = (&window.first, &window.last) {
        out.push_str(&format!("Window: {first} .. {last}\n"));
    }

    out.push_str("\nInput suggestions (by prompt bundle)\n");
    if report.suggestions.is_empty() {
        out.push_str("  none\n");
    }
    for (bundle, stats) in &report.suggestions {
        out.push_str(&format!(
            "  {bundle}: shown {} accepted {} ({}) verbatim {} ({}) edited {} dismissed {}\n",
            stats.shown,
            stats.accepted,
            percent(stats.accept_rate),
            stats.sent_verbatim,
            percent(stats.verbatim_rate),
            stats.sent_edited,
            stats.dismissed
        ));
    }

    out.push_str("\nWorkers (by profile)\n");
    if report.workers.is_empty() {
        out.push_str("  none\n");
    }
    for (profile, stats) in &report.workers {
        out.push_str(&format!(
            "  {profile}: runs {} no-result {} ({}) incomplete {} timed-out {} verifier-exhausted {} tool-failures {}\n",
            stats.runs,
            stats.no_result,
            percent(stats.no_result_rate),
            stats.incomplete,
            stats.timed_out,
            stats.verifier_exhausted,
            stats.tool_failures
        ));
    }

    out.push_str("\nTool failures\n");
    if report.tool_failures.is_empty() {
        out.push_str("  none\n");
    }
    for failure in report.tool_failures.iter().take(TOP_TOOL_FAILURES) {
        out.push_str(&format!(
            "  {:>5}  {}/{}\n",
            failure.count, failure.tool, failure.error_class
        ));
    }

    let goals = &report.goals;
    out.push_str(&format!(
        "\nGoals: completed {} rejected {} ({}) budget-limited {}\n",
        goals.completed,
        goals.completion_rejected,
        percent(goals.rejection_rate),
        goals.budget_limited
    ));

    let corrections = &report.corrections;
    out.push_str(&format!(
        "\nCorrections: {} of {} follow-ups ({}), {} right after a completed turn\n",
        corrections.corrective,
        corrections.followups,
        percent(corrections.rate),
        corrections.completed_then_corrected
    ));
    for week in &corrections.weekly {
        out.push_str(&format!(
            "  week of {}: {}/{} ({})\n",
            week.week,
            week.corrective,
            week.followups,
            percent(week.rate)
        ));
    }

    out.push_str(&format!("\nCandidates ({})\n", report.candidates.len()));
    if report.candidates.is_empty() {
        out.push_str("  none\n");
    }
    for candidate in &report.candidates {
        out.push_str(&format!(
            "  [{}] {} / {}{}: {}\n",
            candidate.signal,
            candidate.target.role,
            candidate.target.section,
            candidate
                .target
                .bundle
                .as_deref()
                .map(|bundle| format!(" ({bundle})"))
                .unwrap_or_default(),
            candidate.suggestion
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: u64 = 3_600_000;
    /// 2026-09-28 00:00:00Z，周一。
    const MONDAY: u64 = 1_790_553_600_000;

    fn row(session: Uuid, ts_ms: u64, signal: &str) -> FeedbackRow {
        FeedbackRow {
            session_id: Some(session),
            agent_id: None,
            ts_ms,
            signal: signal.to_owned(),
            worker_profile: None,
            prompt_bundle: None,
            tool: None,
            error_class: None,
            report_len: None,
            followup_hint: None,
            prev_status: None,
            decision: None,
        }
    }

    fn worker_row(session: Uuid, agent: Uuid, ts_ms: u64, signal: &str) -> FeedbackRow {
        FeedbackRow {
            agent_id: Some(agent),
            worker_profile: Some("implementer".to_owned()),
            prompt_bundle: Some("worker:implementer@aaaaaaaaaaaa".to_owned()),
            ..row(session, ts_ms, signal)
        }
    }

    fn sample() -> Vec<FeedbackRow> {
        let session = Uuid::from_u128(1);
        let mut rows = Vec::new();
        // 建议：25 次展示、2 次采用 → 低采用率候选。
        for index in 0..25 {
            let mut shown = row(session, MONDAY + index, "suggestion_shown");
            shown.prompt_bundle = Some("input_suggestion@bbbbbbbbbbbb".to_owned());
            rows.push(shown);
        }
        for index in 0..2 {
            let mut accepted = row(session, MONDAY + 100 + index, "suggestion_accepted");
            accepted.prompt_bundle = Some("input_suggestion@bbbbbbbbbbbb".to_owned());
            rows.push(accepted);
        }
        // 工具失败：edit_text_not_found 6 次 → 候选；approval_denied 9 次 → 不是。
        for index in 0..6 {
            let mut failed = row(session, MONDAY + 200 + index, "tool_failed");
            failed.tool = Some("edit_file".to_owned());
            failed.error_class = Some("edit_text_not_found".to_owned());
            failed.prompt_bundle = Some("main@cccccccccccc".to_owned());
            rows.push(failed);
        }
        for index in 0..9 {
            let mut denied = row(session, MONDAY + 300 + index, "tool_failed");
            denied.tool = Some("run_command".to_owned());
            denied.error_class = Some("approval_denied".to_owned());
            rows.push(denied);
        }
        // Worker：5 次派工，2 次没有结果（一次空报告的未收敛 + 一次超时）。
        let agents: Vec<Uuid> = (10..15).map(Uuid::from_u128).collect();
        for (index, agent) in agents.iter().enumerate() {
            rows.push(worker_row(
                session,
                *agent,
                MONDAY + 400 + index as u64,
                "worker_started",
            ));
        }
        let mut empty = worker_row(session, agents[0], MONDAY + 500, "agent_incomplete");
        empty.report_len = Some(0);
        rows.push(empty);
        // 同一个 Worker 重试时又记了一次：按 Worker 去重。
        let mut again = worker_row(session, agents[0], MONDAY + 501, "agent_incomplete");
        again.report_len = Some(0);
        rows.push(again);
        let mut partial = worker_row(session, agents[1], MONDAY + 502, "agent_incomplete");
        partial.report_len = Some(40);
        rows.push(partial);
        rows.push(worker_row(
            session,
            agents[2],
            MONDAY + 503,
            "worker_timed_out",
        ));
        // Goal：3 完成、2 被打回 → 40% 被打回。
        for index in 0..3 {
            rows.push(row(session, MONDAY + 600 + index, "goal_completed"));
        }
        for index in 0..2 {
            rows.push(row(
                session,
                MONDAY + 700 + index,
                "goal_completion_rejected",
            ));
        }
        // 纠正：两周各 6 句后续输入。第一周 1 句纠正，第二周 3 句（其中一句紧跟回退）。
        for week in 0..2_u64 {
            let session = Uuid::from_u128(100 + u128::from(week));
            for index in 0..6_u64 {
                let at = MONDAY + week * 7 * 24 * HOUR + (index + 1) * HOUR;
                let mut followup = row(session, at, "user_followup");
                followup.prompt_bundle = Some("main@cccccccccccc".to_owned());
                let corrective = if week == 0 { index == 0 } else { index < 2 };
                followup.followup_hint = Some(
                    if corrective {
                        "correction"
                    } else {
                        "supplement"
                    }
                    .to_owned(),
                );
                followup.prev_status = Some("completed".to_owned());
                if week == 1 && index == 4 {
                    rows.push(row(session, at - 60_000, "session_rewound"));
                }
                rows.push(followup);
            }
        }
        rows
    }

    #[test]
    fn the_report_aggregates_across_sessions() {
        let report = build_report(&sample(), 2, None, None, MONDAY + 30 * 24 * HOUR);
        assert_eq!(report.window.unparsable, 2);
        assert_eq!(report.window.sessions, 3);
        let suggestions = &report.suggestions["input_suggestion@bbbbbbbbbbbb"];
        assert_eq!((suggestions.shown, suggestions.accepted), (25, 2));
        let worker = &report.workers["implementer"];
        assert_eq!(worker.runs, 5);
        assert_eq!(worker.no_result, 2, "deduplicated by worker id");
        assert_eq!(worker.incomplete, 2);
        assert_eq!(worker.timed_out, 1);
        assert_eq!(worker.no_result_rate, Some(0.4));
        assert_eq!(report.tool_failures[0].tool, "run_command");
        assert_eq!(report.tool_failures[0].count, 9);
        assert_eq!(report.goals.rejection_rate, Some(0.4));
        let corrections = &report.corrections;
        assert_eq!(corrections.followups, 12);
        assert_eq!(corrections.corrective, 4);
        assert_eq!(corrections.completed_then_corrected, 4);
        assert_eq!(corrections.weekly.len(), 2);
        assert_eq!(corrections.weekly[0].week, "2026-09-28");
        assert_eq!(
            (
                corrections.weekly[1].corrective,
                corrections.weekly[1].followups
            ),
            (3, 6)
        );
        assert_eq!(corrections.by_bundle["main@cccccccccccc"].hits, 4);
        assert_eq!(corrections.top_sessions[0].session_id, Uuid::from_u128(101));
    }

    #[test]
    fn candidates_follow_the_thresholds() {
        let report = build_report(&sample(), 0, None, None, MONDAY);
        let signals: Vec<&str> = report
            .candidates
            .iter()
            .map(|candidate| candidate.signal.as_str())
            .collect();
        assert_eq!(
            signals,
            [
                "tool_failed:edit_file/edit_text_not_found",
                "worker_no_result",
                "suggestion_low_accept",
                "goal_completion_rejected",
                "user_correction",
            ]
        );
        let tool = &report.candidates[0];
        assert_eq!(tool.target.role, "main");
        assert_eq!(tool.target.bundle.as_deref(), Some("main@cccccccccccc"));
        assert_eq!(tool.target.section, "tool_rules:edit");
        assert_eq!(tool.evidence.examples, [Uuid::from_u128(1)]);
        let worker = &report.candidates[1];
        assert_eq!(worker.target.role, "worker:implementer");
        assert_eq!(
            worker.target.bundle.as_deref(),
            Some("worker:implementer@aaaaaaaaaaaa")
        );

        // 窗口收窄到只剩第二周：样本不够的规则全部不触发。
        let late = build_report(
            &sample(),
            0,
            Some("2026-10-05".to_owned()),
            Some(MONDAY + 7 * 24 * HOUR),
            MONDAY,
        );
        assert_eq!(late.window.rows, 7);
        assert!(late.candidates.is_empty(), "{:?}", late.candidates);
    }

    #[test]
    fn outputs_never_carry_text() {
        let report = build_report(&sample(), 0, None, None, MONDAY);
        let text = render_text(&report);
        assert!(text.contains("Candidates (5)"));
        assert!(text.contains("week of 2026-09-28: 1/6 (17%)"));

        // 账本里存了正文（store_text）也只进计数：从真实的行走一遍加载。
        let dir = std::env::temp_dir().join(format!("willdeep-feedback-report-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let session = Uuid::new_v4();
        let mut lines = String::new();
        for (index, signal) in [
            "suggestion_shown",
            "suggestion_sent_edited",
            "user_followup",
        ]
        .iter()
        .enumerate()
        {
            lines.push_str(
                &serde_json::json!({
                    "schema": willdeep_core::feedback::SCHEMA,
                    "id": Uuid::new_v4(),
                    "ts": format!("2026-09-28T00:00:0{index}.000Z"),
                    "client": "tui",
                    "session_id": session,
                    "signal": signal,
                    "prompt_bundle": "input_suggestion@bbbbbbbbbbbb",
                    "followup_hint": "supplement",
                    "text": "SECRET-SENTINEL suggestion body",
                    "sent_text": "SECRET-SENTINEL sent body",
                })
                .to_string(),
            );
            lines.push('\n');
        }
        lines.push_str("not json\n");
        std::fs::write(dir.join("2026-09.jsonl"), lines).unwrap();
        let (rows, unparsable) = load_feedback(&dir).unwrap();
        assert_eq!((rows.len(), unparsable), (3, 1));
        let report = build_report(&rows, unparsable, None, None, MONDAY);
        assert_eq!(report.suggestions["input_suggestion@bbbbbbbbbbbb"].shown, 1);
        assert_eq!(report.corrections.followups, 1);
        let json = serde_json::to_string(&report).unwrap();
        for output in [json, render_text(&report)] {
            assert!(!output.contains("SECRET-SENTINEL"), "{output}");
            assert!(!output.contains("sent_text"), "{output}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
