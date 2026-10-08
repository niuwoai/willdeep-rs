//! 本机反馈账本 `willdeep.feedback.v1`：**每个用户反馈信号一行**，只追加。
//!
//! 这是 RSI 的强反馈数据入口（设计见 `docs/PROMPT_RSI_DESIGN.md` §6 / §10）。
//! 第一批信号是「下一句建议」的整个生命周期：展示、Tab 采用、Esc 放弃、无视
//! 并另起一句、被新一轮顶掉，以及最终发出去的话与建议的关系（原样 / 改过 /
//! 另说别的）。采用而不发送、发送而不改，是两个强度完全不同的信号，所以拆开记。
//!
//! - 位置：`$WILLDEEP_HOME/feedback/YYYY-MM.jsonl`，按 UTC 月份分片，权限 0600。
//! - 每行一个 JSON 对象、整行小于 4096 字节，`O_APPEND` 一次 `write` 写完。
//! - **默认只记 hash 与长度，不记正文。** `store_text` 显式打开才写建议 / 发送
//!   的原文；含凭据特征的文本即使打开也不写。hash 用于关联与去重，不是保密手段。
//! - 旁路：热路径只做一次 `try_send`，满了丢一行并计数；IO 错误只警告一次，
//!   从不上抛，不让界面或回合失败或变慢。

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::usage_ledger::{format_ts, now_millis};

/// 行格式版本。读端遇到不认识的 schema 整行跳过。
pub const SCHEMA: &str = "willdeep.feedback.v1";

/// 一行（含结尾换行）的硬上限，保证 `O_APPEND` 单次写入原子。
pub const MAX_LINE_BYTES: usize = 4096;

/// 目录相对 `$WILLDEEP_HOME` 的名字。
pub const FEEDBACK_DIR: &str = "feedback";

const CHANNEL_CAPACITY: usize = 256;

/// 编辑距离只对不超过这个字符数的发送文本计算：再长的话与一句 120 字的建议
/// 已经没有可比性，算了也没有信息量。
const MAX_DISTANCE_CHARS: usize = 600;

/// 存正文时每段文本的字符上限，给 4096 字节的整行留余量。
const MAX_TEXT_CHARS: usize = 400;

/// `$WILLDEEP_HOME/feedback`。
pub fn feedback_dir(home: &Path) -> PathBuf {
    home.join(FEEDBACK_DIR)
}

/// 反馈信号。只增不改：读端按字符串匹配，新增成员对旧读端是「未知信号」。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Signal {
    ModelAssessment,
    HumanDisposition,
    RunFinished,
    /// 建议以灰字出现在空输入框里。分母。
    SuggestionShown,
    /// Tab 采用（只填入，没发送）。
    SuggestionAccepted,
    /// Esc 放弃。
    SuggestionDismissed,
    /// 没动建议，直接开始打别的字。
    SuggestionIgnoredTyped,
    /// 没有得到处置就被新一轮 / 新建议顶掉。已采用但始终没发送也记这个。
    SuggestionSuperseded,
    /// 采用后原样发送。最强的正向信号。
    SuggestionSentVerbatim,
    /// 采用后改了几个字再发送；`edit_distance` 给出改了多少。
    SuggestionSentEdited,
    /// 采用后基本重写（编辑距离超过建议长度一半）再发送。
    SuggestionSentRewritten,
    /// 一次工具调用失败。`tool` 与 `error_class` 说明是哪个工具、哪一类错；
    /// 主 Agent 与 Worker 都记，Worker 的行带 `agent_id` / `worker_profile`。
    ToolFailed,
    /// 已成功返回的工具调用；与 tool_failed 配对构成有结局调用的分母。
    ToolSucceeded,
    /// 运行真正进入 Agent 循环；取消或 Provider 出错也保留分母。
    RunStarted,
    /// 一次 Agent 运行没有收敛就停了（轮次耗尽、输出被截断、改动未验证……），
    /// `stop_reason` 说明是哪一种，`report_len` 为 0 表示连部分结果都没有。
    AgentIncomplete,
    /// 一次 Worker 运行开始。按工种算「没有结果」比例时的分母。
    WorkerStarted,
    /// Worker 超时被掐断，没有交回任何结果。
    WorkerTimedOut,
    /// Worker 用完了全部验证尝试也没通过验证命令。
    WorkerVerifierExhausted,
    /// 用户在上一轮之后又说了一句。`prev_status`、`gap_ms`、`followup_hint`
    /// 给出上下文：上一轮怎么结束的、隔了多久、这句话像纠正还是补充。
    /// 精确分类离线做，这里只保证能按会话把前后两轮接起来。
    UserFollowup,
    /// 本轮进行中插话（steer）。`delivered` 说明模型有没有收到。
    UserSteer,
    /// 用户回退了会话：`count` 为丢掉的轮数。回退几乎总是说明那几轮做错了。
    SessionRewound,
    /// 一次审批 / 提问得到处置：`decision`（allow_once / always_allow / deny /
    /// answer / cancelled），`latency_ms` 为人想了多久。
    ApprovalResolved,
    /// 用户中途停下了一轮。`prev_status` 为停下时轮次的状态。
    TurnCancelled,
    /// 目标宣告完成并通过了完成门禁。
    GoalCompleted,
    /// 模型宣告目标完成，但门禁不认（验收项没做完或 Worker 还在跑）：
    /// `count` 为未完成的验收项数。虚报完成的强信号。
    GoalCompletionRejected,
    /// 目标没做完但预算耗尽，按交接快照收尾。
    GoalBudgetLimited,
    /// daemon 重启打断了一个还在进行的目标，运行时自动排了一轮续推。
    GoalResumed,
    /// 一次 Agent 运行结束时的验证结论：`verification` 为 `passed` / `failed` /
    /// `stale` / `unverified`（见 `tools::RunVerification`），`count` 为这次运行
    /// 实际跑过的验证命令数。运行时的事实，不靠用户怎么说。
    RunVerified,
}

