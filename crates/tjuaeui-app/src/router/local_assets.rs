#![allow(clippy::disallowed_types)]

use std::sync::Arc;

use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Json, Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use tjuaeui_api_types::{
    ApiResponse, AssetCollaborationCapability, AssetCollaborationProtocolResponse, AssetDetailResponse,
    AssetDiffResponse, AssetFileResponse, AssetOperationRequest, AssetOperationResponse, AssetOverlayResponse,
    AssetRuntimeCommandRequest, AssetRuntimeStatusResponse, AssetSummaryResponse, ConfigureAssetRequest,
    CreateAssetRequest, DuplicateAssetRequest, GetAssetQuery, ListAssetsQuery, ReadAssetFileQuery,
    WriteAssetFileRequest,
};
use tjuaeui_asset::{AssetCatalogService, AssetError, MarketIndexManager, TJUAE_ASSET_PROTOCOL_VERSION};
use tjuaeui_auth::CurrentUser;
use tjuaeui_common::ApiError;

#[derive(Clone)]
pub struct LocalAssetRouterState {
    pub service: Arc<AssetCatalogService>,
    pub market: Arc<MarketIndexManager>,
}

pub fn local_asset_routes(state: LocalAssetRouterState) -> Router {
    Router::new()
        .route("/api/assets/protocol", get(get_asset_collaboration_protocol))
        .route("/api/assets", get(list_assets).post(create_asset))
        .route("/api/assets/{assetId}", get(get_asset))
        .route("/api/assets/{assetId}/duplicate", post(duplicate_asset))
        .route("/api/assets/{assetId}/overlay", get(get_asset_overlay))
        .route("/api/assets/{assetId}/configure", axum::routing::put(configure_asset))
        .route("/api/assets/{assetId}/validate", post(validate_asset))
        .route("/api/assets/{assetId}/try-run", post(try_run_asset))
        .route("/api/assets/{assetId}/activate", post(activate_asset))
        .route("/api/assets/{assetId}/deactivate", post(deactivate_asset))
        .route(
            "/api/assets/{assetId}/files",
            get(read_asset_file).put(write_asset_file),
        )
        .route("/api/assets/{assetId}/diff", get(diff_asset))
        .route("/api/assets/{assetId}/uninstall", post(uninstall_asset))
        .route("/api/assets/{assetId}/detach", post(detach_asset))
        .with_state(state)
}

async fn create_asset(
    State(state): State<LocalAssetRouterState>,
    Extension(user): Extension<CurrentUser>,
    body: Result<Json<CreateAssetRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<ApiResponse<AssetDetailResponse>>), ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let asset = state.service.create(&user.id, request).await.map_err(asset_api_error)?;
    Ok((StatusCode::CREATED, Json(ApiResponse::ok(asset))))
}

async fn duplicate_asset(
    State(state): State<LocalAssetRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path(source_id): Path<String>,
    body: Result<Json<DuplicateAssetRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<ApiResponse<AssetDetailResponse>>), ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let asset = state
        .service
        .duplicate(&user.id, &source_id, request)
        .await
        .map_err(asset_api_error)?;
    Ok((StatusCode::CREATED, Json(ApiResponse::ok(asset))))
}

async fn get_asset_overlay(
    State(state): State<LocalAssetRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path(asset_id): Path<String>,
) -> Result<Json<ApiResponse<AssetOverlayResponse>>, ApiError> {
    let overlay = state
        .service
        .get_overlay(&user.id, &asset_id)
        .await
        .map_err(asset_api_error)?;
    Ok(Json(ApiResponse::ok(overlay)))
}

async fn configure_asset(
    State(state): State<LocalAssetRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path(asset_id): Path<String>,
    body: Result<Json<ConfigureAssetRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<AssetOverlayResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let overlay = state
        .service
        .configure(&user.id, &asset_id, request)
        .await
        .map_err(asset_api_error)?;
    Ok(Json(ApiResponse::ok(overlay)))
}

