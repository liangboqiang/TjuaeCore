use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Request body for detecting an ACP CLI executable.
///
/// `backend` is a vendor label (e.g. "claude"). The service resolves it
/// against the `agent_metadata` catalog.
#[derive(Debug, Deserialize)]
pub struct DetectCliRequest {
    pub backend: String,
}

/// Response for CLI detection.
#[derive(Debug, Serialize)]
pub struct DetectCliResponse {
    /// Path to the detected CLI, `None` if not found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Response for ACP environment variables.
#[derive(Debug, Serialize)]
pub struct AcpEnvResponse {
    pub env: HashMap<String, String>,
}

/// Response for agent session mode.
#[derive(Debug, Serialize)]
pub struct AgentModeResponse {
    pub mode: String,
    pub initialized: bool,
}

/// Request body for setting session mode.
#[derive(Debug, Deserialize)]
pub struct SetModeRequest {
    pub mode: String,
}

/// Request body for setting ACP session model.
#[derive(Debug, Deserialize)]
pub struct SetModelRequest {
    pub model_id: String,
}

/// A single available model entry in the frontend-facing model info response.
#[derive(Debug, Clone, Serialize)]
pub struct ModelInfoEntry {
    pub id: String,
    pub label: String,
}

/// Frontend-compatible model info response.
///
/// Maps from the SDK's camelCase `SessionModelState` to the snake_case
/// `AcpModelInfo` format the renderer expects.
#[derive(Debug, Serialize)]
pub struct GetModelInfoResponse {
    pub model_info: Option<ModelInfoPayload>,
}

/// A single select option inside an ACP config option.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcpConfigSelectOptionDto {
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Frontend-facing ACP config option. Always serializes with snake_case field names.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcpConfigOptionDto {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(rename = "type")]
    pub option_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_value: Option<String>,
    #[serde(default)]
    pub options: Vec<AcpConfigSelectOptionDto>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfigOptionConfirmation {
    Observed,
    CommandAck,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct SetConfigOptionRequest {
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GetConfigOptionsResponse {
    pub config_options: Vec<AcpConfigOptionDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SetConfigOptionResponse {
    pub confirmation: ConfigOptionConfirmation,
    pub config_options: Option<Vec<AcpConfigOptionDto>>,
}

/// Inner model info payload matching the frontend's `AcpModelInfo` type.
#[derive(Debug, Clone, Serialize)]
pub struct ModelInfoPayload {
    pub current_model_id: Option<String>,
    pub current_model_label: Option<String>,
    pub available_models: Vec<ModelInfoEntry>,
}

/// Request body for probing model information.
#[derive(Debug, Deserialize)]
pub struct ProbeModelRequest {
    pub backend: String,
}

/// Engine Adapter 试跑的结构化结果。
///
/// Tagged enum: `step` distinguishes the states the frontend's Alert component
/// renders (success → green, fail_cli → red, fail_acp → yellow, fail_auth →
/// yellow with a "needs login" hint). `error` carries a human-readable reason
/// for the failure variants.
///
/// The probe reaches `session/new` (not just `initialize`), so `fail_auth`
/// distinguishes "reachable but not authorized" (ACP `auth_required`,
/// JSON-RPC `-32000`) from other ACP failures — `initialize` alone cannot
/// tell these apart.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum EngineAdapterProbeResponse {
    Success,
    FailCli { error: String },
    FailAcp { error: String },
    FailAuth { error: String },
}

/// Query parameters for workspace browse.
#[derive(Debug, Deserialize)]
pub struct WorkspaceBrowseQuery {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
}

/// A file or directory entry in the workspace browse response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceEntry {
    pub name: String,
    #[serde(rename = "type")]
    pub entry_type: String,
}

/// Request body for side question.
#[derive(Debug, Deserialize)]
pub struct SideQuestionRequest {
    pub question: String,
}

/// Response for side question.
#[derive(Debug, Serialize)]
pub struct SideQuestionResponse {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detect_cli_request_serde() {
        let json = json!({ "backend": "claude" });
        let req: DetectCliRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.backend, "claude");
    }

    #[test]
    fn detect_cli_response_with_path() {
        let resp = DetectCliResponse {
            path: Some("/usr/local/bin/claude".into()),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["path"], "/usr/local/bin/claude");
    }

    #[test]
    fn detect_cli_response_without_path() {
        let resp = DetectCliResponse { path: None };
        let json = serde_json::to_value(&resp).unwrap();
        assert!(json.get("path").is_none());
    }

    #[test]
    fn set_mode_request_serde() {
        let json = json!({ "mode": "code" });
        let req: SetModeRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.mode, "code");
    }

    #[test]
    fn set_model_request_serde() {
        let json = json!({ "model_id": "claude-sonnet-4" });
        let req: SetModelRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.model_id, "claude-sonnet-4");
    }

    #[test]
    fn config_options_response_serializes_snake_case() {
        let resp = GetConfigOptionsResponse {
            config_options: vec![AcpConfigOptionDto {
                id: "reasoning_effort".to_owned(),
                name: Some("Reasoning Effort".to_owned()),
                label: None,
                description: None,
                category: Some("thought_level".to_owned()),
                option_type: "select".to_owned(),
                current_value: Some("high".to_owned()),
                options: vec![AcpConfigSelectOptionDto {
                    value: "high".to_owned(),
                    name: Some("High".to_owned()),
                    label: None,
                    description: None,
                }],
            }],
        };

        let value = serde_json::to_value(resp).unwrap();
        assert_eq!(value["config_options"][0]["current_value"], "high");
        assert_eq!(value["config_options"][0]["type"], "select");
        assert!(value["config_options"][0].get("currentValue").is_none());
    }

    #[test]
    fn set_config_option_response_serializes_command_ack_without_snapshot() {
        let resp = SetConfigOptionResponse {
            confirmation: ConfigOptionConfirmation::CommandAck,
            config_options: None,
        };

        let value = serde_json::to_value(resp).unwrap();
        assert_eq!(value["confirmation"], "command_ack");
        assert!(value["config_options"].is_null());
    }

    #[test]
    fn engine_adapter_probe_response_tag_serializes() {
        use super::EngineAdapterProbeResponse;
        let ok = EngineAdapterProbeResponse::Success;
        assert_eq!(
            serde_json::to_value(&ok).unwrap(),
            serde_json::json!({"step":"success"})
        );

        let fail = EngineAdapterProbeResponse::FailCli {
            error: "not found".into(),
        };
        assert_eq!(
            serde_json::to_value(&fail).unwrap(),
            serde_json::json!({"step":"fail_cli","error":"not found"})
        );

        // Reachable-but-unauthorized is its own tag so the UI can show a
        // "needs login" hint instead of a generic ACP failure.
        let auth = EngineAdapterProbeResponse::FailAuth {
            error: "requires login".into(),
        };
        assert_eq!(
            serde_json::to_value(&auth).unwrap(),
            serde_json::json!({"step":"fail_auth","error":"requires login"})
        );
    }

    #[test]
    fn env_response_serde() {
        let resp = AcpEnvResponse {
            env: HashMap::from([("PATH".into(), "/usr/bin".into()), ("HOME".into(), "/home/user".into())]),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["env"]["PATH"], "/usr/bin");
    }

    #[test]
    fn probe_model_request_serde() {
        let json = json!({ "backend": "claude" });
        let req: ProbeModelRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.backend, "claude");
    }
}
