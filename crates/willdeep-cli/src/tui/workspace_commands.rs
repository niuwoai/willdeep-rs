use super::*;

pub(super) async fn handle_workspace_command(
    prompt: &str,
    app: &mut App,
    session: &mut Session,
    store: &SessionStore,
    runtime: &mut TuiRuntime,
) -> Result<bool> {
    let value = prompt.trim();
    if value != "/workspace" && !value.starts_with("/workspace ") {
        return Ok(false);
    }
    let arguments = value.strip_prefix("/workspace").unwrap_or_default().trim();
    let (action, rest) = arguments.split_once(' ').unwrap_or((arguments, ""));
    let result = match action {
        "" | "list" if rest.trim().is_empty() => list(app, runtime).await?,
        "switch" if !rest.trim().is_empty() => {
            switch(app, session, store, runtime, rest.trim()).await?
        }
        _ => app
            .language
            .text(
                "用法：/workspace list | switch <工作区ID>",
                "Usage: /workspace list | switch <workspace-id>",
                "使用法：/workspace list | switch <ワークスペースID>",
            )
            .to_owned(),
    };
    app.append_transcript(format!("System: {result}"));
    Ok(true)
}

async fn list(app: &App, runtime: &TuiRuntime) -> Result<String> {
    let workspaces = crate::daemon::remote_workspaces(&runtime.home).await?;
    if workspaces.is_empty() {
        return Ok(app
            .language
            .text(
                "尚未注册工作区",
                "No Workspaces are registered",
                "ワークスペースはまだ登録されていません",
            )
            .to_owned());
    }
    Ok(workspaces
        .into_iter()
        .map(|workspace| {
            format!(
                "{} {} · {} · {:?} · {}",
                if workspace.active { "*" } else { "-" },
                workspace.id,
                workspace.name,
                workspace.access,
                workspace.root.display()
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

async fn switch(
    app: &mut App,
    session: &mut Session,
    store: &SessionStore,
    runtime: &mut TuiRuntime,
    id: &str,
) -> Result<String> {
    if app.running {
        bail!("cannot switch Workspace while a turn is running");
    }
    let id = uuid::Uuid::parse_str(id).context("invalid Workspace ID")?;
    let workspace = crate::daemon::remote_workspaces(&runtime.home)
        .await?
        .into_iter()
        .find(|workspace| workspace.id == id)
        .context("Runtime Workspace not found")?;
    if workspace.root == session.workspace.canonicalize()? {
        crate::daemon::activate_remote_workspace(&runtime.home, id).await?;
        return Ok(app
            .language
            .text(
                "当前已在该工作区",
                "Workspace is already open",
                "このワークスペースは既に開いています",
            )
            .to_owned());
    }

    session.attention_read = app.attention_read.clone();
    session.runtime_event_cursor = app.runtime_event_cursor;
    store.save(session)?;
    let target = store
        .digests()
        .into_iter()
        .filter(|candidate| {
            candidate
                .workspace
                .canonicalize()
                .is_ok_and(|root| root == workspace.root)
        })
        .max_by_key(|candidate| candidate.updated_at)
        .and_then(|candidate| store.load(candidate.id).ok());
    let mut target = target.unwrap_or_else(|| {
        Session::new(
            workspace.root.clone(),
            workspace.provider_profile.clone(),
            &workspace.name,
        )
    });
    if target.config.is_none() {
        target.config = runtime.runtime_submit.config.clone();
    }
    if target.model.is_none() {
        target.model = runtime.runtime_submit.model.clone();
    }
    target.runtime_managed = true;
    if target.runtime_event_cursor == 0 {
        target.runtime_event_cursor = crate::daemon::runtime_event_head(&runtime.home)
            .await
            .unwrap_or_default();
    }
    store.save(&mut target)?;
    crate::daemon::ensure_runtime_session(
        &runtime.home,
        target.id,
        &workspace.root,
        workspace.provider_profile.clone(),
        target.model.clone(),
    )
    .await?;
    crate::daemon::activate_remote_workspace(&runtime.home, id).await?;

    runtime.runtime_submit.workspace = workspace.root.clone();
    runtime.runtime_submit.profile = workspace.provider_profile.clone();
    runtime.runtime_submit.model = target.model.clone();
    runtime.runtime_submit.config = target.config.clone();
    let _ = runtime.refresh_provider_config();
    runtime.skills =
        Arc::new(SkillCatalog::discover(&workspace.root, &[]).allow_only(&workspace.skills));
    app.load_session(&target);
    app.runtime_event_cursor = target.runtime_event_cursor;
    app.runtime_attention.clear();
    app.runtime_gates.clear();
    app.runtime_agents.clear();
    app.runtime_tools.clear();
    app.runtime_artifacts.clear();
    runtime.relay_bridge.set_session(target.id.to_string());
    *session = target;
    Ok(format!(
        "{}: {} · {}",
        app.language.text(
            "已切换工作区",
            "Workspace switched",
            "ワークスペースを切り替えました",
        ),
        workspace.name,
        workspace.root.display()
    ))
}
