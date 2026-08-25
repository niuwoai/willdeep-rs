use super::*;

/// 打开历史会话面板所需的初始状态：关键词进输入框，其余过滤器随每次刷新一起发。
#[derive(Default)]
pub(super) struct SessionPickerRequest {
    pub(super) query: String,
    pub(super) filters: Vec<(String, String)>,
}

/// `/history` 与 `/session search` 都落到同一个面板：前者列当前工作区最近的会话，
/// 后者把关键词和过滤器带进面板。返回 `Ok(None)` 表示这条提示词不归它管。
pub(super) fn parse_session_picker_command(prompt: &str) -> Result<Option<SessionPickerRequest>> {
    // 按词切，不按字节前缀切：`/session  search` 和 `/session search` 是同一条命令。
    let mut tokens = prompt.split_whitespace();
    let arguments = match tokens.next() {
        Some("/history") => tokens.collect::<Vec<_>>(),
        Some("/session") => match tokens.next() {
            Some("search") => tokens.collect::<Vec<_>>(),
            _ => return Ok(None),
        },
        _ => return Ok(None),
    };
    if arguments.is_empty() {
        return Ok(Some(SessionPickerRequest::default()));
    }
    let mut filters = parse_search_options(&arguments.join(" "))?;
    // 关键词单独抽出来放进面板输入框，用户可以直接接着改。
    let query = filters
        .iter()
        .position(|(key, _)| key == "q")
        .map(|position| filters.remove(position).1)
        .unwrap_or_default();
    Ok(Some(SessionPickerRequest { query, filters }))
}

