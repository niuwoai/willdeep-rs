//! 正在跑的任务各自的插话收件箱。
//!
//! 用户在本轮进行中按回车，那句话不该等到轮次结束：`turn.steer` 按会话找到正在跑
//! 的任务，把话塞进它的收件箱，Agent 主循环在下一次调模型前以用户身份注入。任务
//! 开始时登记，结束时注销；注销时还剩在箱里的，就是没赶上这一轮的，交回给客户端
//! 重新排队。句柄只活在内存里——Runtime 重启后任务本来就不在了。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use willdeep_core::{AgentInstruction, AgentInstructionInbox};

#[derive(Default)]
pub(super) struct SteeringInboxes {
    tasks: Mutex<HashMap<uuid::Uuid, LiveInbox>>,
}

struct LiveInbox {
    session_id: Option<uuid::Uuid>,
    inbox: Arc<AgentInstructionInbox>,
}

impl SteeringInboxes {
    pub(super) fn register(
        &self,
        task_id: uuid::Uuid,
        session_id: Option<uuid::Uuid>,
    ) -> Arc<AgentInstructionInbox> {
        let inbox = Arc::new(AgentInstructionInbox::default());
        if let Ok(mut tasks) = self.tasks.lock() {
            tasks.insert(
                task_id,
                LiveInbox {
                    session_id,
                    inbox: inbox.clone(),
                },
            );
        }
        inbox
    }

    /// 把用户的话送进该会话正在跑的任务，返回收下它的任务；没有在途任务返回 None。
    pub(super) fn steer(&self, session_id: uuid::Uuid, message: String) -> Option<uuid::Uuid> {
        let tasks = self.tasks.lock().ok()?;
        let (task_id, live) = tasks
            .iter()
            .find(|(_, live)| live.session_id == Some(session_id))?;
        live.inbox.push_operator(message).then_some(*task_id)
    }

    /// 注销任务，交回没赶上这一轮的用户插话。
    pub(super) fn remove(&self, task_id: uuid::Uuid) -> Vec<String> {
        let Some(live) = self
            .tasks
            .lock()
            .ok()
            .and_then(|mut tasks| tasks.remove(&task_id))
        else {
            return Vec::new();
        };
        live.inbox
            .drain()
            .into_iter()
            .filter_map(|instruction| match instruction {
                AgentInstruction::Operator(text) => Some(text),
                AgentInstruction::Parent(_) => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steering_reaches_the_running_task_of_that_session_only() {
        let inboxes = SteeringInboxes::default();
        let session = uuid::Uuid::new_v4();
        let task = uuid::Uuid::new_v4();
        let inbox = inboxes.register(task, Some(session));
        inboxes.register(uuid::Uuid::new_v4(), None);

        assert_eq!(inboxes.steer(uuid::Uuid::new_v4(), "别的会话".into()), None);
        assert_eq!(inboxes.steer(session, "先别删".into()), Some(task));
        assert_eq!(
            inboxes.steer(session, "   ".into()),
            None,
            "blank steer is refused"
        );
        assert_eq!(
            inbox.drain(),
            vec![AgentInstruction::Operator("先别删".into())]
        );
    }

    #[test]
    fn removing_a_task_hands_back_only_the_operator_leftovers() {
        let inboxes = SteeringInboxes::default();
        let session = uuid::Uuid::new_v4();
        let task = uuid::Uuid::new_v4();
        let inbox = inboxes.register(task, Some(session));
        inbox.push("parent note".into());
        inboxes.steer(session, "没赶上的话".into());

        assert_eq!(inboxes.remove(task), vec!["没赶上的话".to_owned()]);
        assert_eq!(
            inboxes.steer(session, "再说一句".into()),
            None,
            "task is gone"
        );
        assert!(
            inboxes.remove(task).is_empty(),
            "second removal finds nothing"
        );
    }
}
