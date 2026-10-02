//! 提示词分段与变体（`docs/PROMPT_RSI_OPERATIONS.md`）。
//!
//! 各角色的系统提示按段命名，一个**变体**（`willdeep.prompt-variant.v1`）只替换
//! 其中一段（设计文档 §8.3 的单点修改）。变体只从环境变量
//! [`VARIANT_ENV`] 加载，给离线评测的进程用，不进配置文件：生产会话不会因为
//! 一份遗留的配置悄悄跑在候选提示词上。
//!
//! 结构门（§11.2 第 1、2 道）在加载时执行，不合法的变体让进程直接报错，而不是
//! 退回默认提示词继续跑——那样评测报告会把 baseline 的结果记在候选名下。
//!
//! 段名：
//! - `main`：`preamble`、`tone`、`coding_conventions`、`version_control`、
//!   `tool_rules`、`delegation`（`STABLE_CONTRACT` 按空行切出来的六段）。工种目录
//!   由工种定义生成，不在此列。
//! - `worker:<工种>`：`boundary`、`capability_prompt`、`report_contract`、
//!   `board_guidance`。变体只作用于这一个工种。
//! - `input_suggestion`：`system_prompt`。

use std::borrow::Cow;
use std::path::Path;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::subagent::SubagentProfile;

pub const VARIANT_SCHEMA: &str = "willdeep.prompt-variant.v1";
pub const VARIANT_ENV: &str = "WILLDEEP_PROMPT_VARIANT";

pub const MAIN_SECTIONS: [&str; 6] = [
    "preamble",
    "tone",
    "coding_conventions",
    "version_control",
    "tool_rules",
    "delegation",
];
pub const WORKER_SECTIONS: [&str; 4] = [
    "boundary",
    "capability_prompt",
    "report_contract",
    "board_guidance",
];
pub const SUGGESTION_SECTIONS: [&str; 1] = ["system_prompt"];

/// 变体文本最多是原段的这么多倍（再长就不是「改一条规则」了）。
const MAX_GROWTH: usize = 2;
/// 很短的段（例如只有一两句）至少允许写到这么多字符。
const MIN_ALLOWED_CHARS: usize = 400;

/// 放宽审批、关验证器、提权、注入话术、破坏性命令、泄露凭据的说法。变体
/// 可以改写规则，不能靠一句话把边界放掉——无论这句话是人写的还是模型写的。
/// 按出现次数比较：原段本来就有的说法不算新增。
const WEAKENING_PHRASES: &[&str] = &[
    "without approval",
    "skip approval",
    "bypass",
    "full-access",
    "--no-verify",
    "skip the verifier",
    "disable the verifier",
    "ignore the verifier",
    "ignore previous",
    "ignore all",
    "rm -rf",
    "force push",
    "--force",
    "reveal",
    "print the api key",
];

/// 变体比原段多出来的削弱安全的说法（小写比较）。
fn weakening_phrases(original: &str, text: &str) -> Vec<&'static str> {
    let original = original.to_lowercase();
    let text = text.to_lowercase();
    WEAKENING_PHRASES
        .iter()
        .copied()
        .filter(|phrase| text.matches(phrase).count() > original.matches(phrase).count())
        .collect()
}

/// 一段变体文本的长度上限（字符）。
pub fn max_chars(original: &str) -> usize {
    (original.chars().count() * MAX_GROWTH).max(MIN_ALLOWED_CHARS)
}

/// 这一段里必须原样保留的片段：只列原段里确实出现的。结构门查的就是这份
/// 清单，优化器的提示词也把它原样交给模型。
pub fn invariant_fragments(role: &PromptRole, section: &str) -> Vec<&'static str> {
    let Some(original) = base_section(role, section) else {
        return Vec::new();
    };
    invariants(role, section)
        .iter()
        .copied()
        .filter(|fragment| original.contains(fragment))
        .collect()
}

