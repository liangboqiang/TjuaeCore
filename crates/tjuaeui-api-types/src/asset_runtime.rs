use serde::{Deserialize, Serialize};

use crate::AssetKind;

/// Core 独立维护的资产运行状态。
///
/// 该状态与安装、跟踪、同步和操作状态相互独立，UI 不得根据其他状态推导。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AssetRuntimeState {
    NotConfigured,
    Inactive,
    Activating,
    Active,
    Degraded,
    NeedsRepair,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AssetRuntimeHealthStatus {
    Unknown,
    Healthy,
    Unhealthy,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AssetRuntimeProjectionKind {
    Assistant,
    EngineAdapter,
    Skill,
    Mcp,
}

/// 可重建的运行投影绑定，不是资产 Definition 的第二事实源。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetRuntimeBindingResponse {
    pub asset_id: String,
    pub kind: AssetKind,
    pub projection_kind: AssetRuntimeProjectionKind,
    /// Definition/Hub 可移植运行身份。Core 内部 projectionRuntimeId 永不出现在
    /// HTTP、Hub 包或 Trace 契约中。
    pub portable_runtime_id: String,
    pub definition_digest: String,
    pub overlay_version: i64,
    pub health_status: AssetRuntimeHealthStatus,
    /// 仅用于证明当前 binding 来自同一 Definition/Overlay 的成功试跑。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub try_run_receipt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error_code: Option<String>,
    pub projected_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_checked_at: Option<i64>,
}

/// 环境变量、请求头等公开配置只引用逻辑凭据槽，不返回密文或明文。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetNamedSecretSlot {
    pub name: String,
    pub secret_slot: String,
}

/// Definition 配置字段所使用的逻辑凭据槽。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetKeyedSecretSlot {
    pub key: String,
    pub secret_slot: String,
}

