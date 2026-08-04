//! Agent 主动诊断的批量编排与进度广播。

use std::collections::HashSet;
use std::sync::Arc;

use futures_util::{StreamExt, stream};
use tjuaeui_api_types::{
    AgentDiagnosticRun, AgentDiagnosticRunState, AgentDiagnosticsChangedPayload, AgentManagementRow,
    AgentManagementStatus, AgentSnapshotCheckKind, StartAgentDiagnosticsRequest, WebSocketMessage,
};
use tjuaeui_common::{AgentType, now_ms};
use tjuaeui_realtime::EventBroadcaster;
use tokio::sync::RwLock;

use super::a2a::A2aAgentService;
use super::availability::AgentAvailabilityService;
use crate::error::AgentError;
use crate::registry::AgentRegistry;

const DIAGNOSTIC_CONCURRENCY: usize = 2;

pub struct AgentDiagnosticsService {
    registry: Arc<AgentRegistry>,
    availability: AgentAvailabilityService,
    a2a: Arc<A2aAgentService>,
    broadcaster: Arc<dyn EventBroadcaster>,
    current_run: RwLock<Option<AgentDiagnosticRun>>,
}

impl AgentDiagnosticsService {
    pub fn new(
        registry: Arc<AgentRegistry>,
        availability: AgentAvailabilityService,
        a2a: Arc<A2aAgentService>,
        broadcaster: Arc<dyn EventBroadcaster>,
    ) -> Arc<Self> {
        Arc::new(Self {
            registry,
            availability,
            a2a,
            broadcaster,
            current_run: RwLock::new(None),
        })
    }

    pub async fn diagnose_one(&self, id: &str) -> Result<AgentManagementRow, AgentError> {
        let agent = self
            .registry
            .list_all_including_hidden()
            .await
            .into_iter()
            .find(|agent| agent.id == id)
            .ok_or_else(|| AgentError::not_found(format!("找不到 Agent“{id}”")))?;
        if agent.agent_type == AgentType::A2a {
            let _ = self.a2a.refresh_with_kind(id, AgentSnapshotCheckKind::Manual).await;
            return self
                .registry
                .management_row_by_id(id)
                .await
                .ok_or_else(|| AgentError::not_found(format!("找不到 Agent“{id}”")));
        }
        self.availability
            .run_diagnostic(id, AgentSnapshotCheckKind::Manual)
            .await
    }

    pub async fn current_run(&self) -> Option<AgentDiagnosticRun> {
        self.current_run.read().await.clone()
    }

    /// 启动后台批量诊断。同一时刻只允许一个任务；重复调用返回正在运行的任务。
    pub async fn start(
        self: &Arc<Self>,
        request: StartAgentDiagnosticsRequest,
    ) -> Result<AgentDiagnosticRun, AgentError> {
        let mut guard = self.current_run.write().await;
        if let Some(run) = guard.as_ref()
            && run.state == AgentDiagnosticRunState::Running
        {
            return Ok(run.clone());
        }

        // Historical runtime kinds stay readable for old conversations but
        // cannot start new sessions, so they do not belong in startup/manual
        // "test all" progress or summary counts.
        let rows: Vec<_> = self
            .registry
            .list_all_including_hidden()
            .await
            .into_iter()
            .filter(|row| matches!(row.agent_type, AgentType::Acp | AgentType::A2a | AgentType::TjuaeCli))
            .collect();
        let ids: Vec<String> = match request.agent_ids {
            Some(requested) => {
                let known: HashSet<&str> = rows.iter().map(|row| row.id.as_str()).collect();
                if let Some(missing) = requested.iter().find(|id| !known.contains(id.as_str())) {
                    return Err(AgentError::not_found(format!("找不到 Agent“{missing}”")));
                }
                let mut seen = HashSet::new();
                requested.into_iter().filter(|id| seen.insert(id.clone())).collect()
            }
            None => rows.into_iter().map(|row| row.id).collect(),
        };
        let started_at = now_ms();
        let run = AgentDiagnosticRun {
            run_id: uuid::Uuid::now_v7().to_string(),
            trigger: request.trigger,
            state: AgentDiagnosticRunState::Running,
            total: ids.len(),
            completed: 0,
            online: 0,
            needs_attention: 0,
            missing: 0,
            started_at,
            finished_at: None,
        };
        *guard = Some(run.clone());
        drop(guard);
        self.broadcast(run.clone(), None);

        let service = self.clone();
        let run_id = run.run_id.clone();
        tokio::spawn(async move {
            service.execute(run_id, request.trigger, ids).await;
        });
        Ok(run)
    }

    async fn execute(self: Arc<Self>, run_id: String, trigger: AgentSnapshotCheckKind, ids: Vec<String>) {
        let service = self.clone();
        let mut results = stream::iter(ids.into_iter().map(move |agent_id| {
            let service = service.clone();
            async move {
                let result = if service
                    .registry
                    .list_all_including_hidden()
                    .await
                    .into_iter()
                    .any(|agent| agent.id == agent_id && agent.agent_type == AgentType::A2a)
                {
                    let _ = service.a2a.refresh_with_kind(&agent_id, trigger).await;
                    service
                        .registry
                        .management_row_by_id(&agent_id)
                        .await
                        .ok_or_else(|| AgentError::not_found(format!("找不到 Agent“{agent_id}”")))
                } else {
                    service.availability.run_diagnostic(&agent_id, trigger).await
                };
                (agent_id, result)
            }
        }))
        .buffer_unordered(DIAGNOSTIC_CONCURRENCY);

        while let Some((agent_id, result)) = results.next().await {
            let mut guard = self.current_run.write().await;
            let Some(run) = guard.as_mut().filter(|run| run.run_id == run_id) else {
                return;
            };
            run.completed += 1;
            let agent = match result {
                Ok(row) => {
                    match row.status {
                        AgentManagementStatus::Online if row.last_check_error_code.is_none() => run.online += 1,
                        AgentManagementStatus::Missing => run.missing += 1,
                        _ => run.needs_attention += 1,
                    }
                    Some(row)
                }
                Err(error) => {
                    run.needs_attention += 1;
                    tracing::warn!(agent_id, %error, "Agent 批量诊断失败");
                    None
                }
            };
            let snapshot = run.clone();
            drop(guard);
            self.broadcast(snapshot, agent);
        }

        let mut guard = self.current_run.write().await;
        let Some(run) = guard.as_mut().filter(|run| run.run_id == run_id) else {
            return;
        };
        run.state = AgentDiagnosticRunState::Completed;
        run.finished_at = Some(now_ms());
        let snapshot = run.clone();
        drop(guard);
        self.broadcast(snapshot, None);
    }