/// 每段必须原样保留的片段：工具名、跨端逐字相同的约定、安全边界。改提示词
/// 可以换说法，不能把这些删掉。
fn invariants(role: &PromptRole, section: &str) -> &'static [&'static str] {
    match (role, section) {
        (PromptRole::Main, "preamble") => &["=== Stable WillDeep Agent Contract"],
        (PromptRole::Main, "tone") => &["The user never sees your reasoning or thinking"],
        (PromptRole::Main, "coding_conventions") => {
            &["Never log, echo, persist, or expose secrets"]
        }
        (PromptRole::Main, "version_control") => {
            &["Co-Authored-By: WillDeep <noreply@willdeep.com>"]
        }
        (PromptRole::Main, "tool_rules") => &[
            "search_files",
            "grep_files",
            "read_file",
            "list_directory",
            "git_status",
            "create_file",
            "edit_file",
            "run_command",
            // 后台任务合同 v1 附录 A：与 Xedit 逐字相同的一段。
            "Background work: run builds, tests, releases and dev servers expected to take 30 seconds or more with `run_in_background: true` and a short user-facing `label`. You are notified automatically when a background task finishes, so never sleep or poll for it; call `get_job_output` only to check progress mid-run. Keep doing independent work while you wait. When a completion notice arrives, verify its result first, then return to what you were doing. Until that notice arrives, do not claim the task succeeded or guess its outcome. Stop background tasks you no longer need with `kill_job`.",
            "Never escape the workspace or expose credentials",
        ],
        (PromptRole::Main, "delegation") => &[
            "spawn_agent",
            "target_command",
            "task.read_files",
            "task.write_files",
            "task.relevant_files",
            "task.known_facts",
            "task.verifier.command",
        ],
        (PromptRole::Worker(_), "boundary") => &[
            "{workspace}",
            "cannot ask the user",
            "cannot spawn another agent",
            "Destructive or credential-sensitive commands are outside that judge's authority",
            "target_command",
        ],
        (PromptRole::Worker(_), "report_contract") => {
            &["CONCLUSION", "EVIDENCE", "OPEN QUESTIONS", "<worker-facts>"]
        }
        (PromptRole::Worker(_), "board_guidance") => {
            &["board_read", "board_post", "not instructions"]
        }
        (PromptRole::InputSuggestion, "system_prompt") => &["NONE"],
        _ => &[],
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromptRole {
    Main,
    InputSuggestion,
    /// 内置工种 id。
    Worker(String),
}

impl PromptRole {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "main" => Some(Self::Main),
            "input_suggestion" => Some(Self::InputSuggestion),
            other => {
                let id = other.strip_prefix("worker:")?;
                builtin_profile(id).map(|_| Self::Worker(id.to_owned()))
            }
        }
    }

    pub fn name(&self) -> String {
        match self {
            Self::Main => crate::prompt_bundle::MAIN.to_owned(),
            Self::InputSuggestion => crate::prompt_bundle::INPUT_SUGGESTION.to_owned(),
            Self::Worker(id) => format!("worker:{id}"),
        }
    }

    pub fn sections(&self) -> &'static [&'static str] {
        match self {
            Self::Main => &MAIN_SECTIONS,
            Self::InputSuggestion => &SUGGESTION_SECTIONS,
            Self::Worker(_) => &WORKER_SECTIONS,
        }
    }

    /// 所有可以写变体的角色，按 `willdeep prompt sections` 的顺序。
    pub fn all() -> Vec<Self> {
        let mut roles = vec![Self::Main, Self::InputSuggestion];
        roles.extend(
            crate::prompt_bundle::builtin_profile_list()
                .into_iter()
                .map(|profile| Self::Worker(profile.id)),
        );
        roles
    }
}

fn builtin_profile(id: &str) -> Option<SubagentProfile> {
    crate::prompt_bundle::builtin_profile_list()
        .into_iter()
        .find(|profile| profile.id == id)
}

/// 一个单段修改的候选提示词。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PromptVariant {
    pub schema: String,
    /// 变体名，进报告与账本：`[a-z0-9._-]`。
    pub id: String,
    pub role: String,
    pub section: String,
    /// 写变体时该角色的版本号；代码里的提示词变了，变体就过期了。
    pub parent_bundle: String,
    pub text: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub expected_effect: String,
    #[serde(default)]
    pub risk: String,
    /// 来源（例如 `feedback report --candidates` 的一条），原样保留。
    #[serde(default)]
    pub source: Option<serde_json::Value>,
}

