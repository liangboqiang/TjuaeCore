use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use tjuaeui_api_types::{
    ActivateAssistantRequest, AgentManagementStatus, AssistantActivationAction, AssistantActivationCandidateResponse,
    AssistantActivationGroupResponse, AssistantActivationItemResponse, AssistantActivationPlanResponse,
    AssistantActivationStatus, AssistantDefaultRef, AssistantIdentityResponse, AssistantOperationResponse,
    AssistantRequirementKind, AssistantRequirementResponse, ExportAssistantRequest, ExportAssistantResponse,
    PublishAssistantCatalogRequest, PublishAssistantCatalogResponse,
};
use tjuaeui_db::{IMcpServerRepository, IProviderRepository, ISkillUserPreferenceRepository};
use tokio::sync::RwLock;

use crate::{AssistantAgentCatalogPort, AssistantCatalogService, AssistantError};

#[derive(Clone)]
pub struct AssistantActivationService {
    pool: SqlitePool,
    catalog: Arc<AssistantCatalogService>,
    skill_preferences: Arc<dyn ISkillUserPreferenceRepository>,
    mcp_servers: Arc<dyn IMcpServerRepository>,
    providers: Arc<dyn IProviderRepository>,
    agents: Arc<dyn AssistantAgentCatalogPort>,
    prepared: Arc<RwLock<HashMap<String, PreparedPlan>>>,
}

