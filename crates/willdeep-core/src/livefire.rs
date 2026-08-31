//! 实弹靶场：把小上下文工种放到真实缺陷上打一轮，量出它们到底行不行。
//!
//! 为什么要有它：`SKILL_WORKERS.md` 的三条纪律里，第一条是「越容易自动验证
//! 的任务，越可以大胆用弱模型」。这句话是个可证伪的断言，而在此之前仓库里
//! 没有任何一处能证伪它——单元测试用的都是桩 Provider，桩 Provider 永远
//! 「修得好」。这个模块用真 Provider、真缺陷、真 `cargo` 退出码跑一遍，产出
//! 的是数字，不是印象。
//!
//! 它**不是** CI 的一部分：默认 `#[ignore]`，需要真实凭据与网络，每跑一轮
//! 都花钱。入口是 `scripts/skill_worker_range.rb`。
//!
//! 一条纪律写在这里，免得后人手滑：靶场只在**专属 worktree** 里改代码，
//! 缺陷样本每次现建现扔，不复用仓库里的任何文件。
#![cfg(test)]

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;

use crate::agent::{AgentEvent, EventSink};
use crate::background::BackgroundTaskRegistry;
use crate::provider::{ApiDialect, Provider, ProviderConfig, ProviderKind, build_provider};
use crate::subagent::{
    SpawnAgentArgs, SubagentCatalog, TaskPacket, TaskVerifier, builtin_profiles,
};

/// 一个缺陷样本。每个样本都必须满足：**派工前 verifier 必须是红的**，
/// 否则这一轮测的不是模型，是运气。
struct RangeCase {
    id: &'static str,
    profile: &'static str,
    goal: &'static str,
    known_facts: &'static [&'static str],
    constraints: &'static [&'static str],
    /// `None` for report-only trades: they have no exit code to judge them,
    /// and pretending otherwise is how a metric starts lying.
    verifier: Option<&'static str>,
    lib_rs: &'static str,
    /// Extra files seeded alongside `src/lib.rs`, for cases where finding the
    /// right file among several is the whole task.
    extra_files: &'static [(&'static str, &'static str)],
    /// Substrings the report must contain to count as a correct answer.
    /// Citation checking asks "does this place exist"; this asks "is it the
    /// right place" — a report can be perfectly grounded and still wrong.
    expect_contains: &'static [&'static str],
    /// Seed a three-commit history and ask which commit introduced the bug.
    git_history: bool,
}

impl RangeCase {
    /// 样本铺下的全部源文件。给 Task Packet 当读写集用。
    ///
    /// 注意这里**给的是文件清单，不是答案**：跨文件样本里哪个文件有 bug 仍要
    /// 工种自己顺着调用链找。把清单藏起来只会把「找 bug」换成「猜文件名」，
    /// 那是另一种能力，而且测不出可复现的东西。
    fn source_files(&self) -> Vec<String> {
        let mut files = vec!["src/lib.rs".to_owned()];
        files.extend(
            self.extra_files
                .iter()
                .map(|(path, _)| (*path).to_owned())
                .filter(|path| path.ends_with(".rs")),
        );
        files
    }
}

const PROMPT_TEST: &str = "把失败的测试修到绿。改实现，不要改测试，更不要删测试或加 #[ignore]。";
const PROMPT_BUILD: &str = "把编译错误修掉，让 cargo build 通过。只做最小改动。";
const PROMPT_REPORT: &str = "回答问题并给出确切位置。只报你亲眼看到的东西，找不到就直说，不要猜。";

