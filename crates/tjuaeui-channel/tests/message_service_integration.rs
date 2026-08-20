use std::sync::{Arc, Mutex};

mod support;

use async_trait::async_trait;
use tjuaeui_ai_agent::agent_task::{AgentInstance, IAgentTask};
use tjuaeui_ai_agent::protocol::events::FinishEventData;
use tjuaeui_ai_agent::types::{BuildTaskOptions, SendMessageData};
use tjuaeui_ai_agent::{AgentError, AgentSendError, AgentStreamEvent, IMockAgent, IWorkerTaskManager};
use tjuaeui_api_types::WebSocketMessage;
use tjuaeui_channel::channel_settings::{ChannelAssistantCatalogEntry, ChannelSettingsService};
use tjuaeui_channel::error::ChannelError;
use tjuaeui_channel::message_service::ChannelMessageService;
use tjuaeui_channel::types::PluginType;
use tjuaeui_common::{AgentKillReason, AgentType, ConversationStatus, TimestampMs};
use tjuaeui_conversation::skill_resolver::{ResolvedAgentSkill, SkillResolver};
use tjuaeui_conversation::{
    AssistantRuntimeCatalogPort, AssistantRuntimePreferenceUpdate, AssistantRuntimeProfile, ConversationService,
};
use tjuaeui_db::models::AssistantSessionRow;
use tjuaeui_db::{
    IAcpSessionRepository, IClientPreferenceRepository, IConversationRepository, SqliteAcpSessionRepository,
    SqliteAgentMetadataRepository, SqliteClientPreferenceRepository, SqliteConversationRepository,
    init_database_memory,
};
use tjuaeui_realtime::EventBroadcaster;
use tokio::sync::broadcast;

use support::StaticChannelAssistantCatalog;

struct TestBroadcaster {
    events: Mutex<Vec<WebSocketMessage<serde_json::Value>>>,
}

impl TestBroadcaster {
    fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }
}

impl EventBroadcaster for TestBroadcaster {
    fn broadcast(&self, event: WebSocketMessage<serde_json::Value>) {
        self.events.lock().unwrap().push(event);
    }
}

struct NoopSkillResolver;

#[async_trait]
impl SkillResolver for NoopSkillResolver {
    async fn resolve_skills(&self, _names: &[String]) -> Vec<ResolvedAgentSkill> {
        Vec::new()
    }

    async fn link_workspace_skills(
        &self,
        _workspace: &std::path::Path,
        _rel_dirs: &[&str],
        _skills: &[ResolvedAgentSkill],
    ) -> usize {
        0
    }
}

struct ScriptedAgent {
    conversation_id: String,
    event_tx: broadcast::Sender<AgentStreamEvent>,
}

impl ScriptedAgent {
    fn new(conversation_id: &str) -> Self {
        let (event_tx, _) = broadcast::channel(16);
        Self {
            conversation_id: conversation_id.to_owned(),
            event_tx,
        }
    }
}

#[async_trait]
impl IAgentTask for ScriptedAgent {
    fn agent_type(&self) -> AgentType {
        AgentType::TjuaeCli
    }

    fn conversation_id(&self) -> &str {
        &self.conversation_id
    }

    fn workspace(&self) -> &str {
        "/tmp/tjuaeui-channel-test"
    }

    fn status(&self) -> Option<ConversationStatus> {
        Some(ConversationStatus::Finished)
    }

    fn last_activity_at(&self) -> TimestampMs {
        0
    }

    fn subscribe(&self) -> broadcast::Receiver<AgentStreamEvent> {
        self.event_tx.subscribe()
    }

    async fn send_message(&self, _data: SendMessageData) -> Result<(), AgentSendError> {
        let _ = self.event_tx.send(AgentStreamEvent::Finish(FinishEventData::default()));
        Ok(())
    }

    async fn cancel(&self) -> Result<(), AgentError> {
        Ok(())
    }

    fn kill(&self, _reason: Option<AgentKillReason>) -> Result<(), AgentError> {
        Ok(())
    }
}

impl IMockAgent for ScriptedAgent {}

