//! 长程自主执行的续推契约（long-horizon.v1 RA1）。
//!
//! 设计文档：`docs/LONG_HORIZON_AUTONOMY.md`（canonical 在 Xedit 仓库）。
//!
//! 核心立场：目标未达 + 预算未尽 = 无条件注入 continuation。模型「不调工具就算完成」
//! 只是一个**候选**停止点，不是终态；只有模型显式声明完成、或预算耗尽，才真的停。
//! 本模块只负责判定与话术，不碰 provider、不碰磁盘——便于纯逻辑单测。状态可以
//! 快照成 [`GoalState`] 交给宿主落盘，下次运行再 [`GoalContinuation::restore`] /
//! [`GoalContinuation::park`] 回来：预算与验收清单跨轮次、跨进程累计。

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

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
    /// 模型声明完成，但还有没满足的验收标准或在跑的 Worker：注入 steering
    /// 点名缺什么，继续干。
    CompletionRejected {
        steering: String,
        open_criteria: usize,
        workers_in_flight: usize,
    },
}

/// 判定所需的一轮观察结果。
#[derive(Clone, Copy, Debug, Default)]
pub struct RoundObservation {
    /// 本轮（自上次判定以来）是否有工具成功执行。
    pub tools_executed: usize,
    /// 是否仍有后台任务/子 Agent 存活——「在等一个长 CI」不算卡死。
    pub background_active: bool,
    /// 还在跑的委派 Worker 数。完成门禁要求它为 0：报告还没回来的目标不算完成。
    pub workers_in_flight: usize,
}

impl RoundObservation {
    pub fn made_progress(&self) -> bool {
        self.tools_executed > 0
    }
}

/// 目标的收尾状态。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    #[default]
    Active,
    /// 模型声明完成且通过了完成门禁。
    Complete,
    /// 预算耗尽，按交接快照收尾。状态留着，之后可以接着做。
    BudgetLimited,
}

/// 一条验收标准。标成完成必须附证据（命令输出、文件路径、测试名……），
/// 没有证据的「完成」不算完成（long-horizon.v1 §2.3）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalCriterion {
    pub text: String,
    #[serde(default)]
    pub done: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

impl GoalCriterion {
    fn satisfied(&self) -> bool {
        self.done
            && self
                .evidence
                .as_deref()
                .is_some_and(|evidence| !evidence.trim().is_empty())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    #[default]
    Pending,
    InProgress,
    Done,
    Blocked,
}

impl StepStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Done => "done",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalStep {
    pub title: String,
    #[serde(default)]
    pub status: StepStatus,
}

/// 可持久化的目标状态：随会话落盘，重启 / 换进程后接着算预算、接着对清单。
///
/// `elapsed_ms` 只累计**实际在跑**的时间：轮次之间、进程不在的时候不算。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalState {
    pub statement: String,
    #[serde(default)]
    pub criteria: Vec<GoalCriterion>,
    #[serde(default)]
    pub steps: Vec<GoalStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    #[serde(default)]
    pub status: GoalStatus,
    #[serde(default)]
    pub elapsed_ms: u64,
    #[serde(default)]
    pub continuations: usize,
    #[serde(default)]
    pub consecutive_no_progress: u32,
    #[serde(default)]
    pub wrap_up_injected: bool,
}

impl GoalState {
    pub fn new(statement: impl Into<String>) -> Self {
        Self {
            statement: statement.into(),
            ..Self::default()
        }
    }

    pub fn elapsed(&self) -> Duration {
        Duration::from_millis(self.elapsed_ms)
    }

    /// 还没满足的验收标准，带 1 起的序号。
    pub fn open_criteria(&self) -> Vec<(usize, &GoalCriterion)> {
        self.criteria
            .iter()
            .enumerate()
            .filter(|(_, criterion)| !criterion.satisfied())
            .map(|(index, criterion)| (index + 1, criterion))
            .collect()
    }

    /// 两个写者（TUI 与 daemon）各自带着一份状态回来时，同一目标的计数取
    /// 单调较大者：预算只能往前走，不能被较旧的一份拨回去。
    pub fn merged_over(mut self, previous: Option<&GoalState>) -> Self {
        if let Some(previous) = previous
            && previous.statement == self.statement
        {
            self.elapsed_ms = self.elapsed_ms.max(previous.elapsed_ms);
            self.continuations = self.continuations.max(previous.continuations);
        }
        self
    }