/// 可公开返回并持久化到 Overlay 的非敏感原子值。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum AssetPrimitiveValue {
    String(String),
    Number(serde_json::Number),
    Boolean(bool),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetConfigurationValue {
    pub key: String,
    pub value: AssetPrimitiveValue,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssistantAssetConfiguration {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_asset_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillAssetConfiguration {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EngineAdapterAssetConfiguration {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    #[serde(default)]
    pub environment: Vec<AssetNamedSecretSlot>,
    #[serde(default)]
    pub values: Vec<AssetConfigurationValue>,
    #[serde(default)]
    pub secrets: Vec<AssetKeyedSecretSlot>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum McpAssetTransport {
    Stdio,
    Sse,
    StreamableHttp,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpAssetConfiguration {
    pub transport: McpAssetTransport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable_path: Option<String>,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_url: Option<String>,
    #[serde(default)]
    pub environment: Vec<AssetNamedSecretSlot>,
    #[serde(default)]
    pub headers: Vec<AssetNamedSecretSlot>,
    #[serde(default)]
    pub values: Vec<AssetConfigurationValue>,
    #[serde(default)]
    pub secrets: Vec<AssetKeyedSecretSlot>,
}

/// 按资产类型判别的公开配置。序列化后形如
/// `{ "kind": "mcp", "configuration": { ... } }`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "configuration", rename_all = "camelCase")]
pub enum AssetPublicConfiguration {
    Assistant(AssistantAssetConfiguration),
    EngineAdapter(EngineAdapterAssetConfiguration),
    Skill(SkillAssetConfiguration),
    Mcp(McpAssetConfiguration),
}

impl AssetPublicConfiguration {
    pub fn kind(&self) -> AssetKind {
        match self {
            Self::Assistant(_) => AssetKind::Assistant,
            Self::EngineAdapter(_) => AssetKind::EngineAdapter,
            Self::Skill(_) => AssetKind::Skill,
            Self::Mcp(_) => AssetKind::Mcp,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetOverlayResponse {
    pub asset_id: String,
    pub kind: AssetKind,
    pub configuration: AssetPublicConfiguration,
    pub secret_slots: Vec<AssetSecretSlotResponse>,
    pub version: i64,
    pub updated_at: i64,
}

/// 单个逻辑凭据槽的脱敏状态。固定掩码不能泄露长度或字符类别。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetSecretSlotResponse {
    pub slot: String,
    pub configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub masked_value: Option<String>,
}

/// 凭据更新只允许“设置”或“清除”，不存在可读的 valueRef。
#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "camelCase", deny_unknown_fields)]
pub enum AssetSecretUpdate {
    Set { slot: String, value: String },
    Clear { slot: String },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigureAssetRequest {
    pub configuration: AssetPublicConfiguration,
    #[serde(default)]
    pub secret_updates: Vec<AssetSecretUpdate>,
    /// 首次配置时省略；更新时必须等于当前版本。
    #[serde(default)]
    pub expected_version: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetRuntimeCommandRequest {
    pub idempotency_key: String,
    pub expected_definition_digest: String,
    #[serde(default)]
    pub expected_overlay_version: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetRuntimeStatusResponse {
    pub asset_id: String,
    pub kind: AssetKind,
    pub runtime_state: AssetRuntimeState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay_version: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_binding: Option<AssetRuntimeBindingResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_configuration_never_accepts_inline_environment_values() {
        let value = serde_json::json!({
            "kind": "engineAdapter",
            "configuration": {
                "command": "demo",
                "environment": [{
                    "name": "API_TOKEN",
                    "value": "secret"
                }]
            }
        });
        assert!(serde_json::from_value::<AssetPublicConfiguration>(value).is_err());
    }

    #[test]
    fn mcp_overlay_contract_is_typed_and_camel_case() {
        let value = serde_json::json!({
            "kind": "mcp",
            "configuration": {
                "transport": "streamableHttp",
                "instanceUrl": "https://example.invalid/mcp",
                "headers": [{
                    "name": "Authorization",
                    "secretSlot": "mcp-authorization"
                }],
                "values": [{"key": "timeoutSeconds", "value": 30}],
                "secrets": [{"key": "apiToken", "secretSlot": "mcp-api-token"}]
            }
        });
        let configuration: AssetPublicConfiguration = serde_json::from_value(value).unwrap();
        assert_eq!(configuration.kind(), AssetKind::Mcp);
    }

    #[test]
    fn skill_overlay_is_empty_and_rejects_removed_assistant_bindings() {
        let empty = serde_json::json!({
            "kind": "skill",
            "configuration": {}
        });
        assert_eq!(
            serde_json::from_value::<AssetPublicConfiguration>(empty).unwrap(),
            AssetPublicConfiguration::Skill(SkillAssetConfiguration {})
        );

        let mut removed_configuration = serde_json::Map::new();
        removed_configuration.insert(
            ["assistant", "Asset", "Ids"].concat(),
            serde_json::json!(["assistant-local-id"]),
        );
        let removed = serde_json::json!({
            "kind": "skill",
            "configuration": removed_configuration
        });
        assert!(serde_json::from_value::<AssetPublicConfiguration>(removed).is_err());
    }

    #[test]
    fn overlay_response_exposes_only_a_fixed_mask() {
        let response = AssetOverlayResponse {
            asset_id: "engine:demo".into(),
            kind: AssetKind::EngineAdapter,
            configuration: AssetPublicConfiguration::EngineAdapter(EngineAdapterAssetConfiguration::default()),
            secret_slots: vec![AssetSecretSlotResponse {
                slot: "api-token".into(),
                configured: true,
                masked_value: Some("••••••".into()),
            }],
            version: 1,
            updated_at: 1,
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("••••••"));
        assert!(!json.contains("plaintext"));
    }

    #[test]
    fn runtime_binding_exposes_only_the_portable_runtime_identity() {
        let response = AssetRuntimeBindingResponse {
            asset_id: "skill:demo".into(),
            kind: AssetKind::Skill,
            projection_kind: AssetRuntimeProjectionKind::Skill,
            portable_runtime_id: "portable-skill-demo".into(),
            definition_digest: "sha256-demo".into(),
            overlay_version: 0,
            health_status: AssetRuntimeHealthStatus::Healthy,
            try_run_receipt_id: Some("receipt-demo".into()),
            last_error_code: None,
            projected_at: 1,
            health_checked_at: Some(1),
        };

        let value = serde_json::to_value(response).unwrap();
        assert_eq!(value["portableRuntimeId"], "portable-skill-demo");
        assert!(value.get("runtimeId").is_none());
        assert!(value.get("projectionRuntimeId").is_none());
        assert!(!value.to_string().contains("tjuae-proj-v1-"));
    }
}