struct RecordingTaskManager {
    agents: Mutex<std::collections::HashMap<String, AgentInstance>>,
}

impl RecordingTaskManager {
    fn new() -> Self {
        Self {
            agents: Mutex::new(std::collections::HashMap::new()),
        }
    }
}

#[async_trait]
impl IWorkerTaskManager for RecordingTaskManager {
    fn get_task(&self, conversation_id: &str) -> Option<AgentInstance> {
        self.agents.lock().unwrap().get(conversation_id).cloned()
    }

    async fn get_or_build_task(
        &self,
        conversation_id: &str,
        _options: BuildTaskOptions,
    ) -> Result<AgentInstance, AgentError> {
        let mut agents = self.agents.lock().unwrap();
        if let Some(agent) = agents.get(conversation_id) {
            return Ok(agent.clone());
        }

        let agent = AgentInstance::Mock(Arc::new(ScriptedAgent::new(conversation_id)));
        agents.insert(conversation_id.to_owned(), agent.clone());
        Ok(agent)
    }

    fn kill(&self, conversation_id: &str, _reason: Option<AgentKillReason>) -> Result<(), AgentError> {
        self.agents.lock().unwrap().remove(conversation_id);
        Ok(())
    }

    fn kill_and_wait(
        &self,
        conversation_id: &str,
        reason: Option<AgentKillReason>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        let _ = self.kill(conversation_id, reason);
        Box::pin(std::future::ready(()))
    }

    async fn clear(&self) {
        self.agents.lock().unwrap().clear();
    }

    fn active_count(&self) -> usize {
        self.agents.lock().unwrap().len()
    }

    fn collect_idle(&self, _idle_threshold_ms: TimestampMs) -> Vec<String> {
        Vec::new()
    }
}

struct StaticConversationAssistantCatalog {
    profiles: Vec<AssistantRuntimeProfile>,
}

#[async_trait]
impl AssistantRuntimeCatalogPort for StaticConversationAssistantCatalog {
    async fn resolve_enabled(
        &self,
        assistant_id: &str,
        _locale: Option<&str>,
    ) -> Result<Option<AssistantRuntimeProfile>, String> {
        Ok(self.profiles.iter().find(|profile| profile.id == assistant_id).cloned())
    }

    async fn update_runtime_preferences(
        &self,
        _assistant_id: &str,
        _updates: AssistantRuntimePreferenceUpdate<'_>,
    ) -> Result<(), String> {
        Ok(())
    }
}

fn assistant_profile(assistant_id: &str, name: &str, agent_id: &str) -> AssistantRuntimeProfile {
    AssistantRuntimeProfile {
        id: assistant_id.to_owned(),
        source: "mine".to_owned(),
        name: name.to_owned(),
        avatar: "🤖".to_owned(),
        agent_id: agent_id.to_owned(),
        rules: String::new(),
        model_mode: "auto".to_owned(),
        model: None,
        permission_mode: "auto".to_owned(),
        permission: None,
        thought_level_mode: "auto".to_owned(),
        thought_level: None,
        skill_ids: Vec::new(),
        mcp_ids: Vec::new(),
    }
}

fn channel_catalog_entry(assistant_id: &str, name: &str, agent_id: &str) -> ChannelAssistantCatalogEntry {
    let (agent_type, backend) = if agent_id == "tjuaecli" {
        ("tjuaecli".to_owned(), None)
    } else {
        ("acp".to_owned(), Some(agent_id.to_owned()))
    };
    ChannelAssistantCatalogEntry {
        assistant_id: assistant_id.to_owned(),
        name: name.to_owned(),
        agent_type,
        backend,
    }
}

fn attach_assistant_catalog(service: &ConversationService, profiles: Vec<AssistantRuntimeProfile>) {
    service.with_assistant_runtime_catalog(Arc::new(StaticConversationAssistantCatalog { profiles }));
}

