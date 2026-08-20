#![allow(clippy::disallowed_types)]

//! HTTP route handlers for `/api/assistants/*`.

use axum::Router;
use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Json, Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::Response;
use axum::routing::{get, post};

use tjuaeui_api_types::{
    ActivateAssistantRequest, ApiResponse, AssistantCatalogDetailResponse, AssistantCatalogFileContentResponse,
    AssistantCatalogFileQuery, AssistantCatalogPageResponse, AssistantCatalogQuery, AssistantIdentityResponse,
    AssistantOperationResponse, AssistantRuntimeAgentResponse, AssistantRuntimeOptionResponse, AssistantSourceResponse,
    AssistantVersionComparisonResponse, AssistantVersionQuery, CopyAssistantToMineRequest, CreateMineAssistantRequest,
    ExportAssistantRequest, ExportAssistantResponse, PrepareAssistantRequest, PublishAssistantCatalogRequest,
    PublishAssistantCatalogResponse, SaveAssistantCatalogFileRequest, UpdateAssistantCatalogPreferencesRequest,
    UpdateAssistantCatalogSettingsRequest,
};
use tjuaeui_common::ApiError;

use crate::error::AssistantError;
pub use crate::state::AssistantRouterState;

/// Build the router for `/api/assistants/*`.
pub fn assistant_routes(state: AssistantRouterState) -> Router {
    Router::new()
        .route("/api/assistant-runtime/options", get(list_runtime_options))
        .route(
            "/api/assistants/catalog/{source}",
            get(list_catalog).post(create_mine_catalog),
        )
        .route(
            "/api/assistants/catalog/{source}/{namespace}/{slug}",
            get(get_catalog_detail)
                .patch(update_catalog_preferences)
                .delete(delete_catalog_assistant),
        )
        .route(
            "/api/assistants/catalog/{source}/{namespace}/{slug}/file",
            get(get_catalog_file).put(save_catalog_file),
        )
        .route(
            "/api/assistants/catalog/{source}/{namespace}/{slug}/settings",
            axum::routing::put(update_catalog_settings),
        )
        .route(
            "/api/assistants/catalog/{source}/{namespace}/{slug}/publish",
            post(publish_catalog_assistant),
        )
        .route(
            "/api/assistants/catalog/{source}/{namespace}/{slug}/compare/{base}/{target}",
            get(compare_catalog_versions),
        )
        .route(
            "/api/assistants/catalog/{source}/{namespace}/{slug}/activation/prepare",
            post(prepare_activation),
        )
        .route(
            "/api/assistants/catalog/{source}/{namespace}/{slug}/activation/commit",
            post(commit_activation),
        )
        .route(
            "/api/assistants/catalog/{source}/{namespace}/{slug}/copy-to-mine",
            post(copy_catalog_to_mine),
        )
        .route(
            "/api/assistants/catalog/{source}/{namespace}/{slug}/export",
            post(export_catalog_assistant),
        )
        .with_state(state)
}

/// Public catalog assets are limited to files declared by the selected
/// assistant revision, so renderer elements can embed them without exposing
/// arbitrary filesystem paths.
pub fn assistant_asset_routes(state: AssistantRouterState) -> Router {
    Router::new()
        .route(
            "/api/assistant-assets/{source}/{namespace}/{slug}",
            get(get_catalog_asset),
        )
        .with_state(state)
}

async fn get_catalog_asset(
    State(state): State<AssistantRouterState>,
    Path((source, namespace, slug)): Path<(String, String, String)>,
    Query(query): Query<AssistantCatalogFileQuery>,
) -> Result<Response, ApiError> {
    let identity = catalog_identity(&source, namespace, slug)?;
    let bytes = state
        .catalog
        .asset_bytes(&identity, query.version.as_deref(), &query.path)
        .await?;
    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            mime_guess::from_path(&query.path).first_or_octet_stream().as_ref(),
        )
        .header(header::CACHE_CONTROL, "public, max-age=3600")
        .body(Body::from(bytes))
        .map_err(|error| ApiError::Internal(error.to_string()))
}

