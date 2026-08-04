#![allow(clippy::disallowed_types)]

use std::collections::HashMap;
use std::path::Path as FsPath;

use axum::Router;
use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Json, Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use axum::routing::{get, post};

use serde_json::json;
use tjuaeui_api_types::{
    ApiResponse, DisableExtensionRequest, EnableExtensionRequest, ExtensionSummaryResponse, GetI18nRequest,
    GetPermissionsRequest, GetRiskLevelRequest, PermissionDetailResponse, PermissionSummaryResponse,
};
use tjuaeui_common::{ApiError, now_ms};

use crate::asset_paths::normalize_relative_asset_path;
use crate::error::ExtensionError;
use crate::permission::{build_permission_summary, calculate_risk_level};
use crate::registry::ExtensionRegistry;

impl From<ExtensionError> for ApiError {
    fn from(err: ExtensionError) -> Self {
        match err {
            ExtensionError::ManifestValidation(msg) => ApiError::BadRequest(msg),
            ExtensionError::ReservedNamePrefix { .. } => ApiError::BadRequest(err.to_string()),
            ExtensionError::InvalidVersion { .. } => ApiError::BadRequest(err.to_string()),
            ExtensionError::UndefinedEnvVariable(var) => ApiError::BadRequest(format!("环境变量未定义：{var}")),
            ExtensionError::FileReferenceNotFound(path) => ApiError::NotFound(format!("找不到文件引用：{path}")),
            ExtensionError::PathTraversal(path) => ApiError::BadRequest(format!("检测到路径遍历：{path}")),
            ExtensionError::EngineIncompatible { .. } => ApiError::BadRequest(err.to_string()),
            ExtensionError::ApiVersionIncompatible { .. } => ApiError::BadRequest(err.to_string()),
            ExtensionError::InvalidWebuiRouteNamespace { .. } => ApiError::BadRequest(err.to_string()),
            ExtensionError::ReservedWebuiRoute { .. } => ApiError::BadRequest(err.to_string()),
            ExtensionError::ThemeCssNotFound(path) => ApiError::NotFound(format!("找不到主题 CSS：{path}")),
            ExtensionError::ResolutionFailed { .. } => ApiError::Internal(err.to_string()),
            ExtensionError::NotFound(name) => ApiError::NotFound(format!("找不到扩展：{name}")),
            ExtensionError::HubNetwork(message) => ApiError::coded(
                StatusCode::BAD_GATEWAY,
                "HUB_NETWORK_ERROR",
                format!("访问 TjuaeHub 失败：{message}"),
                None,
            ),
            ExtensionError::HubIntegrity(message) => ApiError::coded(
                StatusCode::UNPROCESSABLE_ENTITY,
                "HUB_INTEGRITY_CHECK_FAILED",
                format!("Hub 资产包完整性校验失败：{message}"),
                None,
            ),
            ExtensionError::HubPackageTooLarge { actual, limit } => ApiError::coded(
                StatusCode::PAYLOAD_TOO_LARGE,
                "HUB_PACKAGE_TOO_LARGE",
                format!("Hub 数据体积为 {actual} 字节，超过上限 {limit} 字节。"),
                Some(json!({ "actual": actual, "limit": limit })),
            ),
            ExtensionError::HubPublishPrerequisite(message) => ApiError::coded(
                StatusCode::PRECONDITION_FAILED,
                "HUB_PUBLISH_PREREQUISITE_FAILED",
                format!("远程发布前置条件未满足：{message}"),
                None,
            ),
            ExtensionError::HubPublishFailed(message) => ApiError::coded(
                StatusCode::BAD_GATEWAY,
                "HUB_PUBLISH_FAILED",
                format!("远程发布失败：{message}"),
                None,
            ),
            ExtensionError::HubPublishConflict(message) => ApiError::coded(
                StatusCode::CONFLICT,
                "HUB_PUBLISH_CONFLICT",
                format!("远程发布幂等冲突：{message}"),
                None,
            ),
            ExtensionError::AssetSanitization(message) => ApiError::coded(
                StatusCode::UNPROCESSABLE_ENTITY,
                "ASSET_SANITIZATION_FAILED",
                format!("资产导出被安全策略拒绝：{message}"),
                None,
            ),
            ExtensionError::StatePersistence(msg) => ApiError::Internal(msg),
            ExtensionError::Db(tjuaeui_db::DbError::NotFound(msg)) => ApiError::NotFound(msg),
            ExtensionError::Db(tjuaeui_db::DbError::Conflict(msg)) => ApiError::Conflict(msg),
            ExtensionError::Db(err) => ApiError::Internal(err.to_string()),
            ExtensionError::InvalidRequest(msg) => ApiError::BadRequest(msg),
            ExtensionError::Internal(msg) => ApiError::Internal(msg),
            ExtensionError::Io(e) => ApiError::Internal(e.to_string()),
            ExtensionError::JsonParse(e) => ApiError::BadRequest(e.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// Router state
// ---------------------------------------------------------------------------

/// Shared state for extension route handlers.
#[derive(Clone)]
pub struct ExtensionRouterState {
    pub registry: ExtensionRegistry,
}

// ---------------------------------------------------------------------------
// Router builder
// ---------------------------------------------------------------------------

/// Build the extension router with all `/api/extensions/*` routes.
///
/// Includes query routes and management routes.
/// All routes require authentication (applied by the caller).
pub fn extension_routes(state: ExtensionRouterState) -> Router {
    Router::new()
        // Query routes
        .route("/api/extensions", get(get_loaded_extensions))
        .route("/api/extensions/themes", get(get_themes))
        .route("/api/extensions/channel-plugins", get(get_channel_plugins))
        .route("/api/extensions/settings-tabs", get(get_settings_tabs))
        .route(
            "/api/extensions/{extension_name}/assets/{*asset_path}",
            get(get_extension_asset),
        )
        .route("/api/extensions/webui", get(get_webui))
        .route("/api/extensions/agent-activity", get(get_agent_activity))
        // Query routes with body
        .route("/api/extensions/i18n", post(get_i18n))
        .route("/api/extensions/permissions", post(get_permissions))
        .route("/api/extensions/risk-level", post(get_risk_level))
        // Management routes
        .route("/api/extensions/enable", post(enable_extension))
        .route("/api/extensions/disable", post(disable_extension))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Query handlers
// ---------------------------------------------------------------------------

/// `GET /api/extensions` — list all loaded extensions.
async fn get_loaded_extensions(
    State(state): State<ExtensionRouterState>,
) -> Result<Json<ApiResponse<Vec<ExtensionSummaryResponse>>>, ApiError> {
    let summaries = state.registry.get_loaded_extensions().await;
    let resp: Vec<ExtensionSummaryResponse> = summaries
        .into_iter()
        .map(|s| {
            let source_str = serde_json::to_value(s.source)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| "local".to_string());
            ExtensionSummaryResponse {
                name: s.name,
                version: s.version,
                display_name: s.display_name,
                description: s.description,
                enabled: s.enabled,
                source: source_str,
            }
        })
        .collect();
    Ok(Json(ApiResponse::ok(resp)))
}

/// `GET /api/extensions/themes` — get all resolved themes.
async fn get_themes(
    State(state): State<ExtensionRouterState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, ApiError> {
    let themes = state.registry.get_themes().await;
    let timestamp = now_ms();
    let value = serde_json::Value::Array(
        themes
            .into_iter()
            .map(|theme| {
                serde_json::json!({
                    "id": format!("ext-{}-{}", theme.extension_name, theme.id),
                    "name": format!("{} ({})", theme.name, theme.extension_name),
                    "cover": theme.cover_image,
                    "css": theme.css_content,
                    "is_preset": true,
                    "created_at": timestamp,
                    "updated_at": timestamp,
                })
            })
            .collect(),
    );
    Ok(Json(ApiResponse::ok(value)))
}

/// `GET /api/extensions/channel-plugins` — get all resolved channel plugins.
async fn get_channel_plugins(
    State(state): State<ExtensionRouterState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, ApiError> {
    let plugins = state.registry.get_channel_plugins().await;
    let value = serde_json::Value::Array(
        plugins
            .into_iter()
            .map(|plugin| {
                serde_json::json!({
                    "id": plugin.id,
                    "type": plugin.id,
                    "name": plugin.name,
                    "platform": plugin.platform,
                    "entryPoint": plugin.entry_point,
                    "enabled": true,
                    "connected": false,
                    "active_users": 0,
                    "has_token": false,
                    "is_extension": true,
                    "extension_meta": {
                        "credentialFields": plugin.credential_fields,
                        "configFields": plugin.config_fields,
                        "description": plugin.description,
                        "extensionName": plugin.extension_name,
                        "icon": plugin.icon,
                    },
                })
            })
            .collect(),
    );
    Ok(Json(ApiResponse::ok(value)))
}

/// `GET /api/extensions/settings-tabs` — get all resolved settings tabs.
async fn get_settings_tabs(
    State(state): State<ExtensionRouterState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, ApiError> {
    let tabs = state.registry.get_settings_tabs().await;
    let value = serde_json::to_value(&tabs).unwrap_or_default();
    Ok(Json(ApiResponse::ok(value)))
}

/// `GET /api/extensions/{extension_name}/assets/{*asset_path}` — serve an
/// extension asset under the trusted extension root.
async fn get_extension_asset(
    State(state): State<ExtensionRouterState>,
    Path((extension_name, asset_path)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let ext = state
        .registry
        .get_extension_by_name(&extension_name)
        .await
        .ok_or_else(|| ApiError::NotFound(format!("找不到扩展：{extension_name}")))?;

    let canonical_root = tokio::fs::canonicalize(&ext.directory)
        .await
        .map_err(map_asset_lookup_error)?;

    let relative_path = normalize_relative_asset_path(&asset_path).ok_or_else(|| {
        ApiError::Forbidden(format!(
            "Asset path escapes extension root: {extension_name}/{asset_path}"
        ))
    })?;

    let requested_path = canonical_root.join(&relative_path);
    let canonical_asset = tokio::fs::canonicalize(&requested_path)
        .await
        .map_err(map_asset_lookup_error)?;

    if !canonical_asset.starts_with(&canonical_root) {
        return Err(ApiError::Forbidden(format!(
            "Asset path escapes extension root: {}",
            canonical_asset.display()
        )));
    }

    let bytes = tokio::fs::read(&canonical_asset)
        .await
        .map_err(map_asset_lookup_error)?;

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type_for_path(&canonical_asset))
        .header(header::CACHE_CONTROL, "public, max-age=3600")
        .body(Body::from(bytes))
        .map_err(|err| ApiError::Internal(err.to_string()))
}

/// `GET /api/extensions/webui` — get all WebUI contributions.
async fn get_webui(
    State(state): State<ExtensionRouterState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, ApiError> {
    let webui = state.registry.get_webui_contributions().await;
    let value = serde_json::to_value(&webui).unwrap_or_default();
    Ok(Json(ApiResponse::ok(value)))
}

/// `GET /api/extensions/agent-activity` — get agent activity snapshot.
///
/// Returns an empty object as a placeholder; real implementation will
/// integrate with the agent subsystem's activity tracking.
async fn get_agent_activity(
    State(_state): State<ExtensionRouterState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, ApiError> {
    // Agent activity snapshot is a cross-module concern;
    // return an empty object for now.
    Ok(Json(ApiResponse::ok(serde_json::json!({}))))
}

/// `POST /api/extensions/i18n` — get i18n data for a locale.
async fn get_i18n(
    State(state): State<ExtensionRouterState>,
    body: Result<Json<GetI18nRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<HashMap<String, HashMap<String, String>>>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let data = state.registry.get_i18n_for_locale(&req.locale).await;
    Ok(Json(ApiResponse::ok(data)))
}

/// `POST /api/extensions/permissions` — get permission summary for an extension.
async fn get_permissions(
    State(state): State<ExtensionRouterState>,
    body: Result<Json<GetPermissionsRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<PermissionSummaryResponse>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;

    let ext = state
        .registry
        .get_extension_by_name(&req.name)
        .await
        .ok_or_else(|| ApiError::NotFound(format!("找不到扩展：{}", req.name)))?;

    let permissions = ext.manifest.permissions.clone().unwrap_or_default();
    let summary = build_permission_summary(&permissions);
    let risk_level = calculate_risk_level(&permissions);

    let details: Vec<PermissionDetailResponse> = summary
        .details
        .into_iter()
        .map(|d| PermissionDetailResponse {
            permission: d.permission,
            level: enum_to_string(&d.level),
            description: d.description,
        })
        .collect();

    let resp = PermissionSummaryResponse {
        permissions: serde_json::to_value(&permissions).unwrap_or_default(),
        risk_level: enum_to_string(&risk_level),
        details,
    };

    Ok(Json(ApiResponse::ok(resp)))
}

/// `POST /api/extensions/risk-level` — get risk level for an extension.
async fn get_risk_level(
    State(state): State<ExtensionRouterState>,
    body: Result<Json<GetRiskLevelRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<serde_json::Value>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;

    let ext = state
        .registry
        .get_extension_by_name(&req.name)
        .await
        .ok_or_else(|| ApiError::NotFound(format!("找不到扩展：{}", req.name)))?;

    let permissions = ext.manifest.permissions.clone().unwrap_or_default();
    let risk_level = calculate_risk_level(&permissions);

    Ok(Json(ApiResponse::ok(
        serde_json::json!({ "riskLevel": enum_to_string(&risk_level) }),
    )))
}

// ---------------------------------------------------------------------------
// Management handlers
// ---------------------------------------------------------------------------

/// `POST /api/extensions/enable` — enable an extension.
async fn enable_extension(
    State(state): State<ExtensionRouterState>,
    body: Result<Json<EnableExtensionRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    state.registry.enable_extension(&req.name).await?;
    Ok(Json(ApiResponse::success()))
}

/// `POST /api/extensions/disable` — disable an extension.
async fn disable_extension(
    State(state): State<ExtensionRouterState>,
    body: Result<Json<DisableExtensionRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    state
        .registry
        .disable_extension(&req.name, req.reason.as_deref())
        .await?;
    Ok(Json(ApiResponse::success()))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Serialize a serde enum to its JSON string representation.
fn enum_to_string<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

fn content_type_for_path(path: &FsPath) -> HeaderValue {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    HeaderValue::from_str(mime.as_ref()).unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"))
}

fn map_asset_lookup_error(error: std::io::Error) -> ApiError {
    match error.kind() {
        std::io::ErrorKind::NotFound => ApiError::NotFound("找不到扩展资源".into()),
        _ => ApiError::Internal(error.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tower::ServiceExt;

    use crate::state::ExtensionStateStore;
    use crate::{ExtensionSource, ScanPath};
    use tjuaeui_realtime::BroadcastEventBus;

    fn make_state() -> ExtensionRouterState {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = ExtensionStateStore::new(tmp.path().join("states.json"));
        let bus = Arc::new(BroadcastEventBus::new(64));
        std::mem::forget(tmp);
        let registry = ExtensionRegistry::new(store, bus, "1.0.0".into());
        ExtensionRouterState { registry }
    }

    #[test]
    fn extension_routes_builds_router() {
        let state = make_state();
        let _router = extension_routes(state);
    }

    #[test]
    fn extension_path_traversal_maps_to_bad_request() {
        let api_err = ApiError::from(ExtensionError::PathTraversal("../secret".into()));
        assert!(matches!(api_err, ApiError::BadRequest(_)));
    }

    #[test]
    fn extension_manifest_validation_maps_to_bad_request() {
        let api_err = ApiError::from(ExtensionError::ManifestValidation("test".into()));
        assert!(matches!(api_err, ApiError::BadRequest(_)));
    }

    #[test]
    fn extension_file_reference_not_found_maps_to_not_found() {
        let api_err = ApiError::from(ExtensionError::FileReferenceNotFound("missing.md".into()));
        assert!(matches!(api_err, ApiError::NotFound(_)));
    }

    #[test]
    fn extension_io_error_maps_to_internal() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let api_err = ApiError::from(ExtensionError::Io(io_err));
        assert!(matches!(api_err, ApiError::Internal(_)));
    }

    async fn make_router_with_extension() -> (Router, tempfile::TempDir, PathBuf) {
        let tmp = tempfile::TempDir::new().unwrap();
        let ext_root = tmp.path().join("extensions");
        let ext_dir = ext_root.join("hello");
        std::fs::create_dir_all(ext_dir.join("settings")).unwrap();
        std::fs::write(
            ext_dir.join("tjuae-extension.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "name": "hello",
                "version": "1.0.0"
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(ext_dir.join("settings").join("index.html"), "<h1>Hello</h1>").unwrap();

        let store = ExtensionStateStore::new(tmp.path().join("states.json"));
        let bus = Arc::new(BroadcastEventBus::new(64));
        let registry = ExtensionRegistry::new(store, bus, "1.0.0".into());
        registry
            .initialize_with_scan_paths(vec![ScanPath {
                path: ext_root,
                source: ExtensionSource::Env,
            }])
            .await
            .unwrap();

        (extension_routes(ExtensionRouterState { registry }), tmp, ext_dir)
    }

    #[tokio::test]
    async fn get_extension_asset_serves_local_file() {
        let (router, _tmp, _ext_dir) = make_router_with_extension().await;
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/extensions/hello/assets/settings/index.html")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "public, max-age=3600");
        assert_eq!(response.headers()[header::CONTENT_TYPE], "text/html");
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(bytes, "<h1>Hello</h1>");
    }

    #[tokio::test]
    async fn get_extension_asset_rejects_traversal() {
        let (router, _tmp, _ext_dir) = make_router_with_extension().await;
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/extensions/hello/assets/%2E%2E%2Fsecret.txt")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn get_extension_asset_returns_not_found_for_missing_file() {
        let (router, _tmp, _ext_dir) = make_router_with_extension().await;
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/extensions/hello/assets/settings/missing.html")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_extension_asset_returns_not_found_for_unknown_extension() {
        let (router, _tmp, _ext_dir) = make_router_with_extension().await;
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/extensions/unknown/assets/settings/index.html")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
