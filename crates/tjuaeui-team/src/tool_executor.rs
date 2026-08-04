use std::sync::Weak;
use std::time::Instant;

use serde_json::Value;
use tjuaeui_api_types::{
    TeamToolCall, TeamToolDescriptor, TeamToolErrorCode, TeamToolErrorPayload, TeamToolName, TeamToolRole,
    TeamToolTransport,
};
use tracing::{info, warn};

use crate::mcp::server::ToolCallError;
use crate::scheduler::TeammateManager;
use crate::service::TeamSessionService;
use crate::types::TeammateRole;

#[derive(Debug, Clone)]
pub struct TeamToolContext {
    pub team_id: String,
    pub caller_slot_id: String,
    pub caller_role: TeammateRole,
    pub user_id: Option<String>,
    pub conversation_id: Option<String>,
    pub transport: TeamToolTransport,
}

pub struct TeamToolExecutor<'a> {
    scheduler: &'a TeammateManager,
    service: &'a Weak<TeamSessionService>,
}

impl<'a> TeamToolExecutor<'a> {
    pub fn new(scheduler: &'a TeammateManager, service: &'a Weak<TeamSessionService>) -> Self {
        Self { scheduler, service }
    }

    pub fn list_tools(&self, context: &TeamToolContext) -> Vec<TeamToolDescriptor> {
        tjuaeui_api_types::team_tool_descriptors_for_role(team_tool_role(context.caller_role))
    }

    pub async fn execute(&self, context: &TeamToolContext, call: TeamToolCall) -> Result<Value, TeamToolErrorPayload> {
        let started = Instant::now();
        let tool = call.tool.as_str();
        let result = crate::mcp::server::dispatch_tool(
            tool,
            &call.arguments,
            self.scheduler,
            self.service,
            &context.team_id,
            &context.caller_slot_id,
            context.caller_role,
        )
        .await
        .map(|text| serde_json::from_str(&text).unwrap_or(Value::String(text)))
        .map_err(map_tool_error);

        let duration_ms = started.elapsed().as_millis();
        match &result {
            Ok(_) => info!(
                transport = ?context.transport,
                team_id = %context.team_id,
                caller_slot_id = %context.caller_slot_id,
                conversation_id = context.conversation_id.as_deref().unwrap_or(""),
                tool,
                result = "success",
                duration_ms,
                "Team tool call completed"
            ),
            Err(error) => warn!(
                transport = ?context.transport,
                team_id = %context.team_id,
                caller_slot_id = %context.caller_slot_id,
                conversation_id = context.conversation_id.as_deref().unwrap_or(""),
                tool,
                error_code = ?error.code,
                classification = error.details.as_ref().and_then(|details| details.get("classification")).and_then(|value| value.as_str()).unwrap_or("business_error"),
                duration_ms,
                "Team tool call failed"
            ),
        }
        result
    }
}

pub fn team_tool_call_from_name(tool_name: &str, arguments: Value) -> Result<TeamToolCall, TeamToolErrorPayload> {
    let tool = TeamToolName::parse(tool_name)
        .ok_or_else(|| TeamToolErrorPayload::new(TeamToolErrorCode::UnknownTool, format!("未知工具：{tool_name}")))?;
    Ok(TeamToolCall { tool, arguments })
}

fn team_tool_role(role: TeammateRole) -> TeamToolRole {
    match role {
        TeammateRole::Lead => TeamToolRole::Lead,
        TeammateRole::Teammate => TeamToolRole::Teammate,
    }
}

fn map_tool_error(error: ToolCallError) -> TeamToolErrorPayload {
    let code = if error.message.starts_with("未知工具：") {
        TeamToolErrorCode::UnknownTool
    } else if error.message.starts_with("只有团队负责人") {
        TeamToolErrorCode::PermissionDenied
    } else if error.message.starts_with("参数无效")
        || error.message.starts_with("缺少必填字段")
        || error.message.contains("不接受参数")
        || error.message.contains("不再接受")
    {
        TeamToolErrorCode::SchemaValidationFailed
    } else if error.message.contains("Invalid agent target") {
        TeamToolErrorCode::AgentNotFound
    } else if error.message.contains("没有活动会话") {
        TeamToolErrorCode::TeamNotFound
    } else if error.message.contains("团队服务不可用") {
        TeamToolErrorCode::TransportUnavailable
    } else {
        TeamToolErrorCode::RuntimeContextMissing
    };

    let mut payload = TeamToolErrorPayload::new(code, error.message);
    if let Some(details) = error.details {
        payload = payload.with_details(details);
    }
    payload
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unknown_tool_maps_to_shared_error_payload() {
        let err = team_tool_call_from_name("missing_tool", json!({})).unwrap_err();
        assert_eq!(err.code, TeamToolErrorCode::UnknownTool);
        assert!(err.message.contains("未知工具"));
    }
}
