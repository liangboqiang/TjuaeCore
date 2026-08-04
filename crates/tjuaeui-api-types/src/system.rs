use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const DEFAULT_NETWORK_PROXY_BYPASS: &str = "localhost,127.0.0.1,::1";

/// Tjuae 全局网络代理模式。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NetworkProxyMode {
    FollowSystem,
    Manual,
    Disabled,
}

/// 持久化的全局网络代理设置。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkProxySettings {
    pub mode: NetworkProxyMode,
    pub proxy_url: Option<String>,
    pub no_proxy: String,
}

impl Default for NetworkProxySettings {
    fn default() -> Self {
        Self {
            mode: NetworkProxyMode::FollowSystem,
            proxy_url: None,
            no_proxy: DEFAULT_NETWORK_PROXY_BYPASS.to_owned(),
        }
    }
}

/// 当前生效代理的连接状态。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NetworkProxyState {
    Active,
    Direct,
}

/// 当前生效代理的来源。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NetworkProxySource {
    Manual,
    Environment,
    WindowsSystem,
    Disabled,
    None,
}

/// 系统代理解析时发现的非致命问题。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NetworkProxyWarning {
    PacUnsupported,
    InvalidSystemProxy,
}

/// 当前实际生效的代理状态。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkProxyStatusResponse {
    pub mode: NetworkProxyMode,
    pub state: NetworkProxyState,
    pub source: NetworkProxySource,
    pub proxy_url: Option<String>,
    pub no_proxy: String,
    pub warning: Option<NetworkProxyWarning>,
}

/// Response for `GET /api/settings`.
///
/// Returns all backend system settings with their current values.
/// When no settings exist in the database, the service layer returns defaults.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SystemSettingsResponse {
    pub language: String,
    pub notification_enabled: bool,
    pub cron_notification_enabled: bool,
    pub command_queue_enabled: bool,
    pub save_upload_to_workspace: bool,
    pub network_proxy: NetworkProxySettings,
}

impl Default for SystemSettingsResponse {
    fn default() -> Self {
        Self {
            language: "en-US".to_owned(),
            notification_enabled: true,
            cron_notification_enabled: false,
            command_queue_enabled: false,
            save_upload_to_workspace: false,
            network_proxy: NetworkProxySettings::default(),
        }
    }
}

/// Request body for `PATCH /api/settings`.
///
/// All fields are optional — only the fields present in the request body
/// are updated. Unknown fields are silently ignored by serde.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateSettingsRequest {
    pub language: Option<String>,
    pub notification_enabled: Option<bool>,
    pub cron_notification_enabled: Option<bool>,
    pub command_queue_enabled: Option<bool>,
    pub save_upload_to_workspace: Option<bool>,
    pub network_proxy: Option<NetworkProxySettings>,
}

impl UpdateSettingsRequest {
    /// Returns `true` if all fields are `None` (no-op update).
    pub fn is_empty(&self) -> bool {
        self.language.is_none()
            && self.notification_enabled.is_none()
            && self.cron_notification_enabled.is_none()
            && self.command_queue_enabled.is_none()
            && self.save_upload_to_workspace.is_none()
            && self.network_proxy.is_none()
    }
}

/// Response for `GET /api/settings/client`.
///
/// A flat key-value map where values can be any JSON type (string,
/// number, boolean). The service layer deserializes stored JSON strings
/// back to their original types.
pub type ClientPreferencesResponse = HashMap<String, Value>;

/// Request body for `PUT /api/settings/client`.
///
/// A flat key-value map for batch updates. A `null` value means
/// the key should be deleted. Non-null values are persisted as-is.
pub type UpdateClientPreferencesRequest = HashMap<String, Value>;

/// Query parameters for `GET /api/system/diagnostics/feedback-report`.
///
/// The UI sends only routing and explicit context. tjuaeCore owns profile
/// resolution, SQL selection, and redaction.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeedbackDiagnosticsQuery {
    pub route_at_open: Option<String>,
    pub route_at_submit: Option<String>,
    pub selected_module: Option<String>,
    /// Optional comma-separated kebab-case profile names.
    pub profiles: Option<String>,
    pub conversation_id: Option<String>,
    pub provider_id: Option<String>,
    pub agent_id: Option<String>,
    pub team_id: Option<String>,
    pub mcp_server_id: Option<String>,
}

