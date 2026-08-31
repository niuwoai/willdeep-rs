//! Worker 的三个模型档位。
//!
//! 与 macOS 版（Xedit `AgentWorkerArchitecture.swift`）同一套语义：**职责和档位
//! 正交**。以前一个工种既定职责又定模型（`someim-32b-<工种>` 一个萝卜一个坑），
//! 于是加一种职责就要在网关上多铺一条模型链，而换个模型又得改工种。现在职责
//! 只管提示词、工具和写入边界，模型档位单独选。
//!
//! # 「档」这个词在本仓有两个意思，别混
//!
//! - **本模块的档（tier）**：Worker 的**模型槽 + 上下文预算配额**，三个：
//!   基础 / 进阶 / 专家。这是运行时的资源配置。
//! - [`crate::routing`] 与 `docs/MODEL_TIERS.md` 的档（S/M/L）：**上下文窗口段**，
//!   论证的是私有化可部署性（哪种机房跑得起来）。那是部署基线。
//!
//! 两者会对不上是正常的：进阶档给 256K 预算，不代表它背后的模型只有 256K
//! 物理窗口（`deepseek-v4-flash` 就是 1M 窗口按 256K 预算用）。预算是我们
//! 给它的额度，窗口是它能吃下的极限。

use serde::{Deserialize, Serialize};

/// 基础档的上下文预算。128K 是开源权重的主流窗口下沿，也是这套体系要求
/// 「私有机房也跑得起来」的那条线。
pub const TIER_WINDOW_STANDARD: u64 = 131_072;
/// 进阶与专家档的预算。
pub const TIER_WINDOW_EXTENDED: u64 = 262_144;

/// 基础档的托管模型。历史上的 `someim-32b-<工种>` 全部归一到它——那些别名
/// 存在的理由是**服务端托管的职责提示词**，而职责提示词现在由客户端随请求
/// 发送，网关再 prepend 一次就是双重注入。
pub const HOSTED_BASE_MODEL: &str = "someim-32b";

/// 曾经在网关上托管职责提示词的七个工种别名。
///
/// 这是一份**兼容清单，不是现役库存，也不该再增加**。它只用于把已保存的配置
/// 和 Workflow 在请求边界归一到基础档。
pub const LEGACY_HOSTED_TRADES: [&str; 7] = [
    "scout",
    "reader",
    "editor",
    "test_fixer",
    "build_fixer",
    "log_inspector",
    "git_detective",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerTier {
    /// 基础：有界任务的主力。便宜、可私有化。
    Standard,
    /// 进阶：需要更强推理或更多材料时的一跳。
    Advanced,
    /// 专家：最贵的一档，受准入控制（见 [`crate::routing`]）。
    Expert,
}

impl Default for WorkerTier {
    /// 默认永远是最便宜那档。向上要申请，向下不用。
    fn default() -> Self {
        Self::Standard
    }
}

impl WorkerTier {
    pub const ALL: [Self; 3] = [Self::Standard, Self::Advanced, Self::Expert];

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "standard" | "basic" | "base" => Some(Self::Standard),
            "advanced" => Some(Self::Advanced),
            // `deep` 是旧的工种名。它当年既表示「复杂调查」这个职责，也表示
            // 「用最贵的模型」这个档位；正交化之后前者归 generalist，后者归这里。
            "expert" | "deep" => Some(Self::Expert),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Advanced => "advanced",
            Self::Expert => "expert",
        }
    }

    /// 失败升级时的下一档。专家档没有下一档——升无可升时该报告失败，
    /// 而不是原地重试更贵的东西。
    pub fn next(self) -> Option<Self> {
        match self {
            Self::Standard => Some(Self::Advanced),
            Self::Advanced => Some(Self::Expert),
            Self::Expert => None,
        }
    }

    /// 这一档是否需要升级票据与预算。
    pub fn requires_admission(self) -> bool {
        matches!(self, Self::Expert)
    }

    pub fn context_budget(self) -> u64 {
        match self {
            Self::Standard => TIER_WINDOW_STANDARD,
            Self::Advanced | Self::Expert => TIER_WINDOW_EXTENDED,
        }
    }

    /// some.im 网关上这一档的默认模型。
    ///
    /// 与 Xedit `AgentWorkerTierModels.defaultBinding` 同一张表：同一个网关、
    /// 同一批账号，两个客户端必须把同一档解析到同一个模型，否则同一个人换个
    /// App 打开就换了个 Worker。显式配置仍然覆盖这里。
    pub fn default_hosted_model(self) -> &'static str {
        match self {
            Self::Standard => HOSTED_BASE_MODEL,
            Self::Advanced => "deepseek-v4-flash",
            Self::Expert => "gpt-5.6-sol",
        }
    }
}

/// 把一个可能是旧别名的模型名归一到基础档。
///
/// 只处理这七个已知别名，其余一律原样返回：私有端点上同名的模型、用户自选的
/// 第三方模型、压缩器与安全裁判的辅助模型都不该被改写。
pub fn normalize_hosted_model(model: &str) -> &str {
    if is_legacy_trade_alias(model) {
        HOSTED_BASE_MODEL
    } else {
        // `someim-32b-compressor` 与 `someim-32b-security-guard` 不在名单里，
        // 它们不是工种别名而是各自独立的职能模型，必须原样放行。
        model
    }
}