impl PromptVariant {
    fn targets(&self, role: &PromptRole, section: &str) -> bool {
        self.section == section && PromptRole::parse(&self.role).as_ref() == Some(role)
    }
}

/// 结构门的结论：新旧版本号与被替换的原文。
#[derive(Clone, Debug)]
pub struct VariantCheck {
    pub role: PromptRole,
    pub base_bundle: String,
    pub candidate_bundle: String,
    pub original: String,
}

/// `STABLE_CONTRACT` 的六段，按顺序。
pub fn main_contract_sections() -> Vec<(&'static str, &'static str)> {
    MAIN_SECTIONS
        .iter()
        .copied()
        .zip(crate::prompt::STABLE_CONTRACT.split("\n\n"))
        .collect()
}

/// 某角色某段在代码里的原文。未知的角色 / 段为 `None`。
pub fn base_section(role: &PromptRole, section: &str) -> Option<String> {
    match role {
        PromptRole::Main => main_contract_sections()
            .into_iter()
            .find(|(name, _)| *name == section)
            .map(|(_, text)| text.to_owned()),
        PromptRole::InputSuggestion => {
            (section == "system_prompt").then(|| crate::input_suggestion::SYSTEM_PROMPT.to_owned())
        }
        PromptRole::Worker(id) => {
            let profile = builtin_profile(id)?;
            crate::subagent::worker_base_section(&profile, section).map(str::to_owned)
        }
    }
}

/// 套上变体之后的主 Agent 稳定契约。
pub fn main_contract(variant: Option<&PromptVariant>) -> Cow<'static, str> {
    let Some(variant) =
        variant.filter(|variant| PromptRole::parse(&variant.role) == Some(PromptRole::Main))
    else {
        return Cow::Borrowed(crate::prompt::STABLE_CONTRACT);
    };
    Cow::Owned(
        main_contract_sections()
            .into_iter()
            .map(|(name, text)| {
                if name == variant.section {
                    variant.text.as_str()
                } else {
                    text
                }
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
    )
}

/// 套上变体之后的输入建议系统提示。
pub fn suggestion_prompt(variant: Option<&PromptVariant>) -> &str {
    match variant {
        Some(variant) if variant.targets(&PromptRole::InputSuggestion, "system_prompt") => {
            &variant.text
        }
        _ => crate::input_suggestion::SYSTEM_PROMPT,
    }
}

/// 套上变体之后某工种的某段；变体不针对它时原样返回。
pub(crate) fn worker_section<'a>(
    profile_id: &str,
    section: &str,
    base: &'a str,
    variant: Option<&'a PromptVariant>,
) -> &'a str {
    match variant {
        Some(variant) if variant.targets(&PromptRole::Worker(profile_id.to_owned()), section) => {
            &variant.text
        }
        _ => base,
    }
}

/// 本进程生效的变体。首次调用时从 [`VARIANT_ENV`] 加载并校验；没设或不合法
/// 都是 `None`（不合法时入口处的 [`init_from_env`] 已经报错退出）。
pub fn active_variant() -> Option<&'static PromptVariant> {
    init_from_env().ok().flatten()
}

/// 本进程实际生效的变体，给评测报告逐条记录用：没有变体时为 `null`，否则是
/// 变体 id、角色、段名与套上它之后的版本号。对照评测拿它核对 baseline 真的
/// 没套变体、候选真的套上了预期那一份——环境变量会被子进程继承，光看命令行
/// 参数说明不了实际跑的是哪份提示词。
pub fn active_variant_report() -> serde_json::Value {
    let Some((variant, check)) =
        active_variant().and_then(|variant| Some((variant, check_variant(variant).ok()?)))
    else {
        return serde_json::Value::Null;
    };
    serde_json::json!({
        "id": variant.id,
        "role": variant.role,
        "section": variant.section,
        "bundle": check.candidate_bundle,
    })
}

