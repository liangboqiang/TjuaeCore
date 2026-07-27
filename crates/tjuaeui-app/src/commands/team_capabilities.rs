use serde_json::{Value, json};
use tjuaeui_api_types::{TEAM_TOOLS_SCHEMA_VERSION, team_tool_descriptors};

pub(crate) fn data() -> Value {
    let tools = team_tool_descriptors()
        .into_iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "cli_command": tool.cli_command,
                "permission": tool.permission,
                "lead_only": tool.lead_only(),
                "description": tool.description,
                "when": tool.when,
                "input_summary": tool.input_summary,
                "stdin_json_schema": tool.input_schema,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "schema_version": TEAM_TOOLS_SCHEMA_VERSION,
        "contract": "agent-facing-team-cli",
        "commands": {
            "capabilities": { "runtime_env_required": [] },
            "help": { "runtime_env_required": [] },
            "context": { "runtime_env_required": ["TJUAE_BASE_URL", "TJUAE_USER_ID", "TJUAE_CONVERSATION_ID", "TJUAE_RUNTIME_TOKEN"] },
            "tool_call": { "runtime_env_required": ["TJUAE_BASE_URL", "TJUAE_USER_ID", "TJUAE_CONVERSATION_ID", "TJUAE_RUNTIME_TOKEN"] }
        },
        "output_envelope": {
            "success": "boolean",
            "data": "success=true 时为对象",
            "error": "success=false 时为对象",
            "meta": { "schema_version": TEAM_TOOLS_SCHEMA_VERSION }
        },
        "tools": tools,
        "errors": [
            "unknown_tool",
            "schema_validation_failed",
            "permission_denied",
            "team_not_found",
            "conversation_not_found",
            "agent_not_found",
            "not_in_team",
            "transport_unavailable",
            "runtime_context_missing",
            "runtime_auth_failed"
        ]
    })
}

pub(crate) fn help_markdown() -> String {
    let mut text = String::from("# TjuaeCore 团队 CLI\n\n使用 `tjuaecore team capabilities` 查看准确 Schema。\n\n");
    for tool in team_tool_descriptors() {
        let command = tool.cli_command.join(" ");
        let permission = if tool.lead_only() {
            "仅限负责人"
        } else {
            "任意团队智能体"
        };
        text.push_str(&format!(
            "- `tjuaecore team {command}` -> `{}` ({permission}): {}\n",
            tool.name, tool.input_summary
        ));
    }
    text
}
