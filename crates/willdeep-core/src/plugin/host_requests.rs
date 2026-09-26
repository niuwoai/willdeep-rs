//! 插件 MCP 进程反向请求宿主：接口与按插件绑定的适配层。
//!
//! 与 macOS 宿主 `AgentMCPHostRequests` 同一份约定（方法名、参数与结果形状、
//! 错误码）。真正的出图、问模型在 CLI 里实现（它才有配置、凭据与 provider），
//! core 只负责把「哪个插件、它声明了哪些权限」连同请求一起递过去。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use super::manifest::PluginPermission;
use crate::mcp::{HostRequestError, HostRequestHandler};

/// 出图：参数 `{prompt, provider?, model?, size?, referenceImagePaths?}`，
/// 结果 `{mediaURL, filePath, model, providerID}`。需要 `ai.image`。
pub const IMAGE_GENERATE: &str = "willdeep/images/generate";
/// 问一次模型（不流式）：形状同页面桥 `ai.complete`，结果
/// `{text, model, providerID, toolCalls}`。需要 `ai.chat`。
pub const AI_COMPLETE: &str = "willdeep/ai/complete";
/// 宿主代管 TTS。macOS 宿主与本宿主目前都没有实现，不宣告——插件据此报
/// 「宿主不支持」而不是干等。
pub const AUDIO_SYNTHESIZE: &str = "willdeep/audio/synthesize";

/// 发出请求的插件是谁、声明了哪些权限。权限读自清单，不读请求。
#[derive(Clone, Debug)]
pub struct PluginRequestContext {
    pub plugin_id: String,
    pub permissions: Vec<PluginPermission>,
}

impl PluginRequestContext {
    pub fn require(&self, permission: PluginPermission) -> Result<(), HostRequestError> {
        if self.permissions.contains(&permission) {
            Ok(())
        } else {
            Err(HostRequestError::failed(format!(
                "plugin `{}` does not declare the `{}` permission",
                self.plugin_id,
                permission.as_str()
            )))
        }
    }
}

#[async_trait]
pub trait PluginHostRequests: Send + Sync {
    /// 实现了的方法。只有这些会在 initialize 里宣告。
    fn methods(&self) -> Vec<String>;
    async fn handle(
        &self,
        context: &PluginRequestContext,
        method: &str,
        params: Value,
    ) -> Result<Value, HostRequestError>;
}

/// 把宿主级处理器绑到某一个插件上，交给那个插件的 MCP 连接。
pub(crate) struct ScopedHostRequests {
    pub(crate) context: PluginRequestContext,
    pub(crate) inner: Arc<dyn PluginHostRequests>,
}

#[async_trait]
impl HostRequestHandler for ScopedHostRequests {
    fn methods(&self) -> Vec<String> {
        self.inner.methods()
    }

    async fn handle(&self, method: &str, params: Value) -> Result<Value, HostRequestError> {
        self.inner.handle(&self.context, method, params).await
    }
}
