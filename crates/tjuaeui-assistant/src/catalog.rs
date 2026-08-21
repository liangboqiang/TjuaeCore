use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tjuaeui_api_types::{
    AssistantCatalogDetailResponse, AssistantCatalogFileContentResponse, AssistantCatalogFileResponse,
    AssistantCatalogItemResponse, AssistantCatalogPageResponse, AssistantDefaultRef, AssistantDefaultScalar,
    AssistantDefaultsCatalogResponse, AssistantIdentityResponse, AssistantManifestResponse, AssistantOperationResponse,
    AssistantPreferencesCatalogResponse, AssistantRequirementKind, AssistantRequirementResponse,
    AssistantSourceResponse, AssistantVersionComparisonResponse, AssistantVersionFileDiffResponse,
    AssistantVersionResponse, CopyAssistantToMineRequest, CreateMineAssistantRequest, ExportAssistantRequest,
    ExportAssistantResponse, ImportAssistantRequest, PublishAssistantCatalogRequest, PublishAssistantCatalogResponse,
    SaveAssistantCatalogFileRequest, UpdateAssistantCatalogPreferencesRequest, UpdateAssistantCatalogSettingsRequest,
    UpdateAssistantRuntimeOverridesRequest,
};
use tjuaeui_catalog::{CatalogError, CatalogFile, CatalogProvider, CatalogVersion};
use tjuaeui_db::{AssistantUserPreferenceRow, IAssistantUserPreferenceRepository, UpsertAssistantUserPreferenceParams};
use tjuaeui_file::GitServiceRef;
use tjuaeui_runtime::Builder as CommandBuilder;

use crate::AssistantError;

const MANIFEST_FILE: &str = "_meta.json";
const ENTRY_FILE: &str = "ASSISTANT.md";
const HUB_INDEX_ENV: &str = "TJUAE_HUB_ASSISTANT_INDEX_URL";
const HUB_INDEX_URL: &str = "https://raw.githubusercontent.com/liangboqiang/TjuaeHub/main/dist/assistants.json";
const HUB_INDEX_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(30);
pub const SYSTEM_ASSISTANT_SLUG: &str = "tjuaeui-assistant";

#[derive(Clone)]
pub struct AssistantCatalogService {
    preferences: Arc<dyn IAssistantUserPreferenceRepository>,
    mine_root: PathBuf,
    hub_worktree: Option<PathBuf>,
    hub_index_snapshot: Option<PathBuf>,
    can_write_hub: bool,
    git: GitServiceRef,
    hub_index_cache: Arc<tokio::sync::RwLock<Option<(tokio::time::Instant, AssistantMarketIndex)>>>,
    hub_asset_cache: Arc<tokio::sync::RwLock<HashMap<String, Vec<u8>>>>,
}

