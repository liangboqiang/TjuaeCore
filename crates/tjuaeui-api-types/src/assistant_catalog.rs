//! 版本化助手目录、用户偏好和显式依赖激活的 HTTP 契约。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum AssistantSourceResponse {
    Mine,
    TjuaeHub,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct AssistantIdentityResponse {
    pub source: AssistantSourceResponse,
    pub namespace: String,
    pub slug: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AssistantCatalogQuery {
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub sort: String,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AssistantVersionQuery {
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantCatalogFileQuery {
    pub path: String,
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AssistantPreferencesCatalogResponse {
    pub selected_version: Option<String>,
    pub follow_latest: bool,
    pub enabled: bool,
    pub activation_status: String,
    pub sort_order: i32,
    pub last_used_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantCatalogItemResponse {
    pub identity: AssistantIdentityResponse,
    pub name: String,
    pub description: String,
    pub avatar_url: Option<String>,
    pub latest_version: String,
    pub categories: Vec<String>,
    pub editable: bool,
    pub system: bool,
    pub can_disable: bool,
    pub can_delete: bool,
    pub preferences: AssistantPreferencesCatalogResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantCatalogPageResponse {
    pub items: Vec<AssistantCatalogItemResponse>,
    pub total: u64,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AssistantDefaultRef {
    pub source: String,
    pub namespace: String,
    pub slug: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AssistantDefaultScalar {
    pub mode: String,
    pub value: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AssistantDefaultsCatalogResponse {
    pub agent: Option<String>,
    pub model: AssistantDefaultScalar,
    pub permission: AssistantDefaultScalar,
    pub thought_level: AssistantDefaultScalar,
    pub skills: Vec<AssistantDefaultRef>,
    pub mcps: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AssistantRequirementKind {
    Skill,
    Mcp,
    Model,
    Agent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantRequirementResponse {
    pub key: String,
    pub kind: AssistantRequirementKind,
    pub required: bool,
    pub label: String,
    pub identity: Option<AssistantDefaultRef>,
    pub preferred_ids: Vec<String>,
    pub version_requirement: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantManifestResponse {
    pub format: String,
    pub format_version: u32,
    pub id: String,
    pub version: String,
    pub name: String,
    pub name_i18n: BTreeMap<String, String>,
    pub description: String,
    pub description_i18n: BTreeMap<String, String>,
    pub categories: Vec<String>,
    pub avatar: Option<String>,
    pub defaults: AssistantDefaultsCatalogResponse,
    pub requirements: Vec<AssistantRequirementResponse>,
    pub recommended_prompts: Vec<String>,
    pub recommended_prompts_i18n: BTreeMap<String, Vec<String>>,
    pub content_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantVersionResponse {
    pub version: String,
    pub revision: String,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantCatalogFileResponse {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantCatalogDetailResponse {
    pub item: AssistantCatalogItemResponse,
    pub manifest: AssistantManifestResponse,
    pub readme: String,
    pub files: Vec<AssistantCatalogFileResponse>,
    pub versions: Vec<AssistantVersionResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantCatalogFileContentResponse {
    pub path: String,
    pub content: String,
    pub size: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveAssistantCatalogFileRequest {
    pub path: String,
    pub content: String,
}

/// Structured assistant settings update. Fields outside this DTO remain
/// intact in the canonical package manifest.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateAssistantCatalogSettingsRequest {
    pub name: String,
    pub description: String,
    pub avatar: Option<String>,
    pub avatar_data_url: Option<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    pub defaults: AssistantDefaultsCatalogResponse,
    pub recommended_prompts: Vec<String>,
    pub rules: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublishAssistantCatalogRequest {
    pub version: String,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishAssistantCatalogResponse {
    pub commit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantVersionFileDiffResponse {
    pub path: String,
    pub status: String,
    pub binary: bool,
    pub base_content: Option<String>,
    pub target_content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantVersionComparisonResponse {
    pub base_version: String,
    pub target_version: String,
    pub files: Vec<AssistantVersionFileDiffResponse>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAssistantCatalogPreferencesRequest {
    pub selected_version: Option<String>,
    pub follow_latest: bool,
    pub enabled: bool,
    pub sort_order: Option<i32>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareAssistantRequest {
    pub version: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AssistantActivationStatus {
    Ready,
    Disabled,
    Missing,
    VersionConflict,
    Ambiguous,
    Incompatible,
    ConfigurationRequired,
    SecretRequired,
    Unavailable,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AssistantActivationAction {
    Keep,
    Enable,
    Import,
    Configure,
    UseDefault,
    Select,
    Skip,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantActivationCandidateResponse {
    pub id: String,
    pub label: String,
    pub version: Option<String>,
    pub enabled: bool,
    pub available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantActivationItemResponse {
    pub requirement_key: String,
    pub label: String,
    pub required: bool,
    pub status: AssistantActivationStatus,
    pub message: String,
    pub allowed_actions: Vec<AssistantActivationAction>,
    pub candidates: Vec<AssistantActivationCandidateResponse>,
    pub current_resource_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantActivationGroupResponse {
    pub kind: AssistantRequirementKind,
    pub items: Vec<AssistantActivationItemResponse>,
    pub requires_confirmation: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantActivationPlanResponse {
    pub plan_id: String,
    pub fingerprint: String,
    pub identity: AssistantIdentityResponse,
    pub version: String,
    pub groups: Vec<AssistantActivationGroupResponse>,
    pub ready_without_changes: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantActivationChoice {
    pub requirement_key: String,
    pub action: AssistantActivationAction,
    pub resource_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivateAssistantRequest {
    pub plan_id: String,
    pub fingerprint: String,
    pub confirmed_groups: Vec<AssistantRequirementKind>,
    pub choices: Vec<AssistantActivationChoice>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantOperationResponse {
    pub identity: AssistantIdentityResponse,
    pub version: String,
    pub enabled: bool,
    pub activation_status: String,
}

/// 会话、团队、定时任务和频道共享的唯一“已激活助手”运行时视图。
///
/// 目录详情负责编辑与版本浏览；运行时选择器只能消费这个经过资源激活校验的
/// 紧凑视图，避免再次读取旧助手表或自行拼装默认值。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantRuntimeOptionResponse {
    pub id: String,
    pub identity: AssistantIdentityResponse,
    pub version: String,
    pub name: String,
    pub name_i18n: BTreeMap<String, String>,
    pub description: String,
    pub description_i18n: BTreeMap<String, String>,
    pub avatar_url: Option<String>,
    pub agent_id: String,
    pub agent: Option<AssistantRuntimeAgentResponse>,
    pub agent_status: String,
    pub team_selectable: bool,
    pub model_ids: Vec<String>,
    pub permission: Option<String>,
    pub thought_level: Option<String>,
    pub skill_ids: Vec<String>,
    pub mcp_ids: Vec<String>,
    pub recommended_prompts: Vec<String>,
    pub recommended_prompts_i18n: BTreeMap<String, Vec<String>>,
    pub sort_order: i32,
    pub last_used_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantRuntimeAgentResponse {
    pub agent_type: String,
    pub source: String,
    pub backend: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAssistantRuntimeOverridesRequest {
    pub model: Option<String>,
    pub permission: Option<String>,
    pub thought_level: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateMineAssistantRequest {
    pub slug: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportAssistantRequest {
    pub archive_path: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyAssistantToMineRequest {
    pub version: Option<String>,
    pub target_slug: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportAssistantRequest {
    pub version: Option<String>,
    pub output_path: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportAssistantResponse {
    pub output_path: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn assistant_publish_requires_an_explicit_version_and_rejects_removed_tag_fields() {
        let request: PublishAssistantCatalogRequest =
            serde_json::from_value(json!({"version":"1.1.0","message":"release"})).unwrap();
        assert_eq!(request.version, "1.1.0");
        assert!(
            serde_json::from_value::<PublishAssistantCatalogRequest>(
                json!({"version":"1.1.0","message":"release","tags":["legacy"]})
            )
            .is_err()
        );
    }
}
