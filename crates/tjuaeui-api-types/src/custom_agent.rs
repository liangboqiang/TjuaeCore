//! 自定义 Agent 增删改查接口的请求与响应类型。
//!
//! 自定义 Agent 是 `agent_metadata` 表中的用户定义记录。ACP Agent 复用
//! 内置 Agent 的进程启动路径，A2A Agent 则保存远程发现信息；两者均由设置页
//! 通过 `/api/agents/custom/*` 接口维护。

use serde::{Deserialize, Serialize};

use crate::agent_discovery::{AgentEnvEntry, BehaviorPolicy};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CustomAgentProtocol {
    #[default]
    Acp,
    A2a,
}

/// `PUT /api/agents/{id}/overrides` 的请求体。
#[derive(Debug, Clone, Deserialize)]
pub struct SetAgentOverridesRequest {
    #[serde(default)]
    pub command_override: Option<String>,
    #[serde(default)]
    pub env_override: Option<Vec<AgentEnvEntry>>,
}

/// `GET /api/agents/{id}/overrides` 的响应体。
#[derive(Debug, Clone, Serialize)]
pub struct AgentOverridesResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_override: Option<String>,
    pub env_override: Vec<AgentEnvEntry>,
}

/// `POST /api/agents/custom` 与 `PUT /api/agents/custom/{id}` 共用的请求体。
///
/// ACP 使用 `command`、`args`、`env` 与 `advanced`，A2A 使用 `endpoint`
/// 及认证字段。`advanced` 中的未知字段按 serde 默认行为忽略。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomAgentUpsertRequest {
    pub name: String,
    #[serde(default)]
    pub protocol: CustomAgentProtocol,
    #[serde(default)]
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,
    #[serde(default)]
    pub allow_insecure: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<AgentEnvEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advanced: Option<CustomAgentAdvancedOverrides>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TryConnectA2aAgentRequest {
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,
    #[serde(default)]
    pub allow_insecure: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TryConnectA2aAgentResponse {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub endpoint: String,
}

/// ACP 高级 JSON 编辑器暴露的可选覆盖项。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CustomAgentAdvancedOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yolo_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_skills_dirs: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior_policy: Option<BehaviorPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// `PATCH /api/agents/{id}/enabled` 的请求体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetEnabledRequest {
    pub enabled: bool,
}

/// `DELETE /api/agents/custom/{id}` 的响应体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteCustomAgentResponse {
    pub deleted: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn advanced_silently_drops_unknown_keys() {
        let payload = json!({
            "yolo_id": "bypassPermissions",
            "unknown_field": 42,
            "another": "ignored"
        });
        let parsed: CustomAgentAdvancedOverrides = serde_json::from_value(payload).unwrap();
        assert_eq!(parsed.yolo_id.as_deref(), Some("bypassPermissions"));
        let roundtrip = serde_json::to_value(&parsed).unwrap();
        assert!(roundtrip.get("unknown_field").is_none());
        assert!(roundtrip.get("another").is_none());
    }

    #[test]
    fn upsert_request_minimal_payload() {
        let payload = json!({
            "name": "My Agent",
            "command": "my-cli"
        });
        let req: CustomAgentUpsertRequest = serde_json::from_value(payload).unwrap();
        assert_eq!(req.name, "My Agent");
        assert_eq!(req.protocol, CustomAgentProtocol::Acp);
        assert_eq!(req.command, "my-cli");
        assert!(req.args.is_empty());
        assert!(req.env.is_empty());
        assert!(req.advanced.is_none());
    }

    #[test]
    fn upsert_request_accepts_a2a_protocol_without_command() {
        let payload = json!({
            "name": "Remote Planner",
            "protocol": "a2a",
            "endpoint": "https://agent.example.com"
        });
        let req: CustomAgentUpsertRequest = serde_json::from_value(payload).unwrap();

        assert_eq!(req.protocol, CustomAgentProtocol::A2a);
        assert!(req.command.is_empty());
        assert_eq!(req.endpoint.as_deref(), Some("https://agent.example.com"));
    }
}