#[derive(Clone)]
struct PreparedPlan {
    response: AssistantActivationPlanResponse,
    requirements: Vec<AssistantRequirementResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PortableResourceIndex {
    format: String,
    format_version: u32,
    resources: Vec<PortableResourceRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PortableResourceRecord {
    kind: AssistantRequirementKind,
    id: String,
    #[serde(default)]
    aliases: Vec<String>,
    path: String,
    sha256: String,
    configuration_required: bool,
}

impl AssistantActivationService {
    pub fn new(
        pool: SqlitePool,
        catalog: Arc<AssistantCatalogService>,
        skill_preferences: Arc<dyn ISkillUserPreferenceRepository>,
        mcp_servers: Arc<dyn IMcpServerRepository>,
        providers: Arc<dyn IProviderRepository>,
        agents: Arc<dyn AssistantAgentCatalogPort>,
    ) -> Self {
        Self {
            pool,
            catalog,
            skill_preferences,
            mcp_servers,
            providers,
            agents,
            prepared: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 使用与发布完全相同的复合资源规则导出助手包。
    pub async fn export(
        &self,
        identity: &AssistantIdentityResponse,
        request: ExportAssistantRequest,
    ) -> Result<ExportAssistantResponse, AssistantError> {
        let files = self
            .portable_resource_files(identity, request.version.as_deref())
            .await?;
        self.catalog.export(identity, request, files).await
    }

    /// 发布前先刷新助手目录中的可移植资源，确保 Hub 包不是只有引用的空壳。
    pub async fn publish(
        &self,
        identity: &AssistantIdentityResponse,
        request: PublishAssistantCatalogRequest,
    ) -> Result<PublishAssistantCatalogResponse, AssistantError> {
        let files = self.portable_resource_files(identity, None).await?;
        self.catalog.replace_embedded_resources(identity, files).await?;
        self.catalog.publish_hub(identity, request).await
    }

    async fn portable_resource_files(
        &self,
        identity: &AssistantIdentityResponse,
        version: Option<&str>,
    ) -> Result<Vec<(String, Vec<u8>)>, AssistantError> {
        let detail = self.catalog.detail(identity, version).await?;
        let mut skill_refs = detail.manifest.defaults.skills.clone();
        skill_refs.extend(
            detail
                .manifest
                .requirements
                .iter()
                .filter_map(|requirement| requirement.identity.clone()),
        );
        let mut seen_skills = BTreeSet::new();
        skill_refs
            .retain(|skill| seen_skills.insert((skill.source.clone(), skill.namespace.clone(), skill.slug.clone())));

        let mut files = self.catalog.portable_skill_files(&skill_refs).await?;
        let mut records = portable_skill_records(&skill_refs, &files);

        let mut mcp_ids = detail.manifest.defaults.mcps.iter().cloned().collect::<BTreeSet<_>>();
        let mut model_ids = BTreeSet::new();
        let mut agent_ids = detail.manifest.defaults.agent.iter().cloned().collect::<BTreeSet<_>>();
        if detail.manifest.defaults.model.mode != "auto"
            && let Some(model) = &detail.manifest.defaults.model.value
        {
            model_ids.insert(model.clone());
        }
        for requirement in &detail.manifest.requirements {
            match requirement.kind {
                AssistantRequirementKind::Mcp => mcp_ids.extend(requirement.preferred_ids.iter().cloned()),
                AssistantRequirementKind::Model => model_ids.extend(requirement.preferred_ids.iter().cloned()),
                AssistantRequirementKind::Agent => agent_ids.extend(requirement.preferred_ids.iter().cloned()),
                AssistantRequirementKind::Skill => {}
            }
        }

        for server in self.mcp_servers.list().await? {
            if !mcp_ids.contains(&server.id) && !mcp_ids.contains(&server.name) {
                continue;
            }
            let descriptor_id = server.id.clone();
            let transport_config = serde_json::from_str::<serde_json::Value>(&server.transport_config)
                .unwrap_or_else(|_| serde_json::json!({}));
            let descriptor = serde_json::json!({
                "format": "tjuae-mcp-resource",
                "formatVersion": 1,
                "id": server.id,
                "name": server.name,
                "description": server.description,
                "transportType": server.transport_type,
                "transportConfig": sanitize_portable_json(transport_config, None),
                "configurationRequired": true,
            });
            push_descriptor(
                AssistantRequirementKind::Mcp,
                &descriptor_id,
                vec![server.name],
                descriptor,
                true,
                &mut files,
                &mut records,
            )?;
        }

        for provider in self.providers.list().await? {
            let models = serde_json::from_str::<Vec<String>>(&provider.models).unwrap_or_default();
            for model in models.into_iter().filter(|model| model_ids.contains(model)) {
                let descriptor_id = model.clone();
                let descriptor = serde_json::json!({
                    "format": "tjuae-model-resource",
                    "formatVersion": 1,
                    "id": descriptor_id,
                    "provider": {
                        "id": provider.id,
                        "platform": provider.platform,
                        "name": provider.name,
                        "baseUrl": provider.base_url,
                        "isFullUrl": provider.is_full_url,
                    },
                    "model": model,
                    "capabilities": parse_json_or_default(&provider.capabilities, serde_json::json!([])),
                    "contextLimit": provider.context_limit,
                    "protocols": provider.model_protocols.as_deref().map(|value| parse_json_or_default(value, serde_json::json!({}))),
                    "settings": sanitize_portable_json(
                        parse_json_or_default(&provider.model_settings, serde_json::json!({})),
                        None,
                    ),
                    "credentialRequired": true,
                    "configurationRequired": true,
                });
                push_descriptor(
                    AssistantRequirementKind::Model,
                    &descriptor_id,
                    vec![],
                    descriptor,
                    true,
                    &mut files,
                    &mut records,
                )?;
            }
        }

        for agent in self.agents.list_management_agents().await? {
            let agent_type = agent.agent_type.serde_name().to_owned();
            let matches = agent_ids.iter().any(|candidate| {
                candidate == &agent.id
                    || agent.backend.as_deref() == Some(candidate.as_str())
                    || candidate == &agent_type
                    || agent.name.eq_ignore_ascii_case(candidate)
            });
            if !matches {
                continue;
            }
            let descriptor_id = agent.id.clone();
            let command = agent.command.as_deref().map(portable_command_name);
            let descriptor = serde_json::json!({
                "format": "tjuae-agent-resource",
                "formatVersion": 1,
                "id": agent.id,
                "name": agent.name,
                "description": agent.description,
                "agentType": agent_type,
                "source": serde_json::to_value(agent.agent_source).unwrap_or(serde_json::Value::Null),
                "backend": agent.backend,
                "command": command,
                "args": agent.args.into_iter().map(|value| portable_string(&value)).collect::<Vec<_>>(),
                "configurationRequired": true,
                "executableIncluded": false,
            });
            push_descriptor(
                AssistantRequirementKind::Agent,
                &descriptor_id,
                [agent.backend, Some(agent_type)].into_iter().flatten().collect(),
                descriptor,
                true,
                &mut files,
                &mut records,
            )?;
        }

        records.sort_by(|left, right| (left.kind, &left.id).cmp(&(right.kind, &right.id)));
        let index = PortableResourceIndex {
            format: "tjuae-assistant-resources".to_owned(),
            format_version: 1,
            resources: records,
        };
        files.retain(|(path, _)| path != "resources/_index.json");
        files.push((
            "resources/_index.json".to_owned(),
            serde_json::to_vec_pretty(&index).map_err(|error| AssistantError::Internal(error.to_string()))?,
        ));
        Ok(files)
    }

    pub async fn prepare(
        &self,
        identity: AssistantIdentityResponse,
        version: Option<&str>,
    ) -> Result<AssistantActivationPlanResponse, AssistantError> {
        let detail = self.catalog.detail(&identity, version).await?;
        let version = version.unwrap_or(&detail.item.latest_version).to_owned();
        let requirements = detail.manifest.requirements.clone();
        let mut groups = vec![
            self.resolve_skills(&requirements).await?,
            self.resolve_mcps(&requirements).await?,
            self.resolve_models(&requirements).await?,
            self.resolve_agents(&requirements).await?,
        ];
        if detail.files.iter().any(|file| file.path == "resources/_index.json") {
            let bytes = self
                .catalog
                .asset_bytes(&identity, Some(&version), "resources/_index.json")
                .await?;
            let index = serde_json::from_slice::<PortableResourceIndex>(&bytes)
                .map_err(|error| AssistantError::BadRequest(format!("助手嵌入资源索引无效：{error}")))?;
            annotate_embedded_resources(&mut groups, &requirements, &index);
        }
        let fingerprint = activation_fingerprint(&identity, &version, &detail.manifest, &groups)?;
        let plan_id = uuid::Uuid::now_v7().to_string();
        let response = AssistantActivationPlanResponse {
            plan_id: plan_id.clone(),
            fingerprint,
            identity,
            version,
            ready_without_changes: groups.iter().all(|group| !group.requires_confirmation),
            groups,
        };
        self.prepared.write().await.insert(
            plan_id,
            PreparedPlan {
                response: response.clone(),
                requirements,
            },
        );
        Ok(response)
    }

    pub async fn activate(
        &self,
        identity: AssistantIdentityResponse,
        request: ActivateAssistantRequest,
    ) -> Result<AssistantOperationResponse, AssistantError> {
        let prepared = self
            .prepared
            .read()
            .await
            .get(&request.plan_id)
            .cloned()
            .ok_or_else(|| AssistantError::Conflict("激活计划已失效，请重新检查".to_owned()))?;
        if prepared.response.identity != identity || prepared.response.fingerprint != request.fingerprint {
            return Err(AssistantError::Conflict("激活计划与当前助手不匹配".to_owned()));
        }
        let refreshed = self.prepare(identity.clone(), Some(&prepared.response.version)).await?;
        self.prepared.write().await.remove(&refreshed.plan_id);
        if refreshed.fingerprint != prepared.response.fingerprint {
            self.prepared.write().await.remove(&request.plan_id);
            return Err(AssistantError::Conflict("本地资源状态已变化，请重新确认".to_owned()));
        }

        let confirmed = request.confirmed_groups.iter().copied().collect::<BTreeSet<_>>();
        let choices = request
            .choices
            .iter()
            .map(|choice| (choice.requirement_key.as_str(), choice))
            .collect::<HashMap<_, _>>();
        let mut bindings = BTreeMap::<String, serde_json::Value>::new();
        let requirements = prepared
            .requirements
            .iter()
            .map(|requirement| (requirement.key.as_str(), requirement))
            .collect::<HashMap<_, _>>();

        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|error| AssistantError::Internal(error.to_string()))?;
        for group in &refreshed.groups {
            if group.requires_confirmation && !confirmed.contains(&group.kind) {
                return Err(AssistantError::BadRequest(format!("尚未确认 {:?} 资源", group.kind)));
            }
            for item in &group.items {
                if item.status == AssistantActivationStatus::Ready {
                    if let Some(resource_id) = &item.current_resource_id {
                        bindings.insert(
                            item.requirement_key.clone(),
                            binding_value(group.kind, AssistantActivationAction::Keep, Some(resource_id)),
                        );
                    }
                    continue;
                }
                let choice = choices
                    .get(item.requirement_key.as_str())
                    .ok_or_else(|| AssistantError::BadRequest(format!("资源 {} 尚未选择处理方式", item.label)))?;
                if !item.allowed_actions.contains(&choice.action) {
                    return Err(AssistantError::BadRequest(format!(
                        "资源 {} 不允许该处理方式",
                        item.label
                    )));
                }
                let requirement = requirements
                    .get(item.requirement_key.as_str())
                    .ok_or_else(|| AssistantError::Internal("激活要求丢失".to_owned()))?;
                let declared_skill_id = (requirement.kind == AssistantRequirementKind::Skill)
                    .then(|| {
                        requirement
                            .identity
                            .as_ref()
                            .map(|skill| format!("{}:{}:{}", skill.source, skill.namespace, skill.slug))
                    })
                    .flatten();
                let mut resource_id = choice
                    .resource_id
                    .as_deref()
                    .or(item.current_resource_id.as_deref())
                    .or(declared_skill_id.as_deref());
                if choice.action == AssistantActivationAction::UseDefault
                    && requirement.kind == AssistantRequirementKind::Agent
                {
                    resource_id = item
                        .candidates
                        .iter()
                        .find(|candidate| candidate.enabled && candidate.available)
                        .map(|candidate| candidate.id.as_str());
                }
                self.apply_choice(&mut transaction, requirement, item, choice.action, resource_id)
                    .await?;
                bindings.insert(
                    item.requirement_key.clone(),
                    binding_value(group.kind, choice.action, resource_id),
                );
            }
        }

        let bindings = serde_json::to_string(&bindings).map_err(|error| AssistantError::Internal(error.to_string()))?;
        let source = source_id(identity.source);
        let now = tjuaeui_common::now_ms();
        sqlx::query(
            "INSERT INTO assistant_user_preferences \
             (source, namespace, slug, selected_version, follow_latest, enabled, activation_status, \
              activation_fingerprint, resource_bindings, runtime_overrides, sort_order, last_used_at, updated_at) \
             VALUES (?, ?, ?, ?, 0, 1, 'ready', ?, ?, '{}', 0, NULL, ?) \
             ON CONFLICT(source, namespace, slug) DO UPDATE SET \
             selected_version = excluded.selected_version, follow_latest = 0, enabled = 1, activation_status = 'ready', \
             activation_fingerprint = excluded.activation_fingerprint, resource_bindings = excluded.resource_bindings, \
             updated_at = excluded.updated_at",
        )
        .bind(source)
        .bind(&identity.namespace)
        .bind(&identity.slug)
        .bind(&refreshed.version)
        .bind(&refreshed.fingerprint)
        .bind(&bindings)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AssistantError::Internal(error.to_string()))?;
        transaction
            .commit()
            .await
            .map_err(|error| AssistantError::Internal(error.to_string()))?;
        self.prepared.write().await.remove(&request.plan_id);
        Ok(AssistantOperationResponse {
            identity,
            version: refreshed.version,
            enabled: true,
            activation_status: "ready".to_owned(),
        })
    }

    async fn apply_choice(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        requirement: &AssistantRequirementResponse,
        item: &AssistantActivationItemResponse,
        action: AssistantActivationAction,
        resource_id: Option<&str>,
    ) -> Result<(), AssistantError> {
        match requirement.kind {
            AssistantRequirementKind::Skill => match action {
                AssistantActivationAction::Enable | AssistantActivationAction::Import => {
                    let skill = requirement
                        .identity
                        .as_ref()
                        .ok_or_else(|| AssistantError::Internal("技能要求缺少身份".to_owned()))?;
                    sqlx::query(
                        "INSERT INTO skill_user_preferences \
                         (source, namespace, slug, selected_version, follow_latest, enabled, auto_inject, updated_at) \
                         VALUES (?, ?, ?, ?, ?, 1, 0, ?) \
                         ON CONFLICT(source, namespace, slug) DO UPDATE SET \
                         selected_version = excluded.selected_version, follow_latest = excluded.follow_latest, \
                         enabled = 1, updated_at = excluded.updated_at",
                    )
                    .bind(&skill.source)
                    .bind(&skill.namespace)
                    .bind(&skill.slug)
                    .bind(requirement.version_requirement.as_deref())
                    .bind(requirement.version_requirement.is_none())
                    .bind(tjuaeui_common::now_ms())
                    .execute(&mut **transaction)
                    .await
                    .map_err(|error| AssistantError::Internal(error.to_string()))?;
                }
                AssistantActivationAction::Skip if !requirement.required => {}
                _ => return Err(AssistantError::BadRequest(format!("技能 {} 不能完成激活", item.label))),
            },
            AssistantRequirementKind::Mcp => match action {
                AssistantActivationAction::Enable => {
                    let id = require_resource_id(item, resource_id)?;
                    let affected = sqlx::query(
                        "UPDATE mcp_servers SET enabled = 1, updated_at = ? WHERE id = ? AND deleted_at IS NULL",
                    )
                    .bind(tjuaeui_common::now_ms())
                    .bind(id)
                    .execute(&mut **transaction)
                    .await
                    .map_err(|error| AssistantError::Internal(error.to_string()))?
                    .rows_affected();
                    if affected != 1 {
                        return Err(AssistantError::Conflict(format!("MCP {} 已不存在", item.label)));
                    }
                }
                AssistantActivationAction::Select | AssistantActivationAction::Configure => {
                    let id = require_resource_id(item, resource_id)?;
                    require_selectable_candidate(item, id)?;
                    let enabled: Option<bool> =
                        sqlx::query_scalar("SELECT enabled FROM mcp_servers WHERE id = ? AND deleted_at IS NULL")
                            .bind(id)
                            .fetch_optional(&mut **transaction)
                            .await
                            .map_err(|error| AssistantError::Internal(error.to_string()))?;
                    if enabled != Some(true) {
                        return Err(AssistantError::Conflict(format!(
                            "请先完成并启用 MCP {} 的配置",
                            item.label
                        )));
                    }
                }
                AssistantActivationAction::UseDefault | AssistantActivationAction::Skip if !requirement.required => {}
                _ => return Err(AssistantError::BadRequest(format!("MCP {} 不能完成激活", item.label))),
            },
            AssistantRequirementKind::Model => match action {
                AssistantActivationAction::Select => {
                    let id = require_resource_id(item, resource_id)?;
                    require_selectable_candidate(item, id)?;
                }
                AssistantActivationAction::UseDefault => {}
                AssistantActivationAction::Skip if !requirement.required => {}
                _ => return Err(AssistantError::BadRequest(format!("资源 {} 不能完成激活", item.label))),
            },
            AssistantRequirementKind::Agent => match action {
                AssistantActivationAction::Select | AssistantActivationAction::UseDefault => {
                    let id = require_resource_id(item, resource_id)?;
                    require_selectable_candidate(item, id)?;
                }
                AssistantActivationAction::Skip if !requirement.required => {}
                _ => return Err(AssistantError::BadRequest(format!("资源 {} 不能完成激活", item.label))),
            },
        }
        Ok(())
    }

    async fn resolve_skills(
        &self,
        requirements: &[AssistantRequirementResponse],
    ) -> Result<AssistantActivationGroupResponse, AssistantError> {
        let preferences = self.skill_preferences.list().await?;
        let mut items = Vec::new();
        for requirement in requirements
            .iter()
            .filter(|item| item.kind == AssistantRequirementKind::Skill)
        {
            let identity = requirement
                .identity
                .as_ref()
                .ok_or_else(|| AssistantError::BadRequest("技能要求缺少身份".to_owned()))?;
            let current = preferences.iter().find(|row| {
                row.source == identity.source && row.namespace == identity.namespace && row.slug == identity.slug
            });
            let version_conflict = current.is_some_and(|row| {
                requirement
                    .version_requirement
                    .as_ref()
                    .is_some_and(|required| row.selected_version.as_deref() != Some(required.as_str()))
            });
            let (status, message, allowed_actions, current_resource_id) = match current {
                Some(row) if version_conflict => (
                    AssistantActivationStatus::VersionConflict,
                    format!(
                        "技能版本不匹配，需要版本 {}",
                        requirement.version_requirement.as_deref().unwrap_or_default()
                    ),
                    with_optional(vec![AssistantActivationAction::Import], requirement.required),
                    Some(format!("{}:{}:{}", row.source, row.namespace, row.slug)),
                ),
                Some(row) if row.enabled => (
                    AssistantActivationStatus::Ready,
                    "技能已启用".to_owned(),
                    vec![AssistantActivationAction::Keep],
                    Some(format!("{}:{}:{}", row.source, row.namespace, row.slug)),
                ),
                Some(row) => (
                    AssistantActivationStatus::Disabled,
                    "技能存在但尚未启用".to_owned(),
                    with_optional(vec![AssistantActivationAction::Enable], requirement.required),
                    Some(format!("{}:{}:{}", row.source, row.namespace, row.slug)),
                ),
                None => (
                    AssistantActivationStatus::Missing,
                    "本地尚未准备该技能".to_owned(),
                    with_optional(vec![AssistantActivationAction::Import], requirement.required),
                    None,
                ),
            };
            items.push(AssistantActivationItemResponse {
                requirement_key: requirement.key.clone(),
                label: requirement.label.clone(),
                required: requirement.required,
                status,
                message,
                allowed_actions,
                candidates: Vec::new(),
                current_resource_id,
            });
        }
        Ok(group(AssistantRequirementKind::Skill, items))
    }

    async fn resolve_mcps(
        &self,
        requirements: &[AssistantRequirementResponse],
    ) -> Result<AssistantActivationGroupResponse, AssistantError> {
        let servers = self.mcp_servers.list().await?;
        let candidates = servers
            .iter()
            .map(|server| AssistantActivationCandidateResponse {
                id: server.id.clone(),
                label: server.name.clone(),
                version: None,
                enabled: server.enabled,
                available: server.deleted_at.is_none(),
            })
            .collect::<Vec<_>>();
        let mut items = Vec::new();
        for requirement in requirements
            .iter()
            .filter(|item| item.kind == AssistantRequirementKind::Mcp)
        {
            let preferred = servers
                .iter()
                .filter(|server| {
                    requirement
                        .preferred_ids
                        .iter()
                        .any(|id| id == &server.id || id == &server.name)
                })
                .collect::<Vec<_>>();
            let (status, message, allowed_actions, current_resource_id) = match preferred.as_slice() {
                [_, _, ..] => (
                    AssistantActivationStatus::Ambiguous,
                    "匹配到多个 MCP，需要逐项选择".to_owned(),
                    with_optional(vec![AssistantActivationAction::Select], requirement.required),
                    None,
                ),
                [server] if server.enabled => (
                    AssistantActivationStatus::Ready,
                    "MCP 已配置并启用".to_owned(),
                    vec![AssistantActivationAction::Keep],
                    Some(server.id.clone()),
                ),
                [server] => (
                    AssistantActivationStatus::Disabled,
                    "MCP 已配置但尚未启用".to_owned(),
                    with_optional(
                        vec![AssistantActivationAction::Enable, AssistantActivationAction::Select],
                        requirement.required,
                    ),
                    Some(server.id.clone()),
                ),
                [] => (
                    AssistantActivationStatus::ConfigurationRequired,
                    "需要单独配置或选择 MCP".to_owned(),
                    with_optional(
                        vec![AssistantActivationAction::Configure, AssistantActivationAction::Select],
                        requirement.required,
                    ),
                    None,
                ),
            };
            items.push(AssistantActivationItemResponse {
                requirement_key: requirement.key.clone(),
                label: requirement.label.clone(),
                required: requirement.required,
                status,
                message,
                allowed_actions,
                candidates: candidates.clone(),
                current_resource_id,
            });
        }
        Ok(group(AssistantRequirementKind::Mcp, items))
    }

    async fn resolve_models(
        &self,
        requirements: &[AssistantRequirementResponse],
    ) -> Result<AssistantActivationGroupResponse, AssistantError> {
        let providers = self.providers.list().await?;
        let mut candidates = Vec::new();
        for provider in &providers {
            let models = serde_json::from_str::<Vec<String>>(&provider.models).unwrap_or_default();
            let model_enabled = provider
                .model_enabled
                .as_deref()
                .and_then(|value| serde_json::from_str::<HashMap<String, bool>>(value).ok())
                .unwrap_or_default();
            for model in models {
                candidates.push(AssistantActivationCandidateResponse {
                    id: model.clone(),
                    label: format!("{} · {model}", provider.name),
                    version: None,
                    enabled: provider.enabled && model_enabled.get(&model).copied().unwrap_or(true),
                    available: provider.enabled,
                });
            }
        }
        let items = requirements
            .iter()
            .filter(|item| item.kind == AssistantRequirementKind::Model)
            .map(|requirement| {
                let selected = candidates
                    .iter()
                    .filter(|candidate| {
                        candidate.enabled
                            && candidate.available
                            && requirement.preferred_ids.iter().any(|id| id == &candidate.id)
                    })
                    .collect::<Vec<_>>();
                let (status, message, actions, current) = if selected.len() > 1 {
                    (
                        AssistantActivationStatus::Ambiguous,
                        "匹配到多个模型，需要逐项选择".to_owned(),
                        with_optional(
                            vec![AssistantActivationAction::Select, AssistantActivationAction::UseDefault],
                            requirement.required,
                        ),
                        None,
                    )
                } else if let Some(selected) = selected.first() {
                    (
                        AssistantActivationStatus::Ready,
                        "模型已可用".to_owned(),
                        vec![AssistantActivationAction::Keep],
                        Some(selected.id.clone()),
                    )
                } else {
                    (
                        AssistantActivationStatus::ConfigurationRequired,
                        "需要为助手选择模型或使用默认模型".to_owned(),
                        with_optional(
                            vec![AssistantActivationAction::Select, AssistantActivationAction::UseDefault],
                            requirement.required,
                        ),
                        None,
                    )
                };
                AssistantActivationItemResponse {
                    requirement_key: requirement.key.clone(),
                    label: requirement.label.clone(),
                    required: requirement.required,
                    status,
                    message,
                    allowed_actions: actions,
                    candidates: candidates.clone(),
                    current_resource_id: current,
                }
            })
            .collect();
        Ok(group(AssistantRequirementKind::Model, items))
    }

    async fn resolve_agents(
        &self,
        requirements: &[AssistantRequirementResponse],
    ) -> Result<AssistantActivationGroupResponse, AssistantError> {
        let agents = self.agents.list_management_agents().await?;
        let candidates = agents
            .iter()
            .map(|agent| AssistantActivationCandidateResponse {
                id: agent.id.clone(),
                label: agent.name.clone(),
                version: agent.agent_source_info.version.clone(),
                enabled: agent.enabled,
                available: agent.installed && agent.status != AgentManagementStatus::Missing,
            })
            .collect::<Vec<_>>();
        let items = requirements
            .iter()
            .filter(|item| item.kind == AssistantRequirementKind::Agent)
            .map(|requirement| {
                let selected = agents
                    .iter()
                    .filter(|agent| {
                        agent.enabled
                            && agent.installed
                            && agent.status != AgentManagementStatus::Missing
                            && requirement.preferred_ids.iter().any(|preferred| {
                                preferred == &agent.id
                                    || agent.backend.as_deref() == Some(preferred.as_str())
                                    || agent.agent_type.serde_name() == preferred
                                    || agent.name.eq_ignore_ascii_case(preferred)
                            })
                    })
                    .collect::<Vec<_>>();
                let (status, message, actions, current) = if selected.len() > 1 {
                    (
                        AssistantActivationStatus::Ambiguous,
                        "匹配到多个智能体运行引擎，需要逐项选择".to_owned(),
                        with_optional(
                            vec![AssistantActivationAction::Select, AssistantActivationAction::UseDefault],
                            requirement.required,
                        ),
                        None,
                    )
                } else if let Some(selected) = selected.first() {
                    (
                        AssistantActivationStatus::Ready,
                        "智能体运行引擎已可用".to_owned(),
                        vec![AssistantActivationAction::Keep],
                        Some(selected.id.clone()),
                    )
                } else {
                    (
                        AssistantActivationStatus::Unavailable,
                        "需要单独选择已安装且启用的智能体运行引擎".to_owned(),
                        with_optional(
                            vec![AssistantActivationAction::Select, AssistantActivationAction::UseDefault],
                            requirement.required,
                        ),
                        None,
                    )
                };
                AssistantActivationItemResponse {
                    requirement_key: requirement.key.clone(),
                    label: requirement.label.clone(),
                    required: requirement.required,
                    status,
                    message,
                    allowed_actions: actions,
                    candidates: candidates.clone(),
                    current_resource_id: current,
                }
            })
            .collect();
        Ok(group(AssistantRequirementKind::Agent, items))
    }
}

fn group(
    kind: AssistantRequirementKind,
    items: Vec<AssistantActivationItemResponse>,
) -> AssistantActivationGroupResponse {
    AssistantActivationGroupResponse {
        requires_confirmation: items.iter().any(|item| item.status != AssistantActivationStatus::Ready),
        kind,
        items,
    }
}

fn with_optional(mut actions: Vec<AssistantActivationAction>, required: bool) -> Vec<AssistantActivationAction> {
    if !required {
        actions.push(AssistantActivationAction::Skip);
    }
    actions
}

fn portable_skill_records(skills: &[AssistantDefaultRef], files: &[(String, Vec<u8>)]) -> Vec<PortableResourceRecord> {
    skills
        .iter()
        .filter_map(|skill| {
            let prefix = format!("resources/skills/{}/{}/", skill.source, skill.slug);
            let mut digest = Sha256::new();
            let mut found = false;
            for (path, bytes) in files.iter().filter(|(path, _)| path.starts_with(&prefix)) {
                found = true;
                digest.update(path.as_bytes());
                digest.update(b"\0");
                digest.update(bytes);
                digest.update(b"\0");
            }
            found.then(|| PortableResourceRecord {
                kind: AssistantRequirementKind::Skill,
                id: format!("{}:{}:{}", skill.source, skill.namespace, skill.slug),
                aliases: vec![skill.slug.clone()],
                path: prefix.trim_end_matches('/').to_owned(),
                sha256: format!("{:x}", digest.finalize()),
                configuration_required: false,
            })
        })
        .collect()
}

fn annotate_embedded_resources(
    groups: &mut [AssistantActivationGroupResponse],
    requirements: &[AssistantRequirementResponse],
    index: &PortableResourceIndex,
) {
    let requirements = requirements
        .iter()
        .map(|requirement| (requirement.key.as_str(), requirement))
        .collect::<HashMap<_, _>>();
    for group in groups {
        for item in &mut group.items {
            if item.status == AssistantActivationStatus::Ready {
                continue;
            }
            let Some(requirement) = requirements.get(item.requirement_key.as_str()) else {
                continue;
            };
            let embedded = index.resources.iter().any(|resource| {
                if resource.kind != requirement.kind {
                    return false;
                }
                match requirement.kind {
                    AssistantRequirementKind::Skill => requirement.identity.as_ref().is_some_and(|skill| {
                        resource.id == format!("{}:{}:{}", skill.source, skill.namespace, skill.slug)
                    }),
                    _ => requirement
                        .preferred_ids
                        .iter()
                        .any(|id| id == &resource.id || resource.aliases.contains(id)),
                }
            });
            if embedded {
                let hint = match requirement.kind {
                    AssistantRequirementKind::Skill => "助手包内含该技能，可在确认后导入并合并",
                    AssistantRequirementKind::Mcp => "助手包内含已脱敏的 MCP 配置模板，需要确认合并并补全密钥",
                    AssistantRequirementKind::Model => "助手包内含模型配置模板，需要确认合并并补全凭据",
                    AssistantRequirementKind::Agent => "助手包内含智能体配置模板，不含第三方可执行文件",
                };
                item.message = format!("{}；{hint}", item.message.trim_end_matches('。'));
            }
        }
    }
}

fn push_descriptor(
    kind: AssistantRequirementKind,
    id: &str,
    aliases: Vec<String>,
    descriptor: serde_json::Value,
    configuration_required: bool,
    files: &mut Vec<(String, Vec<u8>)>,
    records: &mut Vec<PortableResourceRecord>,
) -> Result<(), AssistantError> {
    let bytes = serde_json::to_vec_pretty(&descriptor).map_err(|error| AssistantError::Internal(error.to_string()))?;
    let kind_name = match kind {
        AssistantRequirementKind::Skill => "skills",
        AssistantRequirementKind::Mcp => "mcp",
        AssistantRequirementKind::Model => "models",
        AssistantRequirementKind::Agent => "agents",
    };
    let id_hash = format!("{:x}", Sha256::digest(id.as_bytes()));
    let path = format!("resources/{kind_name}/{}.json", &id_hash[..16]);
    records.push(PortableResourceRecord {
        kind,
        id: id.to_owned(),
        aliases,
        path: path.clone(),
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        configuration_required,
    });
    files.push((path, bytes));
    Ok(())
}

fn parse_json_or_default(value: &str, default: serde_json::Value) -> serde_json::Value {
    serde_json::from_str(value).unwrap_or(default)
}

fn sanitize_portable_json(value: serde_json::Value, parent_key: Option<&str>) -> serde_json::Value {
    match value {
        serde_json::Value::Object(entries) => serde_json::Value::Object(
            entries
                .into_iter()
                .map(|(key, value)| {
                    let normalized = key.to_ascii_lowercase();
                    let value = if parent_key.is_some_and(|parent| parent.eq_ignore_ascii_case("env")) {
                        serde_json::Value::String(format!("${{env:{key}}}"))
                    } else if is_sensitive_key(&normalized) {
                        serde_json::Value::String(format!("${{secret:{key}}}"))
                    } else {
                        sanitize_portable_json(value, Some(&key))
                    };
                    (key, value)
                })
                .collect(),
        ),
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .into_iter()
                .map(|value| sanitize_portable_json(value, parent_key))
                .collect(),
        ),
        serde_json::Value::String(value) => serde_json::Value::String(portable_string(&value)),
        value => value,
    }
}

fn is_sensitive_key(key: &str) -> bool {
    [
        "token",
        "secret",
        "password",
        "api_key",
        "apikey",
        "authorization",
        "cookie",
    ]
    .iter()
    .any(|candidate| key.contains(candidate))
}

fn portable_command_name(value: &str) -> String {
    Path::new(value)
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or(value)
        .to_owned()
}

fn portable_string(value: &str) -> String {
    if Path::new(value).is_absolute() {
        portable_command_name(value)
    } else {
        value.to_owned()
    }
}

fn require_resource_id<'a>(
    item: &AssistantActivationItemResponse,
    resource_id: Option<&'a str>,
) -> Result<&'a str, AssistantError> {
    resource_id
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AssistantError::BadRequest(format!("资源 {} 尚未选择具体配置", item.label)))
}

