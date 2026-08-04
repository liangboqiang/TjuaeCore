use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use tjuaeui_ai_agent::AgentRegistry;
use tjuaeui_api_types::{AssetKind, AssetPublicConfiguration, AssistantAssetConfiguration, SkillAssetConfiguration};
use tjuaeui_asset::SkillPaths;
use tjuaeui_asset::{
    AssetDefinitionFile, AssetError, AssetRuntimeProjector, RuntimeAssetDefinition, RuntimeProjectionTransaction,
    is_projection_runtime_id, normalize_relative_path,
};
use tjuaeui_assistant::asset_definition::{
    HUB_ASSISTANT_SCHEMA, HubAssistantAvatar, HubAssistantDefinition, LOCAL_ASSISTANT_SCHEMA, LocalAssistantDefinition,
    PortableAssistantAvatar,
};
use tjuaeui_common::now_ms;
use tjuaeui_db::models::{AssistantDefinitionRow, AssistantOverlayRow, AssistantPreferenceRow, SkillRow};
use tjuaeui_db::{
    IAgentMetadataRepository, IAssistantDefinitionRepository, IAssistantOverlayRepository,
    IAssistantPreferenceRepository, IMcpServerRepository, ISkillRepository, UpsertAssistantDefinitionParams,
    UpsertAssistantOverlayParams, UpsertAssistantPreferenceParams, UpsertSkillParams,
};
use tjuaeui_mcp::McpConnectionTestService;
use uuid::Uuid;

mod engine_adapter;
mod mcp;

const DEFAULT_TJUAECLI_AGENT_ID: &str = "632f31d2";
#[derive(Clone)]
pub(crate) struct CoreRuntimeAssetProjector {
    assistant_repo: Arc<dyn IAssistantDefinitionRepository>,
    assistant_overlay_repo: Arc<dyn IAssistantOverlayRepository>,
    assistant_preference_repo: Arc<dyn IAssistantPreferenceRepository>,
    skill_repo: Arc<dyn ISkillRepository>,
    skill_paths: Arc<SkillPaths>,
    agent_metadata_repo: Arc<dyn IAgentMetadataRepository>,
    agent_registry: Arc<AgentRegistry>,
    mcp_server_repo: Arc<dyn IMcpServerRepository>,
    mcp_connection_test: McpConnectionTestService,
    data_dir: PathBuf,
}