/// Response for `GET /api/system/diagnostics/feedback-report`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedbackDiagnosticsResponse {
    pub schema_version: String,
    pub context: FeedbackDiagnosticsContextResponse,
    pub profiles: Vec<FeedbackDiagnosticsProfileResponse>,
    pub privacy: FeedbackDiagnosticsPrivacyResponse,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeedbackDiagnosticsContextResponse {
    pub route_at_open: Option<String>,
    pub route_at_submit: Option<String>,
    pub selected_module: Option<String>,
    pub conversation_id: Option<String>,
    pub provider_id: Option<String>,
    pub agent_id: Option<String>,
    pub team_id: Option<String>,
    pub mcp_server_id: Option<String>,
    pub selected_profiles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedbackDiagnosticsProfileResponse {
    pub name: String,
    pub mode: String,
    pub data: Value,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeedbackDiagnosticsPrivacyResponse {
    pub redaction: String,
    pub raw_content_included: bool,
    pub api_keys_included: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -- SystemSettingsResponse --

    #[test]
    fn test_settings_response_default() {
        let resp = SystemSettingsResponse::default();
        assert_eq!(resp.language, "en-US");
        assert!(resp.notification_enabled);
        assert!(!resp.cron_notification_enabled);
        assert!(!resp.command_queue_enabled);
        assert!(!resp.save_upload_to_workspace);
        assert_eq!(resp.network_proxy, NetworkProxySettings::default());
    }

    #[test]
    fn test_settings_response_serialization_snake_case() {
        let resp = SystemSettingsResponse::default();
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["language"], "en-US");
        assert_eq!(json["notification_enabled"], true);
        assert_eq!(json["cron_notification_enabled"], false);
        assert_eq!(json["command_queue_enabled"], false);
        assert_eq!(json["save_upload_to_workspace"], false);
        assert_eq!(json["network_proxy"]["mode"], "follow_system");
        // Verify snake_case, not camelCase
        assert!(json.get("notificationEnabled").is_none());
        assert!(json.get("cronNotificationEnabled").is_none());
    }

    #[test]
    fn test_settings_response_deserialization_snake_case() {
        let raw = json!({
            "language": "zh-CN",
            "notification_enabled": false,
            "cron_notification_enabled": true,
            "command_queue_enabled": true,
            "save_upload_to_workspace": true,
            "network_proxy": {
                "mode": "manual",
                "proxy_url": "http://127.0.0.1:7897",
                "no_proxy": "localhost,127.0.0.1,::1"
            }
        });
        let resp: SystemSettingsResponse = serde_json::from_value(raw).unwrap();
        assert_eq!(resp.language, "zh-CN");
        assert!(!resp.notification_enabled);
        assert!(resp.cron_notification_enabled);
        assert!(resp.command_queue_enabled);
        assert!(resp.save_upload_to_workspace);
        assert_eq!(resp.network_proxy.mode, NetworkProxyMode::Manual);
    }

    #[test]
    fn test_settings_response_roundtrip() {
        let original = SystemSettingsResponse {
            language: "ja-JP".to_owned(),
            notification_enabled: false,
            cron_notification_enabled: true,
            command_queue_enabled: true,
            save_upload_to_workspace: true,
            network_proxy: NetworkProxySettings {
                mode: NetworkProxyMode::Disabled,
                ..Default::default()
            },
        };
        let json = serde_json::to_string(&original).unwrap();
        let parsed: SystemSettingsResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, original);
    }

    // -- UpdateSettingsRequest --

    #[test]
    fn test_update_request_partial_fields() {
        let raw = r#"{"language":"zh-CN"}"#;
        let req: UpdateSettingsRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(req.language.as_deref(), Some("zh-CN"));
        assert!(req.notification_enabled.is_none());
        assert!(req.cron_notification_enabled.is_none());
        assert!(req.command_queue_enabled.is_none());
        assert!(req.save_upload_to_workspace.is_none());
        assert!(req.network_proxy.is_none());
    }

    #[test]
    fn test_update_request_empty_body() {
        let raw = r#"{}"#;
        let req: UpdateSettingsRequest = serde_json::from_str(raw).unwrap();
        assert!(req.is_empty());
    }

    #[test]
    fn test_update_request_multiple_fields() {
        let raw = json!({
            "notification_enabled": false,
            "command_queue_enabled": true
        });
        let req: UpdateSettingsRequest = serde_json::from_value(raw).unwrap();
        assert!(req.language.is_none());
        assert_eq!(req.notification_enabled, Some(false));
        assert_eq!(req.command_queue_enabled, Some(true));
        assert!(!req.is_empty());
    }

    #[test]
    fn test_update_request_camel_case_ignored() {
        // camelCase keys are treated as unknown fields and silently ignored
        let raw = r#"{"notificationEnabled":true}"#;
        let req: UpdateSettingsRequest = serde_json::from_str(raw).unwrap();
        assert!(req.notification_enabled.is_none());
        assert!(req.is_empty());
    }

    #[test]
    fn test_update_request_unknown_field_ignored() {
        let raw = r#"{"unknownField":123}"#;
        let req: UpdateSettingsRequest = serde_json::from_str(raw).unwrap();
        assert!(req.is_empty());
    }

    // -- ClientPreferencesResponse / UpdateClientPreferencesRequest --

    #[test]
    fn test_client_preferences_response_empty() {
        let resp: ClientPreferencesResponse = HashMap::new();
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json, json!({}));
    }

    #[test]
    fn test_client_preferences_response_mixed_types() {
        let mut resp: ClientPreferencesResponse = HashMap::new();
        resp.insert("system.closeToTray".into(), json!(false));
        resp.insert("ui.fontSize.chat".into(), json!(14));
        resp.insert("theme.activeId".into(), json!("dark"));
        resp.insert("ui.zoomFactor".into(), json!(1.0));

        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["system.closeToTray"], false);
        assert_eq!(json["ui.fontSize.chat"], 14);
        assert_eq!(json["theme.activeId"], "dark");
        assert_eq!(json["ui.zoomFactor"], 1.0);
    }

    #[test]
    fn test_update_client_preferences_with_null_delete() {
        let raw = json!({
            "theme.activeId": null,
            "ui.zoomFactor": 1.25
        });
        let req: UpdateClientPreferencesRequest = serde_json::from_value(raw).unwrap();
        assert!(req["theme.activeId"].is_null());
        assert_eq!(req["ui.zoomFactor"], 1.25);
    }
}
