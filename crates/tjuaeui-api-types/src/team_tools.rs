use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const TEAM_TOOLS_SCHEMA_VERSION: u32 = 1;

pub const TEAM_SPAWN_AGENT_DESCRIPTION: &str = r#"创建一个加入团队的新队员智能体。

只有满足以下条件之一时才能使用：
- 用户已在之前的消息中明确批准人员方案。
- 用户明确要求立即创建某个指定队员。

正常规划流程中，调用本工具前必须：
- 先用一句话说明增加队员的价值。
- 告知用户推荐哪些队员。
- 用表格列出名称、职责和推荐助手。
- 询问按原方案创建还是调整名称、职责或助手。
- 提醒用户：后续若配置不合适，仍可要求替换或调整队员。
- 不要在提出方案的同一回合调用本工具；等待后续消息中的明确批准。

调用时必须提供助手目录中的 assistant_id，不要传模型。新队员使用所选助手的
已配置或默认模型，用户可以在 UI 模型选择器中调整。

新智能体创建并加入团队后，可以向其分配任务和发送消息。"#;

pub const TEAM_DESCRIBE_ASSISTANT_DESCRIPTION: &str = "创建队员前获取助手详情。\n\n\
返回助手的完整说明、已启用技能和示例任务，以判断是否符合用户请求。系统提示词中的\n\
单行目录有两个或更多相关候选时使用本工具。\n\n\
先用 team_list_assistants 查找候选 assistant_id；确认匹配后，使用同一 assistant_id\n\
调用 team_spawn_agent。";

pub const TEAM_LIST_ASSISTANTS_DESCRIPTION: &str = "列出可用于创建团队队员的助手。返回真实助手目录，\
包括 assistant_id、名称、后端、说明和技能。\n\n需要准确的队员 assistant_id 时，应在 \
team_spawn_agent 之前调用本工具。不要根据 claude/codex/gemini 等后端名称猜测；\
只能使用本工具返回的 assistant_id。";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamToolPermission {
    AnyTeamAgent,
    LeadOnly,
}

