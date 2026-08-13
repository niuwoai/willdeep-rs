//! 长程自主执行的续推契约（long-horizon.v1 RA1）。
//!
//! 设计文档：`docs/LONG_HORIZON_AUTONOMY.md`（canonical 在 Xedit 仓库）。
//!
//! 核心立场：目标未达 + 预算未尽 = 无条件注入 continuation。模型「不调工具就算完成」
//! 只是一个**候选**停止点，不是终态；只有模型显式声明完成、或预算耗尽，才真的停。
//! 本模块只负责判定与话术，不碰 provider、不碰持久化——便于纯逻辑单测。

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 模型声明目标达成的标记。宿主只认这一种显式收口方式。
pub const GOAL_COMPLETE_MARKER: &str = "<goal-status>complete</goal-status>";

/// 退避阶梯：连续无进展轮次达到该值前，只做引导。
const GUIDANCE_ROUNDS: u32 = 2;
/// 连续无进展轮次达到该值前，做只读对账；之后进入退避档。
const RECONCILE_ROUNDS: u32 = 4;

/// 默认 wall-clock 预算：4 小时一段（设计文档 §7 假设 1）。
pub const DEFAULT_WALL_CLOCK_BUDGET: Duration = Duration::from_secs(4 * 60 * 60);
/// 默认续推次数上限。这是防失控的兜底，不是主要闸门——主要闸门是 wall-clock。
pub const DEFAULT_MAX_CONTINUATIONS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GoalBudget {
    pub wall_clock: Option<Duration>,
    pub max_continuations: usize,
}

impl Default for GoalBudget {
    fn default() -> Self {
        Self {
            wall_clock: Some(DEFAULT_WALL_CLOCK_BUDGET),
            max_continuations: DEFAULT_MAX_CONTINUATIONS,
        }
    }
}

