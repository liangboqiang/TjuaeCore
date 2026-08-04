use std::path::PathBuf;
use std::sync::Arc;

use tjuaeui_ai_agent::AcpSessionSyncService;
use tjuaeui_ai_agent::AcpSkillManager;
use tjuaeui_ai_agent::factory::{AgentFactoryDeps, build_agent_factory};
use tjuaeui_ai_agent::registry::AgentRegistry;
use tjuaeui_ai_agent::session_context::{
    AgentSessionContext, AgentSessionKind, ConversationContext, TjuaeCliSessionBuildContext, WorkspaceContext,
};
use tjuaeui_ai_agent::types::BuildTaskOptions;
use tjuaeui_api_types::TjuaeCliBuildExtra;
use tjuaeui_common::{AgentType, ProviderWithModel, encrypt_string};
use tjuaeui_db::{
    CreateProviderParams, IA2aRepository, IAcpSessionRepository, IProviderRepository, SqliteA2aRepository,
    SqliteAcpSessionRepository, SqliteAgentMetadataRepository, SqliteProviderRepository, init_database_memory,
};
use tjuaeui_realtime::BroadcastEventBus;

fn test_encryption_key() -> [u8; 32] {
    [0xABu8; 32]
}

async fn setup() -> (
    Arc<dyn IProviderRepository>,
    Arc<AgentRegistry>,
    Arc<AcpSessionSyncService>,
    Arc<dyn IA2aRepository>,
) {
    let db = init_database_memory().await.unwrap();
    let pool = db.pool().clone();
    let provider_repo: Arc<dyn IProviderRepository> = Arc::new(SqliteProviderRepository::new(pool.clone()));
    let metadata_repo = Arc::new(SqliteAgentMetadataRepository::new(pool.clone()));
    let registry = AgentRegistry::new(metadata_repo);
    registry.hydrate().await.unwrap();
    let session_repo: Arc<dyn IAcpSessionRepository> = Arc::new(SqliteAcpSessionRepository::new(pool.clone()));
    let acp_agent_service = AcpSessionSyncService::new(session_repo);
    let a2a_repo: Arc<dyn IA2aRepository> = Arc::new(SqliteA2aRepository::new(pool));
    (provider_repo, registry, acp_agent_service, a2a_repo)
}

async fn insert_test_provider(repo: &dyn IProviderRepository, id: &str, platform: &str) {
    let key = test_encryption_key();
    let encrypted_api_key = encrypt_string("sk-test-key-12345", &key).unwrap();
    repo.create(CreateProviderParams {
        id: Some(id),
        platform,
        name: "Test Provider",
        base_url: "https://api.example.com/v1",
        api_key_encrypted: &encrypted_api_key,
        models: r#"["gpt-4o","gpt-5.4"]"#,
        enabled: true,
        capabilities: "[]",
        context_limit: None,
        model_protocols: None,
        model_enabled: None,
        model_health: None,
        model_settings: "{}",
        bedrock_config: None,
        is_full_url: false,
    })
    .await
    .unwrap();
}

fn make_factory(
    provider_repo: Arc<dyn IProviderRepository>,
    agent_registry: Arc<AgentRegistry>,
    acp_agent_service: Arc<AcpSessionSyncService>,
    a2a_repo: Arc<dyn IA2aRepository>,
) -> tjuaeui_ai_agent::task_manager::AgentFactory {
    let tmp = tempfile::TempDir::new().unwrap();
    let skill_paths = Arc::new(tjuaeui_asset::resolve_skill_paths(tmp.path(), tmp.path()));
    // These provider integration tests only build tjuae_cli tasks, which never touch
    // the session spawner. It just has to be constructable (the field is no longer
    // optional now that claude/codex always use the direct-CLI session path).
    let process_registry = Arc::new(tjuaeui_process::FileRegistryStore::new(tmp.path()));
    let session_spawner: Arc<dyn tjuaeui_process::Spawner> = Arc::new(tjuaeui_process::RealSpawner::new(
        process_registry,
        uuid::Uuid::now_v7(),
        tjuaeui_process::local_machine_id(tmp.path()),
    ));
    build_agent_factory(AgentFactoryDeps {
        skill_manager: AcpSkillManager::new(skill_paths),
        provider_repo,
        a2a_repo,
        encryption_key: test_encryption_key(),
        agent_registry,
        acp_agent_service,
        data_dir: PathBuf::from("/tmp/tjuaecli-test"),
        dump_prompts: false,
        broadcaster: Arc::new(BroadcastEventBus::new(16)),
        backend_binary_path: Arc::new(PathBuf::from("/tmp/tjuaecli-test/tjuaecore")),
        mcp_server_repo: None,
        runtime_asset_configuration_resolver: None,
        session_spawner,
    })
}

