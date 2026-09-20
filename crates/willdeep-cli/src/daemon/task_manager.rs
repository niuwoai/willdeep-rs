use super::*;

impl TaskManager {
    /// 用户在本轮进行中的插话：送进该会话正在跑的任务，返回收下它的任务；
    /// 没有在途任务返回 None，由客户端排队等本轮结束。
    pub(super) fn steer(&self, session_id: uuid::Uuid, message: String) -> Option<uuid::Uuid> {
        self.steering.steer(session_id, message)
    }

    pub(super) fn open(options: TaskManagerOptions) -> Result<Self> {
        let TaskManagerOptions {
            path,
            interactions_path,
            home,
            events,
            agents,
            sessions,
            turn_scheduler,
            runtime_url,
            runtime_token,
        } = options;
        let mut tasks = load_tasks(&path)?;
        let mut interactions = load_interactions(&interactions_path)?;
        let mut recovered = false;
        let mut recovered_tasks = Vec::new();
        for task in tasks.values_mut() {
            if matches!(
                task.status,
                RuntimeTaskStatus::Queued
                    | RuntimeTaskStatus::Running
                    | RuntimeTaskStatus::Cancelling
                    | RuntimeTaskStatus::WaitingApproval
                    | RuntimeTaskStatus::WaitingAnswer
            ) {
                task.status = RuntimeTaskStatus::Interrupted;
                task.pid = None;
                task.completed_at = Some(now());
                task.error = Some("Runtime restarted while task was active".to_owned());
                recovered = true;
                recovered_tasks.push(task.clone());
            }
            // 会话可能已被用户删除；历史任务保留，但降级为无会话根 Agent，
            // 绝不能让悬空引用阻止 Runtime 启动。
            let agent = if let Some(session) = task
                .session_id
                .map(|session_id| sessions.get(session_id))
                .transpose()?
                .flatten()
            {
                agents.ensure_session_root(
                    session.root_agent_id,
                    task.id,
                    task.workspace.clone(),
                    task.profile.clone(),
                    task.model.clone(),
                    agent_status(task.status),
                )?
            } else {
                agents.ensure_root(
                    task.id,
                    task.workspace.clone(),
                    task.profile.clone(),
                    task.model.clone(),
                    agent_status(task.status),
                )?
            };
            if task.agent_id != Some(agent.id) {
                task.agent_id = Some(agent.id);
                recovered = true;
            }
        }
        if recovered {
            persist_tasks(&path, &tasks)?;
        }
        let mut interactions_recovered = false;
        let mut recovered_interactions = Vec::new();
        for interaction in interactions.values_mut() {
            if interaction.status == InteractionStatus::Pending {
                interaction.status = InteractionStatus::Cancelled;
                interaction.resolution = Some(match &interaction.kind {
                    InteractionKind::Approval { .. } => InteractionResolution::Deny,
                    InteractionKind::Question { .. } => InteractionResolution::Answer(None),
                });
                interaction.resolved_at = Some(now());
                interactions_recovered = true;
                recovered_interactions.push((interaction.id, interaction.task_id));
            }
        }
        if interactions_recovered {
            persist_interactions(&interactions_path, &interactions)?;
        }
        for task in &recovered_tasks {
            let error = task.error.as_deref().unwrap_or_default();
            events.append(
                "task.interrupted",
                format!(
                    "task_id={} session_id={} turn_id={} exit_code=none error={error}",
                    task.id,
                    task.session_id
                        .map_or_else(|| "none".to_owned(), |id| id.to_string()),
                    task.turn_id
                        .map_or_else(|| "none".to_owned(), |id| id.to_string())
                ),
            )?;
            if let (Some(session_id), Some(turn_id)) = (task.session_id, task.turn_id) {
                let requeued = sessions
                    .get_turn(turn_id)?
                    .is_some_and(|turn| turn.status == session_store::RuntimeTurnStatus::Queued);
                events.append(
                    if requeued {
                        "turn.requeued"
                    } else {
                        "turn.interrupted"
                    },
                    format!(
                        "session_id={session_id} turn_id={turn_id} task_id={} exit_code=none replay={} error={error}",
                        task.id, requeued
                    ),
                )?;
            }
        }
        for (interaction_id, task_id) in recovered_interactions {
            events.append(
                "task.interaction_cancelled",
                format!(
                    "task_id={task_id} interaction_id={interaction_id} reason=runtime_restarted"
                ),
            )?;
        }
        let workspaces = Arc::new(workspace_store::WorkspaceStore::open(
            home.join("runtime/workspaces.json"),
        )?);
        let tools = Arc::new(tool_store::ToolStore::open(
            home.join("runtime/tools.json"),
        )?);
        Ok(Self {
            path,
            home,
            events,
            agents,
            tools,
            sessions,
            workspaces,
            runtime_url,
            runtime_token,
            tasks: RwLock::new(tasks),
            persistence: AsyncMutex::new(()),
            cancellations: Mutex::new(HashMap::new()),
            approval_modes: approval_modes::LiveApprovalModes::default(),
            steering: steering::SteeringInboxes::default(),
            interactions_path,
            interactions: RwLock::new(interactions),
            interaction_waiters: Mutex::new(HashMap::new()),
            turn_scheduler,
            herdr: herdr::HerdrReporter::detect(),
        })
    }

