use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum SkillSourceResponse {
    #[serde(rename = "mine")]
    Mine,
    #[serde(rename = "tjuae-hub")]
    TjuaeHub,
    #[serde(rename = "skillhub")]
    SkillHub,
    #[serde(rename = "clawhub")]
    ClawHub,
}

impl SkillSourceResponse {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mine => "mine",
            Self::TjuaeHub => "tjuae-hub",
            Self::SkillHub => "skillhub",
            Self::ClawHub => "clawhub",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "mine" => Some(Self::Mine),
            "tjuae-hub" => Some(Self::TjuaeHub),
            "skillhub" => Some(Self::SkillHub),
            "clawhub" => Some(Self::ClawHub),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillIdentityResponse {
    pub source: SkillSourceResponse,
    #[serde(default)]
    pub namespace: String,
    pub slug: String,
}

impl SkillIdentityResponse {
    /// Stable identity stored by assistants. Skill slugs alone are not unique
    /// once more than one Hub is enabled.
    pub fn reference(&self) -> String {
        format!("{}:{}:{}", self.source.as_str(), self.namespace, self.slug)
    }

    pub fn parse_reference(value: &str) -> Option<Self> {
        let mut parts = value.splitn(3, ':');
        let source = SkillSourceResponse::parse(parts.next()?)?;
        let namespace = parts.next()?.to_owned();
        let slug = parts.next()?.to_owned();
        (!slug.is_empty()).then_some(Self {
            source,
            namespace,
            slug,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillPreferencesResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_version: Option<String>,
    pub follow_latest: bool,
    pub enabled: bool,
    pub auto_inject: bool,
}

impl Default for SkillPreferencesResponse {
    fn default() -> Self {
        Self {
            selected_version: None,
            follow_latest: true,
            enabled: false,
            auto_inject: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillVersionResponse {
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillFileResponse {
    pub path: String,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillCatalogItemResponse {
    pub identity: SkillIdentityResponse,
    pub name: String,
    pub description: String,
    pub latest_version: String,
    pub categories: Vec<String>,
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub preferences: SkillPreferencesResponse,
    pub editable: bool,
    pub can_copy_to_mine: bool,
    pub can_publish_to_tjuae_hub: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillCatalogPageResponse {
    pub items: Vec<SkillCatalogItemResponse>,
    pub total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillCatalogDetailResponse {
    pub skill: SkillCatalogItemResponse,
    pub selected_version: String,
    pub versions: Vec<SkillVersionResponse>,
    pub files: Vec<SkillFileResponse>,
    pub readme: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillCatalogQuery {
    #[serde(default)]
    pub q: String,
    #[serde(default)]
    pub sources: String,
    #[serde(default)]
    pub categories: String,
    #[serde(default)]
    pub tags: String,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub auto_inject: Option<bool>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillVersionQuery {
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillCatalogFileQuery {
    pub path: String,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillCatalogFileContentResponse {
    pub path: String,
    pub content: String,
    pub size: u64,
    pub editable: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompareSkillVersionsQuery {
    pub base: String,
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillVersionFileDiffResponse {
    pub path: String,
    pub status: String,
    pub binary: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillVersionComparisonResponse {
    pub identity: SkillIdentityResponse,
    pub base_version: String,
    pub target_version: String,
    pub files: Vec<SkillVersionFileDiffResponse>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateSkillPreferencesRequest {
    #[serde(default)]
    pub selected_version: Option<String>,
    pub follow_latest: bool,
    pub enabled: bool,
    pub auto_inject: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CopySkillRequest {
    pub version: String,
    pub target_slug: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportSkillRequest {
    pub archive_path: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportSkillRequest {
    pub version: String,
    pub output_path: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveSkillFileRequest {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateSkillProfileRequest {
    pub name: String,
    pub description: String,
    pub categories: Vec<String>,
    pub tags: Vec<String>,
    #[serde(default)]
    pub icon_data_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateSkillRequest {
    pub slug: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillOperationResponse {
    pub identity: SkillIdentityResponse,
    pub version: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_reference_round_trips_all_identity_parts() {
        let identity = SkillIdentityResponse {
            source: SkillSourceResponse::TjuaeHub,
            namespace: "official".into(),
            slug: "skill-creator".into(),
        };
        assert_eq!(identity.reference(), "tjuae-hub:official:skill-creator");
        assert_eq!(
            SkillIdentityResponse::parse_reference(&identity.reference()),
            Some(identity)
        );
        assert!(SkillIdentityResponse::parse_reference("skill-creator").is_none());
    }
    use serde_json::json;

    #[test]
    fn preference_contract_is_provider_scoped_and_has_no_package_provenance() {
        let value = serde_json::to_value(SkillCatalogItemResponse {
            identity: SkillIdentityResponse {
                source: SkillSourceResponse::SkillHub,
                namespace: "alice".into(),
                slug: "writer".into(),
            },
            name: "Writer".into(),
            description: "Write".into(),
            latest_version: "1.2.0".into(),
            categories: vec![],
            tags: vec![],
            icon_url: None,
            author: None,
            preferences: SkillPreferencesResponse {
                selected_version: Some("1.1.0".into()),
                follow_latest: false,
                enabled: true,
                auto_inject: true,
            },
            editable: false,
            can_copy_to_mine: true,
            can_publish_to_tjuae_hub: false,
        })
        .unwrap();
        assert_eq!(
            value["identity"],
            json!({"source":"skillhub","namespace":"alice","slug":"writer"})
        );
        assert!(value.get("installed").is_none());
        assert!(value.get("syncState").is_none());
    }

    #[test]
    fn comparison_has_one_identity_and_two_versions() {
        let query: CompareSkillVersionsQuery = serde_json::from_value(json!({
            "base": "1.0.0", "target": "1.1.0"
        }))
        .unwrap();
        assert_eq!(query.base, "1.0.0");
        assert!(
            serde_json::from_value::<CompareSkillVersionsQuery>(json!({
                "leftSource": "skillhub", "base": "1.0.0", "target": "1.1.0"
            }))
            .is_err()
        );
    }
}