fn make_tjuae_cli_options(
    conversation_id: &str,
    workspace: &str,
    model: ProviderWithModel,
    config: TjuaeCliBuildExtra,
) -> BuildTaskOptions {
    BuildTaskOptions::new(AgentSessionContext {
        conversation: ConversationContext {
            conversation_id: conversation_id.to_owned(),
            user_id: "user-1".to_owned(),
            agent_type: AgentType::TjuaeCli,
            source: None,
        },
        workspace: WorkspaceContext {
            path: workspace.to_owned(),
            stored_path: workspace.to_owned(),
            is_custom: !workspace.is_empty(),
        },
        model,
        skills: vec![],
        skill_roots: vec![],
        runtime_env: vec![],
        team: None,
        kind: AgentSessionKind::TjuaeCli(Box::new(TjuaeCliSessionBuildContext {
            config,
            team: None,
            belongs_to_team: false,
        })),
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tjuae_cli_factory_returns_error_for_missing_provider() {
    let (provider_repo, agent_registry, acp_agent_service, a2a_repo) = setup().await;
    let factory = make_factory(provider_repo, agent_registry, acp_agent_service, a2a_repo);

    let options = make_tjuae_cli_options(
        "conv-test-1",
        "",
        ProviderWithModel {
            provider_id: "nonexistent-provider".into(),
            model: "gpt-4o".into(),
            use_model: None,
        },
        TjuaeCliBuildExtra::default(),
    );

    let result = factory(options).await;
    match result {
        Ok(_) => panic!("缺少提供商时应返回错误，但实际成功"),
        Err(e) => {
            let err_msg = e.to_string();
            assert!(err_msg.contains("找不到"), "期望未找到错误，实际为：{err_msg}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tjuae_cli_factory_resolves_provider_from_db() {
    let (provider_repo, agent_registry, acp_agent_service, a2a_repo) = setup().await;
    insert_test_provider(&*provider_repo, "prov-001", "openai").await;
    let factory = make_factory(provider_repo, agent_registry, acp_agent_service, a2a_repo);

    let options = make_tjuae_cli_options(
        "conv-test-2",
        "/tmp/test-workspace",
        ProviderWithModel {
            provider_id: "prov-001".into(),
            model: "gpt-4o".into(),
            use_model: None,
        },
        TjuaeCliBuildExtra::default(),
    );

    let result = factory(options).await;
    assert!(result.is_ok(), "Expected Ok, got: {:?}", result.err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tjuae_cli_factory_respects_use_model_override() {
    let (provider_repo, agent_registry, acp_agent_service, a2a_repo) = setup().await;
    insert_test_provider(&*provider_repo, "prov-002", "openai").await;
    let factory = make_factory(provider_repo, agent_registry, acp_agent_service, a2a_repo);

    let options = make_tjuae_cli_options(
        "conv-test-3",
        "/tmp/test-workspace",
        ProviderWithModel {
            provider_id: "prov-002".into(),
            model: "gpt-4o".into(),
            use_model: Some("gpt-5.4".into()),
        },
        TjuaeCliBuildExtra::default(),
    );

    let result = factory(options).await;
    assert!(result.is_ok(), "Expected Ok, got: {:?}", result.err());
}
