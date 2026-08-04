//! 助手运行投影的只读查询与规则解析服务。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json;
use tjuaeui_api_types::{
    AgentManagementRow, AgentManagementStatus, AgentSource, AssetKind, AssistantCapabilitiesResponse,
    AssistantDefaultListResponse, AssistantDefaultScalarResponse, AssistantDefaultsResponse, AssistantDetailResponse,
    AssistantEngineDescriptor, AssistantEngineResponse, AssistantPreferencesResponse, AssistantProfileResponse,
    AssistantPromptsResponse, AssistantResponse, AssistantRulesResponse, AssistantSource, AssistantStateResponse,
    assistant_avatar_response_value_with_version, is_local_avatar_value,
};
use tjuaeui_asset::{AssetCatalogService, AssetError, AssistantRuleDispatcher};
use tjuaeui_common::generate_prefixed_id;
use tjuaeui_db::{
    AssistantDefinitionRow, AssistantOverlayRow, IAssistantDefinitionRepository, IAssistantOverlayRepository,
    IAssistantPreferenceRepository, IProviderRepository, SqlitePool, UpsertAssistantDefinitionParams,
    UpsertAssistantOverlayParams, resolve_agent_binding,
};
use tracing::{debug, info, warn};

use crate::agent_catalog::AssistantAgentCatalogPort;
use crate::error::AssistantError;

/// Safely-served bytes for a local assistant avatar.
pub struct AssistantAvatarAsset {
    pub bytes: Vec<u8>,
    pub extension: Option<String>,
}

/// Max attempts (initial + retries) and per-retry backoff for a bootstrap step
/// contended by a concurrent startup (concurrent-startup regression).
const BOOTSTRAP_RETRY_MAX_ATTEMPTS: u32 = 5;
const BOOTSTRAP_RETRY_BACKOFF_MS: [u64; 4] = [50, 100, 200, 400];
const DEFAULT_LOCAL_USER_ID: &str = "system_default_user";

/// Whether an assistant error is transient SQLite busy/locked contention. Repos
/// convert `DbError` into `AssistantError::Internal(other.to_string())`, so the
/// service can only classify by text — reusing the same markers as
/// `DbError::is_busy` (single source of truth).
fn assistant_error_is_busy(error: &AssistantError) -> bool {
    matches!(error, AssistantError::Internal(message) if tjuaeui_db::message_indicates_busy(message))
}

/// Whether an assistant error is a UNIQUE constraint violation — either the
/// explicit `Conflict` variant (from `DbError::Conflict`) or a UNIQUE message
/// surfaced through `Internal`.
fn assistant_error_is_unique(error: &AssistantError) -> bool {
    match error {
        AssistantError::Conflict(_) => true,
        AssistantError::Internal(message) => tjuaeui_db::message_indicates_unique_violation(message),
        _ => false,
    }
}

/// Run one bootstrap step with bounded concurrent-startup retry. Free function
/// (no `self`) so the retry policy is unit-testable.
///
/// - `Ok(())` → done.
/// - UNIQUE conflict → treated as already-applied by a concurrent startup
///   (idempotent convergence), returns `Ok(())`.
/// - SQLITE_BUSY → retried with backoff; if still busy after the budget,
///   returns [`AssistantError::ConcurrentBootstrapContention`].
/// - any other error → returned immediately (real errors are not swallowed).
async fn retry_bootstrap_step<'a, F>(step_name: &'static str, op: F) -> Result<(), AssistantError>
where
    F: Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), AssistantError>> + Send + 'a>>,
{
    for attempt in 0..BOOTSTRAP_RETRY_MAX_ATTEMPTS {
        match op().await {
            Ok(()) => return Ok(()),
            Err(error) if assistant_error_is_unique(&error) => {
                debug!(
                    step = step_name,
                    "bootstrap step already applied by a concurrent startup (unique conflict treated as done)"
                );
                return Ok(());
            }
            Err(error) if assistant_error_is_busy(&error) => {
                if attempt + 1 >= BOOTSTRAP_RETRY_MAX_ATTEMPTS {
                    return Err(AssistantError::ConcurrentBootstrapContention(format!(
                        "bootstrap step '{step_name}' contended after retries"
                    )));
                }
                let backoff = BOOTSTRAP_RETRY_BACKOFF_MS[(attempt as usize).min(BOOTSTRAP_RETRY_BACKOFF_MS.len() - 1)];
                warn!(
                    step = step_name,
                    attempt = attempt + 1,
                    backoff_ms = backoff,
                    "bootstrap step contended under concurrent startup (busy); retrying"
                );
                tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
            }
            Err(error) => return Err(error),
        }
    }
    // The loop always returns above; this satisfies the type checker.
    Err(AssistantError::ConcurrentBootstrapContention(format!(
        "bootstrap step '{step_name}' contended after retries"
    )))
}

/// Aggregated business logic for `/api/assistants/*` and rule/skill dispatch.
pub struct AssistantService {
    pool: SqlitePool,
    definition_repo: Arc<dyn IAssistantDefinitionRepository>,
    state_repo: Arc<dyn IAssistantOverlayRepository>,
    preference_repo: Arc<dyn IAssistantPreferenceRepository>,
    /// Used to infer a sane `agent_id` default when the caller did not supply
    /// one. The historical default of `"gemini"` 400'd within
    /// 1 ms on machines without the Gemini CLI (ELECTRON-1J1 / 1KV); we now
    /// pick an agent that actually matches the configured provider list.
    provider_repo: Arc<dyn IProviderRepository>,
    agent_catalog: Option<Arc<dyn AssistantAgentCatalogPort>>,
    runtime_asset_catalog: Arc<AssetCatalogService>,
    /// Root directory holding user-authored rule/skill md files and avatars.
    /// Defaults to `~/.tjuaeui/` but can be overridden for tests.
    user_data_dir: PathBuf,
}

