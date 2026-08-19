//! Small-model-first routing and deep-tier admission control.
//!
//! The model may help compile a task packet, but it does not decide whether
//! scarce 1M context is free. High-confidence read-only work is dispatched
//! deterministically; deep work needs runtime-observed lower-tier evidence
//! plus an explicit escalation ticket.

use std::collections::BTreeSet;
use std::sync::Mutex;

use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoutingTier {
    Worker,
    Standard,
    Deep,
}

impl RoutingTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Worker => "worker",
            Self::Standard => "standard",
            Self::Deep => "deep",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteDecision {
    pub tier: RoutingTier,
    pub profile: Option<&'static str>,
    pub confidence: u8,
    pub auto_dispatch_read_only: bool,
    pub reason: &'static str,
}

impl RouteDecision {
    pub fn steering(&self) -> Option<String> {
        let profile = self.profile?;
        if self.auto_dispatch_read_only {
            return None;
        }
        Some(format!(
            "<runtime-route tier={:?} profile={profile:?} confidence=\"{}\">\n\
The runtime classified this as bounded `{profile}` work. Compile a task packet, keep read_files separate from write_files, and delegate it before doing the implementation in the parent. Give a verifier whenever completion is command-decidable.\n\
</runtime-route>",
            self.tier.as_str(),
            self.confidence,
        ))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RoutingPolicy {
    pub auto_dispatch_read_only: bool,
    pub max_deep_calls: usize,
}

impl Default for RoutingPolicy {
    fn default() -> Self {
        Self {
            auto_dispatch_read_only: true,
            max_deep_calls: 1,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalationTicket {
    pub reason: String,
    #[serde(default)]
    pub attempted_profiles: Vec<String>,
    pub context_evidence: String,
    pub why_not_decompose: String,
}

#[derive(Default)]
struct RoutingState {
    attempted_profiles: BTreeSet<String>,
    successful_inspections: usize,
    deep_calls: usize,
}

pub struct RoutingGuard {
    policy: RoutingPolicy,
    state: Mutex<RoutingState>,
}

impl RoutingGuard {
    pub fn new(policy: RoutingPolicy) -> Self {
        Self {
            policy,
            state: Mutex::new(RoutingState::default()),
        }
    }

    pub fn route(&self, prompt: &str) -> RouteDecision {
        classify(prompt, self.policy.auto_dispatch_read_only)
    }

    pub fn record_tool_success(&self, name: &str) {
        if matches!(
            name,
            "search_files"
                | "grep_files"
                | "read_file"
                | "list_directory"
                | "git_status"
                | "git_diff"
                | "git_log"
                | "git_blame"
        ) && let Ok(mut state) = self.state.lock()
        {
            state.successful_inspections = state.successful_inspections.saturating_add(1);
        }
    }

    pub fn record_profile_attempt(&self, profile: &str) {
        if profile != "deep"
            && let Ok(mut state) = self.state.lock()
        {
            state.attempted_profiles.insert(profile.to_owned());
        }
    }

    pub fn authorize_deep(&self, ticket: Option<&EscalationTicket>) -> Result<(), String> {
        let ticket = ticket.ok_or_else(|| {
            "deep requires escalation.reason, attempted_profiles, context_evidence and why_not_decompose"
                .to_owned()
        })?;
        for (name, value) in [
            ("reason", ticket.reason.trim()),
            ("context_evidence", ticket.context_evidence.trim()),
            ("why_not_decompose", ticket.why_not_decompose.trim()),
        ] {
            if value.len() < 12 {
                return Err(format!("deep escalation {name} is too vague"));
            }
        }
        if ticket.attempted_profiles.is_empty() {
            return Err("deep escalation must name the lower tiers already attempted".to_owned());
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| "deep escalation state is unavailable".to_owned())?;
        let observed_profile = ticket.attempted_profiles.iter().any(|profile| {
            state
                .attempted_profiles
                .contains(&profile.trim().to_ascii_lowercase())
        });
        if !observed_profile && state.successful_inspections < 2 {
            return Err(
                "deep escalation refused: runtime observed neither a lower-tier worker attempt nor two successful repository inspections"
                    .to_owned(),
            );
        }
        if state.deep_calls >= self.policy.max_deep_calls {
            return Err(format!(
                "deep escalation budget exhausted: maximum {} call(s) per harness",
                self.policy.max_deep_calls
            ));
        }
        state.deep_calls = state.deep_calls.saturating_add(1);
        Ok(())
    }
}

fn classify(prompt: &str, auto_dispatch_read_only: bool) -> RouteDecision {
    let sample = prompt
        .chars()
        .take(8_192)
        .collect::<String>()
        .to_lowercase();
    let mutating = contains_any(
        &sample,
        &[
            "修复",
            "修改",
            "实现",
            "新增",
            "删除",
            "重构",
            "fix",
            "implement",
            "add ",
            "change",
            "edit",
            "refactor",
            "write",
            "create",
        ],
    );
    if contains_any(
        &sample,
        &[
            "test fail",
            "failing test",
            "测试失败",
            "测试不通过",
            "断言失败",
        ],
    ) {
        if !mutating {
            return worker(
                "log_inspector",
                92,
                auto_dispatch_read_only,
                "a read-only failure explanation fits the log inspection worker",
            );
        }
        return worker(
            "test_fixer",
            96,
            false,
            "failing tests have a deterministic verifier",
        );
    }
    if contains_any(
        &sample,
        &[
            "compile error",
            "build fail",
            "lint error",
            "类型错误",
            "编译失败",
            "构建失败",
        ],
    ) {
        if !mutating {
            return worker(
                "log_inspector",
                92,
                auto_dispatch_read_only,
                "a read-only build explanation fits the log inspection worker",
            );
        }
        return worker(
            "build_fixer",
            96,
            false,
            "build diagnostics have a deterministic verifier",
        );
    }
    if !mutating
        && contains_any(
            &sample,
            &[
                "git history",
                "regression",
                "哪个提交",
                "哪次提交",
                "引入这个问题",
            ],
        )
    {
        return worker(
            "git_detective",
            94,
            auto_dispatch_read_only,
            "repository archaeology is bounded and read-only",
        );
    }
    if !mutating
        && contains_any(
            &sample,
            &[
                "traceback",
                "stack trace",
                "panic",
                "错误日志",
                "分析日志",
                "报错输出",
            ],
        )
    {
        return worker(
            "log_inspector",
            92,
            auto_dispatch_read_only,
            "failure output can be inspected in an isolated small window",
        );
    }
    if !mutating
        && contains_any(
            &sample,
            &[
                "在哪里",
                "在哪个文件",
                "定位",
                "locate",
                "where is",
                "find where",
            ],
        )
    {
        return worker(
            "scout",
            90,
            auto_dispatch_read_only,
            "symbol and file location is bounded read-only work",
        );
    }
    if !mutating
        && contains_any(
            &sample,
            &[
                "总结",
                "解释",
                "阅读",
                "summarize",
                "explain",
                "read the",
                "review docs",
                "扫一下",
                "扫描",
                "审查",
                "audit",
                "scan the",
                "review the",
            ],
        )
    {
        return worker(
            "reader",
            86,
            auto_dispatch_read_only,
            "reading can be isolated from the parent context",
        );
    }
    if mutating {
        return RouteDecision {
            tier: RoutingTier::Standard,
            profile: Some("implementer"),
            confidence: 78,
            auto_dispatch_read_only: false,
            reason: "ordinary implementation belongs on the GLM-5 standard tier",
        };
    }
    RouteDecision {
        tier: RoutingTier::Standard,
        profile: None,
        confidence: 70,
        auto_dispatch_read_only: false,
        reason: "keep ambiguous work on the standard tier and decompose before escalating",
    }
}

fn worker(
    profile: &'static str,
    confidence: u8,
    auto_dispatch_read_only: bool,
    reason: &'static str,
) -> RouteDecision {
    RouteDecision {
        tier: RoutingTier::Worker,
        profile: Some(profile),
        confidence,
        auto_dispatch_read_only,
        reason,
    }
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_location_is_auto_dispatched_but_find_and_fix_is_not() {
        let locate = classify("定位用户权限在哪个文件", true);
        assert_eq!(locate.profile, Some("scout"));
        assert!(locate.auto_dispatch_read_only);

        let fix = classify("定位并修复用户权限检查", true);
        assert_eq!(fix.profile, Some("implementer"));
        assert!(!fix.auto_dispatch_read_only);
    }

    #[test]
    fn failing_tests_route_to_verified_worker() {
        let route = classify("测试失败，请修复断言", true);
        assert_eq!(route.profile, Some("test_fixer"));
        assert_eq!(route.tier, RoutingTier::Worker);
        assert!(!route.auto_dispatch_read_only);
    }

    #[test]
    fn repository_audit_routes_to_the_reader_worker() {
        let route = classify("扫一下这个仓库，解释当前架构", true);
        assert_eq!(route.tier, RoutingTier::Worker);
        assert_eq!(route.profile, Some("reader"));
        assert!(route.auto_dispatch_read_only);
    }

    #[test]
    fn explaining_a_test_failure_stays_read_only() {
        let route = classify("解释这个测试失败的原因", true);
        assert_eq!(route.profile, Some("log_inspector"));
        assert!(route.auto_dispatch_read_only);
    }

    #[test]
    fn deep_needs_runtime_evidence_ticket_and_budget() {
        let guard = RoutingGuard::new(RoutingPolicy::default());
        let ticket = EscalationTicket {
            reason: "cross-module invariants still conflict".to_owned(),
            attempted_profiles: vec!["reader".to_owned()],
            context_evidence: "twenty modules remain coupled after slicing".to_owned(),
            why_not_decompose: "the same invariant must be proven across every module".to_owned(),
        };
        assert!(guard.authorize_deep(Some(&ticket)).is_err());
        guard.record_profile_attempt("reader");
        assert!(guard.authorize_deep(Some(&ticket)).is_ok());
        assert!(guard.authorize_deep(Some(&ticket)).is_err());
    }
}