    fn broadcast(&self, run: AgentDiagnosticRun, agent: Option<AgentManagementRow>) {
        let payload = AgentDiagnosticsChangedPayload { run, agent };
        match serde_json::to_value(payload) {
            Ok(payload) => self
                .broadcaster
                .broadcast(WebSocketMessage::new("engine.diagnosticsChanged", payload)),
            Err(error) => tracing::error!(%error, "序列化 Agent 诊断进度失败"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tjuaeui_api_types::{
        AgentDiagnosticRunState, AgentSnapshotCheckKind, StartAgentDiagnosticsRequest, WebSocketMessage,
    };
    use tjuaeui_common::CommandSpec;
    use tjuaeui_db::{
        CreateProviderParams, IProviderRepository, SqliteA2aRepository, SqliteAgentMetadataRepository,
        SqliteProviderRepository, init_database_memory,
    };
    use tjuaeui_process::{ManagedProcess, ProcessError, Spawner};
    use tjuaeui_realtime::EventBroadcaster;

    use super::AgentDiagnosticsService;
    use crate::registry::AgentRegistry;
    use crate::services::a2a::A2aAgentService;
    use crate::services::availability::AgentAvailabilityService;

    struct FailingSpawner;

    #[async_trait::async_trait]
    impl Spawner for FailingSpawner {
        async fn spawn(
            &self,
            _spec: CommandSpec,
            _extra_env: &[(String, String)],
            _opaque_owner_tag: &str,
        ) -> Result<Arc<ManagedProcess>, ProcessError> {
            Err(ProcessError::internal("batch diagnostic test spawner"))
        }
    }

    #[derive(Default)]
    struct RecordingBroadcaster {
        events: Mutex<Vec<WebSocketMessage<serde_json::Value>>>,
    }

    impl EventBroadcaster for RecordingBroadcaster {
        fn broadcast(&self, event: WebSocketMessage<serde_json::Value>) {
            self.events.lock().unwrap().push(event);
        }
    }

    #[tokio::test]
    async fn batch_deduplicates_ids_tracks_progress_and_broadcasts_completion() {
        let db = init_database_memory().await.unwrap();
        let registry = AgentRegistry::new(Arc::new(SqliteAgentMetadataRepository::new(db.pool().clone())));
        registry.hydrate().await.unwrap();
        let tjuae_cli_id = registry
            .list_all_including_hidden()
            .await
            .into_iter()
            .find(|agent| agent.agent_type == tjuaeui_common::AgentType::TjuaeCli)
            .unwrap()
            .id;

        let provider_repo: Arc<dyn IProviderRepository> = Arc::new(SqliteProviderRepository::new(db.pool().clone()));
        provider_repo
            .create(CreateProviderParams {
                id: Some("diagnostic-provider"),
                platform: "openai",
                name: "Diagnostic Provider",
                base_url: "https://api.example.com",
                api_key_encrypted: "encrypted",
                models: r#"["gpt-test"]"#,
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

        let availability = AgentAvailabilityService::new(registry.clone(), provider_repo, Arc::new(FailingSpawner));
        let broadcaster = Arc::new(RecordingBroadcaster::default());
        let a2a = Arc::new(A2aAgentService::new(
            Arc::new(SqliteA2aRepository::new(db.pool().clone())),
            registry.clone(),
            [0; 32],
        ));
        let service = AgentDiagnosticsService::new(registry, availability, a2a, broadcaster.clone());
        let started = service
            .start(StartAgentDiagnosticsRequest {
                agent_ids: Some(vec![tjuae_cli_id.clone(), tjuae_cli_id]),
                trigger: AgentSnapshotCheckKind::Startup,
            })
            .await
            .unwrap();

        assert_eq!(started.total, 1, "duplicate requested ids must be diagnosed once");

        let completed = loop {
            let snapshot = service.current_run().await.unwrap();
            if snapshot.state == AgentDiagnosticRunState::Completed {
                break snapshot;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };
        assert_eq!(completed.completed, 1);
        assert_eq!(completed.online, 1);
        assert_eq!(completed.needs_attention, 0);
        assert_eq!(completed.missing, 0);

        let events = broadcaster.events.lock().unwrap();
        assert!(
            events.len() >= 3,
            "start, item progress, and completion must be broadcast"
        );
        assert!(events.iter().all(|event| event.name == "engine.diagnosticsChanged"));
        assert!(
            events.iter().any(|event| event.data["agent"].is_object()),
            "item progress must carry the refreshed Agent row so clients can update caches without refetching"
        );
        assert_eq!(
            events.last().unwrap().data["run"]["state"],
            serde_json::Value::String("completed".to_owned())
        );
        assert!(
            events.last().unwrap().data.get("agent").is_none(),
            "completion is a run-level event and must not carry a stale Agent row"
        );
    }
}