pub struct AssistantServiceDeps {
    pub definition_repo: Arc<dyn IAssistantDefinitionRepository>,
    pub state_repo: Arc<dyn IAssistantOverlayRepository>,
    pub preference_repo: Arc<dyn IAssistantPreferenceRepository>,
    pub provider_repo: Arc<dyn IProviderRepository>,
    pub agent_catalog: Option<Arc<dyn AssistantAgentCatalogPort>>,
    pub runtime_asset_catalog: Arc<AssetCatalogService>,
}

impl AssistantService {
    /// Construct an `AssistantService` pinned to the runtime data directory.
    ///
    /// `user_data_dir` is the runtime projection root for assistant rules and
    /// avatar uploads. Editable assistant Definitions remain in AssetCatalog.
    /// Production code passes the same `services.data_dir` that the SQLite
    /// database lives under, so dev / packaged / multi-instance launches
    /// keep their rule files alongside the matching db. Tests pin a temp
    /// directory.
    ///
    /// There is no implicit `~/.tjuaeui` fallback on purpose: an earlier
    /// version had one, and dev builds silently wrote rule files to the
    /// release directory while the db lived under `~/.tjuaeui-dev/`,
    /// resulting in `read_rule` returning empty in dev mode. Forcing the
    /// caller to pass a path makes the wiring explicit.
    pub fn new(pool: SqlitePool, deps: AssistantServiceDeps, user_data_dir: PathBuf) -> Self {
        let AssistantServiceDeps {
            definition_repo,
            state_repo,
            preference_repo,
            provider_repo,
            agent_catalog,
            runtime_asset_catalog,
        } = deps;
        Self {
            pool,
            definition_repo,
            state_repo,
            preference_repo,
            provider_repo,
            agent_catalog,
            runtime_asset_catalog,
            user_data_dir,
        }
    }

    /// Bootstrap unified assistant storage from local runtime projections.
    pub async fn bootstrap_assistant_storage(&self) -> Result<(), AssistantError> {
        // Each step already re-runs idempotently on every startup. Wrap each in
        // bounded concurrent-startup retry so a transient SQLITE_BUSY is retried
        // and a UNIQUE conflict (another startup already inserted the row) is
        // treated as done, instead of bubbling up as a fatal BOOTSTRAP_SERVER_FAILED
        // (concurrent-startup regression). We do NOT widen any startup lock here.
        retry_bootstrap_step("reconcile_generated_assistants", || {
            Box::pin(async { self.reconcile_generated_assistants().await.map(|_| ()) })
        })
        .await?;
        Ok(())
    }

    async fn reconcile_generated_assistants(&self) -> Result<Vec<AgentManagementRow>, AssistantError> {
        let Some(agent_catalog) = &self.agent_catalog else {
            return Ok(Vec::new());
        };

        let rows = agent_catalog.list_management_agents().await?;
        let definitions = self.definition_repo.list().await.map_err(|e| {
            AssistantError::Internal(format!("list assistant definitions for generated reconcile: {e}"))
        })?;
        let generated_source_refs: HashSet<String> = definitions
            .iter()
            .filter(|definition| definition.source == "generated")
            .filter_map(|definition| definition.source_ref.clone())
            .collect();
        let has_existing_generated = !generated_source_refs.is_empty();
        let existing_min_sort_order = self
            .state_repo
            .list()
            .await
            .map_err(|e| AssistantError::Internal(format!("list assistant overlays for generated reconcile: {e}")))?
            .into_iter()
            .map(|state| state.sort_order)
            .min()
            .unwrap_or_default()
            .min(0);
        let generated_rows: Vec<&AgentManagementRow> = rows
            .iter()
            .filter(|row| {
                row.enabled
                    && row.installed
                    && row.agent_type.supports_new_conversation()
                    && matches!(
                        row.status,
                        AgentManagementStatus::Online | AgentManagementStatus::Unchecked
                    )
            })
            .collect();
        let missing_generated_count = generated_rows
            .iter()
            .filter(|row| !generated_source_refs.contains(&row.id))
            .count();

        let mut missing_index = 0usize;
        for row in generated_rows {
            if let Err(error) = self
                .reconcile_generated_assistant(
                    row,
                    &definitions,
                    has_existing_generated,
                    existing_min_sort_order,
                    missing_generated_count,
                    &mut missing_index,
                )
                .await
            {
                warn!(
                    agent_id = %row.id,
                    error = %error,
                    "skip dirty generated assistant during startup bootstrap"
                );
            }
        }

        Ok(rows)
    }

