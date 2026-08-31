//! Test doubles shared by the submodule test suites: a provider that returns
//! a canned report, one that records which model it was reconfigured to, an
//! event sink that keeps everything it saw, and a judge that always allows.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::agent::{AgentEvent, EventSink};
use crate::background::BackgroundTaskRegistry;
use crate::judge::{JudgeRequest, JudgeVerdict, SafetyJudge};
use crate::provider::{Provider, ProviderError};
use crate::types::{Completion, Message, ToolDefinition};

use super::{SubagentCatalog, builtin_profiles};

pub(super) struct ReportProvider;

#[derive(Clone)]
pub(super) struct ModelProvider {
    pub(super) model: String,
    pub(super) seen: Arc<Mutex<Vec<String>>>,
}

#[derive(Default)]
pub(super) struct CaptureSink(pub(super) Mutex<Vec<AgentEvent>>);

#[async_trait]
impl EventSink for CaptureSink {
    async fn emit(&self, event: AgentEvent) {
        self.0.lock().unwrap().push(event);
    }
}

#[async_trait]
impl Provider for ReportProvider {
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<Completion, ProviderError> {
        assert!(messages[0].content.contains("cannot spawn another agent"));
        assert!(tools.iter().all(|tool| tool.name != "spawn_agent"));
        Ok(Completion {
            content: "subagent report".to_owned(),
            tool_calls: Vec::new(),
            finish_reason: Some("stop".to_owned()),
            usage: None,
        })
    }
}

#[async_trait]
impl Provider for ModelProvider {
    fn with_model(&self, model: &str) -> Result<Arc<dyn Provider>, ProviderError> {
        Ok(Arc::new(Self {
            model: model.to_owned(),
            seen: self.seen.clone(),
        }))
    }

    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
    ) -> Result<Completion, ProviderError> {
        self.seen.lock().unwrap().push(self.model.clone());
        Ok(Completion {
            content: format!("report from {}", self.model),
            tool_calls: Vec::new(),
            finish_reason: Some("stop".to_owned()),
            usage: None,
        })
    }
}

/// Stands in for the gateway's `someim-security-guard`: a verifier that
/// writes (as every honest build or test does) is reviewed, not banned.
pub(super) struct AllowingJudge;

#[async_trait]
impl SafetyJudge for AllowingJudge {
    async fn judge(&self, _request: JudgeRequest) -> JudgeVerdict {
        JudgeVerdict::Allow
    }

    fn model(&self) -> &str {
        "test-judge"
    }
}

/// A catalog over a fresh temp workspace, wired to [`ReportProvider`].
pub(super) fn fixture() -> (SubagentCatalog, PathBuf) {
    let root = std::env::temp_dir().join(format!("willdeep-subagent-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("workspace");
    let provider: Arc<dyn Provider> = Arc::new(ReportProvider);
    let background = Arc::new(BackgroundTaskRegistry::default());
    (
        SubagentCatalog::new(&root, builtin_profiles(provider), background),
        root,
    )
}
