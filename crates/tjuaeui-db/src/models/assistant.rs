//! Row models and repository parameter structs for the assistants domain.

use serde::{Deserialize, Serialize};
use tjuaeui_common::TimestampMs;

/// Row mapping for the `assistant_definitions` table.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct AssistantDefinitionRow {
    pub id: String,
    pub assistant_id: String,
    pub source: String,
    pub owner_type: String,
    pub source_ref: Option<String>,
    pub name: String,
    pub name_i18n: String,
    pub description: Option<String>,
    pub description_i18n: String,
    pub avatar_type: String,
    pub avatar_value: Option<String>,
    pub agent_id: String,
    pub rule_resource_type: String,
    pub rule_resource_ref: Option<String>,
    pub recommended_prompts: String,
    pub recommended_prompts_i18n: String,
    pub default_model_mode: String,
    pub default_model_value: Option<String>,
    pub default_permission_mode: String,
    pub default_permission_value: Option<String>,
    pub default_thought_level_mode: String,
    pub default_thought_level_value: Option<String>,
    pub default_skills_mode: String,
    pub default_skill_ids: String,
    pub custom_skill_names: String,
    pub default_mcps_mode: String,
    pub default_mcp_ids: String,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
    pub deleted_at: Option<TimestampMs>,
}

/// Row mapping for the `assistant_overlays` table.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct AssistantOverlayRow {
    pub assistant_definition_id: String,
    pub enabled: bool,
    pub sort_order: i32,
    pub agent_id_override: Option<String>,
    pub last_used_at: Option<TimestampMs>,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
}

/// Row mapping for the `assistant_preferences` table.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct AssistantPreferenceRow {
    pub assistant_definition_id: String,
    pub last_model_id: Option<String>,
    pub last_permission_value: Option<String>,
    pub last_thought_level_value: Option<String>,
    pub last_skill_ids: String,
    pub last_mcp_ids: String,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
}

/// Insert-or-update parameters for `assistant_definitions`.
#[derive(Debug, Clone)]
pub struct UpsertAssistantDefinitionParams<'a> {
    pub id: &'a str,
    pub assistant_id: &'a str,
    pub source: &'a str,
    pub owner_type: &'a str,
    pub source_ref: Option<&'a str>,
    pub name: &'a str,
    pub name_i18n: &'a str,
    pub description: Option<&'a str>,
    pub description_i18n: &'a str,
    pub avatar_type: &'a str,
    pub avatar_value: Option<&'a str>,
    pub agent_id: &'a str,
    pub rule_resource_type: &'a str,
    pub rule_resource_ref: Option<&'a str>,
    pub recommended_prompts: &'a str,
    pub recommended_prompts_i18n: &'a str,
    pub default_model_mode: &'a str,
    pub default_model_value: Option<&'a str>,
    pub default_permission_mode: &'a str,
    pub default_permission_value: Option<&'a str>,
    pub default_thought_level_mode: &'a str,
    pub default_thought_level_value: Option<&'a str>,
    pub default_skills_mode: &'a str,
    pub default_skill_ids: &'a str,
    pub custom_skill_names: &'a str,
    pub default_mcps_mode: &'a str,
    pub default_mcp_ids: &'a str,
}

/// Insert-or-update parameters for `assistant_overlays`.
#[derive(Debug, Clone)]
pub struct UpsertAssistantOverlayParams<'a> {
    pub assistant_definition_id: &'a str,
    pub enabled: bool,
    pub sort_order: i32,
    pub agent_id_override: Option<&'a str>,
    pub last_used_at: Option<TimestampMs>,
}

/// Insert-or-update parameters for `assistant_preferences`.
#[derive(Debug, Clone)]
pub struct UpsertAssistantPreferenceParams<'a> {
    pub assistant_definition_id: &'a str,
    pub last_model_id: Option<&'a str>,
    pub last_permission_value: Option<&'a str>,
    pub last_thought_level_value: Option<&'a str>,
    pub last_skill_ids: &'a str,
    pub last_mcp_ids: &'a str,
}