/// 一行反馈。可选字段一律输出、未知即 `null`，从不省略键。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FeedbackRecord {
    pub schema: String,
    pub id: Uuid,
    pub ts: String,
    /// 发起前端：`tui` / `web` / `cli` / `mobile` / `unknown`。
    pub client: String,
    pub session_id: Option<Uuid>,
    /// Runtime 轮次 id；进程内执行时为 `null`。
    pub turn_id: Option<String>,
    /// Worker 的 id；主 Agent 为 `null`。
    pub agent_id: Option<Uuid>,
    /// Worker 的工种（`implementer`、`tester`……）；主 Agent 为 `null`。
    pub worker_profile: Option<String>,
    /// 产生这一行的提示词版本（[`crate::prompt_bundle`]）：`main@…`、
    /// `worker:<工种>@…`、`input_suggestion@…`；未知为 `null`。
    #[serde(default)]
    pub prompt_bundle: Option<String>,
    #[serde(default)]
    pub run_id: Option<Uuid>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub runtime_parameters: Option<crate::runtime_parameters::RuntimeParameters>,
    #[serde(default)]
    pub runtime_parameters_sha256: Option<String>,
    #[serde(default)]
    pub judgment: Option<String>,
    #[serde(default)]
    pub judgment_source: Option<String>,
    #[serde(default)]
    pub confidence: Option<u8>,
    #[serde(default)]
    pub reason_code: Option<String>,
    #[serde(default)]
    pub assessment_provider: Option<String>,
    #[serde(default)]
    pub assessment_model: Option<String>,
    #[serde(default)]
    pub assessment_prompt_sha256: Option<String>,
    pub signal: Signal,
    /// 把同一条建议的多个信号串起来。
    pub suggestion_id: Option<Uuid>,
    /// 建议文本的 SHA-256 前 16 个十六进制字符。
    pub text_hash: Option<String>,
    pub text_len: Option<usize>,
    /// 最终发送文本的字符数（仅 sent_* 信号）。
    pub sent_len: Option<usize>,
    pub sent_hash: Option<String>,
    /// 发送文本相对建议的字符级编辑距离（仅 sent_* 信号，过长则为 `null`）。
    pub edit_distance: Option<usize>,
    /// 距离建议出现过去了多久（毫秒）。采用得越快越像「正中下怀」。
    pub dwell_ms: Option<u64>,
    /// 仅 `tool_failed`：工具名与错误类别（见 `ToolError::class`）。
    pub tool: Option<String>,
    pub error_class: Option<String>,
    /// 仅 Agent / Worker 收尾类信号。
    pub stop_reason: Option<String>,
    pub turns: Option<usize>,
    pub attempts: Option<usize>,
    /// 部分结果 / 报告的字符数；0 表示什么都没交回来。
    pub report_len: Option<usize>,
    /// 仅 `user_followup` / `turn_cancelled`：上一轮（或被停下那一轮）的状态。
    pub prev_status: Option<String>,
    /// 仅 `user_followup`：距上一轮结束过去了多久（毫秒）。
    pub gap_ms: Option<u64>,
    /// 仅 `user_followup`：提交时前一轮还没跑完，这句排在它后面。
    pub queued_behind: Option<bool>,
    /// 仅 `user_followup`：上一轮的 Runtime 轮次 id。本行的 `turn_id` 是新提交
    /// 的这一轮；这句话是对 `prev_turn_id` 那一轮结果的反应。
    #[serde(default)]
    pub prev_turn_id: Option<String>,
    /// 仅 `run_verified`：这次运行的验证结论。
    #[serde(default)]
    pub verification: Option<String>,
    /// 仅 `user_followup` / `user_steer`：入口处的词法粗分类，见 [`followup_hint`]。
    pub followup_hint: Option<String>,
    /// 仅 `user_steer`：插话是否送达了正在跑的轮次。
    pub delivered: Option<bool>,
    /// 仅 `approval_resolved`：`approval` / `question`。
    pub interaction_kind: Option<String>,
    /// `approval_resolved` 的处置；`session_rewound` 为是否还原工作区。
    pub decision: Option<String>,
    /// 仅 `approval_resolved`：从提出到处置的毫秒数。
    pub latency_ms: Option<u64>,
    /// `session_rewound` 丢掉的轮数；`goal_*` 未完成的验收项数。
    pub count: Option<usize>,
    /// 仅 `goal_*`：续跑次数与累计运行时长。
    pub continuations: Option<u32>,
    pub elapsed_ms: Option<u64>,
    /// 仅 `store_text` 打开且不含凭据特征时才有。
    pub text: Option<String>,
    pub sent_text: Option<String>,
}

impl FeedbackRecord {
    fn blank(client: &str, signal: Signal) -> Self {
        Self {
            schema: SCHEMA.to_owned(),
            id: Uuid::new_v4(),
            ts: format_ts(now_millis()),
            client: client.to_owned(),
            session_id: None,
            turn_id: None,
            agent_id: None,
            worker_profile: None,
            prompt_bundle: None,
            run_id: None,
            provider: None,
            model: None,
            runtime_parameters: None,
            runtime_parameters_sha256: None,
            judgment: None,
            judgment_source: None,
            confidence: None,
            reason_code: None,
            assessment_provider: None,
            assessment_model: None,
            assessment_prompt_sha256: None,
            signal,
            suggestion_id: None,
            text_hash: None,
            text_len: None,
            sent_len: None,
            sent_hash: None,
            edit_distance: None,
            dwell_ms: None,
            tool: None,
            error_class: None,
            stop_reason: None,
            turns: None,
            attempts: None,
            report_len: None,
            prev_status: None,
            gap_ms: None,
            queued_behind: None,
            prev_turn_id: None,
            verification: None,
            followup_hint: None,
            delivered: None,
            interaction_kind: None,
            decision: None,
            latency_ms: None,
            count: None,
            continuations: None,
            elapsed_ms: None,
            text: None,
            sent_text: None,
        }
    }

    fn month_file_name(&self) -> String {
        // `ts` 形如 `2026-09-29T…`，前 7 个字符就是月份。
        format!("{}.jsonl", self.ts.get(..7).unwrap_or("unknown"))
    }

    fn to_line(&self) -> Option<Vec<u8>> {
        let mut line = serde_json::to_vec(self).ok()?;
        line.push(b'\n');
        (line.len() <= MAX_LINE_BYTES).then_some(line)
    }
}