async fn list_runtime_options(
    State(state): State<AssistantRouterState>,
) -> Result<Json<ApiResponse<Vec<AssistantRuntimeOptionResponse>>>, ApiError> {
    let profiles = state.catalog.list_runtime_profiles().await?;
    let agents = state.agents.list_management_agents().await?;
    Ok(Json(ApiResponse::ok(
        profiles
            .into_iter()
            .map(|profile| {
                let management = agents.iter().find(|candidate| {
                    candidate.id == profile.agent_id
                        || candidate.backend.as_deref() == Some(profile.agent_id.as_str())
                        || candidate.agent_type.serde_name() == profile.agent_id
                        || candidate.name.eq_ignore_ascii_case(&profile.agent_id)
                });
                AssistantRuntimeOptionResponse {
                    id: profile.id,
                    identity: profile.identity,
                    version: profile.version,
                    name: profile.name,
                    name_i18n: profile.name_i18n,
                    description: profile.description,
                    description_i18n: profile.description_i18n,
                    avatar_url: profile.avatar_url,
                    agent_id: management.map(|agent| agent.id.clone()).unwrap_or(profile.agent_id),
                    agent: management.map(|agent| AssistantRuntimeAgentResponse {
                        agent_type: agent.agent_type.serde_name().to_owned(),
                        source: serde_enum_string(&agent.agent_source),
                        backend: agent
                            .backend
                            .clone()
                            .unwrap_or_else(|| agent.agent_type.serde_name().to_owned()),
                    }),
                    agent_status: management
                        .map(|agent| serde_enum_string(&agent.status))
                        .unwrap_or_else(|| "missing".to_owned()),
                    team_selectable: management.is_some_and(|agent| {
                        agent.enabled
                            && agent.installed
                            && agent.status == tjuaeui_api_types::AgentManagementStatus::Online
                    }),
                    model_ids: profile.model.into_iter().collect(),
                    permission: profile.permission,
                    thought_level: profile.thought_level,
                    skill_ids: profile.skill_ids,
                    mcp_ids: profile.mcp_ids,
                    recommended_prompts: profile.recommended_prompts,
                    recommended_prompts_i18n: profile.recommended_prompts_i18n,
                    sort_order: profile.sort_order,
                    last_used_at: profile.last_used_at,
                }
            })
            .collect(),
    )))
}

fn serde_enum_string(value: &impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

impl From<AssistantError> for ApiError {
    fn from(error: AssistantError) -> Self {
        match error {
            AssistantError::NotFound(message) => Self::NotFound(message),
            AssistantError::BadRequest(message) => Self::BadRequest(message),
            AssistantError::Forbidden(message) => Self::Forbidden(message),
            AssistantError::Conflict(message) => Self::Conflict(message),
            AssistantError::Internal(message) => Self::Internal(message),
        }
    }
}

async fn list_catalog(
    State(state): State<AssistantRouterState>,
    Path(source): Path<String>,
    Query(query): Query<AssistantCatalogQuery>,
) -> Result<Json<ApiResponse<AssistantCatalogPageResponse>>, ApiError> {
    let page = state
        .catalog
        .list(
            parse_catalog_source(&source)?,
            &query.query,
            &query.sort,
            query.cursor.as_deref(),
            query.limit.unwrap_or(40),
        )
        .await?;
    Ok(Json(ApiResponse::ok(page)))
}

async fn create_mine_catalog(
    State(state): State<AssistantRouterState>,
    Path(source): Path<String>,
    body: Result<Json<CreateMineAssistantRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<ApiResponse<AssistantCatalogDetailResponse>>), ApiError> {
    if parse_catalog_source(&source)? != AssistantSourceResponse::Mine {
        return Err(ApiError::BadRequest("只能在“我的助手”中创建助手".to_owned()));
    }
    let Json(request) = body.map_err(ApiError::from)?;
    let detail = state.catalog.create_mine(request).await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::ok(detail))))
}

async fn get_catalog_detail(
    State(state): State<AssistantRouterState>,
    Path((source, namespace, slug)): Path<(String, String, String)>,
    Query(query): Query<AssistantVersionQuery>,
) -> Result<Json<ApiResponse<AssistantCatalogDetailResponse>>, ApiError> {
    let identity = catalog_identity(&source, namespace, slug)?;
    let detail = state.catalog.detail(&identity, query.version.as_deref()).await?;
    Ok(Json(ApiResponse::ok(detail)))
}

async fn get_catalog_file(
    State(state): State<AssistantRouterState>,
    Path((source, namespace, slug)): Path<(String, String, String)>,
    Query(query): Query<AssistantCatalogFileQuery>,
) -> Result<Json<ApiResponse<AssistantCatalogFileContentResponse>>, ApiError> {
    let identity = catalog_identity(&source, namespace, slug)?;
    let file = state
        .catalog
        .file_content(&identity, query.version.as_deref(), &query.path)
        .await?;
    Ok(Json(ApiResponse::ok(file)))
}

async fn save_catalog_file(
    State(state): State<AssistantRouterState>,
    Path((source, namespace, slug)): Path<(String, String, String)>,
    body: Result<Json<SaveAssistantCatalogFileRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<AssistantCatalogDetailResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let identity = catalog_identity(&source, namespace, slug)?;
    let detail = state.catalog.save_file(&identity, request).await?;
    Ok(Json(ApiResponse::ok(detail)))
}

