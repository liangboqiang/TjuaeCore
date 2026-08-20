//! Module-level router states + their builders.
//!
//! `ModuleStates` is the bundle returned by `build_module_states`; each
//! `build_*_state` constructs one `*RouterState` from `AppServices`.

use std::sync::Arc;
use std::time::Instant;

use tjuaeui_ai_agent::{AgentRouterState, AgentService, RemoteAgentRouterState, RemoteAgentService};
use tjuaeui_assistant::{
    AssistantActivationService, AssistantAgentCatalogPort, AssistantCatalogService, AssistantError,
    AssistantRouterState,
};
use tjuaeui_auth::extract_token_from_ws_headers;
use tjuaeui_channel::ChannelRouterState;
use tjuaeui_conversation::{
    AssistantRuntimeCatalogPort, AssistantRuntimePreferenceUpdate, ConversationRouterState, ConversationService,
};
use tjuaeui_cron::{CronEventEmitter, CronRouterState, service::CronServiceDeps};
use tjuaeui_db::{
    IAcpSessionRepository, IAgentMetadataRepository, IConversationRepository, IProviderRepository,
    SqliteAcpSessionRepository, SqliteAgentMetadataRepository, SqliteAssistantUserPreferenceRepository,
    SqliteClientPreferenceRepository, SqliteConversationRepository, SqliteFeedbackDiagnosticsRepository,
    SqliteMcpServerRepository, SqliteProviderRepository, SqliteRemoteAgentRepository, SqliteSettingsRepository,
    SqliteSkillUserPreferenceRepository,
};
use tjuaeui_extension::{
    ExtensionRegistry, ExtensionRouterState, ExtensionStateStore, HubIndexManager, HubInstaller, HubRouterState,
    SkillRouterState, resolve_install_target_dir_for_data_dir, resolve_scan_paths_for_data_dir,
    resolve_state_file_path,
};
use tjuaeui_file::{BrowseRoots, FileRouterState, FileService, FileWatchService};
use tjuaeui_mcp::{
    ClaudeAdapter, CodeBuddyAdapter, CodexAdapter, GeminiAdapter, McpAgentAdapter, McpConfigService,
    McpConnectionTestService, McpRouterState, McpSyncService, OpencodeAdapter, QwenAdapter, TjuaeCliAdapter,
    TjuaeUIAdapter,
};
use tjuaeui_office::{ConversionService, OfficeRouterState, SnapshotService as OfficeSnapshotService};
use tjuaeui_realtime::{NoopMessageRouter, WsHandlerState};
use tjuaeui_shell::ShellRouterState;
use tjuaeui_system::{
    ClientPrefService, ConnectionTestRouterState, ConnectionTestService, FeedbackDiagnosticsService, ModelFetchService,
    ProtocolDetectionService, ProviderService, RuntimePrepareService, SettingsService, SystemRouterState,
    VersionCheckService,
};
use tjuaeui_team::{
    AgentTurnCancellationPort, AgentTurnExecutionPort, TeamAssistantCatalogEntry, TeamAssistantCatalogPort,
    TeamConversationProvisioningPort, TeamProjectionMessageStore, TeamRouterState, TeamSessionService,
};

use crate::config::derive_encryption_key;
use crate::router::team_conversation_adapters::TeamConversationAdapters;
use crate::services::AppServices;

#[derive(Clone)]
struct CatalogConversationRuntime {
    catalog: Arc<AssistantCatalogService>,
}

#[async_trait::async_trait]
impl AssistantRuntimeCatalogPort for CatalogConversationRuntime {
    async fn resolve_enabled(
        &self,
        assistant_id: &str,
        locale: Option<&str>,
    ) -> Result<Option<tjuaeui_conversation::AssistantRuntimeProfile>, String> {
        let profile = self
            .catalog
            .runtime_profile(assistant_id)
            .await
            .map_err(|error| error.to_string())?;
        Ok(profile.map(|profile| {
            let localized_name = locale
                .and_then(|locale| profile.name_i18n.get(locale))
                .cloned()
                .unwrap_or(profile.name);
            tjuaeui_conversation::AssistantRuntimeProfile {
                id: profile.id,
                source: match profile.identity.source {
                    tjuaeui_api_types::AssistantSourceResponse::Mine => "mine".to_owned(),
                    tjuaeui_api_types::AssistantSourceResponse::TjuaeHub => "tjuae-hub".to_owned(),
                },
                name: localized_name,
                avatar: profile.avatar_url.unwrap_or_default(),
                agent_id: profile.agent_id,
                rules: profile.rules,
                model_mode: profile.model_mode,
                model: profile.model,
                permission_mode: profile.permission_mode,
                permission: profile.permission,
                thought_level_mode: profile.thought_level_mode,
                thought_level: profile.thought_level,
                skill_ids: profile.skill_ids,
                mcp_ids: profile.mcp_ids,
            }
        }))
    }

    async fn update_runtime_preferences(
        &self,
        assistant_id: &str,
        updates: AssistantRuntimePreferenceUpdate<'_>,
    ) -> Result<(), String> {
        self.catalog
            .update_runtime_overrides(
                assistant_id,
                tjuaeui_api_types::UpdateAssistantRuntimeOverridesRequest {
                    model: updates.model.map(str::to_owned),
                    permission: updates.permission.map(str::to_owned),
                    thought_level: updates.thought_level.map(str::to_owned),
                },
            )
            .await
            .map_err(|error| error.to_string())
    }
}

#[derive(Debug)]
pub struct RouterBuildError {
    stage: &'static str,
    message: &'static str,
    source: Option<anyhow::Error>,
}

impl RouterBuildError {
    pub fn new(stage: &'static str, message: &'static str) -> Self {
        Self {
            stage,
            message,
            source: None,
        }
    }

    pub fn with_source(mut self, source: impl Into<anyhow::Error>) -> Self {
        self.source = Some(source.into());
        self
    }

    pub fn stage(&self) -> &'static str {
        self.stage
    }

    pub fn message(&self) -> &'static str {
        self.message
    }
}

impl std::fmt::Display for RouterBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.stage, self.message)
    }
}

impl std::error::Error for RouterBuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source.as_ref() as &(dyn std::error::Error + 'static))
    }
}

/// All module-level router states bundled into a single struct.
///
/// Reduces parameter bloat on router constructors and makes it easy for
/// tests to override individual modules.
pub struct ModuleStates {
    pub system: SystemRouterState,
    pub conversation: ConversationRouterState,
    pub remote_agent: RemoteAgentRouterState,
    pub agent: AgentRouterState,