impl TeamToolPermission {
    pub fn is_lead_only(self) -> bool {
        self == Self::LeadOnly
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamToolRole {
    Lead,
    Teammate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamToolTransport {
    Mcp,
    CliAssumed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamToolName {
    TeamMembers,
    TeamSendMessage,
    TeamTaskCreate,
    TeamTaskUpdate,
    TeamTaskList,
    TeamListAssistants,
    TeamDescribeAssistant,
    TeamSpawnAgent,
    TeamRenameAgent,
    TeamShutdownAgent,
}

impl TeamToolName {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TeamMembers => "team_members",
            Self::TeamSendMessage => "team_send_message",
            Self::TeamTaskCreate => "team_task_create",
            Self::TeamTaskUpdate => "team_task_update",
            Self::TeamTaskList => "team_task_list",
            Self::TeamListAssistants => "team_list_assistants",
            Self::TeamDescribeAssistant => "team_describe_assistant",
            Self::TeamSpawnAgent => "team_spawn_agent",
            Self::TeamRenameAgent => "team_rename_agent",
            Self::TeamShutdownAgent => "team_shutdown_agent",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "team_members" => Self::TeamMembers,
            "team_send_message" => Self::TeamSendMessage,
            "team_task_create" => Self::TeamTaskCreate,
            "team_task_update" => Self::TeamTaskUpdate,
            "team_task_list" => Self::TeamTaskList,
            "team_list_assistants" => Self::TeamListAssistants,
            "team_describe_assistant" => Self::TeamDescribeAssistant,
            "team_spawn_agent" => Self::TeamSpawnAgent,
            "team_rename_agent" => Self::TeamRenameAgent,
            "team_shutdown_agent" => Self::TeamShutdownAgent,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamToolDescriptor {
    pub name: String,
    pub permission: TeamToolPermission,
    pub description: String,
    pub input_schema: Value,
    pub cli_command: Vec<String>,
    pub when: String,
    pub input_summary: String,
}

impl TeamToolDescriptor {
    pub fn lead_only(&self) -> bool {
        self.permission.is_lead_only()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamToolCall {
    pub tool: TeamToolName,
    #[serde(default)]
    pub arguments: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamToolErrorCode {
    UnknownTool,
    SchemaValidationFailed,
    PermissionDenied,
    TeamNotFound,
    ConversationNotFound,
    AgentNotFound,
    NotInTeam,
    TransportUnavailable,
    RuntimeContextMissing,
    RuntimeAuthFailed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeamToolErrorPayload {
    pub code: TeamToolErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl TeamToolErrorPayload {
    pub fn new(code: TeamToolErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamToolCliMeta {
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamToolCliEnvelope<T> {
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<TeamToolErrorPayload>,
    pub meta: TeamToolCliMeta,
}

impl<T> TeamToolCliEnvelope<T> {
    pub fn success(data: T, command: Option<String>) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
            meta: TeamToolCliMeta {
                schema_version: TEAM_TOOLS_SCHEMA_VERSION,
                command,
            },
        }
    }

    pub fn failure(error: TeamToolErrorPayload, command: Option<String>) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(error),
            meta: TeamToolCliMeta {
                schema_version: TEAM_TOOLS_SCHEMA_VERSION,
                command,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamToolContextResponse {
    pub in_team: bool,
    pub conversation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<TeamToolRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<TeamToolTransport>,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamToolRuntimeCallRequest {
    pub tool: TeamToolName,
    #[serde(default)]
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamToolRuntimeCallResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<TeamToolErrorPayload>,
}

pub fn team_tool_descriptors() -> Vec<TeamToolDescriptor> {
    tool_specs()
        .into_iter()
        .map(|spec| TeamToolDescriptor {
            name: spec.name.as_str().to_owned(),
            permission: spec.permission,
            description: spec.description.to_owned(),
            input_schema: spec.input_schema,
            cli_command: spec.cli_command.iter().map(|part| (*part).to_owned()).collect(),
            when: spec.when.to_owned(),
            input_summary: spec.input_summary.to_owned(),
        })
        .collect()
}

pub fn team_tool_descriptors_for_role(role: TeamToolRole) -> Vec<TeamToolDescriptor> {
    let is_lead = role == TeamToolRole::Lead;
    team_tool_descriptors()
        .into_iter()
        .filter(|descriptor| is_lead || !descriptor.permission.is_lead_only())
        .collect()
}

pub fn team_tool_descriptor(name: &str) -> Option<TeamToolDescriptor> {
    team_tool_descriptors()
        .into_iter()
        .find(|descriptor| descriptor.name == name)
}

pub fn cli_command_for_tool(name: &str) -> Option<&'static [&'static str]> {
    tool_specs()
        .into_iter()
        .find(|spec| spec.name.as_str() == name)
        .map(|spec| spec.cli_command)
}

pub fn tool_name_for_cli_path(path: &[String]) -> Option<TeamToolName> {
    tool_specs()
        .into_iter()
        .find(|spec| spec.cli_command == path.iter().map(String::as_str).collect::<Vec<_>>())
        .map(|spec| spec.name)
}

#[derive(Debug, Clone)]
struct TeamToolSpec {
    name: TeamToolName,
    permission: TeamToolPermission,
    description: &'static str,
    input_schema: Value,
    cli_command: &'static [&'static str],
    when: &'static str,
    input_summary: &'static str,
}

fn tool_specs() -> Vec<TeamToolSpec> {
    vec![
        TeamToolSpec {
            name: TeamToolName::TeamMembers,
            permission: TeamToolPermission::AnyTeamAgent,
            description: "列出所有团队成员及其角色和当前状态。",
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {}
            }),
            cli_command: &["members"],
            when: "检查成员和状态",
            input_summary: "{}",
        },
        TeamToolSpec {
            name: TeamToolName::TeamSendMessage,
            permission: TeamToolPermission::AnyTeamAgent,
            description: "向队员发送消息，或通过 to=\"*\" 广播。委派依赖用户附件的工作时，在 files 中转发附件绝对路径。",
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "to": { "type": "string", "description": "目标智能体 slot_id；广播时使用 \"*\"" },
                    "message": { "type": "string", "description": "消息内容" },
                    "files": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "要转发给目标智能体的附件绝对路径"
                    }
                },
                "required": ["to", "message"]
            }),
            cli_command: &["send-message"],
            when: "发送队员消息",
            input_summary: "to, message",
        },
        TeamToolSpec {
            name: TeamToolName::TeamTaskCreate,
            permission: TeamToolPermission::AnyTeamAgent,
            description: "在团队任务看板上创建任务。",
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "subject": { "type": "string", "description": "任务主题" },
                    "description": { "type": "string", "description": "任务说明" },
                    "owner": { "type": "string", "description": "负责智能体的 slot_id" },
                    "blocked_by": { "type": "array", "items": { "type": "string" }, "description": "本任务依赖的任务 ID" }
                },
                "required": ["subject"]
            }),
            cli_command: &["task", "create"],
            when: "创建任务",
            input_summary: "subject，可选 owner/deps",
        },
        TeamToolSpec {
            name: TeamToolName::TeamTaskUpdate,
            permission: TeamToolPermission::AnyTeamAgent,
            description: "更新团队任务看板中的现有任务。",
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "task_id": { "type": "string", "description": "要更新的任务 ID" },
                    "status": { "type": "string", "enum": ["pending", "in_progress", "completed", "deleted"], "description": "新状态" },
                    "description": { "type": "string", "description": "新说明" },
                    "owner": { "type": "string", "description": "新的负责智能体 slot_id" },
                    "blocked_by": { "type": "array", "items": { "type": "string" }, "description": "新依赖列表" }
                },
                "required": ["task_id"]
            }),
            cli_command: &["task", "update"],
            when: "更新任务",
            input_summary: "task_id，可选 status/owner/deps",
        },
        TeamToolSpec {
            name: TeamToolName::TeamTaskList,
            permission: TeamToolPermission::AnyTeamAgent,
            description: "列出团队任务看板。传入 {} 返回完整看板，也可用 owner/status/include_deleted/limit 筛选。",
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "owner": {
                        "type": "string",
                        "description": "只返回由该智能体 slot_id 负责的任务"
                    },
                    "status": {
                        "description": "只返回指定状态的任务；接受一个状态字符串或状态字符串数组。",
                        "anyOf": [
                            {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed", "deleted"]
                            },
                            {
                                "type": "array",
                                "minItems": 1,
                                "items": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed", "deleted"]
                                }
                            }
                        ]
                    },
                    "include_deleted": {
                        "type": "boolean",
                        "description": "未指定 status 时是否包含已删除任务；默认为 true，因此 {} 返回完整看板。"
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "最多返回的任务数；大于 200 的值会限制为 200。"
                    }
                }
            }),
            cli_command: &["task", "list"],
            when: "列出任务",
            input_summary: "owner, status, include_deleted, limit",
        },
        TeamToolSpec {
            name: TeamToolName::TeamListAssistants,
            permission: TeamToolPermission::AnyTeamAgent,
            description: TEAM_LIST_ASSISTANTS_DESCRIPTION,
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {}
            }),
            cli_command: &["list-assistants"],
            when: "列出可创建队员的助手",
            input_summary: "{}",
        },
        TeamToolSpec {
            name: TeamToolName::TeamDescribeAssistant,
            permission: TeamToolPermission::AnyTeamAgent,
            description: TEAM_DESCRIBE_ASSISTANT_DESCRIPTION,
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "assistant_id": { "type": "string", "description": "可用助手目录中的助手 ID，例如 \"word-creator\"。" },
                    "locale": { "type": "string", "description": "语言区域，例如 \"zh-CN\" 或 \"en-US\"；省略时使用用户当前 UI 语言。" }
                },
                "required": ["assistant_id"]
            }),
            cli_command: &["describe-assistant"],
            when: "查看助手详情",
            input_summary: "assistant_id，可选 locale",
        },
        TeamToolSpec {
            name: TeamToolName::TeamSpawnAgent,
            permission: TeamToolPermission::LeadOnly,
            description: TEAM_SPAWN_AGENT_DESCRIPTION,
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "name": { "type": "string", "description": "智能体显示名称" },
                    "assistant_id": { "type": "string", "description": "用于创建队员的助手 ID。需要候选项时调用 team_list_assistants；运行后端由该助手确定。" }
                },
                "required": ["name", "assistant_id"]
            }),
            cli_command: &["spawn-agent"],
            when: "创建队员",
            input_summary: "name, assistant_id",
        },
        TeamToolSpec {
            name: TeamToolName::TeamRenameAgent,
            permission: TeamToolPermission::LeadOnly,
            description: "重命名团队成员；仅限负责人。",
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "slot_id": { "type": "string", "description": "要重命名的智能体 slot_id" },
                    "new_name": { "type": "string", "description": "新显示名称" }
                },
                "required": ["slot_id", "new_name"]
            }),
            cli_command: &["rename-agent"],
            when: "重命名队员",
            input_summary: "slot_id, new_name",
        },
        TeamToolSpec {
            name: TeamToolName::TeamShutdownAgent,
            permission: TeamToolPermission::LeadOnly,
            description: "发起队员下线；仅限负责人。向目标智能体发送 shutdown_request。",
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "slot_id": { "type": "string", "description": "要下线的智能体 slot_id" },
                    "reason": { "type": "string", "description": "下线原因" }
                },
                "required": ["slot_id"]
            }),
            cli_command: &["shutdown-agent"],
            when: "下线队员",
            input_summary: "slot_id，可选 reason",
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn descriptor_count_and_names_are_unique() {
        let descriptors = team_tool_descriptors();
        assert_eq!(descriptors.len(), 10);
        let names = descriptors
            .iter()
            .map(|descriptor| descriptor.name.as_str())
            .collect::<HashSet<_>>();
        assert_eq!(names.len(), descriptors.len());
    }

    #[test]
    fn descriptors_have_required_prompt_and_schema_fields() {
        for descriptor in team_tool_descriptors() {
            assert!(!descriptor.name.is_empty());
            assert!(!descriptor.description.is_empty());
            assert!(!descriptor.when.is_empty());
            assert!(!descriptor.input_summary.is_empty());
            assert!(!descriptor.cli_command.is_empty());
            assert_eq!(descriptor.input_schema["type"], "object");
        }
    }

    #[test]
    fn cli_command_mapping_matches_spec() {
        let cases = [
            ("team_members", vec!["members"]),
            ("team_send_message", vec!["send-message"]),
            ("team_task_create", vec!["task", "create"]),
            ("team_task_update", vec!["task", "update"]),
            ("team_task_list", vec!["task", "list"]),
            ("team_list_assistants", vec!["list-assistants"]),
            ("team_describe_assistant", vec!["describe-assistant"]),
            ("team_spawn_agent", vec!["spawn-agent"]),
            ("team_rename_agent", vec!["rename-agent"]),
            ("team_shutdown_agent", vec!["shutdown-agent"]),
        ];
        for (tool, path) in cases {
            assert_eq!(cli_command_for_tool(tool), Some(path.as_slice()));
            let owned = path.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert_eq!(tool_name_for_cli_path(&owned).map(TeamToolName::as_str), Some(tool));
        }
    }

    #[test]
    fn teammate_role_hides_lead_only_tools() {
        let names = team_tool_descriptors_for_role(TeamToolRole::Teammate)
            .into_iter()
            .map(|descriptor| descriptor.name)
            .collect::<Vec<_>>();
        assert!(!names.contains(&"team_spawn_agent".to_owned()));
        assert!(!names.contains(&"team_rename_agent".to_owned()));
        assert!(!names.contains(&"team_shutdown_agent".to_owned()));
        assert!(names.contains(&"team_send_message".to_owned()));
    }

    #[test]
    fn spawn_schema_is_assistant_first_and_excludes_legacy_fields() {
        let descriptor = team_tool_descriptor("team_spawn_agent").expect("spawn descriptor");
        assert_eq!(descriptor.permission, TeamToolPermission::LeadOnly);
        let props = descriptor.input_schema["properties"].as_object().unwrap();
        assert!(props.contains_key("name"));
        assert!(props.contains_key("assistant_id"));
        assert!(!props.contains_key("model"));
        assert!(!props.contains_key("backend"));
        assert!(!props.contains_key("agent_type"));
        assert!(!props.contains_key("role"));
        let required = descriptor.input_schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("name")));
        assert!(required.contains(&json!("assistant_id")));
    }
}
