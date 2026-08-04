use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use tjuaeui_api_types::{
    AgentHandshake, AgentManagementRow, AgentMetadata, AgentSnapshotCheckKind, AgentSnapshotCheckStatus, AgentSource,
};
use tjuaeui_common::AgentType;
use tjuaeui_common::now_ms;
use tjuaeui_db::{IProviderRepository, UpdateAgentAvailabilitySnapshotParams};
use tjuaeui_process::Spawner;

use crate::error::AgentError;
use crate::protocol::engine_adapter_probe;
use crate::registry::{AgentRegistry, guidance_for_snapshot_error_code};
use crate::services::direct_diagnostic::{DirectProbeFailure, probe_direct_session_catalog};

#[async_trait::async_trait]
pub trait AgentAvailabilityFeedbackPort: Send + Sync {
    async fn record_session_success(&self, agent_id: &str) -> Result<(), AgentError>;
    async fn record_session_failure(&self, agent_id: &str, code: &str, message: &str) -> Result<(), AgentError>;
}

struct AvailabilitySnapshot {
    status: &'static str,
    kind: &'static str,
    error_code: Option<String>,
    error_message: Option<String>,
    latency_ms: i64,
    checked_at: i64,
    catalog: Option<AgentHandshake>,
}

#[derive(Clone)]
pub struct AgentAvailabilityService {
    registry: Arc<AgentRegistry>,
    // Used to decide tjuae_cli (built-in, no external CLI) availability: it is
    // usable only when at least one model provider is configured & enabled.
    provider_repo: Arc<dyn IProviderRepository>,
    session_spawner: Arc<dyn Spawner>,
}

impl AgentAvailabilityService {
    pub fn new(
        registry: Arc<AgentRegistry>,
        provider_repo: Arc<dyn IProviderRepository>,
        session_spawner: Arc<dyn Spawner>,
    ) -> Self {
        Self {
            registry,
            provider_repo,
            session_spawner,
        }
    }

    pub async fn list_management_rows(&self) -> Vec<AgentManagementRow> {
        self.registry.list_management_rows().await
    }

    pub async fn run_diagnostic(
        &self,
        id: &str,
        kind: AgentSnapshotCheckKind,
    ) -> Result<AgentManagementRow, AgentError> {
        let meta = self
            .registry
            .reload_one(id)
            .await
            .and_then(|row| row.ok_or_else(|| AgentError::not_found(format!("找不到 Agent“{id}”"))))?;

        // #675: never short-circuit on a stale availability verdict — the
        // manual check is the user's self-rescue path. `run_probe` handles a
        // missing binary itself (persisted command_not_found snapshot), and a
        // success restores the agent.
        let snapshot = run_probe(&self.registry, &self.provider_repo, &self.session_spawner, &meta, kind).await;
        if let Some(catalog) = snapshot.catalog.as_ref() {
            self.registry.apply_diagnostic_catalog(id, catalog).await?;
        }
        self.persist_snapshot(id, &snapshot).await?;
        self.management_row_by_id(id)
            .await
            .ok_or_else(|| AgentError::not_found(format!("找不到 Agent“{id}”")))
    }

    pub async fn record_session_failure(&self, agent_id: &str, code: &str, message: &str) -> Result<(), AgentError> {
        let checked_at = now_ms();
        let snapshot = AvailabilitySnapshot {
            status: "offline",
            kind: "session",
            error_code: Some(code.to_owned()),
            error_message: Some(message.to_owned()),
            latency_ms: 0,
            checked_at,
            catalog: None,
        };
        self.persist_snapshot(agent_id, &snapshot).await
    }

    pub async fn record_session_success(&self, agent_id: &str) -> Result<(), AgentError> {
        let checked_at = now_ms();
        let snapshot = AvailabilitySnapshot {
            status: "online",
            kind: "session",
            error_code: None,
            error_message: None,
            latency_ms: 0,
            checked_at,
            catalog: None,
        };
        self.persist_snapshot(agent_id, &snapshot).await
    }

    pub async fn management_row_by_id(&self, id: &str) -> Option<AgentManagementRow> {
        self.registry.management_row_by_id(id).await
    }

