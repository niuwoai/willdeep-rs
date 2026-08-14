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
        let _ = ui.send(UiMessage::RuntimeNotice(message));
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
