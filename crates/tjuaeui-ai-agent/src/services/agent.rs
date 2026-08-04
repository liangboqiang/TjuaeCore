//! Business-logic layer for the ai-agent crate.
//!
//! Per `AGENTS.md` "Domain Crate Structure", this is the sole location
//! for agent-related business logic. HTTP handlers in `routes/` should
//! only extract inputs, call methods on this service, and wrap the
//! result in `ApiResponse`.
//!
//! Session-scoped operations (mode/model/config/usage/capabilities/
//! slash-commands/side-question/workspace/openclaw-runtime) now live in
//! `tjuaeui-conversation::ConversationService`, which dispatches through
//! `AgentInstance`. This service retains only agent-catalog and
//! active-diagnostics responsibilities. Engine Adapter mutation belongs to
//! the typed Core asset lifecycle rather than this read-only catalog service.

use std::path::PathBuf;
use std::sync::Arc;

use tjuaeui_api_types::{
    AgentDiagnosticRun, AgentLogoEntry, AgentManagementRow, ProviderHealthCheckRequest, ProviderHealthCheckResponse,
    StartAgentDiagnosticsRequest,
};
use tjuaeui_db::IProviderRepository;
use tjuaeui_realtime::EventBroadcaster;

use super::a2a::A2aAgentService;
use super::availability::{AgentAvailabilityFeedbackPort, AgentAvailabilityService};
use super::diagnostics::AgentDiagnosticsService;
use super::provider_health::ProviderHealthCheckService;
use crate::error::AgentError;
use crate::registry::AgentRegistry;

pub struct AgentService {
    registry: Arc<AgentRegistry>,
    provider_health: ProviderHealthCheckService,
    availability: AgentAvailabilityService,
    diagnostics: Arc<AgentDiagnosticsService>,
}

impl AgentService {
    pub fn new(
        registry: Arc<AgentRegistry>,
        a2a_service: Arc<A2aAgentService>,
        broadcaster: Arc<dyn EventBroadcaster>,
        provider_repo: Arc<dyn IProviderRepository>,
        encryption_key: [u8; 32],
        data_dir: PathBuf,
        session_spawner: Arc<dyn tjuaeui_process::Spawner>,
    ) -> Arc<Self> {
        let provider_health = ProviderHealthCheckService::new(provider_repo.clone(), encryption_key, data_dir.clone());
        let availability = AgentAvailabilityService::new(registry.clone(), provider_repo, session_spawner);
        let diagnostics =
            AgentDiagnosticsService::new(registry.clone(), availability.clone(), a2a_service, broadcaster.clone());
        Arc::new(Self {
            registry,
            provider_health,
            availability,
            diagnostics,
        })
    }

    pub fn availability_feedback_port(&self) -> Arc<dyn AgentAvailabilityFeedbackPort> {
        Arc::new(self.availability.clone())
    }
}

// Agent operations
impl AgentService {
    pub async fn list_management_agents(&self) -> Result<Vec<AgentManagementRow>, AgentError> {
        Ok(self.availability.list_management_rows().await)
    }

    /// Backend → logo URL catalog for business surfaces.
    ///
    /// Business pages (guid, team, cron, conversation lists) must render
    /// an agent logo from a backend identifier alone, without owning a
    /// hardcoded path map. This projects every known agent row — including
    /// user-disabled or currently-missing ones, so historical conversations
    /// still resolve a logo — down to its `backend` and stored `icon` URL.
    pub async fn list_agent_logos(&self) -> Result<Vec<AgentLogoEntry>, AgentError> {
        let mut seen = std::collections::HashSet::new();
        let mut entries = Vec::new();
        for agent in self.registry.list_all_including_hidden().await {
            let Some(logo) = agent.icon.filter(|value| !value.is_empty()) else {
                continue;
            };
            // Frontend rows resolve a logo from the conversation's runtime key,
            // which is the vendor `backend` for ACP agents but the `agent_type`
            // for backends without a vendor label (e.g. tjuae_cli, where `backend`
            // is NULL). Key on `backend` when present, otherwise the agent_type.
            let key = agent
                .backend
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| agent.agent_type.serde_name().to_owned());
            if key.is_empty() {
                continue;
            }
            if seen.insert(key.clone()) {
                entries.push(AgentLogoEntry { backend: key, logo });
            }
        }
        Ok(entries)
    }

    pub async fn diagnose_agent_by_id(&self, id: &str) -> Result<AgentManagementRow, AgentError> {
        self.diagnostics.diagnose_one(id).await
    }

    pub async fn start_agent_diagnostics(
        self: &Arc<Self>,
        request: StartAgentDiagnosticsRequest,
    ) -> Result<AgentDiagnosticRun, AgentError> {
        self.diagnostics.start(request).await
    }

    pub async fn current_agent_diagnostics(&self) -> Option<AgentDiagnosticRun> {
        self.diagnostics.current_run().await
    }

    pub async fn provider_health_check(
        &self,
        req: ProviderHealthCheckRequest,
    ) -> Result<ProviderHealthCheckResponse, AgentError> {
        self.provider_health.health_check(req).await
    }

    pub async fn get_agent_overrides(&self, id: &str) -> Result<tjuaeui_api_types::AgentOverridesResponse, AgentError> {
        let row = self
            .registry
            .repo_handle()
            .get(id)
            .await
            .map_err(|e| AgentError::internal(format!("读取 Agent 仓储失败：{e}")))?
            .ok_or_else(|| AgentError::not_found(format!("找不到 Agent“{id}”")))?;

        let env_override = row
            .env_override
            .as_deref()
            .and_then(|s| serde_json::from_str::<Vec<tjuaeui_api_types::AgentEnvEntry>>(s).ok())
            .unwrap_or_default();

        Ok(tjuaeui_api_types::AgentOverridesResponse {
            command_override: if is_internal_tjuae_cli_row(&row) {
                None
            } else {
                row.command_override
            },
            env_override,
        })
    }
}

fn is_internal_tjuae_cli_row(row: &tjuaeui_db::AgentMetadataRow) -> bool {
    row.agent_type.eq_ignore_ascii_case("tjuaecli") && row.agent_source.eq_ignore_ascii_case("internal")
}
