//! The static trade table: window tiers, payload caps, the relay's hosted
//! job-prompt models, and every profile the catalog ships with. Pure data
//! construction — nothing here dispatches, runs or judges anything.

use std::sync::Arc;

use crate::provider::Provider;
use crate::subagent_worktree::SubagentWorktreePolicy;

use super::types::{SubagentProfile, SubagentShell, SubagentWriteScope};

/// Attempts a verified run makes before it gives up and reports failure.
pub(super) const DEFAULT_MAX_ATTEMPTS: usize = 3;

/// Context window tiers. A cheap model with a 128K window configured is not
/// the same thing as a cheap model that *works* at 128K: the discipline that
/// makes small models succeed is a small average context, and the only place
/// to enforce it is here. `deep` is the deliberate exception — it runs on the
/// parent model precisely because its work does not fit in a small window.
pub const WORKER_WINDOW_STANDARD: u64 = 32_768;
pub const WORKER_WINDOW_BALANCED: u64 = 49_152;
pub const WORKER_WINDOW_WIDE: u64 = 65_536;
pub const STANDARD_WINDOW: u64 = 262_144;

/// Tool payload caps per tier, in bytes. One 128 KB test log would consume a
/// 32K window several times over, so the cap is part of the tier, not a knob.
const PAYLOAD_LIMIT_STANDARD: usize = 4 * 1024;
const PAYLOAD_LIMIT_BALANCED: usize = 5 * 1024;
const PAYLOAD_LIMIT_WIDE: usize = 6 * 1024;
const PAYLOAD_LIMIT_IMPLEMENTER: usize = 16 * 1024;

/// Prefix of the some.im virtual models that used to host trade job prompts.
/// `someim-32b` is a *tier* name, not a context promise: the model behind it
/// is the same cheap model, and the small-window discipline stays on this
/// side of the wire.
pub const HOSTED_WORKER_MODEL_PREFIX: &str = "someim-32b";

/// The hosted model a trade runs on, or `None` when the trade has no default
/// binding on this gateway.
///
/// Every trade now resolves to the shared base model rather than its own
/// `someim-32b-<trade>` chain. Those per-trade chains existed to prepend a job
/// prompt server-side, but the job prompt travels with the request from
/// [`builtin_profiles`] — keeping both means the worker gets its instructions
/// twice, and it means every new responsibility needs a new chain provisioned
/// on the relay before it can ship.
///
/// Mirrors macOS Xedit's `AgentSubagentModelCompatibility.recommendedModel` —
/// same gateway, same accounts, so the two clients must resolve the same trade
/// to the same model or the same operator gets two different workers depending
/// on which app they opened.
pub fn hosted_worker_model(id: &str) -> Option<String> {
    // `generalist` 合并了 scout / reader / log_inspector / git_detective 四个
    // 基础档窄工种，它在这个网关上的默认自然也是基础档。想要更强的，走
    // WorkerTier::Expert——那一档要票据。
    let base = crate::worker_tier::LEGACY_HOSTED_TRADES.contains(&id) || id == "generalist";
    base.then(|| crate::worker_tier::HOSTED_BASE_MODEL.to_owned())
}

