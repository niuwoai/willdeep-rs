//! 主 Agent 给自己起的后台子 Agent 发消息、停掉它。
//!
//! 合同见 Xedit `docs/BACKGROUND_TASK_CONTRACT.md`「第三批 · C·二」。归属按本目录
//! 起过的名单校验，名单外（别的会话、编造的）id 一律报找不到，不透露它是否存在。

use super::*;
use crate::tools::MAX_AGENT_MESSAGE_CHARS;

impl SubagentCatalog {
    /// 投递到子 Agent 的指令收件箱，下一个回合边界生效。成功时返回解析出的
    /// 子 Agent id。
    pub(crate) fn send_agent_message(
        &self,
        agent_ref: &str,
        message: &str,
    ) -> Result<uuid::Uuid, String> {
        let count = message.chars().count();
        if count > MAX_AGENT_MESSAGE_CHARS {
            return Err(format!(
                "message is {count} characters; the limit is {MAX_AGENT_MESSAGE_CHARS} and messages are never truncated. Shorten it and send again."
            ));
        }
        if message.trim().is_empty() {
            return Err("message is empty".to_owned());
        }
        let agent_id = self.running_owned_agent(agent_ref)?;
        if !self.background.instruct_agent(agent_id, message.to_owned()) {
            return Err(finished_error(agent_ref, None));
        }
        Ok(agent_id)
    }

    /// 停止运行中的后台子 Agent；它的报告照常以 `status: killed` 投回。
    pub(crate) fn stop_agent(&self, agent_ref: &str) -> Result<uuid::Uuid, String> {
        let agent_id = self.running_owned_agent(agent_ref)?;
        if !self.background.kill_agent(agent_id) {
            return Err(finished_error(agent_ref, None));
        }
        Ok(agent_id)
    }

    fn running_owned_agent(&self, agent_ref: &str) -> Result<uuid::Uuid, String> {
        let agent_ref = agent_ref.trim();
        let not_found = || {
            format!(
                "background agent not found: {agent_ref}. Only background subagents started by this session can be addressed."
            )
        };
        let agent_id = match uuid::Uuid::parse_str(agent_ref) {
            Ok(id) => id,
            Err(_) if agent_ref.starts_with("agent_") => self
                .background
                .agent_for_task(agent_ref)
                .ok_or_else(not_found)?,
            Err(_) => return Err(not_found()),
        };
        if !self
            .owned_background_agents
            .lock()
            .expect("owned background agents")
            .contains(&agent_id)
        {
            return Err(not_found());
        }
        match self.background.agent_status(agent_id) {
            None => Err(not_found()),
            Some(BackgroundTaskStatus::Running) => Ok(agent_id),
            Some(status) => Err(finished_error(agent_ref, Some(&status))),
        }
    }
}

fn finished_error(agent_ref: &str, status: Option<&BackgroundTaskStatus>) -> String {
    let status = status
        .and_then(|status| serde_json::to_value(status).ok())
        .and_then(|value| value.as_str().map(str::to_owned))
        .map(|status| format!(" (status: {status})"))
        .unwrap_or_default();
    format!(
        "background agent {agent_ref} has already finished{status}; its report has been or will be delivered"
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::subagent::builtin_profiles;
    use crate::subagent::test_support::ReportProvider;

    fn catalog() -> (SubagentCatalog, Arc<BackgroundTaskRegistry>) {
        let root = std::env::temp_dir().join(format!("willdeep-control-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let background = Arc::new(BackgroundTaskRegistry::default());
        let catalog = SubagentCatalog::new(
            &root,
            builtin_profiles(Arc::new(ReportProvider)),
            background.clone(),
        );
        (catalog, background)
    }

    /// 在注册表里挂一个永远跑着的后台子 Agent，模拟本会话 spawn 出来的那个。
    fn hang_agent(
        catalog: &SubagentCatalog,
        background: &BackgroundTaskRegistry,
        owned: bool,
    ) -> (uuid::Uuid, Arc<AgentInstructionInbox>, String) {
        let agent_id = uuid::Uuid::new_v4();
        let inbox = Arc::new(AgentInstructionInbox::default());
        let handle = background.start_retriable_with_lifecycle(
            agent_id,
            BackgroundTaskKind::Subagent,
            "child".to_owned(),
            inbox.clone(),
            || async { std::future::pending::<TaskResult>().await },
            |_| async {},
        );
        if owned {
            catalog
                .owned_background_agents
                .lock()
                .unwrap()
                .insert(agent_id);
        }
        (agent_id, inbox, handle)
    }

    #[tokio::test]
    async fn messages_reach_only_this_sessions_running_children() {
        let (catalog, background) = catalog();
        let (agent_id, inbox, handle) = hang_agent(&catalog, &background, true);
        assert_eq!(
            catalog.send_agent_message(&agent_id.to_string(), "also check docs/"),
            Ok(agent_id)
        );
        assert_eq!(
            catalog.send_agent_message(&handle, "and the changelog"),
            Ok(agent_id)
        );
        assert_eq!(
            inbox
                .drain()
                .iter()
                .map(crate::AgentInstruction::text)
                .collect::<Vec<_>>(),
            vec!["also check docs/", "and the changelog"]
        );

        // 别的会话起的子 Agent：同一个注册表里跑着，但不在本目录名单上。
        let (foreign, foreign_inbox, _) = hang_agent(&catalog, &background, false);
        let error = catalog
            .send_agent_message(&foreign.to_string(), "hijack")
            .unwrap_err();
        assert!(error.starts_with("background agent not found"), "{error}");
        assert!(catalog.stop_agent(&foreign.to_string()).is_err());
        assert!(foreign_inbox.drain().is_empty());
        let unknown = uuid::Uuid::new_v4().to_string();
        assert!(
            catalog
                .send_agent_message(&unknown, "x")
                .unwrap_err()
                .starts_with("background agent not found")
        );
    }

    #[tokio::test]
    async fn an_over_length_message_is_rejected_not_truncated() {
        let (catalog, background) = catalog();
        let (agent_id, inbox, _) = hang_agent(&catalog, &background, true);
        let exact = "字".repeat(MAX_AGENT_MESSAGE_CHARS);
        assert_eq!(
            catalog.send_agent_message(&agent_id.to_string(), &exact),
            Ok(agent_id)
        );
        let error = catalog
            .send_agent_message(&agent_id.to_string(), &format!("{exact}!"))
            .unwrap_err();
        assert!(error.contains("4001 characters") && error.contains("never truncated"));
        assert_eq!(inbox.drain(), vec![crate::AgentInstruction::Parent(exact)]);
    }

    #[tokio::test]
    async fn stopping_kills_the_child_and_a_finished_child_says_so() {
        let (catalog, background) = catalog();
        let mut events = background.subscribe();
        let (agent_id, _, _) = hang_agent(&catalog, &background, true);
        assert_eq!(catalog.stop_agent(&agent_id.to_string()), Ok(agent_id));
        let report = events.recv().await.unwrap();
        assert_eq!(report.snapshot.status, BackgroundTaskStatus::Killed);
        let error = catalog.stop_agent(&agent_id.to_string()).unwrap_err();
        assert!(
            error.contains("already finished (status: killed)"),
            "{error}"
        );
        assert!(
            catalog
                .send_agent_message(&agent_id.to_string(), "late")
                .is_err()
        );
    }
}