fn require_selectable_candidate(
    item: &AssistantActivationItemResponse,
    resource_id: &str,
) -> Result<(), AssistantError> {
    if item
        .candidates
        .iter()
        .any(|candidate| candidate.id == resource_id && candidate.enabled && candidate.available)
    {
        Ok(())
    } else {
        Err(AssistantError::Conflict(format!(
            "资源 {} 的所选配置不可用，请重新检查",
            item.label
        )))
    }
}

fn source_id(source: tjuaeui_api_types::AssistantSourceResponse) -> &'static str {
    match source {
        tjuaeui_api_types::AssistantSourceResponse::Mine => "mine",
        tjuaeui_api_types::AssistantSourceResponse::TjuaeHub => "tjuae-hub",
    }
}

fn binding_value(
    kind: AssistantRequirementKind,
    action: AssistantActivationAction,
    resource_id: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "kind": kind,
        "action": action,
        "resourceId": resource_id,
    })
}

fn activation_fingerprint(
    identity: &AssistantIdentityResponse,
    version: &str,
    manifest: &tjuaeui_api_types::AssistantManifestResponse,
    groups: &[AssistantActivationGroupResponse],
) -> Result<String, AssistantError> {
    let bytes = serde_json::to_vec(&(identity, version, manifest, groups))
        .map_err(|error| AssistantError::Internal(error.to_string()))?;
    Ok(format!("sha256-{:x}", Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    use async_trait::async_trait;
    use std::io::Read;
    use tempfile::TempDir;
    use tjuaeui_api_types::{
        AgentManagementRow, AssistantActivationChoice, AssistantSourceResponse, CreateMineAssistantRequest,
        SaveAssistantCatalogFileRequest,
    };
    use tjuaeui_db::{
        CreateMcpServerParams, CreateProviderParams, IAssistantUserPreferenceRepository, IMcpServerRepository,
        IProviderRepository, ISkillUserPreferenceRepository, SqliteAssistantUserPreferenceRepository,
        SqliteMcpServerRepository, SqliteProviderRepository, SqliteSkillUserPreferenceRepository,
        UpdateMcpServerParams, UpsertSkillUserPreferenceParams,
    };
    use tjuaeui_file::GitService;

    #[derive(Clone)]
    struct TestAgentCatalog {
        rows: Vec<AgentManagementRow>,
    }

    #[async_trait]
    impl AssistantAgentCatalogPort for TestAgentCatalog {
        async fn list_management_agents(&self) -> Result<Vec<AgentManagementRow>, AssistantError> {
            Ok(self.rows.clone())
        }
    }

    struct Fixture {
        _temp: TempDir,
        catalog: Arc<AssistantCatalogService>,
        activation: AssistantActivationService,
        assistant_preferences: Arc<SqliteAssistantUserPreferenceRepository>,
        skill_preferences: Arc<SqliteSkillUserPreferenceRepository>,
        mcp_servers: Arc<SqliteMcpServerRepository>,
        providers: Arc<SqliteProviderRepository>,
    }

    impl Fixture {
        async fn new(agents: Vec<AgentManagementRow>) -> Self {
            let temp = TempDir::new().unwrap();
            let database = tjuaeui_db::init_database_memory().await.unwrap();
            let pool = database.pool().clone();
            let assistant_preferences = Arc::new(SqliteAssistantUserPreferenceRepository::new(pool.clone()));
            let skill_preferences = Arc::new(SqliteSkillUserPreferenceRepository::new(pool.clone()));
            let mcp_servers = Arc::new(SqliteMcpServerRepository::new(pool.clone()));
            let providers = Arc::new(SqliteProviderRepository::new(pool.clone()));
            let catalog = Arc::new(AssistantCatalogService::new(
                assistant_preferences.clone(),
                temp.path(),
                None,
                None,
                false,
                Arc::new(GitService::new()),
            ));
            let activation = AssistantActivationService::new(
                pool.clone(),
                catalog.clone(),
                skill_preferences.clone(),
                mcp_servers.clone(),
                providers.clone(),
                Arc::new(TestAgentCatalog { rows: agents }),
            );
            Self {
                _temp: temp,
                catalog,
                activation,
                assistant_preferences,
                skill_preferences,
                mcp_servers,
                providers,
            }
        }

        async fn create_mcp(&self, name: &str, enabled: bool) -> String {
            self.mcp_servers
                .create(CreateMcpServerParams {
                    name,
                    description: None,
                    enabled,
                    transport_type: "stdio",
                    transport_config: r#"{"command":"demo"}"#,
                    tools: None,
                    original_json: None,
                    builtin: false,
                })
                .await
                .unwrap()
                .id
        }

        async fn create_provider(&self, id: &str, model: &str) {
            let models = serde_json::to_string(&vec![model]).unwrap();
            let model_enabled = serde_json::json!({ model: true }).to_string();
            self.providers
                .create(CreateProviderParams {
                    id: Some(id),
                    platform: "openai",
                    name: id,
                    base_url: "https://example.invalid/v1",
                    api_key_encrypted: "test",
                    models: &models,
                    enabled: true,
                    capabilities: "[]",
                    context_limit: None,
                    model_protocols: None,
                    model_enabled: Some(&model_enabled),
                    model_health: None,
                    model_settings: "{}",
                    bedrock_config: None,
                    is_full_url: false,
                })
                .await
                .unwrap();
        }

        async fn create_assistant(&self, slug: &str, requirements: serde_json::Value) -> AssistantIdentityResponse {
            self.catalog
                .create_mine(CreateMineAssistantRequest {
                    slug: slug.to_owned(),
                    name: slug.to_owned(),
                    description: "activation fixture".to_owned(),
                })
                .await
                .unwrap();
            let identity = AssistantIdentityResponse {
                source: AssistantSourceResponse::Mine,
                namespace: String::new(),
                slug: slug.to_owned(),
            };
            let file = self.catalog.file_content(&identity, None, "_meta.json").await.unwrap();
            let mut manifest = serde_json::from_str::<serde_json::Value>(&file.content).unwrap();
            manifest["defaults"]["agent"] = serde_json::Value::String("agent-a".to_owned());
            manifest["requirements"] = requirements;
            self.catalog
                .save_file(
                    &identity,
                    SaveAssistantCatalogFileRequest {
                        path: "_meta.json".to_owned(),
                        content: serde_json::to_string_pretty(&manifest).unwrap(),
                    },
                )
                .await
                .unwrap();
            identity
        }
    }

    fn agent(id: &str, backend: &str) -> AgentManagementRow {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "name": id,
            "backend": backend,
            "agent_type": "acp",
            "agent_source": "builtin",
            "agent_source_info": {},
            "enabled": true,
            "installed": true,
            "sort_order": 0,
            "status": "online"
        }))
        .unwrap()
    }

    fn full_requirements() -> serde_json::Value {
        serde_json::json!({
            "skills": [{
                "key": "skill-resource",
                "required": true,
                "identity": {"source": "tjuae-hub", "namespace": "official", "slug": "missing-dependency"}
            }],
            "mcps": [{
                "key": "mcp-resource",
                "required": true,
                "preferredMcpIds": ["mcp-disabled"]
            }],
            "models": [{
                "key": "model-resource",
                "required": true,
                "preferredModelIds": ["model-missing"]
            }],
            "agents": [{
                "key": "agent-resource",
                "required": true,
                "preferredAgentIds": ["agent-missing"]
            }]
        })
    }

    fn activation_choices(agent_id: &str) -> Vec<AssistantActivationChoice> {
        vec![
            AssistantActivationChoice {
                requirement_key: "skill-resource".to_owned(),
                action: AssistantActivationAction::Import,
                resource_id: None,
            },
            AssistantActivationChoice {
                requirement_key: "mcp-resource".to_owned(),
                action: AssistantActivationAction::Enable,
                resource_id: None,
            },
            AssistantActivationChoice {
                requirement_key: "model-resource".to_owned(),
                action: AssistantActivationAction::UseDefault,
                resource_id: None,
            },
            AssistantActivationChoice {
                requirement_key: "agent-resource".to_owned(),
                action: AssistantActivationAction::Select,
                resource_id: Some(agent_id.to_owned()),
            },
        ]
    }

    fn all_groups() -> Vec<AssistantRequirementKind> {
        vec![
            AssistantRequirementKind::Skill,
            AssistantRequirementKind::Mcp,
            AssistantRequirementKind::Model,
            AssistantRequirementKind::Agent,
        ]
    }

    #[test]
    fn optional_resource_has_explicit_skip_but_required_resource_does_not() {
        assert_eq!(
            with_optional(vec![AssistantActivationAction::Select], true),
            vec![AssistantActivationAction::Select]
        );
        assert_eq!(
            with_optional(vec![AssistantActivationAction::Select], false),
            vec![AssistantActivationAction::Select, AssistantActivationAction::Skip]
        );
    }

    #[tokio::test]
    async fn prepare_separates_all_resource_kinds_without_silent_changes() {
        let fixture = Fixture::new(vec![agent("agent-a", "codex")]).await;
        let mcp_id = fixture.create_mcp("mcp-disabled", false).await;
        fixture.create_provider("provider-a", "model-a").await;
        let identity = fixture.create_assistant("all-kinds", full_requirements()).await;

        let plan = fixture.activation.prepare(identity, None).await.unwrap();
        assert_eq!(
            plan.groups.iter().map(|group| group.kind).collect::<Vec<_>>(),
            all_groups()
        );
        assert!(
            plan.groups.iter().all(|group| group.requires_confirmation),
            "unexpected groups: {:#?}",
            plan.groups
        );
        assert_eq!(plan.groups[0].items[0].status, AssistantActivationStatus::Missing);
        assert_eq!(plan.groups[1].items[0].status, AssistantActivationStatus::Disabled);
        assert_eq!(
            plan.groups[2].items[0].status,
            AssistantActivationStatus::ConfigurationRequired
        );
        assert_eq!(plan.groups[3].items[0].status, AssistantActivationStatus::Unavailable);

        assert!(
            fixture
                .skill_preferences
                .get("tjuae-hub", "official", "missing-dependency")
                .await
                .unwrap()
                .is_none()
        );
        assert!(!fixture.mcp_servers.find_by_id(&mcp_id).await.unwrap().unwrap().enabled);
        assert!(fixture.assistant_preferences.list_enabled().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn activation_requires_every_group_and_every_conflicted_item() {
        let fixture = Fixture::new(vec![agent("agent-a", "codex")]).await;
        let mcp_id = fixture.create_mcp("mcp-disabled", false).await;
        fixture.create_provider("provider-a", "model-a").await;
        let identity = fixture
            .create_assistant("confirmed-activation", full_requirements())
            .await;
        let plan = fixture.activation.prepare(identity.clone(), None).await.unwrap();

        let error = fixture
            .activation
            .activate(
                identity.clone(),
                ActivateAssistantRequest {
                    plan_id: plan.plan_id.clone(),
                    fingerprint: plan.fingerprint.clone(),
                    confirmed_groups: vec![
                        AssistantRequirementKind::Skill,
                        AssistantRequirementKind::Mcp,
                        AssistantRequirementKind::Model,
                    ],
                    choices: activation_choices("agent-a"),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error, AssistantError::BadRequest(_)));
        assert!(!fixture.mcp_servers.find_by_id(&mcp_id).await.unwrap().unwrap().enabled);

        let plan = fixture.activation.prepare(identity.clone(), None).await.unwrap();
        let error = fixture
            .activation
            .activate(
                identity.clone(),
                ActivateAssistantRequest {
                    plan_id: plan.plan_id,
                    fingerprint: plan.fingerprint,
                    confirmed_groups: all_groups(),
                    choices: activation_choices("agent-a").into_iter().take(3).collect(),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error, AssistantError::BadRequest(_)));

        let plan = fixture.activation.prepare(identity.clone(), None).await.unwrap();
        let result = fixture
            .activation
            .activate(
                identity.clone(),
                ActivateAssistantRequest {
                    plan_id: plan.plan_id,
                    fingerprint: plan.fingerprint,
                    confirmed_groups: all_groups(),
                    choices: activation_choices("agent-a"),
                },
            )
            .await
            .unwrap();
        assert!(result.enabled);
        assert_eq!(result.activation_status, "ready");
        assert!(fixture.mcp_servers.find_by_id(&mcp_id).await.unwrap().unwrap().enabled);
        assert!(
            fixture
                .skill_preferences
                .get("tjuae-hub", "official", "missing-dependency")
                .await
                .unwrap()
                .unwrap()
                .enabled
        );
        assert!(
            fixture
                .catalog
                .runtime_profile("mine::confirmed-activation")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn prepare_reports_version_and_ambiguous_conflicts_per_item() {
        let fixture = Fixture::new(vec![agent("agent-a", "codex"), agent("agent-b", "codex")]).await;
        fixture
            .skill_preferences
            .upsert(UpsertSkillUserPreferenceParams {
                source: "tjuae-hub",
                namespace: "official",
                slug: "skill-creator",
                selected_version: Some("1.0.0"),
                follow_latest: false,
                enabled: true,
                auto_inject: false,
            })
            .await
            .unwrap();
        let first_mcp = fixture.create_mcp("mcp-a", true).await;
        let second_mcp = fixture.create_mcp("mcp-b", true).await;
        fixture.create_provider("provider-a", "shared-model").await;
        fixture.create_provider("provider-b", "shared-model").await;

        let requirements = vec![
            AssistantRequirementResponse {
                key: "skill".to_owned(),
                kind: AssistantRequirementKind::Skill,
                required: true,
                label: "skill-creator".to_owned(),
                identity: Some(tjuaeui_api_types::AssistantDefaultRef {
                    source: "tjuae-hub".to_owned(),
                    namespace: "official".to_owned(),
                    slug: "skill-creator".to_owned(),
                }),
                preferred_ids: vec![],
                version_requirement: Some("2.0.0".to_owned()),
            },
            AssistantRequirementResponse {
                key: "mcp".to_owned(),
                kind: AssistantRequirementKind::Mcp,
                required: true,
                label: "MCP".to_owned(),
                identity: None,
                preferred_ids: vec![first_mcp, second_mcp],
                version_requirement: None,
            },
            AssistantRequirementResponse {
                key: "model".to_owned(),
                kind: AssistantRequirementKind::Model,
                required: true,
                label: "shared-model".to_owned(),
                identity: None,
                preferred_ids: vec!["shared-model".to_owned()],
                version_requirement: None,
            },
            AssistantRequirementResponse {
                key: "agent".to_owned(),
                kind: AssistantRequirementKind::Agent,
                required: true,
                label: "codex".to_owned(),
                identity: None,
                preferred_ids: vec!["codex".to_owned()],
                version_requirement: None,
            },
        ];

        assert_eq!(
            fixture.activation.resolve_skills(&requirements).await.unwrap().items[0].status,
            AssistantActivationStatus::VersionConflict
        );
        assert_eq!(
            fixture.activation.resolve_mcps(&requirements).await.unwrap().items[0].status,
            AssistantActivationStatus::Ambiguous
        );
        assert_eq!(
            fixture.activation.resolve_models(&requirements).await.unwrap().items[0].status,
            AssistantActivationStatus::Ambiguous
        );
        assert_eq!(
            fixture.activation.resolve_agents(&requirements).await.unwrap().items[0].status,
            AssistantActivationStatus::Ambiguous
        );
    }

    #[tokio::test]
    async fn stale_resource_state_and_file_edits_invalidate_prepared_plan() {
        let fixture = Fixture::new(vec![agent("agent-a", "codex")]).await;
        let mcp_id = fixture.create_mcp("mcp-disabled", false).await;
        fixture.create_provider("provider-a", "model-a").await;
        let identity = fixture.create_assistant("stale-plan", full_requirements()).await;

        let plan = fixture.activation.prepare(identity.clone(), None).await.unwrap();
        fixture
            .mcp_servers
            .update(
                &mcp_id,
                UpdateMcpServerParams {
                    enabled: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let error = fixture
            .activation
            .activate(
                identity.clone(),
                ActivateAssistantRequest {
                    plan_id: plan.plan_id,
                    fingerprint: plan.fingerprint,
                    confirmed_groups: all_groups(),
                    choices: activation_choices("agent-a"),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error, AssistantError::Conflict(_)));

        fixture
            .mcp_servers
            .update(
                &mcp_id,
                UpdateMcpServerParams {
                    enabled: Some(false),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let plan = fixture.activation.prepare(identity.clone(), None).await.unwrap();
        fixture
            .catalog
            .save_file(
                &identity,
                SaveAssistantCatalogFileRequest {
                    path: "ASSISTANT.md".to_owned(),
                    content: "# Changed after prepare".to_owned(),
                },
            )
            .await
            .unwrap();
        let error = fixture
            .activation
            .activate(
                identity,
                ActivateAssistantRequest {
                    plan_id: plan.plan_id,
                    fingerprint: plan.fingerprint,
                    confirmed_groups: all_groups(),
                    choices: activation_choices("agent-a"),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error, AssistantError::Conflict(_)));
    }

    #[test]
    fn source_is_part_of_runtime_identity() {
        let mine = AssistantIdentityResponse {
            source: AssistantSourceResponse::Mine,
            namespace: String::new(),
            slug: "writer".to_owned(),
        };
        let hub = AssistantIdentityResponse {
            source: AssistantSourceResponse::TjuaeHub,
            namespace: "official".to_owned(),
            slug: "writer".to_owned(),
        };
        assert_ne!(crate::catalog::runtime_id(&mine), crate::catalog::runtime_id(&hub));
    }

    #[test]
    fn portable_json_removes_secrets_environment_values_and_absolute_paths() {
        let sanitized = sanitize_portable_json(
            serde_json::json!({
                "apiKey": "secret-value",
                "env": {"HOME_TOKEN": "secret-value"},
                "command": r"C:\\tools\\server.exe",
                "url": "https://example.invalid/mcp"
            }),
            None,
        );
        assert_eq!(sanitized["apiKey"], "${secret:apiKey}");
        assert_eq!(sanitized["env"]["HOME_TOKEN"], "${env:HOME_TOKEN}");
        assert_eq!(sanitized["command"], "server.exe");
        assert_eq!(sanitized["url"], "https://example.invalid/mcp");
    }

    #[tokio::test]
    async fn export_contains_local_skill_files_and_sanitized_resource_index() {
        let fixture = Fixture::new(vec![]).await;
        let data_root = fixture.catalog.mine_root().parent().unwrap();
        let skill_root = data_root.join("skills").join("portable-skill");
        tokio::fs::create_dir_all(&skill_root).await.unwrap();
        tokio::fs::write(skill_root.join("SKILL.md"), "# Portable skill")
            .await
            .unwrap();
        fixture.create_mcp("portable-mcp", true).await;
        let identity = fixture
            .create_assistant(
                "portable-assistant",
                serde_json::json!({
                    "skills": [{
                        "key": "portable-skill",
                        "required": true,
                        "identity": {"source": "mine", "namespace": "", "slug": "portable-skill"}
                    }],
                    "mcps": [{
                        "key": "portable-mcp",
                        "required": true,
                        "preferredMcpIds": ["portable-mcp"]
                    }],
                    "models": [],
                    "agents": []
                }),
            )
            .await;
        let output = data_root.join("portable-assistant.zip");
        fixture
            .activation
            .export(
                &identity,
                ExportAssistantRequest {
                    version: None,
                    output_path: output.to_string_lossy().into_owned(),
                },
            )
            .await
            .unwrap();

        let mut archive = zip::ZipArchive::new(std::fs::File::open(output).unwrap()).unwrap();
        assert!(archive.by_name("resources/skills/mine/portable-skill/SKILL.md").is_ok());
        let mut index = String::new();
        archive
            .by_name("resources/_index.json")
            .unwrap()
            .read_to_string(&mut index)
            .unwrap();
        assert!(index.contains("portable-skill"));
        assert!(index.contains("portable-mcp"));
        assert!(!index.contains("secret-value"));
    }
}
