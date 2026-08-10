use super::*;

#[derive(Clone)]
pub(crate) enum RemoteGate {
    Approval {
        id: uuid::Uuid,
        description: String,
        always_allow_available: bool,
    },
    Question {
        id: uuid::Uuid,
        question: String,
        options: Vec<String>,
        multi_select: bool,
    },
}

impl RemoteGate {
    pub(crate) fn id(&self) -> uuid::Uuid {
        match self {
            Self::Approval { id, .. } | Self::Question { id, .. } => *id,
        }
    }
}

pub(crate) struct RuntimeSnapshot {
    pub attention: Vec<willdeep_core::AttentionItem>,
    pub gates: Vec<RemoteGate>,
}

pub(crate) async fn runtime_snapshot(home: &Path) -> Result<RuntimeSnapshot> {
    let paths = DaemonPaths::new(home);
    let state = match load_state(&paths.state) {
        Ok(state) if probe(&state).await.is_ok() => state,
        _ => {
            return Ok(RuntimeSnapshot {
                attention: Vec::new(),
                gates: Vec::new(),
            });
        }
    };
    let tasks: Vec<RuntimeTask> = authorized_get(&state, "/v1/tasks").await?;
    let interactions: Vec<RuntimeInteraction> = authorized_get(&state, "/v1/interactions").await?;
    let mut attention = tasks
        .into_iter()
        .filter(|task| task.status != RuntimeTaskStatus::Queued)
        .map(runtime_task_attention)
        .collect::<Vec<_>>();
    let mut gates = Vec::new();
    for interaction in interactions {
        let item = match &interaction.kind {
            InteractionKind::Approval { description, .. } => {
                gates.push(RemoteGate::Approval {
                    id: interaction.id,
                    description: description.clone(),
                    always_allow_available: matches!(
                        interaction.kind,
                        InteractionKind::Approval {
                            always_allow_available: true,
                            ..
                        }
                    ),
                });
                willdeep_core::AttentionItem::approval(description.clone())
            }
            InteractionKind::Question {
                question,
                options,
                multi_select,
            } => {
                gates.push(RemoteGate::Question {
                    id: interaction.id,
                    question: question.clone(),
                    options: options.clone(),
                    multi_select: *multi_select,
                });
                willdeep_core::AttentionItem::question(question.clone())
            }
        };
        attention.push(willdeep_core::AttentionItem {
            id: format!("runtime-interaction:{}", interaction.id),
            title: item.title,
            detail: format!("Runtime task {}\n{}", interaction.task_id, item.detail),
            ..item
        });
    }
    Ok(RuntimeSnapshot { attention, gates })
}

fn runtime_task_attention(task: RuntimeTask) -> willdeep_core::AttentionItem {
    let status = match task.status {
        RuntimeTaskStatus::Queued | RuntimeTaskStatus::Running => {
            willdeep_core::RuntimeStatus::Working
        }
        RuntimeTaskStatus::Cancelling => willdeep_core::RuntimeStatus::Working,
        RuntimeTaskStatus::WaitingApproval => willdeep_core::RuntimeStatus::WaitingApproval,
        RuntimeTaskStatus::WaitingAnswer => willdeep_core::RuntimeStatus::WaitingAnswer,
        RuntimeTaskStatus::Completed => willdeep_core::RuntimeStatus::Done,
        RuntimeTaskStatus::Failed | RuntimeTaskStatus::Interrupted => {
            willdeep_core::RuntimeStatus::Failed
        }
        RuntimeTaskStatus::Cancelled => willdeep_core::RuntimeStatus::Cancelled,
    };
    willdeep_core::AttentionItem {
        id: format!("runtime-task:{}", task.id),
        source: willdeep_core::AttentionSource::BackgroundShell,
        status,
        title: format!("Runtime task {}", task.id),
        detail: format!(
            "Workspace: {}\nStatus: {:?}\nPID: {}\nError: {}",
            task.workspace.display(),
            task.status,
            task.pid
                .map_or_else(|| "-".to_owned(), |pid| pid.to_string()),
            task.error.unwrap_or_default()
        ),
        elapsed_millis: task
            .started_at
            .map(|started| now().saturating_sub(started).saturating_mul(1_000)),
    }
}

pub(crate) async fn resolve_remote_approval(
    home: &Path,
    id: uuid::Uuid,
    decision: willdeep_core::ApprovalDecision,
) -> Result<()> {
    let resolution = match decision {
        willdeep_core::ApprovalDecision::AllowOnce => InteractionResolution::AllowOnce,
        willdeep_core::ApprovalDecision::Deny => InteractionResolution::Deny,
        willdeep_core::ApprovalDecision::AlwaysAllow => InteractionResolution::AlwaysAllow,
    };
    resolve_interaction_quiet(home, id, resolution).await
}

pub(crate) async fn answer_remote_question(
    home: &Path,
    id: uuid::Uuid,
    answer: Option<String>,
) -> Result<()> {
    resolve_interaction_quiet(home, id, InteractionResolution::Answer(answer)).await
}

pub(crate) async fn cancel_remote_task(home: &Path, id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    let response = client()
        .post(format!("http://{}/v1/tasks/{id}/stop", state.address))
        .header(TOKEN_HEADER, &state.token)
        .send()
        .await?;
    if !response.status().is_success() {
        bail!(
            "Runtime rejected task cancellation: HTTP {}",
            response.status()
        );
    }
    Ok(())
}

async fn resolve_interaction_quiet(
    home: &Path,
    id: uuid::Uuid,
    resolution: InteractionResolution,
) -> Result<()> {
    let state = ensure_running(home).await?;
    let response = client()
        .post(format!(
            "http://{}/v1/interactions/{id}/resolve",
            state.address
        ))
        .header(TOKEN_HEADER, &state.token)
        .json(&ResolveInteraction { resolution })
        .send()
        .await?;
    if !response.status().is_success() {
        bail!(
            "Runtime rejected interaction resolution: HTTP {}",
            response.status()
        );
    }
    Ok(())
}