/// 一条建议的一次处置，交给 [`FeedbackRecorder::record_suggestion`]。
pub struct SuggestionEvent<'a> {
    pub suggestion_id: Uuid,
    pub signal: Signal,
    pub suggestion: &'a str,
    pub dwell: Option<Duration>,
    /// 仅 sent_* 信号：最终发出去的话。
    pub sent: Option<&'a str>,
}

/// 记录者：带着出处（前端、会话、轮次、Worker）与隐私开关。克隆便宜，
/// 可以放进 UI 状态、Agent 与 Worker 派发器里。
#[derive(Clone)]
pub struct FeedbackRecorder {
    sink: Option<Arc<FeedbackSink>>,
    client: String,
    store_text: bool,
    session_id: Option<Uuid>,
    turn_id: Option<String>,
    agent_id: Option<Uuid>,
    worker_profile: Option<String>,
    prompt_bundle: Option<String>,
    run_id: Option<Uuid>,
    provider: Option<String>,
    model: Option<String>,
    runtime_parameters: Option<crate::runtime_parameters::RuntimeParameters>,
}

impl std::fmt::Debug for FeedbackRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeedbackRecorder")
            .field("enabled", &self.sink.is_some())
            .field("client", &self.client)
            .field("session_id", &self.session_id)
            .field("agent_id", &self.agent_id)
            .finish()
    }
}

impl FeedbackRecorder {
    /// 什么都不记。配置里关掉反馈、测试里不关心它时用。
    pub fn disabled() -> Self {
        Self {
            sink: None,
            client: "unknown".to_owned(),
            store_text: false,
            session_id: None,
            turn_id: None,
            agent_id: None,
            worker_profile: None,
            prompt_bundle: None,
            run_id: None,
            provider: None,
            model: None,
            runtime_parameters: None,
        }
    }

    pub fn new(sink: Arc<FeedbackSink>, client: impl Into<String>, store_text: bool) -> Self {
        Self {
            sink: Some(sink),
            client: client.into(),
            ..Self::disabled()
        }
        .with_store_text(store_text)
    }

    fn with_store_text(mut self, store_text: bool) -> Self {
        self.store_text = store_text;
        self
    }

    pub fn with_session(mut self, session_id: Option<Uuid>) -> Self {
        self.session_id = session_id;
        self
    }

    pub fn with_turn(mut self, turn_id: Option<String>) -> Self {
        self.turn_id = turn_id;
        self
    }

    /// 换一个前端标识。daemon 用一个记录者服务所有前端，按每轮的
    /// `origin_client`（`tui:<uuid>`、`web:<uuid>`……）取前缀。
    pub fn with_client(mut self, origin: Option<&str>) -> Self {
        let prefix = origin
            .and_then(|origin| origin.split(':').next())
            .map(|prefix| prefix.trim().to_ascii_lowercase())
            .unwrap_or_default();
        self.client = match prefix.as_str() {
            "cli" | "tui" | "web" | "mobile" => prefix,
            _ => "unknown".to_owned(),
        };
        self
    }

    /// Worker 的记录者：同一出处，行上带 Worker 的 id 与工种。
    pub fn for_worker(&self, agent_id: Uuid, profile: &str) -> Self {
        let mut worker = self.clone();
        worker.agent_id = Some(agent_id);
        worker.worker_profile = Some(profile.to_owned());
        worker
    }

    /// 给这个记录者写出的行打上提示词版本戳。
    pub fn with_prompt_bundle(mut self, bundle: impl Into<String>) -> Self {
        self.prompt_bundle = Some(bundle.into());
        self
    }

    pub fn is_enabled(&self) -> bool {
        self.sink.is_some()
    }

    pub fn runtime_parameters(&self) -> Option<&crate::runtime_parameters::RuntimeParameters> {
        self.runtime_parameters.as_ref()
    }

    /// 新运行有独立身份，不把旧模型或父 Worker 的模型误挂到新运行上。
    pub fn for_run(&self, identity: Option<crate::provider::ProviderIdentity>) -> Self {
        let mut recorder = self.clone();
        recorder.run_id = Some(Uuid::new_v4());
        recorder.provider = identity.as_ref().map(|identity| identity.provider.clone());
        recorder.model = identity.map(|identity| identity.model);
        recorder
    }

    pub fn record_run_started(&self) {
        self.record(Signal::RunStarted, |record| {
            record.runtime_parameters = self.runtime_parameters.clone();
            record.runtime_parameters_sha256 = self
                .runtime_parameters
                .as_ref()
                .map(|value| value.fingerprint());
        });
    }

    pub fn record_run_finished(&self, reason: &str) {
        self.record(Signal::RunFinished, |record| {
            record.stop_reason = Some(reason.into());
        });
    }

    pub fn with_runtime_parameters(
        mut self,
        parameters: crate::runtime_parameters::RuntimeParameters,
    ) -> Self {
        self.runtime_parameters = Some(parameters);
        self
    }

    pub fn with_execution_limits(mut self, max_turns: usize, token_budget: Option<u64>) -> Self {
        if let Some(parameters) = &mut self.runtime_parameters {
            parameters.max_turns = max_turns;
            parameters.token_budget = token_budget;
        }
        self
    }

    pub fn record_review(
        &self,
        run_id: Uuid,
        assessment: Option<&crate::feedback_assessment::Assessment>,
        human: Option<&str>,
        identity: Option<crate::provider::ProviderIdentity>,
        parameter_hash: Option<String>,
    ) {
        if assessment.is_some() == human.is_some() {
            return;
        }
        if human.is_some_and(|value| !["accepted", "needs_changes", "unknown"].contains(&value)) {
            return;
        }
        let signal = if human.is_some() {
            Signal::HumanDisposition
        } else {
            Signal::ModelAssessment
        };
        self.record(signal, |record| {
            record.run_id = Some(run_id);
            record.judgment_source = Some(if human.is_some() { "human" } else { "model" }.into());
            record.judgment = human
                .map(str::to_owned)
                .or_else(|| assessment.map(|value| value.judgment.clone()));
            record.confidence = assessment.map(|value| value.confidence);
            record.reason_code = assessment.map(|value| value.reason_code.clone());
            record.runtime_parameters_sha256 = parameter_hash;
            if assessment.is_some() {
                use sha2::{Digest, Sha256};
                record.assessment_provider = identity.as_ref().map(|value| value.provider.clone());
                record.assessment_model = identity.map(|value| value.model);
                record.assessment_prompt_sha256 = Some(format!(
                    "{:x}",
                    Sha256::digest(crate::feedback_assessment::PROMPT.as_bytes())
                ));
            }
        });
    }

