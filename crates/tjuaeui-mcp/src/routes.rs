#![allow(clippy::disallowed_types)]

use axum::Router;
use axum::extract::{Json, Path, State};
use axum::routing::get;

use tjuaeui_api_types::{ApiResponse, DetectedMcpServerResponse, McpServerResponse};
use tjuaeui_common::ApiError;

use crate::error::McpError;
use crate::service::McpConfigService;
use crate::sync_service::McpSyncService;

impl From<McpError> for ApiError {
    fn from(err: McpError) -> Self {
        match err {
            McpError::NotFound(msg) => ApiError::NotFound(msg),
            McpError::Conflict(msg) => ApiError::Conflict(msg),
            McpError::InvalidEdit(msg) => ApiError::BadRequest(msg),
            McpError::InvalidTransport(msg) => ApiError::BadRequest(msg),
            McpError::AgentNotInstalled(msg) => ApiError::BadRequest(msg),
            McpError::AgentOperationFailed(msg) => ApiError::Internal(msg),
            McpError::ConnectionFailed(msg) => ApiError::BadGateway(msg),
            McpError::OAuth(msg) => ApiError::Internal(format!("OAuth error: {msg}")),
            McpError::Database(db_err) => ApiError::Internal(db_err.to_string()),
            McpError::Json(e) => ApiError::Internal(format!("JSON error: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------
// Router state
// ---------------------------------------------------------------------------

/// Shared state for MCP route handlers.
#[derive(Clone)]
pub struct McpRouterState {
    pub config_service: McpConfigService,
    pub sync_service: McpSyncService,
}

// ---------------------------------------------------------------------------
// Router builder
// ---------------------------------------------------------------------------

/// Build the MCP router with all `/api/mcp/*` routes.
///
/// 仅暴露运行投影的只读视图和诊断扫描。Definition/Overlay、试跑与启停
/// 必须统一通过 `/api/assets` 的类型化生命周期完成。
pub fn mcp_routes(state: McpRouterState) -> Router {
    Router::new()
        .route("/api/mcp/servers", get(list_servers))
        .route("/api/mcp/servers/{id}", get(get_server))
        .route("/api/mcp/agent-configs", get(get_agent_configs))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// CRUD Handlers
// ---------------------------------------------------------------------------

/// `GET /api/mcp/servers` — list all MCP servers.
async fn list_servers(
    State(state): State<McpRouterState>,
) -> Result<Json<ApiResponse<Vec<McpServerResponse>>>, ApiError> {
    let servers = state.config_service.list_servers().await.map_err(ApiError::from)?;
    Ok(Json(ApiResponse::ok(servers)))
}

/// `GET /api/mcp/servers/:id` — get a single MCP server.
async fn get_server(
    State(state): State<McpRouterState>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<McpServerResponse>>, ApiError> {
    let server = state.config_service.get_server(&id).await.map_err(ApiError::from)?;
    Ok(Json(ApiResponse::ok(server)))
}

// ---------------------------------------------------------------------------
// Agent Sync Handlers
// ---------------------------------------------------------------------------

/// `GET /api/mcp/agent-configs` — scan all installed Agent CLIs
/// and return their current MCP server configurations.
async fn get_agent_configs(
    State(state): State<McpRouterState>,
) -> Result<Json<ApiResponse<Vec<DetectedMcpServerResponse>>>, ApiError> {
    let configs = state.sync_service.get_agent_configs().await.map_err(ApiError::from)?;
    Ok(Json(ApiResponse::ok(configs)))
}

#[cfg(test)]
mod error_mapping_tests {
    use super::*;

    #[test]
    fn not_found_maps_to_app_not_found() {
        let err = ApiError::from(McpError::NotFound("mcp_123".into()));
        assert!(matches!(err, ApiError::NotFound(msg) if msg == "mcp_123"));
    }

    #[test]
    fn conflict_maps_to_app_conflict() {
        let err = ApiError::from(McpError::Conflict("test-server".into()));
        assert!(matches!(err, ApiError::Conflict(_)));
    }

    #[test]
    fn invalid_transport_maps_to_bad_request() {
        let err = ApiError::from(McpError::InvalidTransport("missing command".into()));
        assert!(matches!(err, ApiError::BadRequest(_)));
    }

    #[test]
    fn invalid_edit_maps_to_bad_request() {
        let err = ApiError::from(McpError::InvalidEdit("rename forbidden".into()));
        assert!(matches!(err, ApiError::BadRequest(_)));
    }

    #[test]
    fn agent_not_installed_maps_to_bad_request() {
        let err = ApiError::from(McpError::AgentNotInstalled("claude".into()));
        assert!(matches!(err, ApiError::BadRequest(_)));
    }

    #[test]
    fn agent_operation_failed_maps_to_internal() {
        let err = ApiError::from(McpError::AgentOperationFailed("exit code 1".into()));
        assert!(matches!(err, ApiError::Internal(_)));
    }

    #[test]
    fn connection_failed_maps_to_bad_gateway() {
        let err = ApiError::from(McpError::ConnectionFailed("timeout".into()));
        assert!(matches!(err, ApiError::BadGateway(_)));
    }

    #[test]
    fn oauth_maps_to_internal() {
        let err = ApiError::from(McpError::OAuth("discovery failed".into()));
        assert!(matches!(err, ApiError::Internal(_)));
    }

    #[test]
    fn json_error_maps_to_internal() {
        let json_err = serde_json::from_str::<serde_json::Value>("invalid").unwrap_err();
        let err = ApiError::from(McpError::Json(json_err));
        assert!(matches!(err, ApiError::Internal(_)));
    }
}
