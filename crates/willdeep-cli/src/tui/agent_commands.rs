use super::*;

#[derive(Debug, PartialEq, Eq)]
enum AgentCommand<'a> {
    Instruct {
        id: uuid::Uuid,
        message: &'a str,
    },
    Stop {
        id: uuid::Uuid,
    },
    Retry {
        id: uuid::Uuid,
        model: Option<&'a str>,
    },
    Spawn {
        profile: &'a str,
        prompt: &'a str,
    },
}

fn split_head(value: &str) -> Option<(&str, &str)> {
    let value = value.trim();
    let index = value.find(char::is_whitespace).unwrap_or(value.len());
    let head = &value[..index];
    (!head.is_empty()).then_some((head, value[index..].trim()))
}

fn parse_agent_command(arguments: &str) -> Result<AgentCommand<'_>> {
    let (action, arguments) = split_head(arguments).context("missing Agent action")?;
    if action == "spawn" {
        let (profile, prompt) = split_head(arguments).context("missing Agent profile")?;
        if !matches!(profile, "generalist" | "reviewer" | "reader" | "judge") {
            bail!("unsupported external Agent profile");
        }
        if prompt.is_empty() {
            bail!("missing Agent task");
        }
        return Ok(AgentCommand::Spawn { profile, prompt });
    }
    let (id, trailing) = split_head(arguments).context("missing Runtime Agent ID")?;
    let id = uuid::Uuid::parse_str(id).context("invalid Runtime Agent ID")?;
    match action {
        "instruct" => {
            let message = trailing;
            if message.is_empty() {
                bail!("missing Agent instruction");
            }
            Ok(AgentCommand::Instruct { id, message })
        }
        "stop" if trailing.is_empty() => Ok(AgentCommand::Stop { id }),
        "retry" => {
            let mut parts = trailing.split_whitespace();
            let options = (parts.next(), parts.next(), parts.next());
            match options {
                (None, None, None) => Ok(AgentCommand::Retry { id, model: None }),
                (Some("--model"), Some(model), None) if !model.trim().is_empty() => {
                    Ok(AgentCommand::Retry {
                        id,
                        model: Some(model),
                    })
                }
                _ => bail!("invalid Agent retry options"),
            }
        }
        _ => bail!("unsupported Agent action"),
    }
}

pub(super) async fn handle_agent_command(
    prompt: &str,
    app: &mut App,
    runtime: &TuiRuntime,
    session_id: uuid::Uuid,
) -> Result<bool> {
    let value = prompt.trim();
    if value != "/agent" && !value.starts_with("/agent ") {
        return Ok(false);
    }
    let arguments = value.strip_prefix("/agent").unwrap_or_default().trim();
    let usage = app.language.text(
        "用法：/agent spawn <reader|judge> <任务> | instruct <ID> <指令> | stop <ID> | retry <ID> [--model <模型>]",
        "Usage: /agent spawn <reader|judge> <task> | instruct <id> <message> | stop <id> | retry <id> [--model <model>]",
        "使用法：/agent spawn <reader|judge> <タスク> | instruct <ID> <指示> | stop <ID> | retry <ID> [--model <モデル>]",
    );
    let command = match parse_agent_command(arguments) {
        Ok(command) => command,
        Err(_) => {
            app.append_transcript(format!("System: {usage}"));
            return Ok(true);
        }
    };
    let result = match command {
        AgentCommand::Instruct { id, message } => {
            crate::daemon::instruct_remote_agent(&runtime.home, id, message.to_owned()).await?;
            app.language.text(
                "补充指令已排队，将在下一次模型请求前送达",
                "Additional instruction queued for the next model request",
                "追加指示を次のモデル要求に向けてキューしました",
            )
        }
        AgentCommand::Stop { id } => {
            crate::daemon::stop_remote_agent(&runtime.home, id).await?;
            app.language.text(
                "已请求停止子 Agent",
                "Child Agent stop requested",
                "子 Agent の停止を要求しました",
            )
        }
        AgentCommand::Retry { id, model } => {
            crate::daemon::retry_remote_agent_with_model(
                &runtime.home,
                id,
                model.map(str::to_owned),
            )
            .await?;
            if model.is_some() {
                app.language.text(
                    "已请求使用指定模型重试子 Agent",
                    "Child Agent retry with the selected model requested",
                    "指定モデルで子 Agent の再試行を要求しました",
                )
            } else {
                app.language.text(
                    "已请求重试子 Agent",
                    "Child Agent retry requested",
                    "子 Agent の再試行を要求しました",
                )
            }
        }
        AgentCommand::Spawn { profile, prompt } => {
            crate::daemon::spawn_remote_agent(
                &runtime.home,
                session_id,
                prompt.to_owned(),
                profile.to_owned(),
                None,
            )
            .await?;
            app.language.text(
                "只读后台子 Agent 已排队",
                "Read-only background child Agent queued",
                "読み取り専用のバックグラウンド子 Agent をキューしました",
            )
        }
    };
    app.append_transcript(format!("System: {result}"));
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_agent_lifecycle_commands_without_ambiguous_trailing_input() {
        let id = uuid::Uuid::new_v4();
        assert_eq!(
            parse_agent_command(&format!("retry {id} --model qwen3-max")).unwrap(),
            AgentCommand::Retry {
                id,
                model: Some("qwen3-max")
            }
        );
        assert_eq!(
            parse_agent_command(&format!("stop {id}")).unwrap(),
            AgentCommand::Stop { id }
        );
        assert_eq!(
            parse_agent_command(&format!("instruct {id} inspect the failing test")).unwrap(),
            AgentCommand::Instruct {
                id,
                message: "inspect the failing test"
            }
        );
        assert!(parse_agent_command(&format!("retry {id} --model")).is_err());
        assert!(parse_agent_command(&format!("stop {id} extra")).is_err());
        assert_eq!(
            parse_agent_command("spawn reader inspect the repository").unwrap(),
            AgentCommand::Spawn {
                profile: "reader",
                prompt: "inspect the repository"
            }
        );
        assert!(parse_agent_command("spawn editor change files").is_err());
        assert!(parse_agent_command("spawn scout inspect the repository").is_err());
        assert!(parse_agent_command("spawn deep inspect everything").is_err());
    }
}