    async fn reconcile_generated_assistant(
        &self,
        row: &AgentManagementRow,
        definitions: &[AssistantDefinitionRow],
        has_existing_generated: bool,
        existing_min_sort_order: i32,
        missing_generated_count: usize,
        missing_index: &mut usize,
    ) -> Result<(), AssistantError> {
        let existing_definition = definitions
            .iter()
            .find(|definition| {
                definition.source == "generated" && definition.source_ref.as_deref() == Some(row.id.as_str())
            })
            .cloned();
        let is_missing = existing_definition.is_none();
        let assistant_id = format!("bare:{}", row.id);
        let (definition_id, assistant_id) = self
            .resolve_definition_identity("generated", Some(&row.id), &assistant_id)
            .await?;
        let avatar_value = row.icon.as_deref().filter(|value| !value.trim().is_empty());
        let (definition, should_upsert) = if let Some(mut definition) = existing_definition {
            let avatar_type = if avatar_value.is_some() { "emoji" } else { "none" };
            let should_upgrade_skill_defaults = definition.default_skills_mode == "auto"
                && decode_str_list(Some(definition.default_skill_ids.as_str()))?.is_empty();
            let identity_changed = definition.name != row.name
                || definition.avatar_type != avatar_type
                || definition.avatar_value.as_deref() != avatar_value
                || definition.agent_id != row.id
                || definition.source_ref.as_deref() != Some(row.id.as_str())
                || definition.rule_resource_type != "user_file"
                || definition.rule_resource_ref.as_deref() != Some(assistant_id.as_str());

            definition.name = row.name.clone();
            definition.avatar_type = avatar_type.to_string();
            definition.avatar_value = avatar_value.map(ToOwned::to_owned);
            definition.agent_id = row.id.clone();
            definition.source_ref = Some(row.id.clone());
            definition.rule_resource_type = "user_file".into();
            definition.rule_resource_ref = Some(assistant_id.clone());
            if should_upgrade_skill_defaults {
                definition.default_skills_mode = "fixed".into();
            }
            (definition, identity_changed || should_upgrade_skill_defaults)
        } else {
            (
                AssistantDefinitionRow {
                    id: definition_id.clone(),
                    assistant_id: assistant_id.clone(),
                    source: "generated".into(),
                    owner_type: "system".into(),
                    source_ref: Some(row.id.clone()),
                    name: row.name.clone(),
                    name_i18n: "{}".into(),
                    description: row.description.clone(),
                    description_i18n: "{}".into(),
                    avatar_type: if avatar_value.is_some() {
                        "emoji".into()
                    } else {
                        "none".into()
                    },
                    avatar_value: avatar_value.map(ToOwned::to_owned),
                    agent_id: row.id.clone(),
                    rule_resource_type: "user_file".into(),
                    rule_resource_ref: Some(assistant_id.clone()),
                    recommended_prompts: "[]".into(),
                    recommended_prompts_i18n: "{}".into(),
                    default_model_mode: "auto".into(),
                    default_model_value: None,
                    default_permission_mode: "auto".into(),
                    default_permission_value: None,
                    default_thought_level_mode: "auto".into(),
                    default_thought_level_value: None,
                    default_skills_mode: "fixed".into(),
                    default_skill_ids: "[]".into(),
                    custom_skill_names: "[]".into(),
                    default_mcps_mode: "auto".into(),
                    default_mcp_ids: "[]".into(),
                    created_at: 0,
                    updated_at: 0,
                    deleted_at: None,
                },
                true,
            )
        };

        if should_upsert {
            self.definition_repo
                .upsert(&upsert_params_from_definition(&definition))
                .await
                .map_err(|e| AssistantError::Internal(format!("upsert generated assistant definition: {e}")))?;
        }

        if !is_missing {
            return Ok(());
        }

        if self
            .state_repo
            .get(&definition_id)
            .await
            .map_err(|e| AssistantError::Internal(format!("get generated assistant overlay: {e}")))?
            .is_none()
        {
            let current_missing_index = *missing_index;
            *missing_index += 1;
            let initial_generated_sort_order = if !has_existing_generated && missing_generated_count > 0 {
                existing_min_sort_order as i64 - missing_generated_count as i64 + current_missing_index as i64
            } else {
                row.sort_order
            };
            self.state_repo
                .upsert(&UpsertAssistantOverlayParams {
                    assistant_definition_id: &definition_id,
                    enabled: true,
                    sort_order: initial_generated_sort_order.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
                    agent_id_override: None,
                    last_used_at: None,
                })
                .await
                .map_err(|e| AssistantError::Internal(format!("upsert generated assistant overlay: {e}")))?;
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Classification
    // -----------------------------------------------------------------------

    /// Classify an assistant id into its source.
    pub async fn classify_source(&self, id: &str) -> AssistantSource {
        if let Ok(Some(definition)) = self.definition_repo.get_by_assistant_id(id).await {
            return match definition.source.as_str() {
                "generated" => AssistantSource::Generated,
                _ => AssistantSource::User,
            };
        }
        AssistantSource::User
    }

    // -----------------------------------------------------------------------
    // List / Get
    // -----------------------------------------------------------------------

    /// Unified local assistant list with per-assistant overlay application.
    /// Also performs opportunistic orphan cleanup on the overrides table.
    pub async fn list(&self) -> Result<Vec<AssistantResponse>, AssistantError> {
        self.list_for_user(DEFAULT_LOCAL_USER_ID).await
    }

    pub async fn list_for_user(&self, user_id: &str) -> Result<Vec<AssistantResponse>, AssistantError> {
        ensure_default_runtime_user(user_id)?;
        let projections = self.reconcile_generated_assistants().await?;
        let runtime_ids = self.active_assistant_id_map(user_id).await?;
        let definitions = self
            .definition_repo
            .list()
            .await
            .map_err(|e| AssistantError::Internal(format!("list assistant definitions: {e}")))?;
        let states = self
            .state_repo
            .list()
            .await
            .map_err(|e| AssistantError::Internal(format!("list assistant overlays: {e}")))?;
        let state_map: HashMap<String, AssistantOverlayRow> = states
            .into_iter()
            .map(|state| (state.assistant_definition_id.clone(), state))
            .collect();

        let mut result = Vec::new();

        for definition in &definitions {
            // Generated bare rows are internal engine projections, not
            // assistant assets. Official assistants are installed from
            // TjuaeHub and user assistants come from the local AssetCatalog.
            if definition.source == "generated" {
                continue;
            }
            if generated_definition_is_uninstalled(definition, &projections) {
                continue;
            }
            let Some(public_id) = runtime_ids.get(&definition.assistant_id) else {
                continue;
            };
            let public_id = public_id.as_str();
            let projection = self
                .project_definition(definition, state_map.get(&definition.id), &projections)
                .await?;
            let mut response = self.definition_to_response(definition, state_map.get(&definition.id), &projection)?;
            response.id = public_id.to_owned();
            if definition.avatar_type == "user_asset" {
                response.avatar = Some(format!(
                    "/api/assistants/{public_id}/avatar?v={}",
                    definition.updated_at
                ));
            }
            result.push(response);
        }

        // Sort by sort_order asc, then last_used_at desc (newer first).
        result.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then_with(|| b.last_used_at.cmp(&a.last_used_at))
        });

        Ok(result)
    }

    pub async fn get(&self, id: &str) -> Result<AssistantResponse, AssistantError> {
        self.get_for_user(DEFAULT_LOCAL_USER_ID, id).await
    }

