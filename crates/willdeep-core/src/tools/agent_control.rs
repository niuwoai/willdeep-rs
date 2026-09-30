//! `send_agent_message` / `stop_agent` 的工具定义与审计。
//!
//! 执行在 Agent 层（`agent/messaging.rs`）：只有持有子 Agent 目录的主 Agent 才
//! 能动它们。这里只放模型看到的定义，以及「不需要审批、但要留痕」的那一笔。

use super::*;

/// 只给主 Agent 的工具。子 Agent 的工具面按工种白名单构建，这份名单在构建时
/// 再剔一遍：白名单写错了也不会把派工或指挥别的 Worker 的能力交给 Worker。
pub(crate) const PARENT_ONLY_TOOLS: &[&str] = &[
    "spawn_agent",
    "send_agent_message",
    "stop_agent",
    "resume_agent",
    "list_agent_recoveries",
    "update_plan",
];

/// `update_plan`：只在有激活目标（`/goal`）时出现在主 Agent 的工具面里。
/// 执行在 Agent 层（它改的是 [`crate::GoalContinuation`]，不碰工作区）。
pub(crate) fn update_plan_definition() -> ToolDefinition {
    definition(
        "update_plan",
        "Record the goal's acceptance criteria, mark criteria done with evidence, update the step plan, or record what blocks you. \
The goal can only be declared complete when every acceptance criterion is marked done with evidence and no delegated worker is still running. \
Define concrete, checkable criteria early; mark one done only with proof (command output, test name, file path, commit). \
All fields are optional; only the fields you pass change. No approval is needed.",
        json!({"type":"object","properties":{
            "criteria":{"type":"array","maxItems":24,"items":{"type":"string"},"description":"Replace the full list of acceptance criteria. Criteria whose text is unchanged keep their done state and evidence."},
            "checklist":{"type":"array","items":{"type":"object","properties":{
                "index":{"type":"integer","minimum":1,"description":"1-based criterion number."},
                "done":{"type":"boolean"},
                "evidence":{"type":"string","description":"Required when done is true."}
            },"required":["index","done"],"additionalProperties":false}},
            "merge":{"type":"boolean","description":"true: update steps by id and append new ids; false or omitted: replace the full step plan."},
            "steps":{"type":"array","maxItems":24,"items":{"type":"object","properties":{
                "id":{"type":"string","description":"Stable step id such as s1; required when merge is true."},
                "title":{"type":"string"},
                "status":{"type":"string","enum":["pending","in_progress","done","skipped","failed"]},
                "note":{"type":"string"}
            },"required":["title"],"additionalProperties":false}},
            "blocked_reason":{"type":"string","description":"What currently blocks progress; an empty string clears it."}
        },"additionalProperties":false}),
    )
}

/// `send_agent_message` 的正文上限（字符）。超出直接报错，不截断。
pub const MAX_AGENT_MESSAGE_CHARS: usize = 4_000;

pub(super) fn definitions() -> [ToolDefinition; 2] {
    [
        definition(
            "send_agent_message",
            "Send an additional instruction to a running background subagent that this session started with spawn_agent. It is delivered at the child's next turn boundary and does not interrupt its current step. The message may be at most 4000 characters; longer messages are rejected, never truncated. No approval is needed; every call is audited.",
            json!({"type":"object","properties":{
                "agent_id":{"type":"string","description":"The agent_id returned by spawn_agent (the background_task handle is also accepted)."},
                "message":{"type":"string","description":"Instruction for the child, at most 4000 characters."}
            },"required":["agent_id","message"],"additionalProperties":false}),
        ),
        definition(
            "stop_agent",
            "Stop a running background subagent that this session started with spawn_agent. Its report is still delivered, with status killed. No approval is needed; every call is audited.",
            json!({"type":"object","properties":{
                "agent_id":{"type":"string","description":"The agent_id returned by spawn_agent (the background_task handle is also accepted)."}
            },"required":["agent_id"],"additionalProperties":false}),
        ),
    ]
}

impl ToolRegistry {
    /// 给子 Agent 发消息、停子 Agent 不弹审批，但照样写进审批审计
    /// （CLI 是 `~/.willdeep/approvals.jsonl`），与命令审批同一格式。
    pub(crate) fn audit_agent_control(&self, action: &str, detail: String) {
        self.report_approval(action, ApprovalSource::NotRequired, detail);
    }
}