    pub(super) async fn list(&self) -> Vec<RuntimeTask> {
        let mut tasks = self
            .tasks
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        tasks.sort_by_key(|task| std::cmp::Reverse(task.created_at));
        tasks
    }

    pub(super) fn schedule_session(&self, session_id: uuid::Uuid) -> Result<()> {
        self.turn_scheduler
            .send(session_id)
            .map_err(|_| anyhow::anyhow!("Runtime Turn scheduler stopped"))
    }

    pub(super) async fn get(&self, id: uuid::Uuid) -> Option<RuntimeTask> {
        self.tasks.read().await.get(&id).cloned()
    }

    pub(super) async fn pending_interactions(&self) -> Vec<RuntimeInteraction> {
        let mut interactions = self
            .interactions
            .read()
            .await
            .values()
            .filter(|item| item.status == InteractionStatus::Pending)
            .cloned()
            .collect::<Vec<_>>();
        interactions.sort_by_key(|item| item.created_at);
        interactions
    }

    pub(super) async fn create_interaction(
        &self,
        task_id: uuid::Uuid,
        kind: InteractionKind,
    ) -> Result<tokio::sync::oneshot::Receiver<InteractionResolution>> {
        let status = match &kind {
            InteractionKind::Approval { .. } => RuntimeTaskStatus::WaitingApproval,
            InteractionKind::Question { .. } => RuntimeTaskStatus::WaitingAnswer,
        };
        let interaction = RuntimeInteraction {
            id: uuid::Uuid::new_v4(),
            task_id,
            kind,
            status: InteractionStatus::Pending,
            resolution: None,
            created_at: now(),
            resolved_at: None,
        };
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let _persistence = self.persistence.lock().await;
        let task_snapshot = {
            let mut tasks = self.tasks.write().await;
            let task = tasks.get_mut(&task_id).context("Runtime task not found")?;
            if !matches!(task.status, RuntimeTaskStatus::Running) {
                bail!("Runtime task is not running");
            }
            task.status = status;
            tasks.clone()
        };
        let interaction_snapshot = {
            let mut interactions = self.interactions.write().await;
            interactions.insert(interaction.id, interaction.clone());
            interactions.clone()
        };
        persist_tasks(&self.path, &task_snapshot)?;
        persist_interactions(&self.interactions_path, &interaction_snapshot)?;
        self.agents
            .set_status_for_task(interaction.task_id, agent_status(status), None)?;
        self.sessions.set_task_waiting(
            interaction.task_id,
            match status {
                RuntimeTaskStatus::WaitingApproval => {
                    session_store::RuntimeTurnStatus::WaitingApproval
                }
                RuntimeTaskStatus::WaitingAnswer => session_store::RuntimeTurnStatus::WaitingAnswer,
                _ => session_store::RuntimeTurnStatus::Running,
            },
        )?;
        self.interaction_waiters
            .lock()
            .map_err(|_| anyhow::anyhow!("Runtime interaction waiter lock poisoned"))?
            .insert(interaction.id, sender);
        self.events.append(
            match &interaction.kind {
                InteractionKind::Approval { .. } => "task.waiting_approval",
                InteractionKind::Question { .. } => "task.waiting_answer",
            },
            format!(
                "task_id={} interaction_id={}",
                interaction.task_id, interaction.id
            ),
        )?;
        self.report_herdr_state().await;
        Ok(receiver)
    }