    async fn persist_snapshot(&self, id: &str, snapshot: &AvailabilitySnapshot) -> Result<(), AgentError> {
        let existing = self
            .registry
            .repo_handle()
            .get(id)
            .await
            .map_err(|error| AgentError::internal(format!("读取 Agent 仓储失败：{error}")))?
            .ok_or_else(|| AgentError::not_found(format!("找不到 Agent“{id}”")))?;

        let params = UpdateAgentAvailabilitySnapshotParams {
            last_check_status: Some(snapshot.status),
            last_check_kind: Some(snapshot.kind),
            last_check_error_code: snapshot.error_code.as_deref(),
            last_check_error_message: snapshot.error_message.as_deref(),
            last_check_guidance: snapshot.error_code.as_deref().and_then(|code| {
                let guidance = guidance_for_snapshot_error_code(code);
                (!guidance.is_empty()).then_some(guidance)
            }),
            last_check_latency_ms: Some(snapshot.latency_ms),
            last_check_at: Some(snapshot.checked_at),
            last_success_at: if snapshot.status == "online" {
                Some(snapshot.checked_at)
            } else {
                existing.last_success_at
            },
            last_failure_at: if snapshot.status == "offline" {
                Some(snapshot.checked_at)
            } else {
                existing.last_failure_at
            },
        };
        self.registry
            .repo_handle()
            .update_availability_snapshot(id, &params)
            .await
            .map_err(|error| AgentError::internal(format!("更新 Agent 可用性快照失败：{error}")))?;
        self.registry.reload_one(id).await?;
        Ok(())
    }
}

async fn run_probe(
    _registry: &Arc<AgentRegistry>,
    provider_repo: &Arc<dyn IProviderRepository>,
    session_spawner: &Arc<dyn Spawner>,
    meta: &AgentMetadata,
    kind: AgentSnapshotCheckKind,
) -> AvailabilitySnapshot {
    let started_at = now_ms();
    let start = Instant::now();

    let (status, error_code, error_message, catalog) = if !meta.enabled {
        (
            AgentSnapshotCheckStatus::Offline,
            Some("disabled".to_owned()),
            Some("Agent 已禁用".to_owned()),
            None,
        )
    } else if !meta.available {
        match crate::cli_probe::validate_with_budget(meta, crate::cli_probe::CLI_VERSION_RECHECK_TIMEOUT).await {
            Ok(_) => (
                AgentSnapshotCheckStatus::Offline,
                Some("runtime_unavailable".to_owned()),
                Some("Agent 命令可执行，但运行时目录仍不可用".to_owned()),
                None,
            ),
            Err(failure) => (
                AgentSnapshotCheckStatus::Offline,
                Some(failure.error_code().to_owned()),
                Some(failure.detail()),
                None,
            ),
        }
    } else if meta.agent_source == AgentSource::Builtin
        && matches!(meta.backend.as_deref(), Some("claude") | Some("codex"))
    {
        match crate::cli_probe::validate_with_budget(meta, crate::cli_probe::CLI_VERSION_RECHECK_TIMEOUT).await {
            Ok(_) => match probe_direct_session_catalog(meta, session_spawner.clone()).await {
                Ok(catalog) => (AgentSnapshotCheckStatus::Online, None, None, Some(catalog)),
                Err(DirectProbeFailure::Catalog(message)) => (
                    AgentSnapshotCheckStatus::Online,
                    Some("catalog_load_failed".to_owned()),
                    Some(message),
                    None,
                ),
                Err(DirectProbeFailure::Connection { code, message }) => {
                    (AgentSnapshotCheckStatus::Offline, Some(code), Some(message), None)
                }
            },
            Err(failure) => (
                AgentSnapshotCheckStatus::Offline,
                Some(failure.error_code().to_owned()),
                Some(failure.detail()),
                None,
            ),
        }
    } else if let Some(command) = meta.command.as_deref() {
        let env: HashMap<String, String> = meta
            .env
            .iter()
            .map(|entry| (entry.name.clone(), entry.value.clone()))
            .collect();
        match explicit_probe_args(meta) {
            Err(error) => (
                AgentSnapshotCheckStatus::Offline,
                Some("package_lock_invalid".to_owned()),
                Some(error),
                None,
            ),
            Ok(args) => match engine_adapter_probe::probe_engine_adapter_detailed(command, &args, &env, None).await {
                engine_adapter_probe::EngineAdapterProbeResult::Success { handshake } => {
                    (AgentSnapshotCheckStatus::Online, None, None, Some(handshake))
                }
                engine_adapter_probe::EngineAdapterProbeResult::FailCli { error } => (
                    AgentSnapshotCheckStatus::Offline,
                    Some("command_not_found".to_owned()),
                    Some(error),
                    None,
                ),
                engine_adapter_probe::EngineAdapterProbeResult::FailAcp { code, error } => (
                    AgentSnapshotCheckStatus::Offline,
                    Some(code.to_owned()),
                    Some(error),
                    None,
                ),
                // Reachable but not authorized: still offline (unusable), but a
                // dedicated code lets the UI guide the user to log in.
                engine_adapter_probe::EngineAdapterProbeResult::FailAuth { error } => (
                    AgentSnapshotCheckStatus::Offline,
                    Some("auth_required".to_owned()),
                    Some(error),
                    None,
                ),
            },
        }
    } else if meta.backend.is_some() {
        // Commandless builtin fallback: same PATH + `--version` treatment as
        // the direct CLIs — no PATH-only side door (#675).
        match crate::cli_probe::validate_with_budget(meta, crate::cli_probe::CLI_VERSION_RECHECK_TIMEOUT).await {
            Ok(_) => (AgentSnapshotCheckStatus::Online, None, None, None),
            Err(failure) => (
                AgentSnapshotCheckStatus::Offline,
                Some(failure.error_code().to_owned()),
                Some(failure.detail()),
                None,
            ),
        }
    } else if meta.agent_type == AgentType::TjuaeCli {
        // tjuae_cli is the built-in Rust agent: there is no external CLI to probe,
        // so its usability hinges entirely on having a configured model. It is
        // online only when at least one model provider is enabled — otherwise
        // it cannot run a single turn.
        let (status, code, message) = probe_tjuae_cli_provider_readiness(provider_repo).await;
        (status, code, message, None)
    } else {
        (AgentSnapshotCheckStatus::Online, None, None, None)
    };

    let latency_ms = start.elapsed().as_millis() as i64;
    let status = match status {
        AgentSnapshotCheckStatus::Online => "online",
        AgentSnapshotCheckStatus::Offline => "offline",
    };

    AvailabilitySnapshot {
        status,
        kind: match kind {
            AgentSnapshotCheckKind::Startup => "startup",
            AgentSnapshotCheckKind::Scheduled => "scheduled",
            AgentSnapshotCheckKind::Manual => "manual",
            AgentSnapshotCheckKind::Session => "session",
        },
        error_code,
        error_message,
        latency_ms,
        checked_at: started_at,
        catalog,
    }
}