pub(super) async fn handle_session_command(
    prompt: &str,
    app: &mut App,
    session: &mut Session,
    store: &SessionStore,
    runtime: &mut TuiRuntime,
) -> Result<bool> {
    let value = prompt.trim();
    if value != "/session" && !value.starts_with("/session ") {
        return Ok(false);
    }
    let arguments = value.strip_prefix("/session").unwrap_or_default().trim();
    let (action, rest) = arguments.split_once(' ').unwrap_or((arguments, ""));
    let usage = app.language.text(
        "用法：/session switch <会话ID> | rename <名称> | retitle（让标题模型重算一次） | fork [--through 轮次ID] [--profile Provider] [--model 模型] [名称] | fork-turn <轮次ID> [名称] | archive | unarchive | search [关键词]（打开历史会话面板，等同 /history） | export <路径> | delete <其他会话ID>",
        "Usage: /session switch <session-id> | rename <title> | retitle (recompute with the title model) | fork [--through turn-id] [--profile provider] [--model model] [title] | fork-turn <turn-id> [title] | archive | unarchive | search [query] (opens the Session history panel, same as /history) | export <path> | delete <other-session-id>",
        "使用法：/session switch <セッションID> | rename <名前> | retitle（タイトルモデルで再計算） | fork [--through ターンID] [--profile Provider] [--model モデル] [名前] | fork-turn <ターンID> [名前] | archive | unarchive | search [検索語]（履歴セッションパネルを開く。/history と同じ） | export <パス> | delete <別セッションID>",
    );
    let result = match action {
        "switch" if !rest.trim().is_empty() => switch(app, session, store, runtime, rest).await?,
        "rename" if !rest.trim().is_empty() => rename(app, session, store, runtime, rest).await?,
        "fork" => fork(app, session, runtime, rest).await?,
        "fork-turn" if !rest.trim().is_empty() => fork_turn(app, session, runtime, rest).await?,
        "archive" if rest.trim().is_empty() => archive(app, session, runtime, true).await?,
        "unarchive" if rest.trim().is_empty() => archive(app, session, runtime, false).await?,
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
    let options = parse_fork_options(title)?;
    let id = crate::daemon::fork_remote_session(
        &runtime.home,
        session.id,
        options.title,
        options.through_turn_id,
        options.provider_profile,
        options.model,
    )
    .await?;
    Ok(format!(
        "{}: {id}",
        app.language.text(
            "已创建分叉会话",
            "Forked Session created",
            "フォークセッションを作成しました"
        )
    ))
}

async fn fork_turn(
    app: &App,
    session: &Session,
    runtime: &TuiRuntime,
    arguments: &str,
) -> Result<String> {
    let (turn_id, title) = arguments
        .trim()
        .split_once(' ')
        .unwrap_or((arguments.trim(), ""));
    let turn_id = uuid::Uuid::parse_str(turn_id).context("invalid Runtime Turn ID")?;
    let title = (!title.trim().is_empty()).then(|| title.trim().to_owned());
    let id = crate::daemon::fork_remote_session(
        &runtime.home,
        session.id,
        title,
        Some(turn_id),
        None,
        None,
    )
    .await?;
    Ok(format!(
        "{}: {id}",
        app.language.text(
            "已从指定轮次创建分叉会话",
            "Forked Session created through the selected Turn",
            "指定ターンまでの分岐セッションを作成しました"
        )
    ))
}

#[derive(Default)]
pub(super) struct ForkOptions {
    pub(super) title: Option<String>,
    pub(super) through_turn_id: Option<uuid::Uuid>,
    pub(super) provider_profile: Option<String>,
    pub(super) model: Option<String>,
}

pub(super) fn parse_fork_options(arguments: &str) -> Result<ForkOptions> {
    let mut options = ForkOptions::default();
    let mut title = Vec::new();
    let mut values = arguments.split_whitespace();
    while let Some(value) = values.next() {
        match value {
            "--through" => {
                let value = values
                    .next()
                    .context("--through requires a Runtime Turn ID")?;
                options.through_turn_id =
                    Some(uuid::Uuid::parse_str(value).context("invalid Runtime Turn ID")?);
            }
            "--profile" => {
                options.provider_profile = Some(
                    values
                        .next()
                        .context("--profile requires a Provider profile")?
                        .to_owned(),
                );
            }
            "--model" => {
                options.model = Some(
                    values
                        .next()
                        .context("--model requires a model")?
                        .to_owned(),
                );
            }
            value if value.starts_with("--") => bail!("unknown Fork option: {value}"),
            value => title.push(value),
        }
    }
    options.title = (!title.is_empty()).then(|| title.join(" "));
    Ok(options)
}

pub(super) async fn switch(
    app: &mut App,
    session: &mut Session,
    store: &SessionStore,
    runtime: &mut TuiRuntime,
    id: &str,
) -> Result<String> {
    if app.running {
        bail!("cannot switch Session while a local turn is running");
    }
    let id = uuid::Uuid::parse_str(id.trim()).context("invalid Session ID")?;
    if id == session.id {
        return Ok(app
            .language
            .text(
                "当前已是该会话",
                "Session is already open",
                "このセッションは既に開いています",
            )
            .to_owned());
    }
    session.attention_read = app.attention_read.clone();
    session.runtime_event_cursor = app.runtime_event_cursor;
    store.save(session)?;
    let mut target = store.load(id).context("load target Session")?;
    if target.workspace.canonicalize()? != session.workspace.canonicalize()? {
        bail!("TUI in-place switching currently requires the same Workspace");
    }
    target.runtime_event_cursor = app.runtime_event_cursor;
    target.runtime_managed = true;
    store.save(&mut target)?;
    runtime.runtime_submit.profile = target.profile.clone();
    runtime.runtime_submit.model = target.model.clone();
    runtime.runtime_submit.config = target.config.clone();
    let _ = runtime.refresh_provider_config();
    app.load_session(&target);
    runtime.relay_bridge.set_session(target.id.to_string());
    *session = target;
    Ok(format!(
        "{}: {} · {}",
        app.language.text(
            "已切换会话",
            "Session switched",
            "セッションを切り替えました"
        ),
        session.id,
        session.title
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

pub(super) fn parse_search_options(arguments: &str) -> Result<Vec<(String, String)>> {
    let mut parameters = Vec::new();
    let mut query = Vec::new();
    let mut values = arguments.split_whitespace();
    while let Some(value) = values.next() {
        let key = match value {
            "--workspace" => Some("workspace"),
            "--status" => Some("status"),
            "--profile" => Some("profile"),
            "--model" => Some("model"),
            "--after" => Some("updated_after"),
            "--before" => Some("updated_before"),
            value if value.starts_with("--") => bail!("unknown Search option: {value}"),
            _ => None,
        };
        if let Some(key) = key {
            parameters.push((
                key.to_owned(),
                values
                    .next()
                    .with_context(|| format!("{value} requires a value"))?
                    .to_owned(),
            ));
        } else {
            query.push(value);
        }
    }
    if !query.is_empty() {
        parameters.push(("q".to_owned(), query.join(" ")));
    }
    if parameters.is_empty() {
        bail!("Session search requires text or at least one filter");
    }
    Ok(parameters)
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
