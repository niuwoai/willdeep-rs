//! 工种（`[subagents.*]`）兑现成哪个模型。
//!
//! 从 `harness::build` 里拆出来，是为了能单独测：运行时与设置面板必须对同一份
//! config 说出同一个模型，以前两边各有一套解析，面板显示的并不是实际在跑的。

use std::sync::Arc;

use anyhow::{Context, Result};
use willdeep_core::provider::{Provider, ProviderConfig, ProviderKind};
use willdeep_core::{SubagentProfile, build_provider, builtin_profiles};

use crate::config::ConfigFile;
use crate::provider_config_from_profile;

/// 组装全部内置工种，套上 `[subagents.*]` 的覆盖。
///
/// 解析阶梯（与 `model_routing::profile_settings` 同一条）：
///
/// 1. `[subagents.<工种>]`（找不到再认改名前的段落名，见
///    [`crate::config::subagent_section_id`]）里的 `provider_profile` / `model`；
/// 2. some.im 上这个工种的托管绑定（[`willdeep_core::subagent::hosted_worker_model`]）；
/// 3. 其余沿用会话主模型——`follow_main`，`/model` 一换跟着换。与 macOS 版一致
///    （它继承父会话配置）；以前 some.im 上这一档写死 `glm-5`，会话切到别的模型，
///    implementer / tester / reviewer / ops_runner 仍悄悄跑 glm-5。
///
/// 第 3 种情况 `model` 留空，展示与派工记录在派工那一刻按 follower 现取。
pub(super) fn configure_worker_profiles(
    file: &ConfigFile,
    parent: &ProviderConfig,
    follow_main: &Arc<dyn Provider>,
    kind: ProviderKind,
) -> Result<Vec<SubagentProfile>> {
    let mut profiles = builtin_profiles(follow_main.clone());
    for subagent in &mut profiles {
        // some.im 上基础档统一是 `someim-32b`：同一个网关、同一批账号，同一个
        // 职责在两个客户端必须解析到同一个模型。历史上的 `someim-32b-<工种>`
        // 已经退役，职责提示词改由客户端随请求发送。
        let hosted = (kind == ProviderKind::SomeIm)
            .then(|| willdeep_core::subagent::hosted_worker_model(&subagent.id))
            .flatten();
        subagent.model = hosted.clone();
        if let Some(hosted_model) = &hosted {
            let mut configured = parent.clone();
            configured.model = hosted_model.clone();
            subagent.provider = build_provider(configured)
                .with_context(|| format!("initialize hosted subagent model {hosted_model}"))?;
        }
        // 段落按正名找，找不到再认改名前的旧名（`[subagents.deep]` →
        // generalist、`[subagents.judge]` → reviewer），与设置面板同一个解析。
        if let Some(settings) = crate::config::subagent_settings(file, &subagent.id) {
            if let Some(provider_name) = settings.provider_profile.as_deref() {
                let mut configured = provider_config_from_profile(file, provider_name)?;
                if let Some(model) = &settings.model {
                    configured.model = model.clone();
                }
                subagent.model = Some(configured.model.clone());
                subagent.provider = build_provider(configured)
                    .with_context(|| format!("initialize subagent profile {}", subagent.id))?;
            } else if let Some(model) = &settings.model {
                let mut configured = parent.clone();
                configured.model = model.clone();
                subagent.model = Some(model.clone());
                subagent.provider = build_provider(configured)
                    .with_context(|| format!("initialize subagent profile {}", subagent.id))?;
            }
            if let Some(max_turns) = settings.max_turns {
                subagent.max_turns = max_turns;
            }
            if let Some(window) = settings.context_window {
                subagent.context_window = window;
            }
            if let Some(token_budget) = settings.token_budget {
                subagent.token_budget = Some(token_budget);
            }
            if let Some(timeout_seconds) = settings.timeout_seconds {
                subagent.timeout_seconds = Some(timeout_seconds);
            }
            if let Some(max_failures) = settings.max_consecutive_failures {
                subagent.max_consecutive_failures = max_failures;
            }
            if let Some(limit) = settings.tool_output_limit {
                subagent.tool_output_limit = Some(limit);
            }
            if let Some(max_attempts) = settings.max_attempts {
                subagent.max_attempts = max_attempts;
            }
            if let Some(worktree) = settings.worktree.as_deref() {
                subagent.worktree = match worktree {
                    "dedicated" => willdeep_core::SubagentWorktreePolicy::Dedicated,
                    _ => willdeep_core::SubagentWorktreePolicy::Shared,
                };
            }
        }
        // 判定放在所有覆盖之后，跟着**最终**解析出的模型走。跟着工种名走是
        // 错的：工种绑成 `someim-32b` 时网关并不会 prepend 职责提示词，客户端
        // 若也把自己那份省掉，Worker 就只剩边界段落、不知道自己是干什么的。
        subagent.hosted_job_prompt = subagent
            .model
            .as_deref()
            .is_some_and(willdeep_core::hosts_job_prompt);
    }
    Ok(profiles)
}

