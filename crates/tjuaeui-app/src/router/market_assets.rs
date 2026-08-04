#![allow(clippy::disallowed_types)]

use std::sync::Arc;

use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Json, Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use tjuaeui_api_types::{
    ApiResponse, AssetDiffResponse, AssetFileResponse, AssetOperationResponse, AssetResolveResponse,
    AssetRestoreResponse, InstallMarketAssetRequest, ListMarketAssetsQuery, MarketCacheResponse, MarketIndexResponse,
    ReadMarketAssetFileQuery, RefreshMarketRequest, ResolveAssetRequest, RestoreAssetRequest,
};
use tjuaeui_asset::{AssetCatalogService, MarketError, MarketIndexManager};
use tjuaeui_auth::CurrentUser;
use tjuaeui_common::ApiError;

use super::local_assets::asset_api_error;

#[derive(Clone)]
pub struct MarketRouterState {
    pub manager: Arc<MarketIndexManager>,
    pub catalog: Arc<AssetCatalogService>,
}

pub fn market_asset_routes(state: MarketRouterState) -> Router {
    Router::new()
        .route("/api/market/assets", get(list_market_assets))
        .route("/api/market/files", get(read_market_asset_file))
        .route("/api/market/assets/install", post(install_market_asset))
        .route("/api/market/assets/sync", post(sync_market_asset))
        .route("/api/market/local/{assetId}/diff", get(diff_local_asset))
        .route("/api/market/local/{assetId}/resolve", post(resolve_local_asset))
        .route("/api/market/local/{assetId}/restore", post(restore_local_asset))
        .route("/api/market/refresh", post(refresh_market))
        .with_state(state)
}

async fn diff_local_asset(
    State(state): State<MarketRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path(asset_id): Path<String>,
) -> Result<Json<ApiResponse<AssetDiffResponse>>, ApiError> {
    let diff = state
        .manager
        .diff_local_asset(&user.id, &state.catalog, &asset_id)
        .await
        .map_err(market_api_error)?;
    Ok(Json(ApiResponse::ok(diff)))
}

async fn resolve_local_asset(
    State(state): State<MarketRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path(asset_id): Path<String>,
    body: Result<Json<ResolveAssetRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<AssetResolveResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let resolved = state
        .manager
        .resolve_local_asset(&user.id, &state.catalog, &asset_id, request)
        .await
        .map_err(market_api_error)?;
    Ok(Json(ApiResponse::ok(resolved)))
}

async fn restore_local_asset(
    State(state): State<MarketRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path(asset_id): Path<String>,
    body: Result<Json<RestoreAssetRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<AssetRestoreResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let restored = state
        .catalog
        .restore_resolution(&user.id, &asset_id, request)
        .await
        .map_err(asset_api_error)?;
    Ok(Json(ApiResponse::ok(restored)))
}

async fn list_market_assets(
    State(state): State<MarketRouterState>,
    Extension(user): Extension<CurrentUser>,
    Query(query): Query<ListMarketAssetsQuery>,
) -> Result<Json<ApiResponse<MarketIndexResponse>>, ApiError> {
    let index = state
        .manager
        .load_index(&user.id, &state.catalog, &query)
        .await
        .map_err(market_api_error)?;
    Ok(Json(ApiResponse::ok(index)))
}

async fn read_market_asset_file(
    State(state): State<MarketRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Query(query): Query<ReadMarketAssetFileQuery>,
) -> Result<Json<ApiResponse<AssetFileResponse>>, ApiError> {
    let file = state
        .manager
        .read_asset_file(&query.remote_asset_id, &query.path)
        .await
        .map_err(market_api_error)?;
    Ok(Json(ApiResponse::ok(file)))
}

async fn install_market_asset(
    State(state): State<MarketRouterState>,
    Extension(user): Extension<CurrentUser>,
    body: Result<Json<InstallMarketAssetRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<AssetOperationResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let operation = state
        .manager
        .install_asset(
            &user.id,
            &state.catalog,
            &request.remote_asset_id,
            &request.idempotency_key,
        )
        .await
        .map_err(market_api_error)?;
    Ok(Json(ApiResponse::ok(operation)))
}