    pub connection_test: ConnectionTestRouterState,
    pub file: FileRouterState,
    pub mcp: McpRouterState,
    pub extension: ExtensionRouterState,
    pub hub: HubRouterState,
    pub skill: SkillRouterState,
    pub channel: ChannelRouterState,
    pub team: TeamRouterState,
    pub cron: CronRouterState,
    pub office: OfficeRouterState,
    pub shell: ShellRouterState,
    pub assistant: AssistantRouterState,
}

fn default_allowed_roots(work_dir: Option<&std::path::Path>) -> Vec<std::path::PathBuf> {
    let mut roots = vec![
        std::env::temp_dir(),
        dirs::home_dir().unwrap_or_else(std::env::temp_dir),
    ];
    // Auto-provisioned per-conversation workspaces live under
    // `{work_dir}/conversations/{label}-temp-{id}/`. On Windows the
    // operator may put `work_dir` on a separate drive (e.g. `X:\TjuaeUI`)
    // that's neither under `temp_dir` nor `home_dir`. Including `work_dir`
    // keeps temp workspaces on the default allowlist without widening it
    // to unrelated paths.
    if let Some(wd) = work_dir
        && !wd.as_os_str().is_empty()
        && !roots.iter().any(|r| r == wd)
    {
        roots.push(wd.to_path_buf());
    }
    roots
}

fn build_module_state_phase<T>(boot: &Instant, phase: &'static str, build: impl FnOnce() -> T) -> T {
    tracing::info!(
        elapsed_ms = boot.elapsed().as_millis(),
        phase,
        "startup: module state phase started"
    );
    let value = build();
    tracing::info!(
        elapsed_ms = boot.elapsed().as_millis(),
        phase,
        "startup: module state phase completed"
    );
    value
}

/// Components needed to start the channel orchestrator.
///
/// Returned alongside `ChannelRouterState` by `build_channel_state`.
/// The caller must spawn the orchestrator as a background task.
pub struct ChannelOrchestratorComponents {
    pub orchestrator: tjuaeui_channel::orchestrator::ChannelOrchestrator,
    pub message_rx: tokio::sync::mpsc::Receiver<tjuaeui_channel::types::UnifiedIncomingMessage>,
    pub confirm_rx: tokio::sync::mpsc::Receiver<(String, String)>,
    pub manager: Arc<tjuaeui_channel::manager::ChannelManager>,
    pub plugin_factory: Arc<tjuaeui_channel::manager::PluginFactory>,
}

/// Build all default `ModuleStates` from application services.
pub async fn build_module_states(
    services: &AppServices,
) -> Result<(ModuleStates, ChannelOrchestratorComponents), RouterBuildError> {
    let boot = Instant::now();
    tracing::info!("startup: module state build started");

    let (ext_state, hub_state, skill_state) = build_extension_states(services).await;
    tracing::info!(
        elapsed_ms = boot.elapsed().as_millis(),
        "startup: extension states built"
    );

    let scan_paths = resolve_scan_paths_for_data_dir(&services.data_dir);
    if let Err(error) = ext_state.registry.initialize_with_scan_paths(scan_paths).await {
        tracing::warn!(
            code = "BOOTSTRAP_DEGRADED_EXTENSION_REGISTRY",
            stage = "extension.registry.initialize",
            error = %error,
            "extension registry initialize failed"
        );
    }
    tracing::info!(
        elapsed_ms = boot.elapsed().as_millis(),
        "startup: extension registry initialized"
    );

    let assistant = build_assistant_state(services);
    assistant
        .catalog
        .ensure_system_assistant(include_str!(
            "../../assets/builtin-assistants/rules/tjuaeui-assistant.zh-CN.md"
        ))
        .await
        .map_err(|error| {
            RouterBuildError::new("router.assistant.system", "无法创建或修复 TjuaeUI 系统管家").with_source(error)
        })?;
    let cron = build_cron_state(services);
    cron.cron_service.init().await;
    tracing::info!(
        elapsed_ms = boot.elapsed().as_millis(),
        "startup: cron state initialized"
    );

    // The agent catalog already hydrated at startup (see `lib.rs`).
    // Extension-contributed rows will land in `agent_metadata` in a
    // later step; for now we rely on the builtin + internal seed rows.

    let (channel_state, channel_components) = build_channel_state(services, ext_state.registry.clone()).await;
    tracing::info!(elapsed_ms = boot.elapsed().as_millis(), "startup: channel state built");

    let backend_binary_path = Arc::new(
        std::env::current_exe()
            .ok()
            .and_then(|p| p.canonicalize().ok())
            .unwrap_or_else(|| std::path::PathBuf::from("tjuaecore")),
    );
    tracing::info!(
        elapsed_ms = boot.elapsed().as_millis(),
        "startup: backend binary path resolved"
    );

    let pool = services.database.pool().clone();
    let provider_repo: Arc<dyn IProviderRepository> = Arc::new(SqliteProviderRepository::new(pool.clone()));
    let encryption_key = derive_encryption_key(&services.jwt_secret_raw);
    let agent_service = AgentService::new_with_spawner(
        services.agent_registry.clone(),
        services.event_bus.clone(),
        provider_repo,
        encryption_key,
        services.data_dir.clone(),
        services.session_spawner.clone(),
    );
    services
        .conversation_service
        .with_agent_availability_feedback(agent_service.availability_feedback_port());
    tracing::info!(elapsed_ms = boot.elapsed().as_millis(), "startup: agent service built");

    tracing::info!(
        elapsed_ms = boot.elapsed().as_millis(),
        "startup: module states bundle started"
    );
    let states = ModuleStates {
        system: build_module_state_phase(&boot, "system", || build_system_state(services)),
        conversation: build_module_state_phase(&boot, "conversation", || {
            build_conversation_state(services, Some(cron.cron_service.clone()))
        }),
        remote_agent: build_module_state_phase(&boot, "remote_agent", || build_remote_agent_state(services)),
        agent: build_module_state_phase(&boot, "agent", || AgentRouterState {
            agent_registry: services.agent_registry.clone(),
            service: agent_service,
        }),
        connection_test: build_module_state_phase(&boot, "connection_test", build_connection_test_state),
        file: build_module_state_phase(&boot, "file", || build_file_state(services))?,
        mcp: build_module_state_phase(&boot, "mcp", || build_mcp_state(services)),
        extension: ext_state,
        hub: hub_state,
        skill: skill_state,
        channel: channel_state,
        team: build_module_state_phase(&boot, "team", || {
            build_team_state(services, Some(cron.cron_service.clone()), backend_binary_path.clone())
        }),
        cron,
        office: build_module_state_phase(&boot, "office", || build_office_state(services)),
        shell: build_module_state_phase(&boot, "shell", || build_shell_state(services)),
        assistant,
    };
    tracing::info!(
        elapsed_ms = boot.elapsed().as_millis(),
        "startup: module state build completed"
    );
    states
        .conversation
        .service
        .recover_stale_runtime_state_on_startup()
        .await;

    Ok((states, channel_components))
}

