use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use uuid::Uuid;
use willdeep_core::feedback_assessment::{Assessment, PROMPT};

use crate::feedback_cmd::FeedbackAction;

#[derive(Clone, Debug, Default, Serialize)]
struct ReviewRun {
    run_id: Uuid,
    session_id: Option<Uuid>,
    turn_id: Option<String>,
    tool_successes: usize,
    tool_failures: usize,
    stop_reason: Option<String>,
    verification: Option<String>,
    human_disposition: Option<String>,
    runtime_parameters_sha256: Option<String>,
    terminal: bool,
}

impl ReviewRun {
    fn priority(&self) -> u8 {
        if self.verification.as_deref() == Some("failed") || self.tool_failures > 0 {
            2
        } else if self.verification.as_deref() != Some("passed") {
            1
        } else {
            0
        }
    }

    fn facts(&self) -> serde_json::Value {
        serde_json::json!({ "tool_successes": self.tool_successes,
            "tool_failures": self.tool_failures, "stop_reason": self.stop_reason,
            "verification": self.verification.as_deref().unwrap_or("unverified") })
    }
}

fn ingest(runs: &mut BTreeMap<Uuid, ReviewRun>, value: &serde_json::Value) {
    if value["schema"] != "willdeep.feedback.v1" {
        return;
    }
    let Some(id) = value["run_id"]
        .as_str()
        .and_then(|id| Uuid::parse_str(id).ok())
    else {
        return;
    };
    let run = runs.entry(id).or_insert_with(|| ReviewRun {
        run_id: id,
        ..Default::default()
    });
    run.session_id = run.session_id.or_else(|| {
        value["session_id"]
            .as_str()
            .and_then(|id| Uuid::parse_str(id).ok())
    });
    if run.turn_id.is_none() {
        run.turn_id = value["turn_id"].as_str().map(str::to_owned);
    }
    if let Some(hash) = value["runtime_parameters_sha256"].as_str()
        && hash.len() == 64
        && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        run.runtime_parameters_sha256 = Some(hash.to_owned());
    }
    match value["signal"].as_str() {
        Some("tool_succeeded") => run.tool_successes += 1,
        Some("tool_failed") => run.tool_failures += 1,
        Some("run_verified" | "run_finished") => {
            run.terminal = true;
            if let Some(verification) = value["verification"]
                .as_str()
                .filter(|value| ["passed", "failed", "stale", "unverified"].contains(value))
            {
                run.verification = Some(verification.into());
            }
        }
        Some("human_disposition") if value["judgment_source"] == "human" => {
            run.human_disposition = value["judgment"]
                .as_str()
                .filter(|value| ["accepted", "needs_changes", "unknown"].contains(value))
                .map(str::to_owned);
        }
        _ => {}
    }
    if let Some(reason) = value["stop_reason"].as_str()
        && [
            "complete",
            "goal_complete",
            "incomplete",
            "unverified",
            "finished",
            "cancelled",
            "error",
            "max_turns",
            "token_budget",
            "unknown_usage",
            "budget_limited",
        ]
        .contains(&reason)
    {
        run.stop_reason = Some(reason.into());
    }
}

fn load(home: &Path) -> Result<BTreeMap<Uuid, ReviewRun>> {
    let directory = willdeep_core::feedback::feedback_dir(home);
    if !directory.exists() {
        return Ok(BTreeMap::new());
    }
    let mut paths = std::fs::read_dir(directory)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|value| value == "jsonl"))
        .collect::<Vec<_>>();
    paths.sort();
    let mut runs = BTreeMap::new();
    for path in paths {
        for line in BufReader::new(std::fs::File::open(path)?).lines() {
            let line = line?;
            if line.len() > willdeep_core::feedback::MAX_LINE_BYTES {
                continue;
            }
            if let Ok(value) = serde_json::from_str(&line) {
                ingest(&mut runs, &value);
            }
        }
    }
    Ok(runs)
}

