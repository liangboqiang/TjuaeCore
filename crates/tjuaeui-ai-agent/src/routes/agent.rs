#![allow(clippy::disallowed_types)]

//! Engine-management API routes backed by the internal agent registry.
//!
//! Endpoints:
//!
//! - `GET  /api/engines/management` — list diagnostics-first engine rows
//! - `POST /api/engines/{id}/diagnostics` — diagnose one persisted engine
//! - `POST /api/engines/diagnostics/run` — start a bounded background batch

use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Json, Path, State};
use axum::routing::{get, post};

use tjuaeui_api_types::{
    AgentDiagnosticRun, AgentLogoEntry, AgentManagementRow, AgentOverridesResponse, ApiResponse,
    ProviderHealthCheckRequest, ProviderHealthCheckResponse, StartAgentDiagnosticsRequest,
};
use tjuaeui_auth::CurrentUser;
use tjuaeui_common::ApiError;

use crate::routes::error_mapping::agent_error_to_api_error;
use crate::routes::state::AgentRouterState;

pub fn engine_routes(state: AgentRouterState) -> Router {
    Router::new()
        .route("/api/engines/logos", get(list_agent_logos))
        .route("/api/engines/management", get(list_management_agents))
        .route("/api/engines/{id}/diagnostics", post(diagnose_agent_by_id))
        .route("/api/engines/diagnostics/run", post(start_agent_diagnostics))
        .route("/api/engines/diagnostics/current", get(current_agent_diagnostics))
        .route("/api/engines/provider-health-check", post(provider_health_check))
        .route("/api/engines/{id}/overrides", get(get_agent_overrides))
        .with_state(state)
}

async fn list_agent_logos(
    State(state): State<AgentRouterState>,
    Extension(_user): Extension<CurrentUser>,
) -> Result<Json<ApiResponse<Vec<AgentLogoEntry>>>, ApiError> {
    Ok(Json(ApiResponse::ok(
        state
            .service
            .list_agent_logos()
            .await
            .map_err(agent_error_to_api_error)?,
    )))
}

async fn list_management_agents(
    State(state): State<AgentRouterState>,
    Extension(_user): Extension<CurrentUser>,
) -> Result<Json<ApiResponse<Vec<AgentManagementRow>>>, ApiError> {
    Ok(Json(ApiResponse::ok(
        state
            .service
            .list_management_agents()
            .await
            .map_err(agent_error_to_api_error)?,
    )))
}

async fn diagnose_agent_by_id(
    State(state): State<AgentRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<AgentManagementRow>>, ApiError> {
    Ok(Json(ApiResponse::ok(
        state
            .service
            .diagnose_agent_by_id(&id)
            .await
            .map_err(agent_error_to_api_error)?,
    )))
}

async fn start_agent_diagnostics(
    State(state): State<AgentRouterState>,
    Extension(_user): Extension<CurrentUser>,
    body: Result<Json<StartAgentDiagnosticsRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<AgentDiagnosticRun>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    Ok(Json(ApiResponse::ok(
        state
            .service
            .start_agent_diagnostics(request)
            .await
            .map_err(agent_error_to_api_error)?,
    )))
}

async fn current_agent_diagnostics(
    State(state): State<AgentRouterState>,
    Extension(_user): Extension<CurrentUser>,
) -> Result<Json<ApiResponse<Option<AgentDiagnosticRun>>>, ApiError> {
    Ok(Json(ApiResponse::ok(state.service.current_agent_diagnostics().await)))
}

async fn provider_health_check(
    State(state): State<AgentRouterState>,
    Extension(_user): Extension<CurrentUser>,
    body: Result<Json<ProviderHealthCheckRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<ProviderHealthCheckResponse>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    Ok(Json(ApiResponse::ok(
        state
            .service
            .provider_health_check(req)
            .await
            .map_err(agent_error_to_api_error)?,
    )))
}

async fn get_agent_overrides(
    State(state): State<AgentRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<AgentOverridesResponse>>, ApiError> {
    Ok(Json(ApiResponse::ok(
        state
            .service
            .get_agent_overrides(&id)
            .await
            .map_err(agent_error_to_api_error)?,
    )))
}