impl CoreRuntimeAssetProjector {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        assistant_repo: Arc<dyn IAssistantDefinitionRepository>,
        assistant_overlay_repo: Arc<dyn IAssistantOverlayRepository>,
        assistant_preference_repo: Arc<dyn IAssistantPreferenceRepository>,
        skill_repo: Arc<dyn ISkillRepository>,
        skill_paths: Arc<SkillPaths>,
        agent_metadata_repo: Arc<dyn IAgentMetadataRepository>,
        agent_registry: Arc<AgentRegistry>,
        mcp_server_repo: Arc<dyn IMcpServerRepository>,
        mcp_connection_test: McpConnectionTestService,
        data_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            assistant_repo,
            assistant_overlay_repo,
            assistant_preference_repo,
            skill_repo,
            skill_paths,
            agent_metadata_repo,
            agent_registry,
            mcp_server_repo,
            mcp_connection_test,
            data_dir: data_dir.into(),
        }
    }

    async fn prepare(
        &self,
        user_id: &str,
        assets: Vec<RuntimeAssetDefinition>,
        mode: ProjectionMode,
    ) -> Result<Box<dyn RuntimeProjectionTransaction>, AssetError> {
        validate_runtime_user_id(user_id)?;
        if assets.is_empty() {
            return Err(AssetError::BundleInvariant("运行时投影 Bundle 不能为空".into()));
        }

        let bundle_skill_names = assets
            .iter()
            .filter(|asset| asset.kind == AssetKind::Skill)
            .map(|asset| asset.projection_runtime_id.clone())
            .collect::<BTreeSet<_>>();
        let bundle_assistant_names = assets
            .iter()
            .filter(|asset| asset.kind == AssetKind::Assistant)
            .map(|asset| asset.projection_runtime_id.clone())
            .collect::<BTreeSet<_>>();
        let bundle_engine_names = assets
            .iter()
            .filter(|asset| asset.kind == AssetKind::EngineAdapter)
            .map(|asset| asset.projection_runtime_id.clone())
            .collect::<BTreeSet<_>>();
        let mut identities = BTreeSet::new();
        let mut actions = Vec::with_capacity(assets.len());
        for asset in assets {
            validate_projection_runtime_id(&asset.projection_runtime_id)?;
            if !identities.insert((runtime_kind_name(asset.kind), asset.projection_runtime_id.clone())) {
                return Err(AssetError::BundleInvariant("Bundle 包含重复运行时身份".into()));
            }
            match asset.kind {
                AssetKind::Assistant => {
                    actions.push(ProjectionAction::Assistant(Box::new(
                        self.prepare_assistant(asset, mode).await?,
                    )));
                }
                AssetKind::Skill => {
                    actions.push(ProjectionAction::Skill(Box::new(
                        self.prepare_skill(asset, mode).await?,
                    )));
                }
                AssetKind::EngineAdapter => {
                    actions.push(ProjectionAction::Engine(Box::new(
                        engine_adapter::prepare(
                            asset,
                            mode,
                            Arc::clone(&self.agent_metadata_repo),
                            Arc::clone(&self.agent_registry),
                        )
                        .await?,
                    )));
                }
                AssetKind::Mcp => {
                    actions.push(ProjectionAction::Mcp(Box::new(
                        mcp::prepare(asset, mode, Arc::clone(&self.mcp_server_repo)).await?,
                    )));
                }
            }
        }
        match mode {
            ProjectionMode::Replace => {
                for action in &actions {
                    let ProjectionAction::Assistant(assistant) = action else {
                        continue;
                    };
                    let Some(replacement) = assistant.replacement.as_ref() else {
                        continue;
                    };
                    for dependency in parse_string_list(&replacement.default_skill_ids)? {
                        if bundle_skill_names.contains(&dependency) {
                            continue;
                        }
                        let installed = self.skill_repo.find_by_name(&dependency).await?;
                        if installed.is_none_or(|row| !row.enabled || row.deleted_at.is_some()) {
                            return Err(AssetError::RuntimeProjectionUnsupported {
                                code: "RUNTIME_ASSISTANT_DEPENDENCY_MISSING",
                                message: format!("助手 {} 依赖尚未安装的技能 {}", replacement.assistant_id, dependency),
                            });
                        }
                    }
                    if let Some(engine_id) = assistant
                        .local_configuration
                        .as_ref()
                        .and_then(|configuration| configuration.replacement_overlay.agent_id_override.as_deref())
                        && !bundle_engine_names.contains(engine_id)
                    {
                        let installed = self.agent_metadata_repo.get(engine_id).await?;
                        if installed.is_none_or(|row| !row.enabled) {
                            return Err(AssetError::RuntimeProjectionUnsupported {
                                code: "RUNTIME_ASSISTANT_ENGINE_MISSING",
                                message: format!("助手 {} 引用尚未激活的引擎 {}", replacement.assistant_id, engine_id),
                            });
                        }
                    }
                }
            }
            ProjectionMode::Remove => {
                let removed_skills = actions
                    .iter()
                    .filter_map(|action| match action {
                        ProjectionAction::Skill(skill) => Some(skill.name.as_str()),
                        ProjectionAction::Assistant(_) | ProjectionAction::Engine(_) | ProjectionAction::Mcp(_) => None,
                        #[cfg(test)]
                        ProjectionAction::Test(_) => None,
                    })
                    .collect::<BTreeSet<_>>();
                if !removed_skills.is_empty() {
                    for assistant in self.assistant_repo.list().await? {
                        if bundle_assistant_names.contains(&assistant.assistant_id) {
                            continue;
                        }
                        let dependencies = parse_string_list(&assistant.default_skill_ids)?;
                        if let Some(dependency) = dependencies
                            .into_iter()
                            .find(|dependency| removed_skills.contains(dependency.as_str()))
                        {
                            return Err(AssetError::RuntimeProjectionUnsupported {
                                code: "RUNTIME_SKILL_DEPENDENCY_IN_USE",
                                message: format!(
                                    "技能 {dependency} 仍被助手 {} 引用，不能卸载",
                                    assistant.assistant_id
                                ),
                            });
                        }
                    }
                }
                if !bundle_engine_names.is_empty() {
                    let definitions = self
                        .assistant_repo
                        .list()
                        .await?
                        .into_iter()
                        .map(|definition| (definition.id, definition.assistant_id))
                        .collect::<BTreeMap<_, _>>();
                    for overlay in self.assistant_overlay_repo.list().await? {
                        let Some(engine_id) = overlay.agent_id_override.as_deref() else {
                            continue;
                        };
                        if !bundle_engine_names.contains(engine_id) {
                            continue;
                        }
                        let assistant_id = definitions.get(&overlay.assistant_definition_id);
                        if assistant_id.is_some_and(|id| bundle_assistant_names.contains(id)) {
                            continue;
                        }
                        return Err(AssetError::RuntimeProjectionUnsupported {
                            code: "RUNTIME_ENGINE_DEPENDENCY_IN_USE",
                            message: format!(
                                "引擎 {engine_id} 仍被助手 {} 引用，不能卸载",
                                assistant_id.map_or("未知助手", String::as_str)
                            ),
                        });
                    }
                }
            }
        }

        actions.sort_by_key(|action| action.topology_rank(mode));

        Ok(Box::new(CoreProjectionTransaction {
            actions,
            applied: 0,
            finalized: false,
        }))
    }

    async fn prepare_assistant(
        &self,
        asset: RuntimeAssetDefinition,
        mode: ProjectionMode,
    ) -> Result<AssistantProjection, AssetError> {
        let raw = definition_file(&asset, &asset.entry_file)?;
        let definition = parse_assistant_definition(raw)?;
        validate_assistant_definition(&definition, &asset)?;
        let runtime_configuration = match asset.runtime_configuration.as_ref() {
            Some(resolved) => {
                if !resolved.secrets.is_empty() {
                    return Err(AssetError::RuntimeProjectionUnsupported {
                        code: "RUNTIME_ASSISTANT_SECRET_UNSUPPORTED",
                        message: "助手本机配置不接受凭据槽".into(),
                    });
                }
                match &resolved.configuration {
                    AssetPublicConfiguration::Assistant(configuration) => Some(configuration.clone()),
                    _ => {
                        return Err(AssetError::RuntimeProjectionUnsupported {
                            code: "RUNTIME_CONFIGURATION_KIND_MISMATCH",
                            message: "本机配置类型与 Assistant 资产不匹配".into(),
                        });
                    }
                }
            }
            None => None,
        };
        let source_ref = assistant_source_ref(definition.ownership, &asset.projection_runtime_id);
        let source = definition.runtime_source();
        let existing = self
            .assistant_repo
            .get_by_assistant_id_including_deleted(&asset.projection_runtime_id)
            .await?
            .or(self
                .assistant_repo
                .get_by_source_ref_including_deleted(source, &source_ref)
                .await?);
        if let Some(row) = existing.as_ref()
            && row.source == "user"
            && row.source_ref.as_deref() != Some(source_ref.as_str())
            && !(definition.ownership == AssistantDefinitionOwnership::Local
                && row.assistant_id == asset.projection_runtime_id
                && row.source_ref.as_deref() == Some(row.assistant_id.as_str()))
        {
            return Err(AssetError::RuntimeProjectionUnsupported {
                code: "RUNTIME_ASSISTANT_ID_COLLISION",
                message: format!("助手 {} 的内部运行时投影已由其他来源占用", asset.portable_runtime_id),
            });
        }

        let (replacement, rule_files) = match mode {
            ProjectionMode::Replace => {
                let row = assistant_row_from_definition(&definition, &asset, existing.as_ref(), &source_ref)?;
                let rules = assistant_rule_files(&definition, &asset)?;
                (Some(row), rules)
            }
            ProjectionMode::Remove => {
                let Some(existing) = existing.as_ref() else {
                    return Err(AssetError::RuntimeProjectionFailed {
                        code: "RUNTIME_ASSISTANT_PROJECTION_MISSING",
                        message: format!("助手 {} 的运行时投影不存在", asset.portable_runtime_id),
                    });
                };
                ensure_assistant_projection_ownership(existing, &source_ref, &asset.portable_runtime_id)?;
                (None, BTreeMap::new())
            }
        };
        let assistant_id = replacement
            .as_ref()
            .map(|row| row.assistant_id.clone())
            .or_else(|| existing.as_ref().map(|row| row.assistant_id.clone()))
            .ok_or_else(|| AssetError::InvalidMetadata("助手缺少运行时 ID".into()))?;
        let definition_id = replacement
            .as_ref()
            .map(|row| row.id.as_str())
            .or_else(|| existing.as_ref().map(|row| row.id.as_str()))
            .ok_or_else(|| AssetError::InvalidMetadata("助手缺少运行时 Definition ID".into()))?;
        let local_configuration = if mode == ProjectionMode::Replace {
            match runtime_configuration {
                Some(configuration) => Some(
                    self.prepare_assistant_local_configuration(
                        definition_id,
                        &configuration,
                        replacement.as_ref().expect("replace projection must have replacement"),
                        &asset.dependency_projection_runtime_ids,
                    )
                    .await?,
                ),
                None => None,
            }
        } else {
            None
        };
        Ok(AssistantProjection {
            repo: Arc::clone(&self.assistant_repo),
            mode,
            previous: existing,
            replacement,
            rules: FileSetProjection::new(
                self.data_dir.join("assistant-rules"),
                assistant_rule_prefix(&assistant_id),
                rule_files,
            ),
            avatars: FileSetProjection::all_files(
                self.data_dir.join("assistant-avatars"),
                format!("{}.", encode_filename_component(&assistant_id)),
                assistant_avatar_files(&definition, &asset)?,
            ),
            local_configuration,
            applied: false,
        })
    }

    async fn prepare_assistant_local_configuration(
        &self,
        definition_id: &str,
        configuration: &AssistantAssetConfiguration,
        replacement: &AssistantDefinitionRow,
        dependency_projection_runtime_ids: &BTreeMap<String, String>,
    ) -> Result<AssistantLocalConfigurationProjection, AssetError> {
        let previous_overlay = self.assistant_overlay_repo.get(definition_id).await?;
        let previous_preference = self.assistant_preference_repo.get(definition_id).await?;
        let agent_id_override = match configuration.engine_asset_id.as_deref() {
            Some(asset_id) => {
                let runtime_id = dependency_projection_runtime_ids.get(asset_id).ok_or_else(|| {
                    AssetError::RuntimeProjectionUnsupported {
                        code: "RUNTIME_ASSISTANT_ENGINE_UNRESOLVED",
                        message: format!("助手引用的引擎资产 {asset_id} 没有固定 runtimeId 映射"),
                    }
                })?;
                validate_runtime_name(runtime_id, "助手引擎 runtimeId")?;
                Some(runtime_id.clone())
            }
            None => None,
        };
        let skill_ids = parse_string_list(&replacement.default_skill_ids)?;
        let last_skill_ids = serde_json::to_string(&skill_ids)?;
        let last_mcp_ids = previous_preference
            .as_ref()
            .map(|preference| preference.last_mcp_ids.clone())
            .unwrap_or_else(|| "[]".into());
        let has_preference =
            configuration.default_model_id.is_some() || !skill_ids.is_empty() || previous_preference.is_some();
        let replacement_overlay = AssistantOverlayReplacement {
            enabled: previous_overlay.as_ref().is_none_or(|overlay| overlay.enabled),
            sort_order: configuration
                .sort_order
                .or_else(|| previous_overlay.as_ref().map(|overlay| overlay.sort_order))
                .unwrap_or_default(),
            agent_id_override,
            last_used_at: previous_overlay.as_ref().and_then(|overlay| overlay.last_used_at),
        };
        let replacement_preference = has_preference.then(|| AssistantPreferenceReplacement {
            last_model_id: configuration.default_model_id.clone(),
            last_permission_value: previous_preference
                .as_ref()
                .and_then(|preference| preference.last_permission_value.clone()),
            last_thought_level_value: previous_preference
                .as_ref()
                .and_then(|preference| preference.last_thought_level_value.clone()),
            last_skill_ids,
            last_mcp_ids,
        });

        Ok(AssistantLocalConfigurationProjection {
            overlay_repo: Arc::clone(&self.assistant_overlay_repo),
            preference_repo: Arc::clone(&self.assistant_preference_repo),
            definition_id: definition_id.into(),
            previous_overlay,
            previous_preference,
            replacement_overlay,
            replacement_preference,
            applied: false,
        })
    }

    async fn prepare_skill(
        &self,
        asset: RuntimeAssetDefinition,
        mode: ProjectionMode,
    ) -> Result<SkillProjection, AssetError> {
        validate_skill_configuration(&asset)?;
        let target = self.skill_paths.user_skills_dir.join(&asset.projection_runtime_id);
        ensure_direct_child(&self.skill_paths.user_skills_dir, &target)?;
        let previous = self.skill_repo.find_by_name_any(&asset.projection_runtime_id).await?;
        if let Some(row) = previous.as_ref() {
            let previous_path = PathBuf::from(&row.path);
            if previous_path != target {
                return Err(AssetError::RuntimeProjectionUnsupported {
                    code: "RUNTIME_SKILL_ID_COLLISION",
                    message: format!("技能 {} 的内部运行时投影已由其他来源占用", asset.portable_runtime_id),
                });
            }
        } else if target.exists() {
            return Err(AssetError::RuntimeProjectionUnsupported {
                code: "RUNTIME_SKILL_ID_COLLISION",
                message: format!(
                    "技能 {} 的投影目录已存在但没有可验证的所有权记录",
                    asset.portable_runtime_id
                ),
            });
        }

        let (files, description) = match mode {
            ProjectionMode::Replace => {
                let manifest = definition_file(&asset, &asset.entry_file)?;
                let metadata = parse_skill_frontmatter(manifest)?;
                if metadata.name != asset.portable_runtime_id {
                    return Err(AssetError::RuntimeProjectionUnsupported {
                        code: "RUNTIME_SKILL_ID_MISMATCH",
                        message: format!(
                            "SKILL.md 名称 {} 与 runtimeId {} 不一致",
                            metadata.name, asset.portable_runtime_id
                        ),
                    });
                }
                (asset.files, metadata.description)
            }
            ProjectionMode::Remove => {
                if previous.as_ref().is_none_or(|row| row.deleted_at.is_some()) {
                    return Err(AssetError::RuntimeProjectionFailed {
                        code: "RUNTIME_SKILL_PROJECTION_MISSING",
                        message: format!("技能 {} 的运行时投影不存在", asset.portable_runtime_id),
                    });
                }
                (Vec::new(), String::new())
            }
        };

        Ok(SkillProjection {
            repo: Arc::clone(&self.skill_repo),
            mode,
            name: asset.projection_runtime_id,
            description,
            target,
            files,
            previous,
            activation: None,
            applied: false,
        })
    }
}

#[async_trait]
impl AssetRuntimeProjector for CoreRuntimeAssetProjector {
    async fn validate(&self, user_id: &str, assets: Vec<RuntimeAssetDefinition>) -> Result<(), AssetError> {
        validate_bundle_scope(user_id, &assets)?;
        for asset in &assets {
            match asset.kind {
                AssetKind::Assistant => validate_assistant_asset(asset)?,
                AssetKind::Skill => validate_skill_asset(asset)?,
                AssetKind::EngineAdapter => engine_adapter::validate(asset)?,
                AssetKind::Mcp => mcp::validate(asset)?,
            }
        }
        Ok(())
    }

