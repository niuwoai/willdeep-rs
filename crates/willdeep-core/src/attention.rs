use serde::{Deserialize, Serialize};

use crate::{BackgroundTaskKind, BackgroundTaskSnapshot, BackgroundTaskStatus};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeStatus {
    Idle,
    Working,
    Blocked,
    WaitingApproval,
    WaitingAnswer,
    Failed,
    Done,
    Cancelled,
    Unknown,
}

impl RuntimeStatus {
    pub fn priority(self) -> u8 {
        match self {
            Self::WaitingApproval => 90,
            Self::WaitingAnswer => 85,
            Self::Blocked => 80,
            Self::Failed => 70,
            Self::Working => 50,
            Self::Done => 30,
            Self::Cancelled => 20,
            Self::Idle => 10,
            Self::Unknown => 0,
        }
    }

    pub fn section(self) -> Option<AttentionSection> {
        match self {
            Self::WaitingApproval | Self::WaitingAnswer | Self::Blocked | Self::Failed => {
                Some(AttentionSection::NeedsYou)
            }
            Self::Working => Some(AttentionSection::Working),
            Self::Done | Self::Cancelled => Some(AttentionSection::Recent),
            Self::Idle | Self::Unknown => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionSection {
    NeedsYou,
    Working,
    Recent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionSource {
    Approval,
    Question,
    BackgroundShell,
    Subagent,
    Worktree,
    DiffReview,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeScopeKind {
    Agent,
    Session,
    Workspace,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusRollup {
    pub kind: RuntimeScopeKind,
    pub id: String,
    pub own_status: RuntimeStatus,
    pub status: RuntimeStatus,
    pub children: Vec<StatusRollup>,
}

impl StatusRollup {
    pub fn leaf(kind: RuntimeScopeKind, id: impl Into<String>, status: RuntimeStatus) -> Self {
        Self {
            kind,
            id: id.into(),
            own_status: status,
            status,
            children: Vec::new(),
        }
    }

    pub fn group(
        kind: RuntimeScopeKind,
        id: impl Into<String>,
        own_status: RuntimeStatus,
        children: Vec<Self>,
    ) -> Self {
        let status = rollup_status(
            std::iter::once(own_status).chain(children.iter().map(|child| child.status)),
        );
        Self {
            kind,
            id: id.into(),
            own_status,
            status,
            children,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionItem {
    pub id: String,
    pub source: AttentionSource,
    pub title: String,
    pub detail: String,
    pub status: RuntimeStatus,
    pub elapsed_millis: Option<u64>,
}

impl AttentionItem {
    pub fn approval(description: impl Into<String>) -> Self {
        Self {
            id: "approval:current".to_owned(),
            source: AttentionSource::Approval,
            title: "Approval required".to_owned(),
            detail: description.into(),
            status: RuntimeStatus::WaitingApproval,
            elapsed_millis: None,
        }
    }

    pub fn question(question: impl Into<String>) -> Self {
        Self {
            id: "question:current".to_owned(),
            source: AttentionSource::Question,
            title: "Answer required".to_owned(),
            detail: question.into(),
            status: RuntimeStatus::WaitingAnswer,
            elapsed_millis: None,
        }
    }

    pub fn from_background(task: &BackgroundTaskSnapshot) -> Self {
        let source = match task.kind {
            BackgroundTaskKind::Shell => AttentionSource::BackgroundShell,
            BackgroundTaskKind::Subagent => AttentionSource::Subagent,
        };
        let status = match task.status {
            BackgroundTaskStatus::Running => RuntimeStatus::Working,
            BackgroundTaskStatus::Blocked => RuntimeStatus::Blocked,
            BackgroundTaskStatus::Completed => RuntimeStatus::Done,
            BackgroundTaskStatus::Failed
            | BackgroundTaskStatus::TimedOut
            | BackgroundTaskStatus::LaunchFailed => RuntimeStatus::Failed,
            BackgroundTaskStatus::Killed => RuntimeStatus::Cancelled,
        };
        Self {
            id: task.id.clone(),
            source,
            title: task.label.clone(),
            detail: format!("{:?}", task.status),
            status,
            elapsed_millis: Some(task.elapsed_millis),
        }
    }
}

pub fn sort_attention_items(items: &mut [AttentionItem]) {
    items.sort_by(|left, right| {
        right
            .status
            .priority()
            .cmp(&left.status.priority())
            .then_with(|| right.elapsed_millis.cmp(&left.elapsed_millis))
            .then_with(|| left.id.cmp(&right.id))
    });
}

pub fn rollup_status(statuses: impl IntoIterator<Item = RuntimeStatus>) -> RuntimeStatus {
    statuses
        .into_iter()
        .max_by_key(|status| status.priority())
        .unwrap_or(RuntimeStatus::Idle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(kind: BackgroundTaskKind, status: BackgroundTaskStatus) -> BackgroundTaskSnapshot {
        BackgroundTaskSnapshot {
            id: "task_1".to_owned(),
            agent_id: None,
            kind,
            label: "Inspect repository".to_owned(),
            status,
            elapsed_millis: 1_500,
            exit_code: None,
            output_bytes: 0,
        }
    }

    #[test]
    fn maps_background_tasks_to_shared_runtime_states() {
        let running = AttentionItem::from_background(&task(
            BackgroundTaskKind::Subagent,
            BackgroundTaskStatus::Running,
        ));
        assert_eq!(running.source, AttentionSource::Subagent);
        assert_eq!(running.status, RuntimeStatus::Working);
        assert_eq!(running.status.section(), Some(AttentionSection::Working));

        let failed = AttentionItem::from_background(&task(
            BackgroundTaskKind::Shell,
            BackgroundTaskStatus::TimedOut,
        ));
        assert_eq!(failed.source, AttentionSource::BackgroundShell);
        assert_eq!(failed.status, RuntimeStatus::Failed);
        assert_eq!(failed.status.section(), Some(AttentionSection::NeedsYou));
    }

    #[test]
    fn human_gates_win_parent_status_rollups() {
        assert_eq!(
            rollup_status([
                RuntimeStatus::Working,
                RuntimeStatus::Failed,
                RuntimeStatus::WaitingApproval,
            ]),
            RuntimeStatus::WaitingApproval
        );
        assert_eq!(rollup_status([]), RuntimeStatus::Idle);
    }

    #[test]
    fn sorting_places_actionable_items_first() {
        let mut items = vec![
            AttentionItem::from_background(&task(
                BackgroundTaskKind::Shell,
                BackgroundTaskStatus::Completed,
            )),
            AttentionItem::question("Choose a target"),
            AttentionItem::approval("Run command"),
        ];
        sort_attention_items(&mut items);
        assert_eq!(items[0].status, RuntimeStatus::WaitingApproval);
        assert_eq!(items[1].status, RuntimeStatus::WaitingAnswer);
        assert_eq!(items[2].status, RuntimeStatus::Done);
    }

    #[test]
    fn rolls_agent_state_through_session_and_workspace() {
        let session = StatusRollup::group(
            RuntimeScopeKind::Session,
            "session-1",
            RuntimeStatus::Idle,
            vec![
                StatusRollup::leaf(RuntimeScopeKind::Agent, "main", RuntimeStatus::Working),
                StatusRollup::leaf(
                    RuntimeScopeKind::Agent,
                    "reviewer",
                    RuntimeStatus::WaitingApproval,
                ),
            ],
        );
        let workspace = StatusRollup::group(
            RuntimeScopeKind::Workspace,
            "workspace-1",
            RuntimeStatus::Idle,
            vec![session],
        );
        assert_eq!(workspace.status, RuntimeStatus::WaitingApproval);
        assert_eq!(workspace.children[0].status, RuntimeStatus::WaitingApproval);
    }
}
