//! Durable run boundaries. A pending call is evidence of uncertainty, never a replay request.

use serde::{Deserialize, Serialize};

use crate::agent::{AgentError, AgentOutcome};
use crate::types::{Message, Role};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointStatus {
    #[default]
    Running,
    Completed,
    Partial,
    Failed,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CheckpointMetadata {
    #[serde(default)]
    pub required_verifications: Vec<String>,
    #[serde(default)]
    pub verification_evidence: Vec<VerificationEvidence>,
    #[serde(default)]
    pub verification_baseline: Option<String>,
    pub status: CheckpointStatus,
    pub turns: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// These calls may have started. Their side effects must be reconciled before any retry.
    pub pending_call_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct VerificationEvidence {
    pub snapshot_id: Option<String>,
    pub command: String,
    pub status: crate::tools::VerificationStatus,
}

#[derive(Clone, Debug)]
pub struct RunCheckpoint {
    pub metadata: CheckpointMetadata,
    pub messages: Vec<Message>,
}

pub trait CheckpointSink: Send + Sync {
    fn required_verifications(&self) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }
    fn verification_evidence(&self) -> Result<Vec<VerificationEvidence>, String> {
        Ok(Vec::new())
    }
    /// A guard held for the entire run. Dropping a cancelled run releases it.
    fn acquire_run(&self) -> Result<Option<Box<dyn Send + Sync>>, String> {
        Ok(None)
    }
    /// An unfinished run retains its original verification baseline.
    fn verification_baseline(&self) -> Result<Option<String>, String> {
        Ok(None)
    }
    /// Must durably save before returning. Failure prevents the next side effect.
    fn save(&self, checkpoint: &RunCheckpoint) -> Result<(), String>;
}

pub(crate) struct CheckpointRecorder<'a> {
    sink: Option<&'a dyn CheckpointSink>,
    latest: Option<RunCheckpoint>,
    verification_baseline: Option<String>,
    stream_prefix: Option<usize>,
    stream_saved: bool,
    stream_dirty: bool,
    pending_stream_bytes: usize,
    evidence_source: Option<&'a crate::tools::ToolRegistry>,
}

pub(crate) const STREAM_CHECKPOINT_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(250);
const STREAM_CHECKPOINT_BYTES: usize = 16 * 1024;

impl<'a> CheckpointRecorder<'a> {
    pub fn new(sink: Option<&'a dyn CheckpointSink>) -> Self {
        Self {
            sink,
            latest: None,
            verification_baseline: None,
            stream_prefix: None,
            stream_saved: false,
            stream_dirty: false,
            pending_stream_bytes: 0,
            evidence_source: None,
        }
    }

    pub fn initialize_evidence(
        &mut self,
        tools: &'a crate::tools::ToolRegistry,
    ) -> Result<(), AgentError> {
        let evidence = self
            .sink
            .map(|sink| sink.verification_evidence())
            .transpose()
            .map_err(AgentError::Checkpoint)?
            .unwrap_or_default();
        tools.restore_verification_evidence(evidence);
        if let Some(sink) = self.sink {
            tools
                .require_verifications(
                    &sink
                        .required_verifications()
                        .map_err(AgentError::Checkpoint)?,
                )
                .map_err(AgentError::Checkpoint)?;
        }
        self.evidence_source = Some(tools);
        Ok(())
    }

    fn refresh_evidence(&mut self) {
        if let (Some(tools), Some(checkpoint)) = (self.evidence_source, self.latest.as_mut()) {
            let evidence = tools.verification_evidence();
            let required = tools.required_verifications();
            if checkpoint.metadata.required_verifications != required {
                checkpoint.metadata.required_verifications = required;
                self.stream_dirty = true;
            }
            if checkpoint.metadata.verification_evidence != evidence {
                checkpoint.metadata.verification_evidence = evidence;
                self.stream_dirty = true;
            }
        }
    }

    pub fn initialize_verification(&mut self, current: Option<String>) -> Result<(), AgentError> {
        let previous = self
            .sink
            .map(|sink| sink.verification_baseline())
            .transpose()
            .map_err(AgentError::Checkpoint)?
            .flatten();
        self.verification_baseline = previous.or(current);
        Ok(())
    }

    pub fn verification_baseline(&self) -> Option<&str> {
        self.verification_baseline.as_deref()
    }