/// 已激活助手的领域运行时快照。所有宿主（会话、团队、定时任务、频道）都从
/// 这一结构构造自己的执行快照，目录文件与用户偏好是唯一事实来源。
#[derive(Debug, Clone)]
pub struct AssistantRuntimeProfile {
    pub id: String,
    pub identity: AssistantIdentityResponse,
    pub version: String,
    pub name: String,
    pub name_i18n: BTreeMap<String, String>,
    pub description: String,
    pub description_i18n: BTreeMap<String, String>,
    pub avatar_url: Option<String>,
    pub agent_id: String,
    pub rules: String,
    pub model_mode: String,
    pub model: Option<String>,
    pub permission_mode: String,
    pub permission: Option<String>,
    pub thought_level_mode: String,
    pub thought_level: Option<String>,
    pub skill_ids: Vec<String>,
    pub mcp_ids: Vec<String>,
    pub recommended_prompts: Vec<String>,
    pub recommended_prompts_i18n: BTreeMap<String, Vec<String>>,
    pub sort_order: i32,
    pub last_used_at: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct RuntimeOverrides {
    model: Option<String>,
    permission: Option<String>,
    thought_level: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeBinding {
    kind: AssistantRequirementKind,
    action: tjuaeui_api_types::AssistantActivationAction,
    resource_id: Option<String>,
}

impl AssistantCatalogService {
    pub fn new(
        preferences: Arc<dyn IAssistantUserPreferenceRepository>,
        data_dir: &Path,
        hub_worktree: Option<PathBuf>,
        hub_index_snapshot: Option<PathBuf>,
        can_write_hub: bool,
        git: GitServiceRef,
    ) -> Self {
        Self {
            preferences,
            mine_root: data_dir.join("assistants"),
            hub_worktree,
            hub_index_snapshot,
            can_write_hub,
            git,
            hub_index_cache: Arc::new(tokio::sync::RwLock::new(None)),
            hub_asset_cache: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        }
    }

    pub fn mine_root(&self) -> &Path {
        &self.mine_root
    }

    /// 创建或修复本地系统管家，并强制保持启用。已通过校验的本地设置会保留；
    /// 只有目录缺失或损坏时才用应用内置定义修复。
    pub async fn ensure_system_assistant(&self, rules: &str) -> Result<(), AssistantError> {
        tokio::fs::create_dir_all(&self.mine_root).await?;
        let root = self.mine_root.join(SYSTEM_ASSISTANT_SLUG);
        let temporary = self
            .mine_root
            .join(format!(".{SYSTEM_ASSISTANT_SLUG}.system.{}", uuid::Uuid::now_v7()));
        let backup = self
            .mine_root
            .join(format!(".{SYSTEM_ASSISTANT_SLUG}.backup.{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir(&temporary).await?;
        let result = async {
            tokio::fs::write(temporary.join(ENTRY_FILE), rules.as_bytes()).await?;
            let mut manifest = system_assistant_manifest(String::new());
            manifest.content_hash = assistant_directory_digest(&temporary)?;
            let manifest_bytes =
                serde_json::to_vec_pretty(&manifest).map_err(|error| AssistantError::Internal(error.to_string()))?;
            tokio::fs::write(temporary.join(MANIFEST_FILE), &manifest_bytes).await?;
            read_manifest_as(&temporary, SYSTEM_ASSISTANT_SLUG).await?;

            let active_manifest = if root.is_dir()
                && let Ok(existing) = read_manifest_as(&root, SYSTEM_ASSISTANT_SLUG).await
            {
                tokio::fs::remove_dir_all(&temporary).await?;
                existing
            } else if root.is_dir() {
                rename_assistant_directory(&root, &backup).await?;
                if let Err(error) = rename_assistant_directory(&temporary, &root).await {
                    let _ = rename_assistant_directory(&backup, &root).await;
                    return Err(error);
                }
                let _ = tokio::fs::remove_dir_all(&backup).await;
                manifest
            } else {
                // A first install has no previous directory to preserve. On
                // Windows, antivirus/indexers can transiently deny renaming a
                // freshly written directory even though creating and copying
                // its files is allowed. Copy the already validated two-file
                // package into place; an interrupted copy is repaired by this
                // same routine on the next startup.
                copy_directory(&temporary, &root)?;
                tokio::fs::remove_dir_all(&temporary).await?;
                manifest
            };

            let current = self.preferences.get("mine", "", SYSTEM_ASSISTANT_SLUG).await?;
            self.preferences
                .upsert(UpsertAssistantUserPreferenceParams {
                    source: "mine",
                    namespace: "",
                    slug: SYSTEM_ASSISTANT_SLUG,
                    selected_version: Some(&active_manifest.version),
                    follow_latest: false,
                    enabled: true,
                    activation_status: "ready",
                    activation_fingerprint: Some(&active_manifest.content_hash),
                    resource_bindings: current
                        .as_ref()
                        .map(|row| row.resource_bindings.as_str())
                        .unwrap_or("{}"),
                    runtime_overrides: current
                        .as_ref()
                        .map(|row| row.runtime_overrides.as_str())
                        .unwrap_or("{}"),
                    sort_order: current.as_ref().map(|row| row.sort_order).unwrap_or(-10_000),
                    last_used_at: current.as_ref().and_then(|row| row.last_used_at),
                })
                .await?;
            Ok::<(), AssistantError>(())
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_dir_all(&temporary).await;
            let _ = tokio::fs::remove_dir_all(&backup).await;
        }
        result
    }

    /// 返回全部已启用且通过资源激活检查的助手。未完成逐项确认的目录项不会
    /// 泄漏到会话、团队、定时任务或频道选择器。
    pub async fn list_runtime_profiles(&self) -> Result<Vec<AssistantRuntimeProfile>, AssistantError> {
        let mut profiles = Vec::new();
        for preference in self.preferences.list_enabled().await? {
            if preference.activation_status != "ready" {
                continue;
            }
            let identity = AssistantIdentityResponse {
                source: parse_source_id(&preference.source)?,
                namespace: preference.namespace.clone(),
                slug: preference.slug.clone(),
            };
            profiles.push(self.runtime_profile_from_preference(identity, preference).await?);
        }
        profiles.sort_by(|left, right| {
            left.sort_order
                .cmp(&right.sort_order)
                .then_with(|| left.name.cmp(&right.name))
        });
        Ok(profiles)
    }

    pub async fn runtime_profile(&self, id: &str) -> Result<Option<AssistantRuntimeProfile>, AssistantError> {
        let identity = parse_runtime_id(id)?;
        let preference = self
            .preferences
            .get(source_id(identity.source), &identity.namespace, &identity.slug)
            .await?;
        let Some(preference) = preference.filter(|row| row.enabled && row.activation_status == "ready") else {
            return Ok(None);
        };
        self.runtime_profile_from_preference(identity, preference)
            .await
            .map(Some)
    }

    pub async fn update_runtime_overrides(
        &self,
        id: &str,
        updates: UpdateAssistantRuntimeOverridesRequest,
    ) -> Result<(), AssistantError> {
        let identity = parse_runtime_id(id)?;
        let Some(current) = self
            .preferences
            .get(source_id(identity.source), &identity.namespace, &identity.slug)
            .await?
        else {
            return Err(AssistantError::NotFound(id.to_owned()));
        };
        let mut overrides = serde_json::from_str::<RuntimeOverrides>(&current.runtime_overrides).unwrap_or_default();
        if updates.model.is_some() {
            overrides.model = updates.model;
        }
        if updates.permission.is_some() {
            overrides.permission = updates.permission;
        }
        if updates.thought_level.is_some() {
            overrides.thought_level = updates.thought_level;
        }
        let overrides =
            serde_json::to_string(&overrides).map_err(|error| AssistantError::Internal(error.to_string()))?;
        self.preferences
            .upsert(UpsertAssistantUserPreferenceParams {
                source: &current.source,
                namespace: &current.namespace,
                slug: &current.slug,
                selected_version: current.selected_version.as_deref(),
                follow_latest: current.follow_latest,
                enabled: current.enabled,
                activation_status: &current.activation_status,
                activation_fingerprint: current.activation_fingerprint.as_deref(),
                resource_bindings: &current.resource_bindings,
                runtime_overrides: &overrides,
                sort_order: current.sort_order,
                last_used_at: current.last_used_at,
            })
            .await?;
        Ok(())
    }

    async fn runtime_profile_from_preference(
        &self,
        identity: AssistantIdentityResponse,
        preference: AssistantUserPreferenceRow,
    ) -> Result<AssistantRuntimeProfile, AssistantError> {
        let detail = self.detail(&identity, preference.selected_version.as_deref()).await?;
        let bindings = serde_json::from_str::<BTreeMap<String, RuntimeBinding>>(&preference.resource_bindings)
            .map_err(|error| AssistantError::Internal(format!("助手资源绑定无效：{error}")))?;
        let overrides = serde_json::from_str::<RuntimeOverrides>(&preference.runtime_overrides).unwrap_or_default();

        let mut skill_ids = detail
            .manifest
            .defaults
            .skills
            .iter()
            .map(|skill| skill.slug.clone())
            .collect::<BTreeSet<_>>();
        let mut mcp_ids = detail.manifest.defaults.mcps.iter().cloned().collect::<BTreeSet<_>>();
        let mut agent_id = detail.manifest.defaults.agent.clone().unwrap_or_default();
        let mut model = detail.manifest.defaults.model.value.clone();
        for requirement in &detail.manifest.requirements {
            let binding = bindings.get(&requirement.key);
            if binding.is_some_and(|binding| binding.kind != requirement.kind) {
                return Err(AssistantError::Internal(format!(
                    "助手资源绑定类型与声明不一致：{}",
                    requirement.key
                )));
            }
            let skipped =
                binding.is_some_and(|binding| binding.action == tjuaeui_api_types::AssistantActivationAction::Skip);
            match requirement.kind {
                AssistantRequirementKind::Skill => {
                    if skipped {
                        if let Some(skill) = &requirement.identity {
                            skill_ids.remove(&skill.slug);
                        }
                    } else if let Some(skill) = &requirement.identity {
                        skill_ids.insert(skill.slug.clone());
                    }
                }
                AssistantRequirementKind::Mcp => {
                    if !skipped && let Some(resource_id) = binding.and_then(|binding| binding.resource_id.as_ref()) {
                        mcp_ids.insert(resource_id.clone());
                    }
                }
                AssistantRequirementKind::Model => {
                    if !skipped && let Some(resource_id) = binding.and_then(|binding| binding.resource_id.as_ref()) {
                        model = Some(resource_id.clone());
                    }
                }
                AssistantRequirementKind::Agent => {
                    if !skipped && let Some(resource_id) = binding.and_then(|binding| binding.resource_id.as_ref()) {
                        agent_id.clone_from(resource_id);
                    }
                }
            }
        }
        if let Some(value) = overrides.model.clone() {
            model = Some(value);
        }
        if agent_id.is_empty() {
            return Err(AssistantError::Conflict(format!(
                "助手 {} 尚未绑定智能体运行引擎",
                detail.item.name
            )));
        }
        Ok(AssistantRuntimeProfile {
            id: runtime_id(&identity),
            identity,
            version: detail.manifest.version.clone(),
            name: detail.manifest.name.clone(),
            name_i18n: detail.manifest.name_i18n.clone(),
            description: detail.manifest.description.clone(),
            description_i18n: detail.manifest.description_i18n.clone(),
            avatar_url: detail.item.avatar_url.clone(),
            agent_id,
            rules: detail.readme,
            model_mode: detail.manifest.defaults.model.mode,
            model,
            permission_mode: detail.manifest.defaults.permission.mode,
            permission: overrides.permission.or(detail.manifest.defaults.permission.value),
            thought_level_mode: detail.manifest.defaults.thought_level.mode,
            thought_level: overrides.thought_level.or(detail.manifest.defaults.thought_level.value),
            skill_ids: skill_ids.into_iter().collect(),
            mcp_ids: mcp_ids.into_iter().collect(),
            recommended_prompts: detail.manifest.recommended_prompts,
            recommended_prompts_i18n: detail.manifest.recommended_prompts_i18n,
            sort_order: preference.sort_order,
            last_used_at: preference.last_used_at,
        })
    }

    pub async fn create_mine(
        &self,
        request: CreateMineAssistantRequest,
    ) -> Result<AssistantCatalogDetailResponse, AssistantError> {
        validate_slug(&request.slug)?;
        if request.name.trim().is_empty() {
            return Err(AssistantError::BadRequest("助手名称不能为空".to_owned()));
        }
        let instruction = format!("# {}\n\n{}\n", request.name.trim(), request.description.trim());
        let manifest = AssistantManifest {
            schema:
                "https://raw.githubusercontent.com/liangboqiang/TjuaeHub/main/schemas/tjuae-assistant.v1.schema.json"
                    .to_owned(),
            format: "tjuae-assistant".to_owned(),
            format_version: 1,
            id: request.slug.clone(),
            version: "0.1.0".to_owned(),
            name: request.name.trim().to_owned(),
            name_i18n: BTreeMap::new(),
            description: request.description.trim().to_owned(),
            description_i18n: BTreeMap::new(),
            categories: Vec::new(),
            tags: Vec::new(),
            avatar: None,
            instructions: InstructionManifest {
                default: ENTRY_FILE.to_owned(),
                locales: BTreeMap::new(),
            },
            defaults: DefaultsManifest::default(),
            requirements: RequirementsManifest::default(),
            recommended_prompts: Vec::new(),
            recommended_prompts_i18n: BTreeMap::new(),
            content_hash: String::new(),
            extensions: BTreeMap::new(),
        };
        let root = self.mine_root.join(&request.slug);
        let temporary = self
            .mine_root
            .join(format!(".{}.{}", request.slug, uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(&self.mine_root).await?;
        if tokio::fs::try_exists(&root).await? {
            return Err(AssistantError::Conflict(format!("助手 {} 已存在", request.slug)));
        }
        tokio::fs::create_dir(&temporary).await?;
        let result = async {
            tokio::fs::write(temporary.join(ENTRY_FILE), instruction).await?;
            let mut manifest = manifest;
            manifest.content_hash = assistant_directory_digest(&temporary)?;
            tokio::fs::write(
                temporary.join(MANIFEST_FILE),
                serde_json::to_vec_pretty(&manifest).map_err(|error| AssistantError::Internal(error.to_string()))?,
            )
            .await?;
            read_manifest_as(&temporary, &request.slug).await?;
            tokio::fs::rename(&temporary, &root).await?;
            Ok::<(), AssistantError>(())
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_dir_all(&temporary).await;
        }
        result?;
        self.detail(&identity(AssistantSourceResponse::Mine, "", &request.slug), None)
            .await
    }

    pub async fn import_mine(
        &self,
        request: ImportAssistantRequest,
    ) -> Result<AssistantCatalogDetailResponse, AssistantError> {
        let archive = PathBuf::from(request.archive_path);
        if !archive
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("zip"))
        {
            return Err(AssistantError::BadRequest(
                "助手导入文件必须使用 .zip 扩展名".to_owned(),
            ));
        }
        if !tokio::fs::try_exists(&archive).await? {
            return Err(AssistantError::NotFound("助手导入文件不存在".to_owned()));
        }
        if !tokio::fs::metadata(&archive).await?.is_file() {
            return Err(AssistantError::BadRequest("助手导入路径不是文件".to_owned()));
        }

        tokio::fs::create_dir_all(&self.mine_root).await?;
        let staging = self.mine_root.join(format!(".importing-{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir(&staging).await?;
        let extract_archive = archive.clone();
        let extract_target = staging.clone();
        let result = async {
            tokio::task::spawn_blocking(move || extract_assistant_archive(&extract_archive, &extract_target))
                .await
                .map_err(|error| AssistantError::Internal(error.to_string()))??;
            let package_root = detect_assistant_package_root(&staging)?;
            let manifest_bytes = tokio::fs::read(package_root.join(MANIFEST_FILE)).await?;
            let mut manifest: AssistantManifest = serde_json::from_slice(&manifest_bytes)
                .map_err(|error| AssistantError::BadRequest(format!("助手清单无效：{error}")))?;
            validate_slug(&manifest.id)?;
            validate_manifest(&manifest, &manifest.id)?;
            if !package_root.join(&manifest.instructions.default).is_file() {
                return Err(AssistantError::BadRequest("助手包缺少规则入口文件".to_owned()));
            }
            let target = self.mine_root.join(&manifest.id);
            if tokio::fs::try_exists(&target).await? {
                return Err(AssistantError::Conflict(format!("助手 {} 已存在", manifest.id)));
            }
            manifest.content_hash = assistant_directory_digest(&package_root)?;
            tokio::fs::write(
                package_root.join(MANIFEST_FILE),
                serde_json::to_vec_pretty(&manifest).map_err(|error| AssistantError::Internal(error.to_string()))?,
            )
            .await?;
            let slug = manifest.id.clone();
            tokio::fs::rename(&package_root, &target).await?;
            let identity = identity(AssistantSourceResponse::Mine, "", &slug);
            self.detail(&identity, None).await
        }
        .await;
        let _ = tokio::fs::remove_dir_all(&staging).await;
        result
    }

    pub async fn save_file(
        &self,
        identity: &AssistantIdentityResponse,
        request: SaveAssistantCatalogFileRequest,
    ) -> Result<AssistantCatalogDetailResponse, AssistantError> {
        tjuaeui_catalog::validate_relative_file(&request.path, request.content.len() as u64)?;
        let root = self.editable_root(identity)?;
        let target = root.join(&request.path);
        if !target.is_file() {
            return Err(AssistantError::NotFound(format!("助手文件 {}", request.path)));
        }

        let parent = root
            .parent()
            .ok_or_else(|| AssistantError::Internal("助手目录缺少父目录".to_owned()))?;
        let temporary = parent.join(format!(".{}.edit.{}", identity.slug, uuid::Uuid::now_v7()));
        let backup = parent.join(format!(".{}.backup.{}", identity.slug, uuid::Uuid::now_v7()));
        let source = root.clone();
        let destination = temporary.clone();
        tokio::task::spawn_blocking(move || copy_directory(&source, &destination))
            .await
            .map_err(|error| AssistantError::Internal(error.to_string()))??;

        let result = async {
            tokio::fs::write(temporary.join(&request.path), request.content.as_bytes()).await?;
            let manifest_path = temporary.join(MANIFEST_FILE);
            let mut manifest: AssistantManifest = serde_json::from_slice(&tokio::fs::read(&manifest_path).await?)
                .map_err(|error| AssistantError::BadRequest(format!("助手清单无效：{error}")))?;
            validate_manifest(&manifest, &identity.slug)?;
            manifest.content_hash = assistant_directory_digest(&temporary)?;
            tokio::fs::write(
                &manifest_path,
                serde_json::to_vec_pretty(&manifest).map_err(|error| AssistantError::Internal(error.to_string()))?,
            )
            .await?;
            read_manifest_as(&temporary, &identity.slug).await?;
            tokio::fs::rename(&root, &backup).await?;
            if let Err(error) = tokio::fs::rename(&temporary, &root).await {
                let _ = tokio::fs::rename(&backup, &root).await;
                return Err(error.into());
            }
            if let Err(error) = self
                .refresh_activation_after_edit(identity, &manifest.content_hash)
                .await
            {
                let _ = tokio::fs::remove_dir_all(&root).await;
                let _ = tokio::fs::rename(&backup, &root).await;
                return Err(error);
            }
            let _ = tokio::fs::remove_dir_all(&backup).await;
            Ok::<(), AssistantError>(())
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_dir_all(&temporary).await;
        }
        result?;
        self.detail(identity, None).await
    }

    pub async fn update_settings(
        &self,
        identity: &AssistantIdentityResponse,
        request: UpdateAssistantCatalogSettingsRequest,
    ) -> Result<AssistantCatalogDetailResponse, AssistantError> {
        let name = request.name.trim();
        let description = request.description.trim();
        if name.is_empty() || name.len() > 120 {
            return Err(AssistantError::BadRequest(
                "助手名称不能为空且不能超过 120 个字符".to_owned(),
            ));
        }
        if description.len() > 2_000 || request.rules.len() > 512 * 1024 {
            return Err(AssistantError::BadRequest("助手说明或规则内容过长".to_owned()));
        }
        if request.recommended_prompts.len() > 50
            || request
                .recommended_prompts
                .iter()
                .any(|prompt| prompt.trim().is_empty() || prompt.len() > 500)
        {
            return Err(AssistantError::BadRequest("建议提示词无效".to_owned()));
        }

        let root = self.editable_root(identity)?;
        let parent = root
            .parent()
            .ok_or_else(|| AssistantError::Internal("助手目录缺少父目录".to_owned()))?;
        let temporary = parent.join(format!(".{}.settings.{}", identity.slug, uuid::Uuid::now_v7()));
        let backup = parent.join(format!(".{}.backup.{}", identity.slug, uuid::Uuid::now_v7()));
        let source = root.clone();
        let destination = temporary.clone();
        tokio::task::spawn_blocking(move || copy_directory(&source, &destination))
            .await
            .map_err(|error| AssistantError::Internal(error.to_string()))??;

        let result = async {
            let manifest_path = temporary.join(MANIFEST_FILE);
            let mut manifest: AssistantManifest = serde_json::from_slice(&tokio::fs::read(&manifest_path).await?)
                .map_err(|error| AssistantError::BadRequest(format!("助手清单无效：{error}")))?;
            manifest.name = name.to_owned();
            manifest.description = description.to_owned();
            manifest.categories = unique_manifest_values(request.categories, 20, 60, "分类")?;
            manifest.tags = unique_manifest_values(request.tags, 40, 60, "标签")?;
            manifest.recommended_prompts = request
                .recommended_prompts
                .into_iter()
                .map(|prompt| prompt.trim().to_owned())
                .collect();
            manifest.defaults = DefaultsManifest {
                agent: request.defaults.agent,
                model: scalar_default(request.defaults.model),
                permission: scalar_default(request.defaults.permission),
                thought_level: scalar_default(request.defaults.thought_level),
                skills: request.defaults.skills,
                mcps: request.defaults.mcps,
            };
            manifest.requirements = requirements_for_defaults(&manifest.defaults);
            if let Some(data_url) = request.avatar_data_url.as_deref() {
                let (extension, bytes) = decode_avatar_data_url(data_url)?;
                let file_name = format!("avatar.upload.{extension}");
                tokio::fs::write(temporary.join(&file_name), bytes).await?;
                manifest.avatar = Some(file_name);
            } else {
                manifest.avatar = request.avatar.filter(|value| !value.trim().is_empty());
            }
            tokio::fs::write(temporary.join(ENTRY_FILE), request.rules.as_bytes()).await?;
            validate_manifest(&manifest, &identity.slug)?;
            manifest.content_hash = assistant_directory_digest(&temporary)?;
            tokio::fs::write(
                &manifest_path,
                serde_json::to_vec_pretty(&manifest).map_err(|error| AssistantError::Internal(error.to_string()))?,
            )
            .await?;
            read_manifest_as(&temporary, &identity.slug).await?;
            tokio::fs::rename(&root, &backup).await?;
            if let Err(error) = tokio::fs::rename(&temporary, &root).await {
                let _ = tokio::fs::rename(&backup, &root).await;
                return Err(error.into());
            }
            if let Err(error) = self
                .refresh_activation_after_edit(identity, &manifest.content_hash)
                .await
            {
                let _ = tokio::fs::remove_dir_all(&root).await;
                let _ = tokio::fs::rename(&backup, &root).await;
                return Err(error);
            }
            let _ = tokio::fs::remove_dir_all(&backup).await;
            Ok::<(), AssistantError>(())
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_dir_all(&temporary).await;
        }
        result?;
        self.detail(identity, None).await
    }

    pub async fn publish_hub(
        &self,
        identity: &AssistantIdentityResponse,
        request: PublishAssistantCatalogRequest,
    ) -> Result<PublishAssistantCatalogResponse, AssistantError> {
        if identity.source != AssistantSourceResponse::TjuaeHub || !self.can_write_hub {
            return Err(AssistantError::Forbidden(
                "当前用户没有发布 TjuaeHub 助手的权限".to_owned(),
            ));
        }
        let message = request.message.trim();
        if message.is_empty() || message.len() > 500 {
            return Err(AssistantError::BadRequest(
                "发布说明不能为空且不能超过 500 个字符".to_owned(),
            ));
        }
        let hub_root = self
            .hub_worktree
            .as_ref()
            .ok_or_else(|| AssistantError::Forbidden("TjuaeHub 开发工作区不可用".to_owned()))?;
        let assistant_root = self.editable_root(identity)?;
        let assistant_workspace = assistant_root.to_string_lossy();
        let commit = self
            .git
            .commit(&assistant_workspace, message, true)
            .await
            .map_err(|error| AssistantError::Internal(error.to_string()))?;

        let mut command = CommandBuilder::clean_cli("node");
        command
            .current_dir(hub_root)
            .env("TJUAE_SOURCE_REVISION", &commit)
            .arg(".github/scripts/build-assets.js");
        let output = tokio::time::timeout(Duration::from_secs(120), command.output())
            .await
            .map_err(|_| AssistantError::Internal("TjuaeHub 索引生成超时".to_owned()))?
            .map_err(|error| AssistantError::Internal(format!("无法生成 TjuaeHub 索引：{error}")))?;
        if !output.status.success() {
            return Err(AssistantError::BadRequest(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        let hub_workspace = hub_root.to_string_lossy();
        self.git
            .stage_file(&hub_workspace, "dist/assistants.json")
            .await
            .map_err(|error| AssistantError::Internal(error.to_string()))?;
        let index_commit = self
            .git
            .commit(&hub_workspace, "chore(hub): 更新助手目录索引", false)
            .await
            .map_err(|error| AssistantError::Internal(error.to_string()))?;
        self.git
            .push(&hub_workspace)
            .await
            .map_err(|error| AssistantError::Internal(error.to_string()))?;
        Ok(PublishAssistantCatalogResponse { commit: index_commit })
    }

    pub async fn copy_to_mine(
        &self,
        source_identity: &AssistantIdentityResponse,
        request: CopyAssistantToMineRequest,
    ) -> Result<AssistantCatalogDetailResponse, AssistantError> {
        validate_slug(&request.target_slug)?;
        let detail = self.detail(source_identity, request.version.as_deref()).await?;
        let target = self.mine_root.join(&request.target_slug);
        let temporary = self
            .mine_root
            .join(format!(".{}.{}", request.target_slug, uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(&self.mine_root).await?;
        if tokio::fs::try_exists(&target).await? {
            return Err(AssistantError::Conflict(format!("助手 {} 已存在", request.target_slug)));
        }
        tokio::fs::create_dir(&temporary).await?;
        let result = async {
            for file in &detail.files {
                tjuaeui_catalog::validate_relative_file(&file.path, 0)?;
                let destination = temporary.join(&file.path);
                if let Some(parent) = destination.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                let mut bytes = self
                    .file_bytes(source_identity, Some(&detail.manifest.version), &file.path)
                    .await?;
                if file.path == MANIFEST_FILE {
                    let mut manifest: AssistantManifest = serde_json::from_slice(&bytes)
                        .map_err(|error| AssistantError::BadRequest(format!("助手清单无效：{error}")))?;
                    manifest.id.clone_from(&request.target_slug);
                    bytes = serde_json::to_vec_pretty(&manifest)
                        .map_err(|error| AssistantError::Internal(error.to_string()))?;
                }
                tokio::fs::write(destination, bytes).await?;
            }
            read_manifest_as(&temporary, &request.target_slug).await?;
            tokio::fs::rename(&temporary, &target).await?;
            Ok::<(), AssistantError>(())
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_dir_all(&temporary).await;
        }
        result?;
        let mine_identity = identity(AssistantSourceResponse::Mine, "", &request.target_slug);
        self.detail(&mine_identity, None).await
    }

    pub async fn export(
        &self,
        identity: &AssistantIdentityResponse,
        request: ExportAssistantRequest,
        additional_files: Vec<(String, Vec<u8>)>,
    ) -> Result<ExportAssistantResponse, AssistantError> {
        let detail = self.detail(identity, request.version.as_deref()).await?;
        let output = PathBuf::from(&request.output_path);
        if output.extension().and_then(|value| value.to_str()) != Some("zip") {
            return Err(AssistantError::BadRequest(
                "助手导出文件必须使用 .zip 扩展名".to_owned(),
            ));
        }
        if tokio::fs::try_exists(&output).await? {
            return Err(AssistantError::Conflict("导出文件已存在".to_owned()));
        }
        let mut files = Vec::with_capacity(detail.files.len());
        for file in &detail.files {
            files.push((
                file.path.clone(),
                self.file_bytes(identity, Some(&detail.manifest.version), &file.path)
                    .await?,
            ));
        }
        for (path, bytes) in additional_files {
            if let Some(existing) = files.iter_mut().find(|(candidate, _)| candidate == &path) {
                existing.1 = bytes;
            } else {
                files.push((path, bytes));
            }
        }
        let temporary = output.with_extension(format!("zip.{}.part", uuid::Uuid::now_v7()));
        let archive_path = temporary.clone();
        tokio::task::spawn_blocking(move || -> Result<(), AssistantError> {
            use std::io::Write;
            let file = std::fs::File::create(&archive_path)?;
            let mut archive = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .unix_permissions(0o644);
            for (path, bytes) in files {
                tjuaeui_catalog::validate_relative_file(&path, 0)?;
                archive
                    .start_file(path, options)
                    .map_err(|error| AssistantError::Internal(error.to_string()))?;
                archive.write_all(&bytes)?;
            }
            archive
                .finish()
                .map_err(|error| AssistantError::Internal(error.to_string()))?;
            Ok(())
        })
        .await
        .map_err(|error| AssistantError::Internal(error.to_string()))??;
        if let Err(error) = tokio::fs::rename(&temporary, &output).await {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(error.into());
        }
        Ok(ExportAssistantResponse {
            output_path: output.to_string_lossy().into_owned(),
        })
    }

    pub async fn portable_skill_files(
        &self,
        skills: &[AssistantDefaultRef],
    ) -> Result<Vec<(String, Vec<u8>)>, AssistantError> {
        let mut roots = Vec::new();
        for skill in skills {
            let root = match skill.source.as_str() {
                "mine" => self
                    .mine_root
                    .parent()
                    .map(|root| root.join("skills").join(&skill.slug)),
                "tjuae-hub" => self
                    .hub_worktree
                    .as_ref()
                    .map(|root| root.join("skills").join(&skill.slug)),
                _ => None,
            };
            if let Some(root) = root.filter(|root| root.is_dir()) {
                roots.push((skill.source.clone(), skill.slug.clone(), root));
            }
        }
        tokio::task::spawn_blocking(move || {
            let mut files = Vec::new();
            for (source, slug, root) in roots {
                collect_portable_directory(&root, &root, &format!("resources/skills/{source}/{slug}"), &mut files)?;
            }
            Ok(files)
        })
        .await
        .map_err(|error| AssistantError::Internal(error.to_string()))?
    }

    pub async fn replace_embedded_resources(
        &self,
        identity: &AssistantIdentityResponse,
        files: Vec<(String, Vec<u8>)>,
    ) -> Result<(), AssistantError> {
        let root = self.editable_root(identity)?;
        let parent = root
            .parent()
            .ok_or_else(|| AssistantError::Internal("助手目录缺少父目录".to_owned()))?;
        let temporary = parent.join(format!(".{}.resources.{}", identity.slug, uuid::Uuid::now_v7()));
        let backup = parent.join(format!(".{}.backup.{}", identity.slug, uuid::Uuid::now_v7()));
        let source = root.clone();
        let destination = temporary.clone();
        tokio::task::spawn_blocking(move || copy_directory(&source, &destination))
            .await
            .map_err(|error| AssistantError::Internal(error.to_string()))??;
        let result = async {
            let resources = temporary.join("resources");
            if tokio::fs::try_exists(&resources).await? {
                tokio::fs::remove_dir_all(&resources).await?;
            }
            for (path, bytes) in files {
                tjuaeui_catalog::validate_relative_file(&path, bytes.len() as u64)?;
                if !path.starts_with("resources/") {
                    return Err(AssistantError::BadRequest("助手嵌入资源路径无效".to_owned()));
                }
                let target = temporary.join(path.replace('/', std::path::MAIN_SEPARATOR_STR));
                if let Some(parent) = target.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(target, bytes).await?;
            }
            let manifest_path = temporary.join(MANIFEST_FILE);
            let mut manifest: AssistantManifest = serde_json::from_slice(&tokio::fs::read(&manifest_path).await?)
                .map_err(|error| AssistantError::BadRequest(format!("助手清单无效：{error}")))?;
            manifest.content_hash = assistant_directory_digest(&temporary)?;
            tokio::fs::write(
                manifest_path,
                serde_json::to_vec_pretty(&manifest).map_err(|error| AssistantError::Internal(error.to_string()))?,
            )
            .await?;
            tokio::fs::rename(&root, &backup).await?;
            if let Err(error) = tokio::fs::rename(&temporary, &root).await {
                let _ = tokio::fs::rename(&backup, &root).await;
                return Err(error.into());
            }
            if let Err(error) = self
                .refresh_activation_after_edit(identity, &manifest.content_hash)
                .await
            {
                let _ = tokio::fs::remove_dir_all(&root).await;
                let _ = tokio::fs::rename(&backup, &root).await;
                return Err(error);
            }
            let _ = tokio::fs::remove_dir_all(&backup).await;
            Ok::<(), AssistantError>(())
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_dir_all(&temporary).await;
        }
        result?;
        Ok(())
    }

    pub async fn delete_mine(&self, identity: &AssistantIdentityResponse) -> Result<(), AssistantError> {
        if identity.source != AssistantSourceResponse::Mine || !identity.namespace.is_empty() {
            return Err(AssistantError::Forbidden("只能删除“我的助手”".to_owned()));
        }
        if is_system_assistant(identity) {
            return Err(AssistantError::Forbidden("TjuaeUI 管家是系统助手，不能删除".to_owned()));
        }
        validate_slug(&identity.slug)?;
        let root = self.mine_root.join(&identity.slug);
        if !tokio::fs::try_exists(&root).await? {
            return Err(AssistantError::NotFound(identity.slug.clone()));
        }
        tokio::fs::remove_dir_all(root).await?;
        self.preferences
            .delete(source_id(identity.source), &identity.namespace, &identity.slug)
            .await?;
        Ok(())
    }

    pub async fn list(
        &self,
        source: AssistantSourceResponse,
        query: &str,
        sort: &str,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<AssistantCatalogPageResponse, AssistantError> {
        let preferences = self.preference_map().await?;
        let mut items = match source {
            AssistantSourceResponse::Mine => self.list_mine(&preferences).await?,
            AssistantSourceResponse::TjuaeHub => self.list_hub(&preferences).await?,
        };
        let needle = query.trim().to_lowercase();
        if !needle.is_empty() {
            items.retain(|item| {
                item.name.to_lowercase().contains(&needle)
                    || item.description.to_lowercase().contains(&needle)
                    || item.identity.slug.to_lowercase().contains(&needle)
                    || item.tags.iter().any(|tag| tag.to_lowercase().contains(&needle))
            });
        }
        match sort {
            "name-desc" => items.sort_by(|left, right| right.name.cmp(&left.name)),
            _ => items.sort_by(|left, right| left.name.cmp(&right.name)),
        }
        let total = items.len() as u64;
        let offset = cursor.and_then(|value| value.parse::<usize>().ok()).unwrap_or_default();
        let limit = limit.clamp(1, 100) as usize;
        let page = items.into_iter().skip(offset).take(limit).collect::<Vec<_>>();
        let next = offset + page.len();
        Ok(AssistantCatalogPageResponse {
            items: page,
            total,
            next_cursor: (next < total as usize).then(|| next.to_string()),
        })
    }

    pub async fn detail(
        &self,
        identity: &AssistantIdentityResponse,
        version: Option<&str>,
    ) -> Result<AssistantCatalogDetailResponse, AssistantError> {
        let preferences = self.preference_map().await?;
        match identity.source {
            AssistantSourceResponse::Mine => self.mine_detail(identity, version, &preferences).await,
            AssistantSourceResponse::TjuaeHub => self.hub_detail(identity, version, &preferences).await,
        }
    }

    pub async fn file_content(
        &self,
        identity: &AssistantIdentityResponse,
        version: Option<&str>,
        path: &str,
    ) -> Result<AssistantCatalogFileContentResponse, AssistantError> {
        let bytes = self.file_bytes(identity, version, path).await?;
        let content = String::from_utf8(bytes)
            .map_err(|_| AssistantError::BadRequest(format!("助手文件不是 UTF-8 文本：{path}")))?;
        Ok(AssistantCatalogFileContentResponse {
            path: path.to_owned(),
            size: content.len() as u64,
            content,
        })
    }

    /// Read a binary catalog asset. The shared file resolver first verifies
    /// that the selected catalog revision declares `path`, so this boundary
    /// cannot become an arbitrary local-file reader.
    pub async fn asset_bytes(
        &self,
        identity: &AssistantIdentityResponse,
        version: Option<&str>,
        path: &str,
    ) -> Result<Vec<u8>, AssistantError> {
        tjuaeui_catalog::validate_relative_file(path, 0)?;
        match identity.source {
            AssistantSourceResponse::Mine => {
                let root = self.mine_root.join(&identity.slug);
                let declared = list_local_files(&root)?.into_iter().any(|file| file.path == path);
                if !declared {
                    return Err(AssistantError::NotFound(format!("助手文件 {path}")));
                }
                tjuaeui_catalog::read_local_file(&root, path)
                    .await
                    .map_err(AssistantError::from)
            }
            AssistantSourceResponse::TjuaeHub => {
                if let Some(root) = self.hub_worktree_root_for_version(identity, version).await? {
                    let declared = list_local_files(&root)?.into_iter().any(|file| file.path == path);
                    if !declared {
                        return Err(AssistantError::NotFound(format!("助手文件 {path}")));
                    }
                    tjuaeui_catalog::read_local_file(&root, path)
                        .await
                        .map_err(AssistantError::from)
                } else {
                    let index = self.hub_index().await?;
                    let entry = index
                        .assistants
                        .iter()
                        .find(|entry| entry.id == identity.slug)
                        .ok_or_else(|| AssistantError::NotFound(identity.slug.clone()))?;
                    let selected_version = version.unwrap_or(&entry.latest_version);
                    let selected = entry
                        .version(selected_version)
                        .ok_or_else(|| AssistantError::NotFound(selected_version.to_owned()))?;
                    let declared = selected
                        .files
                        .iter()
                        .find(|file| file.path == path)
                        .ok_or_else(|| AssistantError::NotFound(format!("助手文件 {path}")))?;
                    let cache_key = format!(
                        "{}:{}:{}:{}:{path}",
                        source_id(identity.source),
                        identity.namespace,
                        identity.slug,
                        selected.revision
                    );
                    if let Some(bytes) = self.hub_asset_cache.read().await.get(&cache_key).cloned() {
                        return Ok(bytes);
                    }
                    let bytes = tjuaeui_catalog::fetch_github_revision_file(
                        &index.repository,
                        &selected.revision,
                        &entry.path,
                        declared,
                    )
                    .await?;
                    self.hub_asset_cache.write().await.insert(cache_key, bytes.clone());
                    Ok(bytes)
                }
            }
        }
    }

    async fn file_bytes(
        &self,
        identity: &AssistantIdentityResponse,
        version: Option<&str>,
        path: &str,
    ) -> Result<Vec<u8>, AssistantError> {
        let detail = self.detail(identity, version).await?;
        let declared = detail
            .files
            .iter()
            .find(|file| file.path == path)
            .ok_or_else(|| AssistantError::NotFound(format!("助手文件 {path}")))?;
        let bytes = match identity.source {
            AssistantSourceResponse::Mine => {
                tjuaeui_catalog::read_local_file(&self.mine_root.join(&identity.slug), path).await?
            }
            AssistantSourceResponse::TjuaeHub => {
                if let Some(root) = self.hub_worktree_root_for_version(identity, version).await? {
                    return Ok(tjuaeui_catalog::read_local_file(&root, path).await?);
                }
                let index = self.hub_index().await?;
                let entry = index
                    .assistants
                    .iter()
                    .find(|entry| entry.id == identity.slug)
                    .ok_or_else(|| AssistantError::NotFound(identity.slug.clone()))?;
                let selected_version = version.unwrap_or(&entry.latest_version);
                let selected = entry
                    .version(selected_version)
                    .ok_or_else(|| AssistantError::NotFound(selected_version.to_owned()))?;
                tjuaeui_catalog::fetch_github_revision_file(
                    &index.repository,
                    &selected.revision,
                    &entry.path,
                    &CatalogFile {
                        path: declared.path.clone(),
                        size: declared.size,
                        sha256: declared.sha256.clone(),
                    },
                )
                .await?
            }
        };
        Ok(bytes)
    }

    pub async fn compare_versions(
        &self,
        identity: &AssistantIdentityResponse,
        base_version: &str,
        target_version: &str,
    ) -> Result<AssistantVersionComparisonResponse, AssistantError> {
        if base_version == target_version {
            return Err(AssistantError::BadRequest("请选择两个不同版本进行比较".to_owned()));
        }
        let base = self.detail(identity, Some(base_version)).await?;
        let target = self.detail(identity, Some(target_version)).await?;
        let paths = base
            .files
            .iter()
            .map(|file| file.path.clone())
            .chain(target.files.iter().map(|file| file.path.clone()))
            .collect::<BTreeSet<_>>();
        let mut base_files = BTreeMap::new();
        let mut target_files = BTreeMap::new();
        for path in paths {
            if base.files.iter().any(|file| file.path == path) {
                base_files.insert(
                    path.clone(),
                    self.file_content(identity, Some(base_version), &path)
                        .await
                        .ok()
                        .map(|file| file.content),
                );
            }
            if target.files.iter().any(|file| file.path == path) {
                target_files.insert(
                    path.clone(),
                    self.file_content(identity, Some(target_version), &path)
                        .await
                        .ok()
                        .map(|file| file.content),
                );
            }
        }
        let files = tjuaeui_catalog::compare_text_files(&base_files, &target_files)
            .into_iter()
            .map(|file| AssistantVersionFileDiffResponse {
                path: file.path,
                status: serde_json::to_value(file.status)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "modified".to_owned()),
                binary: file.binary,
                base_content: file.base_content,
                target_content: file.target_content,
            })
            .collect();
        Ok(AssistantVersionComparisonResponse {
            base_version: base_version.to_owned(),
            target_version: target_version.to_owned(),
            files,
        })
    }

    pub async fn update_preferences(
        &self,
        identity: &AssistantIdentityResponse,
        request: UpdateAssistantCatalogPreferencesRequest,
    ) -> Result<AssistantOperationResponse, AssistantError> {
        if is_system_assistant(identity) && !request.enabled {
            return Err(AssistantError::Forbidden(
                "TjuaeUI 管家是系统助手，必须始终保持启用".to_owned(),
            ));
        }
        let detail = self.detail(identity, request.selected_version.as_deref()).await?;
        let current = self
            .preferences
            .get(source_id(identity.source), &identity.namespace, &identity.slug)
            .await?;
        let selected_version = request
            .selected_version
            .as_deref()
            .unwrap_or(&detail.item.latest_version);
        if request.enabled
            && current.as_ref().is_none_or(|row| {
                row.activation_status != "ready"
                    || row.selected_version.as_deref() != Some(selected_version)
                    || row.follow_latest != request.follow_latest
            })
        {
            return Err(AssistantError::Conflict(
                "启用助手或切换版本必须先完成资源检查与逐项确认".to_owned(),
            ));
        }
        let activation_status = if request.enabled { "ready" } else { "inactive" };
        let empty_bindings = "{}";
        let row = self
            .preferences
            .upsert(UpsertAssistantUserPreferenceParams {
                source: source_id(identity.source),
                namespace: &identity.namespace,
                slug: &identity.slug,
                selected_version: Some(selected_version),
                follow_latest: request.follow_latest,
                enabled: request.enabled,
                activation_status,
                activation_fingerprint: current.as_ref().and_then(|row| row.activation_fingerprint.as_deref()),
                resource_bindings: current
                    .as_ref()
                    .map(|row| row.resource_bindings.as_str())
                    .unwrap_or(empty_bindings),
                runtime_overrides: current
                    .as_ref()
                    .map(|row| row.runtime_overrides.as_str())
                    .unwrap_or(empty_bindings),
                sort_order: request
                    .sort_order
                    .or_else(|| current.as_ref().map(|row| row.sort_order))
                    .unwrap_or(0),
                last_used_at: current.as_ref().and_then(|row| row.last_used_at),
            })
            .await?;
        Ok(AssistantOperationResponse {
            identity: identity.clone(),
            version: row
                .selected_version
                .unwrap_or_else(|| detail.item.latest_version.clone()),
            enabled: row.enabled,
            activation_status: row.activation_status,
        })
    }

    async fn list_mine(
        &self,
        preferences: &HashMap<IdentityKey, AssistantUserPreferenceRow>,
    ) -> Result<Vec<AssistantCatalogItemResponse>, AssistantError> {
        tokio::fs::create_dir_all(&self.mine_root).await?;
        let mut entries = tokio::fs::read_dir(&self.mine_root).await?;
        let mut items = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            if !entry.file_type().await?.is_dir() {
                continue;
            }
            let manifest = match read_manifest(&entry.path()).await {
                Ok(manifest) => manifest,
                Err(error) => {
                    tracing::warn!(path = %entry.path().display(), %error, "跳过无效的本地助手包");
                    continue;
                }
            };
            let identity = identity(AssistantSourceResponse::Mine, "", &manifest.id);
            items.push(item_from_manifest(
                identity.clone(),
                &manifest,
                None,
                Some(&manifest.version),
                true,
                preferences.get(&IdentityKey::from(&identity)),
            ));
        }
        Ok(items)
    }

    async fn list_hub(
        &self,
        preferences: &HashMap<IdentityKey, AssistantUserPreferenceRow>,
    ) -> Result<Vec<AssistantCatalogItemResponse>, AssistantError> {
        let index = self.hub_index().await?;
        Ok(index
            .assistants
            .iter()
            .filter(|entry| entry.id != SYSTEM_ASSISTANT_SLUG)
            .map(|entry| {
                let identity = identity(AssistantSourceResponse::TjuaeHub, "official", &entry.id);
                item_from_entry(
                    entry,
                    identity.clone(),
                    self.can_write_hub,
                    preferences.get(&IdentityKey::from(&identity)),
                )
            })
            .collect())
    }

    async fn mine_detail(
        &self,
        identity: &AssistantIdentityResponse,
        requested_version: Option<&str>,
        preferences: &HashMap<IdentityKey, AssistantUserPreferenceRow>,
    ) -> Result<AssistantCatalogDetailResponse, AssistantError> {
        let root = self.mine_root.join(&identity.slug);
        let manifest = read_manifest(&root).await?;
        if requested_version.is_some_and(|version| version != manifest.version) {
            return Err(AssistantError::NotFound(
                requested_version.unwrap_or_default().to_owned(),
            ));
        }
        let files = list_local_files(&root)?;
        let readme = tokio::fs::read_to_string(root.join(ENTRY_FILE)).await?;
        Ok(detail_from_manifest(
            identity.clone(),
            manifest,
            readme,
            files,
            "working-tree".to_owned(),
            preferences.get(&IdentityKey::from(identity)),
        ))
    }

    async fn hub_detail(
        &self,
        identity: &AssistantIdentityResponse,
        requested_version: Option<&str>,
        preferences: &HashMap<IdentityKey, AssistantUserPreferenceRow>,
    ) -> Result<AssistantCatalogDetailResponse, AssistantError> {
        if identity.slug == SYSTEM_ASSISTANT_SLUG {
            return Err(AssistantError::NotFound(identity.slug.clone()));
        }
        let index = self.hub_index().await?;
        let entry = index
            .assistants
            .iter()
            .find(|entry| entry.id == identity.slug)
            .ok_or_else(|| AssistantError::NotFound(identity.slug.clone()))?;
        let selected_version = requested_version.unwrap_or(&entry.latest_version);
        if let Some(root) = self.hub_worktree_root_for_version(identity, requested_version).await? {
            let manifest = read_manifest(&root).await?;
            let files = list_local_files(&root)?;
            let readme = tokio::fs::read_to_string(root.join(&manifest.instructions.default)).await?;
            let mut detail = detail_from_manifest(
                identity.clone(),
                manifest,
                readme,
                files,
                "working-tree".to_owned(),
                preferences.get(&IdentityKey::from(identity)),
            );
            detail.item.editable = self.can_write_hub;
            detail.item.latest_version.clone_from(&entry.latest_version);
            detail.versions = entry.versions.iter().map(version_response).collect();
            return Ok(detail);
        }
        let selected = entry
            .version(selected_version)
            .ok_or_else(|| AssistantError::NotFound(selected_version.to_owned()))?;
        let manifest = if entry.manifest.version == selected_version {
            entry.manifest.clone()
        } else {
            let manifest_file = selected
                .files
                .iter()
                .find(|file| file.path == MANIFEST_FILE)
                .ok_or_else(|| AssistantError::BadRequest("助手版本缺少 _meta.json".to_owned()))?;
            let bytes = tjuaeui_catalog::fetch_github_revision_file(
                &index.repository,
                &selected.revision,
                &entry.path,
                manifest_file,
            )
            .await?;
            serde_json::from_slice(&bytes)
                .map_err(|error| AssistantError::BadRequest(format!("助手清单无效：{error}")))?
        };
        validate_manifest(&manifest, &entry.id)?;
        let mut item = item_from_manifest(
            identity.clone(),
            &manifest,
            entry.avatar.as_deref(),
            Some(selected_version),
            false,
            preferences.get(&IdentityKey::from(identity)),
        );
        item.latest_version.clone_from(&entry.latest_version);
        Ok(AssistantCatalogDetailResponse {
            item,
            manifest: manifest_response(&manifest),
            readme: selected.readme.clone(),
            files: selected.files.iter().cloned().map(file_response).collect(),
            versions: entry.versions.iter().map(version_response).collect(),
        })
    }

    async fn hub_index(&self) -> Result<AssistantMarketIndex, AssistantError> {
        if self.hub_worktree.is_none()
            && let Some((cached_at, index)) = self.hub_index_cache.read().await.as_ref()
            && cached_at.elapsed() < HUB_INDEX_CACHE_TTL
        {
            return Ok(index.clone());
        }
        let configured = std::env::var(HUB_INDEX_ENV).ok();
        let worktree_index = self.hub_worktree.as_ref().map(|root| root.join("dist/assistants.json"));
        let local = worktree_index.as_deref().or(self.hub_index_snapshot.as_deref());
        let index: AssistantMarketIndex =
            tjuaeui_catalog::load_json(configured.as_deref(), local, HUB_INDEX_URL, "assistants").await?;
        if index.schema_version != 1 || index.market.id != "tjuae-hub" {
            return Err(AssistantError::BadRequest("TjuaeHub 助手索引版本或标识无效".to_owned()));
        }
        if self.hub_worktree.is_none() {
            *self.hub_index_cache.write().await = Some((tokio::time::Instant::now(), index.clone()));
        }
        Ok(index)
    }

    /// 当请求的版本与开发工作树中的助手清单一致时直接读取工作树。
    ///
    /// TjuaeHub 索引的 revision 只能指向 Git 已提交内容，而开发工作树允许在
    /// 发布前预览尚未提交的助手。调用方即使显式传入当前版本，也不能因此绕过
    /// 工作树并请求一个尚不存在于远程 revision 的文件。
    async fn hub_worktree_root_for_version(
        &self,
        identity: &AssistantIdentityResponse,
        requested_version: Option<&str>,
    ) -> Result<Option<PathBuf>, AssistantError> {
        if identity.source != AssistantSourceResponse::TjuaeHub || identity.namespace != "official" {
            return Ok(None);
        }
        validate_slug(&identity.slug)?;
        let Some(root) = self
            .hub_worktree
            .as_ref()
            .map(|worktree| worktree.join("assistants").join(&identity.slug))
            .filter(|root| root.is_dir())
        else {
            return Ok(None);
        };
        let manifest = read_manifest(&root).await?;
        Ok(requested_version
            .is_none_or(|version| version == manifest.version)
            .then_some(root))
    }

    fn editable_root(&self, identity: &AssistantIdentityResponse) -> Result<PathBuf, AssistantError> {
        validate_slug(&identity.slug)?;
        match identity.source {
            AssistantSourceResponse::Mine if identity.namespace.is_empty() => Ok(self.mine_root.join(&identity.slug)),
            AssistantSourceResponse::TjuaeHub if identity.namespace == "official" && self.can_write_hub => self
                .hub_worktree
                .as_ref()
                .map(|root| root.join("assistants").join(&identity.slug))
                .ok_or_else(|| AssistantError::Forbidden("TjuaeHub 开发工作区不可用".to_owned())),
            AssistantSourceResponse::Mine => Err(AssistantError::Forbidden("我的助手不能使用命名空间".to_owned())),
            AssistantSourceResponse::TjuaeHub => Err(AssistantError::Forbidden(
                "当前用户没有编辑 TjuaeHub 助手的权限".to_owned(),
            )),
        }
    }

    async fn refresh_activation_after_edit(
        &self,
        identity: &AssistantIdentityResponse,
        content_hash: &str,
    ) -> Result<(), AssistantError> {
        let Some(current) = self
            .preferences
            .get(source_id(identity.source), &identity.namespace, &identity.slug)
            .await?
        else {
            return Ok(());
        };
        let enabled = current.enabled || is_system_assistant(identity);
        self.preferences
            .upsert(UpsertAssistantUserPreferenceParams {
                source: &current.source,
                namespace: &current.namespace,
                slug: &current.slug,
                selected_version: current.selected_version.as_deref(),
                follow_latest: current.follow_latest,
                enabled,
                activation_status: if enabled { "ready" } else { "pending" },
                activation_fingerprint: enabled.then_some(content_hash),
                resource_bindings: if enabled { &current.resource_bindings } else { "{}" },
                runtime_overrides: &current.runtime_overrides,
                sort_order: current.sort_order,
                last_used_at: current.last_used_at,
            })
            .await?;
        Ok(())
    }

    async fn preference_map(&self) -> Result<HashMap<IdentityKey, AssistantUserPreferenceRow>, AssistantError> {
        Ok(self
            .preferences
            .list()
            .await?
            .into_iter()
            .map(|row| (IdentityKey::from(&row), row))
            .collect())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AssistantMarketIndex {
    #[serde(rename = "$schema")]
    _schema: String,
    schema_version: u32,
    market: CatalogProvider,
    repository: String,
    #[allow(dead_code)]
    revision: String,
    assistants: Vec<AssistantMarketEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AssistantMarketEntry {
    id: String,
    path: String,
    name: String,
    description: String,
    manifest: AssistantManifest,
    avatar: Option<String>,
    categories: Vec<String>,
    tags: Vec<String>,
    latest_version: String,
    versions: Vec<CatalogVersion>,
}

impl AssistantMarketEntry {
    fn version(&self, version: &str) -> Option<&CatalogVersion> {
        self.versions.iter().find(|candidate| candidate.version == version)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AssistantManifest {
    #[serde(rename = "$schema")]
    schema: String,
    format: String,
    format_version: u32,
    id: String,
    version: String,
    name: String,
    #[serde(default)]
    name_i18n: BTreeMap<String, String>,
    description: String,
    #[serde(default)]
    description_i18n: BTreeMap<String, String>,
    #[serde(default)]
    categories: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
    avatar: Option<String>,
    instructions: InstructionManifest,
    defaults: DefaultsManifest,
    requirements: RequirementsManifest,
    #[serde(default)]
    recommended_prompts: Vec<String>,
    #[serde(default)]
    recommended_prompts_i18n: BTreeMap<String, Vec<String>>,
    content_hash: String,
    #[serde(default)]
    extensions: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InstructionManifest {
    default: String,
    #[serde(default)]
    locales: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DefaultsManifest {
    agent: Option<String>,
    #[serde(default)]
    model: ScalarDefaultManifest,
    #[serde(default)]
    permission: ScalarDefaultManifest,
    #[serde(default)]
    thought_level: ScalarDefaultManifest,
    #[serde(default)]
    skills: Vec<AssistantDefaultRef>,
    #[serde(default)]
    mcps: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScalarDefaultManifest {
    #[serde(default = "auto_mode")]
    mode: String,
    value: Option<String>,
}

fn auto_mode() -> String {
    "auto".to_owned()
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RequirementsManifest {
    #[serde(default)]
    skills: Vec<SkillRequirementManifest>,
    #[serde(default)]
    mcps: Vec<NamedRequirementManifest>,
    #[serde(default)]
    models: Vec<NamedRequirementManifest>,
    #[serde(default)]
    agents: Vec<NamedRequirementManifest>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SkillRequirementManifest {
    key: String,
    required: bool,
    identity: AssistantDefaultRef,
    #[serde(default)]
    version: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NamedRequirementManifest {
    key: String,
    required: bool,
    #[serde(default)]
    preferred_mcp_ids: Vec<String>,
    #[serde(default)]
    preferred_model_ids: Vec<String>,
    #[serde(default)]
    preferred_agent_ids: Vec<String>,
    #[serde(default)]
    version_requirement: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct IdentityKey(String, String, String);

impl From<&AssistantIdentityResponse> for IdentityKey {
    fn from(value: &AssistantIdentityResponse) -> Self {
        Self(
            source_id(value.source).to_owned(),
            value.namespace.clone(),
            value.slug.clone(),
        )
    }
}

impl From<&AssistantUserPreferenceRow> for IdentityKey {
    fn from(value: &AssistantUserPreferenceRow) -> Self {
        Self(value.source.clone(), value.namespace.clone(), value.slug.clone())
    }
}

impl From<CatalogError> for AssistantError {
    fn from(value: CatalogError) -> Self {
        match value {
            CatalogError::NotFound(message) => Self::NotFound(message),
            CatalogError::InvalidRequest(message) | CatalogError::InvalidContent(message) => Self::BadRequest(message),
            CatalogError::Transport(message) => Self::Internal(message),
            CatalogError::Io(error) => Self::Internal(error.to_string()),
        }
    }
}

impl From<std::io::Error> for AssistantError {
    fn from(value: std::io::Error) -> Self {
        Self::Internal(value.to_string())
    }
}

fn scalar_default(value: AssistantDefaultScalar) -> ScalarDefaultManifest {
    ScalarDefaultManifest {
        mode: value.mode,
        value: value.value.filter(|item| !item.trim().is_empty()),
    }
}

fn requirements_for_defaults(defaults: &DefaultsManifest) -> RequirementsManifest {
    let skills = defaults
        .skills
        .iter()
        .map(|identity| SkillRequirementManifest {
            key: format!("skill:{}:{}:{}", identity.source, identity.namespace, identity.slug),
            required: true,
            identity: identity.clone(),
            version: None,
        })
        .collect();
    let mcps = defaults
        .mcps
        .iter()
        .map(|id| NamedRequirementManifest {
            key: format!("mcp:{id}"),
            required: true,
            preferred_mcp_ids: vec![id.clone()],
            preferred_model_ids: Vec::new(),
            preferred_agent_ids: Vec::new(),
            version_requirement: None,
        })
        .collect();
    let models = defaults
        .model
        .value
        .iter()
        .map(|id| NamedRequirementManifest {
            key: format!("model:{id}"),
            required: defaults.model.mode == "fixed",
            preferred_mcp_ids: Vec::new(),
            preferred_model_ids: vec![id.clone()],
            preferred_agent_ids: Vec::new(),
            version_requirement: None,
        })
        .collect();
    let agents = defaults
        .agent
        .iter()
        .map(|id| NamedRequirementManifest {
            key: format!("agent:{id}"),
            required: true,
            preferred_mcp_ids: Vec::new(),
            preferred_model_ids: Vec::new(),
            preferred_agent_ids: vec![id.clone()],
            version_requirement: None,
        })
        .collect();
    RequirementsManifest {
        skills,
        mcps,
        models,
        agents,
    }
}

fn decode_avatar_data_url(data_url: &str) -> Result<(&'static str, Vec<u8>), AssistantError> {
    let (media_type, encoded) = data_url
        .split_once(",")
        .ok_or_else(|| AssistantError::BadRequest("头像数据格式无效".to_owned()))?;
    let extension = match media_type {
        "data:image/png;base64" => "png",
        "data:image/jpeg;base64" => "jpg",
        "data:image/webp;base64" => "webp",
        "data:image/gif;base64" => "gif",
        "data:image/svg+xml;base64" => "svg",
        _ => return Err(AssistantError::BadRequest("头像格式不受支持".to_owned())),
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| AssistantError::BadRequest("头像数据无法解码".to_owned()))?;
    if bytes.is_empty() || bytes.len() > 5 * 1024 * 1024 {
        return Err(AssistantError::BadRequest("头像大小必须在 5 MB 以内".to_owned()));
    }
    Ok((extension, bytes))
}

fn unique_manifest_values(
    values: Vec<String>,
    max_items: usize,
    max_length: usize,
    label: &str,
) -> Result<Vec<String>, AssistantError> {
    let mut seen = BTreeSet::new();
    let values = values
        .into_iter()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty() && seen.insert(value.clone()))
        .collect::<Vec<_>>();
    if values.len() > max_items || values.iter().any(|value| value.chars().count() > max_length) {
        return Err(AssistantError::BadRequest(format!(
            "{label}最多 {max_items} 项，单项不能超过 {max_length} 个字符"
        )));
    }
    Ok(values)
}

fn source_id(source: AssistantSourceResponse) -> &'static str {
    match source {
        AssistantSourceResponse::Mine => "mine",
        AssistantSourceResponse::TjuaeHub => "tjuae-hub",
    }
}

fn parse_source_id(source: &str) -> Result<AssistantSourceResponse, AssistantError> {
    match source {
        "mine" => Ok(AssistantSourceResponse::Mine),
        "tjuae-hub" => Ok(AssistantSourceResponse::TjuaeHub),
        _ => Err(AssistantError::BadRequest(format!("未知助手来源：{source}"))),
    }
}

/// 稳定且无歧义的运行时标识。来源、命名空间、slug 都来自受校验目录字段，
/// 不再复用可能跨来源重名的裸 slug。
pub fn runtime_id(identity: &AssistantIdentityResponse) -> String {
    format!(
        "{}:{}:{}",
        source_id(identity.source),
        identity.namespace,
        identity.slug
    )
}

pub fn parse_runtime_id(value: &str) -> Result<AssistantIdentityResponse, AssistantError> {
    let mut parts = value.splitn(3, ':');
    let source = parts.next().unwrap_or_default();
    let namespace = parts.next().unwrap_or_default();
    let slug = parts.next().unwrap_or_default();
    if source.is_empty() || slug.is_empty() {
        return Err(AssistantError::BadRequest("助手运行时标识无效".to_owned()));
    }
    validate_slug(slug)?;
    Ok(AssistantIdentityResponse {
        source: parse_source_id(source)?,
        namespace: namespace.to_owned(),
        slug: slug.to_owned(),
    })
}

fn identity(source: AssistantSourceResponse, namespace: &str, slug: &str) -> AssistantIdentityResponse {
    AssistantIdentityResponse {
        source,
        namespace: namespace.to_owned(),
        slug: slug.to_owned(),
    }
}

fn default_preferences(row: Option<&AssistantUserPreferenceRow>) -> AssistantPreferencesCatalogResponse {
    row.map(|row| AssistantPreferencesCatalogResponse {
        selected_version: row.selected_version.clone(),
        follow_latest: row.follow_latest,
        enabled: row.enabled,
        activation_status: row.activation_status.clone(),
        sort_order: row.sort_order,
        last_used_at: row.last_used_at,
    })
    .unwrap_or(AssistantPreferencesCatalogResponse {
        selected_version: None,
        follow_latest: true,
        enabled: false,
        activation_status: "inactive".to_owned(),
        sort_order: 0,
        last_used_at: None,
    })
}

fn item_from_entry(
    entry: &AssistantMarketEntry,
    identity: AssistantIdentityResponse,
    editable: bool,
    preference: Option<&AssistantUserPreferenceRow>,
) -> AssistantCatalogItemResponse {
    let avatar_url = catalog_avatar_url(&identity, entry.avatar.as_deref(), Some(&entry.latest_version));
    AssistantCatalogItemResponse {
        identity,
        name: entry.name.clone(),
        description: entry.description.clone(),
        avatar_url,
        latest_version: entry.latest_version.clone(),
        categories: entry.categories.clone(),
        tags: entry.tags.clone(),
        editable,
        system: false,
        can_disable: true,
        can_delete: false,
        preferences: default_preferences(preference),
    }
}

fn item_from_manifest(
    identity: AssistantIdentityResponse,
    manifest: &AssistantManifest,
    avatar: Option<&str>,
    version: Option<&str>,
    editable: bool,
    preference: Option<&AssistantUserPreferenceRow>,
) -> AssistantCatalogItemResponse {
    let avatar_url = catalog_avatar_url(&identity, avatar.or(manifest.avatar.as_deref()), version);
    let system = is_system_assistant(&identity);
    let can_delete = identity.source == AssistantSourceResponse::Mine && !system;
    AssistantCatalogItemResponse {
        identity,
        name: manifest.name.clone(),
        description: manifest.description.clone(),
        avatar_url,
        latest_version: manifest.version.clone(),
        categories: manifest.categories.clone(),
        tags: manifest.tags.clone(),
        editable,
        system,
        can_disable: !system,
        can_delete,
        preferences: default_preferences(preference),
    }
}

fn is_system_assistant(identity: &AssistantIdentityResponse) -> bool {
    identity.source == AssistantSourceResponse::Mine
        && identity.namespace.is_empty()
        && identity.slug == SYSTEM_ASSISTANT_SLUG
}

fn system_assistant_manifest(content_hash: String) -> AssistantManifest {
    let skill = |slug: &str| AssistantDefaultRef {
        source: "tjuae-hub".to_owned(),
        namespace: "official".to_owned(),
        slug: slug.to_owned(),
    };
    let skill_requirement = |key: &str, slug: &str| SkillRequirementManifest {
        key: key.to_owned(),
        required: true,
        identity: skill(slug),
        version: None,
    };
    AssistantManifest {
        schema: "https://raw.githubusercontent.com/liangboqiang/TjuaeHub/main/schemas/tjuae-assistant.v1.schema.json"
            .to_owned(),
        format: "tjuae-assistant".to_owned(),
        format_version: 1,
        id: SYSTEM_ASSISTANT_SLUG.to_owned(),
        version: "1.0.0".to_owned(),
        name: "TjuaeUI管家".to_owned(),
        name_i18n: BTreeMap::from([
            ("en-US".to_owned(), "TjuaeUI Butler".to_owned()),
            ("zh-CN".to_owned(), "TjuaeUI管家".to_owned()),
        ]),
        description: "配置、诊断并管理 TjuaeUI 的常驻系统管家。".to_owned(),
        description_i18n: BTreeMap::from([
            (
                "en-US".to_owned(),
                "The always-on system butler for configuring, diagnosing, and managing TjuaeUI.".to_owned(),
            ),
            (
                "zh-CN".to_owned(),
                "配置、诊断并管理 TjuaeUI 的常驻系统管家。".to_owned(),
            ),
        ]),
        categories: Vec::new(),
        tags: Vec::new(),
        avatar: Some("/api/assets/logos/brand/tjuae-cli.svg".to_owned()),
        instructions: InstructionManifest {
            default: ENTRY_FILE.to_owned(),
            locales: BTreeMap::new(),
        },
        defaults: DefaultsManifest {
            agent: Some("tjuaecli".to_owned()),
            model: ScalarDefaultManifest::default(),
            permission: ScalarDefaultManifest::default(),
            thought_level: ScalarDefaultManifest::default(),
            skills: vec![
                skill("tjuaeui-config"),
                skill("tjuaeui-troubleshooting"),
                skill("tjuaeui-webui-public"),
            ],
            mcps: Vec::new(),
        },
        requirements: RequirementsManifest {
            skills: vec![
                skill_requirement("skill-config", "tjuaeui-config"),
                skill_requirement("skill-troubleshooting", "tjuaeui-troubleshooting"),
                skill_requirement("skill-webui-public", "tjuaeui-webui-public"),
            ],
            mcps: Vec::new(),
            models: Vec::new(),
            agents: vec![NamedRequirementManifest {
                key: "primary-agent".to_owned(),
                required: true,
                preferred_mcp_ids: Vec::new(),
                preferred_model_ids: Vec::new(),
                preferred_agent_ids: vec!["tjuaecli".to_owned()],
                version_requirement: None,
            }],
        },
        recommended_prompts: vec![
            "添加一个新的 LLM 模型和 API Key，并设为默认模型".to_owned(),
            "帮我配置远程访问，让我在外面用手机也能打开 TjuaeUI".to_owned(),
            "有个会话卡住了，帮我诊断哪里出了问题".to_owned(),
            "创建一个新助手，并给它绑定一个技能".to_owned(),
        ],
        recommended_prompts_i18n: BTreeMap::new(),
        content_hash,
        extensions: BTreeMap::new(),
    }
}

fn detail_from_manifest(
    identity: AssistantIdentityResponse,
    manifest: AssistantManifest,
    readme: String,
    files: Vec<CatalogFile>,
    revision: String,
    preference: Option<&AssistantUserPreferenceRow>,
) -> AssistantCatalogDetailResponse {
    let version = manifest.version.clone();
    AssistantCatalogDetailResponse {
        item: item_from_manifest(identity, &manifest, None, Some(&version), true, preference),
        manifest: manifest_response(&manifest),
        readme,
        files: files.iter().cloned().map(file_response).collect(),
        versions: vec![AssistantVersionResponse {
            version: manifest.version,
            revision,
            digest: manifest.content_hash,
        }],
    }
}

fn catalog_avatar_url(
    identity: &AssistantIdentityResponse,
    avatar: Option<&str>,
    version: Option<&str>,
) -> Option<String> {
    let avatar = avatar.map(str::trim).filter(|value| !value.is_empty())?;
    if avatar.starts_with("http://")
        || avatar.starts_with("https://")
        || avatar.starts_with("data:")
        || avatar.starts_with('/')
        || (!avatar.contains('/') && !avatar.contains('.'))
    {
        return Some(avatar.to_owned());
    }
    if tjuaeui_catalog::validate_relative_file(avatar, 0).is_err() {
        return None;
    }
    let namespace = if identity.namespace.is_empty() {
        "~"
    } else {
        identity.namespace.as_str()
    };
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("path", avatar);
    if let Some(version) = version.filter(|value| !value.is_empty()) {
        query.append_pair("version", version);
    }
    Some(format!(
        "/api/assistant-assets/{}/{}/{}?{}",
        source_id(identity.source),
        namespace,
        identity.slug,
        query.finish()
    ))
}

fn manifest_response(manifest: &AssistantManifest) -> AssistantManifestResponse {
    let mut requirements = Vec::new();
    requirements.extend(
        manifest
            .requirements
            .skills
            .iter()
            .map(|requirement| AssistantRequirementResponse {
                key: requirement.key.clone(),
                kind: AssistantRequirementKind::Skill,
                required: requirement.required,
                label: requirement.identity.slug.clone(),
                identity: Some(requirement.identity.clone()),
                preferred_ids: Vec::new(),
                version_requirement: requirement.version.clone(),
            }),
    );
    for (kind, values) in [
        (AssistantRequirementKind::Mcp, &manifest.requirements.mcps),
        (AssistantRequirementKind::Model, &manifest.requirements.models),
        (AssistantRequirementKind::Agent, &manifest.requirements.agents),
    ] {
        requirements.extend(values.iter().map(|requirement| {
            let preferred_ids = match kind {
                AssistantRequirementKind::Mcp => requirement.preferred_mcp_ids.clone(),
                AssistantRequirementKind::Model => requirement.preferred_model_ids.clone(),
                AssistantRequirementKind::Agent => requirement.preferred_agent_ids.clone(),
                AssistantRequirementKind::Skill => Vec::new(),
            };
            AssistantRequirementResponse {
                key: requirement.key.clone(),
                kind,
                required: requirement.required,
                label: preferred_ids
                    .first()
                    .cloned()
                    .unwrap_or_else(|| requirement.key.clone()),
                identity: None,
                preferred_ids,
                version_requirement: requirement.version_requirement.clone(),
            }
        }));
    }
    AssistantManifestResponse {
        format: manifest.format.clone(),
        format_version: manifest.format_version,
        id: manifest.id.clone(),
        version: manifest.version.clone(),
        name: manifest.name.clone(),
        name_i18n: manifest.name_i18n.clone(),
        description: manifest.description.clone(),
        description_i18n: manifest.description_i18n.clone(),
        categories: manifest.categories.clone(),
        tags: manifest.tags.clone(),
        avatar: manifest.avatar.clone(),
        defaults: AssistantDefaultsCatalogResponse {
            agent: manifest.defaults.agent.clone(),
            model: AssistantDefaultScalar {
                mode: manifest.defaults.model.mode.clone(),
                value: manifest.defaults.model.value.clone(),
            },
            permission: AssistantDefaultScalar {
                mode: manifest.defaults.permission.mode.clone(),
                value: manifest.defaults.permission.value.clone(),
            },
            thought_level: AssistantDefaultScalar {
                mode: manifest.defaults.thought_level.mode.clone(),
                value: manifest.defaults.thought_level.value.clone(),
            },
            skills: manifest.defaults.skills.clone(),
            mcps: manifest.defaults.mcps.clone(),
        },
        requirements,
        recommended_prompts: manifest.recommended_prompts.clone(),
        recommended_prompts_i18n: manifest.recommended_prompts_i18n.clone(),
        content_hash: manifest.content_hash.clone(),
    }
}

fn version_response(value: &CatalogVersion) -> AssistantVersionResponse {
    AssistantVersionResponse {
        version: value.version.clone(),
        revision: value.revision.clone(),
        digest: value.digest.clone(),
    }
}

fn file_response(value: CatalogFile) -> AssistantCatalogFileResponse {
    AssistantCatalogFileResponse {
        path: value.path,
        size: value.size,
        sha256: value.sha256,
    }
}

async fn read_manifest(root: &Path) -> Result<AssistantManifest, AssistantError> {
    let expected = root.file_name().and_then(|value| value.to_str()).unwrap_or_default();
    read_manifest_as(root, expected).await
}

async fn read_manifest_as(root: &Path, expected: &str) -> Result<AssistantManifest, AssistantError> {
    let bytes = tokio::fs::read(root.join(MANIFEST_FILE)).await?;
    let manifest: AssistantManifest =
        serde_json::from_slice(&bytes).map_err(|error| AssistantError::BadRequest(format!("助手清单无效：{error}")))?;
    validate_manifest(&manifest, expected)?;
    Ok(manifest)
}

fn validate_manifest(manifest: &AssistantManifest, expected_id: &str) -> Result<(), AssistantError> {
    if manifest.format != "tjuae-assistant" || manifest.format_version != 1 {
        return Err(AssistantError::BadRequest("助手包格式或版本无效".to_owned()));
    }
    if manifest.id != expected_id || manifest.id.is_empty() || manifest.version.is_empty() {
        return Err(AssistantError::BadRequest("助手包标识或版本无效".to_owned()));
    }
    tjuaeui_catalog::validate_relative_file(&manifest.instructions.default, 0)?;
    for path in manifest.instructions.locales.values() {
        tjuaeui_catalog::validate_relative_file(path, 0)?;
    }
    let mut keys = BTreeSet::new();
    for key in manifest
        .requirements
        .skills
        .iter()
        .map(|item| &item.key)
        .chain(manifest.requirements.mcps.iter().map(|item| &item.key))
        .chain(manifest.requirements.models.iter().map(|item| &item.key))
        .chain(manifest.requirements.agents.iter().map(|item| &item.key))
    {
        if key.is_empty() || !keys.insert(key) {
            return Err(AssistantError::BadRequest(format!("助手资源要求键无效或重复：{key}")));
        }
    }
    Ok(())
}

fn validate_slug(value: &str) -> Result<(), AssistantError> {
    let valid = !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if !valid || value.starts_with('-') || value.ends_with('-') || value.contains("--") {
        return Err(AssistantError::BadRequest(
            "助手标识只能包含小写字母、数字和单个连字符".to_owned(),
        ));
    }
    Ok(())
}

fn extract_assistant_archive(archive_path: &Path, target: &Path) -> Result<(), AssistantError> {
    const MAX_FILES: usize = 2_500;
    const MAX_BYTES: u64 = 100 * 1024 * 1024;
    let file = std::fs::File::open(archive_path)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| AssistantError::BadRequest(error.to_string()))?;
    if archive.len() > MAX_FILES {
        return Err(AssistantError::BadRequest("助手包文件数量超过 2500".to_owned()));
    }
    let mut total = 0_u64;
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|error| AssistantError::BadRequest(error.to_string()))?;
        let enclosed = file
            .enclosed_name()
            .ok_or_else(|| AssistantError::BadRequest(format!("助手包路径越界：{}", file.name())))?;
        if file.unix_mode().is_some_and(|mode| mode & 0o170000 == 0o120000) {
            return Err(AssistantError::BadRequest(format!(
                "助手包不能包含符号链接：{}",
                file.name()
            )));
        }
        total = total.saturating_add(file.size());
        if total > MAX_BYTES {
            return Err(AssistantError::BadRequest("助手包解压后超过 100 MB".to_owned()));
        }
        let destination = target.join(enclosed);
        if file.is_dir() {
            std::fs::create_dir_all(destination)?;
            continue;
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::io::copy(&mut file, &mut std::fs::File::create(destination)?)?;
    }
    Ok(())
}

fn detect_assistant_package_root(staging: &Path) -> Result<PathBuf, AssistantError> {
    if staging.join(MANIFEST_FILE).is_file() && staging.join(ENTRY_FILE).is_file() {
        return Ok(staging.to_path_buf());
    }
    let directories = std::fs::read_dir(staging)?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .collect::<Vec<_>>();
    if directories.len() == 1 {
        let root = directories[0].path();
        if root.join(MANIFEST_FILE).is_file() && root.join(ENTRY_FILE).is_file() {
            return Ok(root);
        }
    }
    Err(AssistantError::BadRequest("压缩包中未找到有效助手".to_owned()))
}

fn list_local_files(root: &Path) -> Result<Vec<CatalogFile>, AssistantError> {
    fn visit(root: &Path, directory: &Path, files: &mut Vec<CatalogFile>) -> Result<(), AssistantError> {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                if entry.file_name() != ".git" {
                    visit(root, &path, files)?;
                }
                continue;
            }
            if !entry.file_type()?.is_file() {
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|error| AssistantError::Internal(error.to_string()))?
                .to_string_lossy()
                .replace('\\', "/");
            let bytes = std::fs::read(&path)?;
            files.push(CatalogFile {
                path: relative,
                size: bytes.len() as u64,
                sha256: format!("{:x}", Sha256::digest(&bytes)),
            });
        }
        Ok(())
    }
    let mut files = Vec::new();
    visit(root, root, &mut files)?;
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

fn assistant_directory_digest(root: &Path) -> Result<String, AssistantError> {
    let mut digest = Sha256::new();
    digest.update(b"tjuae-assistant-workspace-v1\0");
    for file in list_local_files(root)? {
        if file.path == MANIFEST_FILE {
            continue;
        }
        digest.update(file.path.as_bytes());
        digest.update(b"\0");
        digest.update(std::fs::read(root.join(&file.path))?);
        digest.update(b"\0");
    }
    Ok(format!("sha256-{:x}", digest.finalize()))
}

fn collect_portable_directory(
    root: &Path,
    current: &Path,
    package_prefix: &str,
    files: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), AssistantError> {
    for entry in std::fs::read_dir(current)? {
        let entry = entry?;
        if entry.file_name() == ".git" {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(AssistantError::BadRequest(format!(
                "可移植资源不能包含符号链接：{}",
                entry.path().display()
            )));
        }
        if file_type.is_dir() {
            collect_portable_directory(root, &entry.path(), package_prefix, files)?;
        } else if file_type.is_file() {
            let relative = entry
                .path()
                .strip_prefix(root)
                .map_err(|error| AssistantError::Internal(error.to_string()))?
                .to_string_lossy()
                .replace('\\', "/");
            let package_path = format!("{package_prefix}/{relative}");
            let bytes = std::fs::read(entry.path())?;
            tjuaeui_catalog::validate_relative_file(&package_path, bytes.len() as u64)?;
            files.push((package_path, bytes));
            if files.len() > 2_000 || files.iter().map(|(_, bytes)| bytes.len()).sum::<usize>() > 100 * 1024 * 1024 {
                return Err(AssistantError::BadRequest("助手依赖资源超过打包限制".to_owned()));
            }
        }
    }
    Ok(())
}

fn copy_directory(source: &Path, destination: &Path) -> Result<(), AssistantError> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        if entry.file_name() == ".git" {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(AssistantError::BadRequest(format!(
                "助手目录不能包含符号链接：{}",
                entry.path().display()
            )));
        }
        let target = destination.join(entry.file_name());
        if file_type.is_dir() {
            copy_directory(&entry.path(), &target)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

async fn rename_assistant_directory(source: &Path, target: &Path) -> Result<(), AssistantError> {
    let mut last_error = None;
    for delay_ms in [0, 25, 50, 100, 200, 400] {
        if delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
        match tokio::fs::rename(source, target).await {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => last_error = Some(error),
            Err(error) => return Err(error.into()),
        }
    }
    Err(last_error.expect("rename retry records an error").into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tjuaeui_db::SqliteAssistantUserPreferenceRepository;
    use tjuaeui_file::GitService;

    #[test]
    fn rejects_duplicate_requirement_keys_across_resource_groups() {
        let manifest: AssistantManifest = serde_json::from_value(serde_json::json!({
            "$schema": "schema",
            "format": "tjuae-assistant",
            "formatVersion": 1,
            "id": "writer",
            "version": "1.0.0",
            "name": "Writer",
            "description": "Writes",
            "categories": [],
            "tags": [],
            "avatar": null,
            "instructions": {"default": "ASSISTANT.md", "locales": {}},
            "defaults": {"agent": null, "model": {"mode":"auto","value":null}, "permission":{"mode":"auto","value":null}, "thoughtLevel":{"mode":"auto","value":null}, "skills":[], "mcps":[]},
            "requirements": {
                "skills": [{"key":"same","required":true,"identity":{"source":"tjuae-hub","namespace":"official","slug":"writer"}}],
                "mcps": [{"key":"same","required":true,"preferredMcpIds":["mcp"]}],
                "models": [], "agents": []
            },
            "recommendedPrompts": [], "contentHash":"sha256-value", "extensions": {}
        })).unwrap();
        assert!(validate_manifest(&manifest, "writer").is_err());
    }

    #[tokio::test]
    async fn system_assistant_is_local_enabled_and_cannot_be_disabled_or_deleted() {
        let temp = tempfile::TempDir::new().unwrap();
        let database = tjuaeui_db::init_database_memory().await.unwrap();
        let service = AssistantCatalogService::new(
            Arc::new(SqliteAssistantUserPreferenceRepository::new(database.pool().clone())),
            temp.path(),
            None,
            None,
            false,
            Arc::new(GitService::new()),
        );
        service.ensure_system_assistant("# TjuaeUI 管家").await.unwrap();

        let page = service
            .list(AssistantSourceResponse::Mine, "", "name", None, 100)
            .await
            .unwrap();
        let item = page
            .items
            .iter()
            .find(|item| item.identity.slug == SYSTEM_ASSISTANT_SLUG)
            .unwrap();
        assert!(item.system);
        assert!(item.editable);
        assert!(item.preferences.enabled);
        assert!(!item.can_disable);
        assert!(!item.can_delete);

        let identity = identity(AssistantSourceResponse::Mine, "", SYSTEM_ASSISTANT_SLUG);
        let disable = service
            .update_preferences(
                &identity,
                UpdateAssistantCatalogPreferencesRequest {
                    selected_version: Some("1.0.0".to_owned()),
                    follow_latest: false,
                    enabled: false,
                    sort_order: None,
                },
            )
            .await;
        assert!(matches!(disable, Err(AssistantError::Forbidden(_))));
        assert!(matches!(
            service.delete_mine(&identity).await,
            Err(AssistantError::Forbidden(_))
        ));
    }

    #[tokio::test]
    async fn exported_assistant_package_round_trips_through_import() {
        let temp = tempfile::TempDir::new().unwrap();
        let database = tjuaeui_db::init_database_memory().await.unwrap();
        let service = AssistantCatalogService::new(
            Arc::new(SqliteAssistantUserPreferenceRepository::new(database.pool().clone())),
            temp.path(),
            None,
            None,
            false,
            Arc::new(GitService::new()),
        );
        service
            .create_mine(CreateMineAssistantRequest {
                slug: "portable-helper".to_owned(),
                name: "Portable Helper".to_owned(),
                description: "Round trip".to_owned(),
            })
            .await
            .unwrap();
        let identity = identity(AssistantSourceResponse::Mine, "", "portable-helper");
        let archive = temp.path().join("portable-helper.zip");
        service
            .export(
                &identity,
                ExportAssistantRequest {
                    version: None,
                    output_path: archive.to_string_lossy().into_owned(),
                },
                Vec::new(),
            )
            .await
            .unwrap();
        service.delete_mine(&identity).await.unwrap();

        let imported = service
            .import_mine(ImportAssistantRequest {
                archive_path: archive.to_string_lossy().into_owned(),
            })
            .await
            .unwrap();

        assert_eq!(imported.item.identity.slug, "portable-helper");
        assert_eq!(imported.manifest.name, "Portable Helper");
        assert!(
            service
                .mine_root()
                .join("portable-helper")
                .join(MANIFEST_FILE)
                .is_file()
        );
    }

    #[tokio::test]
    async fn copy_to_mine_returns_the_new_identity_and_keeps_the_source() {
        let temp = tempfile::TempDir::new().unwrap();
        let database = tjuaeui_db::init_database_memory().await.unwrap();
        let service = AssistantCatalogService::new(
            Arc::new(SqliteAssistantUserPreferenceRepository::new(database.pool().clone())),
            temp.path(),
            None,
            None,
            false,
            Arc::new(GitService::new()),
        );
        service
            .create_mine(CreateMineAssistantRequest {
                slug: "source-helper".to_owned(),
                name: "Source Helper".to_owned(),
                description: "Source".to_owned(),
            })
            .await
            .unwrap();
        let source = identity(AssistantSourceResponse::Mine, "", "source-helper");

        let copied = service
            .copy_to_mine(
                &source,
                CopyAssistantToMineRequest {
                    version: Some("0.1.0".to_owned()),
                    target_slug: "copied-helper".to_owned(),
                },
            )
            .await
            .unwrap();

        assert_eq!(copied.item.identity.slug, "copied-helper");
        assert_eq!(copied.manifest.id, "copied-helper");
        assert!(service.mine_root().join("source-helper").is_dir());
        assert!(service.mine_root().join("copied-helper").is_dir());
    }

    #[test]
    fn assistant_archive_rejects_parent_directory_entries() {
        let temp = tempfile::TempDir::new().unwrap();
        let archive_path = temp.path().join("unsafe.zip");
        let file = std::fs::File::create(&archive_path).unwrap();
        let mut archive = zip::ZipWriter::new(file);
        archive
            .start_file("../outside.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        archive.write_all(b"unsafe").unwrap();
        archive.finish().unwrap();

        let result = extract_assistant_archive(&archive_path, &temp.path().join("target"));
        assert!(matches!(result, Err(AssistantError::BadRequest(_))));
        assert!(!temp.path().join("outside.txt").exists());
    }

    #[tokio::test]
    async fn editing_enabled_system_assistant_keeps_activation_ready() {
        let temp = tempfile::TempDir::new().unwrap();
        let database = tjuaeui_db::init_database_memory().await.unwrap();
        let preferences = Arc::new(SqliteAssistantUserPreferenceRepository::new(database.pool().clone()));
        let service = AssistantCatalogService::new(
            preferences.clone(),
            temp.path(),
            None,
            None,
            false,
            Arc::new(GitService::new()),
        );
        service.ensure_system_assistant("# TjuaeUI 管家").await.unwrap();
        let identity = identity(AssistantSourceResponse::Mine, "", SYSTEM_ASSISTANT_SLUG);

        let saved = service
            .update_settings(
                &identity,
                UpdateAssistantCatalogSettingsRequest {
                    name: "TjuaeUI 管家".to_owned(),
                    description: "使用 Codex CLI 的系统管家".to_owned(),
                    avatar: None,
                    avatar_data_url: None,
                    categories: vec!["系统".to_owned()],
                    tags: vec!["管家".to_owned()],
                    defaults: AssistantDefaultsCatalogResponse {
                        agent: Some("codex-agent-id".to_owned()),
                        ..AssistantDefaultsCatalogResponse::default()
                    },
                    recommended_prompts: vec!["创建一个助手".to_owned()],
                    rules: "# 更新后的管家".to_owned(),
                },
            )
            .await
            .unwrap();

        assert_eq!(saved.manifest.defaults.agent.as_deref(), Some("codex-agent-id"));
        assert_eq!(saved.manifest.categories, vec!["系统"]);
        assert_eq!(saved.manifest.tags, vec!["管家"]);
        let preference = preferences
            .get("mine", "", SYSTEM_ASSISTANT_SLUG)
            .await
            .unwrap()
            .unwrap();
        assert!(preference.enabled);
        assert_eq!(preference.activation_status, "ready");
        assert_eq!(
            preference.activation_fingerprint.as_deref(),
            Some(saved.manifest.content_hash.as_str())
        );
        let profile = service.runtime_profile(&runtime_id(&identity)).await.unwrap().unwrap();
        assert_eq!(profile.agent_id, "codex-agent-id");
    }
}
