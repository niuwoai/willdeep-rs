//! 记账的上下文与计量：谁在调（会话、任务、前端、执行位置），以及一次调用
//! 从发出到收完的计时与结局。
//!
//! 两条入口：
//!
//! - 主回合、子 Agent、上下文压缩由 [`crate::Agent`] 在收到 usage 的同一处用
//!   [`ModelCall`] 记账——那里知道这是哪一类请求，也能从事件宿主拿到 daemon
//!   事件日志的序号（`event_sequence`）。
//! - 标题、下一句预测、路由分类、安全判官、看图兜底这些辅助请求不经过主循环，
//!   由 [`LedgeredProvider`] 包住 Provider，按调用记账。
//!
//! 两条入口互斥：被 Agent 主循环使用的 Provider 不能再包 `LedgeredProvider`，
//! 否则同一次调用会记两行。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use uuid::Uuid;

use super::{ClientKind, Execution, Outcome, UsageKind, UsageLedgerRecord, UsageLedgerSink};
use crate::provider::{
    Provider, ProviderError, ProviderEvent, ProviderEventSink, ProviderIdentity,
};
use crate::types::{Completion, Message, ToolDefinition, Usage};

/// 一次运行（一个 harness）固定不变的记账上下文。
#[derive(Clone, Debug)]
pub struct UsageLedgerContext {
    pub client: ClientKind,
    pub client_instance: Option<String>,
    pub execution: Execution,
    pub session_id: Option<Uuid>,
    pub turn_id: Option<String>,
    pub task_id: Option<String>,
    pub workspace: Option<PathBuf>,
}

impl UsageLedgerContext {
    pub fn new(client: ClientKind, execution: Execution) -> Self {
        Self {
            client,
            client_instance: None,
            execution,
            session_id: None,
            turn_id: None,
            task_id: None,
            workspace: None,
        }
    }
}

/// 记账句柄。便宜可克隆；子 Agent 用 [`UsageLedgerScope::for_subagent`] 派生。
#[derive(Clone)]
pub struct UsageLedgerScope {
    sink: Arc<UsageLedgerSink>,
    context: Arc<UsageLedgerContext>,
    agent_id: Option<String>,
    subagent: bool,
}

impl std::fmt::Debug for UsageLedgerScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsageLedgerScope")
            .field("dir", &self.sink.dir())
            .field("context", &self.context)
            .field("agent_id", &self.agent_id)
            .finish()
    }
}

impl UsageLedgerScope {
    pub fn new(sink: Arc<UsageLedgerSink>, context: UsageLedgerContext) -> Self {
        Self {
            sink,
            context: Arc::new(context),
            agent_id: None,
            subagent: false,
        }
    }

    pub fn context(&self) -> &UsageLedgerContext {
        &self.context
    }

    /// 子 Agent 的句柄：主循环记 `subagent`，`agent_id` 填子 Agent 的 id。
    pub fn for_subagent(&self, agent_id: Uuid) -> Self {
        Self {
            sink: self.sink.clone(),
            context: self.context.clone(),
            agent_id: Some(agent_id.to_string()),
            subagent: true,
        }
    }

    /// 主循环请求记哪一类。
    pub fn loop_kind(&self) -> UsageKind {
        if self.subagent {
            UsageKind::Subagent
        } else {
            UsageKind::Main
        }
    }

    /// 开始计量一次调用。
    pub fn begin(&self, kind: UsageKind, identity: Option<ProviderIdentity>) -> ModelCall {
        ModelCall {
            state: Some(CallState {
                scope: self.clone(),
                kind,
                identity,
                started: Instant::now(),
                ended: None,
                usage: None,
                outcome: Outcome::Cancelled,
            }),
        }
    }

    /// 把一个辅助用途的 Provider 包成按调用记账的版本。
    pub fn auxiliary(&self, provider: Arc<dyn Provider>) -> Arc<dyn Provider> {
        Arc::new(LedgeredProvider {
            inner: provider,
            scope: self.clone(),
            kind: UsageKind::Auxiliary,
        })
    }

    fn record(&self, call: CallState, event_sequence: Option<u64>) {
        let mut record = UsageLedgerRecord::new(call.kind, self.context.execution);
        record.client = self.context.client;
        record.client_instance = self.context.client_instance.clone();
        record.session_id = self.context.session_id;
        record.turn_id = self.context.turn_id.clone();
        record.task_id = self.context.task_id.clone();
        record.agent_id = self.agent_id.clone();
        record.workspace = self.context.workspace.clone();
        if let Some(identity) = call.identity {
            record.provider = Some(identity.provider);
            record.model = Some(identity.model);
            record.local = identity.local;
        }
        record.usage = call.usage.unwrap_or_default();
        let ended = call.ended.unwrap_or_else(Instant::now);
        record.latency_ms = Some(ended.saturating_duration_since(call.started).as_millis() as u64);
        // ts 是调用完成时刻：结果到手时打的点，而不是事件宿主处理完之后。
        let settled_ago = Instant::now().saturating_duration_since(ended).as_millis() as u64;
        record.ts = record.ts.saturating_sub(settled_ago);
        record.outcome = call.outcome;
        record.event_sequence = event_sequence;
        self.sink.submit(record);
    }
}