    pub fn record(
        &mut self,
        messages: &[Message],
        turns: usize,
        input_tokens: u64,
        output_tokens: u64,
    ) -> Result<(), AgentError> {
        let Some(sink) = self.sink else { return Ok(()) };
        let results: std::collections::HashSet<_> = messages
            .iter()
            .filter(|message| message.role == Role::Tool)
            .filter_map(|message| message.tool_call_id.as_deref())
            .collect();
        let pending_call_ids = messages
            .iter()
            .flat_map(|message| &message.tool_calls)
            .filter(|call| !results.contains(call.id.as_str()))
            .map(|call| call.id.clone())
            .collect();
        let checkpoint = RunCheckpoint {
            metadata: CheckpointMetadata {
                required_verifications: self
                    .evidence_source
                    .map(|tools| tools.required_verifications())
                    .unwrap_or_default(),
                verification_evidence: self
                    .evidence_source
                    .map(|tools| tools.verification_evidence())
                    .unwrap_or_default(),
                verification_baseline: self.verification_baseline.clone(),
                status: CheckpointStatus::Running,
                turns,
                input_tokens,
                output_tokens,
                pending_call_ids,
            },
            messages: messages.to_vec(),
        };
        sink.save(&checkpoint).map_err(AgentError::Checkpoint)?;
        self.latest = Some(checkpoint);
        self.stream_prefix = None;
        self.stream_saved = false;
        self.stream_dirty = false;
        self.pending_stream_bytes = 0;
        Ok(())
    }

    pub fn record_stream(
        &mut self,
        delta: Option<&str>,
        turns: usize,
        input_tokens: u64,
        output_tokens: u64,
    ) -> Result<(), AgentError> {
        if self.sink.is_none() {
            return Ok(());
        }
        let checkpoint = self.latest.as_mut().ok_or_else(|| {
            AgentError::Checkpoint("stream started without a durable history boundary".into())
        })?;
        let prefix = *self.stream_prefix.get_or_insert(checkpoint.messages.len());
        if let Some(delta) = delta.filter(|text| !text.is_empty()) {
            self.pending_stream_bytes = self.pending_stream_bytes.saturating_add(delta.len());
            if checkpoint.messages.len() == prefix {
                checkpoint
                    .messages
                    .push(Message::assistant(delta, Vec::new()));
            } else {
                checkpoint.messages[prefix].content.push_str(delta);
            }
        }
        checkpoint.metadata.turns = turns;
        checkpoint.metadata.input_tokens = input_tokens;
        checkpoint.metadata.output_tokens = output_tokens;
        self.stream_dirty = true;
        if !self.stream_saved
            || delta.is_none()
            || self.pending_stream_bytes >= STREAM_CHECKPOINT_BYTES
        {
            self.flush_stream()?;
        }
        Ok(())
    }

    pub fn flush_stream(&mut self) -> Result<(), AgentError> {
        if self.stream_dirty {
            if let (Some(sink), Some(checkpoint)) = (self.sink, self.latest.as_ref()) {
                sink.save(checkpoint).map_err(AgentError::Checkpoint)?;
            }
            self.stream_dirty = false;
            self.pending_stream_bytes = 0;
            self.stream_saved = true;
        }
        Ok(())
    }

    pub fn finish(&mut self, result: &Result<AgentOutcome, AgentError>) -> Result<(), AgentError> {
        self.refresh_evidence();
        if let Ok(outcome) = result {
            self.record(
                &outcome.messages,
                outcome.turns,
                outcome.input_tokens,
                outcome.output_tokens,
            )?;
        }
        if let (Some(sink), Some(checkpoint)) = (self.sink, self.latest.as_mut()) {
            checkpoint.metadata.status = match result {
                Ok(outcome)
                    if matches!(
                        outcome.stop_reason,
                        crate::agent::AgentStopReason::Finished
                            | crate::agent::AgentStopReason::GoalComplete
                    ) =>
                {
                    CheckpointStatus::Completed
                }
                Ok(_) => CheckpointStatus::Partial,
                Err(AgentError::TokenBudgetExceeded { .. }) => CheckpointStatus::Partial,
                Err(_) => CheckpointStatus::Failed,
            };
            sink.save(checkpoint).map_err(AgentError::Checkpoint)?;
            self.stream_dirty = false;
        }
        Ok(())
    }
}

impl Drop for CheckpointRecorder<'_> {
    fn drop(&mut self) {
        // Dropping a cancelled Future must preserve its buffered text before
        // the execution guard releases ownership. Hard kills cannot run Drop.
        self.refresh_evidence();
        if self.flush_stream().is_err() {
            eprintln!("failed to flush cancelled execution checkpoint");
        }
    }
}