#[cfg(test)]
mod tests {
    use super::*;
    use willdeep_core::provider::{ApiDialect, MainModelHandle, provider_model};

    const SOMEIM_CONFIG: &str = r#"
version = 1
default_provider = "some-im"

[providers.some-im]
provider = "some-im"
api_base = "https://some.im/v1"
api_key_env = "WILLDEEP_WORKER_PROFILES_TEST_KEY"
model = "glm-5"
"#;

    fn parent(kind: ProviderKind, model: &str) -> ProviderConfig {
        ProviderConfig::new(
            kind,
            ApiDialect::ChatCompletions,
            "https://some.im/v1".to_owned(),
            "placeholder".to_owned(),
            model.to_owned(),
        )
    }

    /// 每个工种此刻实际会用的模型名：有固定的就是固定的，跟随主模型的按
    /// follower 现取。
    fn effective(profiles: &[SubagentProfile], id: &str) -> String {
        let profile = profiles
            .iter()
            .find(|profile| profile.id == id)
            .unwrap_or_else(|| panic!("profile {id}"));
        profile
            .model
            .clone()
            .or_else(|| provider_model(profile.provider.as_ref()))
            .unwrap_or_default()
    }

    fn build(
        source: &str,
        kind: ProviderKind,
        model: &str,
    ) -> (Vec<SubagentProfile>, MainModelHandle) {
        let file: ConfigFile = toml::from_str(source).expect("parse fixture");
        let parent = parent(kind, model);
        let main_model = MainModelHandle::new(build_provider(parent.clone()).expect("provider"));
        let profiles = configure_worker_profiles(&file, &parent, &main_model.follower(), kind)
            .expect("configure worker profiles");
        (profiles, main_model)
    }

    #[test]
    fn legacy_deep_and_judge_sections_reach_the_runtime() {
        // 以前校验放行 `[subagents.deep]` / `[subagents.judge]`，面板也回落旧名
        // 显示它们的模型，运行时却只按正名找——显示的模型根本没在跑。
        let source = format!(
            "{SOMEIM_CONFIG}\n[subagents.deep]\nmodel = \"deep-model\"\nmax_turns = 7\n\n[subagents.judge]\nmodel = \"judge-model\"\n"
        );
        let (profiles, _) = build(&source, ProviderKind::SomeIm, "glm-5");
        assert_eq!(effective(&profiles, "generalist"), "deep-model");
        assert_eq!(
            profiles
                .iter()
                .find(|profile| profile.id == "generalist")
                .map(|profile| profile.max_turns),
            Some(7)
        );
        assert_eq!(effective(&profiles, "reviewer"), "judge-model");
    }

    #[test]
    fn the_canonical_section_wins_over_a_legacy_one() {
        let source = format!(
            "{SOMEIM_CONFIG}\n[subagents.generalist]\nmodel = \"new\"\n\n[subagents.deep]\nmodel = \"old\"\n"
        );
        let (profiles, _) = build(&source, ProviderKind::SomeIm, "glm-5");
        assert_eq!(effective(&profiles, "generalist"), "new");
    }

    #[test]
    fn runtime_and_settings_panel_agree_on_every_public_trade() {
        let source = format!(
            "{SOMEIM_CONFIG}\n[subagents.deep]\nmodel = \"deep-model\"\n\n[subagents.judge]\nmodel = \"judge-model\"\n"
        );
        let (profiles, _) = build(&source, ProviderKind::SomeIm, "glm-5");
        let dir =
            std::env::temp_dir().join(format!("willdeep-worker-profiles-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("config.toml");
        std::fs::write(&path, &source).expect("write config");
        let panel = crate::model_routing::load(&path, None).expect("load routing settings");
        for row in &panel.profiles {
            assert_eq!(
                row.effective_model,
                effective(&profiles, &row.id),
                "面板与运行时对 {} 说的模型不一样",
                row.id
            );
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unhosted_trades_inherit_the_session_model_and_follow_a_switch() {
        // some.im 上没有托管绑定的工种以前写死 glm-5；现在跟会话主模型走。
        let (profiles, main_model) = build(SOMEIM_CONFIG, ProviderKind::SomeIm, "deepseek-v4-pro");
        for trade in ["implementer", "tester", "reviewer", "ops_runner"] {
            assert_eq!(effective(&profiles, trade), "deepseek-v4-pro", "{trade}");
        }
        // 托管绑定的照旧。
        assert_eq!(effective(&profiles, "generalist"), "someim-32b");

        main_model.set_model("glm-5.1").expect("switch");
        assert_eq!(effective(&profiles, "implementer"), "glm-5.1");
        assert_eq!(effective(&profiles, "generalist"), "someim-32b");
    }

    #[test]
    fn an_explicit_worker_model_does_not_follow_a_switch() {
        let source = format!("{SOMEIM_CONFIG}\n[subagents.implementer]\nmodel = \"pinned\"\n");
        let (profiles, main_model) = build(&source, ProviderKind::SomeIm, "glm-5");
        main_model.set_model("glm-5.1").expect("switch");
        assert_eq!(effective(&profiles, "implementer"), "pinned");
    }
}
