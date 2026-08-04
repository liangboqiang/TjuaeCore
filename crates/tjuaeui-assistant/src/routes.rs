#![allow(clippy::disallowed_types)]

//! HTTP route handlers for `/api/assistants/*`.

use axum::Router;
use axum::body::Body;
use axum::extract::{Extension, Json, Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use axum::routing::get;

use tjuaeui_api_types::{ApiResponse, AssistantDetailResponse, AssistantResponse};
use tjuaeui_auth::CurrentUser;
use tjuaeui_common::ApiError;

use crate::error::AssistantError;
pub use crate::state::AssistantRouterState;

/// Build the router for `/api/assistants/*`.
pub fn assistant_routes(state: AssistantRouterState) -> Router {
    Router::new()
        .route("/api/assistants", get(list))
        .route("/api/assistants/{id}", get(get_one))
        .route("/api/assistants/{id}/avatar", get(get_avatar))
        .with_state(state)
}

#[derive(Debug, serde::Deserialize, Default)]
struct GetAssistantDetailQuery {
    locale: Option<String>,
}

impl From<AssistantError> for ApiError {
    fn from(error: AssistantError) -> Self {
        match error {
            AssistantError::NotFound(message) => Self::NotFound(message),
            AssistantError::BadRequest(message) => Self::BadRequest(message),
            AssistantError::Forbidden(message) => Self::Forbidden(message),
            AssistantError::Conflict(message) => Self::Conflict(message),
            AssistantError::Internal(message) => Self::Internal(message),
            // Only produced by startup assistant-storage bootstrap (never on an
            // HTTP path); treated as a transient internal condition if it ever
            // surfaces through the API boundary.
            AssistantError::ConcurrentBootstrapContention(message) => Self::Internal(message),
        }
    }
}

async fn list(
    State(state): State<AssistantRouterState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<ApiResponse<Vec<AssistantResponse>>>, ApiError> {
    let items = state.service.list_for_user(&user.id).await?;
    Ok(Json(ApiResponse::ok(items)))
}

async fn get_one(
    State(state): State<AssistantRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path(id): Path<String>,
    Query(query): Query<GetAssistantDetailQuery>,
) -> Result<Json<ApiResponse<AssistantDetailResponse>>, ApiError> {
    let detail = state
        .service
        .get_detail_for_user(&user.id, &id, query.locale.as_deref())
        .await?;
    Ok(Json(ApiResponse::ok(detail)))
}

/// Serve the raw avatar bytes for an assistant. Content-Type inferred from the
/// file extension (png/jpg/svg default). Extensions return 404 — the frontend
/// serves those via `tjuae-asset://`.
async fn get_avatar(
    State(state): State<AssistantRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let asset = state
        .service
        .avatar_asset_for_user(&user.id, &id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("找不到头像“{id}”")))?;

    let content_type = content_type_for_extension(asset.extension.as_deref());

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(asset.bytes))
        .map_err(|e| ApiError::Internal(e.to_string()))
}

fn content_type_for_extension(ext: Option<&str>) -> HeaderValue {
    let mime = match ext {
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => "application/octet-stream",
    };
    HeaderValue::from_static(mime)
}
