pub mod acp_assembler;

mod a2a;
mod acp;
mod acp_launch_policy;
mod context;
pub(crate) mod tjuae_cli;

use std::path::PathBuf;
use std::sync::Arc;

use futures_util::FutureExt;
use tjuaeui_db::{IA2aRepository, IMcpServerRepository, IProviderRepository};
use tjuaeui_realtime::EventBroadcaster;

use crate::agent_task::AgentInstance;
use crate::capability::skill_manager::AcpSkillManager;
use crate::error::AgentError;
use crate::factory::context::FactoryContext;
use crate::persistence::AcpSessionSyncService;
use crate::registry::AgentRegistry;
use crate::session_context::AgentSessionKind;
use crate::task_manager::AgentFactory;
use crate::types::BuildTaskOptions;

/// Dependencies needed by the agent factory to construct agents.
pub struct AgentFactoryDeps {
    pub skill_manager: Arc<AcpSkillManager>,
    pub provider_repo: Arc<dyn IProviderRepository>,
    pub a2a_repo: Arc<dyn IA2aRepository>,
    pub encryption_key: [u8; 32],
    pub agent_registry: Arc<AgentRegistry>,
    pub acp_agent_service: Arc<AcpSessionSyncService>,
    pub data_dir: PathBuf,
    pub dump_prompts: bool,
    pub broadcaster: Arc<dyn EventBroadcaster>,
    /// Absolute path to the backend binary, reused as the `command` of the
    /// stdio MCP bridge injected into ACP `session/new` for team sessions.
    /// Captured once at app startup (`std::env::current_exe()`).
    pub backend_binary_path: Arc<PathBuf>,
    /// User-configured MCP servers repository. Used by ACP factory to
    /// inject enabled servers into `session/new` (ELECTRON-1JG fix).
    /// `None` for tests/composition paths that do not need MCP injection.
    pub mcp_server_repo: Option<Arc<dyn IMcpServerRepository>>,
    /// Resolves the current user's non-persisted runtime Overlay and credentials
    /// for managed Engine/MCP assets immediately before a process/connection is
    /// created. `None` is only valid when no managed runtime asset is launched.
    pub runtime_asset_configuration_resolver: Option<Arc<dyn tjuaeui_asset::RuntimeAssetConfigurationResolver>>,
    /// Subprocess spawner for the clean-slate session model. claude/codex always
    /// run through `SessionAgentTask` (direct-CLI) instead of the ACP manager, so
    /// the spawner is unconditionally wired — there is no fallback to the ACP path.
    pub session_spawner: Arc<dyn tjuaeui_process::Spawner>,
}

/// Build a production agent factory that dispatches to concrete agent types.
///
/// [`AgentFactory`] is async: the returned `BoxFuture` is driven by
/// [`crate::task_manager::IWorkerTaskManager::get_or_build_task`] on whatever
/// runtime is currently polling it. This lets us spawn CLI processes and
/// await ACP handshakes directly, without the scoped-thread + `block_on`
/// bridge the old sync-factory version needed.
pub fn build_agent_factory(deps: AgentFactoryDeps) -> AgentFactory {
    let deps = Arc::new(deps);

    Arc::new(move |options: BuildTaskOptions| {
        let deps = deps.clone();
        async move { build_agent(deps, options).await }.boxed()
    })
}

async fn build_agent(deps: Arc<AgentFactoryDeps>, options: BuildTaskOptions) -> Result<AgentInstance, AgentError> {
    let runtime_asset_request = options.runtime_asset_request;
    let runtime_boundary_reporter = options.runtime_boundary_reporter;
    let context = options.context;
    let ctx = FactoryContext::resolve(&context).await?;
    let model = context.model.clone();
    match context.kind {
        AgentSessionKind::Acp(acp_context) => {
            acp::build(
                deps,
                *acp_context,
                ctx,
                runtime_asset_request,
                runtime_boundary_reporter,
            )
            .await
        }
        AgentSessionKind::A2a(a2a_context) => {
            a2a::build(
                deps,
                *a2a_context,
                ctx,
                runtime_asset_request,
                runtime_boundary_reporter,
            )
            .await
        }
        AgentSessionKind::TjuaeCli(tjuae_cli_context) => {
            tjuae_cli::build(
                deps,
                *tjuae_cli_context,
                model,
                ctx,
                runtime_asset_request,
                runtime_boundary_reporter,
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_deps_can_be_constructed() {
        // Verify types compile — actual construction requires DB
        let _: fn() -> AgentFactoryDeps = || {
            panic!("compile-time check only");
        };
    }
}