    pub(super) async fn resolve_interaction(
        &self,
        id: uuid::Uuid,
        resolution: InteractionResolution,
    ) -> Result<Option<RuntimeInteraction>> {
        let _persistence = self.persistence.lock().await;
        let (interaction, interaction_snapshot) = {
            let mut interactions = self.interactions.write().await;
            let Some(interaction) = interactions.get_mut(&id) else {
                return Ok(None);
            };
            if interaction.status != InteractionStatus::Pending {
                bail!("Runtime interaction is no longer pending");
            }
            validate_resolution(&interaction.kind, &resolution)?;
            interaction.status = InteractionStatus::Resolved;
            interaction.resolution = Some(resolution.clone());
            interaction.resolved_at = Some(now());
            (interaction.clone(), interactions.clone())
        };
        let task_snapshot = {
            let mut tasks = self.tasks.write().await;
            if let Some(task) = tasks.get_mut(&interaction.task_id)
                && matches!(
                    task.status,
                    RuntimeTaskStatus::WaitingApproval | RuntimeTaskStatus::WaitingAnswer
                )
            {
                task.status = RuntimeTaskStatus::Running;
            }
            tasks.clone()
        };
        persist_interactions(&self.interactions_path, &interaction_snapshot)?;
        persist_tasks(&self.path, &task_snapshot)?;
        self.agents
            .set_status_for_task(interaction.task_id, RuntimeAgentStatus::Running, None)?;
        self.sessions.set_task_waiting(
            interaction.task_id,
            session_store::RuntimeTurnStatus::Running,
        )?;
        let sender = self
            .interaction_waiters
            .lock()
            .map_err(|_| anyhow::anyhow!("Runtime interaction waiter lock poisoned"))?
            .remove(&id);
        if let Some(sender) = sender {
            let _ = sender.send(resolution);
        }
        self.events.append(
            "task.interaction_resolved",
            format!(
                "task_id={} interaction_id={}",
                interaction.task_id, interaction.id
            ),
        )?;
        self.report_herdr_state().await;
        Ok(Some(interaction))
    }