/// Build the default `AssistantRouterState` from application services.
pub fn build_assistant_state(services: &AppServices) -> AssistantRouterState {
    #[derive(Clone)]
    struct RegistryAssistantAgentCatalog {
        registry: Arc<tjuaeui_ai_agent::AgentRegistry>,
    }

    #[async_trait::async_trait]
    impl AssistantAgentCatalogPort for RegistryAssistantAgentCatalog {
        async fn list_management_agents(&self) -> Result<Vec<tjuaeui_api_types::AgentManagementRow>, AssistantError> {
            Ok(self.registry.list_management_rows().await)
        }
    }

    let agent_catalog: Arc<dyn AssistantAgentCatalogPort> = Arc::new(RegistryAssistantAgentCatalog {
        registry: services.agent_registry.clone(),
    });
    let catalog = build_assistant_catalog(services);
    let activation = Arc::new(AssistantActivationService::new(
        services.database.pool().clone(),
        catalog.clone(),
        Arc::new(SqliteSkillUserPreferenceRepository::new(
            services.database.pool().clone(),
        )),
        Arc::new(SqliteMcpServerRepository::new(services.database.pool().clone())),
        Arc::new(SqliteProviderRepository::new(services.database.pool().clone())),
        agent_catalog.clone(),
    ));
    AssistantRouterState {
        catalog,
        activation,
        agents: agent_catalog,
    }
}

fn resolve_tjuae_hub_index_snapshot() -> Option<std::path::PathBuf> {
    let mut starts = Vec::new();
    if let Ok(current) = std::env::current_dir() {
        starts.push(current);
    }
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        starts.push(directory.to_path_buf());
    }
    starts
        .iter()
        .flat_map(|start| start.ancestors().take(10))
        .flat_map(|root| {
            [
                root.join("hub/assistants.json"),
                root.join("resources/hub/assistants.json"),
            ]
        })
        .find(|path| path.is_file())
}

fn build_assistant_catalog(services: &AppServices) -> Arc<AssistantCatalogService> {
    let developer_mode =
        cfg!(debug_assertions) || std::env::var("TJUAE_DEVELOPER_MODE").is_ok_and(|value| value == "1");
    let hub_worktree = services.skill_paths.tjuae_hub_worktree_dir.clone();
    let hub_index_snapshot = resolve_tjuae_hub_index_snapshot();
    Arc::new(AssistantCatalogService::new(
        Arc::new(SqliteAssistantUserPreferenceRepository::new(
            services.database.pool().clone(),
        )),
        &services.data_dir,
        hub_worktree.clone(),
        hub_index_snapshot,
        developer_mode && hub_worktree.is_some(),
        services.git_service.clone(),
    ))
}

fn attach_assistant_runtime_catalog(services: &AppServices, conversation_service: &ConversationService) {
    conversation_service.with_assistant_runtime_catalog(Arc::new(CatalogConversationRuntime {
        catalog: build_assistant_catalog(services),
    }));
}

/// Build the default `SystemRouterState` from application services.
pub fn build_system_state(services: &AppServices) -> SystemRouterState {
    let encryption_key = derive_encryption_key(&services.jwt_secret_raw);
    let pool = services.database.pool().clone();
    let provider_repo = Arc::new(SqliteProviderRepository::new(pool.clone()));
    let http_client = reqwest::Client::new();

    SystemRouterState {
        settings_service: SettingsService::new(Arc::new(SqliteSettingsRepository::new(pool.clone()))),
        client_pref_service: ClientPrefService::with_keep_awake_controller(
            Arc::new(SqliteClientPreferenceRepository::new(pool.clone())),
            Arc::new(tjuaeui_system::SystemKeepAwakeController::new()),
        ),
        provider_service: ProviderService::new(provider_repo.clone(), encryption_key),
        model_fetch_service: ModelFetchService::new(provider_repo, encryption_key, http_client.clone()),
        protocol_detection_service: ProtocolDetectionService::new(http_client.clone()),
        version_check_service: VersionCheckService::new(http_client, env!("CARGO_PKG_VERSION").to_owned()),
        runtime_prepare_service: RuntimePrepareService::new(services.event_bus.clone()),
        feedback_diagnostics_service: FeedbackDiagnosticsService::new(Arc::new(
            SqliteFeedbackDiagnosticsRepository::new(pool),
        )),
    }
}

/// Build the default `ConversationRouterState` from application services.
pub fn build_conversation_state(
    services: &AppServices,
    cron_service: Option<Arc<tjuaeui_cron::service::CronService>>,
) -> ConversationRouterState {
    let conversation_service = services.conversation_service.clone();
    attach_assistant_runtime_catalog(services, &conversation_service);
    if let Some(cron_service) = cron_service {
        conversation_service.with_delete_hook(cron_service.clone());
    }
    ConversationRouterState {
        service: conversation_service,
        task_manager: services.worker_task_manager.clone(),
        active_leases: services.active_lease_registry.clone(),
    }
}

/// Build the default `RemoteAgentRouterState` from application services.
pub fn build_remote_agent_state(services: &AppServices) -> RemoteAgentRouterState {
    let encryption_key = derive_encryption_key(&services.jwt_secret_raw);
    let pool = services.database.pool().clone();
    let repo = Arc::new(SqliteRemoteAgentRepository::new(pool));
    RemoteAgentRouterState {
        service: Arc::new(RemoteAgentService::new(repo, encryption_key)),
    }
}

/// Build the default `ConnectionTestRouterState`.
pub fn build_connection_test_state() -> ConnectionTestRouterState {
    ConnectionTestRouterState {
        service: ConnectionTestService::new(reqwest::Client::new()),
    }
}