async fn validate_asset(
    State(state): State<LocalAssetRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path(asset_id): Path<String>,
    body: Result<Json<AssetRuntimeCommandRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<AssetRuntimeStatusResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let status = state
        .service
        .validate_runtime(&user.id, &asset_id, request)
        .await
        .map_err(asset_api_error)?;
    Ok(Json(ApiResponse::ok(status)))
}

async fn try_run_asset(
    State(state): State<LocalAssetRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path(asset_id): Path<String>,
    body: Result<Json<AssetRuntimeCommandRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<AssetRuntimeStatusResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let status = state
        .service
        .try_run(&user.id, &asset_id, request)
        .await
        .map_err(asset_api_error)?;
    Ok(Json(ApiResponse::ok(status)))
}

async fn activate_asset(
    State(state): State<LocalAssetRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path(asset_id): Path<String>,
    body: Result<Json<AssetRuntimeCommandRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<AssetRuntimeStatusResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let status = state
        .service
        .activate(&user.id, &asset_id, request)
        .await
        .map_err(asset_api_error)?;
    Ok(Json(ApiResponse::ok(status)))
}

async fn deactivate_asset(
    State(state): State<LocalAssetRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path(asset_id): Path<String>,
    body: Result<Json<AssetRuntimeCommandRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<AssetRuntimeStatusResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let status = state
        .service
        .deactivate(&user.id, &asset_id, request)
        .await
        .map_err(asset_api_error)?;
    Ok(Json(ApiResponse::ok(status)))
}

/// 提供明确的资产协作协议版本和能力，禁止客户端猜测旧后端行为。
async fn get_asset_collaboration_protocol() -> Json<ApiResponse<AssetCollaborationProtocolResponse>> {
    Json(ApiResponse::ok(AssetCollaborationProtocolResponse {
        protocol_version: TJUAE_ASSET_PROTOCOL_VERSION.into(),
        build_identifier: env!("CARGO_PKG_VERSION").into(),
        capabilities: vec![
            AssetCollaborationCapability::LocalAssetCatalogV1,
            AssetCollaborationCapability::RemoteMarketV2,
            AssetCollaborationCapability::HubPullRequestPublishV1,
            AssetCollaborationCapability::RuntimeAssetReceiptV1,
            AssetCollaborationCapability::TypedAssetRuntimeV1,
        ],
    }))
}

async fn list_assets(
    State(state): State<LocalAssetRouterState>,
    Extension(user): Extension<CurrentUser>,
    Query(query): Query<ListAssetsQuery>,
) -> Result<Json<ApiResponse<Vec<AssetSummaryResponse>>>, ApiError> {
    let assets = state
        .market
        .list_local_assets(&user.id, &state.service, query.kind, query.scope)
        .await
        .map_err(super::market_assets::market_api_error)?;
    Ok(Json(ApiResponse::ok(assets)))
}

async fn get_asset(
    State(state): State<LocalAssetRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path(asset_id): Path<String>,
    Query(query): Query<GetAssetQuery>,
) -> Result<Json<ApiResponse<AssetDetailResponse>>, ApiError> {
    let asset = state
        .service
        .get_from_source(&user.id, &asset_id, query.source)
        .await
        .map_err(asset_api_error)?;
    Ok(Json(ApiResponse::ok(asset)))
}

async fn read_asset_file(
    State(state): State<LocalAssetRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path(asset_id): Path<String>,
    Query(query): Query<ReadAssetFileQuery>,
) -> Result<Json<ApiResponse<AssetFileResponse>>, ApiError> {
    let file = state
        .service
        .read_file(&user.id, &asset_id, &query.path, query.source)
        .await
        .map_err(asset_api_error)?;
    Ok(Json(ApiResponse::ok(file)))
}

async fn write_asset_file(
    State(state): State<LocalAssetRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path(asset_id): Path<String>,
    body: Result<Json<WriteAssetFileRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<AssetDetailResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let asset = state
        .service
        .write_file(
            &user.id,
            &asset_id,
            &request.path,
            &request.content,
            &request.expected_digest,
        )
        .await
        .map_err(asset_api_error)?;
    Ok(Json(ApiResponse::ok(asset)))
}