async fn sync_market_asset(
    State(state): State<MarketRouterState>,
    Extension(user): Extension<CurrentUser>,
    body: Result<Json<InstallMarketAssetRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<AssetOperationResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let operation = state
        .manager
        .sync_asset(
            &user.id,
            &state.catalog,
            &request.remote_asset_id,
            &request.idempotency_key,
        )
        .await
        .map_err(market_api_error)?;
    Ok(Json(ApiResponse::ok(operation)))
}

async fn refresh_market(
    State(state): State<MarketRouterState>,
    Extension(_user): Extension<CurrentUser>,
    body: Result<Json<RefreshMarketRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<MarketCacheResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let cache = state
        .manager
        .refresh(request.distribution_revision.as_deref())
        .await
        .map_err(market_api_error)?;
    Ok(Json(ApiResponse::ok(cache)))
}

pub(super) fn market_api_error(error: MarketError) -> ApiError {
    match error {
        MarketError::Unavailable => ApiError::coded(
            StatusCode::SERVICE_UNAVAILABLE,
            "MARKET_UNAVAILABLE",
            "远程资产市场当前不可用，请稍后重试。",
            None,
        ),
        MarketError::Network(_) => ApiError::coded(
            StatusCode::BAD_GATEWAY,
            "MARKET_UPSTREAM_UNAVAILABLE",
            "无法连接 TjuaeHub，请检查网络后重试。",
            None,
        ),
        MarketError::TooLarge { .. } => ApiError::coded(
            StatusCode::PAYLOAD_TOO_LARGE,
            "MARKET_RESPONSE_TOO_LARGE",
            "远程市场响应超过安全大小限制。",
            None,
        ),
        MarketError::Invalid(_) | MarketError::InvalidCommit | MarketError::Json(_) => ApiError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "MARKET_INDEX_INVALID",
            "远程市场索引未通过完整性校验。",
            None,
        ),
        MarketError::Asset(error) => asset_api_error(error),
        MarketError::Io(_) => ApiError::coded(
            StatusCode::INTERNAL_SERVER_ERROR,
            "MARKET_CACHE_FAILED",
            "远程市场缓存操作失败。",
            None,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    #[tokio::test]
    async fn strict_query_rejects_user_identity_override_before_network_access() {
        let database = tjuaeui_db::init_database_memory().await.unwrap();
        let repo: Arc<dyn tjuaeui_db::IAssetRepository> =
            Arc::new(tjuaeui_db::SqliteAssetRepository::new(database.pool().clone()));
        let temp = tempfile::tempdir().unwrap();
        let state = MarketRouterState {
            manager: Arc::new(MarketIndexManager::new(temp.path())),
            catalog: Arc::new(AssetCatalogService::new(repo, temp.path())),
        };
        let mut request = Request::builder()
            .uri("/api/market/assets?userId=other")
            .body(Body::empty())
            .unwrap();
        request.extensions_mut().insert(CurrentUser {
            id: "system_default_user".into(),
            username: "system_default_user".into(),
        });
        let response = market_asset_routes(state).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn resolve_route_rejects_unknown_fields_before_market_access() {
        let database = tjuaeui_db::init_database_memory().await.unwrap();
        let repo: Arc<dyn tjuaeui_db::IAssetRepository> =
            Arc::new(tjuaeui_db::SqliteAssetRepository::new(database.pool().clone()));
        let temp = tempfile::tempdir().unwrap();
        let state = MarketRouterState {
            manager: Arc::new(MarketIndexManager::new(temp.path())),
            catalog: Arc::new(AssetCatalogService::new(repo, temp.path())),
        };
        let body = serde_json::json!({
            "strategy": "keepLocal",
            "expectedLocalDigest": format!("sha256-{}", "1".repeat(64)),
            "expectedBaseDigest": format!("sha256-{}", "2".repeat(64)),
            "expectedRemoteDigest": format!("sha256-{}", "3".repeat(64)),
            "idempotencyKey": "route-test",
            "confirmDestructive": false,
            "userId": "other-user"
        });
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/market/local/skill-demo/resolve")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        request.extensions_mut().insert(CurrentUser {
            id: "system_default_user".into(),
            username: "system_default_user".into(),
        });
        let response = market_asset_routes(state).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
