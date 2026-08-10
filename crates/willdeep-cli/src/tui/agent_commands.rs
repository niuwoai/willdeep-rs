use super::*;

pub(super) async fn handle_agent_command(
    prompt: &str,
    app: &mut App,
    runtime: &TuiRuntime,
) -> Result<bool> {
    let value = prompt.trim();
    if value != "/agent" && !value.starts_with("/agent ") {
        return Ok(false);
    }
    let arguments = value.strip_prefix("/agent").unwrap_or_default().trim();
    let mut parts = arguments.splitn(3, ' ');
    let action = parts.next().unwrap_or_default();
    let id = parts.next().unwrap_or_default();
    let message = parts.next().unwrap_or_default().trim();
    let usage = app.language.text(
        "用法：/agent instruct <Agent ID> <补充指令>",
        "Usage: /agent instruct <agent-id> <additional instruction>",
        "使用法：/agent instruct <Agent ID> <追加指示>",
    );
    let result = if action == "instruct" && !id.is_empty() && !message.is_empty() {
        let id = uuid::Uuid::parse_str(id).context("invalid Runtime Agent ID")?;
        crate::daemon::instruct_remote_agent(&runtime.home, id, message.to_owned()).await?;
        app.language.text(
            "补充指令已排队，将在下一次模型请求前送达",
            "Additional instruction queued for the next model request",
            "追加指示を次のモデル要求に向けてキューしました",
        )
    } else {
        usage
    };
    app.append_transcript(format!("System: {result}"));
    Ok(true)
}
