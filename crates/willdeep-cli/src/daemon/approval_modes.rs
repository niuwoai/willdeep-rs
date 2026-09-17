//! 正在跑的任务各自持有的审批档位句柄。
//!
//! 档位切换要作用到「这一轮」，而这一轮的 Harness 已经在任务开始时建好了。
//! 任务开始时把共享句柄登记在这里，会话切档时按会话找到它们逐个改掉；任务
//! 结束时注销。句柄只活在内存里——Runtime 重启后任务本来就不在了。

use std::collections::HashMap;
use std::sync::Mutex;

use willdeep_core::SharedApprovalMode;

use super::WorkspaceAccess;

#[derive(Default)]
pub(super) struct LiveApprovalModes {
    tasks: Mutex<HashMap<uuid::Uuid, LiveTask>>,
}

struct LiveTask {
    session_id: Option<uuid::Uuid>,
    workspace_access: WorkspaceAccess,
    handle: SharedApprovalMode,
}

impl LiveApprovalModes {
    /// `workspace_access` 是工作区策略本身，不是合成后的档位：之后会话切档时
    /// 还要拿它当上限。
    pub(super) fn register(
        &self,
        task_id: uuid::Uuid,
        session_id: Option<uuid::Uuid>,
        workspace_access: WorkspaceAccess,
        effective: WorkspaceAccess,
    ) -> SharedApprovalMode {
        let handle = SharedApprovalMode::new(effective.approval_mode());
        if let Ok(mut tasks) = self.tasks.lock() {
            tasks.insert(
                task_id,
                LiveTask {
                    session_id,
                    workspace_access,
                    handle: handle.clone(),
                },
            );
        }
        handle
    }

    pub(super) fn remove(&self, task_id: uuid::Uuid) {
        if let Ok(mut tasks) = self.tasks.lock() {
            tasks.remove(&task_id);
        }
    }

    /// 把会话里正在跑的任务切到新档位，返回改动的任务数。每个任务仍受自己
    /// 工作区策略的上限约束。
    pub(super) fn update_session(&self, session_id: uuid::Uuid, mode: WorkspaceAccess) -> usize {
        let Ok(tasks) = self.tasks.lock() else {
            return 0;
        };
        tasks
            .values()
            .filter(|task| task.session_id == Some(session_id))
            .map(|task| {
                let effective = task.workspace_access.with_session_override(Some(mode));
                task.handle.set(effective.approval_mode());
            })
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use willdeep_core::ApprovalMode;

    #[test]
    fn session_switch_reaches_only_its_own_running_tasks_and_respects_read_only() {
        let live = LiveApprovalModes::default();
        let session = uuid::Uuid::new_v4();
        let other = uuid::Uuid::new_v4();
        let mine = live.register(
            uuid::Uuid::new_v4(),
            Some(session),
            WorkspaceAccess::Smart,
            WorkspaceAccess::Smart,
        );
        let capped = live.register(
            uuid::Uuid::new_v4(),
            Some(session),
            WorkspaceAccess::ReadOnly,
            WorkspaceAccess::ReadOnly,
        );
        let theirs = live.register(
            uuid::Uuid::new_v4(),
            Some(other),
            WorkspaceAccess::Smart,
            WorkspaceAccess::Smart,
        );

        assert_eq!(live.update_session(session, WorkspaceAccess::FullAccess), 2);
        assert_eq!(mine.get(), ApprovalMode::FullAccess);
        assert_eq!(capped.get(), ApprovalMode::ReadOnly);
        assert_eq!(theirs.get(), ApprovalMode::Smart);
    }

    #[test]
    fn removed_tasks_are_no_longer_switched() {
        let live = LiveApprovalModes::default();
        let session = uuid::Uuid::new_v4();
        let task = uuid::Uuid::new_v4();
        let handle = live.register(
            task,
            Some(session),
            WorkspaceAccess::Smart,
            WorkspaceAccess::Smart,
        );
        live.remove(task);
        assert_eq!(live.update_session(session, WorkspaceAccess::Strict), 0);
        assert_eq!(handle.get(), ApprovalMode::Smart);
    }
}