    pub(super) async fn submit(self: &Arc<Self>, mut request: SubmitTask) -> Result<RuntimeTask> {
        if request.prompt.trim().is_empty() && request.attachments.is_empty() {
            bail!("task prompt and attachments must not both be empty");
        }
        let workspace = self.workspaces.ensure_registered(&request.workspace)?;
        request.workspace = workspace.root;
        request.workspace_access = Some(workspace.access);
        request.workspace_skills = Some(workspace.skills);
        request.workspace_mcp_servers = Some(workspace.mcp_servers);
        if request.profile.is_none() {
            request.profile = workspace.provider_profile;
        }
        if let Some(config) = request.config.as_mut() {
            *config = config
                .canonicalize()
                .with_context(|| format!("invalid config: {}", config.display()))?;
        }

        let id = uuid::Uuid::new_v4();
        let mut task = RuntimeTask {
            id,
            session_id: request.session_id,
            turn_id: request.turn_id,
            agent_id: None,
            event_start_sequence: 0,
            status: RuntimeTaskStatus::Queued,
            workspace: request.workspace.clone(),
            profile: request.profile.clone(),
            model: request.model.clone(),
            origin_client: request.origin_client.clone(),
            prompt_excerpt: task_prompt_excerpt(&request.prompt),
            pid: None,
            created_at: now(),
            started_at: None,
            completed_at: None,
            exit_code: None,
            failure_domain: None,
            error: None,
        };
        let agent = if let Some(session_id) = request.session_id {
            let session = self
                .sessions
                .get(session_id)?
                .context("Runtime Session not found")?;
            self.agents.ensure_session_root(
                session.root_agent_id,
                id,
                request.workspace.clone(),
                request.profile.clone(),
                request.model.clone(),
                RuntimeAgentStatus::Queued,
            )?
        } else {
            self.agents.ensure_root(
                id,
                request.workspace.clone(),
                request.profile.clone(),
                request.model.clone(),
                RuntimeAgentStatus::Queued,
            )?
        };
        task.agent_id = Some(agent.id);
        self.events.append(
            "agent.created",
            format!("agent_id={} task_id={id} parent_id=none", agent.id),
        )?;
        self.insert_and_persist(task.clone()).await?;
        if let Some(turn_id) = task.turn_id
            && !self.sessions.bind_task(turn_id, id)?
        {
            task.status = RuntimeTaskStatus::Cancelled;
            task.completed_at = Some(now());
            self.insert_and_persist(task.clone()).await?;
            self.agents.set_status_for_task(
                id,
                RuntimeAgentStatus::Cancelled,
                Some("Turn was cancelled before task startup".to_owned()),
            )?;
            self.events.append(
                "task.cancelled",
                format!("task_id={id} session_id={} turn_id={turn_id} exit_code=none error=cancelled before startup", task.session_id.map_or_else(|| "none".to_owned(), |value| value.to_string())),
            )?;
            return Ok(task);
        }
        task.event_start_sequence = self
            .events
            .append("task.queued", format!("task_id={id}"))?
            .sequence;
        self.insert_and_persist(task.clone()).await?;

        task.status = RuntimeTaskStatus::Running;
        task.pid = None;
        task.started_at = Some(now());
        self.insert_and_persist(task.clone()).await?;
        self.agents
            .set_status_for_task(id, RuntimeAgentStatus::Running, None)?;
        self.events.append(
            "agent.running",
            format!("agent_id={} task_id={id}", agent.id),
        )?;
        self.events
            .append("task.started", format!("task_id={id} mode=in_process"))?;
        let cancellation = Arc::new(Notify::new());
        self.cancellations
            .lock()
            .map_err(|_| anyhow::anyhow!("Runtime task cancellation lock poisoned"))?
            .insert(id, cancellation.clone());

        let manager = self.clone();
        let home = self.home.clone();
        let connection = RuntimeConnection {
            url: self.runtime_url.clone(),
            token: self.runtime_token.clone(),
            task_id: id,
        };
        let sink: Arc<dyn willdeep_core::EventSink> = Arc::new(RuntimeEventSink {
            task_id: id,
            session_id: task.session_id,
            turn_id: task.turn_id,
            root_agent_id: agent.id,
            home: self.home.clone(),
            workspace: request.workspace.clone(),
            events: self.events.clone(),
            agents: self.agents.clone(),
            tools: self.tools.clone(),
            diff_baselines: AsyncMutex::new(HashMap::new()),
            child_workspaces: AsyncMutex::new(HashMap::new()),
        });
        // 会话里用户选过的档位压过工作区默认档（只读工作区除外），并登记句柄，
        // 让这一轮跑到一半时切档也能生效。
        let session_mode = match request.session_id {
            Some(session_id) => self
                .sessions
                .get(session_id)?
                .and_then(|session| session.approval_mode),
            None => None,
        };
        let workspace_access = request.workspace_access.unwrap_or_default();
        let effective_access = workspace_access.with_session_override(session_mode);
        request.workspace_access = Some(effective_access);
        request.approval_handle = Some(self.approval_modes.register(
            id,
            request.session_id,
            workspace_access,
            effective_access,
        ));
        // 用户在本轮进行中的插话由此送达：turn.steer 按会话找到这个收件箱。
        request.instruction_inbox = Some(self.steering.register(id, request.session_id));
        tokio::spawn(async move {
            let execution =
                crate::harness::execute_runtime(&home, request, connection, sink, |core| {
                    manager.sessions.prepare_execution(id, core)
                });
            let (result, cancelled) = tokio::select! {
                result = execution => (Some(result), false),
                _ = cancellation.notified() => {
                    (None, true)
                }
            };
            if let Ok(mut cancellations) = manager.cancellations.lock() {
                cancellations.remove(&id);
            }
            manager.approval_modes.remove(id);
            // 没赶上这一轮的插话交回客户端重新排队，先于收尾事件发出，界面收到
            // 收尾时队列已经排好。
            for text in manager.steering.remove(id) {
                let undelivered = serde_json::json!({"type":"steer_undelivered","text":text});
                let _ = manager
                    .events
                    .append("task.output", format!("task_id={id} {undelivered}"));
            }
            let result = result.map(|result| {
                result.and_then(|outcome| {
                    manager.sessions.record_execution_end(
                        id,
                        outcome.message_end,
                        outcome.message_generation,
                    )?;
                    Ok(outcome)
                })
            });
            let (final_status, error, failure_domain) = match result {
                _ if cancelled => (RuntimeTaskStatus::Cancelled, None, None),
                Some(Ok(outcome)) => {
                    let completed = serde_json::json!({
                        "type":if outcome.stop_reason.is_complete() { "completed" } else { "partial" },
                        "stop_reason":outcome.stop_reason.as_str(),
                        "turns":outcome.turns,
                        "text":outcome.final_text,
                        "session_id":outcome.session_id,
                    });
                    let _ = manager
                        .events
                        .append("task.output", format!("task_id={id} {completed}"));
                    (
                        if outcome.stop_reason.is_complete() {
                            RuntimeTaskStatus::Completed
                        } else {
                            RuntimeTaskStatus::Partial
                        },
                        None,
                        None,
                    )
                }
                Some(Err(error)) => (
                    RuntimeTaskStatus::Failed,
                    Some(format!("{error:#}")),
                    Some(crate::runtime_failure_domain(&error)),
                ),
                None => (RuntimeTaskStatus::Cancelled, None, None),
            };
            if let Err(error) = manager
                .finish(id, final_status, None, error, failure_domain)
                .await
            {
                eprintln!("persist Runtime task {id} completion: {error:#}");
            }
        });
        Ok(task)
    }

