//! HTTP contract types for `/api/assistants/*`.
//!
//! Mirror of `src/common/types/assistantTypes.ts` on the frontend; any
//! shape change must land in the same PR on both sides.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use tjuaeui_common::AgentType;

use crate::{AgentManagementStatus, AgentSource};

// ---------------------------------------------------------------------------
// Response + source enum
// ---------------------------------------------------------------------------

/// Runtime ownership of a locally installed assistant.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AssistantSource {
    Generated,
    User,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantEngineDescriptor {
    #[serde(rename = "type")]
    pub r#type: AgentType,
    pub ownership: AgentSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acp_backend: Option<String>,
}

/// Wire shape returned by `GET /api/assistants` (single element) and
/// by the single-resource CRUD handlers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantResponse {
    pub id: String,
    pub source: AssistantSource,
    pub name: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub name_i18n: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub description_i18n: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    pub enabled: bool,
    pub sort_order: i32,
    pub engine_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<AssistantEngineDescriptor>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enabled_skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custom_skill_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub context_i18n: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompts: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub prompts_i18n: HashMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<i64>,
    pub engine_status: AgentManagementStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_status_message: Option<String>,
    pub team_selectable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_block_reason: Option<String>,
    pub deletable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantProfileResponse {
    pub name: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub name_i18n: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub description_i18n: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantStateResponse {
    pub enabled: bool,
    pub sort_order: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantEngineResponse {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub descriptor: Option<AssistantEngineDescriptor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantRulesResponse {
    pub content: String,
    pub storage_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantPromptsResponse {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recommended: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub recommended_i18n: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantDefaultScalarResponse {
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantDefaultListResponse {
    pub mode: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub value: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantDefaultsResponse {
    pub model: AssistantDefaultScalarResponse,
    pub permission: AssistantDefaultScalarResponse,
    pub thought_level: AssistantDefaultScalarResponse,
    pub skills: AssistantDefaultListResponse,
    pub mcps: AssistantDefaultListResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantCapabilitiesResponse {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub default_skill_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custom_skill_names: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantPreferencesResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_permission_value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_thought_level_value: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub last_skill_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub last_mcp_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantDetailResponse {
    pub id: String,
    pub source: AssistantSource,
    pub engine_status: AgentManagementStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_status_message: Option<String>,
    pub team_selectable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_block_reason: Option<String>,
    pub deletable: bool,
    pub profile: AssistantProfileResponse,
    pub state: AssistantStateResponse,
    pub engine: AssistantEngineResponse,
    pub rules: AssistantRulesResponse,
    pub prompts: AssistantPromptsResponse,
    pub defaults: AssistantDefaultsResponse,
    pub capabilities: AssistantCapabilitiesResponse,
    pub preferences: AssistantPreferencesResponse,
}

pub fn assistant_avatar_response_value(
    avatar_type: &str,
    avatar_value: Option<&str>,
    assistant_id: &str,
) -> Option<String> {
    if avatar_type == "user_asset" {
        return Some(format!("/api/assistants/{assistant_id}/avatar"));
    }

    let value = avatar_value.map(str::trim).filter(|value| !value.is_empty())?;

    match avatar_type {
        _ if is_unsupported_direct_avatar_value(value) => None,
        _ if is_local_avatar_value(value) => None,
        _ => Some(value.to_owned()),
    }
}

pub fn assistant_avatar_response_value_with_version(
    avatar_type: &str,
    avatar_value: Option<&str>,
    assistant_id: &str,
    version: i64,
) -> Option<String> {
    if avatar_type == "user_asset" {
        return Some(format!("/api/assistants/{assistant_id}/avatar?v={version}"));
    }

    assistant_avatar_response_value(avatar_type, avatar_value, assistant_id)
}

pub fn is_local_avatar_value(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return false;
    }
    if value.starts_with("file://") {
        return true;
    }
    if value.starts_with("/api/") || value.starts_with("/assets/") {
        return false;
    }
    if value.starts_with("//") || value.contains("://") || value.starts_with("data:") {
        return false;
    }
    if value.as_bytes().get(1) == Some(&b':') && matches!(value.as_bytes().first(), Some(b'A'..=b'Z' | b'a'..=b'z')) {
        return true;
    }
    std::path::Path::new(value).is_absolute()
}

fn is_unsupported_direct_avatar_value(value: &str) -> bool {
    let value = value.trim().to_ascii_lowercase();
    value.starts_with("http://") || value.starts_with("https://") || value.starts_with("data:")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assistant_source_serializes_lowercase() {
        let json = serde_json::to_string(&AssistantSource::Generated).unwrap();
        assert_eq!(json, "\"generated\"");
        let json = serde_json::to_string(&AssistantSource::User).unwrap();
        assert_eq!(json, "\"user\"");
    }

    #[test]
    fn assistant_source_rejects_legacy_bare_value() {
        let parsed = serde_json::from_str::<AssistantSource>("\"bare\"");
        assert!(parsed.is_err());
    }

    #[test]
    fn assistant_avatar_response_value_routes_asset_values_through_backend() {
        assert_eq!(
            assistant_avatar_response_value("user_asset", Some("data:image/svg+xml;base64,abc"), "custom-1").as_deref(),
            Some("/api/assistants/custom-1/avatar")
        );
        assert_eq!(
            assistant_avatar_response_value("user_asset", None, "custom-1").as_deref(),
            Some("/api/assistants/custom-1/avatar")
        );
        assert_eq!(
            assistant_avatar_response_value("user_asset", Some("https://example.invalid/avatar.png"), "custom-1")
                .as_deref(),
            Some("/api/assistants/custom-1/avatar")
        );
    }

    #[test]
    fn assistant_avatar_response_value_with_version_routes_asset_values_through_backend() {
        assert_eq!(
            assistant_avatar_response_value_with_version("user_asset", Some("custom-1.png"), "custom-1", 1782714544060)
                .as_deref(),
            Some("/api/assistants/custom-1/avatar?v=1782714544060")
        );
        assert_eq!(
            assistant_avatar_response_value_with_version("emoji", Some("🧠"), "custom-1", 1782714544060).as_deref(),
            Some("🧠")
        );
    }

    #[test]
    fn assistant_avatar_response_value_never_exposes_local_paths() {
        assert_eq!(
            assistant_avatar_response_value(
                "user_asset",
                Some("/Users/veryliu/.tjuaeui/assistant-avatars/custom-1.jpg"),
                "custom-1",
            )
            .as_deref(),
            Some("/api/assistants/custom-1/avatar")
        );
        assert_eq!(
            assistant_avatar_response_value(
                "emoji",
                Some("file:///Users/veryliu/.tjuaeui/assistant-avatars/custom-1.jpg"),
                "custom-1",
            ),
            None
        );
        assert_eq!(
            assistant_avatar_response_value("emoji", Some("https://example.invalid/avatar.png"), "custom-1"),
            None
        );
    }

    #[test]
    fn assistant_response_round_trip_snake_case() {
        let resp = AssistantResponse {
            id: "a1".into(),
            source: AssistantSource::User,
            name: "Name".into(),
            name_i18n: HashMap::new(),
            description: None,
            description_i18n: HashMap::new(),
            avatar: None,
            enabled: true,
            sort_order: 5,
            engine_id: "agent-gemini".into(),
            engine: Some(AssistantEngineDescriptor {
                r#type: AgentType::Acp,
                ownership: AgentSource::Builtin,
                acp_backend: Some("gemini".into()),
            }),
            enabled_skills: vec![],
            custom_skill_names: vec![],
            context: None,
            context_i18n: HashMap::new(),
            prompts: vec![],
            prompts_i18n: HashMap::new(),
            models: vec![],
            last_used_at: Some(1_234),
            engine_status: AgentManagementStatus::Online,
            engine_status_message: None,
            team_selectable: true,
            team_block_reason: None,
            deletable: true,
        };

        let json = serde_json::to_value(&resp).unwrap();
        assert!(json.get("preset_agent_type").is_none());
        assert_eq!(json["engine_id"], "agent-gemini");
        assert!(json["engine"].get("id").is_none());
        assert!(json["engine"].get("backend").is_none());
        assert_eq!(json["engine"]["acp_backend"], "gemini");
        assert_eq!(json["sort_order"], 5);
        assert_eq!(json["last_used_at"], 1234);
    }

    #[test]
    fn assistant_response_rejects_camel_case() {
        // Body has BOTH snake_case (valid required values) AND camelCase aliases.
        // Prove: snake is consumed; camel is silently ignored (NOT aliased over snake).
        let json = serde_json::json!({
            "id": "a1",
            "source": "user",
            "name": "X",
            "enabled": true,
            "sort_order": 7,                   // snake required field
            "engine_id": "agent-gemini",       // snake required field
            "engine_status": "online",          // snake required field
            "team_selectable": true,           // snake required field
            "deletable": true,                 // snake required field
            "engineId": "agent-claude",        // camel — must be ignored
            "sortOrder": 99,                   // legacy camel — must be ignored
            "lastUsedAt": 111_222,             // legacy camel for optional field — must be ignored
        });
        let resp: AssistantResponse = serde_json::from_value(json).unwrap();
        // If camel were aliased, these would be the camel values.
        assert_eq!(resp.engine_id, "agent-gemini", "snake_case engine_id must win");
        assert_eq!(resp.sort_order, 7, "snake_case sort_order must win");
        assert!(
            resp.last_used_at.is_none(),
            "camelCase lastUsedAt must NOT alias into last_used_at"
        );
    }
}