async fn update_catalog_settings(
    State(state): State<AssistantRouterState>,
    Path((source, namespace, slug)): Path<(String, String, String)>,
    body: Result<Json<UpdateAssistantCatalogSettingsRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<AssistantCatalogDetailResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let identity = catalog_identity(&source, namespace, slug)?;
    let detail = state.catalog.update_settings(&identity, request).await?;
    Ok(Json(ApiResponse::ok(detail)))
}

async fn publish_catalog_assistant(
    State(state): State<AssistantRouterState>,
    Path((source, namespace, slug)): Path<(String, String, String)>,
    body: Result<Json<PublishAssistantCatalogRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<PublishAssistantCatalogResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let identity = catalog_identity(&source, namespace, slug)?;
    let result = state.activation.publish(&identity, request).await?;
    Ok(Json(ApiResponse::ok(result)))
}

async fn compare_catalog_versions(
    State(state): State<AssistantRouterState>,
    Path((source, namespace, slug, base, target)): Path<(String, String, String, String, String)>,
) -> Result<Json<ApiResponse<AssistantVersionComparisonResponse>>, ApiError> {
    let identity = catalog_identity(&source, namespace, slug)?;
    let comparison = state.catalog.compare_versions(&identity, &base, &target).await?;
    Ok(Json(ApiResponse::ok(comparison)))
}

async fn update_catalog_preferences(
    State(state): State<AssistantRouterState>,
    Path((source, namespace, slug)): Path<(String, String, String)>,
    body: Result<Json<UpdateAssistantCatalogPreferencesRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<AssistantOperationResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let identity = catalog_identity(&source, namespace, slug)?;
    let operation = state.catalog.update_preferences(&identity, request).await?;
    Ok(Json(ApiResponse::ok(operation)))
}

async fn delete_catalog_assistant(
    State(state): State<AssistantRouterState>,
    Path((source, namespace, slug)): Path<(String, String, String)>,
) -> Result<Json<ApiResponse<()>>, ApiError> {
    let identity = catalog_identity(&source, namespace, slug)?;
    state.catalog.delete_mine(&identity).await?;
    Ok(Json(ApiResponse::success()))
}

async fn copy_catalog_to_mine(
    State(state): State<AssistantRouterState>,
    Path((source, namespace, slug)): Path<(String, String, String)>,
    body: Result<Json<CopyAssistantToMineRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<AssistantCatalogDetailResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let identity = catalog_identity(&source, namespace, slug)?;
    let detail = state.catalog.copy_to_mine(&identity, request).await?;
    Ok(Json(ApiResponse::ok(detail)))
}

async fn export_catalog_assistant(
    State(state): State<AssistantRouterState>,
    Path((source, namespace, slug)): Path<(String, String, String)>,
    body: Result<Json<ExportAssistantRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<ExportAssistantResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let identity = catalog_identity(&source, namespace, slug)?;
    let exported = state.activation.export(&identity, request).await?;
    Ok(Json(ApiResponse::ok(exported)))
}

async fn prepare_activation(
    State(state): State<AssistantRouterState>,
    Path((source, namespace, slug)): Path<(String, String, String)>,
    body: Result<Json<PrepareAssistantRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<tjuaeui_api_types::AssistantActivationPlanResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let identity = catalog_identity(&source, namespace, slug)?;
    let plan = state.activation.prepare(identity, request.version.as_deref()).await?;
    Ok(Json(ApiResponse::ok(plan)))
}

async fn commit_activation(
    State(state): State<AssistantRouterState>,
    Path((source, namespace, slug)): Path<(String, String, String)>,
    body: Result<Json<ActivateAssistantRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<AssistantOperationResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let identity = catalog_identity(&source, namespace, slug)?;
    let operation = state.activation.activate(identity, request).await?;
    Ok(Json(ApiResponse::ok(operation)))
}

fn parse_catalog_source(source: &str) -> Result<AssistantSourceResponse, ApiError> {
    match source {
        "mine" => Ok(AssistantSourceResponse::Mine),
        "tjuae-hub" => Ok(AssistantSourceResponse::TjuaeHub),
        _ => Err(ApiError::BadRequest(format!("未知助手来源：{source}"))),
    }
}

fn catalog_identity(source: &str, namespace: String, slug: String) -> Result<AssistantIdentityResponse, ApiError> {
    if slug.trim().is_empty() {
        return Err(ApiError::BadRequest("助手标识不能为空".to_owned()));
    }
    Ok(AssistantIdentityResponse {
        source: parse_catalog_source(source)?,
        namespace: if namespace == "~" { String::new() } else { namespace },
        slug,
    })
}