/// 加载并校验 [`VARIANT_ENV`] 指向的变体，结果在进程内缓存。
pub fn init_from_env() -> Result<Option<&'static PromptVariant>, String> {
    static ACTIVE: OnceLock<Result<Option<PromptVariant>, String>> = OnceLock::new();
    ACTIVE
        .get_or_init(|| {
            let Some(path) = std::env::var_os(VARIANT_ENV).filter(|path| !path.is_empty()) else {
                return Ok(None);
            };
            let variant = load_variant(Path::new(&path))?;
            check_variant(&variant).map_err(|problems| {
                format!(
                    "{VARIANT_ENV}={} is not a valid prompt variant:\n- {}",
                    Path::new(&path).display(),
                    problems.join("\n- ")
                )
            })?;
            Ok(Some(variant))
        })
        .as_ref()
        .map(Option::as_ref)
        .map_err(Clone::clone)
}

pub fn load_variant(path: &Path) -> Result<PromptVariant, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read prompt variant {}: {error}", path.display()))?;
    serde_json::from_str(&text)
        .map_err(|error| format!("cannot parse prompt variant {}: {error}", path.display()))
}

/// 结构门与安全门。返回全部问题，而不是第一个。
pub fn check_variant(variant: &PromptVariant) -> Result<VariantCheck, Vec<String>> {
    let mut problems = Vec::new();
    if variant.schema != VARIANT_SCHEMA {
        problems.push(format!(
            "schema must be {VARIANT_SCHEMA}, got {:?}",
            variant.schema
        ));
    }
    if variant.id.is_empty()
        || !variant.id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
    {
        problems.push("id must be non-empty and use only a-z, 0-9, '.', '_' and '-'".to_owned());
    }
    let Some(role) = PromptRole::parse(&variant.role) else {
        problems.push(format!(
            "unknown role {:?}; use main, input_suggestion or worker:<built-in profile>",
            variant.role
        ));
        return Err(problems);
    };
    let Some(original) = base_section(&role, &variant.section) else {
        problems.push(format!(
            "role {} has no section {:?}; sections: {}",
            role.name(),
            variant.section,
            role.sections().join(", ")
        ));
        return Err(problems);
    };
    let profile = match &role {
        PromptRole::Worker(id) => builtin_profile(id),
        _ => None,
    };
    if profile
        .as_ref()
        .is_some_and(|profile| profile.hosted_job_prompt)
        && matches!(
            variant.section.as_str(),
            "capability_prompt" | "report_contract"
        )
    {
        problems.push(format!(
            "{} runs a relay-hosted job prompt; its {} is not sent by the client",
            role.name(),
            variant.section
        ));
    }
    let base_bundles = base_bundles(&role, profile.as_ref());
    if !base_bundles.contains(&variant.parent_bundle) {
        problems.push(format!(
            "parent_bundle {:?} is stale: this build's {} is {}",
            variant.parent_bundle,
            role.name(),
            base_bundles.join(" / ")
        ));
    }
    let text = variant.text.trim();
    if text.is_empty() {
        problems.push("text must not be empty".to_owned());
    }
    if variant.text == original {
        problems.push("text is identical to the current section".to_owned());
    }
    let limit = max_chars(&original);
    let length = variant.text.chars().count();
    if length > limit {
        problems.push(format!(
            "text has {length} characters; a single-rule change stays within {limit}"
        ));
    }
    for fragment in invariant_fragments(&role, &variant.section) {
        if !variant.text.contains(fragment) {
            problems.push(format!("text drops a required fragment: {fragment:?}"));
        }
    }
    // 只查新增的行：原文里本来就有 “token”“secrets” 这样的词，脱敏器会动它们。
    if variant
        .text
        .lines()
        .filter(|line| !original.lines().any(|kept| kept == *line))
        .any(|line| crate::judge::redact_credentials(line) != line)
    {
        problems.push("text contains something that looks like a credential".to_owned());
    }
    for phrase in weakening_phrases(&original, &variant.text) {
        problems.push(format!(
            "text adds {phrase:?}, which would weaken a safety rule; say what to do, not what to skip"
        ));
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    let candidate_bundle = match (&role, profile.as_ref()) {
        (PromptRole::Main, _) => crate::prompt_bundle::main_bundle_with(Some(variant)),
        (PromptRole::InputSuggestion, _) => {
            crate::prompt_bundle::input_suggestion_bundle_with(Some(variant))
        }
        (PromptRole::Worker(_), Some(profile)) => {
            crate::prompt_bundle::worker_bundle_with(profile, true, Some(variant))
        }
        (PromptRole::Worker(_), None) => unreachable!("worker roles resolve to a profile"),
    };
    Ok(VariantCheck {
        role,
        base_bundle: base_bundles[0].clone(),
        candidate_bundle,
        original,
    })
}

/// 代码里该角色的版本号（不套变体）。Worker 有带黑板与不带黑板两个。
fn base_bundles(role: &PromptRole, profile: Option<&SubagentProfile>) -> Vec<String> {
    match (role, profile) {
        (PromptRole::Main, _) => vec![crate::prompt_bundle::main_bundle_with(None)],
        (PromptRole::InputSuggestion, _) => {
            vec![crate::prompt_bundle::input_suggestion_bundle_with(None)]
        }
        (PromptRole::Worker(_), Some(profile)) => vec![
            crate::prompt_bundle::worker_bundle_with(profile, true, None),
            crate::prompt_bundle::worker_bundle_with(profile, false, None),
        ],
        (PromptRole::Worker(_), None) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variant(role: &str, section: &str, text: String) -> PromptVariant {
        let role_parsed = PromptRole::parse(role).unwrap();
        let parent_bundle = base_bundles(
            &role_parsed,
            match &role_parsed {
                PromptRole::Worker(id) => builtin_profile(id),
                _ => None,
            }
            .as_ref(),
        )[0]
        .clone();
        PromptVariant {
            schema: VARIANT_SCHEMA.to_owned(),
            id: "test-variant".to_owned(),
            role: role.to_owned(),
            section: section.to_owned(),
            parent_bundle,
            text,
            reason: String::new(),
            expected_effect: String::new(),
            risk: String::new(),
            source: None,
        }
    }

    #[test]
    fn the_contract_splits_into_named_sections_that_rejoin_exactly() {
        let sections = main_contract_sections();
        assert_eq!(
            crate::prompt::STABLE_CONTRACT.split("\n\n").count(),
            MAIN_SECTIONS.len(),
            "a new blank line in STABLE_CONTRACT needs a new section name"
        );
        assert!(sections[1].1.starts_with("Tone and response style:"));
        assert!(sections[4].1.starts_with("Stable tool contract:"));
        assert!(sections[5].1.starts_with("Delegation contract:"));
        assert_eq!(main_contract(None), crate::prompt::STABLE_CONTRACT);
        for role in PromptRole::all() {
            for section in role.sections() {
                if let PromptRole::Worker(id) = &role
                    && builtin_profile(id).unwrap().hosted_job_prompt
                {
                    continue;
                }
                assert!(
                    base_section(&role, section).is_some(),
                    "{} {section}",
                    role.name()
                );
            }
        }
    }

    #[test]
    fn a_valid_variant_changes_only_its_own_role() {
        let original = base_section(&PromptRole::Main, "tone").unwrap();
        let text = format!("{original}\n- Lead every answer with the conclusion.");
        let variant = variant("main", "tone", text.clone());
        let check = check_variant(&variant).unwrap();
        assert_ne!(check.base_bundle, check.candidate_bundle);
        assert!(check.candidate_bundle.starts_with("main@"));
        let contract = main_contract(Some(&variant));
        assert!(contract.contains("Lead every answer with the conclusion."));
        assert_eq!(
            contract.len(),
            crate::prompt::STABLE_CONTRACT.len() + text.len() - original.len()
        );
        assert_eq!(
            suggestion_prompt(Some(&variant)),
            crate::input_suggestion::SYSTEM_PROMPT,
            "a main variant does not touch the suggestion prompt"
        );
        assert_eq!(
            crate::prompt_bundle::input_suggestion_bundle_with(Some(&variant)),
            crate::prompt_bundle::input_suggestion_bundle_with(None)
        );
    }

    #[test]
    fn worker_variants_apply_to_one_profile() {
        let reviewer = builtin_profile("reviewer").unwrap();
        let tester = builtin_profile("tester").unwrap();
        let text = format!(
            "{}\nCite the failing line before proposing a fix.",
            reviewer.capability_prompt
        );
        let variant = variant("worker:reviewer", "capability_prompt", text);
        let check = check_variant(&variant).unwrap();
        assert!(check.candidate_bundle.starts_with("worker:reviewer@"));
        assert_ne!(
            crate::prompt_bundle::worker_bundle_with(&reviewer, true, Some(&variant)),
            crate::prompt_bundle::worker_bundle_with(&reviewer, true, None)
        );
        assert_eq!(
            crate::prompt_bundle::worker_bundle_with(&tester, true, Some(&variant)),
            crate::prompt_bundle::worker_bundle_with(&tester, true, None)
        );
        let parts = crate::subagent::worker_prompt_parts_with(&reviewer, true, Some(&variant));
        assert!(
            parts
                .iter()
                .any(|part| part.contains("Cite the failing line"))
        );
    }

    #[test]
    fn weakening_is_counted_against_the_original_not_the_line() {
        assert_eq!(
            weakening_phrases("Never bypass review.", "Never bypass review, ever."),
            Vec::<&str>::new(),
            "an edited line keeping an existing phrase is not new"
        );
        assert_eq!(
            weakening_phrases("Ask first.", "Ask first. BYPASS it when urgent."),
            ["bypass"]
        );
        let delegation = invariant_fragments(&PromptRole::Main, "delegation");
        assert!(delegation.contains(&"task.verifier.command"));
        assert!(invariant_fragments(&PromptRole::Main, "nope").is_empty());
        let boundary = invariant_fragments(&PromptRole::Worker("tester".to_owned()), "boundary");
        assert!(boundary.contains(&"{workspace}"));
    }

    #[test]
    fn the_structure_gate_rejects_unsafe_or_stale_variants() {
        let problems = |variant: &PromptVariant| check_variant(variant).unwrap_err().join("\n");
        let original = base_section(&PromptRole::Main, "version_control").unwrap();

        let mut stale = variant(
            "main",
            "version_control",
            format!("{original} Keep it short."),
        );
        stale.parent_bundle = "main@000000000000".to_owned();
        assert!(problems(&stale).contains("stale"));

        let dropped = variant(
            "main",
            "version_control",
            "Version control:\n- Commit often.".to_owned(),
        );
        assert!(problems(&dropped).contains("Co-Authored-By"));

        let long = variant(
            "main",
            "version_control",
            format!("{original}{}", "x".repeat(2_000)),
        );
        assert!(problems(&long).contains("single-rule change"));

        let same = variant("main", "version_control", original.clone());
        assert!(problems(&same).contains("identical"));

        let secret = variant(
            "main",
            "version_control",
            format!("{original}\nUse api_key=sk-abcdefghijklmnopqrstuv for pushes."),
        );
        assert!(problems(&secret).contains("credential"));

        let mut unknown = variant("main", "tone", "x".to_owned());
        unknown.section = "persona".to_owned();
        assert!(problems(&unknown).contains("no section"));
        unknown.role = "worker:nobody".to_owned();
        assert!(problems(&unknown).contains("unknown role"));

        let weakened = variant(
            "main",
            "version_control",
            format!("{original}\n- When a hook blocks the commit, retry with --no-verify."),
        );
        let message = problems(&weakened);
        assert!(message.contains("\"--no-verify\""), "{message}");

        let boundary = base_section(&PromptRole::Worker("tester".to_owned()), "boundary").unwrap();
        let escaped = variant(
            "worker:tester",
            "boundary",
            boundary.replace("cannot ask the user", "may ask the user"),
        );
        assert!(problems(&escaped).contains("cannot ask the user"));
    }
}