    async fn get_for_user(&self, user_id: &str, id: &str) -> Result<AssistantResponse, AssistantError> {
        let projections = self.reconcile_generated_assistants().await?;
        if let Some((definition, public_id)) = self.resolve_definition_for_user(user_id, id).await? {
            if generated_definition_is_uninstalled(&definition, &projections) {
                return Err(AssistantError::NotFound(format!("assistant '{id}' not found")));
            }
            let state = self.state_repo.get(&definition.id).await?;
            let projection = self
                .project_definition(&definition, state.as_ref(), &projections)
                .await?;
            let mut response = self.definition_to_response(&definition, state.as_ref(), &projection)?;
            response.id = public_id.clone();
            if definition.avatar_type == "user_asset" {
                response.avatar = Some(format!(
                    "/api/assistants/{public_id}/avatar?v={}",
                    definition.updated_at
                ));
            }
            return Ok(response);
        }

        Err(AssistantError::NotFound(format!("assistant '{id}' not found")))
    }

    pub async fn get_detail(&self, id: &str, locale: Option<&str>) -> Result<AssistantDetailResponse, AssistantError> {
        self.get_detail_for_user(DEFAULT_LOCAL_USER_ID, id, locale).await
    }

    pub async fn get_detail_for_user(
        &self,
        user_id: &str,
        id: &str,
        locale: Option<&str>,
    ) -> Result<AssistantDetailResponse, AssistantError> {
        ensure_default_runtime_user(user_id)?;
        let projections = self.reconcile_generated_assistants().await?;
        if let Some((definition, public_id)) = self.resolve_definition_for_user(user_id, id).await? {
            if generated_definition_is_uninstalled(&definition, &projections) {
                return Err(AssistantError::NotFound(format!("assistant '{id}' not found")));
            }
            let state = self.state_repo.get(&definition.id).await?;
            let preference = self.preference_repo.get(&definition.id).await?;
            let rules_content = self.read_user_rule_with_fallback(&definition.assistant_id, locale);
            let projection = self
                .project_definition(&definition, state.as_ref(), &projections)
                .await?;
            let mut response = self.definition_to_detail_response(
                &definition,
                state.as_ref(),
                preference.as_ref(),
                &rules_content,
                &projection,
            )?;
            response.id = public_id.clone();
            if definition.avatar_type == "user_asset" {
                response.profile.avatar = Some(format!(
                    "/api/assistants/{public_id}/avatar?v={}",
                    definition.updated_at
                ));
            }
            return Ok(response);
        }

        Err(AssistantError::NotFound(format!("assistant '{id}' not found")))
    }

    async fn active_assistant_id_map(&self, user_id: &str) -> Result<HashMap<String, String>, AssistantError> {
        self.runtime_asset_catalog
            .list_active_runtime_bindings(user_id, AssetKind::Assistant)
            .await
            .map(|bindings| {
                bindings
                    .into_iter()
                    .map(|binding| (binding.projection_runtime_id, binding.provenance.local_asset_id))
                    .collect()
            })
            .map_err(|error| AssistantError::Internal(format!("list active assistant assets: {error}")))
    }

    async fn resolve_definition_for_user(
        &self,
        user_id: &str,
        id: &str,
    ) -> Result<Option<(AssistantDefinitionRow, String)>, AssistantError> {
        if let Some(definition) = self.definition_repo.get_by_assistant_id(id).await?
            && definition.source == "generated"
        {
            return Ok(Some((definition, id.to_owned())));
        }

        let bindings = self
            .runtime_asset_catalog
            .list_active_runtime_bindings(user_id, AssetKind::Assistant)
            .await
            .map_err(|error| AssistantError::Internal(format!("list active assistant assets: {error}")))?;
        let bound = match self
            .runtime_asset_catalog
            .resolve_bound_runtime_asset(user_id, AssetKind::Assistant, id)
            .await
        {
            Ok(bound) => Some(bound),
            Err(AssetError::NotFound(_)) => bindings.into_iter().find(|binding| binding.projection_runtime_id == id),
            Err(error) => {
                return Err(AssistantError::Internal(format!(
                    "resolve active assistant asset: {error}"
                )));
            }
        };
        let Some(bound) = bound else {
            return Ok(None);
        };
        let definition = self
            .definition_repo
            .get_by_assistant_id(&bound.projection_runtime_id)
            .await?
            .ok_or_else(|| AssistantError::Internal("active assistant Binding has no runtime projection".into()))?;
        Ok(Some((definition, bound.provenance.local_asset_id)))
    }

    // -----------------------------------------------------------------------
    // Default-agent inference
    // -----------------------------------------------------------------------

    /// Pick a sane `agent_id` default for newly created / imported assistants
    /// when the caller did not supply one.
    ///
    /// Inference rule (ELECTRON-1J1 / 1KV):
    /// 1. If any enabled provider exists (Anthropic, OpenAI, custom,
    ///    Bedrock, Vertex, …), return `"tjuaecli"`. TjuaeCLI speaks both
    ///    OpenAI-compatible and Anthropic-protocol APIs over the
    ///    user-configured base URL and does not require any third-party
    ///    CLI to be installed. CLI-based agents (`claude`, `gemini`)
    ///    must be opted into explicitly via `agent_id` because
    ///    the presence of an Anthropic API key does not imply that the
    ///    Claude Code CLI is on `PATH`.
    /// 2. Otherwise (no providers configured), return a `BadRequest`
    ///    error. The previous code silently fell back to `"gemini"`,
    ///    which on machines without the Gemini CLI 400'd within 1 ms
    ///    with `Agent 'Gemini CLI' CLI not found in PATH`.
    pub async fn resolve_default_agent_id(&self) -> Result<String, AssistantError> {
        let providers = self
            .provider_repo
            .list()
            .await
            .map_err(|e| AssistantError::Internal(format!("failed to list providers: {e}")))?;

        if providers.iter().any(|p| p.enabled) {
            self.resolve_agent_id_for_agent_ref("tjuaecli").await
        } else {
            Err(AssistantError::BadRequest(
                "Cannot create assistant: no providers configured. Add a provider before creating an assistant, \
                 or pass an explicit `agent_id` in the request body."
                    .into(),
            ))
        }
    }