/// Build the default `FileRouterState` from application services.
pub fn build_file_state(services: &AppServices) -> Result<FileRouterState, RouterBuildError> {
    let broadcaster = services.event_bus.clone();
    let allowed_roots = default_allowed_roots(Some(services.work_dir.as_path()));
    let browse_roots = BrowseRoots::new();
    let file_service = Arc::new(FileService::new(broadcaster.clone(), allowed_roots.clone()));
    let watch_service = Arc::new(FileWatchService::new(broadcaster).map_err(file_watch_init_error)?);
    Ok(FileRouterState {
        file_service,
        watch_service,
        git_service: services.git_service.clone(),
        allowed_roots,
        browse_roots,
    })
}

fn file_watch_init_error(error: tjuaeui_file::FileError) -> RouterBuildError {
    RouterBuildError::new("router.file_watch", "failed to initialize file watch service").with_source(error)
}

/// Build the default `McpRouterState` from application services.
pub fn build_mcp_state(services: &AppServices) -> McpRouterState {
    let pool = services.database.pool().clone();
    let repo: Arc<dyn tjuaeui_db::IMcpServerRepository> = Arc::new(tjuaeui_db::SqliteMcpServerRepository::new(pool));

    let adapters: Vec<Arc<dyn McpAgentAdapter>> = vec![
        Arc::new(ClaudeAdapter),
        Arc::new(GeminiAdapter),
        Arc::new(QwenAdapter),
        Arc::new(CodexAdapter),
        Arc::new(CodeBuddyAdapter),
        Arc::new(OpencodeAdapter),
        Arc::new(TjuaeCliAdapter),
        Arc::new(TjuaeUIAdapter::new(repo.clone())),
    ];

    let oauth_token_repo: Arc<dyn tjuaeui_db::IOAuthTokenRepository> = Arc::new(
        tjuaeui_db::SqliteOAuthTokenRepository::new(services.database.pool().clone()),
    );
    let http_client = reqwest::Client::new();

    McpRouterState {
        config_service: McpConfigService::new(repo.clone()),
        sync_service: McpSyncService::new(repo, adapters),
        connection_test_service: McpConnectionTestService::new(http_client.clone(), services.event_bus.clone()),
        oauth_service: tjuaeui_mcp::McpOAuthService::new(oauth_token_repo, http_client),
    }
}

fn build_channel_settings_service(
    services: &AppServices,
) -> Arc<tjuaeui_channel::channel_settings::ChannelSettingsService> {
    #[derive(Clone)]
    struct RuntimeChannelCatalog {
        catalog: Arc<AssistantCatalogService>,
        registry: Arc<tjuaeui_ai_agent::AgentRegistry>,
    }

    #[async_trait::async_trait]
    impl tjuaeui_channel::channel_settings::ChannelAssistantCatalogPort for RuntimeChannelCatalog {
        async fn list_runtime_assistants(
            &self,
        ) -> Result<
            Vec<tjuaeui_channel::channel_settings::ChannelAssistantCatalogEntry>,
            tjuaeui_channel::error::ChannelError,
        > {
            let profiles = self.catalog.list_runtime_profiles().await.map_err(|error| {
                tjuaeui_channel::error::ChannelError::InvalidConfig(format!("assistant catalog unavailable: {error}"))
            })?;
            let agents = self.registry.list_management_rows().await;
            Ok(profiles
                .into_iter()
                .filter_map(|profile| {
                    let agent = agents.iter().find(|candidate| {
                        candidate.id == profile.agent_id
                            || candidate.backend.as_deref() == Some(profile.agent_id.as_str())
                            || candidate.agent_type.serde_name() == profile.agent_id
                    })?;
                    let agent_type = agent.agent_type.serde_name().to_owned();
                    Some(tjuaeui_channel::channel_settings::ChannelAssistantCatalogEntry {
                        assistant_id: profile.id,
                        name: profile.name,
                        backend: (agent_type == "acp")
                            .then(|| agent.backend.clone().unwrap_or_else(|| profile.agent_id.clone())),
                        agent_type,
                    })
                })
                .collect())
        }
    }

    let pref_repo: Arc<dyn tjuaeui_db::IClientPreferenceRepository> =
        Arc::new(SqliteClientPreferenceRepository::new(services.database.pool().clone()));

    Arc::new(tjuaeui_channel::channel_settings::ChannelSettingsService::new(
        pref_repo,
        Arc::new(RuntimeChannelCatalog {
            catalog: build_assistant_catalog(services),
            registry: services.agent_registry.clone(),
        }),
    ))
}

async fn build_channel_message_service(
    services: &AppServices,
    channel_settings: Arc<tjuaeui_channel::channel_settings::ChannelSettingsService>,
) -> Arc<tjuaeui_channel::message_service::ChannelMessageService> {
    let owner_user_id = services
        .user_repo
        .get_primary_webui_user()
        .await
        .ok()
        .flatten()
        .map(|u| u.id)
        .unwrap_or_else(|| "system_default_user".to_string());
    let conversation_service = services.conversation_service.clone();
    attach_assistant_runtime_catalog(services, &conversation_service);

    Arc::new(tjuaeui_channel::message_service::ChannelMessageService::new(
        Arc::new(conversation_service),
        services.worker_task_manager.clone(),
        channel_settings,
        owner_user_id,
    ))
}

/// Build the default `ChannelRouterState` and orchestrator components.
pub async fn build_channel_state(
    services: &AppServices,
    extension_registry: ExtensionRegistry,
) -> (ChannelRouterState, ChannelOrchestratorComponents) {
    let pool = services.database.pool().clone();
    let repo: Arc<dyn tjuaeui_db::IChannelRepository> = Arc::new(tjuaeui_db::SqliteChannelRepository::new(pool));
    let encryption_key = derive_encryption_key(&services.jwt_secret_raw);

    let (message_tx, message_rx) = tokio::sync::mpsc::channel(256);
    let (confirm_tx, confirm_rx) = tokio::sync::mpsc::channel(256);

    let manager = Arc::new(tjuaeui_channel::manager::ChannelManager::new(
        repo.clone(),
        services.event_bus.clone(),
        encryption_key,
        message_tx,
        confirm_tx,
    ));

    let pairing_service = Arc::new(tjuaeui_channel::pairing::PairingService::new(
        repo.clone(),
        services.event_bus.clone(),
    ));

    let session_manager = Arc::new(tjuaeui_channel::session::SessionManager::new(repo.clone()));

    let plugin_factory: Arc<tjuaeui_channel::manager::PluginFactory> =
        Arc::new(Box::new(tjuaeui_channel::plugins::create_plugin));

    // Build channel settings service for per-plugin agent/model configuration.
    let channel_settings = build_channel_settings_service(services);

    // Build orchestrator dependencies
    let action_executor = Arc::new(tjuaeui_channel::action::ActionExecutor::new(
        Arc::clone(&pairing_service),
        Arc::clone(&session_manager),
        Arc::clone(&channel_settings),
    ));

    let message_service = build_channel_message_service(services, Arc::clone(&channel_settings)).await;

    let orchestrator = tjuaeui_channel::orchestrator::ChannelOrchestrator::new(
        action_executor,
        message_service,
        Arc::clone(&session_manager),
        manager.clone() as Arc<dyn tjuaeui_channel::stream_relay::ChannelSender>,
    );

    let state = ChannelRouterState {
        manager: Arc::clone(&manager),
        pairing_service,
        session_manager,
        repo,
        plugin_factory: Arc::clone(&plugin_factory),
        settings_service: channel_settings,
        extension_registry,
    };

    let components = ChannelOrchestratorComponents {
        orchestrator,
        message_rx,
        confirm_rx,
        manager,
        plugin_factory,
    };

    (state, components)
}

