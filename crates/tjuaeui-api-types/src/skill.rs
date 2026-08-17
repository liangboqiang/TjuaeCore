use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillPreferencesResponse {
    pub enabled: bool,
    pub auto_inject: bool,
}

/// Provenance is a link, not a second class of skill. Every installed skill is
/// still one ordinary local workspace and uses the same editor/Git/runtime path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum SkillSourceResponse {
    Local,
    Market {
        #[serde(rename = "marketId")]
        market_id: String,
        repository: String,
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        revision: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillWorkspaceResponse {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub path: String,
    pub source: SkillSourceResponse,
    pub categories: Vec<String>,
    pub preferences: SkillPreferencesResponse,
    pub git_status: SkillGitStatusResponse,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SkillGitStatusResponse {
    Clean,
    Modified,
    Conflicted,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarketInfoResponse {
    pub id: String,
    pub name: String,
    pub repository: String,
    pub revision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarketSkillResponse {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub path: String,
    pub digest: String,
    pub categories: Vec<String>,
    pub market: MarketInfoResponse,
    pub installed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_version: Option<String>,
    pub sync_state: MarketSyncStateResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum MarketSyncStateResponse {
    NotInstalled,
    Synced,
    LocalChanged,
    UpdateAvailable,
    Diverged,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarketFileComparisonResponse {
    pub path: String,
    pub status: String,
    pub binary: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarketSkillComparisonResponse {
    pub slug: String,
    pub base_revision: String,
    pub remote_revision: String,
    pub sync_state: MarketSyncStateResponse,
    pub files: Vec<MarketFileComparisonResponse>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublishMarketSkillRequest {
    pub fork_repository_url: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublishMarketSkillResponse {
    pub branch: String,
    pub commit: String,
    pub compare_url: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateSkillPreferencesRequest {
    pub enabled: bool,
    pub auto_inject: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CopySkillRequest {
    pub target_slug: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ImportSkillRequest {
    pub skill_path: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateSkillRequest {
    pub slug: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CloneSkillRequest {
    pub repository_url: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReadAssistantRuleRequest {
    pub assistant_id: String,
    #[serde(default)]
    pub locale: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WriteAssistantRuleRequest {
    pub assistant_id: String,
    pub content: String,
    #[serde(default)]
    pub locale: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MaterializeSkillsRequest {
    pub conversation_id: String,
    #[serde(default)]
    pub skills: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MaterializedSkillRef {
    pub name: String,
    pub source_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MaterializeSkillsResponse {
    pub skills: Vec<MaterializedSkillRef>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn skill_contract_has_one_source_and_two_preferences() {
        let value = serde_json::to_value(SkillWorkspaceResponse {
            id: "cron".into(),
            slug: "cron".into(),
            name: "cron".into(),
            description: "test".into(),
            version: "1.0.0".into(),
            path: "/tmp/cron".into(),
            source: SkillSourceResponse::Local,
            categories: vec![],
            preferences: SkillPreferencesResponse {
                enabled: true,
                auto_inject: false,
            },
            git_status: SkillGitStatusResponse::Clean,
        })
        .unwrap();
        assert_eq!(value["source"]["kind"], "local");
        assert_eq!(value["preferences"], json!({"enabled": true, "autoInject": false}));
        assert_eq!(value["gitStatus"], "clean");
        assert!(value.get("origin").is_none());
    }

    #[test]
    fn materialize_request_has_one_current_spelling() {
        let current: MaterializeSkillsRequest = serde_json::from_value(json!({
            "conversation_id": "conv-1",
            "skills": ["cron"]
        }))
        .unwrap();
        assert_eq!(current.skills, vec!["cron"]);
        assert!(
            serde_json::from_value::<MaterializeSkillsRequest>(json!({
                "conversation_id": "conv-1",
                "enabled_skills": ["cron"]
            }))
            .is_err()
        );
    }

    #[test]
    fn import_request_rejects_old_camel_case_field() {
        assert!(serde_json::from_value::<ImportSkillRequest>(json!({"skillPath": "/tmp/skill"})).is_err());
        assert_eq!(
            serde_json::from_value::<ImportSkillRequest>(json!({"skill_path": "/tmp/skill"}))
                .unwrap()
                .skill_path,
            "/tmp/skill"
        );
    }
}