#[tokio::test]
async fn send_to_agent_warms_cold_task_before_returning_stream_subscription() {
    let db = init_database_memory().await.unwrap();
    let pool = db.pool().clone();

    let task_manager: Arc<dyn IWorkerTaskManager> = Arc::new(RecordingTaskManager::new());
    let conversation_svc = Arc::new(ConversationService::new(
        std::env::temp_dir(),
        Arc::new(TestBroadcaster::new()),
        Arc::new(NoopSkillResolver),
        Arc::clone(&task_manager),
        Arc::new(SqliteConversationRepository::new(pool.clone())),
        Arc::new(SqliteAgentMetadataRepository::new(pool.clone())),
        Arc::new(SqliteAcpSessionRepository::new(pool.clone())),
    ));

    let settings = Arc::new(ChannelSettingsService::new(
        Arc::new(SqliteClientPreferenceRepository::new(pool)),
        Arc::new(StaticChannelAssistantCatalog::empty()),
    ));
    let message_svc = ChannelMessageService::new(
        conversation_svc,
        Arc::clone(&task_manager),
        settings,
        "system_default_user".to_owned(),
    );

    let session = AssistantSessionRow {
        id: "session-1".to_owned(),
        user_id: "channel-user-1".to_owned(),
        agent_type: "tjuaecli".to_owned(),
        conversation_id: None,
        workspace: None,
        chat_id: Some("7088048016".to_owned()),
        created_at: 1,
        last_activity: 1,
    };

    for platform in [
        PluginType::Telegram,
        PluginType::Lark,
        PluginType::Dingtalk,
        PluginType::Weixin,
    ] {
        let result = message_svc.send_to_agent(&session, "hello", platform).await.unwrap();

        assert!(
            result.stream_rx.is_some(),
            "channel relay must have an agent stream receiver after cold start for {platform:?}"
        );
        assert!(task_manager.get_task(&result.conversation_id).is_some());
    }
}

