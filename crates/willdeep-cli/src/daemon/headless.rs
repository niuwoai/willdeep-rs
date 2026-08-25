use super::*;

const EVENT_PAGE_LIMIT: usize = 200;

#[derive(Clone, Debug)]
pub(crate) struct HeadlessRuntimeOutcome {
    pub session_id: uuid::Uuid,
    pub final_text: String,
    pub turns: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeadlessRuntimeStatus {
    WaitingApproval,
    WaitingAnswer,
    Failed(Option<willdeep_runtime_protocol::FailureDomain>),
    Cancelled,
    Interrupted,
}

pub(crate) struct HeadlessRuntimeRequest {
    pub session_id: uuid::Uuid,
    pub workspace: PathBuf,
    pub profile: Option<String>,
    pub model: Option<String>,
    pub prompt: String,
    pub attachments: Vec<willdeep_core::MessageAttachment>,
}

pub(crate) async fn execute_headless_turn(
    home: &Path,
    request: HeadlessRuntimeRequest,
    mut on_event: impl FnMut(serde_json::Value),
) -> Result<Result<HeadlessRuntimeOutcome, HeadlessRuntimeStatus>> {
    let mut cursor = runtime_event_head(home).await?;
    ensure_runtime_session(
        home,
        request.session_id,
        &request.workspace,
        request.profile,
        request.model,
    )
    .await?;
    let submitted = submit_runtime_turn(
        home,
        request.session_id,
        request.prompt,
        request.attachments,
    )
    .await?;
    let mut task_id = None;
    let mut completed_payload = None;

    'poll: loop {
        let state = ensure_running(home).await?;
        let client = runtime_client(&state)?;
        let turn = match client.turn(submitted.id).await {
            Ok(response) => api_data(response)?,
            Err(error) => {
                if wait_for_runtime_handoff(home, &state.token).await {
                    continue;
                }
                return Err(error.into());
            }
        };
        task_id = turn.active_task_id.or(task_id);
        loop {
            let events = match client
                .events(&willdeep_runtime_protocol::EventListParams {
                    after: cursor,
                    limit: EVENT_PAGE_LIMIT,
                })
                .await
            {
                Ok(response) => api_data(response)?,
                Err(error) => {
                    if wait_for_runtime_handoff(home, &state.token).await {
                        continue 'poll;
                    }
                    return Err(error.into());
                }
            };
            let page_is_full = events.len() == EVENT_PAGE_LIMIT;
            task_id = events
                .iter()
                .find(|event| message_uuid(&event.message, "turn_id") == Some(submitted.id))
                .and_then(|event| message_uuid(&event.message, "task_id"))
                .or(task_id);
            for event in events {
                cursor = cursor.max(event.sequence);
                if event.kind == "turn.started"
                    && message_uuid(&event.message, "turn_id") == Some(submitted.id)
                {
                    task_id = message_uuid(&event.message, "task_id").or(task_id);
                }
                if event.kind != "task.output" || message_uuid(&event.message, "task_id") != task_id
                {
                    continue;
                }
                let Some((_, payload)) = event.message.split_once(' ') else {
                    continue;
                };
                let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
                    continue;
                };
                if value.get("type").and_then(serde_json::Value::as_str) == Some("completed") {
                    completed_payload = Some(value.clone());
                } else {
                    on_event(value);
                }
            }
            if !page_is_full {
                break;
            }
        }

        use willdeep_runtime_protocol::TurnStatus;
        match turn.status {
            TurnStatus::Completed => {
                let (final_text, turns) = completion_values(completed_payload.as_ref())
                    .unwrap_or_else(|| (session_final_text(home, request.session_id), 0));
                return Ok(Ok(HeadlessRuntimeOutcome {
                    session_id: request.session_id,
                    final_text,
                    turns,
                }));
            }
            TurnStatus::WaitingApproval => {
                return Ok(Err(HeadlessRuntimeStatus::WaitingApproval));
            }
            TurnStatus::WaitingAnswer => return Ok(Err(HeadlessRuntimeStatus::WaitingAnswer)),
            TurnStatus::Failed => {
                let failure_domain = match task_id {
                    Some(id) => match client.task(id).await {
                        Ok(response) => api_data(response)?.failure_domain,
                        Err(error) => {
                            if wait_for_runtime_handoff(home, &state.token).await {
                                continue 'poll;
                            }
                            return Err(error.into());
                        }
                    },
                    None => None,
                };
                return Ok(Err(HeadlessRuntimeStatus::Failed(failure_domain)));
            }
            TurnStatus::Cancelled => return Ok(Err(HeadlessRuntimeStatus::Cancelled)),
            TurnStatus::Interrupted => return Ok(Err(HeadlessRuntimeStatus::Interrupted)),
            TurnStatus::Queued | TurnStatus::Running => {}
        }

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                let _ = client.stop_turn(submitted.id, uuid::Uuid::new_v4()).await;
                return Ok(Err(HeadlessRuntimeStatus::Cancelled));
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    }
}

async fn wait_for_runtime_handoff(home: &Path, previous_token: &str) -> bool {
    let state_path = DaemonPaths::new(home).state;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        if let Ok(state) = load_state(&state_path)
            && state.token != previous_token
            && probe(&state).await.is_ok()
        {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

fn completion_values(value: Option<&serde_json::Value>) -> Option<(String, usize)> {
    let value = value?;
    Some((
        value.get("text")?.as_str()?.to_owned(),
        value
            .get("turns")
            .and_then(serde_json::Value::as_u64)
            .and_then(|turns| usize::try_from(turns).ok())
            .unwrap_or_default(),
    ))
}

fn session_final_text(home: &Path, session_id: uuid::Uuid) -> String {
    willdeep_core::SessionStore::new(home)
        .load(session_id)
        .ok()
        .and_then(|session| {
            session
                .messages
                .iter()
                .rev()
                .find(|message| {
                    message.role == willdeep_core::Role::Assistant
                        && !message.content.trim().is_empty()
                })
                .map(|message| message.content.clone())
        })
        .unwrap_or_default()
}

fn message_uuid(message: &str, key: &str) -> Option<uuid::Uuid> {
    let prefix = format!("{key}=");
    message
        .split_whitespace()
        .find_map(|part| part.strip_prefix(&prefix))?
        .parse()
        .ok()
}

fn api_data<T>(response: willdeep_runtime_protocol::ApiResponse<T>) -> Result<T> {
    match response {
        willdeep_runtime_protocol::ApiResponse::Ok { data, .. } => Ok(data),
        willdeep_runtime_protocol::ApiResponse::Error { error, .. } => {
            bail!("Runtime API error: {}", error.message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_named_runtime_ids_and_completion_payloads() {
        let turn = uuid::Uuid::new_v4();
        let task = uuid::Uuid::new_v4();
        let message = format!(
            "session_id={} turn_id={turn} task_id={task}",
            uuid::Uuid::new_v4()
        );
        assert_eq!(message_uuid(&message, "turn_id"), Some(turn));
        assert_eq!(message_uuid(&message, "task_id"), Some(task));
        assert_eq!(message_uuid(&message, "agent_id"), None);
        let value = serde_json::json!({"type":"completed","text":"done","turns":3});
        assert_eq!(
            completion_values(Some(&value)),
            Some(("done".to_owned(), 3))
        );
    }
}