    async fn try_run(&self, user_id: &str, assets: Vec<RuntimeAssetDefinition>) -> Result<(), AssetError> {
        self.validate(user_id, assets.clone()).await?;
        for asset in &assets {
            match asset.kind {
                AssetKind::EngineAdapter => engine_adapter::try_run(asset).await?,
                AssetKind::Mcp => mcp::try_run(asset, &self.mcp_connection_test).await?,
                AssetKind::Assistant | AssetKind::Skill => {}
            }
        }
        Ok(())
    }

    async fn prepare_replace(
        &self,
        user_id: &str,
        assets: Vec<RuntimeAssetDefinition>,
    ) -> Result<Box<dyn RuntimeProjectionTransaction>, AssetError> {
        self.prepare(user_id, assets, ProjectionMode::Replace).await
    }

    async fn prepare_remove(
        &self,
        user_id: &str,
        assets: Vec<RuntimeAssetDefinition>,
    ) -> Result<Box<dyn RuntimeProjectionTransaction>, AssetError> {
        self.prepare(user_id, assets, ProjectionMode::Remove).await
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProjectionMode {
    Replace,
    Remove,
}

struct CoreProjectionTransaction {
    actions: Vec<ProjectionAction>,
    applied: usize,
    finalized: bool,
}

enum ProjectionAction {
    Assistant(Box<AssistantProjection>),
    Skill(Box<SkillProjection>),
    Engine(Box<engine_adapter::EngineProjection>),
    Mcp(Box<mcp::McpProjection>),
    #[cfg(test)]
    Test(TestProjection),
}

impl ProjectionAction {
    async fn apply(&mut self) -> Result<(), AssetError> {
        match self {
            Self::Assistant(action) => action.apply().await,
            Self::Skill(action) => action.apply().await,
            Self::Engine(action) => action.apply().await,
            Self::Mcp(action) => action.apply().await,
            #[cfg(test)]
            Self::Test(action) => action.apply(),
        }
    }

    async fn rollback(&mut self) -> Result<(), AssetError> {
        match self {
            Self::Assistant(action) => action.rollback().await,
            Self::Skill(action) => action.rollback().await,
            Self::Engine(action) => action.rollback().await,
            Self::Mcp(action) => action.rollback().await,
            #[cfg(test)]
            Self::Test(action) => action.rollback(),
        }
    }

    fn finalize(&mut self) {
        match self {
            Self::Assistant(action) => action.finalize(),
            Self::Skill(action) => action.finalize(),
            Self::Engine(action) => action.finalize(),
            Self::Mcp(action) => action.finalize(),
            #[cfg(test)]
            Self::Test(action) => action.finalize(),
        }
    }

    fn topology_rank(&self, mode: ProjectionMode) -> u8 {
        let kind = match self {
            Self::Assistant(_) => AssetKind::Assistant,
            Self::Skill(_) => AssetKind::Skill,
            Self::Engine(_) => AssetKind::EngineAdapter,
            Self::Mcp(_) => AssetKind::Mcp,
            #[cfg(test)]
            Self::Test(_) => return 1,
        };
        projection_topology_rank(mode, kind)
    }
}

fn projection_topology_rank(mode: ProjectionMode, kind: AssetKind) -> u8 {
    match (mode, kind) {
        (ProjectionMode::Replace, AssetKind::EngineAdapter | AssetKind::Mcp) => 0,
        (ProjectionMode::Replace, AssetKind::Skill) => 1,
        (ProjectionMode::Replace, AssetKind::Assistant) => 2,
        (ProjectionMode::Remove, AssetKind::Assistant) => 0,
        (ProjectionMode::Remove, AssetKind::Skill | AssetKind::Mcp) => 1,
        (ProjectionMode::Remove, AssetKind::EngineAdapter) => 2,
    }
}

#[cfg(test)]
struct TestProjection {
    name: &'static str,
    events: Arc<std::sync::Mutex<Vec<String>>>,
    fail_apply: bool,
    fail_rollback: bool,
    applied: bool,
}

#[cfg(test)]
impl TestProjection {
    fn apply(&mut self) -> Result<(), AssetError> {
        self.events.lock().unwrap().push(format!("apply:{}", self.name));
        if self.fail_apply {
            return Err(AssetError::RuntimeProjectionFailed {
                code: "TEST_RUNTIME_APPLY_FAILED",
                message: "故障注入".into(),
            });
        }
        self.applied = true;
        Ok(())
    }

    fn rollback(&mut self) -> Result<(), AssetError> {
        if self.applied {
            self.events.lock().unwrap().push(format!("rollback:{}", self.name));
            if self.fail_rollback {
                return Err(AssetError::RuntimeProjectionFailed {
                    code: "TEST_RUNTIME_ROLLBACK_FAILED",
                    message: "补偿故障注入".into(),
                });
            }
            self.applied = false;
        }
        Ok(())
    }

    fn finalize(&mut self) {
        self.events.lock().unwrap().push(format!("finalize:{}", self.name));
        self.applied = false;
    }
}

#[async_trait]
impl RuntimeProjectionTransaction for CoreProjectionTransaction {
    async fn apply(&mut self) -> Result<(), AssetError> {
        for index in self.applied..self.actions.len() {
            self.actions[index].apply().await?;
            self.applied += 1;
        }
        Ok(())
    }

    async fn rollback(&mut self) -> Result<(), AssetError> {
        let mut first_error = None;
        while self.applied > 0 {
            self.applied -= 1;
            if let Err(error) = self.actions[self.applied].rollback().await
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    async fn finalize(mut self: Box<Self>) {
        for action in &mut self.actions {
            action.finalize();
        }
        self.finalized = true;
    }
}

impl Drop for CoreProjectionTransaction {
    fn drop(&mut self) {
        if !self.finalized && self.applied > 0 {
            tracing::error!(
                applied_actions = self.applied,
                "runtime projection transaction dropped before rollback/finalize"
            );
        }
    }
}

struct AssistantProjection {
    repo: Arc<dyn IAssistantDefinitionRepository>,
    mode: ProjectionMode,
    previous: Option<AssistantDefinitionRow>,
    replacement: Option<AssistantDefinitionRow>,
    rules: FileSetProjection,
    avatars: FileSetProjection,
    local_configuration: Option<AssistantLocalConfigurationProjection>,
    applied: bool,
}

impl AssistantProjection {
    async fn apply(&mut self) -> Result<(), AssetError> {
        self.rules.apply()?;
        if let Err(error) = self.avatars.apply() {
            self.rules.rollback()?;
            return Err(error);
        }
        let result = match self.mode {
            ProjectionMode::Replace => match self.replacement.as_ref() {
                Some(replacement) => match upsert_assistant(self.repo.as_ref(), replacement).await {
                    Ok(_) => match self.local_configuration.as_mut() {
                        Some(configuration) => configuration.apply().await,
                        None => Ok(()),
                    },
                    Err(error) => Err(error),
                },
                None => Err(AssetError::InvalidState("缺少助手替换定义".into())),
            },
            ProjectionMode::Remove => match self.previous.as_ref() {
                Some(previous) => self
                    .repo
                    .soft_delete(&previous.id, now_ms())
                    .await
                    .map_err(AssetError::from)
                    .and_then(|removed| {
                        removed
                            .then_some(())
                            .ok_or_else(|| AssetError::RuntimeProjectionFailed {
                                code: "RUNTIME_ASSISTANT_PROJECTION_MISSING",
                                message: "助手运行时投影已不存在".into(),
                            })
                    }),
                None => Err(AssetError::InvalidState("缺少助手运行定义".into())),
            },
        };
        if let Err(error) = result {
            if let Some(configuration) = self.local_configuration.as_mut() {
                configuration.rollback().await?;
            }
            if self.mode == ProjectionMode::Replace {
                if let Some(previous) = self.previous.as_ref() {
                    upsert_assistant(self.repo.as_ref(), previous).await?;
                    if let Some(deleted_at) = previous.deleted_at {
                        self.repo.soft_delete(&previous.id, deleted_at).await?;
                    }
                } else if let Some(replacement) = self.replacement.as_ref() {
                    self.repo.soft_delete(&replacement.id, now_ms()).await?;
                }
            }
            self.avatars.rollback()?;
            self.rules.rollback()?;
            return Err(error);
        }
        self.applied = true;
        Ok(())
    }

    async fn rollback(&mut self) -> Result<(), AssetError> {
        if !self.applied {
            return Ok(());
        }
        if let Some(configuration) = self.local_configuration.as_mut() {
            configuration.rollback().await?;
        }
        if let Some(previous) = self.previous.as_ref() {
            upsert_assistant(self.repo.as_ref(), previous).await?;
            if let Some(deleted_at) = previous.deleted_at {
                self.repo.soft_delete(&previous.id, deleted_at).await?;
            }
        } else if let Some(replacement) = self.replacement.as_ref() {
            self.repo.soft_delete(&replacement.id, now_ms()).await?;
        }
        self.avatars.rollback()?;
        self.rules.rollback()?;
        self.applied = false;
        Ok(())
    }

    fn finalize(&mut self) {
        if let Some(configuration) = self.local_configuration.as_mut() {
            configuration.finalize();
        }
        self.avatars.finalize();
        self.rules.finalize();
        self.applied = false;
    }
}

struct AssistantLocalConfigurationProjection {
    overlay_repo: Arc<dyn IAssistantOverlayRepository>,
    preference_repo: Arc<dyn IAssistantPreferenceRepository>,
    definition_id: String,
    previous_overlay: Option<AssistantOverlayRow>,
    previous_preference: Option<AssistantPreferenceRow>,
    replacement_overlay: AssistantOverlayReplacement,
    replacement_preference: Option<AssistantPreferenceReplacement>,
    applied: bool,
}

struct AssistantOverlayReplacement {
    enabled: bool,
    sort_order: i32,
    agent_id_override: Option<String>,
    last_used_at: Option<i64>,
}

struct AssistantPreferenceReplacement {
    last_model_id: Option<String>,
    last_permission_value: Option<String>,
    last_thought_level_value: Option<String>,
    last_skill_ids: String,
    last_mcp_ids: String,
}

impl AssistantLocalConfigurationProjection {
    async fn apply(&mut self) -> Result<(), AssetError> {
        self.overlay_repo
            .upsert(&UpsertAssistantOverlayParams {
                assistant_definition_id: &self.definition_id,
                enabled: self.replacement_overlay.enabled,
                sort_order: self.replacement_overlay.sort_order,
                agent_id_override: self.replacement_overlay.agent_id_override.as_deref(),
                last_used_at: self.replacement_overlay.last_used_at,
            })
            .await?;
        let preference_result = match self.replacement_preference.as_ref() {
            Some(preference) => self
                .preference_repo
                .upsert(&UpsertAssistantPreferenceParams {
                    assistant_definition_id: &self.definition_id,
                    last_model_id: preference.last_model_id.as_deref(),
                    last_permission_value: preference.last_permission_value.as_deref(),
                    last_thought_level_value: preference.last_thought_level_value.as_deref(),
                    last_skill_ids: &preference.last_skill_ids,
                    last_mcp_ids: &preference.last_mcp_ids,
                })
                .await
                .map(|_| ()),
            None => self.preference_repo.delete(&self.definition_id).await.map(|_| ()),
        };
        if let Err(error) = preference_result {
            restore_overlay(
                self.overlay_repo.as_ref(),
                &self.definition_id,
                self.previous_overlay.as_ref(),
            )
            .await?;
            restore_preference(
                self.preference_repo.as_ref(),
                &self.definition_id,
                self.previous_preference.as_ref(),
            )
            .await?;
            return Err(error.into());
        }
        self.applied = true;
        Ok(())
    }

    async fn rollback(&mut self) -> Result<(), AssetError> {
        if !self.applied {
            return Ok(());
        }
        restore_preference(
            self.preference_repo.as_ref(),
            &self.definition_id,
            self.previous_preference.as_ref(),
        )
        .await?;
        restore_overlay(
            self.overlay_repo.as_ref(),
            &self.definition_id,
            self.previous_overlay.as_ref(),
        )
        .await?;
        self.applied = false;
        Ok(())
    }

    fn finalize(&mut self) {
        self.applied = false;
    }
}

async fn restore_overlay(
    repo: &dyn IAssistantOverlayRepository,
    definition_id: &str,
    previous: Option<&AssistantOverlayRow>,
) -> Result<(), AssetError> {
    match previous {
        Some(previous) => {
            repo.upsert(&UpsertAssistantOverlayParams {
                assistant_definition_id: definition_id,
                enabled: previous.enabled,
                sort_order: previous.sort_order,
                agent_id_override: previous.agent_id_override.as_deref(),
                last_used_at: previous.last_used_at,
            })
            .await?;
        }
        None => {
            repo.delete(definition_id).await?;
        }
    }
    Ok(())
}

async fn restore_preference(
    repo: &dyn IAssistantPreferenceRepository,
    definition_id: &str,
    previous: Option<&AssistantPreferenceRow>,
) -> Result<(), AssetError> {
    match previous {
        Some(previous) => {
            repo.upsert(&UpsertAssistantPreferenceParams {
                assistant_definition_id: definition_id,
                last_model_id: previous.last_model_id.as_deref(),
                last_permission_value: previous.last_permission_value.as_deref(),
                last_thought_level_value: previous.last_thought_level_value.as_deref(),
                last_skill_ids: &previous.last_skill_ids,
                last_mcp_ids: &previous.last_mcp_ids,
            })
            .await?;
        }
        None => {
            repo.delete(definition_id).await?;
        }
    }
    Ok(())
}

struct SkillProjection {
    repo: Arc<dyn ISkillRepository>,
    mode: ProjectionMode,
    name: String,
    description: String,
    target: PathBuf,
    files: Vec<AssetDefinitionFile>,
    previous: Option<SkillRow>,
    activation: Option<DirectoryProjection>,
    applied: bool,
}

impl SkillProjection {
    async fn apply(&mut self) -> Result<(), AssetError> {
        let mut activation = DirectoryProjection::new(self.target.clone(), self.files.clone(), self.mode)?;
        activation.apply()?;
        let result = match self.mode {
            ProjectionMode::Replace => {
                let path = self.target.to_string_lossy().into_owned();
                let enabled = self
                    .previous
                    .as_ref()
                    .filter(|previous| previous.deleted_at.is_none())
                    .map(|previous| previous.enabled)
                    .unwrap_or(true);
                self.repo
                    .upsert(UpsertSkillParams {
                        name: &self.name,
                        description: Some(&self.description),
                        path: &path,
                        source: "user",
                        enabled,
                    })
                    .await
                    .map(|_| ())
                    .map_err(AssetError::from)
            }
            ProjectionMode::Remove => self
                .repo
                .delete_by_name(&self.name)
                .await
                .map(|_| ())
                .map_err(AssetError::from),
        };
        if let Err(error) = result {
            activation.rollback()?;
            return Err(error);
        }
        self.activation = Some(activation);
        self.applied = true;
        Ok(())
    }

    async fn rollback(&mut self) -> Result<(), AssetError> {
        if !self.applied {
            return Ok(());
        }
        if let Some(previous) = self.previous.as_ref() {
            self.repo
                .upsert(UpsertSkillParams {
                    name: &previous.name,
                    description: previous.description.as_deref(),
                    path: &previous.path,
                    source: &previous.source,
                    enabled: previous.enabled,
                })
                .await?;
            if previous.deleted_at.is_some() {
                self.repo.delete_by_name(&previous.name).await?;
            }
        } else {
            self.repo.delete_by_name(&self.name).await?;
        }
        if let Some(activation) = self.activation.as_mut() {
            activation.rollback()?;
        }
        self.applied = false;
        Ok(())
    }

    fn finalize(&mut self) {
        if let Some(activation) = self.activation.as_mut() {
            activation.finalize();
        }
        self.applied = false;
    }
}

struct DirectoryProjection {
    target: PathBuf,
    staging: PathBuf,
    backup: PathBuf,
    rollback_new: PathBuf,
    mode: ProjectionMode,
    files: Vec<AssetDefinitionFile>,
    had_previous: bool,
    applied: bool,
    #[cfg(test)]
    fail_rollback_target_rename: bool,
}

impl DirectoryProjection {
    fn new(target: PathBuf, files: Vec<AssetDefinitionFile>, mode: ProjectionMode) -> Result<Self, AssetError> {
        let parent = target
            .parent()
            .ok_or_else(|| AssetError::UnsafePath(target.display().to_string()))?
            .to_path_buf();
        let token = Uuid::now_v7();
        Ok(Self {
            target,
            staging: parent.join(format!(".asset-staging-{token}")),
            backup: parent.join(format!(".asset-backup-{token}")),
            rollback_new: parent.join(format!(".asset-rollback-new-{token}")),
            mode,
            files,
            had_previous: false,
            applied: false,
            #[cfg(test)]
            fail_rollback_target_rename: false,
        })
    }

    fn apply(&mut self) -> Result<(), AssetError> {
        let parent = self
            .target
            .parent()
            .ok_or_else(|| AssetError::UnsafePath(self.target.display().to_string()))?;
        std::fs::create_dir_all(parent)?;
        self.had_previous = self.target.exists();
        if self.mode == ProjectionMode::Remove && !self.had_previous {
            return Err(AssetError::RuntimeProjectionFailed {
                code: "RUNTIME_SKILL_DIRECTORY_MISSING",
                message: "技能运行目录不存在，拒绝仅删除元数据".into(),
            });
        }
        if self.mode == ProjectionMode::Replace
            && let Err(error) = write_definition_tree(&self.staging, &self.files)
        {
            let _ = std::fs::remove_dir_all(&self.staging);
            return Err(error);
        }
        if self.had_previous {
            std::fs::rename(&self.target, &self.backup)?;
        }
        if self.mode == ProjectionMode::Replace
            && let Err(error) = std::fs::rename(&self.staging, &self.target)
        {
            if self.had_previous {
                let _ = std::fs::rename(&self.backup, &self.target);
            }
            let _ = std::fs::remove_dir_all(&self.staging);
            return Err(error.into());
        }
        self.applied = true;
        Ok(())
    }

    fn rollback(&mut self) -> Result<(), AssetError> {
        if !self.applied {
            return Ok(());
        }
        let mut cleanup_new = None;
        if self.target.exists() {
            #[cfg(test)]
            if self.fail_rollback_target_rename {
                return Err(AssetError::RuntimeProjectionFailed {
                    code: "RUNTIME_DIRECTORY_ROLLBACK_BLOCKED",
                    message: "injected rollback rename failure".into(),
                });
            }
            std::fs::rename(&self.target, &self.rollback_new).map_err(|error| AssetError::RuntimeProjectionFailed {
                code: "RUNTIME_DIRECTORY_ROLLBACK_BLOCKED",
                message: format!("无法隔离待回滚的技能目录：{error}"),
            })?;
            cleanup_new = Some(self.rollback_new.clone());
        }
        if self.had_previous
            && self.backup.exists()
            && let Err(error) = std::fs::rename(&self.backup, &self.target)
        {
            if let Some(new_path) = cleanup_new.as_ref() {
                let _ = std::fs::rename(new_path, &self.target);
            }
            return Err(AssetError::RuntimeProjectionFailed {
                code: "RUNTIME_DIRECTORY_RESTORE_FAILED",
                message: format!("无法恢复旧技能目录：{error}"),
            });
        }
        if self.staging.exists() {
            let _ = std::fs::remove_dir_all(&self.staging);
        }
        if let Some(new_path) = cleanup_new
            && new_path.exists()
            && let Err(error) = std::fs::remove_dir_all(&new_path)
        {
            tracing::warn!(path = %new_path.display(), %error, "failed to clean quarantined runtime skill directory");
        }
        self.applied = false;
        Ok(())
    }

    fn finalize(&mut self) {
        if self.backup.exists()
            && let Err(error) = std::fs::remove_dir_all(&self.backup)
        {
            tracing::warn!(path = %self.backup.display(), %error, "failed to remove runtime asset backup");
        }
        if self.staging.exists() {
            let _ = std::fs::remove_dir_all(&self.staging);
        }
        if self.rollback_new.exists() {
            let _ = std::fs::remove_dir_all(&self.rollback_new);
        }
        self.applied = false;
    }
}

struct FileSetProjection {
    root: PathBuf,
    prefix: String,
    required_suffix: Option<String>,
    replacements: BTreeMap<String, Vec<u8>>,
    backup_dir: PathBuf,
    previous: Vec<PathBuf>,
    written: Vec<PathBuf>,
    applied: bool,
    #[cfg(test)]
    fail_after_operations: Option<usize>,
}

impl FileSetProjection {
    fn new(root: PathBuf, prefix: String, replacements: BTreeMap<String, Vec<u8>>) -> Self {
        Self::with_suffix(root, prefix, replacements, Some(".md".into()))
    }

    fn all_files(root: PathBuf, prefix: String, replacements: BTreeMap<String, Vec<u8>>) -> Self {
        Self::with_suffix(root, prefix, replacements, None)
    }

    fn with_suffix(
        root: PathBuf,
        prefix: String,
        replacements: BTreeMap<String, Vec<u8>>,
        required_suffix: Option<String>,
    ) -> Self {
        Self {
            backup_dir: root.join(format!(".asset-backup-{}", Uuid::now_v7())),
            root,
            prefix,
            required_suffix,
            replacements,
            previous: Vec::new(),
            written: Vec::new(),
            applied: false,
            #[cfg(test)]
            fail_after_operations: None,
        }
    }

    fn apply(&mut self) -> Result<(), AssetError> {
        let result = self.apply_inner();
        if let Err(error) = result {
            if let Err(rollback_error) = self.rollback() {
                tracing::error!(%error, %rollback_error, "assistant rule activation and compensation both failed");
                return Err(AssetError::RuntimeProjectionFailed {
                    code: "RUNTIME_RULE_ACTIVATION_ROLLBACK_FAILED",
                    message: "助手规则写入失败且无法完整恢复旧规则".into(),
                });
            }
            return Err(error);
        }
        self.applied = true;
        Ok(())
    }

    fn apply_inner(&mut self) -> Result<(), AssetError> {
        std::fs::create_dir_all(&self.root)?;
        std::fs::create_dir_all(&self.backup_dir)?;
        let staged_dir = self.backup_dir.join("staged");
        let previous_dir = self.backup_dir.join("previous");
        std::fs::create_dir(&staged_dir)?;
        std::fs::create_dir(&previous_dir)?;
        let mut operation = 0_usize;
        for (name, bytes) in &self.replacements {
            self.maybe_fail(operation)?;
            operation += 1;
            let target = staged_dir.join(name);
            let mut options = std::fs::OpenOptions::new();
            use std::io::Write;
            let mut output = options.create_new(true).write(true).open(&target)?;
            output.write_all(bytes)?;
            output.sync_all()?;
        }
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let name_text = name.to_string_lossy();
            if entry.file_type()?.is_file()
                && name_text.starts_with(&self.prefix)
                && self
                    .required_suffix
                    .as_deref()
                    .is_none_or(|suffix| name_text.ends_with(suffix))
            {
                self.maybe_fail(operation)?;
                operation += 1;
                let backup = previous_dir.join(&name);
                std::fs::rename(entry.path(), backup)?;
                self.previous.push(PathBuf::from(name));
            }
        }
        for name in self.replacements.keys() {
            self.maybe_fail(operation)?;
            operation += 1;
            let target = self.root.join(name);
            std::fs::rename(staged_dir.join(name), &target)?;
            self.written.push(PathBuf::from(name));
        }
        Ok(())
    }

    fn rollback(&mut self) -> Result<(), AssetError> {
        if !self.applied && self.previous.is_empty() && self.written.is_empty() {
            if self.backup_dir.exists() {
                std::fs::remove_dir_all(&self.backup_dir)?;
            }
            return Ok(());
        }
        let quarantine_dir = self.backup_dir.join("quarantine");
        let previous_dir = self.backup_dir.join("previous");
        let mut first_error = None;
        if let Err(error) = std::fs::create_dir_all(&quarantine_dir) {
            first_error = Some(error);
        }
        for name in self.written.drain(..) {
            let target = self.root.join(name);
            if target.exists()
                && let Err(error) =
                    std::fs::rename(&target, quarantine_dir.join(target.file_name().unwrap_or_default()))
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        for name in self.previous.drain(..) {
            let backup = previous_dir.join(&name);
            if backup.exists() {
                let target = self.root.join(name);
                if target.exists() {
                    if first_error.is_none() {
                        first_error = Some(std::io::Error::new(
                            std::io::ErrorKind::AlreadyExists,
                            "new rule file could not be quarantined",
                        ));
                    }
                    continue;
                }
                if let Err(error) = std::fs::rename(backup, target)
                    && first_error.is_none()
                {
                    first_error = Some(error);
                }
            }
        }
        if first_error.is_none() && self.backup_dir.exists() {
            let _ = std::fs::remove_dir_all(&self.backup_dir);
        }
        if let Some(error) = first_error {
            return Err(AssetError::RuntimeProjectionFailed {
                code: "RUNTIME_RULE_ROLLBACK_FAILED",
                message: format!("无法恢复助手规则文件：{error}"),
            });
        }
        self.applied = false;
        Ok(())
    }

    fn finalize(&mut self) {
        if self.backup_dir.exists()
            && let Err(error) = std::fs::remove_dir_all(&self.backup_dir)
        {
            tracing::warn!(path = %self.backup_dir.display(), %error, "failed to remove assistant rule backup");
        }
        self.previous.clear();
        self.written.clear();
        self.applied = false;
    }

    #[cfg(test)]
    fn maybe_fail(&self, operation: usize) -> Result<(), AssetError> {
        if self.fail_after_operations == Some(operation) {
            return Err(AssetError::Io(std::io::Error::other(
                "injected assistant rule filesystem failure",
            )));
        }
        Ok(())
    }

    #[cfg(not(test))]
    fn maybe_fail(&self, _operation: usize) -> Result<(), AssetError> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssistantDefinitionOwnership {
    Local,
    Hub,
}

#[derive(Debug)]
struct AssistantDefinition {
    ownership: AssistantDefinitionOwnership,
    schema_version: u32,
    kind: String,
    runtime_id: String,
    name: String,
    name_i18n: BTreeMap<String, String>,
    description: Option<String>,
    description_i18n: BTreeMap<String, String>,
    rules: BTreeMap<String, String>,
    recommended_prompts: Vec<String>,
    recommended_prompts_i18n: BTreeMap<String, Vec<String>>,
    skill_dependencies: Vec<String>,
    avatar: RuntimeAssistantAvatar,
}

impl AssistantDefinition {
    fn runtime_source(&self) -> &'static str {
        "user"
    }
}

fn assistant_source_ref(ownership: AssistantDefinitionOwnership, projection_runtime_id: &str) -> String {
    match ownership {
        AssistantDefinitionOwnership::Local => format!("asset:{projection_runtime_id}"),
        AssistantDefinitionOwnership::Hub => format!("market:{projection_runtime_id}"),
    }
}

fn ensure_assistant_projection_ownership(
    existing: &AssistantDefinitionRow,
    expected_source_ref: &str,
    runtime_id: &str,
) -> Result<(), AssetError> {
    if existing.source_ref.as_deref() == Some(expected_source_ref) {
        return Ok(());
    }
    Err(AssetError::RuntimeProjectionUnsupported {
        code: "RUNTIME_ASSISTANT_OWNERSHIP_MISMATCH",
        message: format!("助手 {runtime_id} 不属于该市场资产"),
    })
}

#[derive(Debug)]
enum RuntimeAssistantAvatar {
    None,
    Emoji(String),
    File(String),
}

fn parse_assistant_definition(raw: &[u8]) -> Result<AssistantDefinition, AssetError> {
    let schema = serde_json::from_slice::<serde_json::Value>(raw)
        .ok()
        .and_then(|value| {
            value
                .get("$schema")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .ok_or_else(|| AssetError::RuntimeProjectionUnsupported {
            code: "RUNTIME_ASSISTANT_DEFINITION_INVALID",
            message: "助手 Definition 缺少 $schema".into(),
        })?;

    match schema.as_str() {
        LOCAL_ASSISTANT_SCHEMA => {
            let definition: LocalAssistantDefinition =
                serde_json::from_slice(raw).map_err(|error| AssetError::RuntimeProjectionUnsupported {
                    code: "RUNTIME_ASSISTANT_DEFINITION_INVALID",
                    message: format!("local-assistant-definition.v1 解析失败：{error}"),
                })?;
            Ok(AssistantDefinition {
                ownership: AssistantDefinitionOwnership::Local,
                schema_version: definition.schema_version,
                kind: definition.kind,
                runtime_id: definition.runtime_id,
                name: definition.name,
                name_i18n: definition.name_i18n,
                description: definition.description,
                description_i18n: definition.description_i18n,
                rules: definition.rules,
                recommended_prompts: definition.recommended_prompts,
                recommended_prompts_i18n: definition.recommended_prompts_i18n,
                skill_dependencies: definition
                    .skill_dependencies
                    .into_iter()
                    .map(|dependency| dependency.asset_id)
                    .collect(),
                avatar: match definition.avatar {
                    PortableAssistantAvatar::None => RuntimeAssistantAvatar::None,
                    PortableAssistantAvatar::Emoji { value } => RuntimeAssistantAvatar::Emoji(value),
                    PortableAssistantAvatar::File { path } => RuntimeAssistantAvatar::File(path),
                },
            })
        }
        HUB_ASSISTANT_SCHEMA => {
            let definition: HubAssistantDefinition =
                serde_json::from_slice(raw).map_err(|error| AssetError::RuntimeProjectionUnsupported {
                    code: "RUNTIME_ASSISTANT_DEFINITION_INVALID",
                    message: format!("assistant-definition.v1 解析失败：{error}"),
                })?;
            Ok(AssistantDefinition {
                ownership: AssistantDefinitionOwnership::Hub,
                schema_version: definition.schema_version,
                kind: definition.kind,
                runtime_id: definition.runtime_id,
                name: definition.name,
                name_i18n: definition.name_i18n,
                description: Some(definition.description),
                description_i18n: definition.description_i18n,
                rules: definition.rules,
                recommended_prompts: definition.recommended_prompts,
                recommended_prompts_i18n: definition.recommended_prompts_i18n,
                skill_dependencies: definition.skill_dependencies,
                avatar: match definition.avatar {
                    HubAssistantAvatar::Emoji { value } => RuntimeAssistantAvatar::Emoji(value),
                    HubAssistantAvatar::File { path } => RuntimeAssistantAvatar::File(path),
                },
            })
        }
        _ => Err(AssetError::RuntimeProjectionUnsupported {
            code: "RUNTIME_ASSISTANT_SCHEMA_UNSUPPORTED",
            message: format!("不支持的助手 Definition schema：{schema}"),
        }),
    }
}

#[derive(Debug, Deserialize)]
struct SkillFrontmatter {
    name: String,
    description: String,
}

fn definition_file<'a>(asset: &'a RuntimeAssetDefinition, path: &str) -> Result<&'a [u8], AssetError> {
    asset
        .files
        .iter()
        .find(|file| file.path == path)
        .map(|file| file.content.as_slice())
        .ok_or_else(|| AssetError::InvalidMetadata(format!("运行时入口文件不存在：{path}")))
}

fn validate_assistant_definition(
    definition: &AssistantDefinition,
    asset: &RuntimeAssetDefinition,
) -> Result<(), AssetError> {
    if definition.schema_version != 1
        || definition.kind != "assistant"
        || definition.runtime_id != asset.portable_runtime_id
        || definition.name.trim().is_empty()
        || definition.rules.is_empty()
    {
        return Err(AssetError::RuntimeProjectionUnsupported {
            code: "RUNTIME_ASSISTANT_DEFINITION_INVALID",
            message: "助手 Definition 的 schema、身份或必填字段无效".into(),
        });
    }
    match &definition.avatar {
        RuntimeAssistantAvatar::None => {}
        RuntimeAssistantAvatar::Emoji(value) if !value.trim().is_empty() => {}
        RuntimeAssistantAvatar::File(path) => {
            normalize_relative_path(path)?;
            definition_file(asset, path)?;
            validate_avatar_extension(path)?;
        }
        RuntimeAssistantAvatar::Emoji(_) => {
            return Err(AssetError::RuntimeProjectionUnsupported {
                code: "RUNTIME_ASSISTANT_AVATAR_UNSUPPORTED",
                message: "助手 emoji 头像不能为空".into(),
            });
        }
    }
    for path in definition.rules.values() {
        normalize_relative_path(path)?;
        definition_file(asset, path)?;
    }
    resolved_assistant_skill_ids(definition, asset)?;
    Ok(())
}

fn assistant_row_from_definition(
    definition: &AssistantDefinition,
    asset: &RuntimeAssetDefinition,
    existing: Option<&AssistantDefinitionRow>,
    source_ref: &str,
) -> Result<AssistantDefinitionRow, AssetError> {
    let id = existing
        .map(|row| row.id.clone())
        .unwrap_or_else(|| format!("asstdef_asset_{}", stable_identity(&asset.projection_runtime_id)));
    let name_i18n = serde_json::to_string(&definition.name_i18n)?;
    let description_i18n = serde_json::to_string(&definition.description_i18n)?;
    let recommended_prompts = serde_json::to_string(&definition.recommended_prompts)?;
    let recommended_prompts_i18n = serde_json::to_string(&definition.recommended_prompts_i18n)?;
    let default_skill_ids = serde_json::to_string(&resolved_assistant_skill_ids(definition, asset)?)?;
    Ok(AssistantDefinitionRow {
        id,
        assistant_id: asset.projection_runtime_id.clone(),
        source: definition.runtime_source().into(),
        owner_type: "user".into(),
        source_ref: Some(source_ref.into()),
        name: definition.name.clone(),
        name_i18n,
        description: definition.description.clone(),
        description_i18n,
        avatar_type: match &definition.avatar {
            RuntimeAssistantAvatar::None => "none".into(),
            RuntimeAssistantAvatar::Emoji(_) => "emoji".into(),
            RuntimeAssistantAvatar::File(_) => "user_asset".into(),
        },
        avatar_value: match &definition.avatar {
            RuntimeAssistantAvatar::None => None,
            RuntimeAssistantAvatar::Emoji(value) => Some(value.clone()),
            RuntimeAssistantAvatar::File(path) => Some(runtime_avatar_filename(&asset.projection_runtime_id, path)?),
        },
        agent_id: existing
            .map(|row| row.agent_id.clone())
            .unwrap_or_else(|| DEFAULT_TJUAECLI_AGENT_ID.into()),
        rule_resource_type: "user_file".into(),
        rule_resource_ref: Some(asset.projection_runtime_id.clone()),
        recommended_prompts,
        recommended_prompts_i18n,
        default_model_mode: existing
            .map(|row| row.default_model_mode.clone())
            .unwrap_or_else(|| "auto".into()),
        default_model_value: existing.and_then(|row| row.default_model_value.clone()),
        default_permission_mode: existing
            .map(|row| row.default_permission_mode.clone())
            .unwrap_or_else(|| "auto".into()),
        default_permission_value: existing.and_then(|row| row.default_permission_value.clone()),
        default_thought_level_mode: existing
            .map(|row| row.default_thought_level_mode.clone())
            .unwrap_or_else(|| "auto".into()),
        default_thought_level_value: existing.and_then(|row| row.default_thought_level_value.clone()),
        default_skills_mode: "fixed".into(),
        default_skill_ids,
        custom_skill_names: existing
            .map(|row| row.custom_skill_names.clone())
            .unwrap_or_else(|| "[]".into()),
        default_mcps_mode: existing
            .map(|row| row.default_mcps_mode.clone())
            .unwrap_or_else(|| "auto".into()),
        default_mcp_ids: existing
            .map(|row| row.default_mcp_ids.clone())
            .unwrap_or_else(|| "[]".into()),
        created_at: existing.map_or_else(now_ms, |row| row.created_at),
        updated_at: now_ms(),
        deleted_at: None,
    })
}

fn resolved_assistant_skill_ids(
    definition: &AssistantDefinition,
    asset: &RuntimeAssetDefinition,
) -> Result<Vec<String>, AssetError> {
    let mut seen_remote = BTreeSet::new();
    let mut seen_runtime = BTreeSet::new();
    let mut resolved = Vec::with_capacity(definition.skill_dependencies.len());
    for remote_asset_id in &definition.skill_dependencies {
        if remote_asset_id.is_empty()
            || remote_asset_id.len() > 256
            || remote_asset_id.contains('\\')
            || remote_asset_id.contains('\0')
            || !seen_remote.insert(remote_asset_id)
        {
            return Err(AssetError::RuntimeProjectionUnsupported {
                code: "RUNTIME_ASSISTANT_DEPENDENCY_INVALID",
                message: format!("助手 {} 包含无效或重复的技能资产依赖", definition.runtime_id),
            });
        }
        let runtime_id = asset
            .dependency_projection_runtime_ids
            .get(remote_asset_id)
            .ok_or_else(|| AssetError::RuntimeProjectionUnsupported {
                code: "RUNTIME_ASSISTANT_DEPENDENCY_UNRESOLVED",
                message: format!("固定市场索引没有解析技能资产依赖 {remote_asset_id}"),
            })?;
        validate_runtime_name(runtime_id, "技能 runtimeId")?;
        if !seen_runtime.insert(runtime_id) {
            return Err(AssetError::RuntimeProjectionUnsupported {
                code: "RUNTIME_ASSISTANT_DEPENDENCY_AMBIGUOUS",
                message: format!("多个远程技能资产映射到了同一 runtimeId {runtime_id}"),
            });
        }
        resolved.push(runtime_id.clone());
    }
    Ok(resolved)
}

fn assistant_rule_files(
    definition: &AssistantDefinition,
    asset: &RuntimeAssetDefinition,
) -> Result<BTreeMap<String, Vec<u8>>, AssetError> {
    let prefix = assistant_rule_prefix(&asset.projection_runtime_id);
    let mut files = BTreeMap::new();
    for (locale, path) in &definition.rules {
        validate_runtime_name(locale, "语言标识")?;
        files.insert(
            format!("{prefix}{}.md", encode_filename_component(locale)),
            definition_file(asset, path)?.to_vec(),
        );
    }
    let default_path = definition
        .rules
        .get("zh-CN")
        .or_else(|| definition.rules.get("en-US"))
        .or_else(|| definition.rules.values().next())
        .ok_or_else(|| AssetError::InvalidMetadata("助手规则不能为空".into()))?;
    files.insert(format!("{prefix}md"), definition_file(asset, default_path)?.to_vec());
    Ok(files)
}

fn assistant_avatar_files(
    definition: &AssistantDefinition,
    asset: &RuntimeAssetDefinition,
) -> Result<BTreeMap<String, Vec<u8>>, AssetError> {
    let RuntimeAssistantAvatar::File(path) = &definition.avatar else {
        return Ok(BTreeMap::new());
    };
    let filename = runtime_avatar_filename(&asset.projection_runtime_id, path)?;
    Ok(BTreeMap::from([(filename, definition_file(asset, path)?.to_vec())]))
}

fn runtime_avatar_filename(runtime_id: &str, definition_path: &str) -> Result<String, AssetError> {
    let extension = validate_avatar_extension(definition_path)?;
    Ok(format!("{}.{}", encode_filename_component(runtime_id), extension))
}

fn validate_avatar_extension(path: &str) -> Result<String, AssetError> {
    let extension = Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|value| matches!(value.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif" | "svg"))
        .ok_or_else(|| AssetError::RuntimeProjectionUnsupported {
            code: "RUNTIME_ASSISTANT_AVATAR_UNSUPPORTED",
            message: format!("助手头像文件类型不受支持：{path}"),
        })?;
    Ok(extension)
}

fn assistant_rule_prefix(assistant_id: &str) -> String {
    format!("{}.", encode_filename_component(assistant_id))
}

fn encode_filename_component(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[(byte >> 4) as usize]));
            encoded.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
    }
    encoded
}

fn parse_skill_frontmatter(bytes: &[u8]) -> Result<SkillFrontmatter, AssetError> {
    let content = std::str::from_utf8(bytes).map_err(|_| AssetError::RuntimeProjectionUnsupported {
        code: "RUNTIME_SKILL_DEFINITION_INVALID",
        message: "SKILL.md 必须是 UTF-8 文本".into(),
    })?;
    let mut lines = content.lines();
    if lines.next() != Some("---") {
        return Err(AssetError::RuntimeProjectionUnsupported {
            code: "RUNTIME_SKILL_DEFINITION_INVALID",
            message: "SKILL.md 缺少 YAML frontmatter".into(),
        });
    }
    let mut yaml = String::new();
    let mut closed = false;
    for line in lines {
        if line == "---" {
            closed = true;
            break;
        }
        yaml.push_str(line);
        yaml.push('\n');
    }
    if !closed {
        return Err(AssetError::RuntimeProjectionUnsupported {
            code: "RUNTIME_SKILL_DEFINITION_INVALID",
            message: "SKILL.md frontmatter 未闭合".into(),
        });
    }
    let metadata: SkillFrontmatter =
        serde_yaml::from_str(&yaml).map_err(|error| AssetError::RuntimeProjectionUnsupported {
            code: "RUNTIME_SKILL_DEFINITION_INVALID",
            message: format!("SKILL.md frontmatter 无效：{error}"),
        })?;
    validate_runtime_name(&metadata.name, "技能名称")?;
    if metadata.description.trim().is_empty() {
        return Err(AssetError::RuntimeProjectionUnsupported {
            code: "RUNTIME_SKILL_DEFINITION_INVALID",
            message: "SKILL.md description 不能为空".into(),
        });
    }
    Ok(metadata)
}

fn validate_bundle_scope(user_id: &str, assets: &[RuntimeAssetDefinition]) -> Result<(), AssetError> {
    validate_runtime_user_id(user_id)?;
    if assets.is_empty() {
        return Err(AssetError::BundleInvariant("运行时资产 Bundle 不能为空".into()));
    }
    let mut identities = BTreeSet::new();
    for asset in assets {
        validate_projection_runtime_id(&asset.projection_runtime_id)?;
        if !identities.insert((runtime_kind_name(asset.kind), asset.projection_runtime_id.as_str())) {
            return Err(AssetError::BundleInvariant("Bundle 包含重复运行时身份".into()));
        }
    }
    Ok(())
}

fn validate_assistant_asset(asset: &RuntimeAssetDefinition) -> Result<(), AssetError> {
    let definition = parse_assistant_definition(definition_file(asset, &asset.entry_file)?)?;
    validate_assistant_definition(&definition, asset)?;
    let Some(resolved) = asset.runtime_configuration.as_ref() else {
        return Ok(());
    };
    if !resolved.secrets.is_empty() {
        return Err(AssetError::RuntimeProjectionUnsupported {
            code: "RUNTIME_ASSISTANT_SECRET_UNSUPPORTED",
            message: "助手本机配置不接受凭据槽".into(),
        });
    }
    let AssetPublicConfiguration::Assistant(configuration) = &resolved.configuration else {
        return Err(AssetError::RuntimeProjectionUnsupported {
            code: "RUNTIME_CONFIGURATION_KIND_MISMATCH",
            message: "本机配置类型与 Assistant 资产不匹配".into(),
        });
    };
    if let Some(engine_asset_id) = configuration.engine_asset_id.as_deref() {
        let runtime_id = asset
            .dependency_projection_runtime_ids
            .get(engine_asset_id)
            .ok_or_else(|| AssetError::RuntimeProjectionUnsupported {
                code: "RUNTIME_ASSISTANT_ENGINE_UNRESOLVED",
                message: format!("助手引用的引擎资产 {engine_asset_id} 没有固定 runtimeId 映射"),
            })?;
        validate_runtime_name(runtime_id, "助手引擎 runtimeId")?;
    }
    Ok(())
}

fn validate_skill_asset(asset: &RuntimeAssetDefinition) -> Result<(), AssetError> {
    validate_runtime_name(&asset.portable_runtime_id, "技能 runtimeId")?;
    let metadata = parse_skill_frontmatter(definition_file(asset, &asset.entry_file)?)?;
    if metadata.name != asset.portable_runtime_id {
        return Err(AssetError::RuntimeProjectionUnsupported {
            code: "RUNTIME_SKILL_ID_MISMATCH",
            message: format!(
                "SKILL.md 名称 {} 与 runtimeId {} 不一致",
                metadata.name, asset.portable_runtime_id
            ),
        });
    }
    validate_skill_configuration(asset)
}

fn validate_skill_configuration(asset: &RuntimeAssetDefinition) -> Result<(), AssetError> {
    let Some(resolved) = asset.runtime_configuration.as_ref() else {
        return Ok(());
    };
    if !resolved.secrets.is_empty() {
        return Err(AssetError::RuntimeProjectionUnsupported {
            code: "RUNTIME_SKILL_SECRET_UNSUPPORTED",
            message: "技能本机配置不接受凭据槽".into(),
        });
    }
    let AssetPublicConfiguration::Skill(SkillAssetConfiguration {}) = &resolved.configuration else {
        return Err(AssetError::RuntimeProjectionUnsupported {
            code: "RUNTIME_CONFIGURATION_KIND_MISMATCH",
            message: "本机配置类型与 Skill 资产不匹配".into(),
        });
    };
    Ok(())
}

async fn upsert_assistant(
    repo: &dyn IAssistantDefinitionRepository,
    row: &AssistantDefinitionRow,
) -> Result<AssistantDefinitionRow, AssetError> {
    Ok(repo
        .upsert(&UpsertAssistantDefinitionParams {
            id: &row.id,
            assistant_id: &row.assistant_id,
            source: &row.source,
            owner_type: &row.owner_type,
            source_ref: row.source_ref.as_deref(),
            name: &row.name,
            name_i18n: &row.name_i18n,
            description: row.description.as_deref(),
            description_i18n: &row.description_i18n,
            avatar_type: &row.avatar_type,
            avatar_value: row.avatar_value.as_deref(),
            agent_id: &row.agent_id,
            rule_resource_type: &row.rule_resource_type,
            rule_resource_ref: row.rule_resource_ref.as_deref(),
            recommended_prompts: &row.recommended_prompts,
            recommended_prompts_i18n: &row.recommended_prompts_i18n,
            default_model_mode: &row.default_model_mode,
            default_model_value: row.default_model_value.as_deref(),
            default_permission_mode: &row.default_permission_mode,
            default_permission_value: row.default_permission_value.as_deref(),
            default_thought_level_mode: &row.default_thought_level_mode,
            default_thought_level_value: row.default_thought_level_value.as_deref(),
            default_skills_mode: &row.default_skills_mode,
            default_skill_ids: &row.default_skill_ids,
            custom_skill_names: &row.custom_skill_names,
            default_mcps_mode: &row.default_mcps_mode,
            default_mcp_ids: &row.default_mcp_ids,
        })
        .await?)
}

fn write_definition_tree(root: &Path, files: &[AssetDefinitionFile]) -> Result<(), AssetError> {
    std::fs::create_dir(root)?;
    for file in files {
        let normalized = normalize_relative_path(&file.path)?;
        let target = root.join(&normalized);
        if !target.starts_with(root) || target == root {
            return Err(AssetError::UnsafePath(file.path.clone()));
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut options = std::fs::OpenOptions::new();
        use std::io::Write;
        let mut output = options.create_new(true).write(true).open(target)?;
        output.write_all(&file.content)?;
        output.sync_all()?;
    }
    Ok(())
}

fn ensure_direct_child(root: &Path, candidate: &Path) -> Result<(), AssetError> {
    if candidate.parent() != Some(root) || candidate == root {
        return Err(AssetError::UnsafePath(candidate.display().to_string()));
    }
    Ok(())
}

pub(super) fn validate_runtime_name(value: &str, field: &str) -> Result<(), AssetError> {
    if value.is_empty()
        || value.len() > 128
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || value.contains('\0')
    {
        return Err(AssetError::InvalidMetadata(format!("{field}不安全")));
    }
    Ok(())
}

fn validate_runtime_user_id(user_id: &str) -> Result<(), AssetError> {
    if user_id.trim().is_empty() || user_id.contains('\0') {
        return Err(AssetError::InvalidMetadata("运行时用户身份无效".into()));
    }
    Ok(())
}

fn validate_projection_runtime_id(value: &str) -> Result<(), AssetError> {
    if !is_projection_runtime_id(value) {
        return Err(AssetError::InvalidMetadata("Core 内部投影身份无效".into()));
    }
    Ok(())
}

fn runtime_kind_name(kind: AssetKind) -> &'static str {
    match kind {
        AssetKind::Assistant => "assistant",
        AssetKind::EngineAdapter => "engineAdapter",
        AssetKind::Skill => "skill",
        AssetKind::Mcp => "mcp",
    }
}

fn parse_string_list(value: &str) -> Result<Vec<String>, AssetError> {
    serde_json::from_str(value).map_err(|error| AssetError::InvalidMetadata(format!("运行时依赖列表无效：{error}")))
}

pub(super) fn stable_identity(value: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(value.as_bytes()))[..24].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_action(
        name: &'static str,
        events: &Arc<std::sync::Mutex<Vec<String>>>,
        fail_apply: bool,
    ) -> ProjectionAction {
        ProjectionAction::Test(TestProjection {
            name,
            events: Arc::clone(events),
            fail_apply,
            fail_rollback: false,
            applied: false,
        })
    }

    fn test_action_with_rollback_failure(
        name: &'static str,
        events: &Arc<std::sync::Mutex<Vec<String>>>,
    ) -> ProjectionAction {
        ProjectionAction::Test(TestProjection {
            name,
            events: Arc::clone(events),
            fail_apply: false,
            fail_rollback: true,
            applied: false,
        })
    }

    fn hub_assistant_projection() -> AssistantDefinitionRow {
        let raw = serde_json::json!({
            "$schema": HUB_ASSISTANT_SCHEMA,
            "schemaVersion": 1,
            "kind": "assistant",
            "runtimeId": "demo",
            "name": "Demo",
            "description": "Demo assistant",
            "rules": {"zh-CN": "rules/zh-CN.md"},
            "avatar": {"type": "emoji", "value": "🤖"}
        });
        let definition = parse_assistant_definition(serde_json::to_vec(&raw).unwrap().as_slice()).unwrap();
        let asset = RuntimeAssetDefinition {
            local_asset_id: "local-hub-assistant".into(),
            kind: AssetKind::Assistant,
            portable_runtime_id: "demo".into(),
            projection_runtime_id: format!("tjuae-proj-v1-{}", "0".repeat(64)),
            entry_file: "assistant.json".into(),
            workspace_path: PathBuf::from("assistants/demo"),
            files: Vec::new(),
            dependency_portable_runtime_ids: BTreeMap::new(),
            dependency_projection_runtime_ids: BTreeMap::new(),
            runtime_configuration: None,
        };
        let source_ref = assistant_source_ref(definition.ownership, &asset.projection_runtime_id);
        assistant_row_from_definition(&definition, &asset, None, &source_ref).unwrap()
    }

    #[test]
    fn hub_assistant_projects_as_user_with_market_source_ref() {
        let row = hub_assistant_projection();
        assert_eq!(row.source, "user");
        assert_eq!(
            row.source_ref.as_deref(),
            Some(format!("market:tjuae-proj-v1-{}", "0".repeat(64)).as_str())
        );
    }

    #[test]
    fn assistant_remove_rejects_legacy_builtin_without_matching_source_ref() {
        let mut row = hub_assistant_projection();
        row.source = "builtin".into();
        row.source_ref = Some("legacy-builtin".into());
        let error = ensure_assistant_projection_ownership(&row, "market:wrong-projection", "demo").unwrap_err();
        assert!(matches!(
            error,
            AssetError::RuntimeProjectionUnsupported {
                code: "RUNTIME_ASSISTANT_OWNERSHIP_MISMATCH",
                ..
            }
        ));
    }

    #[test]
    fn strict_assistant_definition_rejects_overlay_fields() {
        let raw = serde_json::json!({
            "$schema": HUB_ASSISTANT_SCHEMA,
            "schemaVersion": 1,
            "kind": "assistant",
            "runtimeId": "demo",
            "name": "Demo",
            "rules": {"zh-CN": "rules/zh-CN.md"},
            "defaultModel": "secret-local-choice"
        });
        assert!(parse_assistant_definition(serde_json::to_vec(&raw).unwrap().as_slice()).is_err());
    }

    #[test]
    fn skill_frontmatter_requires_matching_portable_identity() {
        let parsed = parse_skill_frontmatter(b"---\nname: demo\ndescription: test\n---\nBody").unwrap();
        assert_eq!(parsed.name, "demo");
        assert!(parse_skill_frontmatter(b"# no frontmatter").is_err());
    }

    #[test]
    fn encoded_rule_names_cannot_escape_runtime_directory() {
        assert_eq!(encode_filename_component("demo:one"), "demo%3Aone");
        assert!(!encode_filename_component("../demo").contains('/'));
    }

    #[test]
    fn assistant_rule_activation_compensates_mid_rename_failure() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("assistant-rules");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("demo.md"), "old-default").unwrap();
        std::fs::write(root.join("demo.zh-CN.md"), "old-zh").unwrap();
        let mut projection = FileSetProjection::new(
            root.clone(),
            "demo.".into(),
            BTreeMap::from([
                ("demo.md".into(), b"new-default".to_vec()),
                ("demo.en-US.md".into(), b"new-en".to_vec()),
            ]),
        );
        // Two staging writes succeed, then one old file is renamed before the
        // injected failure. apply() must restore that partial visible change.
        projection.fail_after_operations = Some(3);
        assert!(projection.apply().is_err());
        assert_eq!(std::fs::read_to_string(root.join("demo.md")).unwrap(), "old-default");
        assert_eq!(std::fs::read_to_string(root.join("demo.zh-CN.md")).unwrap(), "old-zh");
        assert!(!root.join("demo.en-US.md").exists());
        assert!(!projection.backup_dir.exists());
    }

    #[test]
    fn assistant_rule_staging_write_failure_has_zero_visible_change() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("assistant-rules");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("demo.md"), "old").unwrap();
        let mut projection = FileSetProjection::new(
            root.clone(),
            "demo.".into(),
            BTreeMap::from([("demo.md".into(), b"new".to_vec())]),
        );
        projection.fail_after_operations = Some(0);
        assert!(projection.apply().is_err());
        assert_eq!(std::fs::read_to_string(root.join("demo.md")).unwrap(), "old");
        assert!(!projection.backup_dir.exists());
    }

    #[test]
    fn directory_rollback_preserves_recovery_material_when_windows_rename_is_blocked() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("demo");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("SKILL.md"), "old").unwrap();
        let mut projection = DirectoryProjection::new(
            target.clone(),
            vec![AssetDefinitionFile::text("SKILL.md", "new")],
            ProjectionMode::Replace,
        )
        .unwrap();
        projection.apply().unwrap();
        projection.fail_rollback_target_rename = true;
        let error = projection.rollback().unwrap_err();
        assert!(matches!(
            error,
            AssetError::RuntimeProjectionFailed {
                code: "RUNTIME_DIRECTORY_ROLLBACK_BLOCKED",
                ..
            }
        ));
        assert_eq!(std::fs::read_to_string(target.join("SKILL.md")).unwrap(), "new");
        assert_eq!(
            std::fs::read_to_string(projection.backup.join("SKILL.md")).unwrap(),
            "old"
        );

        projection.fail_rollback_target_rename = false;
        projection.rollback().unwrap();
        assert_eq!(std::fs::read_to_string(target.join("SKILL.md")).unwrap(), "old");
    }

    #[tokio::test]
    async fn mixed_bundle_failure_rolls_back_completed_actions_in_reverse_order() {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut transaction = CoreProjectionTransaction {
            actions: vec![
                test_action("engine", &events, false),
                test_action("skill", &events, false),
                test_action("assistant", &events, true),
            ],
            applied: 0,
            finalized: false,
        };

        assert!(transaction.apply().await.is_err());
        assert_eq!(
            *events.lock().unwrap(),
            ["apply:engine", "apply:skill", "apply:assistant"]
        );
        assert_eq!(transaction.applied, 2);
        transaction.rollback().await.unwrap();
        assert_eq!(
            *events.lock().unwrap(),
            [
                "apply:engine",
                "apply:skill",
                "apply:assistant",
                "rollback:skill",
                "rollback:engine",
            ]
        );
        assert_eq!(transaction.applied, 0);
    }

    #[test]
    fn mixed_bundle_uses_dependency_order_for_replace_and_inverse_order_for_remove() {
        let mut replace = vec![
            AssetKind::Assistant,
            AssetKind::Mcp,
            AssetKind::EngineAdapter,
            AssetKind::Skill,
        ];
        replace.sort_by_key(|kind| projection_topology_rank(ProjectionMode::Replace, *kind));
        assert_eq!(
            replace,
            [
                AssetKind::Mcp,
                AssetKind::EngineAdapter,
                AssetKind::Skill,
                AssetKind::Assistant,
            ]
        );

        let mut remove = replace.clone();
        remove.sort_by_key(|kind| projection_topology_rank(ProjectionMode::Remove, *kind));
        assert_eq!(
            remove,
            [
                AssetKind::Assistant,
                AssetKind::Mcp,
                AssetKind::Skill,
                AssetKind::EngineAdapter,
            ]
        );
    }

    #[tokio::test]
    async fn bundle_rollback_reports_failure_but_still_compensates_earlier_actions() {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut transaction = CoreProjectionTransaction {
            actions: vec![
                test_action("engine", &events, false),
                test_action_with_rollback_failure("mcp", &events),
                test_action("skill", &events, false),
            ],
            applied: 0,
            finalized: false,
        };

        transaction.apply().await.unwrap();
        let error = transaction.rollback().await.unwrap_err();
        assert!(matches!(
            error,
            AssetError::RuntimeProjectionFailed {
                code: "TEST_RUNTIME_ROLLBACK_FAILED",
                ..
            }
        ));
        assert_eq!(
            *events.lock().unwrap(),
            [
                "apply:engine",
                "apply:mcp",
                "apply:skill",
                "rollback:skill",
                "rollback:mcp",
                "rollback:engine",
            ]
        );
        assert_eq!(transaction.applied, 0);
    }

    #[tokio::test]
    async fn successful_bundle_finalizes_every_action_once() {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut transaction = CoreProjectionTransaction {
            actions: vec![
                test_action("engine", &events, false),
                test_action("mcp", &events, false),
            ],
            applied: 0,
            finalized: false,
        };
        transaction.apply().await.unwrap();
        Box::new(transaction).finalize().await;
        assert_eq!(
            *events.lock().unwrap(),
            ["apply:engine", "apply:mcp", "finalize:engine", "finalize:mcp"]
        );
    }
}