    pub(super) async fn cancel(&self, id: uuid::Uuid) -> Result<Option<RuntimeTask>> {
        let cancellation = self
            .cancellations
            .lock()
            .ok()
            .and_then(|items| items.get(&id).cloned());
        if let Some(cancellation) = cancellation {
            let _persistence = self.persistence.lock().await;
            let task = {
                let mut tasks = self.tasks.write().await;
                let Some(task) = tasks.get_mut(&id) else {
                    return Ok(None);
                };
                task.status = RuntimeTaskStatus::Cancelling;
                let task = task.clone();
                persist_tasks(&self.path, &tasks)?;
                task
            };
            cancellation.notify_one();
            self.events
                .append("task.cancellation_requested", format!("task_id={id}"))?;
            drop(_persistence);
            self.report_herdr_state().await;
            return Ok(Some(task));
        }
        Ok(self.get(id).await)
    }

    pub(super) async fn cancel_all(&self) {
        let cancellations = self
            .cancellations
            .lock()
            .map(|items| items.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for cancellation in cancellations {
            cancellation.notify_one();
        }
        for _ in 0..50 {
            if self
                .cancellations
                .lock()
                .is_ok_and(|items| items.is_empty())
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub(super) async fn wait_until_idle(&self) {
        loop {
            let has_active_tasks = self
                .tasks
                .read()
                .await
                .values()
                .any(|task| runtime_task_status_blocks_drain(task.status));
            if !has_active_tasks {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// 那些不会拦住 drain、但会随交接一起没掉的任务——全在等人回应。
    pub(super) async fn tasks_awaiting_a_human(&self) -> Vec<uuid::Uuid> {
        let mut waiting = self
            .tasks
            .read()
            .await
            .values()
            .filter(|task| {
                runtime_task_status_is_active(task.status)
                    && !runtime_task_status_blocks_drain(task.status)
            })
            .map(|task| task.id)
            .collect::<Vec<_>>();
        waiting.sort();
        waiting
    }

    pub(super) async fn insert_and_persist(&self, task: RuntimeTask) -> Result<()> {
        let _persistence = self.persistence.lock().await;
        let snapshot = {
            let mut tasks = self.tasks.write().await;
            tasks.insert(task.id, task);
            tasks.clone()
        };
        persist_tasks(&self.path, &snapshot)?;
        drop(_persistence);
        self.report_herdr_state().await;
        Ok(())
    }

    pub(super) async fn finish(
        self: &Arc<Self>,
        id: uuid::Uuid,
        status: RuntimeTaskStatus,
        exit_code: Option<i32>,
        error: Option<String>,
        failure_domain: Option<willdeep_runtime_protocol::FailureDomain>,
    ) -> Result<()> {
        let _persistence = self.persistence.lock().await;
        let (finished_task, snapshot) = {
            let mut tasks = self.tasks.write().await;
            let task = tasks.get_mut(&id).context("Runtime task disappeared")?;
            task.status = status;
            task.completed_at = Some(now());
            task.exit_code = exit_code;
            task.failure_domain = failure_domain;
            task.error = error.clone();
            (task.clone(), tasks.clone())
        };
        persist_tasks(&self.path, &snapshot)?;
        self.agents
            .set_status_for_task(id, agent_status(status), error.clone())?;
        self.events.append(
            match status {
                RuntimeTaskStatus::Completed => "task.completed",
                RuntimeTaskStatus::Partial => "task.partial",
                RuntimeTaskStatus::Cancelled => "task.cancelled",
                RuntimeTaskStatus::Interrupted => "task.interrupted",
                _ => "task.failed",
            },
            format!(
                "task_id={id} session_id={} turn_id={} exit_code={} failure_domain={} error={}",
                finished_task
                    .session_id
                    .map_or_else(|| "none".to_owned(), |id| id.to_string()),
                finished_task
                    .turn_id
                    .map_or_else(|| "none".to_owned(), |id| id.to_string()),
                exit_code.map_or_else(|| "none".to_owned(), |code| code.to_string()),
                failure_domain.map_or("none", |domain| match domain {
                    willdeep_runtime_protocol::FailureDomain::Provider => "provider",
                    willdeep_runtime_protocol::FailureDomain::Policy => "policy",
                    willdeep_runtime_protocol::FailureDomain::Tool => "tool",
                    willdeep_runtime_protocol::FailureDomain::Harness => "harness",
                    willdeep_runtime_protocol::FailureDomain::Internal => "internal",
                    willdeep_runtime_protocol::FailureDomain::Unknown => "unknown",
                }),
                error.clone().unwrap_or_default()
            ),
        )?;
        let runtime_session = self.sessions.complete_task(id, status, error.clone())?;
        if let (Some(session_id), Some(turn_id)) = (finished_task.session_id, finished_task.turn_id)
        {
            self.events.append(
                match status {
                    RuntimeTaskStatus::Completed => "turn.completed",
                    RuntimeTaskStatus::Partial => "turn.partial",
                    RuntimeTaskStatus::Cancelled => "turn.cancelled",
                    RuntimeTaskStatus::Interrupted => "turn.interrupted",
                    _ => "turn.failed",
                },
                format!(
                    "session_id={session_id} turn_id={turn_id} task_id={id} exit_code={} error={}",
                    exit_code.map_or_else(|| "none".to_owned(), |code| code.to_string()),
                    error.clone().unwrap_or_default()
                ),
            )?;
        }
        drop(_persistence);
        self.cancel_task_interactions(id).await?;
        if let Some(session_id) = runtime_session {
            self.schedule_session(session_id)?;
        }
        self.report_herdr_state().await;
        Ok(())
    }

    pub(super) async fn report_herdr_state(&self) {
        let Some(reporter) = &self.herdr else {
            return;
        };
        let statuses = self
            .tasks
            .read()
            .await
            .values()
            .map(|task| task.status)
            .collect::<Vec<_>>();
        reporter.report(statuses.into_iter());
    }

    pub(super) async fn cancel_task_interactions(&self, task_id: uuid::Uuid) -> Result<()> {
        let pending = self
            .interactions
            .read()
            .await
            .values()
            .filter(|item| item.task_id == task_id && item.status == InteractionStatus::Pending)
            .map(|item| {
                let resolution = match &item.kind {
                    InteractionKind::Approval { .. } => InteractionResolution::Deny,
                    InteractionKind::Question { .. } => InteractionResolution::Answer(None),
                };
                (item.id, resolution)
            })
            .collect::<Vec<_>>();
        for (id, resolution) in pending {
            let _ = self.resolve_interaction(id, resolution).await;
        }
        Ok(())
    }
}
