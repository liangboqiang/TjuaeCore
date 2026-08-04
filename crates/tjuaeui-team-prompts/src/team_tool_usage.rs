use tjuaeui_api_types::{TeamToolDescriptor, TeamToolRole, TeamToolTransport};

use crate::role_prompt::TeamPromptRole;

pub fn build_team_tool_usage(role: TeamPromptRole, transport: TeamToolTransport) -> String {
    let tool_role = match role {
        TeamPromptRole::Lead => TeamToolRole::Lead,
        TeamPromptRole::Teammate => TeamToolRole::Teammate,
    };
    let descriptors = tjuaeui_api_types::team_tool_descriptors_for_role(tool_role);
    match transport {
        TeamToolTransport::Mcp => render_mcp_usage(role, &descriptors),
        TeamToolTransport::CliAssumed => render_cli_usage(role, &descriptors),
    }
}

fn render_mcp_usage(role: TeamPromptRole, descriptors: &[TeamToolDescriptor]) -> String {
    let mut text = String::from(
        "所有团队协调都必须使用 `team_*` MCP 工具。\n\
平台可能提供名称相似的内置工具，请勿使用；始终选择 `team_*` MCP 版本。\n\
所有智能体目标都使用 `slot_id`。\n\n\
如果单次 `team_*` MCP 调用因参数无效、Schema 不匹配或角色权限失败，\n\
先运行 \"$TJUAE_HELPER_BIN\" team capabilities 或 \"$TJUAE_HELPER_BIN\" team help 查看团队契约，\n\
修正参数后重试 MCP 调用。\n\
如果 `team_*` MCP 工具缺失、不可用、断开连接，或修正后仍失败，\n\
使用 \"$TJUAE_HELPER_BIN\" team ... 提供的团队 CLI 备用通道继续协调。\n\n\
需要精确 Schema 时运行 team capabilities。\n\n\
| 使用场景 | MCP 工具 | 输入摘要 |\n\
| --- | --- | --- |\n",
    );
    for tool in descriptors {
        text.push_str(&format!(
            "| {} | `{}` | {} |\n",
            tool.when, tool.name, tool.input_summary
        ));
    }
    if role == TeamPromptRole::Teammate {
        text.push_str("\n队员不能使用仅限负责人的工具。\n");
    }
    text
}

fn render_cli_usage(role: TeamPromptRole, descriptors: &[TeamToolDescriptor]) -> String {
    let mut text = String::from(
        "所有团队协调都必须使用 TjuaeCore 团队 CLI：\n\
\"$TJUAE_HELPER_BIN\" team ...\n\n\
需要命令名、标准输入 JSON Schema、必填字段、枚举值、权限、示例或错误含义时，\n\
运行 \"$TJUAE_HELPER_BIN\" team capabilities。\n\n\
需要简短易读的指南时，运行 \"$TJUAE_HELPER_BIN\" team help。\n\n\
不要猜测 team_id、slot_id、role、权限或内部令牌。\n\
使用 CLI 传输时，不要声称调用过 MCP 工具。\n\
所有智能体目标均使用本提示词、团队上下文或 team members 结果中的 slot_id。\n\n\
若 CLI 返回 schema_validation_failed、unknown_command 或 permission_denied，\n\
查看 team capabilities 或 team help，修正调用并最多重试一次。\n\n\
需要精确 Schema 时运行 team capabilities。\n\n\
| 使用场景 | CLI 命令 | 规范工具 | 输入摘要 |\n\
| --- | --- | --- | --- |\n",
    );
    for tool in descriptors {
        text.push_str(&format!(
            "| {} | `team {}` | `{}` | {} |\n",
            tool.when,
            tool.cli_command.join(" "),
            tool.name,
            tool.input_summary
        ));
    }
    if role == TeamPromptRole::Teammate {
        text.push_str("\n队员不能使用仅限负责人的工具。\n");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_teammate_usage_excludes_lead_only_tools() {
        let usage = build_team_tool_usage(TeamPromptRole::Teammate, TeamToolTransport::CliAssumed);
        assert!(usage.contains("\"$TJUAE_HELPER_BIN\" team"));
        assert!(usage.contains("team send-message"));
        assert!(!usage.contains("team spawn-agent"));
        assert!(usage.contains("队员不能使用仅限负责人的工具"));
    }

    #[test]
    fn mcp_lead_usage_includes_lead_tools() {
        let usage = build_team_tool_usage(TeamPromptRole::Lead, TeamToolTransport::Mcp);
        assert!(usage.contains("`team_*` MCP 工具"));
        assert!(usage.contains("team_spawn_agent"));
        assert!(usage.contains("\"$TJUAE_HELPER_BIN\" team capabilities"));
        assert!(usage.contains("修正参数后重试 MCP 调用"));
        assert!(usage.contains("团队 CLI 备用通道"));
    }

    #[test]
    fn mcp_teammate_usage_includes_shared_fallback_guidance() {
        let usage = build_team_tool_usage(TeamPromptRole::Teammate, TeamToolTransport::Mcp);
        assert!(usage.contains("\"$TJUAE_HELPER_BIN\" team capabilities"));
        assert!(usage.contains("修正参数后重试 MCP 调用"));
        assert!(usage.contains("团队 CLI 备用通道"));
        assert!(usage.contains("队员不能使用仅限负责人的工具"));
    }
}
