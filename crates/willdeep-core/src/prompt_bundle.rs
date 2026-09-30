//! 提示词版本戳（`docs/PROMPT_RSI_DESIGN.md` Phase 0）。
//!
//! 反馈账本里的每一行都该能回答「当时用的是哪一版提示词」，否则改了提示词
//! 之后采用率涨了还是跌了无从比较。版本号是 `角色@<sha256 前 12 位>`，只对
//! **不随运行变化**的部分取哈希：工作区路径、平台、用户的全局规则都不计入，
//! 同一份代码在哪台机器上跑都是同一个版本号；改了提示词里的一个字就换一个。

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use sha2::{Digest, Sha256};

use crate::provider::{Provider, ProviderError};
use crate::subagent::SubagentProfile;
use crate::types::{Completion, Message, ToolDefinition};

pub const MAIN: &str = "main";
pub const INPUT_SUGGESTION: &str = "input_suggestion";

/// `role@<sha256(parts) 前 12 位>`。各段之间加分隔符，免得段界挪动撞出同一个哈希。
pub fn bundle_id(role: &str, parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0u8]);
    }
    let digest = hasher.finalize();
    let hex: String = digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{role}@{hex}")
}

/// 主 Agent：稳定契约 + 公开工种契约。
pub fn main_bundle() -> String {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        let trades = crate::subagent::public_trade_contract();
        bundle_id(MAIN, &[crate::prompt::STABLE_CONTRACT, &trades])
    })
    .clone()
}

/// 输入建议的系统提示。
pub fn input_suggestion_bundle() -> String {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| bundle_id(INPUT_SUGGESTION, &[crate::input_suggestion::SYSTEM_PROMPT]))
        .clone()
}

/// 一个工种的 Worker：`worker:<工种>`，托管工种加 `@hosted` 后缀再接哈希。
pub fn worker_bundle(profile: &SubagentProfile, has_board: bool) -> String {
    let role = if profile.hosted_job_prompt {
        format!("worker:{}@hosted", profile.id)
    } else {
        format!("worker:{}", profile.id)
    };
    bundle_id(
        &role,
        &crate::subagent::worker_prompt_parts(profile, has_board),
    )
}

/// 当前代码里各角色的版本号（`willdeep feedback bundles`）。Worker 按挂着
/// 共享黑板算，这是 harness 的默认装配。
pub fn current_bundles() -> Vec<String> {
    let mut bundles = vec![main_bundle(), input_suggestion_bundle()];
    bundles.extend(
        crate::subagent::builtin_profiles(Arc::new(NoProvider))
            .iter()
            .map(|profile| worker_bundle(profile, true)),
    );
    bundles
}

/// 只为列出内置工种而存在：不会被调用。
struct NoProvider;

#[async_trait]
impl Provider for NoProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
    ) -> Result<Completion, ProviderError> {
        Err(ProviderError::EmptyResponse)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_ids_are_stable_and_change_with_content() {
        let a = bundle_id("main", &["alpha", "beta"]);
        assert_eq!(a, bundle_id("main", &["alpha", "beta"]));
        assert!(a.starts_with("main@"));
        assert_eq!(a.len(), "main@".len() + 12);
        assert_ne!(a, bundle_id("main", &["alpha", "betA"]));
        assert_ne!(a, bundle_id("main", &["alphab", "eta"]), "segment boundary");
    }

    #[test]
    fn worker_bundles_do_not_depend_on_the_workspace_and_cover_every_profile() {
        let bundles = current_bundles();
        assert!(bundles[0].starts_with("main@"));
        assert!(bundles[1].starts_with("input_suggestion@"));
        let profiles = crate::subagent::builtin_profiles(Arc::new(NoProvider));
        assert_eq!(bundles.len(), 2 + profiles.len());
        let mut profile = profiles[0].clone();
        let plain = worker_bundle(&profile, false);
        assert!(plain.starts_with(&format!("worker:{}@", profile.id)));
        assert_ne!(
            plain,
            worker_bundle(&profile, true),
            "board guidance counts"
        );
        profile.hosted_job_prompt = true;
        assert!(
            worker_bundle(&profile, false).starts_with(&format!("worker:{}@hosted@", profile.id))
        );
        assert_eq!(current_bundles(), bundles, "no dynamic input");
    }
}
