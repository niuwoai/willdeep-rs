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
        // 光秃秃的 `/workspace` 开面板。以前它打印一串 UUID，切换要人把 ID
        // 从聊天记录里抄回输入框——列表不是选择，选中才是。
        "" => return open_picker(app, session, runtime).await.map(|()| true),
        "list" if rest.trim().is_empty() => list(app, runtime).await?,
        "switch" if !rest.trim().is_empty() => {
            switch(app, session, store, runtime, rest.trim()).await?
        }
        _ => app
            .language
            .text(
                "用法：/workspace（打开面板） | list | switch <工作区ID|名称|路径>",
                "Usage: /workspace (opens the panel) | list | switch <workspace-id|name|path>",
                "使用法：/workspace（パネルを開く） | list | switch <ワークスペースID|名前|パス>",
            )
            .to_owned(),
    };
    app.append_transcript(format!("System: {result}"));
    Ok(true)
}

/// 拉一次工作区清单并把面板支起来。拉取失败要说清楚，别开一个空面板让人
/// 以为一个工作区都没注册。
pub(super) async fn open_picker(
    app: &mut App,
    session: &Session,
    runtime: &TuiRuntime,
) -> Result<()> {
    let workspaces = crate::daemon::remote_workspaces(&runtime.home).await?;
    if workspaces.is_empty() {
        app.append_transcript(format!(
            "System: {}",
            app.language.text(
                "尚未注册工作区",
                "No Workspaces are registered",
                "ワークスペースはまだ登録されていません",
            )
        ));
        return Ok(());
    }
    let current_root = session
        .workspace
        .canonicalize()
        .unwrap_or_else(|_| session.workspace.clone());
    app.open_workspace_picker(workspaces, current_root);
    Ok(())
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

pub(super) async fn switch(
    app: &mut App,
    session: &mut Session,
    store: &SessionStore,
    runtime: &mut TuiRuntime,
    id: &str,
) -> Result<String> {
    if app.running {
        bail!("cannot switch Workspace while a turn is running");
    }
    // ID、名称、路径都认。手敲的时候人记得住的是名字，ID 是面板和日志用的。
    let workspaces = crate::daemon::remote_workspaces(&runtime.home).await?;
    let parsed = uuid::Uuid::parse_str(id).ok();
    let workspace = workspaces
        .iter()
        .find(|workspace| match parsed {
            Some(parsed) => workspace.id == parsed,
            None => {
                workspace.name == id
                    || workspace.root == std::path::Path::new(id)
                    || workspace.root.to_string_lossy() == id
            }
        })
        .cloned()
        .context("Runtime Workspace not found")?;
    let id = workspace.id;
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
