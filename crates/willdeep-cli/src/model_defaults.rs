//! 客户端写死的缺省模型名，集中在这一处。
//!
//! 这些名字以前散在 main / harness / onboarding 各处，改一个漏一个：会话根
//! 模型的缺省在三个地方各写一遍 `glm-5`，哪天网关换了缺省，就会出现「新装的
//! 用户是 A、零配置启动是 B」。这里只放**客户端自己决定**的缺省；网关档位表
//! （`someim-32b` / `deepseek-v4-flash` / `gpt-5.6-sol`）与 macOS 版共用，仍在
//! `willdeep_core::worker_tier`。

/// some.im 会话没写模型名时的根模型，也是浏览器登录后网关没给
/// `standard_model` 时新装机的缺省。
pub(crate) const SOMEIM_DEFAULT_MODEL: &str = "glm-5";

/// Anthropic 会话没写模型名时的根模型。
pub(crate) const ANTHROPIC_DEFAULT_MODEL: &str = "claude-sonnet-4-5";

/// some.im 上安全裁判的缺省：安全策略托管在网关，服务端能收紧而不必发客户端。
/// 它是推理模型，先写一长段私有推理再给裁决，所以裁判的输出预算不能卡紧（见
/// `willdeep_core::judge`）。与 macOS 版同一个别名。
pub(crate) const SOMEIM_SECURITY_GUARD_MODEL: &str = "someim-security-guard";

/// some.im 上上下文压缩的缺省：网关托管的压缩器，固定压缩指令在服务端以
/// replace 模式注入（muchtoken docs/someim-32b-compressor.md），按 flash 档计价。
pub(crate) const SOMEIM_CONTEXT_COMPRESSOR_MODEL: &str = "someim-32b-compressor";

/// some.im 纯文本根模型遇到图片时，先用它把图片描述成文字。
pub(crate) const SOMEIM_VISION_FALLBACK_MODEL: &str = "qwen3-vl-plus";

/// 会话配置没写 `context_window` 时的缺省窗口。
pub(crate) const DEFAULT_CONTEXT_WINDOW: u64 = 128_000;

/// `someim-*` 虚拟模型（someim-32b、someim-code-*、someim-auto-*…）名字里只有
/// 档位、没有上游模型，看不出窗口；它们背后的上游现在都是 256K 级，按 128K
/// 算会过早压缩。与 macOS 版同一个缺省。
pub(crate) const SOMEIM_VIRTUAL_MODEL_CONTEXT_WINDOW: u64 = 262_144;

/// 没有显式配置时，按模型名给出的缺省上下文窗口。
pub(crate) fn default_context_window(model: &str) -> u64 {
    if model.to_ascii_lowercase().starts_with("someim-") {
        SOMEIM_VIRTUAL_MODEL_CONTEXT_WINDOW
    } else {
        DEFAULT_CONTEXT_WINDOW
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn someim_virtual_models_default_to_256k() {
        for model in ["someim-32b", "someim-code-high", "SomeIM-Auto-Flash"] {
            assert_eq!(default_context_window(model), 262_144, "{model}");
        }
        assert_eq!(default_context_window("glm-5"), 128_000);
    }
}
