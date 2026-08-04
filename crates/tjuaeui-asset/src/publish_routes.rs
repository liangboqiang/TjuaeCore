#![allow(clippy::disallowed_types)]

use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Json, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use tjuaeui_api_types::{
    ApiResponse, HubAssetPublishPreparation, HubAssetPublishRequest, HubAssetPublishResponse,
    HubPublishConnectionStatus,
};
use tjuaeui_auth::CurrentUser;
use tjuaeui_common::ApiError;

use crate::publish::HubAssetService;
use crate::publish_error::AssetPublishError;

#[derive(Clone)]
pub struct HubRouterState {
    pub asset_service: HubAssetService,
}

/// 构建 TjuaeHub 发布路由。市场浏览、安装与同步由 `/api/market/*`
/// 和 `/api/assets/*` 提供；这里不再承载旧扩展市场。
pub fn hub_routes(state: HubRouterState) -> Router {
    Router::new()
        .route(
            "/api/hub/publish/connection",
            get(get_publish_connection).delete(disconnect_publish_account),
        )
        .route("/api/hub/publish/authorize", post(start_publish_authorization))
        .route("/api/hub/publish/authorize/poll", post(poll_publish_authorization))
        .route("/api/hub/assets/publish-request", post(create_publish_request))
        .route("/api/hub/assets/publish", post(publish_asset))
        .with_state(state)
}

