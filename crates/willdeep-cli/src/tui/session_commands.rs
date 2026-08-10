use super::*;

pub(super) async fn handle_session_command(
    prompt: &str,
    app: &mut App,
    session: &mut Session,
    store: &SessionStore,
    runtime: &TuiRuntime,
) -> Result<bool> {
    let value = prompt.trim();
    if value != "/session" && !value.starts_with("/session ") {
        return Ok(false);
    }
    let arguments = value.strip_prefix("/session").unwrap_or_default().trim();
    let (action, rest) = arguments.split_once(' ').unwrap_or((arguments, ""));
    let usage = app.language.text(
        "用法：/session rename <名称> | fork [名称] | archive | unarchive | search <关键词> | export <路径> | delete <其他会话ID>",
        "Usage: /session rename <title> | fork [title] | archive | unarchive | search <query> | export <path> | delete <other-session-id>",
        "使用法：/session rename <名前> | fork [名前] | archive | unarchive | search <検索語> | export <パス> | delete <別セッションID>",
    );
    let result = match action {
        "rename" if !rest.trim().is_empty() => rename(app, session, store, runtime, rest).await?,
        "fork" => fork(app, session, runtime, rest).await?,
        "archive" if rest.trim().is_empty() => archive(app, session, runtime, true).await?,
        "unarchive" if rest.trim().is_empty() => archive(app, session, runtime, false).await?,
        "search" if !rest.trim().is_empty() => search(app, runtime, rest.trim()).await?,
        "export" if !rest.trim().is_empty() => export(app, session, runtime, rest.trim()).await?,
        "delete" if !rest.trim().is_empty() => delete(app, session, runtime, rest.trim()).await?,
        _ => usage.to_owned(),
    };
    app.append_transcript(format!("System: {result}"));
    Ok(true)
}

async fn rename(
    app: &App,
    session: &mut Session,
    store: &SessionStore,
    runtime: &TuiRuntime,
    title: &str,
) -> Result<String> {
    crate::daemon::rename_remote_session(&runtime.home, session.id, title.trim().to_owned())
        .await?;
    *session = store.load(session.id)?;
    Ok(format!(
        "{}: {}",
        app.language.text(
            "会话已重命名",
            "Session renamed",
            "セッション名を変更しました"
        ),
        session.title
    ))
}

async fn fork(app: &App, session: &Session, runtime: &TuiRuntime, title: &str) -> Result<String> {
    let title = (!title.trim().is_empty()).then(|| title.trim().to_owned());
    let id = crate::daemon::fork_remote_session(&runtime.home, session.id, title).await?;
    Ok(format!(
        "{}: {id}",
        app.language.text(
            "已创建分叉会话",
            "Forked Session created",
            "フォークセッションを作成しました"
        )
    ))
}

async fn archive(
    app: &App,
    session: &Session,
    runtime: &TuiRuntime,
    archived: bool,
) -> Result<String> {
    crate::daemon::set_remote_session_archived(&runtime.home, session.id, archived).await?;
    Ok(if archived {
        app.language.text(
            "当前会话已归档",
            "Current Session archived",
            "現在のセッションをアーカイブしました",
        )
    } else {
        app.language.text(
            "当前会话已取消归档",
            "Current Session unarchived",
            "現在のセッションのアーカイブを解除しました",
        )
    }
    .to_owned())
}

async fn search(app: &App, runtime: &TuiRuntime, query: &str) -> Result<String> {
    let value = crate::daemon::search_remote_sessions(&runtime.home, query).await?;
    let rows = value
        .as_array()
        .into_iter()
        .flatten()
        .take(20)
        .map(|item| {
            format!(
                "{} · {} · {}",
                item.get("id")
                    .and_then(|value| value.as_str())
                    .unwrap_or("?"),
                item.get("title")
                    .and_then(|value| value.as_str())
                    .unwrap_or("?"),
                item.get("snippet")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
            )
        })
        .collect::<Vec<_>>();
    Ok(if rows.is_empty() {
        app.language
            .text(
                "没有匹配的会话",
                "No matching Sessions",
                "一致するセッションはありません",
            )
            .to_owned()
    } else {
        rows.join("\n")
    })
}

async fn export(app: &App, session: &Session, runtime: &TuiRuntime, path: &str) -> Result<String> {
    let path = PathBuf::from(path);
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let export = crate::daemon::export_remote_session(&runtime.home, session.id).await?;
    std::fs::write(&path, serde_json::to_vec_pretty(&export)?)?;
    Ok(format!(
        "{}: {}",
        app.language.text(
            "会话已导出",
            "Session exported",
            "セッションをエクスポートしました"
        ),
        path.display()
    ))
}

async fn delete(app: &App, session: &Session, runtime: &TuiRuntime, id: &str) -> Result<String> {
    let id = uuid::Uuid::parse_str(id).context("invalid Session ID")?;
    if id == session.id {
        bail!("cannot delete the Session currently open in TUI");
    }
    crate::daemon::delete_remote_session(&runtime.home, id).await?;
    Ok(format!(
        "{}: {id}",
        app.language
            .text("会话已删除", "Session deleted", "セッションを削除しました")
    ))
}