    async fn resolve_agent_id_for_agent_ref(&self, agent_ref: &str) -> Result<String, AssistantError> {
        let trimmed = agent_ref.trim();
        let Some(binding) = resolve_agent_binding(&self.pool, trimmed)
            .await
            .map_err(|e| AssistantError::Internal(format!("resolve agent binding: {e}")))?
        else {
            return Err(AssistantError::BadRequest(format!("Unknown agent_ref '{trimmed}'")));
        };
        Ok(binding.agent_id)
    }

    async fn project_definition(
        &self,
        definition: &AssistantDefinitionRow,
        state: Option<&AssistantOverlayRow>,
        agent_rows: &[AgentManagementRow],
    ) -> Result<AssistantRuntimeProjection, AssistantError> {
        let effective_agent_id = effective_agent_id_for_definition(definition, state);
        let runtime_backend = resolve_agent_binding(&self.pool, effective_agent_id)
            .await
            .map_err(|e| AssistantError::Internal(format!("resolve agent binding: {e}")))?
            .map(|binding| binding.runtime_backend);
        Ok(assistant_projection_for_definition(
            definition,
            state,
            agent_rows,
            runtime_backend.as_deref(),
        ))
    }

    // -----------------------------------------------------------------------
    // Rule / skill dispatch helpers
    // -----------------------------------------------------------------------

    /// Read an assistant rule file from the unified local runtime projection.
    pub async fn read_rule(&self, id: &str, locale: Option<&str>) -> Result<String, AssistantError> {
        self.read_rule_for_user(DEFAULT_LOCAL_USER_ID, id, locale).await
    }

    pub async fn read_rule_for_user(
        &self,
        user_id: &str,
        id: &str,
        locale: Option<&str>,
    ) -> Result<String, AssistantError> {
        ensure_default_runtime_user(user_id)?;
        let definition = self
            .resolve_definition_for_user(user_id, id)
            .await?
            .map(|(definition, _)| definition)
            .ok_or_else(|| AssistantError::NotFound(format!("assistant '{id}' not found")))?;
        Ok(self.read_user_rule_with_fallback(&definition.assistant_id, locale))
    }

    /// Read a user assistant's rule, falling back to any saved `<id>.*.md` file
    /// when the locale-specific `<id>.<locale>.md` is absent. Scheduled/cron runs
    /// create the conversation with `assistant: None`, so no UI locale reaches
    /// rule resolution and the localized file would otherwise be missed.
    fn read_user_rule_with_fallback(&self, id: &str, locale: Option<&str>) -> String {
        let rules_dir = self.user_rules_dir();
        let content = read_assistant_md_with_legacy(&rules_dir, id, locale);
        if !content.is_empty() {
            return content;
        }

        if locale.is_some_and(|value| !value.is_empty()) {
            let locale_less = read_assistant_md_with_legacy(&rules_dir, id, None);
            if !locale_less.is_empty() {
                return locale_less;
            }
        }

        read_first_assistant_md(&rules_dir, id)
    }

    // -----------------------------------------------------------------------
    // Avatar helpers
    // -----------------------------------------------------------------------

    /// Resolve the avatar bytes for an assistant together with its file
    /// extension (for `Content-Type` inference).
    ///
    /// The avatar must be a managed file referenced by the unified local
    /// assistant definition.
    pub async fn avatar_asset(&self, id: &str) -> Option<AssistantAvatarAsset> {
        self.avatar_asset_for_user(DEFAULT_LOCAL_USER_ID, id)
            .await
            .ok()
            .flatten()
    }

    pub async fn avatar_asset_for_user(
        &self,
        user_id: &str,
        id: &str,
    ) -> Result<Option<AssistantAvatarAsset>, AssistantError> {
        ensure_default_runtime_user(user_id)?;
        let Some((definition, _)) = self.resolve_definition_for_user(user_id, id).await? else {
            return Ok(None);
        };
        if definition.avatar_type != "user_asset" {
            return Ok(None);
        }
        Ok(definition
            .avatar_value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .and_then(|value| self.read_user_avatar_asset_by_filename(value)))
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    fn user_rules_dir(&self) -> PathBuf {
        self.user_data_dir.join("assistant-rules")
    }

    fn user_avatars_dir(&self) -> PathBuf {
        self.user_data_dir.join("assistant-avatars")
    }

    fn read_user_avatar_asset_by_filename(&self, value: &str) -> Option<AssistantAvatarAsset> {
        let value = value.trim();
        if value.is_empty() || value.contains('/') || value.contains('\\') {
            return None;
        }
        read_user_avatar_asset_from_path(&self.user_avatars_dir().join(value))
    }

    fn user_asset_avatar_value_is_renderable(&self, definition: &AssistantDefinitionRow) -> bool {
        let Some(value) = definition
            .avatar_value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return false;
        };
        if is_local_avatar_value(value) || value.contains('/') || value.contains('\\') {
            return false;
        }
        self.read_user_avatar_asset_by_filename(value).is_some()
    }

    async fn resolve_definition_identity(
        &self,
        source: &str,
        source_ref: Option<&str>,
        assistant_id: &str,
    ) -> Result<(String, String), AssistantError> {
        if let Some(source_ref) = source_ref
            && let Some(existing) = self
                .definition_repo
                .get_by_source_ref_including_deleted(source, source_ref)
                .await
                .map_err(|e| AssistantError::Internal(format!("get assistant definition by source_ref: {e}")))?
        {
            return Ok((existing.id, existing.assistant_id));
        }

        if let Some(existing) = self
            .definition_repo
            .get_by_assistant_id_including_deleted(assistant_id)
            .await
            .map_err(|e| AssistantError::Internal(format!("get assistant definition by key: {e}")))?
        {
            return Ok((existing.id, existing.assistant_id));
        }

        Ok((generate_prefixed_id("asstdef"), assistant_id.to_string()))
    }
}

fn ensure_default_runtime_user(user_id: &str) -> Result<(), AssistantError> {
    if user_id == DEFAULT_LOCAL_USER_ID {
        Ok(())
    } else {
        Err(AssistantError::Forbidden(
            "当前助手运行时仍是单用户全局投影；非默认用户请求已拒绝，避免跨用户读写".into(),
        ))
    }
}