/// Every profile the catalog ships with.
///
/// One provider is enough now: each responsibility declares its own context
/// budget, and the session window belongs to the expert tier — which the
/// catalog receives separately through
/// [`SubagentCatalog::with_tier_binding`](super::catalog::SubagentCatalog::with_tier_binding)
/// because reaching it costs a ticket.
pub fn builtin_profiles(worker: Arc<dyn Provider>) -> Vec<SubagentProfile> {
    let cheap = worker;
    vec![
        profile(
            cheap.clone(),
            ProfileSpec {
                id: "scout",
                shell: SubagentShell::None,
                purpose: "Locate files, symbols and call sites quickly; no shell or writes.",
                tools: &["search_files", "grep_files", "list_directory", "read_file"],
                prompt: "Your trade is LOCATION. Report exact paths and line numbers; do not redesign.",
                max_turns: 8,
                context_window: WORKER_WINDOW_STANDARD,
                tool_output_limit: Some(PAYLOAD_LIMIT_STANDARD),
                write_scope: SubagentWriteScope::None,
                timeout_seconds: 300,
                worktree: SubagentWorktreePolicy::Shared,
            },
        ),
        profile(
            cheap.clone(),
            ProfileSpec {
                id: "reader",
                shell: SubagentShell::None,
                purpose: "Read and summarize long files or documentation; no shell or writes.",
                tools: &["read_file", "list_directory", "search_files"],
                prompt: "Your trade is READING. Answer with specific evidence and say which parts you read.",
                max_turns: 8,
                context_window: WORKER_WINDOW_BALANCED,
                tool_output_limit: Some(PAYLOAD_LIMIT_BALANCED),
                write_scope: SubagentWriteScope::None,
                timeout_seconds: 300,
                worktree: SubagentWorktreePolicy::Shared,
            },
        ),
        profile(
            cheap.clone(),
            ProfileSpec {
                id: "log_inspector",
                shell: SubagentShell::None,
                purpose: "Explain a failure log or error output and classify the cause; no shell or writes.",
                tools: &["read_file"],
                prompt: "Your trade is READING FAILURE OUTPUT. Quote the failing assertion or error verbatim — never paraphrase it — then name the single most likely cause and the file it lives in. If the output does not support a conclusion, say so instead of guessing.",
                max_turns: 4,
                context_window: WORKER_WINDOW_STANDARD,
                tool_output_limit: Some(PAYLOAD_LIMIT_STANDARD),
                write_scope: SubagentWriteScope::None,
                timeout_seconds: 300,
                worktree: SubagentWorktreePolicy::Shared,
            },
        ),
        profile(
            cheap.clone(),
            ProfileSpec {
                id: "git_detective",
                shell: SubagentShell::Reviewed,
                purpose: "Find when and where a regression was introduced from repository history; read-only.",
                tools: &[
                    "git_log",
                    "git_diff",
                    "git_blame",
                    "git_status",
                    "read_file",
                    "run_command",
                ],
                prompt: "Your trade is REGRESSION ARCHAEOLOGY. Prefer read-only `git` commands — `git log -p`, `git show <sha>`, `git diff <a> <b>`, `git bisect` inspection. Work backwards from the symptom through history and report exact commits, dates and hunks. Name the commit you believe introduced the change, and say plainly when the evidence does not single one out.",
                max_turns: 8,
                context_window: WORKER_WINDOW_STANDARD,
                tool_output_limit: Some(PAYLOAD_LIMIT_STANDARD),
                write_scope: SubagentWriteScope::None,
                timeout_seconds: 300,
                worktree: SubagentWorktreePolicy::Shared,
            },
        ),
        profile(
            cheap.clone(),
            ProfileSpec {
                // 旧名 `deep`：它当年既表示「复杂调查」这个职责，也表示
                // 「用最贵的模型 + 整个会话窗口」这个档位。正交化之后职责留在
                // 这里并**默认走便宜档**——贵模型只能经 WorkerTier::Expert 拿到，
                // 而那一档要票据。否则改个名字就等于白送一次升档。
                id: "generalist",
                shell: SubagentShell::Reviewed,
                purpose: "Complex investigation across files and repository state.",
                tools: &[
                    "search_files",
                    "grep_files",
                    "read_file",
                    "list_directory",
                    "git_status",
                    "run_command",
                ],
                prompt: "Your trade is INVESTIGATION. Follow evidence across files and state what you could not confirm.",
                max_turns: 12,
                // 默认基础档预算。整个会话窗口是专家档兑现的东西，不是这个
                // 职责与生俱来的——否则「调查」这个名字就等于一张免费的贵模型券。
                context_window: crate::WorkerTier::Standard.context_budget(),
                tool_output_limit: Some(PAYLOAD_LIMIT_WIDE),
                write_scope: SubagentWriteScope::None,
                timeout_seconds: 300,
                worktree: SubagentWorktreePolicy::Shared,
            },
        ),
        profile(
            cheap.clone(),
            ProfileSpec {
                id: "editor",
                shell: SubagentShell::None,
                purpose: "Edit exactly one separately approved target_file.",
                tools: &["read_file", "edit_file"],
                prompt: "Your trade is EDITING EXACTLY ONE FILE. Read it first, make a minimal exact edit, and touch no other path.",
                max_turns: 6,
                context_window: WORKER_WINDOW_BALANCED,
                tool_output_limit: Some(PAYLOAD_LIMIT_BALANCED),
                write_scope: SubagentWriteScope::SingleFile,
                timeout_seconds: 300,
                worktree: SubagentWorktreePolicy::Dedicated,
            },
        ),
        profile(
            cheap.clone(),
            ProfileSpec {
                id: "implementer",
                shell: SubagentShell::ReviewedVerifierOptional,
                purpose: "Implement a bounded multi-file change with a deployable 256K model.",
                tools: &[
                    "search_files",
                    "grep_files",
                    "list_directory",
                    "read_file",
                    "create_file",
                    "edit_file",
                    "run_command",
                ],
                prompt: "Your trade is BOUNDED IMPLEMENTATION. Complete the requested multi-file change inside the declared file set. Inspect neighboring code, preserve public behaviour outside the task, and use the verifier when one is supplied. You may create or edit only paths declared in task.write_files (legacy packets use task.relevant_files). Report changed files, verification and remaining uncertainty.",
                max_turns: 18,
                context_window: STANDARD_WINDOW,
                tool_output_limit: Some(PAYLOAD_LIMIT_IMPLEMENTER),
                write_scope: SubagentWriteScope::FileSet,
                timeout_seconds: 1_200,
                worktree: SubagentWorktreePolicy::Dedicated,
            },
        ),
        profile(
            cheap.clone(),
            ProfileSpec {
                id: "test_fixer",
                shell: SubagentShell::ReviewedVerifierOnly,
                purpose: "Drive failing tests back to green across the declared file set; needs a verifier command.",
                tools: &["read_file", "edit_file", "run_command"],
                prompt: "Your trade is MAKING A FAILING TEST PASS. The verifier command is the only judge — run it, read the real failure, fix the cause. Prefer fixing the implementation; change the test only when the test itself encodes the wrong expectation, and say so explicitly in your report. Never delete, skip or weaken a test to make it pass. You may edit only the files declared in your task packet.",
                max_turns: 8,
                context_window: WORKER_WINDOW_WIDE,
                tool_output_limit: Some(PAYLOAD_LIMIT_WIDE),
                write_scope: SubagentWriteScope::FileSet,
                timeout_seconds: 900,
                worktree: SubagentWorktreePolicy::Dedicated,
            },
        ),
        profile(
            cheap.clone(),
            ProfileSpec {
                id: "build_fixer",
                shell: SubagentShell::ReviewedVerifierOnly,
                purpose: "Fix compile, type and lint errors across the declared file set; needs a verifier command.",
                tools: &["read_file", "edit_file", "run_command"],
                prompt: "Your trade is MAKING THE BUILD PASS. Read the compiler or linter diagnostic literally: it usually names the file, line and expected type. Make the smallest change that satisfies it without changing behaviour, and never silence a diagnostic with a suppression unless your task packet asked for one. You may edit only the files declared in your task packet.",
                max_turns: 8,
                context_window: WORKER_WINDOW_BALANCED,
                tool_output_limit: Some(PAYLOAD_LIMIT_BALANCED),
                write_scope: SubagentWriteScope::FileSet,
                timeout_seconds: 900,
                worktree: SubagentWorktreePolicy::Dedicated,
            },
        ),
        profile(
            cheap.clone(),
            ProfileSpec {
                id: "tester",
                shell: SubagentShell::Reviewed,
                purpose: "Test and review behavior without modifying source files.",
                tools: &[
                    "search_files",
                    "grep_files",
                    "list_directory",
                    "read_file",
                    "git_status",
                    "git_diff",
                    "run_command",
                ],
                prompt: "Your trade is TESTING AND REVIEW. Reproduce claims with the narrowest relevant checks, inspect failures literally, and report defects with exact evidence. Do not edit source files. Commands that are not statically safe require contextual AI safety review.",
                max_turns: 18,
                context_window: WORKER_WINDOW_WIDE,
                tool_output_limit: Some(PAYLOAD_LIMIT_WIDE),
                write_scope: SubagentWriteScope::None,
                timeout_seconds: 900,
                worktree: SubagentWorktreePolicy::Shared,
            },
        ),
        profile(
            cheap.clone(),
            ProfileSpec {
                id: "ops_runner",
                shell: SubagentShell::Reviewed,
                purpose: "Run bounded operational commands with static, AI, and exact human safety gates.",
                tools: &[
                    "search_files",
                    "grep_files",
                    "list_directory",
                    "read_file",
                    "git_status",
                    "run_command",
                ],
                prompt: "Your trade is BOUNDED OPERATIONS. Inspect before acting and run only commands needed for the declared task. Static safe commands run directly; other non-destructive commands require AI review. Dangerous or credential-sensitive commands are never delegated to the judge. If review declines or is unavailable, return the exact command so the parent can request one-time human authorization through target_command.",
                max_turns: 32,
                context_window: WORKER_WINDOW_BALANCED,
                tool_output_limit: Some(PAYLOAD_LIMIT_BALANCED),
                write_scope: SubagentWriteScope::None,
                timeout_seconds: 1_200,
                worktree: SubagentWorktreePolicy::Shared,
            },
        ),
        profile(
            cheap,
            ProfileSpec {
                // 旧名 `judge`。改叫 reviewer 是为了与 Xedit 的五职责同名；
                // `judge` 与 `security_guard` 继续路由到这里。
                id: "reviewer",
                shell: SubagentShell::None,
                purpose: "Review correctness, risk, and policy boundaries without shell or writes.",
                tools: &[
                    "search_files",
                    "grep_files",
                    "list_directory",
                    "read_file",
                    "git_status",
                    "git_diff",
                ],
                prompt: "Your trade is INDEPENDENT JUDGMENT. Audit the proposed behavior and evidence, identify concrete correctness or safety risks, and distinguish proven facts from uncertainty. You have no shell and make no changes.",
                max_turns: 12,
                context_window: WORKER_WINDOW_BALANCED,
                tool_output_limit: Some(PAYLOAD_LIMIT_BALANCED),
                write_scope: SubagentWriteScope::None,
                timeout_seconds: 600,
                worktree: SubagentWorktreePolicy::Shared,
            },
        ),
    ]
}

