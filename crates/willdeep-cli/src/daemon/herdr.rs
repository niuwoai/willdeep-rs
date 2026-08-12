use super::RuntimeTaskStatus;

use std::ffi::OsString;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HerdrState {
    Idle,
    Working,
    Blocked,
}

impl HerdrState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Clone)]
pub(super) struct HerdrReporter {
    binary: OsString,
    pane_id: OsString,
    last_state: Arc<Mutex<Option<HerdrState>>>,
}

impl HerdrReporter {
    pub(super) fn detect() -> Option<Self> {
        let enabled = std::env::var_os("HERDR_ENV").is_some_and(|value| value == "1");
        let pane_id = std::env::var_os("HERDR_PANE_ID").filter(|value| !value.is_empty())?;
        enabled.then(|| Self {
            binary: std::env::var_os("WILLDEEP_HERDR_BIN")
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| OsString::from("herdr")),
            pane_id,
            last_state: Arc::new(Mutex::new(None)),
        })
    }

    pub(super) fn report(&self, statuses: impl Iterator<Item = RuntimeTaskStatus>) {
        let state = aggregate_state(statuses);
        let Ok(mut last_state) = self.last_state.lock() else {
            return;
        };
        if *last_state == Some(state) {
            return;
        }
        *last_state = Some(state);
        drop(last_state);

        let binary = self.binary.clone();
        let pane_id = self.pane_id.clone();
        tokio::spawn(async move {
            let _ = tokio::process::Command::new(binary)
                .arg("pane")
                .arg("report-agent")
                .arg(pane_id)
                .arg("--source")
                .arg("willdeep:runtime")
                .arg("--agent")
                .arg("willdeep")
                .arg("--state")
                .arg(state.as_str())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        });
    }
}

fn aggregate_state(statuses: impl Iterator<Item = RuntimeTaskStatus>) -> HerdrState {
    let mut state = HerdrState::Idle;
    for status in statuses {
        if matches!(
            status,
            RuntimeTaskStatus::WaitingApproval
                | RuntimeTaskStatus::WaitingAnswer
                | RuntimeTaskStatus::Failed
                | RuntimeTaskStatus::Interrupted
        ) {
            return HerdrState::Blocked;
        }
        if matches!(
            status,
            RuntimeTaskStatus::Queued | RuntimeTaskStatus::Running | RuntimeTaskStatus::Cancelling
        ) {
            state = HerdrState::Working;
        }
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregates_runtime_states_by_attention_priority() {
        assert_eq!(aggregate_state([].into_iter()), HerdrState::Idle);
        assert_eq!(
            aggregate_state(
                [RuntimeTaskStatus::Completed, RuntimeTaskStatus::Cancelled].into_iter()
            ),
            HerdrState::Idle
        );
        assert_eq!(
            aggregate_state([RuntimeTaskStatus::Completed, RuntimeTaskStatus::Running].into_iter()),
            HerdrState::Working
        );
        assert_eq!(
            aggregate_state(
                [
                    RuntimeTaskStatus::Running,
                    RuntimeTaskStatus::WaitingApproval,
                ]
                .into_iter()
            ),
            HerdrState::Blocked
        );
        assert_eq!(
            aggregate_state([RuntimeTaskStatus::Failed].into_iter()),
            HerdrState::Blocked
        );
    }
}