#[async_trait::async_trait]
impl AssistantRuleDispatcher for AssistantService {
    async fn read_rule(&self, user_id: &str, id: &str, locale: Option<&str>) -> Result<String, AssetError> {
        AssistantService::read_rule_for_user(self, user_id, id, locale)
            .await
            .map_err(assistant_error_to_asset_error)
    }
}

fn assistant_error_to_asset_error(error: AssistantError) -> AssetError {
    match error {
        AssistantError::BadRequest(message) => AssetError::InvalidMetadata(message),
        AssistantError::NotFound(message) => AssetError::NotFound(message),
        AssistantError::Internal(message) => AssetError::SourceUnavailable(message),
        other => AssetError::InvalidState(other.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Response conversion
// ---------------------------------------------------------------------------

impl AssistantService {
    fn avatar_display_value(&self, definition: &AssistantDefinitionRow) -> Option<String> {
        if definition.avatar_type == "user_asset" && !self.user_asset_avatar_value_is_renderable(definition) {
            return None;
        }

        let value = assistant_avatar_response_value_with_version(
            definition.avatar_type.as_str(),
            definition.avatar_value.as_deref(),
            definition.assistant_id.as_str(),
            definition.updated_at,
        )?;

        Some(value)
    }

    fn definition_to_response(
        &self,
        definition: &AssistantDefinitionRow,
        state: Option<&AssistantOverlayRow>,
        projection: &AssistantRuntimeProjection,
    ) -> Result<AssistantResponse, AssistantError> {
        let source = match definition.source.as_str() {
            "generated" => AssistantSource::Generated,
            _ => AssistantSource::User,
        };
        let models = match (
            definition.default_model_mode.as_str(),
            definition.default_model_value.as_deref(),
        ) {
            ("fixed", Some(model)) => vec![model.to_string()],
            _ => Vec::new(),
        };

        Ok(AssistantResponse {
            id: definition.assistant_id.clone(),
            source,
            name: definition.name.clone(),
            name_i18n: decode_str_map(Some(definition.name_i18n.as_str()))?,
            description: definition.description.clone(),
            description_i18n: decode_str_map(Some(definition.description_i18n.as_str()))?,
            avatar: self.avatar_display_value(definition),
            enabled: state.map(|row| row.enabled).unwrap_or(true),
            sort_order: state.map(|row| row.sort_order).unwrap_or(0),
            engine_id: projection.engine_id.clone(),
            engine: projection.engine.clone(),
            enabled_skills: decode_str_list(Some(definition.default_skill_ids.as_str()))?,
            custom_skill_names: decode_str_list(Some(definition.custom_skill_names.as_str()))?,
            context: None,
            context_i18n: HashMap::new(),
            prompts: decode_str_list(Some(definition.recommended_prompts.as_str()))?,
            prompts_i18n: decode_list_map(Some(definition.recommended_prompts_i18n.as_str()))?,
            models,
            last_used_at: state.and_then(|row| row.last_used_at),
            engine_status: projection.engine_status,
            engine_status_message: projection.engine_status_message.clone(),
            team_selectable: projection.team_selectable,
            team_block_reason: projection.team_block_reason.clone(),
            deletable: projection.deletable,
        })
    }

    fn definition_to_detail_response(
        &self,
        definition: &AssistantDefinitionRow,
        state: Option<&AssistantOverlayRow>,
        preference: Option<&tjuaeui_db::AssistantPreferenceRow>,
        rules_content: &str,
        projection: &AssistantRuntimeProjection,
    ) -> Result<AssistantDetailResponse, AssistantError> {
        let default_skill_ids = decode_str_list(Some(definition.default_skill_ids.as_str()))?;
        let custom_skill_names = decode_str_list(Some(definition.custom_skill_names.as_str()))?;
        let default_mcp_ids = decode_str_list(Some(definition.default_mcp_ids.as_str()))?;
        let last_skill_ids = preference
            .map(|row| decode_str_list(Some(row.last_skill_ids.as_str())))
            .transpose()?
            .unwrap_or_default();
        let last_mcp_ids = preference
            .map(|row| decode_str_list(Some(row.last_mcp_ids.as_str())))
            .transpose()?
            .unwrap_or_default();

        Ok(AssistantDetailResponse {
            id: definition.assistant_id.clone(),
            source: match definition.source.as_str() {
                "generated" => AssistantSource::Generated,
                _ => AssistantSource::User,
            },
            engine_status: projection.engine_status,
            engine_status_message: projection.engine_status_message.clone(),
            team_selectable: projection.team_selectable,
            team_block_reason: projection.team_block_reason.clone(),
            deletable: projection.deletable,
            profile: AssistantProfileResponse {
                name: definition.name.clone(),
                name_i18n: decode_str_map(Some(definition.name_i18n.as_str()))?,
                description: definition.description.clone(),
                description_i18n: decode_str_map(Some(definition.description_i18n.as_str()))?,
                avatar: self.avatar_display_value(definition),
            },
            state: AssistantStateResponse {
                enabled: state.map(|row| row.enabled).unwrap_or(true),
                sort_order: state.map(|row| row.sort_order).unwrap_or_default(),
                last_used_at: state.and_then(|row| row.last_used_at),
            },
            engine: AssistantEngineResponse {
                id: projection.engine_id.clone(),
                descriptor: projection.engine.clone(),
            },
            rules: AssistantRulesResponse {
                content: rules_content.to_owned(),
                storage_mode: definition.rule_resource_type.clone(),
            },
            prompts: AssistantPromptsResponse {
                recommended: decode_str_list(Some(definition.recommended_prompts.as_str()))?,
                recommended_i18n: decode_list_map(Some(definition.recommended_prompts_i18n.as_str()))?,
            },
            defaults: AssistantDefaultsResponse {
                model: AssistantDefaultScalarResponse {
                    mode: definition.default_model_mode.clone(),
                    value: definition.default_model_value.clone(),
                },
                permission: AssistantDefaultScalarResponse {
                    mode: definition.default_permission_mode.clone(),
                    value: definition.default_permission_value.clone(),
                },
                thought_level: AssistantDefaultScalarResponse {
                    mode: definition.default_thought_level_mode.clone(),
                    value: definition.default_thought_level_value.clone(),
                },
                skills: AssistantDefaultListResponse {
                    mode: definition.default_skills_mode.clone(),
                    value: default_skill_ids.clone(),
                },
                mcps: AssistantDefaultListResponse {
                    mode: definition.default_mcps_mode.clone(),
                    value: default_mcp_ids,
                },
            },
            capabilities: AssistantCapabilitiesResponse {
                default_skill_ids,
                custom_skill_names,
            },
            preferences: AssistantPreferencesResponse {
                last_model_id: preference.and_then(|row| row.last_model_id.clone()),
                last_permission_value: preference.and_then(|row| row.last_permission_value.clone()),
                last_thought_level_value: preference.and_then(|row| row.last_thought_level_value.clone()),
                last_skill_ids,
                last_mcp_ids,
            },
        })
    }
}

fn read_user_avatar_asset_from_path(path: &Path) -> Option<AssistantAvatarAsset> {
    let bytes = std::fs::read(path).ok()?;
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    Some(AssistantAvatarAsset { bytes, extension })
}

#[derive(Debug, Clone)]
struct AssistantRuntimeProjection {
    engine_id: String,
    engine: Option<AssistantEngineDescriptor>,
    engine_status: AgentManagementStatus,
    engine_status_message: Option<String>,
    team_selectable: bool,
    team_block_reason: Option<String>,
    deletable: bool,
}

fn assistant_projection_for_definition(
    definition: &AssistantDefinitionRow,
    state: Option<&AssistantOverlayRow>,
    agent_rows: &[AgentManagementRow],
    resolved_runtime_backend: Option<&str>,
) -> AssistantRuntimeProjection {
    let enabled = state.is_none_or(|row| row.enabled);
    let source = match definition.source.as_str() {
        "generated" => AssistantSource::Generated,
        _ => AssistantSource::User,
    };
    let effective_agent_id = effective_agent_id_for_definition(definition, state);
    let fallback_runtime_backend = resolved_runtime_backend.unwrap_or(effective_agent_id);

    // An agent row identifies its runtime key by `backend` for vendor ACP
    // agents, but tjuae_cli (the built-in Rust agent) has a NULL `backend` and is
    // keyed by its `agent_type` ("tjuaecli") instead. Match on either so tjuae_cli
    // assistants resolve to the tjuae_cli row rather than falling back to Missing.
    let row_matches_backend = |row: &&AgentManagementRow| {
        row.backend.as_deref() == Some(effective_agent_id)
            || row.agent_type.serde_name() == effective_agent_id
            || row.backend.as_deref() == Some(fallback_runtime_backend)
            || row.agent_type.serde_name() == fallback_runtime_backend
    };

    let agent_row = if matches!(source, AssistantSource::Generated) {
        agent_rows.iter().find(|row| row.id == effective_agent_id).or_else(|| {
            definition
                .source_ref
                .as_deref()
                .and_then(|source_ref| agent_rows.iter().find(|row| row.id == source_ref))
        })
    } else {
        agent_rows
            .iter()
            .find(|row| row.id == effective_agent_id)
            .or_else(|| {
                agent_rows
                    .iter()
                    .find(|row| row_matches_backend(row) && row.agent_source != AgentSource::Custom)
            })
            .or_else(|| agent_rows.iter().find(row_matches_backend))
    };
    let engine_id = agent_row
        .map(|row| row.id.clone())
        .unwrap_or_else(|| effective_agent_id.to_owned());
    let engine = agent_row.map(|row| AssistantEngineDescriptor {
        r#type: row.agent_type,
        ownership: row.agent_source,
        acp_backend: row.backend.clone(),
    });

    let engine_status = agent_row
        .map(|row| row.status)
        .unwrap_or(AgentManagementStatus::Missing);
    let engine_status_message = agent_row.and_then(|row| {
        row.last_check_error_message
            .clone()
            .or_else(|| row.last_check_guidance.clone())
    });
    let team_block_reason = if !enabled {
        Some("Assistant is disabled.".to_string())
    } else {
        match agent_row {
            Some(row) if matches!(row.status, AgentManagementStatus::Missing) => {
                Some("This assistant's agent is not installed.".to_string())
            }
            Some(row) if matches!(row.status, AgentManagementStatus::Offline) => Some(
                row.last_check_error_message
                    .clone()
                    .or_else(|| row.last_check_guidance.clone())
                    .unwrap_or_else(|| "This assistant's agent is unavailable.".to_string()),
            ),
            Some(row) if !row.team_capable => Some("This assistant's agent does not support team mode.".to_string()),
            None => Some("This assistant's agent could not be resolved.".to_string()),
            _ => None,
        }
    };

    AssistantRuntimeProjection {
        engine_id,
        engine,
        engine_status,
        engine_status_message,
        team_selectable: enabled
            && agent_row.is_some_and(|row| {
                matches!(
                    row.status,
                    AgentManagementStatus::Online | AgentManagementStatus::Unchecked
                ) && row.team_capable
            }),
        team_block_reason,
        deletable: matches!(source, AssistantSource::User),
    }
}

fn generated_definition_is_uninstalled(definition: &AssistantDefinitionRow, agent_rows: &[AgentManagementRow]) -> bool {
    if definition.source != "generated" {
        return false;
    }

    let agent_id = definition.agent_id.as_str();
    let source_ref = definition.source_ref.as_deref();
    let Some(row) = agent_rows
        .iter()
        .find(|row| row.id == agent_id || source_ref == Some(row.id.as_str()))
    else {
        return true;
    };

    !row.installed
}

fn effective_agent_id_for_definition<'a>(
    definition: &'a AssistantDefinitionRow,
    state: Option<&'a AssistantOverlayRow>,
) -> &'a str {
    state
        .and_then(|row| row.agent_id_override.as_deref())
        .unwrap_or(definition.agent_id.as_str())
}

fn upsert_params_from_definition(definition: &AssistantDefinitionRow) -> UpsertAssistantDefinitionParams<'_> {
    UpsertAssistantDefinitionParams {
        id: &definition.id,
        assistant_id: &definition.assistant_id,
        source: &definition.source,
        owner_type: &definition.owner_type,
        source_ref: definition.source_ref.as_deref(),
        name: &definition.name,
        name_i18n: &definition.name_i18n,
        description: definition.description.as_deref(),
        description_i18n: &definition.description_i18n,
        avatar_type: &definition.avatar_type,
        avatar_value: definition.avatar_value.as_deref(),
        agent_id: &definition.agent_id,
        rule_resource_type: &definition.rule_resource_type,
        rule_resource_ref: definition.rule_resource_ref.as_deref(),
        recommended_prompts: &definition.recommended_prompts,
        recommended_prompts_i18n: &definition.recommended_prompts_i18n,
        default_model_mode: &definition.default_model_mode,
        default_model_value: definition.default_model_value.as_deref(),
        default_permission_mode: &definition.default_permission_mode,
        default_permission_value: definition.default_permission_value.as_deref(),
        default_thought_level_mode: &definition.default_thought_level_mode,
        default_thought_level_value: definition.default_thought_level_value.as_deref(),
        default_skills_mode: &definition.default_skills_mode,
        default_skill_ids: &definition.default_skill_ids,
        custom_skill_names: &definition.custom_skill_names,
        default_mcps_mode: &definition.default_mcps_mode,
        default_mcp_ids: &definition.default_mcp_ids,
    }
}