async fn get_publish_connection(
    State(state): State<HubRouterState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<ApiResponse<HubPublishConnectionStatus>>, ApiError> {
    let status = state
        .asset_service
        .publish_connection_status(&user.id)
        .await
        .map_err(hub_api_error)?;
    Ok(Json(ApiResponse::ok(status)))
}

async fn start_publish_authorization(
    State(state): State<HubRouterState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<ApiResponse<HubPublishConnectionStatus>>, ApiError> {
    let status = state
        .asset_service
        .start_publish_authorization(&user.id)
        .await
        .map_err(hub_api_error)?;
    Ok(Json(ApiResponse::ok(status)))
}

async fn poll_publish_authorization(
    State(state): State<HubRouterState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<ApiResponse<HubPublishConnectionStatus>>, ApiError> {
    let status = state
        .asset_service
        .poll_publish_authorization(&user.id)
        .await
        .map_err(hub_api_error)?;
    Ok(Json(ApiResponse::ok(status)))
}

async fn disconnect_publish_account(
    State(state): State<HubRouterState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<ApiResponse<HubPublishConnectionStatus>>, ApiError> {
    let status = state
        .asset_service
        .disconnect_publish_account(&user.id)
        .await
        .map_err(hub_api_error)?;
    Ok(Json(ApiResponse::ok(status)))
}

async fn create_publish_request(
    State(state): State<HubRouterState>,
    Extension(user): Extension<CurrentUser>,
    body: Result<Json<HubAssetPublishRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<HubAssetPublishPreparation>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let response = state
        .asset_service
        .publish_request(&user.id, &request)
        .await
        .map_err(hub_api_error)?;
    Ok(Json(ApiResponse::ok(response)))
}

async fn publish_asset(
    State(state): State<HubRouterState>,
    Extension(user): Extension<CurrentUser>,
    body: Result<Json<HubAssetPublishRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<HubAssetPublishResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let response = state
        .asset_service
        .publish(&user.id, &request)
        .await
        .map_err(hub_api_error)?;
    Ok(Json(ApiResponse::ok(response)))
}

fn hub_api_error(error: AssetPublishError) -> ApiError {
    let status = match &error {
        AssetPublishError::NotFound(_) => StatusCode::NOT_FOUND,
        AssetPublishError::HubPublishConflict(_) => StatusCode::CONFLICT,
        AssetPublishError::HubNetwork(_) | AssetPublishError::HubPublishFailed(_) => StatusCode::BAD_GATEWAY,
        AssetPublishError::HubPackageTooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE,
        AssetPublishError::HubPublishPrerequisite(_) => StatusCode::PRECONDITION_FAILED,
        AssetPublishError::HubIntegrity(_) => StatusCode::UNPROCESSABLE_ENTITY,
        AssetPublishError::Io(_) | AssetPublishError::Json(_) | AssetPublishError::Internal(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
        _ => StatusCode::BAD_REQUEST,
    };
    let code = extension_error_code(&error);
    let message = match &error {
        AssetPublishError::HubNetwork(_) => "无法访问 TjuaeHub，请检查网络或代理设置。".to_owned(),
        AssetPublishError::HubPublishPrerequisite(code) => match code.as_str() {
            "GITHUB_APP_NOT_CONFIGURED" => "发布服务尚未配置 GitHub App。".to_owned(),
            "GITHUB_NOT_CONNECTED" | "GITHUB_AUTH_EXPIRED" | "GITHUB_AUTH_REVOKED" => {
                "GitHub 连接已失效，请重新授权。".to_owned()
            }
            "GITHUB_INSUFFICIENT_PERMISSIONS" => "当前 GitHub 授权缺少发布所需权限。".to_owned(),
            _ => "发布前置条件未满足，请检查 GitHub 连接状态。".to_owned(),
        },
        AssetPublishError::HubPublishFailed(_) => "已停止发布，未对 main 分支写入任何内容。".to_owned(),
        AssetPublishError::HubPublishConflict(_) => "该幂等键已用于另一份发布内容，请重新发起发布。".to_owned(),
        AssetPublishError::Io(_) | AssetPublishError::Json(_) | AssetPublishError::Internal(_) => {
            "Hub 操作失败，请查看服务日志。".to_owned()
        }
        _ => error.to_string(),
    };
    ApiError::coded(status, code, message, None)
}

fn extension_error_code(error: &AssetPublishError) -> &'static str {
    match error {
        AssetPublishError::HubNetwork(_) => "HUB_NETWORK_ERROR",
        AssetPublishError::HubIntegrity(_) => "HUB_INTEGRITY_FAILED",
        AssetPublishError::HubPackageTooLarge { .. } => "HUB_PACKAGE_TOO_LARGE",
        AssetPublishError::HubPublishPrerequisite(_) => "HUB_PUBLISH_PREREQUISITE",
        AssetPublishError::HubPublishFailed(_) => "HUB_PUBLISH_FAILED",
        AssetPublishError::HubPublishConflict(_) => "HUB_PUBLISH_CONFLICT",
        AssetPublishError::AssetSanitization(_) => "HUB_ASSET_UNSAFE",
        AssetPublishError::NotFound(_) => "HUB_ASSET_NOT_FOUND",
        AssetPublishError::InvalidRequest(_) | AssetPublishError::InvalidVersion { .. } => "HUB_INVALID_REQUEST",
        _ => "HUB_OPERATION_FAILED",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::DisabledHubAssetPort;
    use std::sync::Arc;

    fn make_state() -> HubRouterState {
        HubRouterState {
            asset_service: HubAssetService::new(Arc::new(DisabledHubAssetPort)),
        }
    }

    #[test]
    fn hub_routes_builds_publish_only_router() {
        let _ = hub_routes(make_state());
    }

    #[test]
    fn errors_have_stable_codes() {
        let error = hub_api_error(AssetPublishError::InvalidRequest("bad request".into()));
        assert_eq!(error.error_code(), "HUB_INVALID_REQUEST");
        assert_eq!(error.status_code(), StatusCode::BAD_REQUEST);

        let conflict = hub_api_error(AssetPublishError::HubPublishConflict(
            "GITHUB_IDEMPOTENCY_KEY_REUSED".into(),
        ));
        assert_eq!(conflict.error_code(), "HUB_PUBLISH_CONFLICT");
        assert_eq!(conflict.status_code(), StatusCode::CONFLICT);
    }
}