    /// 每轮钉在 system 消息里的目标块：压缩摘要不会碰 system 消息，所以目标、
    /// 清单与在飞 Worker 永远不会被摘要掉。
    pub fn render_pin(&self, budget: GoalBudget, workers_in_flight: usize) -> String {
        let mut out = String::from("<goal-state>\n");
        out.push_str(&format!("GOAL: {}\n", self.statement));
        out.push_str(&format!(
            "BUDGET: elapsed {}, continuation {} of {}, wall-clock remaining {}\n",
            format_elapsed(self.elapsed()),
            self.continuations,
            budget.max_continuations,
            format_remaining(budget, self.elapsed()),
        ));
        if self.criteria.is_empty() {
            out.push_str("ACCEPTANCE CRITERIA: none recorded yet. Use `update_plan` to record the concrete, checkable criteria this goal must meet.\n");
        } else {
            out.push_str("ACCEPTANCE CRITERIA:\n");
            for (index, criterion) in self.criteria.iter().enumerate() {
                let mark = if criterion.satisfied() { "x" } else { " " };
                out.push_str(&format!("  {}. [{mark}] {}", index + 1, criterion.text));
                if let Some(evidence) = criterion
                    .evidence
                    .as_deref()
                    .filter(|_| criterion.satisfied())
                {
                    out.push_str(&format!(" — evidence: {}", truncate(evidence, 160)));
                }
                out.push('\n');
            }
        }
        if !self.steps.is_empty() {
            out.push_str("PLAN:\n");
            for step in &self.steps {
                out.push_str(&format!("  - [{}] {}\n", step.status.label(), step.title));
            }
        }
        if let Some(reason) = &self.blocked_reason {
            out.push_str(&format!("BLOCKED: {reason}\n"));
        }
        if workers_in_flight > 0 {
            out.push_str(&format!(
                "DELEGATED WORKERS STILL RUNNING: {workers_in_flight}. The goal cannot be declared complete until their reports are back.\n"
            ));
        }
        out.push_str("</goal-state>");
        out
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max_chars).collect();
    cut.push('…');
    cut
}

/// `update_plan` 的参数。所有字段可选：只改给出的部分。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanUpdate {
    /// 整组替换验收标准（新定义或重新拆分时用）。已有的完成状态按文本对上号保留。
    #[serde(default)]
    pub criteria: Option<Vec<String>>,
    /// 按 1 起的序号更新验收标准的完成状态。
    #[serde(default)]
    pub checklist: Vec<ChecklistUpdate>,
    /// 整组替换步骤。
    #[serde(default)]
    pub steps: Option<Vec<GoalStep>>,
    /// 阻塞原因；传空字符串清除。
    #[serde(default)]
    pub blocked_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChecklistUpdate {
    pub index: usize,
    pub done: bool,
    #[serde(default)]
    pub evidence: Option<String>,
}

/// 单条验收标准 / 步骤 / 证据的长度上限，免得清单本身把上下文吃掉。
const MAX_PLAN_TEXT_CHARS: usize = 500;
const MAX_PLAN_ITEMS: usize = 24;

struct ActiveGoal {
    state: GoalState,
    budget: GoalBudget,
    /// 本段开始跑的时刻；暂停（轮次之间）时为 `None`。
    segment_start: Option<Instant>,
    /// 上次判定以来清单有没有变过。更新清单本身就是进展（§2.1）。
    plan_changed: bool,
}

impl ActiveGoal {
    fn new(state: GoalState, budget: GoalBudget, running: bool) -> Self {
        Self {
            state,
            budget,
            segment_start: running.then(Instant::now),
            plan_changed: false,
        }
    }

    fn elapsed(&self) -> Duration {
        self.state.elapsed()
            + self
                .segment_start
                .map(|start| start.elapsed())
                .unwrap_or_default()
    }

    /// 把在跑的这一段折进累计时长，得到一份可落盘的状态。
    fn snapshot(&self) -> GoalState {
        let mut state = self.state.clone();
        state.elapsed_ms = self.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        state
    }

    fn pause(&mut self) {
        self.state = self.snapshot();
        self.segment_start = None;
    }
}