fn decode_str_list(raw: Option<&str>) -> Result<Vec<String>, AssistantError> {
    match raw {
        Some(s) if !s.is_empty() => {
            serde_json::from_str(s).map_err(|e| AssistantError::Internal(format!("decode list: {e}")))
        }
        _ => Ok(Vec::new()),
    }
}

fn decode_str_map(raw: Option<&str>) -> Result<HashMap<String, String>, AssistantError> {
    match raw {
        Some(s) if !s.is_empty() => {
            serde_json::from_str(s).map_err(|e| AssistantError::Internal(format!("decode map: {e}")))
        }
        _ => Ok(HashMap::new()),
    }
}

fn decode_list_map(raw: Option<&str>) -> Result<HashMap<String, Vec<String>>, AssistantError> {
    match raw {
        Some(s) if !s.is_empty() => {
            serde_json::from_str(s).map_err(|e| AssistantError::Internal(format!("decode map: {e}")))
        }
        _ => Ok(HashMap::new()),
    }
}

// ---------------------------------------------------------------------------
// Filesystem helpers
// ---------------------------------------------------------------------------

fn assistant_md_path(dir: &Path, id: &str, locale: Option<&str>) -> PathBuf {
    let id = encode_filename_component(id);
    let filename = match locale {
        Some(loc) if !loc.is_empty() => format!("{id}.{}.md", encode_filename_component(loc)),
        _ => format!("{id}.md"),
    };
    dir.join(filename)
}