async fn diff_asset(
    State(state): State<LocalAssetRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path(asset_id): Path<String>,
) -> Result<Json<ApiResponse<AssetDiffResponse>>, ApiError> {
    let diff = state.service.diff(&user.id, &asset_id).await.map_err(asset_api_error)?;
    Ok(Json(ApiResponse::ok(diff)))
}

async fn detach_asset(
    State(state): State<LocalAssetRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path(asset_id): Path<String>,
) -> Result<Json<ApiResponse<AssetSummaryResponse>>, ApiError> {
    let asset = state
        .service
        .detach(&user.id, &asset_id)
        .await
        .map_err(asset_api_error)?;
    Ok(Json(ApiResponse::ok(asset)))
}

async fn uninstall_asset(
    State(state): State<LocalAssetRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path(asset_id): Path<String>,
    body: Result<Json<AssetOperationRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<AssetOperationResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let operation = state
        .service
        .uninstall(&user.id, &asset_id, &request.idempotency_key)
        .await
        .map_err(asset_api_error)?;
    Ok(Json(ApiResponse::ok(operation)))
}

pub(super) fn asset_api_error(error: AssetError) -> ApiError {
    match error {
        AssetError::NotFound(_) => {
            ApiError::coded(StatusCode::NOT_FOUND, "ASSET_NOT_FOUND", "找不到该本地资产。", None)
        }
        AssetError::ConcurrentModification => ApiError::coded(
            StatusCode::CONFLICT,
            "ASSET_CONCURRENT_MODIFICATION",
            "文件已被其他操作修改，请刷新后重试。",
            None,
        ),
        AssetError::MergeConflict(_) => ApiError::coded(
            StatusCode::CONFLICT,
            "ASSET_MERGE_CONFLICT",
            "本地与远程修改存在重叠或二进制冲突，未写入任何内容。",
            None,
        ),
        AssetError::DestructiveConfirmationRequired => ApiError::coded(
            StatusCode::CONFLICT,
            "ASSET_DESTRUCTIVE_CONFIRMATION_REQUIRED",
            "采用远程内容前需要再次确认。",
            None,
        ),
        AssetError::LocalChanges => ApiError::coded(
            StatusCode::CONFLICT,
            "ASSET_LOCAL_CHANGES",
            "本地资产包含未同步修改，已停止自动覆盖。",
            None,
        ),
        AssetError::MissingBaseSnapshot => ApiError::coded(
            StatusCode::CONFLICT,
            "ASSET_BASE_MISSING",
            "缺少可验证的同步基线，已停止覆盖。",
            None,
        ),
        AssetError::SourceUnavailable(_) => ApiError::coded(
            StatusCode::CONFLICT,
            "ASSET_SOURCE_UNAVAILABLE",
            "所选资产内容尚未缓存，请刷新远程市场后重试。",
            None,
        ),
        AssetError::OverlayNotConfigured => ApiError::coded(
            StatusCode::NOT_FOUND,
            "ASSET_OVERLAY_NOT_CONFIGURED",
            "该资产尚未配置本机运行参数。",
            None,
        ),
        AssetError::UnsafePath(_) | AssetError::InvalidMetadata(_) | AssetError::InvalidState(_) => {
            ApiError::coded(StatusCode::BAD_REQUEST, "ASSET_INVALID_REQUEST", "资产请求无效。", None)
        }
        AssetError::BinaryFile(_) => ApiError::coded(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "ASSET_BINARY_FILE",
            "该文件不是可编辑的文本文件。",
            None,
        ),
        AssetError::FileTooLarge { .. } | AssetError::TotalTooLarge { .. } => ApiError::coded(
            StatusCode::PAYLOAD_TOO_LARGE,
            "ASSET_TOO_LARGE",
            "资产内容超过大小限制。",
            None,
        ),
        AssetError::DigestMismatch { .. } | AssetError::CorruptObject(_) => ApiError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "ASSET_INTEGRITY_FAILED",
            "资产完整性校验失败。",
            None,
        ),
        AssetError::UpstreamMismatch => ApiError::coded(
            StatusCode::CONFLICT,
            "ASSET_UPSTREAM_MISMATCH",
            "本地资产与远程来源不一致。",
            None,
        ),
        AssetError::BundleInvariant(_) => ApiError::coded(
            StatusCode::CONFLICT,
            "ASSET_BUNDLE_INVARIANT",
            "该资产属于原子资产包，必须整体安装、同步或卸载。",
            None,
        ),
        AssetError::RuntimeProjectionUnsupported { code, .. } => ApiError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            code,
            "当前运行时无法安全启用该资产，操作未产生任何更改。",
            None,
        ),
        AssetError::RuntimeProjectionFailed { code, .. } => ApiError::coded(
            StatusCode::INTERNAL_SERVER_ERROR,
            code,
            "资产运行时启用失败，已回滚本次操作。",
            None,
        ),
        AssetError::Database(_) | AssetError::Io(_) | AssetError::Json(_) | AssetError::Crypto(_) => ApiError::coded(
            StatusCode::INTERNAL_SERVER_ERROR,
            "ASSET_INTERNAL",
            "资产操作失败，请查看服务日志。",
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
    async fn route_uses_authenticated_extension_instead_of_query_user_id() {
        let database = tjuaeui_db::init_database_memory().await.unwrap();
        let repo: Arc<dyn tjuaeui_db::IAssetRepository> =
            Arc::new(tjuaeui_db::SqliteAssetRepository::new(database.pool().clone()));
        let temp = tempfile::tempdir().unwrap();
        let state = LocalAssetRouterState {
            service: Arc::new(AssetCatalogService::new(repo, temp.path())),
            market: Arc::new(MarketIndexManager::new(temp.path())),
        };
        let mut rejected = Request::builder()
            .uri("/api/assets?userId=other-user")
            .body(Body::empty())
            .unwrap();
        rejected.extensions_mut().insert(CurrentUser {
            id: "system_default_user".into(),
            username: "system_default_user".into(),
        });
        let response = local_asset_routes(state.clone()).oneshot(rejected).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let mut accepted = Request::builder().uri("/api/assets").body(Body::empty()).unwrap();
        accepted.extensions_mut().insert(CurrentUser {
            id: "system_default_user".into(),
            username: "system_default_user".into(),
        });
        let response = local_asset_routes(state).oneshot(accepted).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn asset_collaboration_protocol_advertises_complete_v1_capabilities() {
        let database = tjuaeui_db::init_database_memory().await.unwrap();
        let repo: Arc<dyn tjuaeui_db::IAssetRepository> =
            Arc::new(tjuaeui_db::SqliteAssetRepository::new(database.pool().clone()));
        let temp = tempfile::tempdir().unwrap();
        let state = LocalAssetRouterState {
            service: Arc::new(AssetCatalogService::new(repo, temp.path())),
            market: Arc::new(MarketIndexManager::new(temp.path())),
        };
        let response = local_asset_routes(state)
            .oneshot(
                Request::builder()
                    .uri("/api/assets/protocol")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let payload: ApiResponse<AssetCollaborationProtocolResponse> = serde_json::from_slice(&body).unwrap();
        let protocol = payload.data.expect("protocol response has data");
        assert_eq!(protocol.protocol_version, TJUAE_ASSET_PROTOCOL_VERSION);
        assert_eq!(protocol.build_identifier, env!("CARGO_PKG_VERSION"));
        assert_eq!(
            protocol.capabilities,
            vec![
                AssetCollaborationCapability::LocalAssetCatalogV1,
                AssetCollaborationCapability::RemoteMarketV2,
                AssetCollaborationCapability::HubPullRequestPublishV1,
                AssetCollaborationCapability::RuntimeAssetReceiptV1,
                AssetCollaborationCapability::TypedAssetRuntimeV1,
            ]
        );
    }
}