/// Build the default `TeamRouterState` from application services.
///
/// `backend_binary_path` is resolved once in `build_module_states` via
/// `std::env::current_exe()` and cloned into each builder that needs it,
/// per `docs/teams/phase1/interface-contracts.md` §10.
pub fn build_team_state(
    services: &AppServices,
    _cron_service: Option<Arc<tjuaeui_cron::service::CronService>>,
    backend_binary_path: Arc<std::path::PathBuf>,
) -> TeamRouterState {
    #[derive(Clone)]
    struct RuntimeTeamCatalog {
        catalog: Arc<AssistantCatalogService>,
        registry: Arc<tjuaeui_ai_agent::AgentRegistry>,
    }

    #[async_trait::async_trait]
    impl TeamAssistantCatalogPort for RuntimeTeamCatalog {
        async fn list_team_selectable_assistants(
            &self,
        ) -> Result<Vec<TeamAssistantCatalogEntry>, tjuaeui_team::TeamError> {
            let assistants = self.catalog.list_runtime_profiles().await.map_err(|error| {
                tjuaeui_team::TeamError::InvalidRequest(format!("assistant catalog unavailable: {error}"))
            })?;
            let agents = self.registry.list_management_rows().await;

            Ok(assistants
                .into_iter()
                .filter_map(|assistant| {
                    let agent = agents.iter().find(|candidate| {
                        candidate.id == assistant.agent_id
                            || candidate.backend.as_deref() == Some(assistant.agent_id.as_str())
                            || candidate.agent_type.serde_name() == assistant.agent_id
                    })?;
                    if !agent.enabled
                        || !agent.installed
                        || agent.status != tjuaeui_api_types::AgentManagementStatus::Online
                    {
                        return None;
                    }
                    let backend = agent
                        .backend
                        .clone()
                        .unwrap_or_else(|| agent.agent_type.serde_name().to_owned());
                    Some(TeamAssistantCatalogEntry {
                        assistant_id: assistant.id,
                        name: assistant.name,
                        name_i18n: assistant.name_i18n,
                        backend,
                        description: assistant.description,
                        description_i18n: assistant.description_i18n,
                        skills: assistant.skill_ids,
                        model: assistant.model,
                        avatar: assistant.avatar_url,
                        recommended_prompts: assistant.recommended_prompts,
                        recommended_prompts_i18n: assistant.recommended_prompts_i18n,
                    })
                })
                .collect())
        }
    }

    let pool = services.database.pool().clone();
    let team_repo: Arc<dyn tjuaeui_db::ITeamRepository> = Arc::new(tjuaeui_db::SqliteTeamRepository::new(pool.clone()));
    let conv_service = services.conversation_service.clone();
    let conv_repo: Arc<dyn IConversationRepository> = Arc::new(SqliteConversationRepository::new(pool));
    let adapters = Arc::new(TeamConversationAdapters::new(
        conv_service,
        conv_repo,
        services.worker_task_manager.clone(),
    ));
    let conversation_port: Arc<dyn TeamConversationProvisioningPort> = adapters.clone();
    let projection_store: Arc<dyn TeamProjectionMessageStore> = adapters.clone();
    let turn_port: Arc<dyn AgentTurnExecutionPort> = adapters.clone();
    let cancellation_port: Arc<dyn AgentTurnCancellationPort> = adapters;
    let service = TeamSessionService::new_with_prompt_dump(
        team_repo,
        Arc::new(SqliteAgentMetadataRepository::new(services.database.pool().clone())),
        Arc::new(RuntimeTeamCatalog {
            catalog: build_assistant_catalog(services),
            registry: services.agent_registry.clone(),
        }),
        Arc::new(SqliteProviderRepository::new(services.database.pool().clone())),
        conversation_port,
        projection_store,
        services.event_bus.clone(),
        services.worker_task_manager.clone(),
        turn_port,
        cancellation_port,
        backend_binary_path,
        tjuaeui_team::TeamPromptDumpConfig::from_data_dir(&services.data_dir, services.dump_prompts),
    );
    service.with_project_service(Arc::new(services.project_service.clone()));
    TeamRouterState {
        service,
        active_leases: services.active_lease_registry.clone(),
    }
}