/// 退避阶梯的档位。档位只改变 steering 话术与约束，不改变「继续」这个结论。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContinuationRung {
    /// 1-2 轮无进展：引导模型指名下一个动作。
    Guidance,
    /// 3-4 轮无进展：只读对账，要求用实际状态核对而不是继续猜。
    Reconcile,
    /// 5+ 轮无进展：退避，要求先确认在等什么外部条件。
    Backoff,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoftStopReason {
    WallClock,
    Continuations,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContinuationDecision {
    /// 模型显式声明完成，正常收口。
    Complete,
    /// 目标未达且预算未尽：注入 steering 继续干。
    Continue {
        steering: String,
        rung: ContinuationRung,
    },
    /// 预算耗尽：注入收尾引导，产出状态快照后有序停止。
    SoftStop {
        steering: String,
        reason: SoftStopReason,
    },
}

/// 判定所需的一轮观察结果。
#[derive(Clone, Copy, Debug)]
pub struct RoundObservation {
    /// 本轮（自上次判定以来）是否有工具成功执行。
    pub tools_executed: usize,
    /// 是否仍有后台任务/子 Agent 存活——「在等一个长 CI」不算卡死。
    pub background_active: bool,
}

impl RoundObservation {
    pub fn made_progress(&self) -> bool {
        self.tools_executed > 0 || self.background_active
    }
}

struct ActiveGoal {
    statement: String,
    budget: GoalBudget,
    started: Instant,
    continuations: usize,
    consecutive_no_progress: u32,
    wrap_up_injected: bool,
}

/// 跨 turn 共享的 Goal 续推句柄。
///
/// 沿用 [`crate::AgentInstructionInbox`] 的既有模式：Agent 在 build 时持有 `Arc`，
/// 前端（TUI `/goal`、Web、daemon）在运行期改写内部状态，无需重建 Agent。
#[derive(Default)]
pub struct GoalContinuation {
    state: Mutex<Option<ActiveGoal>>,
}

impl GoalContinuation {
    pub fn new() -> Self {
        Self::default()
    }

    /// 激活或替换目标。语句为空视为清除。
    pub fn activate(&self, statement: impl Into<String>, budget: GoalBudget) {
        let statement = statement.into().trim().to_owned();
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if statement.is_empty() {
            *state = None;
            return;
        }
        // 同一目标重复激活不重置计时，避免每条消息都把预算刷新成满格。
        if let Some(active) = state.as_mut()
            && active.statement == statement
        {
            active.budget = budget;
            return;
        }
        *state = Some(ActiveGoal {
            statement,
            budget,
            started: Instant::now(),
            continuations: 0,
            consecutive_no_progress: 0,
            wrap_up_injected: false,
        });
    }

    pub fn clear(&self) {
        if let Ok(mut state) = self.state.lock() {
            *state = None;
        }
    }

    pub fn is_active(&self) -> bool {
        self.state
            .lock()
            .map(|state| state.is_some())
            .unwrap_or(false)
    }

    pub fn statement(&self) -> Option<String> {
        self.state
            .lock()
            .ok()
            .and_then(|state| state.as_ref().map(|goal| goal.statement.clone()))
    }

    /// 已经注入过收尾引导——此后模型的第一次自然收口应当被接受。
    pub fn wrap_up_pending(&self) -> bool {
        self.state
            .lock()
            .map(|state| {
                state
                    .as_ref()
                    .map(|goal| goal.wrap_up_injected)
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }

    /// 对一次「模型没有调用工具」的候选停止点做判定。
    ///
    /// 返回 `None` 表示当前没有激活的目标，调用方按原有逻辑正常收口。
    pub fn evaluate(
        &self,
        reply: &str,
        observation: RoundObservation,
    ) -> Option<ContinuationDecision> {
        let Ok(mut guard) = self.state.lock() else {
            return None;
        };
        let goal = guard.as_mut()?;

        // 收尾引导已注入过：这一轮无论说什么都收口，不再无限追问。
        if goal.wrap_up_injected {
            let decision = ContinuationDecision::Complete;
            *guard = None;
            return Some(decision);
        }

        if declares_complete(reply) {
            *guard = None;
            return Some(ContinuationDecision::Complete);
        }

        let elapsed = goal.started.elapsed();
        if let Some(limit) = goal.budget.wall_clock
            && elapsed >= limit
        {
            goal.wrap_up_injected = true;
            return Some(ContinuationDecision::SoftStop {
                steering: wrap_up_steering(&goal.statement, SoftStopReason::WallClock, elapsed),
                reason: SoftStopReason::WallClock,
            });
        }
        if goal.continuations >= goal.budget.max_continuations {
            goal.wrap_up_injected = true;
            return Some(ContinuationDecision::SoftStop {
                steering: wrap_up_steering(&goal.statement, SoftStopReason::Continuations, elapsed),
                reason: SoftStopReason::Continuations,
            });
        }

        if observation.made_progress() {
            goal.consecutive_no_progress = 0;
        } else {
            goal.consecutive_no_progress = goal.consecutive_no_progress.saturating_add(1);
        }
        goal.continuations = goal.continuations.saturating_add(1);

        let rung = rung_for(goal.consecutive_no_progress);
        let steering = continuation_steering(
            &goal.statement,
            rung,
            elapsed,
            goal.continuations,
            goal.budget,
            observation,
        );
        Some(ContinuationDecision::Continue { steering, rung })
    }
}

fn rung_for(consecutive_no_progress: u32) -> ContinuationRung {
    if consecutive_no_progress == 0 || consecutive_no_progress <= GUIDANCE_ROUNDS {
        ContinuationRung::Guidance
    } else if consecutive_no_progress <= RECONCILE_ROUNDS {
        ContinuationRung::Reconcile
    } else {
        ContinuationRung::Backoff
    }
}

fn declares_complete(reply: &str) -> bool {
    let normalized = reply.to_ascii_lowercase();
    normalized.contains(GOAL_COMPLETE_MARKER)
}

fn format_elapsed(elapsed: Duration) -> String {
    let total = elapsed.as_secs();
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else {
        format!("{minutes}m")
    }
}

fn format_remaining(budget: GoalBudget, elapsed: Duration) -> String {
    match budget.wall_clock {
        Some(limit) => format_elapsed(limit.saturating_sub(elapsed)),
        None => "unbounded".to_owned(),
    }
}

/// 续推 steering 的四段内容契约（设计文档 §2.2）：
/// 目标 → 态势 → 上一轮判定 → 动作要求。顺序固定，便于模型定位。
fn continuation_steering(
    statement: &str,
    rung: ContinuationRung,
    elapsed: Duration,
    continuations: usize,
    budget: GoalBudget,
    observation: RoundObservation,
) -> String {
    let mut steering = String::new();
    steering.push_str(
        "[goal-continuation] This is an automated harness message, not a user reply.\n\n",
    );

    steering.push_str(&format!("1. GOAL (still active):\n{statement}\n\n"));

    steering.push_str(&format!(
        "2. SITUATION: elapsed {}, continuation {} of {}, wall-clock budget remaining {}.\n\n",
        format_elapsed(elapsed),
        continuations,
        budget.max_continuations,
        format_remaining(budget, elapsed),
    ));

    steering.push_str("3. WHY YOU ARE SEEING THIS: you produced a reply without calling any tool, but the goal has not been declared complete. ");
    if observation.made_progress() {
        steering.push_str("The previous round did make progress.\n");
    } else {
        steering.push_str(
            "The previous round produced no tool activity and no live background work.\n",
        );
    }
    match rung {
        ContinuationRung::Guidance => {}
        ContinuationRung::Reconcile => {
            steering.push_str(
                "Several rounds have passed without progress. Before doing anything else, reconcile your assumptions against reality: read the actual files, run `git status`/`git diff`, and check whether the work you believe is done actually exists on disk.\n",
            );
        }
        ContinuationRung::Backoff => {
            steering.push_str(
                "Many rounds have passed without progress. State explicitly what external condition you are waiting on (a build, a test run, a background task) and either check it directly or take a different concrete step. Do not repeat the previous approach unchanged.\n",
            );
        }
    }
    steering.push('\n');

    steering.push_str(&format!(
        "4. WHAT TO DO NOW: name the single next concrete action that advances the goal and execute it in this turn. \
Do NOT summarize work already done. Do NOT ask whether to continue — no operator is waiting to answer. \
This instruction supersedes the general guidance about stopping once enough evidence exists: while a goal is active, stopping requires the goal to be met. \
If the goal is genuinely and fully achieved, reply with the exact marker {GOAL_COMPLETE_MARKER} followed by a short summary of what was delivered and how it was verified.\n"
    ));

    steering
}

/// 预算耗尽后的收尾引导（设计文档 §3.2）：软停不是失败，是有序交接。
fn wrap_up_steering(statement: &str, reason: SoftStopReason, elapsed: Duration) -> String {
    let cause = match reason {
        SoftStopReason::WallClock => "the wall-clock budget for this goal segment is exhausted",
        SoftStopReason::Continuations => {
            "the continuation budget for this goal segment is exhausted"
        }
    };
    format!(
        "[goal-budget-limited] This is an automated harness message, not a user reply.\n\n\
GOAL: {statement}\n\n\
The goal was NOT completed: {cause} (elapsed {}). This is an orderly wrap-up, not a failure.\n\n\
Stop all new substantive work now. Do not start new edits, new files, or new background tasks. \
In this turn produce a handover snapshot with exactly these sections:\n\
- STATE: current git branch, uncommitted files, current version\n\
- DONE: what was actually completed and how it was verified\n\
- REMAINING: what still has to happen, in the order it should happen\n\
- BLOCKERS: anything that stopped progress, with the specific error or condition\n\
- NEXT: the single action whoever resumes this goal should take first\n",
        format_elapsed(elapsed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progressed() -> RoundObservation {
        RoundObservation {
            tools_executed: 3,
            background_active: false,
        }
    }

    fn stalled() -> RoundObservation {
        RoundObservation {
            tools_executed: 0,
            background_active: false,
        }
    }

    fn goal_with(budget: GoalBudget) -> GoalContinuation {
        let continuation = GoalContinuation::new();
        continuation.activate("ship the release", budget);
        continuation
    }

    #[test]
    fn no_active_goal_leaves_stopping_untouched() {
        let continuation = GoalContinuation::new();
        assert!(continuation.evaluate("all done", progressed()).is_none());
    }

    #[test]
    fn plain_reply_is_refused_and_continued() {
        let continuation = goal_with(GoalBudget::default());
        let decision = continuation
            .evaluate("I have finished the first part.", progressed())
            .expect("goal active");
        let ContinuationDecision::Continue { steering, rung } = decision else {
            panic!("expected continue, got {decision:?}");
        };
        assert_eq!(rung, ContinuationRung::Guidance);
        assert!(steering.contains("ship the release"));
        assert!(steering.contains("GOAL (still active)"));
        assert!(steering.contains("WHAT TO DO NOW"));
        assert!(continuation.is_active());
    }

    #[test]
    fn explicit_marker_completes_and_clears() {
        let continuation = goal_with(GoalBudget::default());
        let decision = continuation
            .evaluate(
                "Everything is verified. <goal-status>complete</goal-status> Shipped rc7.",
                progressed(),
            )
            .expect("goal active");
        assert_eq!(decision, ContinuationDecision::Complete);
        assert!(!continuation.is_active());
    }

    #[test]
    fn background_work_counts_as_progress_so_waiting_is_not_a_stall() {
        let continuation = goal_with(GoalBudget::default());
        let waiting = RoundObservation {
            tools_executed: 0,
            background_active: true,
        };
        for _ in 0..6 {
            let decision = continuation.evaluate("waiting for CI", waiting).unwrap();
            let ContinuationDecision::Continue { rung, .. } = decision else {
                panic!("expected continue while waiting on background work");
            };
            assert_eq!(rung, ContinuationRung::Guidance);
        }
    }

    #[test]
    fn stalling_escalates_through_the_ladder() {
        let continuation = goal_with(GoalBudget::default());
        let rungs = (0..6)
            .map(
                |_| match continuation.evaluate("still thinking", stalled()) {
                    Some(ContinuationDecision::Continue { rung, .. }) => rung,
                    other => panic!("expected continue, got {other:?}"),
                },
            )
            .collect::<Vec<_>>();
        assert_eq!(
            rungs,
            vec![
                ContinuationRung::Guidance,
                ContinuationRung::Guidance,
                ContinuationRung::Reconcile,
                ContinuationRung::Reconcile,
                ContinuationRung::Backoff,
                ContinuationRung::Backoff,
            ]
        );
    }

    #[test]
    fn progress_resets_the_ladder() {
        let continuation = goal_with(GoalBudget::default());
        for _ in 0..3 {
            continuation.evaluate("thinking", stalled());
        }
        let decision = continuation
            .evaluate("did the thing", progressed())
            .unwrap();
        let ContinuationDecision::Continue { rung, .. } = decision else {
            panic!("expected continue");
        };
        assert_eq!(rung, ContinuationRung::Guidance);
    }

    #[test]
    fn continuation_budget_exhaustion_soft_stops_with_handover() {
        let continuation = goal_with(GoalBudget {
            wall_clock: None,
            max_continuations: 2,
        });
        continuation.evaluate("one", progressed());
        continuation.evaluate("two", progressed());
        let decision = continuation.evaluate("three", progressed()).unwrap();
        let ContinuationDecision::SoftStop { steering, reason } = decision else {
            panic!("expected soft stop, got {decision:?}");
        };
        assert_eq!(reason, SoftStopReason::Continuations);
        assert!(steering.contains("BLOCKERS"));
        assert!(steering.contains("REMAINING"));
        assert!(continuation.wrap_up_pending());
    }

    #[test]
    fn wall_clock_exhaustion_soft_stops() {
        let continuation = goal_with(GoalBudget {
            wall_clock: Some(Duration::ZERO),
            max_continuations: 100,
        });
        let decision = continuation.evaluate("anything", progressed()).unwrap();
        let ContinuationDecision::SoftStop { reason, .. } = decision else {
            panic!("expected soft stop");
        };
        assert_eq!(reason, SoftStopReason::WallClock);
    }

    #[test]
    fn wrap_up_reply_is_accepted_without_further_nagging() {
        let continuation = goal_with(GoalBudget {
            wall_clock: Some(Duration::ZERO),
            max_continuations: 100,
        });
        assert!(matches!(
            continuation.evaluate("anything", progressed()),
            Some(ContinuationDecision::SoftStop { .. })
        ));
        assert_eq!(
            continuation.evaluate("STATE: branch main …", stalled()),
            Some(ContinuationDecision::Complete)
        );
        assert!(!continuation.is_active());
    }

    #[test]
    fn reactivating_the_same_goal_does_not_refresh_the_budget() {
        let continuation = goal_with(GoalBudget {
            wall_clock: None,
            max_continuations: 2,
        });
        continuation.evaluate("one", progressed());
        continuation.activate(
            "ship the release",
            GoalBudget {
                wall_clock: None,
                max_continuations: 2,
            },
        );
        continuation.evaluate("two", progressed());
        assert!(matches!(
            continuation.evaluate("three", progressed()),
            Some(ContinuationDecision::SoftStop { .. })
        ));
    }

    #[test]
    fn activating_empty_statement_clears_the_goal() {
        let continuation = goal_with(GoalBudget::default());
        continuation.activate("   ", GoalBudget::default());
        assert!(!continuation.is_active());
    }
}
