//! 本机入站与自动唤醒额度。
//!
//! rs 只做**本机入站**：持 Runtime Token 的 daemon 端点、hooks、本机定时
//! 任务。云端中继归桌面端（Xedit 加 Go relay），本仓不实现 `collab-relay.v2`
//! 的任何一端。决策记在 `docs/AGENT_RUNTIME_KERNEL.md` 阶段 5。
//!
//! # 唤醒额度只有一个出口
//!
//! 「要不要为这条事件把模型拉起来」的判断只能有一处。macOS 版 1.315.0-rc16
//! 修的就是这个：首次入队、启动恢复、provider slot 释放、轮次收尾四条路径
//! 各自判断，漏掉任何一条，限流就形同虚设。所有路径都走
//! [`WakeLedger::admit`]，且**只有真正取得 slot 才记账**。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use uuid::Uuid;
use willdeep_runtime_protocol::kernel_event::{
    ContentProvenance, EXTERNAL_WAKE_EVENTS, EXTERNAL_WAKE_WINDOW_SECONDS, EventAuthority,
    EventPriority, EventSource, InterruptPolicy, KernelEvent,
};

/// 这次要不要放行一次自动唤醒。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeDecision {
    /// 可以启动一轮。
    Allowed,
    /// 额度用完了。事件继续排队，等到这个时间之后再试一次。
    Throttled { retry_after: Duration },
    /// 这一档来源根本不许自动唤醒——未认证来源只能排队等别的原因开新一轮。
    NotPermitted,
}

impl WakeDecision {
    pub fn is_allowed(self) -> bool {
        matches!(self, Self::Allowed)
    }
}

/// 每会话的自动唤醒记账本。
#[derive(Debug, Default)]
pub struct WakeLedger {
    /// 会话 → 最近这些次唤醒的时刻，只保留窗口内的。
    wakes: HashMap<Uuid, Vec<Instant>>,
}

impl WakeLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// 判定并记账。
    ///
    /// **返回 `Allowed` 就已经记了这一笔**，所以调用方拿到之后必须真的去启动
    /// 那一轮；拿了不用等于白扣一次额度。反过来，还没确定能拿到 provider slot
    /// 就别问——问了就算。
    pub fn admit(&mut self, session: Uuid, authority: EventAuthority) -> WakeDecision {
        self.admit_at(session, authority, Instant::now())
    }

    fn admit_at(&mut self, session: Uuid, authority: EventAuthority, now: Instant) -> WakeDecision {
        if !authority.may_auto_wake() {
            return WakeDecision::NotPermitted;
        }
        if !authority.counts_against_wake_budget() {
            // 宿主与本机可信来源不限流：限流是给外面来的流量准备的，宿主自己
            // 签发的 critical 本来就该到。
            return WakeDecision::Allowed;
        }
        let window = Duration::from_secs(EXTERNAL_WAKE_WINDOW_SECONDS);
        let entry = self.wakes.entry(session).or_default();
        entry.retain(|at| now.duration_since(*at) < window);
        if entry.len() < EXTERNAL_WAKE_EVENTS as usize {
            entry.push(now);
            return WakeDecision::Allowed;
        }
        // 最早那一笔滑出窗口的时刻，就是下一次可以再试的时刻。
        let oldest = entry.iter().min().copied().unwrap_or(now);
        WakeDecision::Throttled {
            retry_after: window.saturating_sub(now.duration_since(oldest)),
        }
    }

    /// 会话归档或删除时把它的额度一起清掉。留着只会在会话 ID 被复用时算错。
    pub fn forget(&mut self, session: Uuid) {
        self.wakes.remove(&session);
    }

    /// 这个会话在窗口内已经用掉几次。给状态展示与测试。
    pub fn spent(&self, session: Uuid) -> usize {
        self.wakes
            .get(&session)
            .map(|entries| {
                let window = Duration::from_secs(EXTERNAL_WAKE_WINDOW_SECONDS);
                let now = Instant::now();
                entries
                    .iter()
                    .filter(|at| now.duration_since(**at) < window)
                    .count()
            })
            .unwrap_or(0)
    }
}

/// 本机入站来源。**认证只提升调度权限，不提升正文信任。**
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalIngress {
    /// 持 Runtime Token 的本机调用方：daemon 控制端点、本机定时任务。
    /// 可以唤醒，但正文仍按网络来源处理。
    Authenticated,
    /// hooks、本机脚本等没有独立凭据的注入。只能排队。
    Unauthenticated,
}

impl LocalIngress {
    fn authority(self) -> EventAuthority {
        match self {
            // 本机已认证调用方按 `authenticated_external` 记账，不按
            // `trusted_local`：这条路上进来的正文终究是外面给的，让它享受
            // 本机可信档的免限流，等于给了一条不限速的注入通道。
            Self::Authenticated => EventAuthority::AuthenticatedExternal,
            Self::Unauthenticated => EventAuthority::Untrusted,
        }
    }
}

