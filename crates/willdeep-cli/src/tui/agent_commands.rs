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
}

fn parse_agent_command(arguments: &str) -> Result<AgentCommand<'_>> {
    let mut parts = arguments.split_whitespace();
    let action = parts.next().context("missing Agent action")?;
    let id = uuid::Uuid::parse_str(parts.next().context("missing Runtime Agent ID")?)
        .context("invalid Runtime Agent ID")?;
    match action {
        "instruct" => {
            let id_end = arguments
                .find(&id.to_string())
                .context("missing Runtime Agent ID")?
                + id.to_string().len();
            let message = arguments[id_end..].trim();
            if message.is_empty() {
                bail!("missing Agent instruction");
            }
            Ok(AgentCommand::Instruct { id, message })
        }
        "stop" if parts.next().is_none() => Ok(AgentCommand::Stop { id }),
        "retry" => match (parts.next(), parts.next(), parts.next()) {
            (None, None, None) => Ok(AgentCommand::Retry { id, model: None }),
            (Some("--model"), Some(model), None) if !model.trim().is_empty() => {
                Ok(AgentCommand::Retry {
                    id,
                    model: Some(model),
                })
            }
            _ => bail!("invalid Agent retry options"),
        },
        _ => bail!("unsupported Agent action"),
    }
}

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
    let usage = app.language.text(
        "用法：/agent instruct <ID> <指令> | stop <ID> | retry <ID> [--model <模型>]",
        "Usage: /agent instruct <id> <message> | stop <id> | retry <id> [--model <model>]",
        "使用法：/agent instruct <ID> <指示> | stop <ID> | retry <ID> [--model <モデル>]",
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
    }
}