#[derive(Default)]
struct Slots {
    active: Option<ActiveGoal>,
    /// 从会话里读回来、但还没被本轮确认的目标：只有本轮带着同一句目标来，
    /// 才接着用它的计数与清单；没带就不激活，免得已关掉的目标「复活」。
    parked: Option<GoalState>,
    /// 刚刚收尾的目标（完成 / 预算耗尽），留给持久化写回最终状态。
    finished: Option<GoalState>,
    /// Agent 是否在跑一轮。计时只在跑的时候走。
    running: bool,
}

/// 跨 turn 共享的 Goal 续推句柄。
///
/// 沿用 [`crate::AgentInstructionInbox`] 的既有模式：Agent 在 build 时持有 `Arc`，
/// 前端（TUI `/goal`、Web、daemon）在运行期改写内部状态，无需重建 Agent。
#[derive(Default)]
pub struct GoalContinuation {
    slots: Mutex<Slots>,
}

impl GoalContinuation {
    pub fn new() -> Self {
        Self::default()
    }

    /// 激活或替换目标。语句为空视为清除。
    ///
    /// 同一目标重复激活不重置计时与清单；换了目标从零开始。停放着的同一目标
    /// （上次运行留下的持久化状态）在这里被接回来。
    pub fn activate(&self, statement: impl Into<String>, budget: GoalBudget) {
        let statement = statement.into().trim().to_owned();
        let Ok(mut slots) = self.slots.lock() else {
            return;
        };
        if statement.is_empty() {
            *slots = Slots {
                running: slots.running,
                ..Slots::default()
            };
            return;
        }
        if let Some(active) = slots.active.as_mut()
            && active.state.statement == statement
        {
            active.budget = budget;
            return;
        }
        let state = match slots.parked.take() {
            Some(parked) if parked.statement == statement => parked,
            _ => GoalState::new(statement),
        };
        slots.finished = None;
        let running = slots.running;
        slots.active = Some(ActiveGoal::new(state, budget, running));
    }

    /// 用持久化的状态直接恢复为激活目标（宿主已确认这就是当前目标时用）。
    /// 已收尾的状态不恢复。
    pub fn restore(&self, state: GoalState, budget: GoalBudget) {
        let Ok(mut slots) = self.slots.lock() else {
            return;
        };
        if state.status != GoalStatus::Active || state.statement.trim().is_empty() {
            return;
        }
        slots.parked = None;
        slots.finished = None;
        let running = slots.running;
        slots.active = Some(ActiveGoal::new(state, budget, running));
    }

    /// 停放一份持久化状态：等本轮用同一句目标 [`Self::activate`] 时再接回来。
    pub fn park(&self, state: GoalState) {
        if state.status != GoalStatus::Active {
            return;
        }
        if let Ok(mut slots) = self.slots.lock() {
            slots.parked = Some(state);
        }
    }

    /// 一轮开始：计时继续走。
    pub fn resume(&self) {
        if let Ok(mut slots) = self.slots.lock() {
            slots.running = true;
            if let Some(active) = slots.active.as_mut()
                && active.segment_start.is_none()
            {
                active.segment_start = Some(Instant::now());
            }
        }
    }

    /// 一轮结束：计时停下，把这段折进累计时长。
    pub fn pause(&self) {
        if let Ok(mut slots) = self.slots.lock() {
            slots.running = false;
            if let Some(active) = slots.active.as_mut() {
                active.pause();
            }
        }
    }

    pub fn clear(&self) {
        if let Ok(mut slots) = self.slots.lock() {
            *slots = Slots {
                running: slots.running,
                ..Slots::default()
            };
        }
    }

    pub fn is_active(&self) -> bool {
        self.slots
            .lock()
            .map(|slots| slots.active.is_some())
            .unwrap_or(false)
    }

    pub fn statement(&self) -> Option<String> {
        self.slots.lock().ok().and_then(|slots| {
            slots
                .active
                .as_ref()
                .map(|goal| goal.state.statement.clone())
        })
    }

    /// 可落盘的状态：激活中的目标，或刚收尾的那个（带最终状态）。
    pub fn snapshot(&self) -> Option<GoalState> {
        let slots = self.slots.lock().ok()?;
        slots
            .active
            .as_ref()
            .map(ActiveGoal::snapshot)
            .or_else(|| slots.finished.clone())
    }