/// 这个模型名是不是那七个 `someim-32b-<工种>` 别名之一。
fn is_legacy_trade_alias(model: &str) -> bool {
    let Some(trade) = model
        .trim()
        .strip_prefix(HOSTED_BASE_MODEL)
        .and_then(|rest| rest.strip_prefix('-'))
    else {
        return false;
    };
    LEGACY_HOSTED_TRADES.contains(&trade.replace('-', "_").as_str())
}

/// 网关是否会替这个模型 prepend 职责提示词。
///
/// 只有那七个旧别名会——它们存在的全部理由就是服务端托管职责提示词。基础档的
/// `someim-32b` 本身**不会**：正交化之后职责提示词由客户端随请求发送。
///
/// 这个判断必须跟着**最终解析出的模型**走，不能跟着工种名走。工种绑定成
/// `someim-32b` 却仍按「托管」处理，客户端就会把自己那份提示词也省掉，于是
/// Worker 只剩边界段落、完全不知道自己是干什么的——比双重注入更糟。
pub fn hosts_job_prompt(model: &str) -> bool {
    is_legacy_trade_alias(model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_parse_and_round_trip() {
        for tier in WorkerTier::ALL {
            assert_eq!(WorkerTier::parse(tier.as_str()), Some(tier));
        }
        assert_eq!(WorkerTier::parse("BASIC"), Some(WorkerTier::Standard));
        assert_eq!(WorkerTier::parse("nope"), None);
    }

    #[test]
    fn deep_is_understood_as_the_expert_tier() {
        // `deep` 当年既是职责也是档位。正交化之后它作为档位别名继续可用，
        // 已保存的流程和别人写的脚本不该在这次改动里断掉。
        assert_eq!(WorkerTier::parse("deep"), Some(WorkerTier::Expert));
        assert!(WorkerTier::Expert.requires_admission());
        assert!(!WorkerTier::Standard.requires_admission());
        assert!(!WorkerTier::Advanced.requires_admission());
    }

    #[test]
    fn the_cheapest_tier_is_the_default_and_escalation_has_a_ceiling() {
        assert_eq!(WorkerTier::default(), WorkerTier::Standard);
        assert_eq!(WorkerTier::Standard.next(), Some(WorkerTier::Advanced));
        assert_eq!(WorkerTier::Advanced.next(), Some(WorkerTier::Expert));
        // 升无可升时报告失败，而不是原地重试更贵的东西。
        assert_eq!(WorkerTier::Expert.next(), None);
    }

    #[test]
    fn legacy_trade_aliases_collapse_to_the_base_model() {
        for trade in LEGACY_HOSTED_TRADES {
            let alias = format!("{HOSTED_BASE_MODEL}-{}", trade.replace('_', "-"));
            assert_eq!(
                normalize_hosted_model(&alias),
                HOSTED_BASE_MODEL,
                "{alias} should collapse to the base tier"
            );
        }
    }

    #[test]
    fn function_specific_hosted_models_are_left_alone() {
        // 这两个不是工种别名：一个是上下文压缩器，一个是命令安全裁判。
        // 它们存在的理由与工种提示词无关，归一化到基础档会直接改坏它们。
        for model in [
            "someim-32b-compressor",
            "someim-32b-security-guard",
            "someim-security-guard",
            "deepseek-v4-flash",
            "glm-5",
            "someim-32b",
        ] {
            assert_eq!(
                normalize_hosted_model(model),
                model,
                "{model} must not be rewritten"
            );
        }
    }

    #[test]
    fn only_the_legacy_aliases_carry_a_server_side_job_prompt() {
        for trade in LEGACY_HOSTED_TRADES {
            let alias = format!("{HOSTED_BASE_MODEL}-{}", trade.replace('_', "-"));
            assert!(hosts_job_prompt(&alias), "{alias} is relay-hosted");
        }
        // 归一之后的基础档模型自己不带职责提示词。当成带的，客户端会把自己
        // 那份也省掉，Worker 就成了没有职责描述的空壳。
        assert!(!hosts_job_prompt(HOSTED_BASE_MODEL));
        for model in [
            "someim-32b-compressor",
            "someim-32b-security-guard",
            "deepseek-v4-flash",
            "gpt-5.6-sol",
            "opus-5",
            "glm-5",
        ] {
            assert!(!hosts_job_prompt(model), "{model} is not relay-hosted");
        }
    }

    #[test]
    fn budgets_match_the_shared_contract() {
        // 与 Xedit `AgentWorkerTierModels` 的 standardWindow / extendedWindow 对齐。
        assert_eq!(WorkerTier::Standard.context_budget(), 131_072);
        assert_eq!(WorkerTier::Advanced.context_budget(), 262_144);
        assert_eq!(WorkerTier::Expert.context_budget(), 262_144);
    }

    #[test]
    fn hosted_defaults_match_the_shared_contract() {
        // 同一个网关、同一批账号：两个客户端必须把同一档解析到同一个模型。
        assert_eq!(WorkerTier::Standard.default_hosted_model(), "someim-32b");
        assert_eq!(
            WorkerTier::Advanced.default_hosted_model(),
            "deepseek-v4-flash"
        );
        assert_eq!(WorkerTier::Expert.default_hosted_model(), "gpt-5.6-sol");
    }
}