/// Build the default `CronRouterState` from application services.
pub fn build_cron_state(services: &AppServices) -> CronRouterState {
    #[derive(Clone)]
    struct RuntimeCronCatalog {
        catalog: Arc<AssistantCatalogService>,
        registry: Arc<tjuaeui_ai_agent::AgentRegistry>,
    }

    #[async_trait::async_trait]
    impl tjuaeui_cron::service::CronAssistantCatalogPort for RuntimeCronCatalog {
        async fn list_runtime_assistants(
            &self,
        ) -> Result<Vec<tjuaeui_cron::service::CronAssistantCatalogEntry>, tjuaeui_cron::error::CronError> {
            let profiles = self.catalog.list_runtime_profiles().await.map_err(|error| {
                tjuaeui_cron::error::CronError::InvalidAgentConfig(format!("assistant catalog unavailable: {error}"))
            })?;
            let agents = self.registry.list_management_rows().await;
            Ok(profiles
                .into_iter()
                .filter_map(|profile| {
                    let agent = agents.iter().find(|candidate| {
                        candidate.id == profile.agent_id
                            || candidate.backend.as_deref() == Some(profile.agent_id.as_str())
                            || candidate.agent_type.serde_name() == profile.agent_id
                    })?;
                    (agent.enabled && agent.installed).then(|| tjuaeui_cron::service::CronAssistantCatalogEntry {
                        assistant_id: profile.id,
                        name: profile.name,
                        agent_id: agent.id.clone(),
                        backend: agent
                            .backend
                            .clone()
                            .unwrap_or_else(|| agent.agent_type.serde_name().to_owned()),
                    })
                })
                .collect())
        }
    }

    let pool = services.database.pool().clone();
    let cron_repo: Arc<dyn tjuaeui_db::ICronRepository> = Arc::new(tjuaeui_db::SqliteCronRepository::new(pool.clone()));

    let conv_repo: Arc<dyn tjuaeui_db::IConversationRepository> =
        Arc::new(SqliteConversationRepository::new(pool.clone()));
    let agent_metadata_repo: Arc<dyn IAgentMetadataRepository> =
        Arc::new(SqliteAgentMetadataRepository::new(pool.clone()));
    let acp_session_repo: Arc<dyn IAcpSessionRepository> = Arc::new(SqliteAcpSessionRepository::new(pool));
    let skill_resolver = Arc::new(tjuaeui_conversation::skill_resolver::ExtensionSkillResolver::new(
        services.skill_paths.clone(),
        Arc::new(tjuaeui_db::SqliteSkillUserPreferenceRepository::new(
            services.database.pool().clone(),
        )),
        services.git_service.clone(),
    ));
    let conv_service = ConversationService::new(
        services.work_dir.clone(),
        services.event_bus.clone(),
        skill_resolver,
        services.worker_task_manager.clone(),
        conv_repo.clone(),
        agent_metadata_repo.clone(),
        acp_session_repo,
    )
    .with_runtime_state(services.conversation_runtime_state.clone())
    .with_runtime_helper_context(services.runtime_helper_bin(), services.runtime_base_url());
    conv_service.with_mcp_server_repo(Arc::new(tjuaeui_db::SqliteMcpServerRepository::new(
        services.database.pool().clone(),
    )));
    attach_assistant_runtime_catalog(services, &conv_service);
    conv_service.with_project_service(Arc::new(services.project_service.clone()));

    let executor = Arc::new(tjuaeui_cron::executor::JobExecutor::new(
        services.worker_task_manager.clone(),
        conv_repo,
        Arc::new(conv_service.clone()),
        services.work_dir.clone(),
        services.data_dir.clone(),
        services.event_bus.clone(),
        services.agent_registry.clone(),
    ));

    let tick_service_ref: Arc<CronServiceTickRef> = Arc::new(CronServiceTickRef::default());
    let tick_ref = tick_service_ref.clone();
    let scheduler = Arc::new(tjuaeui_cron::scheduler::CronScheduler::new(Arc::new(
        move |tick: tjuaeui_cron::scheduler::ScheduledTick| {
            let svc = tick_ref.0.lock().unwrap().clone();
            tokio::spawn(async move {
                if let Some(svc) = svc {
                    svc.tick(&tick.job_id, tick.scheduled_at).await;
                }
            });
        },
    )));

    let emitter = CronEventEmitter::new(services.event_bus.clone());
    let cron_service = Arc::new(tjuaeui_cron::service::CronService::new(CronServiceDeps {
        repo: cron_repo,
        agent_metadata_repo,
        assistant_catalog: Arc::new(RuntimeCronCatalog {
            catalog: build_assistant_catalog(services),
            registry: services.agent_registry.clone(),
        }),
        scheduler,
        executor,
        emitter,
        data_dir: services.data_dir.clone(),
    }));

    tick_service_ref.0.lock().unwrap().replace(cron_service.clone());

    CronRouterState {
        cron_service,
        conversation_service: conv_service,
    }
}

/// Build the default `OfficeRouterState` from application services.
pub fn build_office_state(services: &AppServices) -> OfficeRouterState {
    let data_dir = services.data_dir.as_path();
    let allowed_roots = default_allowed_roots(Some(services.work_dir.as_path()));

    let snapshot_service = Arc::new(OfficeSnapshotService::new(data_dir));
    let conversion_service = Arc::new(ConversionService::new());

    OfficeRouterState {
        snapshot_service,
        conversion_service,
        allowed_roots,
    }
}

/// Build the default `ShellRouterState` from application services.
pub fn build_shell_state(services: &AppServices) -> ShellRouterState {
    let pool = services.database.pool().clone();
    let client_pref_repo = Arc::new(SqliteClientPreferenceRepository::new(pool));
    let client_pref_service = ClientPrefService::new(client_pref_repo);

    ShellRouterState {
        shell_service: Arc::new(tjuaeui_shell::ShellService::new(Arc::new(
            tjuaeui_shell::DefaultSystemOpener,
        ))),
        stt_service: Arc::new(tjuaeui_shell::SttService::new(reqwest::Client::new())),
        client_pref_service,
    }
}

/// Helper to break the circular reference between CronScheduler and CronService.
#[derive(Default)]
struct CronServiceTickRef(std::sync::Mutex<Option<Arc<tjuaeui_cron::service::CronService>>>);

/// Build the default extension-related router states.
///
/// Returns `(ExtensionRouterState, HubRouterState, SkillRouterState)`.
pub async fn build_extension_states(
    services: &AppServices,
) -> (ExtensionRouterState, HubRouterState, SkillRouterState) {
    let skill_data_dir = services.data_dir.clone();

    let state_store = ExtensionStateStore::new(resolve_state_file_path(&skill_data_dir));
    let registry = ExtensionRegistry::new(state_store, services.event_bus.clone(), services.app_version.clone());

    let hub_dir = resolve_install_target_dir_for_data_dir(&skill_data_dir);
    let index_manager = HubIndexManager::new(hub_dir, registry.clone());
    let installer = HubInstaller::new(index_manager.clone(), registry.clone());

    let ext_state = ExtensionRouterState {
        registry: registry.clone(),
    };

    let hub_state = HubRouterState {
        index_manager,
        installer,
    };

    let developer_mode =
        cfg!(debug_assertions) || std::env::var("TJUAE_DEVELOPER_MODE").is_ok_and(|value| value == "1");
    let skill_state = SkillRouterState {
        skill_paths: services.skill_paths.as_ref().clone(),
        git: services.git_service.clone(),
        preferences: Arc::new(tjuaeui_db::SqliteSkillUserPreferenceRepository::new(
            services.database.pool().clone(),
        )),
        can_write_tjuae_hub: developer_mode && services.skill_paths.tjuae_hub_worktree_dir.is_some(),
    };

    (ext_state, hub_state, skill_state)
}