    /// 钉在 system 消息里的目标块；没有激活目标时为 `None`。
    pub fn pin(&self, workers_in_flight: usize) -> Option<String> {
        let slots = self.slots.lock().ok()?;
        let active = slots.active.as_ref()?;
        Some(
            active
                .snapshot()
                .render_pin(active.budget, workers_in_flight),
        )
    }

    /// 已经注入过收尾引导——此后模型的第一次自然收口应当被接受。
    pub fn wrap_up_pending(&self) -> bool {
        self.slots
            .lock()
            .map(|slots| {
                slots
                    .active
                    .as_ref()
                    .map(|goal| goal.state.wrap_up_injected)
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }

    /// `update_plan` 工具：改清单、步骤、阻塞原因。返回给模型的回执。
    ///
    /// 校验失败返回 `Err`，什么都不改，也不消耗续推预算。
    pub fn update_plan(&self, update: PlanUpdate) -> Result<String, String> {
        let mut slots = self
            .slots
            .lock()
            .map_err(|_| "goal state is unavailable".to_owned())?;
        let active = slots.active.as_mut().ok_or_else(|| {
            "there is no active goal; update_plan only applies while a /goal is active".to_owned()
        })?;
        let mut next = active.state.clone();
        if let Some(criteria) = update.criteria {
            if criteria.len() > MAX_PLAN_ITEMS {
                return Err(format!(
                    "at most {MAX_PLAN_ITEMS} acceptance criteria are allowed"
                ));
            }
            let mut replaced = Vec::with_capacity(criteria.len());
            for text in criteria {
                let text = text.trim().to_owned();
                if text.is_empty() || text.chars().count() > MAX_PLAN_TEXT_CHARS {
                    return Err(format!(
                        "each acceptance criterion must contain 1 to {MAX_PLAN_TEXT_CHARS} characters"
                    ));
                }
                // 同一条标准换了位置也保留它的完成状态与证据。
                let kept = next
                    .criteria
                    .iter()
                    .find(|existing| existing.text == text)
                    .cloned()
                    .unwrap_or(GoalCriterion {
                        text,
                        done: false,
                        evidence: None,
                    });
                replaced.push(kept);
            }
            next.criteria = replaced;
        }
        for item in update.checklist {
            let count = next.criteria.len();
            let criterion = item
                .index
                .checked_sub(1)
                .and_then(|index| next.criteria.get_mut(index))
                .ok_or_else(|| {
                    format!(
                        "checklist index {} is out of range (1 to {count})",
                        item.index
                    )
                })?;
            let evidence = item
                .evidence
                .map(|evidence| evidence.trim().to_owned())
                .filter(|evidence| !evidence.is_empty());
            if item.done && evidence.is_none() {
                return Err(format!(
                    "criterion {} cannot be marked done without evidence: cite the command output, test name, file path or commit that proves it",
                    item.index
                ));
            }
            if let Some(evidence) = &evidence
                && evidence.chars().count() > MAX_PLAN_TEXT_CHARS
            {
                return Err(format!(
                    "evidence must be at most {MAX_PLAN_TEXT_CHARS} characters"
                ));
            }
            criterion.done = item.done;
            criterion.evidence = if item.done { evidence } else { None };
        }
        if let Some(steps) = update.steps {
            if steps.len() > MAX_PLAN_ITEMS {
                return Err(format!("at most {MAX_PLAN_ITEMS} steps are allowed"));
            }
            if steps.iter().any(|step| {
                step.title.trim().is_empty() || step.title.chars().count() > MAX_PLAN_TEXT_CHARS
            }) {
                return Err(format!(
                    "each step title must contain 1 to {MAX_PLAN_TEXT_CHARS} characters"
                ));
            }
            next.steps = steps;
        }
        if let Some(reason) = update.blocked_reason {
            let reason = reason.trim();
            next.blocked_reason =
                (!reason.is_empty()).then(|| truncate(reason, MAX_PLAN_TEXT_CHARS));
        }
        if next != active.state {
            active.plan_changed = true;
        }
        active.state = next;
        let open = active.state.open_criteria().len();
        Ok(format!(
            "Plan updated: {} of {} acceptance criteria met, {} step(s). {}",
            active.state.criteria.len() - open,
            active.state.criteria.len(),
            active.state.steps.len(),
            if active.state.criteria.is_empty() {
                "No acceptance criteria are recorded yet."
            } else if open == 0 {
                "All criteria are met with evidence; declare completion when the work is verified."
            } else {
                "Keep working on the open criteria."
            }
        ))
    }

    /// 对一次「模型没有调用工具」的候选停止点做判定。
    ///
    /// 返回 `None` 表示当前没有激活的目标，调用方按原有逻辑正常收口。
    pub fn evaluate(
        &self,
        reply: &str,
        observation: RoundObservation,
    ) -> Option<ContinuationDecision> {
        let Ok(mut guard) = self.slots.lock() else {
            return None;
        };
        let slots = &mut *guard;
        let goal = slots.active.as_mut()?;

        // 收尾引导已注入过：这一轮无论说什么都收口，不再无限追问。状态保留为
        // 「预算耗尽」，以后可以接着做。
        if goal.state.wrap_up_injected {
            let mut finished = goal.snapshot();
            finished.status = GoalStatus::BudgetLimited;
            slots.finished = Some(finished);
            slots.active = None;
            return Some(ContinuationDecision::Complete);
        }

        let elapsed = goal.elapsed();
        if declares_complete(reply) {
            let open = goal.state.open_criteria().len();
            if open == 0 && observation.workers_in_flight == 0 {
                let mut finished = goal.snapshot();
                finished.status = GoalStatus::Complete;
                slots.finished = Some(finished);
                slots.active = None;
                return Some(ContinuationDecision::Complete);
            }
            // 声明完成但门禁不认：照样算一次续推，免得虚报完成变成绕开预算的死循环。
            goal.state.continuations = goal.state.continuations.saturating_add(1);
            goal.plan_changed = false;
            return Some(ContinuationDecision::CompletionRejected {
                steering: rejection_steering(&goal.state, observation.workers_in_flight),
                open_criteria: open,
                workers_in_flight: observation.workers_in_flight,
            });
        }

        if let Some(limit) = goal.budget.wall_clock
            && elapsed >= limit
        {
            goal.state.wrap_up_injected = true;
            return Some(ContinuationDecision::SoftStop {
                steering: wrap_up_steering(
                    &goal.state.statement,
                    SoftStopReason::WallClock,
                    elapsed,
                ),
                reason: SoftStopReason::WallClock,
            });
        }
        if goal.state.continuations >= goal.budget.max_continuations {
            goal.state.wrap_up_injected = true;
            return Some(ContinuationDecision::SoftStop {
                steering: wrap_up_steering(
                    &goal.state.statement,
                    SoftStopReason::Continuations,
                    elapsed,
                ),
                reason: SoftStopReason::Continuations,
            });
        }

        if observation.made_progress() || std::mem::take(&mut goal.plan_changed) {
            goal.state.consecutive_no_progress = 0;
        } else {
            goal.state.consecutive_no_progress =
                goal.state.consecutive_no_progress.saturating_add(1);
        }
        goal.state.continuations = goal.state.continuations.saturating_add(1);

        let rung = rung_for(goal.state.consecutive_no_progress);
        let steering = continuation_steering(
            &goal.state.statement,
            rung,
            elapsed,
            goal.state.continuations,
            goal.budget,
            observation,
        );
        Some(ContinuationDecision::Continue { steering, rung })
    }
}

/// 模型声明完成、门禁不认时的引导：点名还缺什么。
fn rejection_steering(state: &GoalState, workers_in_flight: usize) -> String {
    let mut steering = String::from(
        "[goal-completion-rejected] This is an automated harness message, not a user reply.\n\n",
    );
    steering.push_str(&format!("GOAL: {}\n\n", state.statement));
    steering.push_str("You declared the goal complete, but the harness cannot accept that yet:\n");
    let open = state.open_criteria();
    if !open.is_empty() {
        steering.push_str("- These acceptance criteria are not marked done with evidence:\n");
        for (index, criterion) in &open {
            steering.push_str(&format!("  {index}. {}\n", criterion.text));
        }
    }
    if workers_in_flight > 0 {
        steering.push_str(&format!(
            "- {workers_in_flight} delegated worker(s) are still running. Wait for their reports with `await_agents` (or stop them deliberately) before declaring completion.\n"
        ));
    }
    steering.push_str(
        "\nEither finish the open work, or, if a criterion is already met, verify it now and record the evidence with `update_plan` (checklist index, done=true, evidence). \
If a criterion turned out to be wrong or impossible, say so explicitly and replace the criteria with `update_plan` — do not mark it done without proof. \
Then declare completion again.\n",
    );
    steering
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
    normalized.trim_start().starts_with(GOAL_COMPLETE_MARKER)
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
            "The previous round produced no new successful tool evidence. Repeated results and failed attempts are not progress.\n",
        );
    }
    if observation.background_active {
        steering.push_str("Background work is still registered. Inspect its current status/output or wait for a completion event; its existence alone does not prove progress. Do not repeatedly poll unchanged state.\n");
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
            ..RoundObservation::default()
        }
    }

    fn stalled() -> RoundObservation {
        RoundObservation::default()
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
                "<goal-status>complete</goal-status> Everything is verified. Shipped rc7.",
                progressed(),
            )
            .expect("goal active");
        assert_eq!(decision, ContinuationDecision::Complete);
        assert!(!continuation.is_active());
    }

    #[test]
    fn unchanged_background_work_does_not_reset_the_progress_ladder() {
        let continuation = goal_with(GoalBudget::default());
        let waiting = RoundObservation {
            background_active: true,
            ..RoundObservation::default()
        };
        for round in 1..=6 {
            let decision = continuation.evaluate("waiting for CI", waiting).unwrap();
            let ContinuationDecision::Continue { rung, .. } = decision else {
                panic!("expected continue while waiting on background work");
            };
            assert_eq!(rung, rung_for(round));
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
    fn update(continuation: &GoalContinuation, json: serde_json::Value) -> Result<String, String> {
        continuation.update_plan(serde_json::from_value(json).expect("plan update"))
    }

    #[test]
    fn completion_is_rejected_until_every_criterion_has_evidence() {
        let continuation = goal_with(GoalBudget::default());
        update(
            &continuation,
            serde_json::json!({"criteria": ["tests pass", "changelog updated"]}),
        )
        .unwrap();
        let decision = continuation
            .evaluate("<goal-status>complete</goal-status> done", progressed())
            .unwrap();
        let ContinuationDecision::CompletionRejected {
            steering,
            open_criteria,
            workers_in_flight,
        } = decision
        else {
            panic!("expected rejection, got {decision:?}");
        };
        assert_eq!((open_criteria, workers_in_flight), (2, 0));
        assert!(steering.contains("1. tests pass"));
        assert!(steering.contains("2. changelog updated"));
        assert!(continuation.is_active());

        let error = update(
            &continuation,
            serde_json::json!({"checklist": [{"index": 1, "done": true}]}),
        )
        .unwrap_err();
        assert!(error.contains("without evidence"), "{error}");
        assert!(
            update(
                &continuation,
                serde_json::json!({"checklist": [{"index": 9, "done": false}]})
            )
            .is_err()
        );
        update(
            &continuation,
            serde_json::json!({"checklist": [
                {"index": 1, "done": true, "evidence": "cargo test: 612 passed"},
                {"index": 2, "done": true, "evidence": "CHANGELOG.md [Unreleased]"}
            ]}),
        )
        .unwrap();
        assert_eq!(
            continuation.evaluate("<goal-status>complete</goal-status> done", progressed()),
            Some(ContinuationDecision::Complete)
        );
        let finished = continuation
            .snapshot()
            .expect("finished state is kept for persistence");
        assert_eq!(finished.status, GoalStatus::Complete);
        assert!(!continuation.is_active());
    }

    #[test]
    fn completion_waits_for_delegated_workers() {
        let continuation = goal_with(GoalBudget::default());
        let busy = RoundObservation {
            workers_in_flight: 2,
            ..progressed()
        };
        let decision = continuation
            .evaluate("<goal-status>complete</goal-status>", busy)
            .unwrap();
        assert!(matches!(
            decision,
            ContinuationDecision::CompletionRejected {
                workers_in_flight: 2,
                ..
            }
        ));
        assert_eq!(
            continuation.evaluate("<goal-status>complete</goal-status>", progressed()),
            Some(ContinuationDecision::Complete)
        );
    }

    #[test]
    fn a_plan_update_counts_as_progress() {
        let continuation = goal_with(GoalBudget::default());
        for _ in 0..3 {
            continuation.evaluate("thinking", stalled());
        }
        update(&continuation, serde_json::json!({"criteria": ["ship it"]})).unwrap();
        let Some(ContinuationDecision::Continue { rung, .. }) =
            continuation.evaluate("recorded the plan", stalled())
        else {
            panic!("expected continue");
        };
        assert_eq!(rung, ContinuationRung::Guidance);
    }

    #[test]
    fn state_survives_a_snapshot_and_restore_round_trip() {
        let continuation = goal_with(GoalBudget::default());
        update(
            &continuation,
            serde_json::json!({
                "criteria": ["a", "b"],
                "checklist": [{"index": 1, "done": true, "evidence": "proof"}],
                "steps": [{"title": "write code", "status": "in_progress"}],
                "blocked_reason": "waiting on CI"
            }),
        )
        .unwrap();
        continuation.evaluate("one", progressed());
        continuation.evaluate("two", progressed());
        let snapshot = continuation.snapshot().unwrap();
        let json = serde_json::to_string(&snapshot).unwrap();
        let decoded: GoalState = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, snapshot);

        let resumed = GoalContinuation::new();
        resumed.restore(decoded, GoalBudget::default());
        let state = resumed.snapshot().unwrap();
        assert_eq!(state.continuations, 2);
        assert_eq!(state.open_criteria().len(), 1);
        assert_eq!(state.steps[0].status, StepStatus::InProgress);
        let pin = resumed.pin(1).unwrap();
        assert!(pin.contains("1. [x] a — evidence: proof"));
        assert!(pin.contains("2. [ ] b"));
        assert!(pin.contains("[in_progress] write code"));
        assert!(pin.contains("BLOCKED: waiting on CI"));
        assert!(pin.contains("DELEGATED WORKERS STILL RUNNING: 1"));
    }

    #[test]
    fn parked_state_is_adopted_only_by_the_same_goal() {
        let mut saved = GoalState::new("ship the release");
        saved.continuations = 5;
        saved.elapsed_ms = 90_000;

        let same = GoalContinuation::new();
        same.park(saved.clone());
        assert!(!same.is_active(), "parking never activates by itself");
        same.activate("ship the release", GoalBudget::default());
        let state = same.snapshot().unwrap();
        assert_eq!(state.continuations, 5);
        assert!(state.elapsed_ms >= 90_000);

        let other = GoalContinuation::new();
        other.park(saved);
        other.activate("write the docs", GoalBudget::default());
        assert_eq!(other.snapshot().unwrap().continuations, 0);

        let mut finished = GoalState::new("done already");
        finished.status = GoalStatus::Complete;
        let closed = GoalContinuation::new();
        closed.restore(finished, GoalBudget::default());
        assert!(!closed.is_active(), "a finished goal is not restored");
    }

    #[test]
    fn elapsed_time_only_accumulates_while_running() {
        let mut saved = GoalState::new("g");
        saved.elapsed_ms = 1_000;
        let continuation = GoalContinuation::new();
        continuation.restore(saved, GoalBudget::default());
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(
            continuation.snapshot().unwrap().elapsed_ms,
            1_000,
            "paused between turns"
        );
        continuation.resume();
        std::thread::sleep(Duration::from_millis(30));
        continuation.pause();
        let after = continuation.snapshot().unwrap().elapsed_ms;
        assert!(after >= 1_030, "{after}");
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(continuation.snapshot().unwrap().elapsed_ms, after);
    }

    #[test]
    fn merging_keeps_budget_counters_monotonic() {
        let mut older = GoalState::new("g");
        older.continuations = 9;
        older.elapsed_ms = 50_000;
        let mut newer = GoalState::new("g");
        newer.continuations = 3;
        newer.elapsed_ms = 60_000;
        let merged = newer.merged_over(Some(&older));
        assert_eq!((merged.continuations, merged.elapsed_ms), (9, 60_000));
        let different = GoalState::new("h").merged_over(Some(&older));
        assert_eq!(different.continuations, 0);
    }

    #[test]
    fn budget_limited_wrap_up_keeps_the_state_for_later() {
        let continuation = goal_with(GoalBudget {
            wall_clock: Some(Duration::ZERO),
            max_continuations: 100,
        });
        continuation.evaluate("anything", progressed());
        continuation.evaluate("STATE: …", stalled());
        let state = continuation.snapshot().unwrap();
        assert_eq!(state.status, GoalStatus::BudgetLimited);
    }
}
