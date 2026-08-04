//! Core 本地助手 Definition 契约。
//!
//! 本地可编辑 Definition 与 Hub 发布 Definition 使用不同的依赖身份：
//! 本地文件只引用当前用户资产仓库中的 `local asset id`，Hub 文件只引用
//! TjuaeHub 的 `remote asset id`。这里故意不接受运行时技能名称，避免在
//! 同名、重命名或多来源场景中进行猜测。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const LOCAL_ASSISTANT_SCHEMA: &str =
    "https://raw.githubusercontent.com/liangboqiang/TjuaeCore/main/schemas/local-assistant-definition.v1.schema.json";
pub const HUB_ASSISTANT_SCHEMA: &str =
    "https://raw.githubusercontent.com/liangboqiang/TjuaeHub/main/schemas/assistant-definition.v1.schema.json";
pub const LOCAL_ASSISTANT_ENTRY_FILE: &str = "assistant.local.json";
pub const LOCAL_ASSET_DESCRIPTOR_FILE: &str = "tjuae.asset.json";

/// 仅在一次运行时投影调用中传递的本机配置。
///
/// 该结构不会序列化进 Definition；AssetCatalog 也不会持久化它。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssistantRuntimeConfiguration {
    pub engine_id: String,
    pub enabled: bool,
    pub sort_order: i32,
    #[serde(default)]
    pub last_used_at: Option<i64>,
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub permission_value: Option<String>,
    #[serde(default)]
    pub thought_level_value: Option<String>,
    #[serde(default)]
    pub mcp_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalAssistantDefinition {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub schema_version: u32,
    pub kind: String,
    pub runtime_id: String,
    pub name: String,
    #[serde(default)]
    pub name_i18n: BTreeMap<String, String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub description_i18n: BTreeMap<String, String>,
    pub rules: BTreeMap<String, String>,
    #[serde(default)]
    pub recommended_prompts: Vec<String>,
    #[serde(default)]
    pub recommended_prompts_i18n: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub skill_dependencies: Vec<LocalSkillDependency>,
    #[serde(default)]
    pub avatar: PortableAssistantAvatar,
}

