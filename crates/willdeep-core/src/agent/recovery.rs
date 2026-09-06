use super::*;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResumeArgs {
    agent_id: uuid::Uuid,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_arguments_cannot_replace_the_original_task() {
        let call = ToolCall { id: "resume".into(), name: "resume_agent".into(), arguments: serde_json::json!({"agent_id":uuid::Uuid::new_v4(),"prompt":"replace original constraints"}).to_string() };
        assert!(parse::<ResumeArgs>(&call).is_err());
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    after_id: Option<uuid::Uuid>,
}

fn parse<T: serde::de::DeserializeOwned>(call: &ToolCall) -> Result<T, ToolError> {
    serde_json::from_str(&call.arguments).map_err(|source| ToolError::InvalidArguments {
        tool: call.name.clone(),
        source,
    })
}

impl Agent {
    pub(super) async fn execute_recovery_tool(
        &self,
        call: &ToolCall,
    ) -> Option<Result<String, ToolError>> {
        if !matches!(call.name.as_str(), "resume_agent" | "list_agent_recoveries") {
            return None;
        }
        let Some(catalog) = &self.subagents else {
            return Some(Err(ToolError::UnknownTool(call.name.clone())));
        };
        Some(
            async {
                let result = if call.name == "resume_agent" {
                    catalog
                        .resume_foreground_agent(parse::<ResumeArgs>(call)?.agent_id)
                        .await
                } else {
                    catalog.list_agent_recoveries(parse::<ListArgs>(call)?.after_id)
                };
                result.map_err(|error| ToolError::Network(error.to_string()))
            }
            .await,
        )
    }
}