fn legacy_assistant_md_path(dir: &Path, id: &str, locale: Option<&str>) -> PathBuf {
    let filename = match locale {
        Some(loc) if !loc.is_empty() => format!("{id}.{loc}.md"),
        _ => format!("{id}.md"),
    };
    dir.join(filename)
}

fn legacy_filename_component_is_safe(value: &str) -> bool {
    !value.bytes().any(|byte| matches!(byte, b'/' | b'\\' | b'\0'))
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

fn read_file_or_empty(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn read_assistant_md_with_legacy(dir: &Path, id: &str, locale: Option<&str>) -> String {
    let path = assistant_md_path(dir, id, locale);
    let content = read_file_or_empty(&path);
    if !content.is_empty() {
        return content;
    }

    if !legacy_filename_component_is_safe(id) || locale.is_some_and(|value| !legacy_filename_component_is_safe(value)) {
        return String::new();
    }

    let legacy_path = legacy_assistant_md_path(dir, id, locale);
    if legacy_path == path {
        return String::new();
    }
    let legacy_content = read_file_or_empty(&legacy_path);
    if legacy_content.is_empty() {
        return String::new();
    }

    match std::fs::write(&path, &legacy_content) {
        Ok(()) => {
            info!(
                assistant_id = id,
                locale = locale.unwrap_or_default(),
                "migrated legacy assistant markdown path"
            );
            if let Err(error) = std::fs::remove_file(&legacy_path) {
                warn!(
                    assistant_id = id,
                    locale = locale.unwrap_or_default(),
                    %error,
                    "failed to remove legacy assistant markdown path after migration"
                );
            }
        }
        Err(error) => {
            warn!(
                assistant_id = id,
                locale = locale.unwrap_or_default(),
                %error,
                "failed to migrate legacy assistant markdown path"
            );
        }
    }
    legacy_content
}

/// Read the first available assistant markdown file in `dir`, preferring the
/// locale-less file. Both encoded filenames and pre-encoding legacy filenames
/// are recognized.
fn read_first_assistant_md(dir: &Path, id: &str) -> String {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return String::new();
    };
    let encoded_id = encode_filename_component(id);
    let encoded_prefix = format!("{encoded_id}.");
    let encoded_exact = format!("{encoded_id}.md");
    let legacy_prefix = format!("{id}.");
    let legacy_exact = format!("{id}.md");
    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy().into_owned();
        let priority = if name == encoded_exact || name == legacy_exact {
            0
        } else if name.starts_with(&encoded_prefix) && name.ends_with(".md") {
            1
        } else if name.starts_with(&legacy_prefix) && name.ends_with(".md") {
            2
        } else {
            continue;
        };
        candidates.push((priority, name, entry.path()));
    }
    candidates.sort_by(|left, right| (left.0, &left.1).cmp(&(right.0, &right.1)));
    for (_, _, path) in candidates {
        let content = read_file_or_empty(&path);
        if !content.is_empty() {
            return content;
        }
    }
    String::new()
}
