//! Router state carrying the assistant service for axum handlers.

use std::sync::Arc;

use crate::{AssistantActivationService, AssistantAgentCatalogPort, AssistantCatalogService};

/// Shared state injected into `/api/assistants/*` handlers.
#[derive(Clone)]
pub struct AssistantRouterState {
    pub catalog: Arc<AssistantCatalogService>,
    pub activation: Arc<AssistantActivationService>,
    pub agents: Arc<dyn AssistantAgentCatalogPort>,
}
