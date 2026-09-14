use super::*;
use futures_util::{StreamExt, stream};

const MAX_PARALLEL_READS: usize = 4;
const RULE_REFRESH_NOTICE: &str = "Applicable project instructions changed. Read the refreshed instructions and reissue this read-only batch on the next round.";

type ReadBatchResults = Option<VecDeque<Result<String, ToolError>>>;

impl Agent {
    /// Only entirely read-only batches qualify. A mixed batch retains its exact
    /// execution order, and arbitrary lifecycle hooks disable parallelism.
    pub(super) fn parallel_reads<'a>(
        &'a self,
        calls: &'a [ToolCall],
        rules: &'a mut crate::project_rules::ProjectRules,
    ) -> futures_util::future::BoxFuture<'a, ReadBatchResults> {
        Box::pin(async move {
            if calls.len() < 2
                || !calls
                    .iter()
                    .all(|call| self.tools.allows_parallel_read(call))
            {
                return None;
            }
            let mut changed = false;
            let mut failures = Vec::with_capacity(calls.len());
            for call in calls {
                match rules.before_call(call) {
                    Ok(value) => {
                        changed |= value;
                        failures.push(None);
                    }
                    Err(error) => failures.push(Some(ToolError::Io(error))),
                }
            }
            let results = stream::iter(calls.iter().cloned().zip(failures))
                .map(|(call, failure)| async move {
                    self.sink
                        .emit(AgentEvent::ToolRequested(call.clone()))
                        .await;
                    if let Some(error) = failure {
                        return Err(error);
                    }
                    if changed {
                        return Err(ToolError::HookDenied(RULE_REFRESH_NOTICE.into()));
                    }
                    self.execute_tool(&call).await
                })
                .buffered(MAX_PARALLEL_READS)
                .collect()
                .await;
            Some(results)
        })
    }
}
