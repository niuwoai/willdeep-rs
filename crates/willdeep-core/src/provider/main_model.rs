//! 会话主模型的共享句柄。
//!
//! `/model` 只换主 Agent 的 Provider 时，启动那一刻抓走主模型的那些「兜底」
//! ——压缩兜底、分类器兜底、标题模型、非 some.im 的安全裁判、没有托管绑定的
//! Worker、专家档回落——都还停在旧模型上：用户以为换了，其实只换了一半。
//!
//! 办法是让「定义为跟主模型一样」的地方不再抓一份快照，而是拿一个
//! [`MainModelHandle::follower`]：它每次请求时才去读句柄里**当前**的主模型。
//! 显式配置了模型的（`judge_model`、`compressor_model`、`[subagents.*] model`
//! ……）照旧是自己那一份，不跟着切。

use std::sync::{Arc, RwLock};

use async_trait::async_trait;

use super::{Provider, ProviderError, ProviderEventSink, ProviderIdentity};
use crate::types::{Completion, Message, ToolDefinition};

/// 主模型所在的那一格。主 Agent 与所有 follower 共用同一格。
#[derive(Clone)]
pub struct MainModelHandle {
    current: Arc<RwLock<Arc<dyn Provider>>>,
}

impl MainModelHandle {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self {
            current: Arc::new(RwLock::new(provider)),
        }
    }

    /// 此刻的主模型 Provider。
    pub fn current(&self) -> Result<Arc<dyn Provider>, ProviderError> {
        self.current
            .read()
            .map(|provider| provider.clone())
            .map_err(|_| ProviderError::InvalidResponse("provider lock poisoned".to_owned()))
    }

    /// 换主模型。所有 follower 从下一次请求起跟着换。
    pub fn set_model(&self, model: &str) -> Result<(), ProviderError> {
        let configured = self.current()?.with_model(model)?;
        *self
            .current
            .write()
            .map_err(|_| ProviderError::InvalidResponse("provider lock poisoned".to_owned()))? =
            configured;
        Ok(())
    }

    /// 此刻主模型的名字；Provider 说不上来（测试替身）时是 `None`。
    pub fn model(&self) -> Option<String> {
        self.current()
            .ok()
            .and_then(|provider| provider.ledger_identity())
            .map(|identity| identity.model)
    }

    /// 一个始终转发给当前主模型的 Provider。
    pub fn follower(&self) -> Arc<dyn Provider> {
        Arc::new(FollowMainModel {
            handle: self.clone(),
        })
    }
}

/// 见 [`MainModelHandle::follower`]。
struct FollowMainModel {
    handle: MainModelHandle,
}

#[async_trait]
impl Provider for FollowMainModel {
    fn ledger_identity(&self) -> Option<ProviderIdentity> {
        self.handle.current().ok()?.ledger_identity()
    }

    /// 显式换模型（例如 Runtime 给某个 Worker 指定模型）就不再跟随：返回的是
    /// 按当前主模型端点配好的一份独立 Provider。
    fn with_model(&self, model: &str) -> Result<Arc<dyn Provider>, ProviderError> {
        self.handle.current()?.with_model(model)
    }

    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<Completion, ProviderError> {
        self.handle.current()?.complete(messages, tools).await
    }

    async fn complete_with_events(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        events: &dyn ProviderEventSink,
    ) -> Result<Completion, ProviderError> {
        self.handle
            .current()?
            .complete_with_events(messages, tools, events)
            .await
    }
}

/// 一个 Provider 背后的模型名，给事件与提示用；说不上来就是 `None`。
pub fn provider_model(provider: &dyn Provider) -> Option<String> {
    provider.ledger_identity().map(|identity| identity.model)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 只会报自己模型名的替身：`with_model` 换出一个新名字的自己。
    struct Named(String);

    #[async_trait]
    impl Provider for Named {
        fn ledger_identity(&self) -> Option<ProviderIdentity> {
            Some(ProviderIdentity {
                provider: "test".to_owned(),
                model: self.0.clone(),
                local: false,
            })
        }

        fn with_model(&self, model: &str) -> Result<Arc<dyn Provider>, ProviderError> {
            Ok(Arc::new(Named(model.to_owned())))
        }

        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> Result<Completion, ProviderError> {
            Ok(Completion {
                content: self.0.clone(),
                ..Completion::default()
            })
        }
    }

    #[tokio::test]
    async fn followers_answer_with_whatever_the_main_model_is_now() {
        let handle = MainModelHandle::new(Arc::new(Named("old".to_owned())));
        let follower = handle.follower();
        assert_eq!(provider_model(follower.as_ref()).as_deref(), Some("old"));

        handle.set_model("new").expect("switch main model");

        assert_eq!(handle.model().as_deref(), Some("new"));
        assert_eq!(provider_model(follower.as_ref()).as_deref(), Some("new"));
        let answer = follower.complete(&[], &[]).await.expect("complete");
        assert_eq!(answer.content, "new", "请求必须落到切换后的主模型上");
    }

    #[test]
    fn an_explicit_model_on_a_follower_stops_following() {
        let handle = MainModelHandle::new(Arc::new(Named("old".to_owned())));
        let pinned = handle.follower().with_model("pinned").expect("pin");
        handle.set_model("new").expect("switch main model");
        assert_eq!(provider_model(pinned.as_ref()).as_deref(), Some("pinned"));
    }
}