    pub fn record_tool_success(&self, tool: &str) {
        self.record(Signal::ToolSucceeded, |record| {
            record.tool = Some(tool.to_owned());
        });
    }

    pub fn is_worker(&self) -> bool {
        self.agent_id.is_some()
    }

    fn record(&self, signal: Signal, fill: impl FnOnce(&mut FeedbackRecord)) {
        let Some(sink) = &self.sink else {
            return;
        };
        let mut record = FeedbackRecord::blank(&self.client, signal);
        record.session_id = self.session_id;
        record.turn_id = self.turn_id.clone();
        record.agent_id = self.agent_id;
        record.worker_profile = self.worker_profile.clone();
        record.prompt_bundle = self.prompt_bundle.clone();
        record.run_id = self.run_id;
        record.provider = self.provider.clone();
        record.model = self.model.clone();
        fill(&mut record);
        sink.submit(record);
    }

    pub fn record_suggestion(&self, event: SuggestionEvent<'_>) {
        let SuggestionEvent {
            suggestion_id,
            signal,
            suggestion,
            dwell,
            sent,
        } = event;
        let text = self.stored_text(Some(suggestion));
        let sent_text = self.stored_text(sent);
        self.record(signal, |record| {
            // 建议由输入建议的提示词生成，与记录者所属的 Agent 无关。
            record.prompt_bundle = Some(crate::prompt_bundle::input_suggestion_bundle());
            record.suggestion_id = Some(suggestion_id);
            record.text_hash = Some(text_hash(suggestion));
            record.text_len = Some(suggestion.chars().count());
            record.sent_len = sent.map(|sent| sent.chars().count());
            record.sent_hash = sent.map(text_hash);
            record.edit_distance = sent.and_then(|sent| edit_distance(suggestion, sent));
            record.dwell_ms = dwell.map(|dwell| dwell.as_millis().min(u128::from(u64::MAX)) as u64);
            record.text = text;
            record.sent_text = sent_text;
        });
    }

    /// 一次工具调用失败。只记工具名与错误类别，不记参数与输出。
    pub fn record_tool_failure(&self, tool: &str, error_class: &str) {
        self.record(Signal::ToolFailed, |record| {
            record.tool = Some(tool.to_owned());
            record.error_class = Some(error_class.to_owned());
        });
    }

    /// 一次 Agent 运行没收敛就停下。`report` 是它交回来的部分结果（可能为空）。
    pub fn record_incomplete(&self, stop_reason: &str, turns: usize, report: &str) {
        self.record(Signal::AgentIncomplete, |record| {
            record.stop_reason = Some(stop_reason.to_owned());
            record.turns = Some(turns);
            record.report_len = Some(report.trim().chars().count());
        });
    }

    /// 一次 Worker 运行开始（每次派工一行，与重试次数无关）。
    pub fn record_worker_started(&self) {
        self.record(Signal::WorkerStarted, |_| {});
    }

    /// Worker 超时或验证用尽这类「没有结果」的收尾。
    pub fn record_worker_failure(&self, signal: Signal, attempts: Option<usize>) {
        self.record(signal, |record| {
            record.attempts = attempts;
            record.report_len = Some(0);
        });
    }

    /// 用户在上一轮之后又说了一句。只记 hash、长度与词法粗分类。
    pub fn record_followup(&self, event: FollowupEvent<'_>) {
        let text = self.stored_text(Some(event.prompt));
        self.record(Signal::UserFollowup, |record| {
            record.text_hash = Some(text_hash(event.prompt));
            record.text_len = Some(event.prompt.chars().count());
            record.followup_hint = Some(followup_hint(event.prompt).to_owned());
            record.prev_status = event.prev_status.map(str::to_owned);
            record.gap_ms = event.gap_ms;
            record.queued_behind = Some(event.queued_behind);
            record.prev_turn_id = event.prev_turn_id.map(str::to_owned);
            record.text = text;
        });
    }

    /// 一次 Agent 运行结束时的验证结论（`run_verified`）。
    pub fn record_run_verification(&self, verification: crate::tools::RunVerification, ran: usize) {
        self.record(Signal::RunVerified, |record| {
            record.verification = Some(verification.as_str().to_owned());
            record.count = Some(ran);
        });
    }

    /// 本轮进行中插话。
    pub fn record_steer(&self, prompt: &str, delivered: bool) {
        let text = self.stored_text(Some(prompt));
        self.record(Signal::UserSteer, |record| {
            record.text_hash = Some(text_hash(prompt));
            record.text_len = Some(prompt.chars().count());
            record.followup_hint = Some(followup_hint(prompt).to_owned());
            record.delivered = Some(delivered);
            record.text = text;
        });
    }

    /// 会话被回退，丢掉了 `dropped_turns` 轮。
    pub fn record_rewind(&self, dropped_turns: usize, restored_workspace: bool) {
        self.record(Signal::SessionRewound, |record| {
            record.count = Some(dropped_turns);
            record.decision = Some(
                if restored_workspace {
                    "workspace_restored"
                } else {
                    "transcript_only"
                }
                .to_owned(),
            );
        });
    }

    /// 审批 / 提问得到处置。
    pub fn record_approval(&self, interaction_kind: &str, decision: &str, latency_ms: Option<u64>) {
        self.record(Signal::ApprovalResolved, |record| {
            record.interaction_kind = Some(interaction_kind.to_owned());
            record.decision = Some(decision.to_owned());
            record.latency_ms = latency_ms;
        });
    }

    /// 用户停下了一轮；`status` 为停下时它的状态。
    pub fn record_cancel(&self, status: &str) {
        self.record(Signal::TurnCancelled, |record| {
            record.prev_status = Some(status.to_owned());
        });
    }