struct ProfileSpec<'a> {
    id: &'a str,
    shell: SubagentShell,
    purpose: &'a str,
    tools: &'a [&'a str],
    prompt: &'a str,
    max_turns: usize,
    context_window: u64,
    tool_output_limit: Option<usize>,
    write_scope: SubagentWriteScope,
    worktree: SubagentWorktreePolicy,
    timeout_seconds: u64,
}

fn profile(provider: Arc<dyn Provider>, spec: ProfileSpec<'_>) -> SubagentProfile {
    SubagentProfile {
        id: spec.id.to_owned(),
        purpose: spec.purpose.to_owned(),
        model: None,
        provider,
        tool_names: spec.tools.iter().map(|value| (*value).to_owned()).collect(),
        capability_prompt: spec.prompt.to_owned(),
        max_turns: spec.max_turns,
        context_window: spec.context_window,
        token_budget: None,
        timeout_seconds: Some(spec.timeout_seconds),
        max_consecutive_failures: 3,
        write_scope: spec.write_scope,
        worktree: spec.worktree,
        tool_output_limit: spec.tool_output_limit,
        max_attempts: DEFAULT_MAX_ATTEMPTS,
        shell: spec.shell,
        hosted_job_prompt: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subagent::test_support::ReportProvider;

    /// The trade→model table is shared with the macOS app. One operator, one
    /// gateway: if the two clients disagree here, the same trade quietly runs
    /// on two different models depending on which app was opened.
    /// 七条 per-trade 模型链已经收敛到一个基础档。它们当年存在的理由是
    /// 服务端 prepend 职责提示词，而职责提示词现在随请求走——留着就是双重
    /// 注入，而且每加一种职责都得先在网关上铺一条链才能发版。
    #[test]
    fn every_hosted_trade_resolves_to_the_shared_base_model() {
        for trade in crate::worker_tier::LEGACY_HOSTED_TRADES {
            assert_eq!(
                hosted_worker_model(trade).as_deref(),
                Some("someim-32b"),
                "{trade} should resolve to the shared base tier"
            );
        }
        // generalist 合并了四个基础档窄工种，默认也落在基础档。
        assert_eq!(
            hosted_worker_model("generalist").as_deref(),
            Some("someim-32b")
        );
        // 没有默认绑定的工种仍然返回 None，而不是凭空造一个网关会拒绝的名字：
        // implementer 是日常编码主力，跑的是更强的那一档。
        assert_eq!(hosted_worker_model("implementer"), None);
        assert_eq!(hosted_worker_model("ops_runner"), None);
    }

    /// Deployable windows are asserted rather than left to drift: narrow
    /// trades stay within 32K-64K, implementer owns the 256K daily-coding
    /// tier, and `generalist` gets the 128K base budget. Nothing inherits the
    /// session window by default any more — that is what the expert tier
    /// cashes in, and it costs a ticket.
    #[test]
    fn workers_run_in_deployable_windows_with_capped_payloads() {
        let provider: Arc<dyn Provider> = Arc::new(ReportProvider);
        for profile in builtin_profiles(provider) {
            if profile.id == "generalist" {
                assert_eq!(
                    profile.context_window,
                    crate::WorkerTier::Standard.context_budget()
                );
                assert_eq!(profile.tool_output_limit, Some(PAYLOAD_LIMIT_WIDE));
                continue;
            }
            if profile.id == "implementer" {
                assert_eq!(profile.context_window, STANDARD_WINDOW);
                assert_eq!(profile.tool_output_limit, Some(PAYLOAD_LIMIT_IMPLEMENTER));
                continue;
            }
            assert!(
                profile.context_window >= WORKER_WINDOW_STANDARD
                    && profile.context_window <= WORKER_WINDOW_WIDE,
                "{} must run in a 32K-64K worker window, got {}",
                profile.id,
                profile.context_window
            );
            let limit = profile
                .tool_output_limit
                .unwrap_or_else(|| panic!("{} must cap its tool payloads", profile.id));
            assert!(
                (limit as u64) < profile.context_window,
                "{} caps payloads at {limit} bytes, which its {}-token window cannot absorb",
                profile.id,
                profile.context_window
            );
        }
    }
}