/// Build the default `WsHandlerState` from application services.
pub fn build_ws_state(services: &AppServices) -> WsHandlerState {
    if services.local {
        return WsHandlerState {
            manager: services.ws_manager.clone(),
            router: Arc::new(NoopMessageRouter),
            token_validator: Arc::new(|_| true),
            token_extractor: Arc::new(|_| Some("local".into())),
        };
    }

    let jwt_service = services.jwt_service.clone();
    let token_validator = Arc::new(move |token: &str| jwt_service.verify(token).is_ok());

    let token_extractor = Arc::new(|headers: &axum::http::HeaderMap| extract_token_from_ws_headers(headers));

    WsHandlerState {
        manager: services.ws_manager.clone(),
        router: Arc::new(NoopMessageRouter),
        token_validator,
        token_extractor,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use crate::AppConfig;
    use tjuaeui_ai_agent::types::{BuildTaskOptions, SendMessageData, TJUAE_BASE_URL_ENV, TJUAE_HELPER_BIN_ENV};
    use tjuaeui_ai_agent::{
        AgentError, AgentInstance, AgentSendError, AgentStreamEvent, IAgentTask, IMockAgent, IWorkerTaskManager,
        WorkerTaskManagerImpl,
    };
    use tjuaeui_api_types::{
        AssistantIdentityResponse, AssistantSourceResponse, CreateConversationRequest, CreateMineAssistantRequest,
        SaveAssistantCatalogFileRequest, SendMessageRequest,
    };
    use tjuaeui_channel::types::PluginType;
    use tjuaeui_common::{AgentKillReason, AgentType, ConversationStatus, TimestampMs};
    use tjuaeui_db::models::AssistantSessionRow;
    use tjuaeui_db::{
        IAssistantUserPreferenceRepository, IClientPreferenceRepository, IConversationRepository,
        SqliteAssistantUserPreferenceRepository, SqliteClientPreferenceRepository, SqliteConversationRepository,
        UpsertAssistantUserPreferenceParams,
    };
    use tjuaeui_extension::{ExtensionSource, ScanPath};

    struct ChannelStateNoopAgent {
        conversation_id: String,
        workspace: String,
    }

    #[async_trait::async_trait]
    impl IAgentTask for ChannelStateNoopAgent {
        fn agent_type(&self) -> AgentType {
            AgentType::TjuaeCli
        }

        fn conversation_id(&self) -> &str {
            &self.conversation_id
        }

        fn workspace(&self) -> &str {
            &self.workspace
        }

        fn status(&self) -> Option<ConversationStatus> {
            Some(ConversationStatus::Finished)
        }

        fn last_activity_at(&self) -> TimestampMs {
            0
        }

        fn subscribe(&self) -> tokio::sync::broadcast::Receiver<AgentStreamEvent> {
            let (tx, _) = tokio::sync::broadcast::channel(1);
            tx.subscribe()
        }

        async fn send_message(&self, _data: SendMessageData) -> Result<(), AgentSendError> {
            Ok(())
        }

        async fn cancel(&self) -> Result<(), AgentError> {
            Ok(())
        }

        fn kill(&self, _reason: Option<AgentKillReason>) -> Result<(), AgentError> {
            Ok(())
        }
    }

    impl IMockAgent for ChannelStateNoopAgent {}

    fn mock_worker_task_manager() -> Arc<dyn IWorkerTaskManager> {
        let factory = Arc::new(|opts: BuildTaskOptions| {
            Box::pin(async move {
                Ok(AgentInstance::Mock(Arc::new(ChannelStateNoopAgent {
                    conversation_id: opts.conversation_id().to_owned(),
                    workspace: opts.context.workspace.path,
                })))
            }) as futures_util::future::BoxFuture<'static, Result<AgentInstance, AgentError>>
        });

        Arc::new(WorkerTaskManagerImpl::new(factory))
    }
    type CapturedEnv = Vec<Vec<(String, String)>>;
    fn capturing_worker_task_manager(captured_env: Arc<Mutex<CapturedEnv>>) -> Arc<dyn IWorkerTaskManager> {
        let factory = Arc::new(move |opts: BuildTaskOptions| {
            let captured_env = captured_env.clone();
            Box::pin(async move {
                let conversation_id = opts.conversation_id().to_owned();
                let workspace = opts.context.workspace.path.clone();
                captured_env.lock().unwrap().push(opts.context.runtime_env.clone());
                Ok(AgentInstance::Mock(Arc::new(ChannelStateNoopAgent {
                    conversation_id,
                    workspace,
                })))
            }) as futures_util::future::BoxFuture<'static, Result<AgentInstance, AgentError>>
        });

        Arc::new(WorkerTaskManagerImpl::new(factory))
    }

    async fn wait_for_captured_env(captured_env: &Arc<Mutex<CapturedEnv>>) -> Vec<(String, String)> {
        for _ in 0..50 {
            if let Some(env) = captured_env.lock().unwrap().first().cloned() {
                return env;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("expected task options to be captured");
    }

    fn make_send_message_request() -> SendMessageRequest {
        serde_json::from_value(serde_json::json!({
            "content": "Check runtime env"
        }))
        .unwrap()
    }

    async fn seed_channel_state_assistant(services: &AppServices) -> String {
        let catalog = build_assistant_catalog(services);
        catalog
            .create_mine(CreateMineAssistantRequest {
                slug: "bare-channel-tjuaecli".to_owned(),
                name: "Bare Channel TjuaeCLI".to_owned(),
                description: "Channel state regression assistant".to_owned(),
            })
            .await
            .unwrap();
        let identity = AssistantIdentityResponse {
            source: AssistantSourceResponse::Mine,
            namespace: String::new(),
            slug: "bare-channel-tjuaecli".to_owned(),
        };
        let mut manifest: serde_json::Value = serde_json::from_str(
            &catalog
                .file_content(&identity, None, "_meta.json")
                .await
                .unwrap()
                .content,
        )
        .unwrap();
        manifest["defaults"]["agent"] = serde_json::Value::String("632f31d2".to_owned());
        catalog
            .save_file(
                &identity,
                SaveAssistantCatalogFileRequest {
                    path: "_meta.json".to_owned(),
                    content: serde_json::to_string_pretty(&manifest).unwrap(),
                },
            )
            .await
            .unwrap();
        SqliteAssistantUserPreferenceRepository::new(services.database.pool().clone())
            .upsert(UpsertAssistantUserPreferenceParams {
                source: "mine",
                namespace: "",
                slug: "bare-channel-tjuaecli",
                selected_version: Some("0.1.0"),
                follow_latest: false,
                enabled: true,
                activation_status: "ready",
                activation_fingerprint: Some("test-ready"),
                resource_bindings: "{}",
                runtime_overrides: "{}",
                sort_order: 0,
                last_used_at: None,
            })
            .await
            .unwrap();
        "mine::bare-channel-tjuaecli".to_owned()
    }

    #[tokio::test]
    async fn build_channel_message_service_uses_app_conversation_service_for_assistant_bindings() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = AppConfig {
            data_dir: tmp.path().join("data"),
            work_dir: tmp.path().join("work"),
            ..Default::default()
        };
        let db = tjuaeui_db::init_database_memory().await.unwrap();
        let services = AppServices::from_config(db, &config)
            .await
            .unwrap()
            .with_worker_task_manager(mock_worker_task_manager());

        let pool = services.database.pool().clone();
        let assistant_id = seed_channel_state_assistant(&services).await;

        let pref_repo = SqliteClientPreferenceRepository::new(pool.clone());
        pref_repo
            .upsert_batch(&[(
                "assistant.weixin.agent",
                &format!(r#"{{"assistant_id":"{assistant_id}","name":"Weixin TjuaeCLI"}}"#),
            )])
            .await
            .unwrap();

        let settings = build_channel_settings_service(&services);
        let message_service = build_channel_message_service(&services, settings).await;
        let session = AssistantSessionRow {
            id: "session-channel-state".to_owned(),
            user_id: "channel-user-state".to_owned(),
            agent_type: "tjuaecli".to_owned(),
            conversation_id: None,
            workspace: None,
            chat_id: Some("wx-chat-state".to_owned()),
            created_at: 1,
            last_activity: 1,
        };

        let first = message_service
            .send_to_agent(&session, "hello", PluginType::Weixin)
            .await
            .unwrap();

        let conversation_repo = SqliteConversationRepository::new(pool);
        let snapshot = conversation_repo
            .get_assistant_snapshot(&first.conversation_id)
            .await
            .unwrap()
            .expect("channel-created conversation should persist assistant snapshot");
        let conversation = conversation_repo
            .get(&first.conversation_id)
            .await
            .unwrap()
            .expect("channel-created conversation should be persisted");

        assert_eq!(snapshot.assistant_id, assistant_id);
        assert_eq!(snapshot.agent_id, "632f31d2");
        assert_eq!(conversation.r#type, AgentType::TjuaeCli.serde_name());
        assert_eq!(conversation.name, "Weixin TjuaeCLI");

        let second_session = AssistantSessionRow {
            conversation_id: Some(first.conversation_id.clone()),
            ..session
        };
        let second = message_service
            .send_to_agent(&second_session, "again", PluginType::Weixin)
            .await
            .unwrap();
        assert_eq!(second.conversation_id, first.conversation_id);

        services.database.close().await;
    }

    #[tokio::test]
    async fn build_cron_state_conversation_service_injects_runtime_helper_context() {
        let tmp = tempfile::TempDir::new().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let config = AppConfig {
            data_dir: tmp.path().join("data"),
            work_dir: tmp.path().join("work"),
            ..Default::default()
        };
        let db = tjuaeui_db::init_database_memory().await.unwrap();
        let captured_env = Arc::new(Mutex::new(Vec::new()));
        let task_manager = capturing_worker_task_manager(captured_env.clone());
        let services = AppServices::from_config(db, &config)
            .await
            .unwrap()
            .with_worker_task_manager(task_manager.clone());
        let cron = build_cron_state(&services);
        let conversation = cron
            .conversation_service
            .create(
                "system_default_user",
                serde_json::from_value::<CreateConversationRequest>(serde_json::json!({
                    "type": "acp",
                    "extra": {
                        "workspace": workspace,
                        "custom_workspace": true
                    }
                }))
                .unwrap(),
            )
            .await
            .unwrap();

        cron.conversation_service
            .send_message(
                "system_default_user",
                &conversation.id,
                make_send_message_request(),
                &task_manager,
            )
            .await
            .unwrap();

        let env = wait_for_captured_env(&captured_env).await;
        assert!(
            env.iter()
                .any(|(key, value)| key == TJUAE_HELPER_BIN_ENV && !value.is_empty()),
            "cron conversation runtime env should include TJUAE_HELPER_BIN"
        );
        assert!(
            env.contains(&(TJUAE_BASE_URL_ENV.to_owned(), config.local_base_url())),
            "cron conversation runtime env should include TJUAE_BASE_URL"
        );

        services.database.close().await;
    }

    #[tokio::test]
    async fn build_extension_states_uses_host_app_version_for_engine_filtering() {
        let tmp = tempfile::TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");
        let ext_root = tmp.path().join("extensions");
        let ext_dir = ext_root.join("demo-ext");

        std::fs::create_dir_all(&ext_dir).unwrap();
        std::fs::write(
            ext_dir.join("tjuae-extension.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "name": "demo-ext",
                "version": "1.0.0",
                "engine": {
                    "tjuae": "^2.0.0"
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let db = tjuaeui_db::init_database_memory().await.unwrap();
        let config = AppConfig {
            data_dir: data_dir.clone(),
            work_dir: data_dir,
            app_version: "2.1.0".to_string(),
            ..Default::default()
        };
        let services = AppServices::from_config(db, &config).await.unwrap();

        let (ext_state, _hub_state, _skill_state) = build_extension_states(&services).await;
        ext_state
            .registry
            .initialize_with_scan_paths(vec![ScanPath {
                path: ext_root,
                source: ExtensionSource::Local,
            }])
            .await
            .unwrap();

        let loaded = ext_state.registry.get_loaded_extensions().await;
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "demo-ext");

        services.database.close().await;
    }

    #[test]
    fn file_watch_init_error_maps_to_bootstrap_server_failed() {
        let err = file_watch_init_error(tjuaeui_file::FileError::Internal("watch backend unavailable".into()));

        assert_eq!(err.stage(), "router.file_watch");
        assert_eq!(err.message(), "failed to initialize file watch service");
        assert!(!err.to_string().contains("watch backend unavailable"));
    }
}