    /// 目标的收尾：完成、完成被拒、预算耗尽。
    pub fn record_goal(
        &self,
        signal: Signal,
        open_criteria: usize,
        continuations: u32,
        elapsed: Duration,
    ) {
        self.record(signal, |record| {
            record.count = Some(open_criteria);
            record.continuations = Some(continuations);
            record.elapsed_ms = Some(elapsed.as_millis().min(u128::from(u64::MAX)) as u64);
        });
    }

    fn stored_text(&self, text: Option<&str>) -> Option<String> {
        let text = text?;
        if !self.store_text || crate::session_title::looks_sensitive(text) {
            return None;
        }
        Some(text.chars().take(MAX_TEXT_CHARS).collect())
    }

    /// 进程退出前等写线程排空。
    pub fn flush(&self, timeout: Duration) -> bool {
        self.sink.as_ref().is_none_or(|sink| sink.flush(timeout))
    }
}

/// 一次后续输入，交给 [`FeedbackRecorder::record_followup`]。
pub struct FollowupEvent<'a> {
    pub prompt: &'a str,
    /// 上一轮的状态（`completed`、`partial`、`failed`、`cancelled`……）。
    pub prev_status: Option<&'a str>,
    /// 距上一轮结束的毫秒数；上一轮还没结束时为 `None`。
    pub gap_ms: Option<u64>,
    pub queued_behind: bool,
    /// 上一轮的 Runtime 轮次 id。
    pub prev_turn_id: Option<&'a str>,
}

/// 后续输入的词法粗分类：`correction` / `redo` / `supplement` / `approval` /
/// `other`。只看开头和少量特征词，中英文都认；按这个顺序判定，先中先得。
///
/// 这是入口处的廉价标签，方便快速聚合，不是结论：精确分类离线结合上一轮
/// 的结局（被停下、被回退、审批被拒）再做。
pub fn followup_hint(prompt: &str) -> &'static str {
    let text = prompt.trim().to_lowercase();
    let starts = |prefixes: &[&str]| prefixes.iter().any(|prefix| text.starts_with(prefix));
    let contains = |needles: &[&str]| needles.iter().any(|needle| text.contains(needle));
    if contains(&[
        "不对",
        "错了",
        "搞错",
        "不是这样",
        "不是这个",
        "不是我要",
        "理解错",
        "不要这样",
        "撤销",
        "回滚",
        "改回",
        "that's wrong",
        "that is wrong",
        "not what i",
        "you misunderstood",
        "incorrect",
        "undo that",
        "revert that",
    ]) || starts(&[
        "no,", "no ", "no.", "nope", "wrong", "actually", "revert", "undo", "don't", "do not",
        "stop", "不是", "别",
    ]) {
        return "correction";
    }
    if contains(&[
        "重试",
        "再试",
        "再来一次",
        "重新来",
        "重新做",
        "try again",
        "re-run",
    ]) || starts(&["retry", "again", "redo", "rerun", "重新", "再来"])
    {
        return "redo";
    }
    if starts(&[
        "另外",
        "补充",
        "还有",
        "顺便",
        "背景",
        "注意",
        "also",
        "additionally",
        "btw",
        "by the way",
        "note that",
        "context",
        "fyi",
        "plus",
    ]) {
        return "supplement";
    }
    if text.chars().count() <= 20
        && starts(&[
            "好",
            "可以",
            "继续",
            "行",
            "没问题",
            "对",
            "谢谢",
            "ok",
            "yes",
            "lgtm",
            "continue",
            "go ahead",
            "sounds good",
            "thanks",
            "great",
            "perfect",
        ])
    {
        return "approval";
    }
    "other"
}