pub(crate) async fn run(action: FeedbackAction, cli: &crate::Cli) -> Result<()> {
    let home = crate::config::willdeep_home()?;
    match action {
        FeedbackAction::ReviewQueue { limit } => {
            let mut queue = load(&home)?
                .into_values()
                .filter(|run| run.terminal && run.human_disposition.is_none())
                .collect::<Vec<_>>();
            queue.sort_by_key(|run| (std::cmp::Reverse(run.priority()), run.run_id));
            queue.truncate(usize::from(limit));
            println!("{}", serde_json::to_string_pretty(&queue)?);
        }
        FeedbackAction::Review {
            run,
            assist,
            decision,
        } => {
            let loaded = crate::config::LoadedConfig::load(cli.config.as_deref())?;
            if (assist || decision.is_some()) && !loaded.file.feedback.enabled {
                bail!("feedback recording is disabled in the configuration");
            }
            let mut runs = load(&home)?;
            let evidence = runs
                .remove(&run)
                .context("run is not present in the feedback ledger")?;
            if !evidence.terminal {
                bail!("run has no terminal evidence; review after it finishes");
            }
            let sink =
                willdeep_core::feedback::shared_sink(&willdeep_core::feedback::feedback_dir(&home));
            let recorder =
                willdeep_core::feedback::FeedbackRecorder::new(sink.clone(), "cli", false)
                    .with_session(evidence.session_id)
                    .with_turn(evidence.turn_id.clone());
            if let Some(decision) = decision {
                recorder.record_review(
                    run,
                    None,
                    Some(&decision),
                    None,
                    evidence.runtime_parameters_sha256.clone(),
                );
                if !sink.flush(Duration::from_secs(5)) {
                    bail!("human feedback could not be flushed");
                }
                println!(
                    "{}",
                    serde_json::json!({"run_id": run, "judgment_source": "human", "judgment": decision})
                );
            } else if assist {
                willdeep_core::runtime_parameters::RuntimeParameters::load(&home)
                    .map_err(anyhow::Error::msg)?;
                let mut configuration =
                    crate::harness::resolve_parent_provider_config(cli, &loaded, None)?;
                configuration.max_output_tokens = 512;
                let provider = willdeep_core::build_provider(configuration)?;
                let identity = provider.ledger_identity();
                let provider =
                    crate::harness::standalone_usage_ledger(&home, evidence.session_id, None)
                        .auxiliary(provider);
                let messages = [
                    willdeep_core::Message::system(PROMPT),
                    willdeep_core::Message::user(evidence.facts().to_string()),
                ];
                let result = tokio::time::timeout(
                    Duration::from_secs(60),
                    provider.complete(&messages, &[]),
                )
                .await
                .context("feedback assessment timed out")?
                .map_err(|_| anyhow::anyhow!("feedback assessment provider request failed"))?;
                let assessment = Assessment::parse(&result.content)
                    .context("model returned an invalid feedback assessment")?;
                recorder.record_review(
                    run,
                    Some(&assessment),
                    None,
                    identity,
                    evidence.runtime_parameters_sha256.clone(),
                );
                if !sink.flush(Duration::from_secs(5)) {
                    bail!("model feedback could not be flushed");
                }
                println!(
                    "{}",
                    serde_json::json!({"run_id": run, "judgment_source": "model", "assessment": assessment, "human_disposition": null})
                );
            } else {
                println!("{}", serde_json::to_string_pretty(&evidence)?);
            }
        }
        other => return crate::feedback_cmd::run(other, &home),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_assessment_never_removes_a_run_from_human_review() {
        let id = Uuid::new_v4();
        let mut runs = BTreeMap::new();
        ingest(
            &mut runs,
            &serde_json::json!({"schema":"willdeep.feedback.v1", "run_id":id, "signal":"model_assessment", "judgment_source":"model", "judgment":"accepted"}),
        );
        assert!(runs[&id].human_disposition.is_none());
        ingest(
            &mut runs,
            &serde_json::json!({"schema":"willdeep.feedback.v1", "run_id":id, "signal":"tool_failed"}),
        );
        assert_eq!(runs[&id].priority(), 2);
        ingest(
            &mut runs,
            &serde_json::json!({"schema":"willdeep.feedback.v1", "run_id":id, "signal":"human_disposition", "judgment_source":"human", "judgment":"needs_changes"}),
        );
        assert_eq!(
            runs[&id].human_disposition.as_deref(),
            Some("needs_changes")
        );
        assert!(!runs[&id].facts().to_string().contains("run_id"));
    }
}