const CASES: &[RangeCase] = &[
    RangeCase {
        id: "test_off_by_one",
        profile: "test_fixer",
        goal: "修复 sum_to 的求和结果少算一项",
        known_facts: &["测试期望 sum_to(4) == 10", "实际返回 6"],
        constraints: &["不改测试，不改函数签名"],
        verifier: Some("cargo test --quiet"),
        lib_rs: r#"
pub fn sum_to(n: u32) -> u32 {
    let mut total = 0;
    for value in 1..n {
        total += value;
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_every_value_up_to_and_including_n() {
        assert_eq!(sum_to(4), 10);
        assert_eq!(sum_to(1), 1);
        assert_eq!(sum_to(0), 0);
    }
}
"#,
        extra_files: &[],
        expect_contains: &[],
        git_history: false,
    },
    RangeCase {
        id: "test_wrong_comparison",
        profile: "test_fixer",
        goal: "修复 max_of 返回的是最小值",
        known_facts: &["测试期望 max_of(&[1, 9, 3]) == Some(9)", "实际得到 Some(1)"],
        constraints: &["不改测试"],
        verifier: Some("cargo test --quiet"),
        lib_rs: r#"
pub fn max_of(values: &[i64]) -> Option<i64> {
    let mut best = *values.first()?;
    for value in values {
        if *value < best {
            best = *value;
        }
    }
    Some(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_the_largest_value() {
        assert_eq!(max_of(&[1, 9, 3]), Some(9));
        assert_eq!(max_of(&[-5, -2]), Some(-2));
        assert_eq!(max_of(&[]), None);
    }
}
"#,
        extra_files: &[],
        expect_contains: &[],
        git_history: false,
    },
    RangeCase {
        id: "test_empty_input_panics",
        profile: "test_fixer",
        goal: "first_word 在空输入上 panic，应当返回 None",
        known_facts: &["测试断言 first_word(\"\") == None", "当前实现直接索引 [0]"],
        constraints: &["不改测试，保持返回类型 Option<&str>"],
        verifier: Some("cargo test --quiet"),
        lib_rs: r#"
pub fn first_word(text: &str) -> Option<&str> {
    let words: Vec<&str> = text.split_whitespace().collect();
    Some(words[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_has_no_first_word() {
        assert_eq!(first_word("hello world"), Some("hello"));
        assert_eq!(first_word("   "), None);
        assert_eq!(first_word(""), None);
    }
}
"#,
        extra_files: &[],
        expect_contains: &[],
        git_history: false,
    },
    RangeCase {
        id: "test_leap_year_rule",
        profile: "test_fixer",
        goal: "闰年判定漏掉 400 年规则",
        known_facts: &["测试期望 is_leap_year(1900) == false 且 is_leap_year(2000) == true"],
        constraints: &["不改测试"],
        verifier: Some("cargo test --quiet"),
        lib_rs: r#"
pub fn is_leap_year(year: u32) -> bool {
    year % 4 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_the_gregorian_rule() {
        assert!(is_leap_year(2024));
        assert!(is_leap_year(2000));
        assert!(!is_leap_year(1900));
        assert!(!is_leap_year(2023));
    }
}
"#,
        extra_files: &[],
        expect_contains: &[],
        git_history: false,
    },
    RangeCase {
        id: "test_normalize_order",
        profile: "test_fixer",
        goal: "normalize 忘了折叠内部连续空白",
        known_facts: &["测试期望 normalize(\"  Hello   World \") == \"hello world\""],
        constraints: &["不改测试"],
        verifier: Some("cargo test --quiet"),
        lib_rs: r#"
pub fn normalize(text: &str) -> String {
    text.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_inner_whitespace_and_lowercases() {
        assert_eq!(normalize("  Hello   World "), "hello world");
        assert_eq!(normalize("A\tB"), "a b");
    }
}
"#,
        extra_files: &[],
        expect_contains: &[],
        git_history: false,
    },
    RangeCase {
        id: "build_missing_mut",
        profile: "build_fixer",
        goal: "修复 counter 缺少 mut 导致的编译错误",
        known_facts: &["rustc 报 cannot borrow `total` as mutable"],
        constraints: &["最小改动"],
        verifier: Some("cargo build --quiet"),
        lib_rs: r#"
pub fn count_words(text: &str) -> usize {
    let total = 0;
    for _ in text.split_whitespace() {
        total += 1;
    }
    total
}
"#,
        extra_files: &[],
        expect_contains: &[],
        git_history: false,
    },
    RangeCase {
        id: "build_type_mismatch",
        profile: "build_fixer",
        goal: "修复返回类型与实际值不符的编译错误",
        known_facts: &["函数声明返回 usize，实际返回 i32 表达式"],
        constraints: &["保持函数签名不变"],
        verifier: Some("cargo build --quiet"),
        lib_rs: r#"
pub fn double_len(text: &str) -> usize {
    let len: i32 = text.len() as i32;
    len * 2
}
"#,
        extra_files: &[],
        expect_contains: &[],
        git_history: false,
    },
    RangeCase {
        id: "build_non_exhaustive_match",
        profile: "build_fixer",
        goal: "补齐 match 的缺失分支",
        known_facts: &["rustc 报 non-exhaustive patterns: `Level::Error` not covered"],
        constraints: &["不要用 `_ =>` 通配分支糊过去，逐个列出"],
        verifier: Some("cargo build --quiet"),
        lib_rs: r#"
pub enum Level {
    Info,
    Warn,
    Error,
}

pub fn label(level: &Level) -> &'static str {
    match level {
        Level::Info => "info",
        Level::Warn => "warn",
    }
}
"#,
        extra_files: &[],
        expect_contains: &[],
        git_history: false,
    },
    RangeCase {
        id: "build_use_after_move",
        profile: "build_fixer",
        goal: "修复 move 之后继续使用的借用错误",
        known_facts: &["rustc 报 borrow of moved value: `name`"],
        constraints: &["保持函数签名不变"],
        verifier: Some("cargo build --quiet"),
        lib_rs: r#"
pub fn greet(name: String) -> (String, usize) {
    let greeting = format!("hello {}", name);
    let owned = name;
    (greeting, name.len())
}
"#,
        extra_files: &[],
        expect_contains: &[],
        git_history: false,
    },
    RangeCase {
        id: "build_missing_import",
        profile: "build_fixer",
        goal: "补上缺失的 HashMap 导入",
        known_facts: &["rustc 报 cannot find type `HashMap` in this scope"],
        constraints: &["最小改动"],
        verifier: Some("cargo build --quiet"),
        lib_rs: r#"
pub fn tally(words: &[&str]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for word in words {
        *counts.entry(word.to_string()).or_insert(0) += 1;
    }
    counts
}
"#,
        extra_files: &[],
        expect_contains: &[],
        git_history: false,
    },
    // 只读工种的样本：没有退出码可判，衡量的是「它说的地方存不存在」
    // 与「是不是那个地方」。两件事都要，前者防编造，后者防答非所问。
    RangeCase {
        id: "scout_finds_the_symbol",
        profile: "scout",
        goal: "找出 retry_budget 这个函数定义在哪个文件的哪一行",
        known_facts: &["仓库里有多个模块，只有一个定义了它"],
        constraints: &["报告要给出路径和行号"],
        verifier: None,
        lib_rs: r#"
pub mod config;
pub mod net;
pub mod util;
"#,
        extra_files: &[
            (
                "src/config.rs",
                "pub fn load() -> u32 {\n    7\n}\n\npub fn timeout_seconds() -> u32 {\n    30\n}\n",
            ),
            (
                "src/net.rs",
                "use crate::util;\n\npub fn send() -> u32 {\n    util::retry_budget()\n}\n",
            ),
            (
                "src/util.rs",
                "/// How many retries a request gets.\npub fn retry_budget() -> u32 {\n    3\n}\n",
            ),
        ],
        expect_contains: &["src/util.rs"],
        git_history: false,
    },
    RangeCase {
        id: "git_detective_finds_the_commit",
        profile: "git_detective",
        goal: "找出哪个 commit 把 retry_budget 的返回值从 3 改成了 0",
        known_facts: &["仓库有三个 commit", "当前 retry_budget 返回 0"],
        constraints: &["报告要给出确切的 commit 哈希"],
        verifier: None,
        lib_rs: r#"
pub fn retry_budget() -> u32 {
    0
}
"#,
        extra_files: &[],
        expect_contains: &["retry"],
        git_history: true,
    },
    // ——— 难样本 ———
    //
    // 上面那批的共同形状是「测试点名的函数就是有 bug 的函数」，一跳可达。
    // 下面这批把 bug 挪到调用链深处、挪到另一个文件里：失败的断言和该改的
    // 那一行之间隔着两到三次跳转。加它们的理由很直接——一个永远 100% 的指标
    // 和没有指标一样没用，得让它有机会掉下来。
    RangeCase {
        id: "test_three_hop_call_chain",
        profile: "test_fixer",
        goal: "pipeline::run 没有把连续空白折叠成一个空格",
        known_facts: &[
            "测试期望 run(\"  hello   world  \") == \"hello world\"",
            "实际得到 \"hello   world\"",
        ],
        constraints: &["不改测试", "不改任何函数签名"],
        verifier: Some("cargo test --quiet"),
        lib_rs: r#"
pub mod pipeline;
pub mod stage;
pub mod text;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_runs_of_whitespace() {
        assert_eq!(pipeline::run("  hello   world  "), "hello world");
        assert_eq!(pipeline::run("a"), "a");
    }
}
"#,
        extra_files: &[
            (
                "src/pipeline.rs",
                "use crate::stage;\n\npub fn run(input: &str) -> String {\n    stage::normalize(input)\n}\n",
            ),
            (
                "src/stage.rs",
                "use crate::text;\n\npub fn normalize(input: &str) -> String {\n    text::squeeze(input.trim())\n}\n",
            ),
            (
                "src/text.rs",
                "/// Collapse every run of whitespace into a single space.\npub fn squeeze(input: &str) -> String {\n    input.to_owned()\n}\n",
            ),
        ],
        expect_contains: &[],
        git_history: false,
    },
    // 两个调用方共用一个有 bug 的辅助函数，两条断言各压一个调用方。
    // 只在其中一个调用方上打补丁能过一条、挂另一条：想两条都过，要么改到
    // 共用的那一层，要么两个调用方都改对——两条路都得先看懂谁调了谁。
    RangeCase {
        id: "test_shared_helper_two_callers",
        profile: "test_fixer",
        goal: "折扣计算按截断取整，应当四舍五入到分",
        known_facts: &[
            "测试期望 cart::total(&[1995], 12) == 1756，实际 1755",
            "测试期望 invoice::line_total(333, 10) == 300，实际 299",
        ],
        constraints: &["不改测试", "不改任何函数签名"],
        verifier: Some("cargo test --quiet"),
        lib_rs: r#"
pub mod cart;
pub mod invoice;
pub mod money;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discounts_round_to_the_nearest_cent() {
        assert_eq!(cart::total(&[1995], 12), 1756);
        assert_eq!(invoice::line_total(333, 10), 300);
    }
}
"#,
        extra_files: &[
            (
                "src/cart.rs",
                "use crate::money;\n\npub fn total(line_cents: &[u64], percent_off: u64) -> u64 {\n    line_cents\n        .iter()\n        .map(|cents| money::apply_discount(*cents, percent_off))\n        .sum()\n}\n",
            ),
            (
                "src/invoice.rs",
                "use crate::money;\n\npub fn line_total(cents: u64, percent_off: u64) -> u64 {\n    money::apply_discount(cents, percent_off)\n}\n",
            ),
            (
                "src/money.rs",
                "/// Apply a percentage discount, rounding to the nearest cent.\npub fn apply_discount(cents: u64, percent_off: u64) -> u64 {\n    cents * (100 - percent_off) / 100\n}\n",
            ),
        ],
        expect_contains: &[],
        git_history: false,
    },
    // 编译错误报在 render.rs，而该补的东西在 event.rs：报错的地方不是该改的
    // 地方。上面那批 build 样本全是「错在哪报在哪」，一行就能修。
    RangeCase {
        id: "build_missing_impl_elsewhere",
        profile: "build_fixer",
        goal: "补上缺失的 trait 实现让 cargo build 通过",
        known_facts: &["报错出现在 src/render.rs", "Describe 定义在 src/event.rs"],
        constraints: &["不改 render.rs 里的调用方式", "不删代码"],
        verifier: Some("cargo build --quiet"),
        lib_rs: r#"
pub mod event;
pub mod render;
"#,
        extra_files: &[
            (
                "src/event.rs",
                "pub trait Describe {\n    fn describe(&self) -> String;\n}\n\npub struct Started;\npub struct Finished;\n\nimpl Describe for Started {\n    fn describe(&self) -> String {\n        \"started\".to_owned()\n    }\n}\n",
            ),
            (
                "src/render.rs",
                "use crate::event::{Describe, Finished, Started};\n\npub fn render_all() -> Vec<String> {\n    vec![Started.describe(), Finished.describe()]\n}\n",
            ),
        ],
        expect_contains: &[],
        git_history: false,
    },
    // 只读工种的难样本：仓库里有两个同名定义，grep 直接给出两个命中，
    // 而正确答案取决于 send() 实际 use 的是哪一个。
    RangeCase {
        id: "scout_picks_the_called_definition",
        profile: "scout",
        goal: "net::send 实际调用的 retry_budget 定义在哪个文件",
        known_facts: &["仓库里有两处同名定义，只有一处被真正调用"],
        constraints: &["报告要给出路径", "说明判断依据"],
        verifier: None,
        lib_rs: r#"
pub mod legacy;
pub mod net;
pub mod util;
"#,
        extra_files: &[
            (
                "src/legacy.rs",
                "/// Old budget, kept for the migration. Nothing calls this any more.\npub fn retry_budget() -> u32 {\n    9\n}\n",
            ),
            (
                "src/net.rs",
                "use crate::util;\n\npub fn send() -> u32 {\n    util::retry_budget()\n}\n",
            ),
            ("src/util.rs", "pub fn retry_budget() -> u32 {\n    3\n}\n"),
        ],
        expect_contains: &["src/util.rs"],
        git_history: false,
    },
];

#[derive(Default)]
struct RangeSink {
    verdicts: Mutex<Vec<(Option<bool>, usize)>>,
    claims: Mutex<(usize, usize)>,
    tokens: Mutex<(u64, u64)>,
    tool_calls: Mutex<HashMap<String, usize>>,
}

#[async_trait]
impl EventSink for RangeSink {
    async fn emit(&self, event: AgentEvent) {
        match event {
            AgentEvent::SubagentVerdict {
                verifier_passed,
                attempts,
                claims_checked,
                claims_unverifiable,
                ..
            } => {
                self.verdicts
                    .lock()
                    .expect("range verdicts")
                    .push((verifier_passed, attempts));
                *self.claims.lock().expect("range claims") = (claims_checked, claims_unverifiable);
            }
            AgentEvent::SubagentUsage { usage, .. } => {
                let mut totals = self.tokens.lock().expect("range tokens");
                totals.0 += usage.input_tokens.unwrap_or(0);
                totals.1 += usage.output_tokens.unwrap_or(0);
            }
            AgentEvent::SubagentToolCompleted { name, is_error, .. } => {
                // 工具调用的成败是诊断的全部：一个「跑满轮次却什么都没改」的
                // Worker，究竟是没想改，还是每次 edit 都被打回来，只有这一列
                // 分得清。
                let key = if is_error {
                    format!("{name}!error")
                } else {
                    name
                };
                *self
                    .tool_calls
                    .lock()
                    .expect("range tool calls")
                    .entry(key)
                    .or_default() += 1;
            }
            _ => {}
        }
    }
}

/// 把每一轮的工具调用与工具回执抄下来。
///
/// 没有它，一次「跑满轮次却一个字都没改」的失败只能看到一个计数；有了它，
/// 能看到 Worker 到底发了什么参数、Runtime 又回了什么错。靶场的价值一半在
/// 判定，一半在这份逐字记录。
struct TracingProvider {
    inner: Arc<dyn Provider>,
    log: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Provider for TracingProvider {
    fn with_model(&self, model: &str) -> Result<Arc<dyn Provider>, crate::provider::ProviderError> {
        Ok(Arc::new(Self {
            inner: self.inner.with_model(model)?,
            log: self.log.clone(),
        }))
    }

    async fn complete(
        &self,
        messages: &[crate::types::Message],
        tools: &[crate::types::ToolDefinition],
    ) -> Result<crate::types::Completion, crate::provider::ProviderError> {
        if let Some(last) = messages.last() {
            self.log.lock().expect("range trace").push(format!(
                "<- {:?}: {}",
                last.role,
                last.content.chars().take(600).collect::<String>()
            ));
        }
        let completion = self.inner.complete(messages, tools).await?;
        let mut log = self.log.lock().expect("range trace");
        for call in &completion.tool_calls {
            log.push(format!(
                "-> tool {} {}",
                call.name,
                call.arguments.chars().take(600).collect::<String>()
            ));
        }
        if !completion.content.trim().is_empty() {
            log.push(format!(
                "-> text {}",
                completion.content.chars().take(600).collect::<String>()
            ));
        }
        Ok(completion)
    }
}

fn cargo_manifest(id: &str) -> String {
    format!(
        "[package]\nname = \"range_{id}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n"
    )
}

/// 建一个独立的 Git 仓库当靶子。必须是仓库：专属 worktree 与 `repo_commit`
/// 锚点都从这里来。
fn seed_fixture(root: &Path, case: &RangeCase) -> std::io::Result<Option<String>> {
    std::fs::create_dir_all(root.join("src"))?;
    std::fs::write(root.join("Cargo.toml"), cargo_manifest(case.id))?;
    std::fs::write(root.join(".gitignore"), "target\n")?;
    for (path, body) in case.extra_files {
        if let Some(parent) = root.join(path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(root.join(path), body)?;
    }
    let commit = |message: &str| -> std::io::Result<()> {
        for args in [
            vec!["add", "."],
            vec![
                "-c",
                "user.name=range",
                "-c",
                "user.email=range@local",
                "commit",
                "--quiet",
                "-m",
                message,
            ],
        ] {
            let status = std::process::Command::new("git")
                .args(&args)
                .current_dir(root)
                .status()?;
            assert!(
                status.success(),
                "git {args:?} failed in {}",
                root.display()
            );
        }
        Ok(())
    };
    let status = std::process::Command::new("git")
        .args(["init", "--quiet", "--initial-branch=main"])
        .current_dir(root)
        .status()?;
    assert!(status.success(), "git init failed in {}", root.display());

    if !case.git_history {
        std::fs::write(root.join("src/lib.rs"), case.lib_rs.trim_start())?;
        commit("seed")?;
        return Ok(None);
    }

    // 三个 commit，中间那个才是真凶：只有一个 commit 的历史里，
    // 「找出哪个 commit」这个问题根本无法答错，也就测不出任何东西。
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn retry_budget() -> u32 {\n    3\n}\n",
    )?;
    commit("add retry budget")?;
    std::fs::write(root.join("src/lib.rs"), case.lib_rs.trim_start())?;
    commit("tune retry budget")?;
    let culprit = String::from_utf8(
        std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(root)
            .output()?
            .stdout,
    )
    .expect("utf8")
    .trim()
    .to_owned();
    std::fs::write(root.join("README.md"), "# range fixture\n")?;
    commit("document the crate")?;
    Ok(Some(culprit))
}

/// 跑一次 verifier，拿退出码。派工前用它确认靶子确实是红的。
fn verifier_passes(root: &Path, command: &str) -> bool {
    std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(root)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Worker 改完之后，测试块还是不是原来那一段。
///
/// 「把测试删了让它变绿」是弱模型最省力的通关方式，而 verifier 的退出码
/// 对此毫无察觉——退出码只知道绿了，不知道绿得干不干净。所以这一项单独查，
/// 并且**作弊即判失败**，不进成功率的分子。
fn test_block_intact(worktree: &Path, case: &RangeCase) -> bool {
    let Some(original) = case.lib_rs.split("#[cfg(test)]").nth(1) else {
        return true;
    };
    let Ok(current) = std::fs::read_to_string(worktree.join("src/lib.rs")) else {
        return false;
    };
    let Some(current_tests) = current.split("#[cfg(test)]").nth(1) else {
        return false;
    };
    normalize_ws(current_tests) == normalize_ws(original)
}

fn normalize_ws(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 专属 worktree 的落点。每个样本一个 worktree 根，跑完只会有一个子目录。
fn only_worktree(root: &Path) -> Option<PathBuf> {
    let mut dirs = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    dirs.sort();
    dirs.pop()
}

fn env_or_skip(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

/// 预检：每个带 verifier 的样本，派工**之前**必须是红的。
///
/// 绿着的靶子测不出任何东西——它会以 100% 通过的姿态进分子，把成绩往上抬，
/// 而没人看得出来。付费那一轮虽然也会在 `mis_seeded` 里点名，但那时钱已经
/// 花完了。这个测试不联网、不调 Provider，只是把每个样本铺出来跑一遍它自己
/// 的 verifier，加样本之后跑一次即可。
///
/// 仍标 `#[ignore]`：它要为每个样本起一次 cargo，几分钟起步，不该拖住常规测试。
#[test]
#[ignore = "preflight: seeds every fixture and runs its verifier; minutes, no network. Run after adding samples."]
fn every_verifiable_sample_starts_red() {
    let root = std::env::temp_dir().join(format!("willdeep-preflight-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("preflight root");

    let mut green = Vec::new();
    for case in CASES {
        let Some(verifier) = case.verifier else {
            continue;
        };
        let fixture = root.join(case.id);
        std::fs::create_dir_all(&fixture).expect("fixture dir");
        seed_fixture(&fixture, case).expect("seed fixture");
        if verifier_passes(&fixture, verifier) {
            green.push(case.id);
        }
    }
    let _ = std::fs::remove_dir_all(&root);

    assert!(
        green.is_empty(),
        "这些样本派工前就是绿的，测不出任何东西：{green:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live fire: needs real provider credentials and spends money; run scripts/skill_worker_range.rb"]
async fn skill_worker_range() {
    let (Some(base_url), Some(api_key)) = (
        env_or_skip("WILLDEEP_RANGE_API_BASE"),
        env_or_skip("WILLDEEP_RANGE_API_KEY"),
    ) else {
        panic!(
            "set WILLDEEP_RANGE_API_BASE and WILLDEEP_RANGE_API_KEY (scripts/skill_worker_range.rb reads them from ~/.willdeep/config.toml)"
        );
    };
    let model = env_or_skip("WILLDEEP_RANGE_MODEL").unwrap_or_else(|| "glm-5".to_owned());
    let only = env_or_skip("WILLDEEP_RANGE_CASES");
    let selected = |id: &str| {
        only.as_ref()
            .is_none_or(|filter| filter.split(',').any(|want| want.trim() == id))
    };

    let mut config = ProviderConfig::new(
        ProviderKind::infer(&base_url),
        ApiDialect::ChatCompletions,
        base_url,
        api_key,
        model.clone(),
    );
    config.max_output_tokens = 32_768;
    let provider: Arc<dyn Provider> = build_provider(config).expect("build range provider");

    let range_root = std::env::temp_dir().join(format!("willdeep-range-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&range_root).expect("range root");
    let out = env_or_skip("WILLDEEP_RANGE_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| range_root.join("range.json"));
    let trace_dir = out.parent().unwrap_or(&range_root).join("traces");
    std::fs::create_dir_all(&trace_dir).expect("trace directory");
    let mut results = Vec::new();

    for case in CASES.iter().filter(|case| selected(case.id)) {
        let fixture = range_root.join(case.id);
        let worktrees = range_root.join(format!("{}-worktrees", case.id));
        std::fs::create_dir_all(&worktrees).expect("worktree root");
        let culprit_commit = seed_fixture(&fixture, case).expect("seed fixture");
        // macOS 的 /var 是指向 /private/var 的符号链接：worktree 管理器拿到的
        // 是解析后的路径，审批过的写入目标必须是同一套写法，否则会被判成
        // 「越界写入」——那是路径写法不一致，不是越权。
        let fixture = std::fs::canonicalize(&fixture).expect("canonical fixture");
        // 没有 verifier 的样本没有「红」这回事：把它算成无效样本会让
        // 只读工种永远背一个它挣不到也输不掉的分。
        let seeded_red = case
            .verifier
            .is_none_or(|command| !verifier_passes(&fixture, command));

        let sink = Arc::new(RangeSink::default());
        let trace = Arc::new(Mutex::new(Vec::new()));
        let traced: Arc<dyn Provider> = Arc::new(TracingProvider {
            inner: provider.clone(),
            log: trace.clone(),
        });
        let catalog = SubagentCatalog::new(
            &fixture,
            builtin_profiles(traced.clone()),
            Arc::new(BackgroundTaskRegistry::default()),
        )
        .with_event_sink(sink.clone())
        .with_worktree_root(&worktrees);

        let writes = case.verifier.is_some();
        // 写集必须覆盖样本铺下的每个源文件，而不是写死 `src/lib.rs`。
        //
        // 写死的话，跨文件样本会在「Worker 发出正确补丁 → 被判越界写入」这一步
        // 挂掉，而报告上看起来就像模型修不动——这个仓库在审批层被同一类假信号
        // 咬过一次（符号链接没规范化那次）。让工种够不着正确答案，测出来的是
        // 围栏的形状，不是它的能力。
        let sources = case.source_files();
        let targets = writes.then(|| {
            sources
                .iter()
                .map(|path| fixture.join(path))
                .collect::<BTreeSet<_>>()
        });
        let mut task_files = sources.clone();
        if !writes {
            // 让只读工种自己去找，测的才是「找」这门手艺。
            task_files.clear();
        }
        let started = Instant::now();
        let outcome = catalog
            .run(
                SpawnAgentArgs {
                    prompt: match case.profile {
                        "test_fixer" => PROMPT_TEST.to_owned(),
                        "build_fixer" => PROMPT_BUILD.to_owned(),
                        _ => PROMPT_REPORT.to_owned(),
                    },
                    profile: Some(case.profile.to_owned()),
                    run_in_background: Some(false),
                    task: Some(TaskPacket {
                        goal: case.goal.to_owned(),
                        read_files: task_files.clone(),
                        write_files: if writes { task_files } else { Vec::new() },
                        relevant_files: Vec::new(),
                        known_facts: case.known_facts.iter().map(|f| (*f).to_owned()).collect(),
                        constraints: case.constraints.iter().map(|c| (*c).to_owned()).collect(),
                        verifier: case.verifier.map(|command| TaskVerifier {
                            command: command.to_owned(),
                            expected_exit_code: Some(0),
                        }),
                        max_attempts: None,
                        skill: None,
                        digest_oversized: None,
                    }),
                    ..SpawnAgentArgs::default()
                },
                targets,
            )
            .await;
        let elapsed = started.elapsed();

        let (verdict, attempts) = sink
            .verdicts
            .lock()
            .expect("range verdicts")
            .last()
            .copied()
            .unwrap_or((None, 0));
        let (input_tokens, output_tokens) = *sink.tokens.lock().expect("range tokens");
        let (claims_checked, claims_unverifiable) = *sink.claims.lock().expect("range claims");
        // 答对了没有：引用存在只说明它没编造地名，不说明它去对了地方。
        let report_text = outcome.as_ref().ok().cloned().unwrap_or_default();
        let mut expectations = case
            .expect_contains
            .iter()
            .map(|needle| (*needle).to_owned())
            .collect::<Vec<_>>();
        if let Some(commit) = &culprit_commit {
            expectations.push(commit[..7].to_owned());
        }
        let expectation_met = expectations.is_empty()
            || expectations
                .iter()
                .all(|needle| report_text.contains(needle.as_str()));
        let tests_intact = only_worktree(&worktrees)
            .map(|worktree| test_block_intact(&worktree, case))
            .unwrap_or(true);
        let error = outcome.as_ref().err().map(ToString::to_string);
        let trace_path = trace_dir.join(format!("{}.log", case.id));
        std::fs::write(&trace_path, trace.lock().expect("range trace").join("\n\n"))
            .expect("write trace");

        eprintln!(
            "[range] {:<26} verdict={:<7} attempts={} tests_intact={} claims={claims_checked}/{claims_unverifiable} answer={expectation_met} {:.1}s",
            case.id,
            match verdict {
                Some(true) => "passed",
                Some(false) => "failed",
                None => "none",
            },
            attempts,
            tests_intact,
            elapsed.as_secs_f64()
        );

        results.push(serde_json::json!({
            "case": case.id,
            "profile": case.profile,
            "model": model,
            "seeded_red": seeded_red,
            "verifier": case.verifier,
            "verifier_passed": verdict,
            "attempts": attempts,
            "tests_intact": tests_intact,
            "verified_success": verdict == Some(true) && tests_intact,
            "claims_checked": claims_checked,
            "claims_unverifiable": claims_unverifiable,
            "expectation_met": expectation_met,
            "expectations": expectations,
            "report_only": case.verifier.is_none(),
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "seconds": (elapsed.as_secs_f64() * 10.0).round() / 10.0,
            "tool_calls": sink.tool_calls.lock().expect("range tool calls").clone(),
            "report": outcome.as_ref().ok().map(|report| {
                report.chars().take(400).collect::<String>()
            }),
            "trace": trace_path.display().to_string(),
            "error": error,
        }));
    }

    let report = serde_json::json!({
        "model": model,
        "cases": results,
    });
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).expect("report directory");
    }
    std::fs::write(
        &out,
        serde_json::to_string_pretty(&report).expect("serialize report"),
    )
    .expect("write report");
    eprintln!("[range] report: {}", out.display());
}
