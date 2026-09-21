//! `/daemon` — Runtime lifecycle from inside the TUI.
//!
//! The TUI is only a front end: tools execute inside the Runtime Daemon, and
//! a daemon started days ago keeps applying its own (old) approval policy.
//! Having to leave the TUI for a second terminal to fix that is exactly how
//! a stale Runtime goes unnoticed for two days.
//!
//! `upgrade` drains active work and can legitimately take minutes, so it is
//! never awaited on the UI thread — it runs as a task and reports through
//! the same notice channel everything else uses.

use std::path::{Path, PathBuf};

use tokio::sync::mpsc;

use super::UiMessage;
use crate::i18n::Language;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DaemonCommand {
    Status,
    Start,
    Stop,
    Upgrade,
}

/// Parse `/daemon [status|start|stop|upgrade]`. `None` means the prompt was
/// not a `/daemon` command at all; `Some(Err(usage))` means it was, but the
/// argument was not understood.
pub(super) fn parse(prompt: &str) -> Option<Result<DaemonCommand, String>> {
    let value = prompt.trim();
    if value != "/daemon" && !value.starts_with("/daemon ") {
        return None;
    }
    let argument = value.strip_prefix("/daemon").unwrap_or_default().trim();
    Some(match argument.to_ascii_lowercase().as_str() {
        "" | "status" => Ok(DaemonCommand::Status),
        "start" => Ok(DaemonCommand::Start),
        "stop" => Ok(DaemonCommand::Stop),
        "upgrade" => Ok(DaemonCommand::Upgrade),
        other => Err(format!(
            "usage: /daemon [status|start|stop|upgrade] (got `{other}`)"
        )),
    })
}

/// How long `/daemon upgrade` waits for a drain before reporting back. The
/// CLI default is 300s; the same budget applies here, but the wait happens
/// off the UI thread so the TUI stays responsive.
const UPGRADE_TIMEOUT_SECONDS: u64 = 300;

/// Run the command in the background, streaming progress and the final
/// result into the transcript.
pub(super) fn dispatch(
    command: DaemonCommand,
    home: PathBuf,
    language: Language,
    ui: mpsc::UnboundedSender<UiMessage>,
) {
    tokio::spawn(async move {
        let progress_ui = ui.clone();
        let report = move |line: String| {
            let _ = progress_ui.send(UiMessage::RuntimeNotice(format!("System: {line}")));
        };
        let result = run(command, &home, &report).await;
        let message = match result {
            Ok(message) => format!("System: {message}"),
            Err(error) => format!(
                "Error: {}: {error}",
                language.text(
                    "Runtime 操作失败",
                    "Runtime action failed",
                    "Runtime 操作に失敗"
                )
            ),
        };
        let _ = ui.send(UiMessage::RuntimeResult(message));
    });
}

/// Runtime 比客户端旧时自动升级一次。前提：Runtime 里没有任何未终结的任务。
/// 有的话不动它，说明原因，版本不一致的警告照旧留着让人决定。
pub(super) fn auto_upgrade(
    home: PathBuf,
    language: Language,
    ui: mpsc::UnboundedSender<UiMessage>,
) {
    tokio::spawn(async move {
        let message = match crate::daemon::runtime_unfinished_task_count(&home).await {
            Ok(0) => {
                let report = |_line: String| {};
                match crate::daemon::runtime_upgrade(&home, UPGRADE_TIMEOUT_SECONDS, &report).await {
                    Ok(_) => format!(
                        "System: {}",
                        language
                            .text(
                                "Runtime 比客户端旧，且没有进行中的任务，已自动升级到 {version}。",
                                "The Runtime was older than this client and idle, so it was upgraded to {version}.",
                                "Runtime がクライアントより古く待機中だったため、{version} へ自動アップグレードしました。",
                            )
                            .replace("{version}", willdeep_core::VERSION)
                    ),
                    Err(error) => format!(
                        "Error: {}: {error}",
                        language.text(
                            "自动升级 Runtime 失败，请手动 `/daemon upgrade`",
                            "Automatic Runtime upgrade failed; run `/daemon upgrade`",
                            "Runtime の自動アップグレードに失敗しました。`/daemon upgrade` を実行してください",
                        )
                    ),
                }
            }
            Ok(count) => format!(
                "System: {}",
                language
                    .text(
                        "Runtime 里还有 {n} 个任务在进行或等你处理，未自动升级（升级会丢掉等人的任务）。处理完后 `/daemon upgrade`。",
                        "{n} Runtime task(s) are still running or waiting on you, so no automatic upgrade (it would drop waiting tasks). Run `/daemon upgrade` once they finish.",
                        "Runtime に進行中または確認待ちのタスクが {n} 件あるため自動アップグレードしません。完了後に `/daemon upgrade` を実行してください。",
                    )
                    .replace("{n}", &count.to_string())
            ),
            Err(error) => format!(
                "System: {}: {error}",
                language.text(
                    "查不到 Runtime 任务列表，未自动升级",
                    "Could not list Runtime tasks; no automatic upgrade",
                    "Runtime のタスク一覧を取得できず、自動アップグレードしません",
                )
            ),
        };
        let _ = ui.send(UiMessage::RuntimeResult(message));
    });
}

async fn run(
    command: DaemonCommand,
    home: &Path,
    report: crate::daemon::DaemonProgress<'_>,
) -> anyhow::Result<String> {
    match command {
        DaemonCommand::Status => crate::daemon::runtime_status_message(home).await,
        DaemonCommand::Start => crate::daemon::runtime_start(home, report).await,
        DaemonCommand::Stop => crate::daemon::runtime_stop(home).await,
        DaemonCommand::Upgrade => {
            crate::daemon::runtime_upgrade(home, UPGRADE_TIMEOUT_SECONDS, report).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_documented_form() {
        assert_eq!(parse("/daemon"), Some(Ok(DaemonCommand::Status)));
        assert_eq!(parse("/daemon status"), Some(Ok(DaemonCommand::Status)));
        assert_eq!(parse("/daemon start"), Some(Ok(DaemonCommand::Start)));
        assert_eq!(parse("/daemon stop"), Some(Ok(DaemonCommand::Stop)));
        assert_eq!(
            parse("  /daemon UPGRADE "),
            Some(Ok(DaemonCommand::Upgrade))
        );
    }

    #[test]
    fn rejects_unknown_arguments_without_swallowing_other_prompts() {
        assert!(matches!(parse("/daemon frobnicate"), Some(Err(_))));
        // Not a /daemon command: must fall through to the model.
        assert_eq!(parse("/daemonize the thing"), None);
        assert_eq!(parse("restart the daemon"), None);
        assert_eq!(parse("/webapp"), None);
    }
}
