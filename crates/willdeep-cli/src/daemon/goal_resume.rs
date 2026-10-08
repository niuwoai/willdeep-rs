//! daemon 重启后自动续推未完成的目标（long-horizon.v1 RA4）。
//!
//! 重启时动过工具的在途轮次不能安全重放，一律标成 `Interrupted`
//! （[`session_store::RESTART_INTERRUPTED_ERROR`]）。会话的目标状态（清单、
//! 预算、计数）早已随检查点落盘，缺的只是「再推一下」：这里给这样的会话排
//! 一轮续推，提示词要求先核对实际状态再接着做。
//!
//! 兜底：
//! - 只接重启造成的中断；人为取消、失败、已完成或已进入收尾的目标都不接。
//! - 目标的时间、次数、token 预算用尽的不接。
//! - 连续 [`MAX_CONSECUTIVE_RESUMES`] 轮都是自动续推、又都被重启打断，就停手：
//!   一个每次都把 daemon 弄崩的轮次不能被无限重放。
//! - `request_id` 由被打断的轮次确定性派生，同一次中断排不出第二轮。

use anyhow::Result;

use super::{ServerState, session_store};

/// 自动续推轮次的 `origin_client` 前缀，后接被打断的轮次 id。
pub(super) const ORIGIN_PREFIX: &str = "goal-resume:";
/// 连续多少轮自动续推都被重启打断就不再续推。
pub(super) const MAX_CONSECUTIVE_RESUMES: usize = 3;
/// 派生续推轮次 `request_id` 用的常量（与被打断轮次的 id 异或）。
const REQUEST_SALT: u128 = 0x676f_616c_2d72_6573_756d_652d_7261_3421;

/// 启动时跑一次；IO 都是同步的小文件读写，放进阻塞线程池。
pub(super) fn spawn(state: std::sync::Arc<ServerState>, settings: crate::config::AgentSettings) {
    tokio::task::spawn_blocking(move || {
        if let Err(error) = recover(&state, &settings) {
            let _ = state
                .events
                .append("goal.resume_error", format!("error={error:#}"));
        }
    });
}

/// 给被重启打断、目标仍在进行的会话各排一轮续推，返回排了几轮。
pub(super) fn recover(
    state: &ServerState,
    settings: &crate::config::AgentSettings,
) -> Result<usize> {
    if !settings.goal_auto_resume.unwrap_or(true) {
        return Ok(0);
    }
    let mut budget = settings.goal_budget();
    if let Some(parameters) =
        willdeep_core::runtime_parameters::RuntimeParameters::load(&state.home)
            .map_err(anyhow::Error::msg)?
    {
        budget.wall_clock = Some(std::time::Duration::from_secs(
            parameters.goal_wall_clock_minutes * 60,
        ));
        budget.max_continuations = parameters.goal_max_continuations;
        budget.max_tokens = parameters.goal_token_budget;
    }
    let mut resumed = 0;
    for candidate in state.sessions.goal_resume_candidates(ORIGIN_PREFIX)? {
        let Ok(Some(goal)) = state.sessions.core_goal_state(candidate.session_id) else {
            continue;
        };
        if let Some(reason) = skip_reason(&goal, &budget, candidate.consecutive_resumes) {
            state.events.append(
                "goal.resume_skipped",
                format!(
                    "session_id={} interrupted_turn_id={} reason={reason}",
                    candidate.session_id, candidate.interrupted_turn_id
                ),
            )?;
            continue;
        }
        let (turn, created, _) = state.sessions.enqueue_turn_observed(
            candidate.session_id,
            session_store::CreateRuntimeTurn {
                request_id: uuid::Uuid::from_u128(
                    candidate.interrupted_turn_id.as_u128() ^ REQUEST_SALT,
                ),
                prompt: willdeep_core::goal::resume_after_restart_prompt(&goal),
                attachments: Vec::new(),
                origin_client: Some(format!("{ORIGIN_PREFIX}{}", candidate.interrupted_turn_id)),
            },
        )?;
        if !created {
            continue;
        }
        state.events.append(
            "turn.queued",
            format!(
                "session_id={} agent_id={} turn_id={}",
                candidate.session_id, candidate.root_agent_id, turn.id
            ),
        )?;
        state.events.append(
            "goal.resumed",
            format!(
                "session_id={} turn_id={} interrupted_turn_id={}",
                candidate.session_id, turn.id, candidate.interrupted_turn_id
            ),
        )?;
        if let Some(feedback) = state.sessions.feedback() {
            feedback
                .clone()
                .with_session(Some(candidate.session_id))
                .record_goal(
                    willdeep_core::feedback::Signal::GoalResumed,
                    goal.open_criteria().len(),
                    u32::try_from(goal.continuations).unwrap_or(u32::MAX),
                    goal.elapsed(),
                );
        }
        // 轮次已经落盘排队；叫醒调度通道失败时，它会被下一次调度领走。
        if let Err(error) = state.tasks.schedule_session(candidate.session_id) {
            let _ = state.events.append(
                "goal.resume_warning",
                format!("session_id={} error={error:#}", candidate.session_id),
            );
        }
        resumed += 1;
    }
    Ok(resumed)
}

/// 这个目标为什么不续推；`None` 表示该续推。
fn skip_reason(
    goal: &willdeep_core::GoalState,
    budget: &willdeep_core::GoalBudget,
    consecutive_resumes: usize,
) -> Option<&'static str> {
    if goal.status != willdeep_core::GoalStatus::Active || goal.statement.trim().is_empty() {
        return Some("goal_not_active");
    }
    if goal.wrap_up_injected {
        return Some("wrapping_up");
    }
    if consecutive_resumes >= MAX_CONSECUTIVE_RESUMES {
        return Some("repeated_restarts");
    }
    if goal.continuations >= budget.max_continuations
        || budget
            .wall_clock
            .is_some_and(|limit| goal.elapsed() >= limit)
        || budget
            .max_tokens
            .is_some_and(|limit| goal.tokens_used >= limit)
    {
        return Some("budget_spent");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_reasons_cover_state_budget_and_restart_loops() {
        let budget = willdeep_core::GoalBudget {
            max_tokens: Some(100),
            ..willdeep_core::GoalBudget::default()
        };
        let goal = willdeep_core::GoalState::new("ship it");
        assert_eq!(skip_reason(&goal, &budget, 0), None);
        assert_eq!(
            skip_reason(&goal, &budget, MAX_CONSECUTIVE_RESUMES - 1),
            None
        );
        assert_eq!(
            skip_reason(&goal, &budget, MAX_CONSECUTIVE_RESUMES),
            Some("repeated_restarts")
        );
        let mut spent = goal.clone();
        spent.continuations = budget.max_continuations;
        assert_eq!(skip_reason(&spent, &budget, 0), Some("budget_spent"));
        let mut late = goal.clone();
        late.elapsed_ms = 5 * 60 * 60 * 1_000;
        assert_eq!(skip_reason(&late, &budget, 0), Some("budget_spent"));
        let mut hungry = goal.clone();
        hungry.tokens_used = 100;
        assert_eq!(skip_reason(&hungry, &budget, 0), Some("budget_spent"));
        let mut limited = goal;
        limited.status = willdeep_core::GoalStatus::BudgetLimited;
        assert_eq!(skip_reason(&limited, &budget, 0), Some("goal_not_active"));
    }
}
