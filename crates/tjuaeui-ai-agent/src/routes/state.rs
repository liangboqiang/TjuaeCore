use std::sync::Arc;

use crate::{A2aAgentService, AgentRegistry, AgentService};

#[derive(Clone)]
pub struct AgentRouterState {
    pub agent_registry: Arc<AgentRegistry>,
    pub service: Arc<AgentService>,
    pub a2a_service: Arc<A2aAgentService>,
}