impl LocalAssistantDefinition {
    pub fn new(runtime_id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            schema: LOCAL_ASSISTANT_SCHEMA.into(),
            schema_version: 1,
            kind: "assistant".into(),
            runtime_id: runtime_id.into(),
            name: name.into(),
            name_i18n: BTreeMap::new(),
            description: None,
            description_i18n: BTreeMap::new(),
            rules: BTreeMap::new(),
            recommended_prompts: Vec::new(),
            recommended_prompts_i18n: BTreeMap::new(),
            skill_dependencies: Vec::new(),
            avatar: PortableAssistantAvatar::None,
        }
    }

    pub fn local_skill_asset_ids(&self) -> impl Iterator<Item = &str> {
        self.skill_dependencies
            .iter()
            .map(|dependency| dependency.asset_id.as_str())
    }

    pub fn to_hub(
        &self,
        remote_skill_asset_ids: BTreeMap<String, String>,
    ) -> Result<HubAssistantDefinition, LocalAssistantDefinitionError> {
        let mut skill_dependencies = Vec::with_capacity(self.skill_dependencies.len());
        for dependency in &self.skill_dependencies {
            let remote_id = remote_skill_asset_ids
                .get(&dependency.asset_id)
                .ok_or_else(|| LocalAssistantDefinitionError::UnpublishableDependency(dependency.asset_id.clone()))?;
            skill_dependencies.push(remote_id.clone());
        }

        let description = self
            .description
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or(LocalAssistantDefinitionError::MissingPublishDescription)?
            .to_owned();
        if self.name_i18n.is_empty() || self.description_i18n.is_empty() {
            return Err(LocalAssistantDefinitionError::MissingPublishLocalization);
        }
        let avatar = match &self.avatar {
            PortableAssistantAvatar::Emoji { value } => HubAssistantAvatar::Emoji { value: value.clone() },
            PortableAssistantAvatar::File { path } => HubAssistantAvatar::File { path: path.clone() },
            PortableAssistantAvatar::None => return Err(LocalAssistantDefinitionError::MissingPublishAvatar),
        };

        Ok(HubAssistantDefinition {
            schema: HUB_ASSISTANT_SCHEMA.into(),
            schema_version: 1,
            kind: "assistant".into(),
            runtime_id: self.runtime_id.clone(),
            name: self.name.clone(),
            name_i18n: self.name_i18n.clone(),
            description,
            description_i18n: self.description_i18n.clone(),
            rules: self.rules.clone(),
            recommended_prompts: self.recommended_prompts.clone(),
            recommended_prompts_i18n: self.recommended_prompts_i18n.clone(),
            skill_dependencies,
            avatar,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalSkillDependency {
    pub source: LocalDependencySource,
    pub asset_id: String,
}

impl LocalSkillDependency {
    pub fn local(asset_id: impl Into<String>) -> Self {
        Self {
            source: LocalDependencySource::Local,
            asset_id: asset_id.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum LocalDependencySource {
    Local,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum PortableAssistantAvatar {
    #[default]
    None,
    Emoji {
        value: String,
    },
    File {
        path: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HubAssistantDefinition {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub schema_version: u32,
    pub kind: String,
    pub runtime_id: String,
    pub name: String,
    #[serde(default)]
    pub name_i18n: BTreeMap<String, String>,
    pub description: String,
    #[serde(default)]
    pub description_i18n: BTreeMap<String, String>,
    pub rules: BTreeMap<String, String>,
    #[serde(default)]
    pub recommended_prompts: Vec<String>,
    #[serde(default)]
    pub recommended_prompts_i18n: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub skill_dependencies: Vec<String>,
    pub avatar: HubAssistantAvatar,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum HubAssistantAvatar {
    Emoji { value: String },
    File { path: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalAssetDescriptor {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub schema_version: u32,
    pub kind: String,
    pub asset_id: String,
    pub contribution_key: String,
    pub contribution: LocalAssistantContribution,
}

impl LocalAssetDescriptor {
    pub fn assistant(asset_id: impl Into<String>, definition: &LocalAssistantDefinition) -> Self {
        Self {
            schema: "tjuae://schemas/local-asset-descriptor.v1".into(),
            schema_version: 1,
            kind: "assistant".into(),
            asset_id: asset_id.into(),
            contribution_key: "assistants".into(),
            contribution: LocalAssistantContribution {
                id: definition.runtime_id.clone(),
                runtime_id: definition.runtime_id.clone(),
                name: definition.name.clone(),
                description: definition.description.clone().unwrap_or_default(),
                file: LOCAL_ASSISTANT_ENTRY_FILE.into(),
                dependencies: definition
                    .skill_dependencies
                    .iter()
                    .map(|dependency| dependency.asset_id.clone())
                    .collect(),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalAssistantContribution {
    pub id: String,
    pub runtime_id: String,
    pub name: String,
    pub description: String,
    pub file: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LocalAssistantDefinitionError {
    #[error("本地技能资产 {0} 尚未跟踪到 TjuaeHub，不能发布助手")]
    UnpublishableDependency(String),
    #[error("发布助手必须填写说明")]
    MissingPublishDescription,
    #[error("发布助手必须提供 nameI18n 和 descriptionI18n")]
    MissingPublishLocalization,
    #[error("发布助手必须提供 emoji 或文件头像")]
    MissingPublishAvatar,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_definition_contains_no_runtime_overlay_fields() {
        let mut definition = LocalAssistantDefinition::new("demo", "Demo");
        definition.rules.insert("zh-CN".into(), "rules/zh-CN.md".into());
        definition
            .skill_dependencies
            .push(LocalSkillDependency::local("local-skill"));
        let value = serde_json::to_value(definition).unwrap();

        for forbidden in [
            "engine",
            "engineId",
            "model",
            "permission",
            "enabled",
            "sortOrder",
            "agentId",
        ] {
            assert!(value.get(forbidden).is_none(), "{forbidden} leaked into Definition");
        }
        assert_eq!(value["skillDependencies"][0]["assetId"], "local-skill");
    }

    #[test]
    fn publishing_requires_explicit_remote_dependency_mapping() {
        let mut definition = LocalAssistantDefinition::new("demo", "Demo");
        definition.description = Some("Description".into());
        definition.name_i18n.insert("zh-CN".into(), "演示".into());
        definition.description_i18n.insert("zh-CN".into(), "说明".into());
        definition.rules.insert("zh-CN".into(), "rules/zh-CN.md".into());
        definition.avatar = PortableAssistantAvatar::Emoji { value: "🤖".into() };
        definition
            .skill_dependencies
            .push(LocalSkillDependency::local("local-skill"));

        assert_eq!(
            definition.to_hub(BTreeMap::new()).unwrap_err(),
            LocalAssistantDefinitionError::UnpublishableDependency("local-skill".into())
        );

        let hub = definition
            .to_hub(BTreeMap::from([(
                "local-skill".into(),
                "tjuaeext-skill-demo/skill/demo".into(),
            )]))
            .unwrap();
        assert_eq!(hub.skill_dependencies, ["tjuaeext-skill-demo/skill/demo"]);
    }

    #[test]
    fn local_dependency_rejects_remote_or_runtime_identity_shape() {
        let raw = serde_json::json!({
            "source": "remote",
            "assetId": "tjuaeext-skill-demo/skill/demo"
        });
        assert!(serde_json::from_value::<LocalSkillDependency>(raw).is_err());
    }
}