fn explicit_probe_args(meta: &AgentMetadata) -> Result<Vec<String>, String> {
    if meta.agent_source == AgentSource::Builtin && meta.agent_source_info.bridge_binary.as_deref() == Some("npx") {
        let backend = meta
            .backend
            .as_deref()
            .ok_or_else(|| "builtin npx agent has no backend".to_owned())?;
        return tjuaeui_runtime::pin_registry_npx_args(backend, &meta.args).map_err(|error| error.to_string());
    }
    Ok(meta.args.clone())
}

/// Readiness check for the built-in tjuae_cli agent.
///
/// tjuae_cli has no external CLI; it runs models through configured providers.
/// Mirrors `AssistantService::resolve_default_agent_type`, which treats tjuae_cli
/// as usable exactly when at least one provider is enabled. With no enabled
/// provider it cannot complete a turn, so we report it offline with a
/// `no_provider` code the UI maps to "configure a model" guidance.
async fn probe_tjuae_cli_provider_readiness(
    provider_repo: &Arc<dyn IProviderRepository>,
) -> (AgentSnapshotCheckStatus, Option<String>, Option<String>) {
    match provider_repo.list().await {
        Ok(providers) if providers.iter().any(|p| p.enabled) => (AgentSnapshotCheckStatus::Online, None, None),
        Ok(_) => (
            AgentSnapshotCheckStatus::Offline,
            Some("no_provider".to_owned()),
            Some("尚未配置模型提供商。请添加并启用提供商后再使用内置 Agent。".to_owned()),
        ),
        Err(e) => (
            AgentSnapshotCheckStatus::Offline,
            Some("no_provider".to_owned()),
            Some(format!("读取模型提供商失败：{e}")),
        ),
    }
}

#[async_trait::async_trait]
impl AgentAvailabilityFeedbackPort for AgentAvailabilityService {
    async fn record_session_success(&self, agent_id: &str) -> Result<(), AgentError> {
        AgentAvailabilityService::record_session_success(self, agent_id).await
    }