/// 造一条本机入站事件。
///
/// 中断策略一律 `yield_at_boundary`，且会在发布时按 authority 再压一次：
/// 未认证来源最终只能 `enqueue`。**入站永远拿不到抢占**，那是宿主专属。
pub fn local_ingress_event(
    session_id: Uuid,
    ingress: LocalIngress,
    kind: &str,
    title: impl Into<String>,
    body: Option<String>,
    requires_user_action: bool,
) -> KernelEvent {
    KernelEvent {
        schema_version: willdeep_runtime_protocol::kernel_event::KERNEL_EVENT_SCHEMA_VERSION
            .to_owned(),
        event_id: Uuid::new_v4(),
        session_id,
        source: EventSource::External,
        kind: kind.to_owned(),
        priority: EventPriority::Normal,
        interrupt: InterruptPolicy::YieldAtBoundary,
        authority: ingress.authority(),
        content_provenance: ContentProvenance::Network,
        audience: willdeep_runtime_protocol::kernel_event::EventAudience {
            model: true,
            user: requires_user_action,
        },
        dedup_key: None,
        requires_user_action,
        title: title.into(),
        body,
        metadata: Default::default(),
        merge_count: 1,
        created_at: crate::kernel::now_iso8601(),
        delivery: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_and_local_trusted_sources_are_not_throttled() {
        let mut ledger = WakeLedger::new();
        let session = Uuid::new_v4();
        for _ in 0..(EXTERNAL_WAKE_EVENTS * 3) {
            assert_eq!(
                ledger.admit(session, EventAuthority::Host),
                WakeDecision::Allowed
            );
            assert_eq!(
                ledger.admit(session, EventAuthority::TrustedLocal),
                WakeDecision::Allowed
            );
        }
        assert_eq!(ledger.spent(session), 0, "宿主的唤醒不占外部额度");
    }

    #[test]
    fn unauthenticated_sources_never_wake_anything() {
        let mut ledger = WakeLedger::new();
        assert_eq!(
            ledger.admit(Uuid::new_v4(), EventAuthority::Untrusted),
            WakeDecision::NotPermitted
        );
    }

    /// 已认证外部事件每会话每窗口只有固定几次。
    #[test]
    fn authenticated_external_events_run_out_of_budget() {
        let mut ledger = WakeLedger::new();
        let session = Uuid::new_v4();
        for _ in 0..EXTERNAL_WAKE_EVENTS {
            assert!(
                ledger
                    .admit(session, EventAuthority::AuthenticatedExternal)
                    .is_allowed()
            );
        }
        let decision = ledger.admit(session, EventAuthority::AuthenticatedExternal);
        match decision {
            WakeDecision::Throttled { retry_after } => {
                assert!(retry_after.as_secs() <= EXTERNAL_WAKE_WINDOW_SECONDS);
                assert!(retry_after.as_secs() > 0, "要给出一个真的能等的时刻");
            }
            other => panic!("额度用完之后不该继续放行：{other:?}"),
        }
        // 别的会话不受影响：额度是每会话的。
        assert!(
            ledger
                .admit(Uuid::new_v4(), EventAuthority::AuthenticatedExternal)
                .is_allowed()
        );
    }

    /// 窗口滑过去之后额度回来。
    #[test]
    fn budget_returns_once_the_window_slides() {
        let mut ledger = WakeLedger::new();
        let session = Uuid::new_v4();
        let start = Instant::now();
        for index in 0..EXTERNAL_WAKE_EVENTS {
            assert!(
                ledger
                    .admit_at(
                        session,
                        EventAuthority::AuthenticatedExternal,
                        start + Duration::from_secs(u64::from(index)),
                    )
                    .is_allowed()
            );
        }
        assert!(
            !ledger
                .admit_at(
                    session,
                    EventAuthority::AuthenticatedExternal,
                    start + Duration::from_secs(10)
                )
                .is_allowed()
        );
        assert!(
            ledger
                .admit_at(
                    session,
                    EventAuthority::AuthenticatedExternal,
                    start + Duration::from_secs(EXTERNAL_WAKE_WINDOW_SECONDS + 1),
                )
                .is_allowed(),
            "窗口滑过去之后应该重新有额度"
        );
    }

    #[test]
    fn forgetting_a_session_clears_its_budget() {
        let mut ledger = WakeLedger::new();
        let session = Uuid::new_v4();
        ledger.admit(session, EventAuthority::AuthenticatedExternal);
        assert_eq!(ledger.spent(session), 1);
        ledger.forget(session);
        assert_eq!(ledger.spent(session), 0);
    }

    /// 本机认证入站可以唤醒，但正文仍是网络来源，也拿不到抢占。
    #[test]
    fn authenticated_ingress_buys_scheduling_not_trust() {
        let event = local_ingress_event(
            Uuid::nil(),
            LocalIngress::Authenticated,
            "external.notice",
            "build 1841 failed",
            Some("log tail".to_owned()),
            true,
        );
        assert_eq!(event.authority, EventAuthority::AuthenticatedExternal);
        assert!(event.authority.may_auto_wake());
        assert_eq!(event.content_provenance, ContentProvenance::Network);
        assert!(event.content_provenance.requires_sanitization());
        assert_eq!(
            event.authority.interrupt_ceiling(),
            InterruptPolicy::YieldAtBoundary,
            "入站永远拿不到抢占"
        );
        event.validate().expect("valid envelope");
    }

    #[test]
    fn unauthenticated_ingress_can_only_queue() {
        let mut event = local_ingress_event(
            Uuid::nil(),
            LocalIngress::Unauthenticated,
            "hook.notice",
            "hook fired",
            None,
            false,
        );
        assert_eq!(event.authority, EventAuthority::Untrusted);
        event.clamp_to_authority();
        assert_eq!(event.interrupt, InterruptPolicy::Enqueue);
        assert!(!event.authority.may_auto_wake());
    }
}