#[tokio::test]
async fn send_to_agent_persists_assistant_snapshot_for_channel_bound_assistant() {
    let db = init_database_memory().await.unwrap();
    let pool = db.pool().clone();

    let task_manager: Arc<dyn IWorkerTaskManager> = Arc::new(RecordingTaskManager::new());
    let conversation_repo = Arc::new(SqliteConversationRepository::new(pool.clone()));
    let conversation_repo_trait: Arc<dyn IConversationRepository> = conversation_repo.clone();
    let acp_session_repo = Arc::new(SqliteAcpSessionRepository::new(pool.clone()));
    let conversation_svc = Arc::new(ConversationService::new(
        std::env::temp_dir(),
        Arc::new(TestBroadcaster::new()),
        Arc::new(NoopSkillResolver),
        Arc::clone(&task_manager),
        conversation_repo_trait,
        Arc::new(SqliteAgentMetadataRepository::new(pool.clone())),
        acp_session_repo.clone(),
    ));
    attach_assistant_catalog(
        conversation_svc.as_ref(),
        vec![assistant_profile("bare-claude", "Claude", "claude")],
    );

    let pref_repo = Arc::new(SqliteClientPreferenceRepository::new(pool.clone()));
    pref_repo
        .upsert_batch(&[(
            "assistant.telegram.agent",
            r#"{"assistant_id":"bare-claude","name":"Claude"}"#,
        )])
        .await
        .unwrap();

    let settings = Arc::new(ChannelSettingsService::new(
        pref_repo,
        Arc::new(StaticChannelAssistantCatalog::new(vec![channel_catalog_entry(
            "bare-claude",
            "Claude",
            "claude",
        )])),
    ));
    let message_svc = ChannelMessageService::new(
        conversation_svc,
        Arc::clone(&task_manager),
        settings,
        "system_default_user".to_owned(),
    );

    let session = AssistantSessionRow {
        id: "session-assisted".to_owned(),
        user_id: "channel-user-1".to_owned(),
        agent_type: "tjuaecli".to_owned(),
        conversation_id: None,
        workspace: None,
        chat_id: Some("7088048016".to_owned()),
        created_at: 1,
        last_activity: 1,
    };

    let result = message_svc
        .send_to_agent(&session, "hello", PluginType::Telegram)
        .await
        .unwrap();

    let snapshot = conversation_repo
        .get_assistant_snapshot(&result.conversation_id)
        .await
        .unwrap();
    assert!(
        snapshot.is_some(),
        "channel-created conversation should persist an assistant snapshot when the platform is bound to an assistant"
    );
    let snapshot = snapshot.unwrap();
    let conversation = conversation_repo.get(&result.conversation_id).await.unwrap().unwrap();
    assert_eq!(conversation.r#type, AgentType::Acp.serde_name());
    let session_row = acp_session_repo
        .get(&result.conversation_id)
        .await
        .unwrap()
        .expect("acp_session row should exist for ACP assistant conversations");
    assert_eq!(session_row.agent_id, "2d23ff1c");
    assert_eq!(snapshot.assistant_id, "bare-claude");
    assert_eq!(snapshot.agent_id, "2d23ff1c");
    assert_eq!(conversation.name, "Claude");
}

#[tokio::test]
async fn send_to_agent_rejects_unresolvable_channel_assistant_binding() {
    let db = init_database_memory().await.unwrap();
    let pool = db.pool().clone();

    let task_manager: Arc<dyn IWorkerTaskManager> = Arc::new(RecordingTaskManager::new());
    let conversation_repo = Arc::new(SqliteConversationRepository::new(pool.clone()));
    let conversation_repo_trait: Arc<dyn IConversationRepository> = conversation_repo.clone();
    let acp_session_repo = Arc::new(SqliteAcpSessionRepository::new(pool.clone()));
    let conversation_svc = Arc::new(ConversationService::new(
        std::env::temp_dir(),
        Arc::new(TestBroadcaster::new()),
        Arc::new(NoopSkillResolver),
        Arc::clone(&task_manager),
        conversation_repo_trait,
        Arc::new(SqliteAgentMetadataRepository::new(pool.clone())),
        acp_session_repo,
    ));

    let pref_repo = Arc::new(SqliteClientPreferenceRepository::new(pool.clone()));
    pref_repo
        .upsert_batch(&[(
            "assistant.telegram.agent",
            r#"{"assistant_id":"missing-assistant","name":"Missing"}"#,
        )])
        .await
        .unwrap();
    let settings = Arc::new(ChannelSettingsService::new(
        pref_repo,
        Arc::new(StaticChannelAssistantCatalog::empty()),
    ));
    let message_svc = ChannelMessageService::new(
        conversation_svc,
        Arc::clone(&task_manager),
        settings,
        "system_default_user".to_owned(),
    );

    let session = AssistantSessionRow {
        id: "session-assisted-missing".to_owned(),
        user_id: "channel-user-missing".to_owned(),
        agent_type: "tjuaecli".to_owned(),
        conversation_id: None,
        workspace: None,
        chat_id: Some("7088048017".to_owned()),
        created_at: 1,
        last_activity: 1,
    };

    let err = message_svc
        .send_to_agent(&session, "hello", PluginType::Telegram)
        .await
        .unwrap_err();
    assert!(matches!(err, ChannelError::MessageSendFailed(_)));
    assert!(
        err.to_string().contains("missing-assistant"),
        "error should surface the unresolved assistant identity"
    );
}

#[tokio::test]
async fn send_to_agent_without_saved_binding_defaults_to_bare_tjuae_cli_assistant() {
    let db = init_database_memory().await.unwrap();
    let pool = db.pool().clone();

    let task_manager: Arc<dyn IWorkerTaskManager> = Arc::new(RecordingTaskManager::new());
    let conversation_repo = Arc::new(SqliteConversationRepository::new(pool.clone()));
    let conversation_repo_trait: Arc<dyn IConversationRepository> = conversation_repo.clone();
    let acp_session_repo = Arc::new(SqliteAcpSessionRepository::new(pool.clone()));
    let conversation_svc = Arc::new(ConversationService::new(
        std::env::temp_dir(),
        Arc::new(TestBroadcaster::new()),
        Arc::new(NoopSkillResolver),
        Arc::clone(&task_manager),
        conversation_repo_trait,
        Arc::new(SqliteAgentMetadataRepository::new(pool.clone())),
        acp_session_repo,
    ));
    attach_assistant_catalog(
        conversation_svc.as_ref(),
        vec![assistant_profile("bare-tjuaecli", "TjuaeCLI", "tjuaecli")],
    );

    let pref_repo = Arc::new(SqliteClientPreferenceRepository::new(pool.clone()));

    let settings = Arc::new(ChannelSettingsService::new(
        pref_repo,
        Arc::new(StaticChannelAssistantCatalog::new(vec![channel_catalog_entry(
            "bare-tjuaecli",
            "TjuaeCLI",
            "tjuaecli",
        )])),
    ));
    let message_svc = ChannelMessageService::new(
        conversation_svc,
        Arc::clone(&task_manager),
        settings,
        "system_default_user".to_owned(),
    );

    let session = AssistantSessionRow {
        id: "session-assisted-default-tjuaecli".to_owned(),
        user_id: "channel-user-default".to_owned(),
        agent_type: "tjuaecli".to_owned(),
        conversation_id: None,
        workspace: None,
        chat_id: Some("7088048018".to_owned()),
        created_at: 1,
        last_activity: 1,
    };

    let result = message_svc
        .send_to_agent(&session, "hello", PluginType::Telegram)
        .await
        .unwrap();

    let snapshot = conversation_repo
        .get_assistant_snapshot(&result.conversation_id)
        .await
        .unwrap()
        .expect("channel-created conversation should default to a bare assistant snapshot");
    let conversation = conversation_repo.get(&result.conversation_id).await.unwrap().unwrap();

    assert_eq!(snapshot.assistant_id, "bare-tjuaecli");
    assert_eq!(snapshot.agent_id, "632f31d2");
    assert_eq!(conversation.r#type, AgentType::TjuaeCli.serde_name());
    assert_eq!(conversation.name, "tg-tjuaecli-70880480");
}

#[tokio::test]
async fn send_to_agent_without_assistant_name_uses_channel_fallback_name() {
    let db = init_database_memory().await.unwrap();
    let pool = db.pool().clone();

    let task_manager: Arc<dyn IWorkerTaskManager> = Arc::new(RecordingTaskManager::new());
    let conversation_repo = Arc::new(SqliteConversationRepository::new(pool.clone()));
    let conversation_repo_trait: Arc<dyn IConversationRepository> = conversation_repo.clone();
    let acp_session_repo = Arc::new(SqliteAcpSessionRepository::new(pool.clone()));
    let conversation_svc = Arc::new(ConversationService::new(
        std::env::temp_dir(),
        Arc::new(TestBroadcaster::new()),
        Arc::new(NoopSkillResolver),
        Arc::clone(&task_manager),
        conversation_repo_trait,
        Arc::new(SqliteAgentMetadataRepository::new(pool.clone())),
        acp_session_repo,
    ));
    attach_assistant_catalog(
        conversation_svc.as_ref(),
        vec![assistant_profile("bare-codex", "bare-codex", "codex")],
    );

    let pref_repo = Arc::new(SqliteClientPreferenceRepository::new(pool.clone()));
    pref_repo
        .upsert_batch(&[("assistant.telegram.agent", r#"{"assistant_id":"bare-codex"}"#)])
        .await
        .unwrap();

    let settings = Arc::new(ChannelSettingsService::new(
        pref_repo,
        Arc::new(StaticChannelAssistantCatalog::new(vec![channel_catalog_entry(
            "bare-codex",
            "bare-codex",
            "codex",
        )])),
    ));
    let message_svc = ChannelMessageService::new(
        conversation_svc,
        Arc::clone(&task_manager),
        settings,
        "system_default_user".to_owned(),
    );

    let session = AssistantSessionRow {
        id: "session-assisted-fallback-name".to_owned(),
        user_id: "channel-user-2".to_owned(),
        agent_type: "tjuaecli".to_owned(),
        conversation_id: None,
        workspace: None,
        chat_id: Some("7088048016".to_owned()),
        created_at: 1,
        last_activity: 1,
    };

    let result = message_svc
        .send_to_agent(&session, "hello", PluginType::Telegram)
        .await
        .unwrap();

    let conversation = conversation_repo.get(&result.conversation_id).await.unwrap().unwrap();
    assert_eq!(conversation.name, "tg-acp-codex-70880480");
}