impl CheckpointMetadata {
    pub fn recovery_notice(&self) -> Option<Message> {
        if self.status == CheckpointStatus::Completed {
            return None;
        }
        Some(Message::user(format!(
            "[execution-recovery] The previous run stopped with status {:?} after {} model round(s). \
             Its saved tool results describe work that already happened; do not repeat successful writes. \
             Calls without a recorded result: {:?}. Their effects are UNKNOWN, not failed or safe to replay. \
             Inspect the actual files, git diff, or external operation status before retrying. \
             For interrupted foreground children, use list_agent_recoveries and resume_agent to recover the original child or its completed report; do not spawn a replacement that repeats its writes. \
             Continue the outstanding user objective and preserve existing user changes. \
             Previous recorded usage: {} input / {} output tokens.",
            self.status, self.turns, self.pending_call_ids, self.input_tokens, self.output_tokens,
        )))
    }
}

/// Update only execution fields so a checkpoint cannot revert concurrent title/pin changes.
pub struct SessionCheckpointSink {
    pub store: crate::session::SessionStore,
    pub session_id: uuid::Uuid,
}

impl SessionCheckpointSink {
    /// Claim before preparing or persisting the next user message. Keep the
    /// returned sink alive through final persistence, including follow-up runs.
    pub fn claim(self) -> Result<ClaimedSessionCheckpointSink, crate::session::SessionError> {
        let ownership = self.store.acquire_execution(self.session_id)?;
        Ok(ClaimedSessionCheckpointSink {
            inner: self,
            _ownership: ownership,
            active: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }
}

pub struct ClaimedSessionCheckpointSink {
    inner: SessionCheckpointSink,
    _ownership: crate::session::ExecutionGuard,
    active: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

struct ActiveRun(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl Drop for ActiveRun {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

impl CheckpointSink for ClaimedSessionCheckpointSink {
    fn required_verifications(&self) -> Result<Vec<String>, String> {
        self.inner.required_verifications()
    }
    fn verification_evidence(&self) -> Result<Vec<VerificationEvidence>, String> {
        self.inner.verification_evidence()
    }
    fn acquire_run(&self) -> Result<Option<Box<dyn Send + Sync>>, String> {
        self.active
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .map_err(|_| "checkpoint sink already has an active run".to_owned())?;
        Ok(Some(Box::new(ActiveRun(self.active.clone()))))
    }

    fn verification_baseline(&self) -> Result<Option<String>, String> {
        self.inner.verification_baseline()
    }

    fn save(&self, checkpoint: &RunCheckpoint) -> Result<(), String> {
        self.inner.save(checkpoint)
    }
}

impl CheckpointSink for SessionCheckpointSink {
    fn required_verifications(&self) -> Result<Vec<String>, String> {
        self.store
            .load(self.session_id)
            .map(|session| {
                session
                    .execution_checkpoint
                    .filter(|checkpoint| checkpoint.status != CheckpointStatus::Completed)
                    .map(|checkpoint| checkpoint.required_verifications)
                    .unwrap_or_default()
            })
            .map_err(|error| error.to_string())
    }
    fn verification_evidence(&self) -> Result<Vec<VerificationEvidence>, String> {
        self.store
            .load(self.session_id)
            .map(|session| {
                session
                    .execution_checkpoint
                    .filter(|checkpoint| checkpoint.status != CheckpointStatus::Completed)
                    .map(|checkpoint| checkpoint.verification_evidence)
                    .unwrap_or_default()
            })
            .map_err(|error| error.to_string())
    }
    fn acquire_run(&self) -> Result<Option<Box<dyn Send + Sync>>, String> {
        self.store
            .acquire_execution(self.session_id)
            .map(|guard| Some(Box::new(guard) as Box<dyn Send + Sync>))
            .map_err(|error| error.to_string())
    }
    fn verification_baseline(&self) -> Result<Option<String>, String> {
        let session = self
            .store
            .load(self.session_id)
            .map_err(|error| error.to_string())?;
        Ok(session
            .execution_checkpoint
            .filter(|checkpoint| checkpoint.status != CheckpointStatus::Completed)
            .and_then(|checkpoint| checkpoint.verification_baseline))
    }

    fn save(&self, checkpoint: &RunCheckpoint) -> Result<(), String> {
        self.store
            .update(self.session_id, |session| {
                session.messages = checkpoint.messages.clone();
                session.execution_checkpoint = Some(checkpoint.metadata.clone());
            })
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod stream_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod unknown_effect_tests;