    async fn record_session_failure(&self, agent_id: &str, code: &str, message: &str) -> Result<(), AgentError> {
        AgentAvailabilityService::record_session_failure(self, agent_id, code, message).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{AgentAvailabilityService, explicit_probe_args, probe_tjuae_cli_provider_readiness, run_probe};
    use crate::registry::AgentRegistry;
    use tjuaeui_api_types::{
        AgentHandshake, AgentManagementStatus, AgentMetadata, AgentSnapshotCheckKind, AgentSnapshotCheckStatus,
        AgentSource, AgentSourceInfo, BehaviorPolicy,
    };
    use tjuaeui_common::AgentType;
    use tjuaeui_common::CommandSpec;
    use tjuaeui_db::{
        CreateProviderParams, IAgentMetadataRepository, IProviderRepository, SqliteAgentMetadataRepository,
        SqliteProviderRepository, UpsertAgentMetadataParams, init_database_memory,
    };
    use tjuaeui_process::{ManagedProcess, ProcessError, Spawner};

    struct FailingSpawner;

    #[async_trait::async_trait]
    impl Spawner for FailingSpawner {
        async fn spawn(
            &self,
            _spec: CommandSpec,
            _extra_env: &[(String, String)],
            _opaque_owner_tag: &str,
        ) -> Result<Arc<ManagedProcess>, ProcessError> {
            Err(ProcessError::internal("diagnostic test spawner"))
        }
    }

    fn test_spawner() -> Arc<dyn Spawner> {
        Arc::new(FailingSpawner)
    }

    fn enabled_provider_params() -> CreateProviderParams<'static> {
        CreateProviderParams {
            id: None,
            platform: "openai",
            name: "OpenAI",
            base_url: "https://api.openai.com",
            api_key_encrypted: "enc",
            models: r#"["gpt-4"]"#,
            enabled: true,
            capabilities: r#"[{"type":"text"}]"#,
            context_limit: None,
            model_protocols: None,
            model_enabled: None,
            model_health: None,
            model_settings: "{}",
            bedrock_config: None,
            is_full_url: false,
        }
    }

    #[tokio::test]
    async fn tjuae_cli_is_offline_without_an_enabled_provider() {
        let db = init_database_memory().await.unwrap();
        let provider_repo: Arc<dyn IProviderRepository> = Arc::new(SqliteProviderRepository::new(db.pool().clone()));

        let (status, code, _msg) = probe_tjuae_cli_provider_readiness(&provider_repo).await;

        assert_eq!(status, AgentSnapshotCheckStatus::Offline);
        assert_eq!(code.as_deref(), Some("no_provider"));
    }

    #[tokio::test]
    async fn tjuae_cli_is_online_when_a_provider_is_enabled() {
        let db = init_database_memory().await.unwrap();
        let provider_repo: Arc<dyn IProviderRepository> = Arc::new(SqliteProviderRepository::new(db.pool().clone()));
        provider_repo.create(enabled_provider_params()).await.unwrap();

        let (status, code, _msg) = probe_tjuae_cli_provider_readiness(&provider_repo).await;

        assert_eq!(status, AgentSnapshotCheckStatus::Online);
        assert!(code.is_none());
    }