struct CallState {
    scope: UsageLedgerScope,
    kind: UsageKind,
    identity: Option<ProviderIdentity>,
    started: Instant,
    ended: Option<Instant>,
    usage: Option<Usage>,
    outcome: Outcome,
}

/// 一次模型调用的计量。**无论怎样结束都恰好记一行**：正常路径调
/// [`ModelCall::finish`]；调用中途整个 future 被丢弃（任务取消、子 Agent
/// 超时）时由 `Drop` 以已知的结果补记，结局默认 `cancelled`。
///
/// 没挂账本时是 [`ModelCall::inert`]，所有操作都是空操作。
pub struct ModelCall {
    state: Option<CallState>,
}

impl ModelCall {
    pub fn inert() -> Self {
        Self { state: None }
    }

    /// 结果到手：记下完成时刻、usage 与结局。可以在 `finish` 前多次调用，以最后一次为准。
    pub fn settle(&mut self, usage: Option<&Usage>, outcome: Outcome) {
        if let Some(state) = &mut self.state {
            state.ended = Some(Instant::now());
            state.usage = usage.cloned();
            state.outcome = outcome;
        }
    }

    /// 落账。`event_sequence` 是 daemon 事件日志里对应 usage 事件的序号。
    pub fn finish(mut self, event_sequence: Option<u64>) {
        if let Some(state) = self.state.take() {
            let scope = state.scope.clone();
            scope.record(state, event_sequence);
        }
    }
}

impl Drop for ModelCall {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            let scope = state.scope.clone();
            scope.record(state, None);
        }
    }
}

/// 从 Provider 结果里取 usage 与结局：流中断时带着的部分 usage 同样算数。
pub(crate) fn settle_from_result(
    call: &mut ModelCall,
    result: &Result<Completion, ProviderError>,
    streamed: Option<&Usage>,
) {
    match result {
        Ok(completion) => call.settle(completion.usage.as_ref().or(streamed), Outcome::Ok),
        Err(ProviderError::StreamInterrupted { partial, .. }) => {
            call.settle(partial.usage.as_ref().or(streamed), Outcome::Error)
        }
        Err(_) => call.settle(streamed, Outcome::Error),
    }
}

/// 按调用记账的 Provider 包装，只给辅助用途用（见模块文档）。
pub struct LedgeredProvider {
    inner: Arc<dyn Provider>,
    scope: UsageLedgerScope,
    kind: UsageKind,
}

/// 流式调用里记下 Provider 报的 usage，再原样转交。
struct UsageTap<'a> {
    inner: &'a dyn ProviderEventSink,
    usage: Mutex<Option<Usage>>,
}

#[async_trait]
impl ProviderEventSink for UsageTap<'_> {
    async fn emit(&self, event: ProviderEvent) {
        if let ProviderEvent::Usage(usage) = &event
            && let Ok(mut slot) = self.usage.lock()
        {
            *slot = Some(usage.clone());
        }
        self.inner.emit(event).await;
    }
}

#[async_trait]
impl Provider for LedgeredProvider {
    fn ledger_identity(&self) -> Option<ProviderIdentity> {
        self.inner.ledger_identity()
    }

    fn with_model(&self, model: &str) -> Result<Arc<dyn Provider>, ProviderError> {
        Ok(Arc::new(Self {
            inner: self.inner.with_model(model)?,
            scope: self.scope.clone(),
            kind: self.kind,
        }))
    }

    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<Completion, ProviderError> {
        let mut call = self.scope.begin(self.kind, self.inner.ledger_identity());
        let result = self.inner.complete(messages, tools).await;
        settle_from_result(&mut call, &result, None);
        call.finish(None);
        result
    }

    async fn complete_with_events(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        events: &dyn ProviderEventSink,
    ) -> Result<Completion, ProviderError> {
        let mut call = self.scope.begin(self.kind, self.inner.ledger_identity());
        let tap = UsageTap {
            inner: events,
            usage: Mutex::new(None),
        };
        let result = self.inner.complete_with_events(messages, tools, &tap).await;
        let streamed = tap.usage.lock().ok().and_then(|usage| usage.clone());
        settle_from_result(&mut call, &result, streamed.as_ref());
        call.finish(None);
        result
    }
}