/// 删掉 `retain_months` 个月之前的分片（按文件名 `YYYY-MM.jsonl` 判断），
/// 返回删掉的文件数。`retain_months == 0` 表示不清理。只动符合命名的文件。
pub fn prune(dir: &Path, retain_months: u32) -> usize {
    if retain_months == 0 {
        return 0;
    }
    let Some(current) = month_index(&format_ts(now_millis())) else {
        return 0;
    };
    let cutoff = current.saturating_sub(retain_months);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(stem) = name.to_str().and_then(|name| name.strip_suffix(".jsonl")) else {
            continue;
        };
        if stem.len() != 7 {
            continue;
        }
        if let Some(month) = month_index(stem)
            && month < cutoff
            && std::fs::remove_file(entry.path()).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

/// `YYYY-MM…` → 自公元 0 年起的月序号。
fn month_index(text: &str) -> Option<u32> {
    let year: u32 = text.get(..4)?.parse().ok()?;
    if text.get(4..5)? != "-" {
        return None;
    }
    let month: u32 = text.get(5..7)?.parse().ok()?;
    (1..=12).contains(&month).then_some(year * 12 + month - 1)
}

/// 发送文本与建议的关系。`sent` 为用户最终提交的话。
///
/// 原文一字未动地保留、只在前后补了话（「commit it」→「commit it please」）
/// 算改过而不是重写：建议本身是用户要的，只是不完整。
pub fn classify_sent(suggestion: &str, sent: &str) -> Signal {
    let suggestion = suggestion.trim();
    let sent = sent.trim();
    if suggestion == sent {
        return Signal::SuggestionSentVerbatim;
    }
    if !suggestion.is_empty() && sent.contains(suggestion) {
        return Signal::SuggestionSentEdited;
    }
    let budget = suggestion.chars().count().max(1) / 2;
    match edit_distance(suggestion, sent) {
        Some(distance) if distance <= budget => Signal::SuggestionSentEdited,
        _ => Signal::SuggestionSentRewritten,
    }
}

/// 文本 SHA-256 的前 16 个十六进制字符。
pub fn text_hash(text: &str) -> String {
    let digest = Sha256::digest(text.trim().as_bytes());
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 字符级 Levenshtein 距离；任一侧超过 [`MAX_DISTANCE_CHARS`] 返回 `None`。
pub fn edit_distance(left: &str, right: &str) -> Option<usize> {
    let left: Vec<char> = left.trim().chars().collect();
    let right: Vec<char> = right.trim().chars().collect();
    if left.len() > MAX_DISTANCE_CHARS || right.len() > MAX_DISTANCE_CHARS {
        return None;
    }
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (row, left_char) in left.iter().enumerate() {
        let mut current = vec![row + 1];
        for (column, right_char) in right.iter().enumerate() {
            let substitution = previous[column] + usize::from(left_char != right_char);
            let insertion = current[column] + 1;
            let deletion = previous[column + 1] + 1;
            current.push(substitution.min(insertion).min(deletion));
        }
        previous = current;
    }
    previous.last().copied()
}

enum Command {
    Write(Box<FeedbackRecord>),
    Flush(SyncSender<()>),
}

/// 一个反馈目录的写入器：有界通道 + 单个后台写线程。
pub struct FeedbackSink {
    dir: PathBuf,
    sender: Mutex<Option<SyncSender<Command>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    stats: Arc<SinkStats>,
}

#[derive(Default)]
struct SinkStats {
    written: AtomicU64,
    failed: AtomicU64,
    dropped: AtomicU64,
    warned: AtomicBool,
}

impl SinkStats {
    /// 每个写入器只喊一次：TUI 占着终端，反复往 stderr 打字会把界面刷花。
    fn warn(&self, dir: &Path, message: &str) {
        if !self.warned.swap(true, Ordering::Relaxed) {
            eprintln!(
                "willdeep: feedback log at {} is not recording ({message}); the session is unaffected and further errors are suppressed",
                dir.display()
            );
        }
    }
}

impl FeedbackSink {
    pub fn spawn(dir: impl Into<PathBuf>) -> Arc<Self> {
        let dir = dir.into();
        let stats = Arc::new(SinkStats::default());
        let (sender, receiver) = sync_channel(CHANNEL_CAPACITY);
        let worker = {
            let dir = dir.clone();
            let stats = stats.clone();
            std::thread::Builder::new()
                .name("willdeep-feedback".to_owned())
                .spawn(move || run_writer(&dir, receiver, &stats))
        };
        let worker = match worker {
            Ok(worker) => Some(worker),
            Err(error) => {
                stats.warn(&dir, &format!("cannot start writer thread: {error}"));
                None
            }
        };
        Arc::new(Self {
            sender: Mutex::new(worker.is_some().then_some(sender)),
            worker: Mutex::new(worker),
            dir,
            stats,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 交给写线程。永不阻塞、永不失败——丢了也只计数。
    pub fn submit(&self, record: FeedbackRecord) {
        let Ok(sender) = self.sender.lock() else {
            return;
        };
        let Some(sender) = sender.as_ref() else {
            self.stats.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        };
        match sender.try_send(Command::Write(Box::new(record))) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
                self.stats.warn(&self.dir, "writer queue is full");
            }
            Err(TrySendError::Disconnected(_)) => {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// 等写线程把此前交来的记录全部落盘；超时返回 `false`。
    pub fn flush(&self, timeout: Duration) -> bool {
        let (ack, done) = sync_channel(1);
        let sent = self
            .sender
            .lock()
            .ok()
            .and_then(|sender| sender.as_ref().cloned())
            .is_some_and(|sender| sender.send(Command::Flush(ack)).is_ok());
        sent && done.recv_timeout(timeout).is_ok()
    }

    pub fn written(&self) -> u64 {
        self.stats.written.load(Ordering::Relaxed)
    }

    pub fn failed(&self) -> u64 {
        self.stats.failed.load(Ordering::Relaxed)
    }

    pub fn dropped(&self) -> u64 {
        self.stats.dropped.load(Ordering::Relaxed)
    }
}

impl Drop for FeedbackSink {
    fn drop(&mut self) {
        // 先断开发送端，写线程排空剩余记录后自然退出，再等它收尾。
        if let Ok(mut sender) = self.sender.lock() {
            sender.take();
        }
        if let Some(worker) = self.worker.lock().ok().and_then(|mut worker| worker.take()) {
            let _ = worker.join();
        }
    }
}

fn registry() -> &'static Mutex<HashMap<PathBuf, Arc<FeedbackSink>>> {
    static SINKS: OnceLock<Mutex<HashMap<PathBuf, Arc<FeedbackSink>>>> = OnceLock::new();
    SINKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 进程内按目录共享的写入器：同一个 `$WILLDEEP_HOME` 只起一个写线程。
pub fn shared_sink(dir: &Path) -> Arc<FeedbackSink> {
    let Ok(mut sinks) = registry().lock() else {
        return FeedbackSink::spawn(dir);
    };
    sinks
        .entry(dir.to_path_buf())
        .or_insert_with(|| FeedbackSink::spawn(dir))
        .clone()
}

/// 进程正常退出前调用：等所有共享写入器排空。
pub fn flush_all(timeout: Duration) {
    let sinks = registry()
        .lock()
        .map(|sinks| sinks.values().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    for sink in sinks {
        sink.flush(timeout);
    }
}

fn run_writer(dir: &Path, receiver: Receiver<Command>, stats: &SinkStats) {
    while let Ok(command) = receiver.recv() {
        match command {
            Command::Write(record) => match append_record(dir, &record) {
                Ok(()) => {
                    stats.written.fetch_add(1, Ordering::Relaxed);
                }
                Err(error) => {
                    stats.failed.fetch_add(1, Ordering::Relaxed);
                    stats.warn(dir, &error);
                }
            },
            Command::Flush(ack) => {
                let _ = ack.send(());
            }
        }
    }
}

fn append_record(dir: &Path, record: &FeedbackRecord) -> Result<(), String> {
    let line = record
        .to_line()
        .ok_or_else(|| "record does not fit in one feedback line".to_owned())?;
    let path = dir.join(record.month_file_name());
    let mut file = match open_append(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(dir)
                .map_err(|error| format!("create {}: {error}", dir.display()))?;
            open_append(&path).map_err(|error| format!("open {}: {error}", path.display()))?
        }
        Err(error) => return Err(format!("open {}: {error}", path.display())),
    };
    file.write_all(&line)
        .map_err(|error| format!("write {}: {error}", path.display()))
}

fn open_append(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("willdeep-feedback-{name}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn read_rows(dir: &Path) -> Vec<serde_json::Value> {
        let mut rows = Vec::new();
        for entry in std::fs::read_dir(dir).expect("read dir") {
            let text = std::fs::read_to_string(entry.expect("entry").path()).expect("read file");
            rows.extend(
                text.lines()
                    .map(|line| serde_json::from_str(line).expect("json row")),
            );
        }
        rows
    }

    fn event<'a>(
        signal: Signal,
        suggestion: &'a str,
        sent: Option<&'a str>,
    ) -> SuggestionEvent<'a> {
        SuggestionEvent {
            suggestion_id: Uuid::nil(),
            signal,
            suggestion,
            dwell: Some(Duration::from_millis(1_500)),
            sent,
        }
    }

    #[test]
    fn run_scopes_keep_successes_attributed_and_do_not_inherit_an_unknown_model() {
        let dir = temp_dir("run-scope");
        let sink = FeedbackSink::spawn(&dir);
        let parent = FeedbackRecorder::new(sink.clone(), "tui", false).for_run(Some(
            crate::provider::ProviderIdentity {
                provider: "local-test".into(),
                model: "fixture-model".into(),
                local: true,
            },
        ));
        parent.record_run_started();
        parent.record_tool_success("read_file");
        let child = parent.for_run(None);
        child.record_run_started();
        assert!(sink.flush(Duration::from_secs(2)));
        let rows = read_rows(&dir);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["run_id"], rows[1]["run_id"]);
        assert_ne!(rows[0]["run_id"], rows[2]["run_id"]);
        assert_eq!(rows[1]["signal"], "tool_succeeded");
        assert_eq!(rows[1]["tool"], "read_file");
        assert_eq!(rows[1]["provider"], "local-test");
        assert_eq!(rows[1]["model"], "fixture-model");
        assert!(rows[2]["provider"].is_null());
        assert!(rows[2]["model"].is_null());
        std::fs::remove_dir_all(dir).expect("remove temp dir");
    }

    #[test]
    fn edit_distance_counts_characters_and_caps_long_text() {
        assert_eq!(edit_distance("kitten", "sitting"), Some(3));
        assert_eq!(edit_distance("继续修复", "继续修复"), Some(0));
        assert_eq!(edit_distance("继续", "继续吧"), Some(1));
        assert_eq!(
            edit_distance("a", &"b".repeat(MAX_DISTANCE_CHARS + 1)),
            None
        );
    }

    #[test]
    fn classify_sent_separates_verbatim_edited_and_rewritten() {
        assert_eq!(
            classify_sent("run the tests", " run the tests "),
            Signal::SuggestionSentVerbatim
        );
        assert_eq!(
            classify_sent("run the tests", "run all the tests"),
            Signal::SuggestionSentEdited
        );
        assert_eq!(
            classify_sent("commit it", "commit it, then push to the release branch"),
            Signal::SuggestionSentEdited,
            "keeping the suggestion verbatim and adding to it is an edit"
        );
        assert_eq!(
            classify_sent("run the tests", "explain how the scheduler works"),
            Signal::SuggestionSentRewritten
        );
    }

    #[test]
    fn default_rows_carry_hash_and_length_but_never_text() {
        let dir = temp_dir("hash-only");
        let sink = FeedbackSink::spawn(&dir);
        let recorder = FeedbackRecorder::new(sink.clone(), "tui", false);
        recorder.record_suggestion(event(
            Signal::SuggestionSentVerbatim,
            "run the tests",
            Some("run the tests"),
        ));
        assert!(recorder.flush(Duration::from_secs(2)));
        let rows = read_rows(&dir);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row["schema"], SCHEMA);
        assert_eq!(row["signal"], "suggestion_sent_verbatim");
        assert_eq!(row["text_len"], 13);
        assert_eq!(row["edit_distance"], 0);
        assert_eq!(row["dwell_ms"], 1_500);
        assert_eq!(row["text_hash"], row["sent_hash"]);
        assert!(row["text"].is_null(), "text must be null by default");
        assert!(
            row["sent_text"].is_null(),
            "sent_text must be null by default"
        );
        let raw = std::fs::read_to_string(dir.join(format!(
            "{}.jsonl",
            row["ts"].as_str().expect("ts").get(..7).expect("month")
        )))
        .expect("month file");
        assert!(!raw.contains("run the tests"), "raw text leaked: {raw}");
    }

    #[test]
    fn store_text_is_opt_in_and_still_skips_credential_shaped_text() {
        let dir = temp_dir("store-text");
        let sink = FeedbackSink::spawn(&dir);
        let recorder = FeedbackRecorder::new(sink, "tui", true);
        recorder.record_suggestion(event(
            Signal::SuggestionSentEdited,
            "run the tests",
            Some("run the tests with api_key=sk-abcdefghijklmnop"),
        ));
        assert!(recorder.flush(Duration::from_secs(2)));
        let rows = read_rows(&dir);
        assert_eq!(rows[0]["text"], "run the tests");
        assert!(
            rows[0]["sent_text"].is_null(),
            "credential-shaped text must never be stored: {}",
            rows[0]
        );
    }

    #[test]
    fn worker_rows_carry_agent_identity_and_outcome() {
        let dir = temp_dir("worker");
        let sink = FeedbackSink::spawn(&dir);
        let recorder = FeedbackRecorder::new(sink, "tui", false)
            .with_session(Some(Uuid::nil()))
            .with_turn(Some("turn-1".to_owned()))
            .with_prompt_bundle("main@000000000000");
        let worker_id = Uuid::new_v4();
        let worker = recorder
            .for_worker(worker_id, "implementer")
            .with_prompt_bundle("worker:implementer@111111111111");
        assert!(worker.is_worker() && !recorder.is_worker());
        recorder.record_tool_failure("run_command", "command_timeout");
        worker.record_worker_started();
        recorder.record_suggestion(SuggestionEvent {
            suggestion_id: Uuid::new_v4(),
            signal: Signal::SuggestionShown,
            suggestion: "run the tests",
            dwell: None,
            sent: None,
        });
        worker.record_incomplete("max_turns", 16, "  ");
        worker.record_worker_failure(Signal::WorkerVerifierExhausted, Some(3));
        assert!(recorder.flush(Duration::from_secs(2)));
        let rows = read_rows(&dir);
        let find = |signal: &str| {
            rows.iter()
                .find(|row| row["signal"] == signal)
                .unwrap_or_else(|| panic!("missing {signal}: {rows:?}"))
                .clone()
        };
        let tool = find("tool_failed");
        assert_eq!(tool["tool"], "run_command");
        assert_eq!(tool["error_class"], "command_timeout");
        assert!(tool["agent_id"].is_null());
        assert_eq!(tool["turn_id"], "turn-1");
        assert_eq!(tool["prompt_bundle"], "main@000000000000");
        assert_eq!(
            find("worker_started")["prompt_bundle"],
            "worker:implementer@111111111111"
        );
        assert_eq!(
            find("suggestion_shown")["prompt_bundle"],
            crate::prompt_bundle::input_suggestion_bundle(),
            "suggestion rows carry the suggestion prompt's bundle"
        );
        let incomplete = find("agent_incomplete");
        assert_eq!(incomplete["agent_id"], worker_id.to_string());
        assert_eq!(incomplete["worker_profile"], "implementer");
        assert_eq!(incomplete["stop_reason"], "max_turns");
        assert_eq!(incomplete["turns"], 16);
        assert_eq!(
            incomplete["report_len"], 0,
            "whitespace-only report is no result"
        );
        let exhausted = find("worker_verifier_exhausted");
        assert_eq!(exhausted["attempts"], 3);
    }

    #[test]
    fn followup_hint_separates_corrections_from_supplements() {
        for (prompt, hint) in [
            ("不对，应该改 config.rs", "correction"),
            ("No, I meant the other file", "correction"),
            ("Actually use the v2 API", "correction"),
            ("你理解错了我的意思", "correction"),
            ("revert that change", "correction"),
            ("重试一下", "redo"),
            ("try again with more context", "redo"),
            ("另外，数据库是 Postgres 15", "supplement"),
            ("Also make sure it runs on Windows", "supplement"),
            ("好的，继续", "approval"),
            ("LGTM", "approval"),
            ("帮我写一个 README", "other"),
            (
                "ok but this long message explains a completely new task",
                "other",
            ),
        ] {
            assert_eq!(followup_hint(prompt), hint, "{prompt}");
        }
    }

    #[test]
    fn prune_removes_only_expired_month_shards() {
        let dir = temp_dir("prune");
        let current = format_ts(now_millis())[..7].to_owned();
        for name in [
            format!("{current}.jsonl"),
            "2001-01.jsonl".to_owned(),
            "2001-02.jsonl".to_owned(),
            "notes.jsonl".to_owned(),
            "2001-01.txt".to_owned(),
        ] {
            std::fs::write(dir.join(name), "{}\n").expect("write shard");
        }
        assert_eq!(prune(&dir, 0), 0, "0 disables pruning");
        assert_eq!(prune(&dir, 12), 2);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .expect("dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .into_string()
                    .expect("utf8")
            })
            .collect();
        left.sort();
        let mut expected = vec![
            format!("{current}.jsonl"),
            "2001-01.txt".to_owned(),
            "notes.jsonl".to_owned(),
        ];
        expected.sort();
        assert_eq!(left, expected);
    }

    #[test]
    fn followup_and_interaction_rows_carry_context_but_not_text() {
        let dir = temp_dir("followup");
        let sink = FeedbackSink::spawn(&dir);
        let recorder = FeedbackRecorder::new(sink, "web", false).with_session(Some(Uuid::nil()));
        recorder.record_followup(FollowupEvent {
            prompt: "不对，改回原来的实现",
            prev_status: Some("completed"),
            gap_ms: Some(4_200),
            queued_behind: false,
            prev_turn_id: Some("turn-0"),
        });
        recorder.record_run_verification(crate::tools::RunVerification::Failed, 2);
        recorder.record_steer("also check the tests", true);
        recorder.record_rewind(2, true);
        recorder.record_approval("approval", "deny", Some(900));
        recorder.record_cancel("running");
        recorder.record_goal(
            Signal::GoalCompletionRejected,
            3,
            7,
            Duration::from_secs(60),
        );
        assert!(recorder.flush(Duration::from_secs(2)));
        let rows = read_rows(&dir);
        let find = |signal: &str| {
            rows.iter()
                .find(|row| row["signal"] == signal)
                .expect(signal)
                .clone()
        };
        let followup = find("user_followup");
        assert_eq!(followup["followup_hint"], "correction");
        assert_eq!(followup["prev_status"], "completed");
        assert_eq!(followup["gap_ms"], 4_200);
        assert_eq!(followup["queued_behind"], false);
        assert_eq!(followup["prev_turn_id"], "turn-0");
        assert!(followup["text"].is_null());
        let verified = find("run_verified");
        assert_eq!(verified["verification"], "failed");
        assert_eq!(verified["count"], 2);
        assert_eq!(find("user_steer")["delivered"], true);
        assert_eq!(find("session_rewound")["count"], 2);
        let approval = find("approval_resolved");
        assert_eq!(approval["decision"], "deny");
        assert_eq!(approval["latency_ms"], 900);
        assert_eq!(find("turn_cancelled")["prev_status"], "running");
        let goal = find("goal_completion_rejected");
        assert_eq!(goal["count"], 3);
        assert_eq!(goal["continuations"], 7);
        assert_eq!(goal["elapsed_ms"], 60_000);
        let raw = serde_json::to_string(&rows).expect("json");
        assert!(!raw.contains("改回原来"), "prompt text leaked: {raw}");
    }

    #[test]
    fn disabled_recorder_writes_nothing() {
        let recorder = FeedbackRecorder::disabled();
        assert!(!recorder.is_enabled());
        recorder.record_suggestion(event(Signal::SuggestionShown, "x", None));
        assert!(recorder.flush(Duration::from_millis(10)));
    }

    #[test]
    fn rows_are_private_and_bounded() {
        let dir = temp_dir("mode");
        let sink = FeedbackSink::spawn(&dir);
        let recorder = FeedbackRecorder::new(sink, "tui", true);
        let long = "字".repeat(1_000);
        recorder.record_suggestion(event(Signal::SuggestionSentRewritten, &long, Some(&long)));
        assert!(recorder.flush(Duration::from_secs(2)));
        for entry in std::fs::read_dir(&dir).expect("read dir") {
            let path = entry.expect("entry").path();
            let text = std::fs::read_to_string(&path).expect("read");
            for line in text.lines() {
                assert!(line.len() < MAX_LINE_BYTES, "line too long: {}", line.len());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
                assert_eq!(mode & 0o777, 0o600);
            }
        }
    }
}