    #[tokio::test]
    async fn record_session_failure_persists_unavailable_snapshot() {
        let db = init_database_memory().await.unwrap();
        let repo: Arc<dyn IAgentMetadataRepository> = Arc::new(SqliteAgentMetadataRepository::new(db.pool().clone()));

        repo.upsert(&UpsertAgentMetadataParams {
            id: "agent-session-failure",
            icon: None,
            name: "Session Failure Agent",
            name_i18n: None,
            description: None,
            description_i18n: None,
            backend: Some("claude"),
            agent_type: "acp",
            agent_source: "custom",
            agent_source_info: Some(r#"{"binary_name":"cargo"}"#),
            enabled: true,
            command: Some("cargo"),
            args: Some("[]"),
            env: Some("[]"),
            native_skills_dirs: None,
            behavior_policy: None,
            yolo_id: None,
            agent_capabilities: None,
            auth_methods: None,
            config_options: None,
            available_modes: None,
            available_models: None,
            available_commands: None,
            sort_order: 100,
        })
        .await
        .unwrap();

        let registry = AgentRegistry::new(repo);
        registry.hydrate().await.unwrap();

        let provider_repo: Arc<dyn IProviderRepository> = Arc::new(SqliteProviderRepository::new(db.pool().clone()));
        let service = AgentAvailabilityService::new(registry.clone(), provider_repo, test_spawner());
        service
            .record_session_failure(
                "agent-session-failure",
                "session_send_failed",
                "provider returned 401 invalid api key",
            )
            .await
            .unwrap();

        let row = service
            .list_management_rows()
            .await
            .into_iter()
            .find(|item| item.id == "agent-session-failure")
            .unwrap();

        assert_eq!(row.status, AgentManagementStatus::Offline);
        assert_eq!(row.last_check_status, Some(AgentSnapshotCheckStatus::Offline));
        assert_eq!(row.last_check_kind, Some(AgentSnapshotCheckKind::Session));
        assert_eq!(row.last_check_error_code.as_deref(), Some("session_send_failed"));
        assert_eq!(
            row.last_check_error_message.as_deref(),
            Some("provider returned 401 invalid api key")
        );
        assert_eq!(
            row.last_check_guidance.as_deref(),
            Some("修复导致上次会话失败的提供商凭据或网络问题，然后开始新对话。")
        );
        assert!(row.last_failure_at.is_some());
    }

    #[tokio::test]
    async fn record_session_success_persists_online_snapshot() {
        let db = init_database_memory().await.unwrap();
        let repo: Arc<dyn IAgentMetadataRepository> = Arc::new(SqliteAgentMetadataRepository::new(db.pool().clone()));

        repo.upsert(&UpsertAgentMetadataParams {
            id: "agent-session-success",
            icon: None,
            name: "Session Success Agent",
            name_i18n: None,
            description: None,
            description_i18n: None,
            backend: Some("claude"),
            agent_type: "acp",
            agent_source: "custom",
            agent_source_info: Some(r#"{"binary_name":"cargo"}"#),
            enabled: true,
            command: Some("cargo"),
            args: Some("[]"),
            env: Some("[]"),
            native_skills_dirs: None,
            behavior_policy: None,
            yolo_id: None,
            agent_capabilities: None,
            auth_methods: None,
            config_options: None,
            available_modes: None,
            available_models: None,
            available_commands: None,
            sort_order: 100,
        })
        .await
        .unwrap();

        let registry = AgentRegistry::new(repo);
        registry.hydrate().await.unwrap();

        let provider_repo: Arc<dyn IProviderRepository> = Arc::new(SqliteProviderRepository::new(db.pool().clone()));
        let service = AgentAvailabilityService::new(registry.clone(), provider_repo, test_spawner());
        service
            .record_session_failure(
                "agent-session-success",
                "session_send_failed",
                "provider returned 401 invalid api key",
            )
            .await
            .unwrap();

        service.record_session_success("agent-session-success").await.unwrap();

        let row = service
            .list_management_rows()
            .await
            .into_iter()
            .find(|item| item.id == "agent-session-success")
            .unwrap();

        assert_eq!(row.status, AgentManagementStatus::Online);
        assert_eq!(row.last_check_status, Some(AgentSnapshotCheckStatus::Online));
        assert_eq!(row.last_check_kind, Some(AgentSnapshotCheckKind::Session));
        assert!(row.last_check_error_code.is_none());
        assert!(row.last_check_error_message.is_none());
        assert!(row.last_check_guidance.is_none());
        assert!(row.last_success_at.is_some());
        assert!(row.last_failure_at.is_some());
    }

    #[tokio::test]
    async fn managed_builtin_probe_checks_primary_binary_before_running_bridge_command() {
        let db = init_database_memory().await.unwrap();
        let repo: Arc<dyn IAgentMetadataRepository> = Arc::new(SqliteAgentMetadataRepository::new(db.pool().clone()));
        let provider_repo: Arc<dyn IProviderRepository> = Arc::new(SqliteProviderRepository::new(db.pool().clone()));
        let registry = AgentRegistry::new(repo);
        registry.hydrate().await.unwrap();

        let meta = AgentMetadata {
            id: "agent-managed-builtin".into(),
            icon: None,
            name: "Claude Code".into(),
            name_i18n: None,
            description: None,
            description_i18n: None,
            backend: Some("claude".into()),
            agent_type: AgentType::Acp,
            agent_source: AgentSource::Builtin,
            agent_source_info: AgentSourceInfo {
                binary_name: Some("definitely-missing-claude-cli".into()),
                bridge_binary: Some("npx".into()),
                hub_package_id: None,
                tjuae_local_asset_id: None,
                version: None,
            },
            enabled: true,
            available: true,
            command: Some("npx".into()),
            resolved_command: None,
            args: vec!["--yes".into(), "@agentclientprotocol/claude-agent-acp@0.58.1".into()],
            env: vec![],
            native_skills_dirs: Some(vec![".claude/skills".into()]),
            behavior_policy: BehaviorPolicy::default(),
            yolo_id: Some("bypassPermissions".into()),
            sort_order: 3100,
            team_capable: true,
            last_check_status: None,
            last_check_kind: None,
            last_check_error_code: None,
            last_check_error_message: None,
            last_check_error_details: None,
            last_check_guidance: None,
            last_check_latency_ms: None,
            last_check_at: None,
            last_success_at: None,
            last_failure_at: None,
            handshake: AgentHandshake::default(),
            has_command_override: false,
            env_override_key_count: 0,
        };

        let snapshot = run_probe(
            &registry,
            &provider_repo,
            &test_spawner(),
            &meta,
            AgentSnapshotCheckKind::Manual,
        )
        .await;

        assert_eq!(snapshot.status, "offline");
        assert_eq!(snapshot.error_code.as_deref(), Some("command_not_found"));
        assert!(
            snapshot
                .error_message
                .as_deref()
                .is_some_and(|message| message.contains("definitely-missing-claude-cli")),
            "expected missing primary binary message, got {:?}",
            snapshot.error_message
        );

        let mut pi = meta.clone();
        pi.name = "Pi".into();
        pi.backend = Some("pi".into());
        pi.agent_source_info.binary_name = Some("pi".into());
        pi.agent_source_info.bridge_binary = Some("npx".into());
        pi.args = vec!["-y".into(), "pi-acp".into()];
        assert_eq!(explicit_probe_args(&pi).unwrap(), ["-y", "pi-acp@0.0.31"]);
    }

    // ---- #675: manual diagnostics run --version for direct CLIs and are
    // never short-circuited by a stale availability verdict ----

    #[cfg(unix)]
    fn write_executable(dir: &std::path::Path, name: &str, contents: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        path.to_string_lossy().to_string()
    }

    #[cfg(unix)]
    fn upsert_builtin_claude_params<'a>(id: &'a str, source_info: &'a str) -> UpsertAgentMetadataParams<'a> {
        UpsertAgentMetadataParams {
            id,
            icon: None,
            name: "Claude Code",
            name_i18n: None,
            description: None,
            description_i18n: None,
            backend: Some("claude"),
            agent_type: "acp",
            agent_source: "builtin",
            agent_source_info: Some(source_info),
            enabled: true,
            command: None,
            args: Some("[]"),
            env: Some("[]"),
            native_skills_dirs: None,
            behavior_policy: None,
            yolo_id: None,
            agent_capabilities: None,
            auth_methods: None,
            config_options: None,
            available_modes: None,
            available_models: None,
            available_commands: None,
            sort_order: 100,
        }
    }

    /// Manual diagnostics on a direct-CLI builtin (claude/codex) must run
    /// `--version` — a corrupted install on PATH is offline with the
    /// classified code, not online-by-PATH (#675).
    #[cfg(unix)]
    #[tokio::test]
    async fn manual_check_flags_corrupted_direct_cli_offline() {
        let db = init_database_memory().await.unwrap();
        let repo: Arc<dyn IAgentMetadataRepository> = Arc::new(SqliteAgentMetadataRepository::new(db.pool().clone()));
        let temp = tempfile::tempdir().unwrap();
        let command = write_executable(
            temp.path(),
            "claude",
            "#!/bin/sh\nprintf 'native binary missing\\n' >&2\nexit 1\n",
        );
        let source_info = serde_json::json!({ "binary_name": command }).to_string();
        repo.upsert(&upsert_builtin_claude_params("agent-corrupted-claude", &source_info))
            .await
            .unwrap();

        let registry = AgentRegistry::new(repo);
        registry.hydrate().await.unwrap();
        let provider_repo: Arc<dyn IProviderRepository> = Arc::new(SqliteProviderRepository::new(db.pool().clone()));
        let service = AgentAvailabilityService::new(registry, provider_repo, test_spawner());

        let row = service
            .run_diagnostic("agent-corrupted-claude", AgentSnapshotCheckKind::Manual)
            .await
            .unwrap();

        assert_eq!(row.status, AgentManagementStatus::Offline);
        assert_eq!(row.last_check_status, Some(AgentSnapshotCheckStatus::Offline));
        assert_eq!(row.last_check_kind, Some(AgentSnapshotCheckKind::Manual));
        assert_eq!(row.last_check_error_code.as_deref(), Some("version_probe_failed"));
        assert!(
            row.last_check_error_message
                .as_deref()
                .is_some_and(|message| message.contains("native binary missing"))
        );
    }

    /// A valid `--version` result is only preflight evidence. The direct CLI
    /// remains offline when the real no-prompt session handshake cannot start.
    #[cfg(unix)]
    #[tokio::test]
    async fn manual_check_requires_session_handshake_after_healthy_version() {
        let db = init_database_memory().await.unwrap();
        let repo: Arc<dyn IAgentMetadataRepository> = Arc::new(SqliteAgentMetadataRepository::new(db.pool().clone()));
        let temp = tempfile::tempdir().unwrap();
        let command = write_executable(temp.path(), "claude", "#!/bin/sh\nprintf 'claude 1.0.0\\n'\n");
        let source_info = serde_json::json!({ "binary_name": command }).to_string();
        repo.upsert(&upsert_builtin_claude_params("agent-healthy-claude", &source_info))
            .await
            .unwrap();

        let registry = AgentRegistry::new(repo);
        registry.hydrate().await.unwrap();
        let provider_repo: Arc<dyn IProviderRepository> = Arc::new(SqliteProviderRepository::new(db.pool().clone()));
        let service = AgentAvailabilityService::new(registry, provider_repo, test_spawner());

        let row = service
            .run_diagnostic("agent-healthy-claude", AgentSnapshotCheckKind::Manual)
            .await
            .unwrap();

        assert_eq!(row.status, AgentManagementStatus::Offline);
        assert_eq!(row.last_check_kind, Some(AgentSnapshotCheckKind::Manual));
        assert_eq!(row.last_check_error_code.as_deref(), Some("process_start_failed"));
    }

    /// Manual diagnostics must reach the real probe even when the binary is
    /// missing entirely: the outcome is a persisted command_not_found manual
    /// snapshot, not a silent early return (#675).
    #[tokio::test]
    async fn manual_check_persists_command_not_found_instead_of_short_circuit() {
        let db = init_database_memory().await.unwrap();
        let repo: Arc<dyn IAgentMetadataRepository> = Arc::new(SqliteAgentMetadataRepository::new(db.pool().clone()));
        let source_info = r#"{"binary_name":"definitely-missing-claude-cli"}"#;
        repo.upsert(&{
            let mut params = UpsertAgentMetadataParams {
                id: "agent-missing-claude",
                icon: None,
                name: "Claude Code",
                name_i18n: None,
                description: None,
                description_i18n: None,
                backend: Some("claude"),
                agent_type: "acp",
                agent_source: "builtin",
                agent_source_info: Some(source_info),
                enabled: true,
                command: None,
                args: Some("[]"),
                env: Some("[]"),
                native_skills_dirs: None,
                behavior_policy: None,
                yolo_id: None,
                agent_capabilities: None,
                auth_methods: None,
                config_options: None,
                available_modes: None,
                available_models: None,
                available_commands: None,
                sort_order: 100,
            };
            params.sort_order = 100;
            params
        })
        .await
        .unwrap();

        let registry = AgentRegistry::new(repo.clone());
        registry.hydrate().await.unwrap();
        let provider_repo: Arc<dyn IProviderRepository> = Arc::new(SqliteProviderRepository::new(db.pool().clone()));
        let service = AgentAvailabilityService::new(registry, provider_repo, test_spawner());

        let row = service
            .run_diagnostic("agent-missing-claude", AgentSnapshotCheckKind::Manual)
            .await
            .unwrap();

        assert_eq!(row.last_check_kind, Some(AgentSnapshotCheckKind::Manual));
        assert_eq!(row.last_check_error_code.as_deref(), Some("command_not_found"));
        let persisted = repo.get("agent-missing-claude").await.unwrap().unwrap();
        assert_eq!(persisted.last_check_error_code.as_deref(), Some("command_not_found"));
    }

    /// A builtin without an explicit spawn command (the non-claude/codex
    /// fallback branch) gets the same PATH + `--version` treatment with
    /// classified errors — no PATH-only side door (#675).
    #[cfg(unix)]
    #[tokio::test]
    async fn commandless_builtin_fallback_probe_runs_version_check() {
        let db = init_database_memory().await.unwrap();
        let repo: Arc<dyn IAgentMetadataRepository> = Arc::new(SqliteAgentMetadataRepository::new(db.pool().clone()));
        let temp = tempfile::tempdir().unwrap();
        let command = write_executable(
            temp.path(),
            "hermes",
            "#!/bin/sh\nprintf 'wrapper broken\\n' >&2\nexit 1\n",
        );
        let registry = AgentRegistry::new(repo);
        registry.hydrate().await.unwrap();
        let provider_repo: Arc<dyn IProviderRepository> = Arc::new(SqliteProviderRepository::new(db.pool().clone()));

        let meta = AgentMetadata {
            id: "agent-fallback-builtin".into(),
            icon: None,
            name: "Fallback Builtin".into(),
            name_i18n: None,
            description: None,
            description_i18n: None,
            backend: Some("hermes".into()),
            agent_type: AgentType::Acp,
            agent_source: AgentSource::Builtin,
            agent_source_info: AgentSourceInfo {
                binary_name: Some(command.clone()),
                ..Default::default()
            },
            enabled: true,
            available: true,
            command: None,
            resolved_command: Some(std::path::PathBuf::from(&command)),
            args: vec![],
            env: vec![],
            native_skills_dirs: None,
            behavior_policy: BehaviorPolicy::default(),
            yolo_id: None,
            sort_order: 0,
            team_capable: false,
            last_check_status: None,
            last_check_kind: None,
            last_check_error_code: None,
            last_check_error_message: None,
            last_check_error_details: None,
            last_check_guidance: None,
            last_check_latency_ms: None,
            last_check_at: None,
            last_success_at: None,
            last_failure_at: None,
            handshake: AgentHandshake::default(),
            has_command_override: false,
            env_override_key_count: 0,
        };

        let snapshot = run_probe(
            &registry,
            &provider_repo,
            &test_spawner(),
            &meta,
            AgentSnapshotCheckKind::Manual,
        )
        .await;
        assert_eq!(snapshot.status, "offline");
        assert_eq!(snapshot.error_code.as_deref(), Some("version_probe_failed"));
        assert!(
            snapshot
                .error_message
                .as_deref()
                .is_some_and(|message| message.contains("wrapper broken"))
        );
    }

    /// A stale offline verdict is replaced by the current deep-probe failure;
    /// passing `--version` alone must never restore a direct agent to online.
    #[cfg(unix)]
    #[tokio::test]
    async fn manual_check_replaces_stale_failure_with_current_session_failure() {
        let db = init_database_memory().await.unwrap();
        let repo: Arc<dyn IAgentMetadataRepository> = Arc::new(SqliteAgentMetadataRepository::new(db.pool().clone()));
        let temp = tempfile::tempdir().unwrap();
        let command = write_executable(temp.path(), "claude", "#!/bin/sh\nprintf 'claude 1.0.0\\n'\n");
        let source_info = serde_json::json!({ "binary_name": command }).to_string();
        repo.upsert(&upsert_builtin_claude_params("agent-restored-claude", &source_info))
            .await
            .unwrap();
        repo.update_availability_snapshot(
            "agent-restored-claude",
            &tjuaeui_db::UpdateAgentAvailabilitySnapshotParams {
                last_check_status: Some("offline"),
                last_check_kind: Some("startup"),
                last_check_error_code: Some("version_probe_timeout"),
                last_check_error_message: Some("version_probe_timeout@5000ms"),
                last_check_guidance: None,
                last_check_latency_ms: Some(5_000),
                last_check_at: Some(1),
                last_success_at: None,
                last_failure_at: Some(1),
            },
        )
        .await
        .unwrap();

        let registry = AgentRegistry::new(repo);
        registry.hydrate().await.unwrap();
        let provider_repo: Arc<dyn IProviderRepository> = Arc::new(SqliteProviderRepository::new(db.pool().clone()));
        let service = AgentAvailabilityService::new(registry, provider_repo, test_spawner());

        let before = service.management_row_by_id("agent-restored-claude").await.unwrap();
        assert_eq!(before.status, AgentManagementStatus::Offline);

        let row = service
            .run_diagnostic("agent-restored-claude", AgentSnapshotCheckKind::Manual)
            .await
            .unwrap();
        assert_eq!(row.status, AgentManagementStatus::Offline);
        assert_eq!(row.last_check_kind, Some(AgentSnapshotCheckKind::Manual));
        assert_eq!(row.last_check_error_code.as_deref(), Some("process_start_failed"));
    }
}
