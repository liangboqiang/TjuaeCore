//! Fail-closed protocol guard for asset collaboration routes.

use axum::Json;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use tjuaeui_api_types::ErrorResponse;
use tjuaeui_asset::TJUAE_ASSET_PROTOCOL_VERSION;
use tjuaeui_common::ApiErrorLogContext;

pub(super) const TJUAE_ASSET_PROTOCOL_HEADER: &str = "x-tjuae-asset-protocol";

const ASSET_PROTOCOL_REQUIRED_CODE: &str = "ASSET_PROTOCOL_REQUIRED";
const ASSET_PROTOCOL_MISMATCH_CODE: &str = "ASSET_PROTOCOL_MISMATCH";

/// Require clients to identify the asset collaboration protocol on every
/// collaboration request. The handshake itself stays open so a client can
/// discover compatibility before entering any asset workflow.
pub(super) async fn require_asset_protocol(request: Request, next: Next) -> Response {
    let path = request.uri().path();
    if !is_guarded_asset_path(path) || path == "/api/assets/protocol" {
        return next.run(request).await;
    }

    let Some(value) = request.headers().get(TJUAE_ASSET_PROTOCOL_HEADER) else {
        return protocol_error(
            "资产协作请求缺少协议版本，请升级 TjuaeUI 后重试。",
            ASSET_PROTOCOL_REQUIRED_CODE,
            None,
        );
    };

    if value.as_bytes() != TJUAE_ASSET_PROTOCOL_VERSION.as_bytes() {
        return protocol_error(
            "TjuaeUI 与 TjuaeCore 的资产协作协议不兼容，请安装匹配版本。",
            ASSET_PROTOCOL_MISMATCH_CODE,
            value.to_str().ok(),
        );
    }

    next.run(request).await
}

fn is_guarded_asset_path(path: &str) -> bool {
    (path_is_at_or_below(path, "/api/assets") && !path_is_at_or_below(path, "/api/assets/logos"))
        || path_is_at_or_below(path, "/api/market")
        || path_is_at_or_below(path, "/api/hub/assets")
}

fn path_is_at_or_below(path: &str, root: &str) -> bool {
    path == root || path.strip_prefix(root).is_some_and(|suffix| suffix.starts_with('/'))
}

fn protocol_error(error: &'static str, code: &'static str, actual: Option<&str>) -> Response {
    let mut details = serde_json::json!({
        "header": TJUAE_ASSET_PROTOCOL_HEADER,
        "expected": TJUAE_ASSET_PROTOCOL_VERSION,
    });
    if let Some(actual) = actual {
        details["actual"] = serde_json::Value::String(actual.to_owned());
    }

    let mut response = (
        StatusCode::UPGRADE_REQUIRED,
        Json(ErrorResponse::new_with_details(error, code, details)),
    )
        .into_response();
    response.extensions_mut().insert(ApiErrorLogContext {
        code,
        message: error.to_owned(),
    });
    response
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use axum::middleware;
    use axum::routing::get;
    use tjuaeui_api_types::ErrorResponse;
    use tjuaeui_asset::TJUAE_ASSET_PROTOCOL_VERSION;
    use tower::ServiceExt;

    use super::{TJUAE_ASSET_PROTOCOL_HEADER, require_asset_protocol};

    fn guarded_test_router() -> Router {
        Router::new()
            .route("/api/assets/protocol", get(|| async { StatusCode::OK }))
            .route("/api/assets", get(|| async { StatusCode::OK }))
            .route("/api/market/assets", get(|| async { StatusCode::OK }))
            .route("/api/hub/assets/publish", get(|| async { StatusCode::OK }))
            .route("/api/hub/publish/connection", get(|| async { StatusCode::OK }))
            .route("/api/assets/logos/example.svg", get(|| async { StatusCode::OK }))
            .route("/api/assets-logo", get(|| async { StatusCode::OK }))
            .layer(middleware::from_fn(require_asset_protocol))
    }

    async fn request(path: &str, protocol: Option<&str>) -> axum::response::Response {
        let mut request = Request::builder().uri(path);
        if let Some(protocol) = protocol {
            request = request.header(TJUAE_ASSET_PROTOCOL_HEADER, protocol);
        }
        guarded_test_router()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    async fn error_body(response: axum::response::Response) -> ErrorResponse {
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn protocol_handshake_does_not_require_protocol_header() {
        assert_eq!(request("/api/assets/protocol", None).await.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn missing_protocol_header_fails_closed_with_stable_error() {
        let response = request("/api/assets", None).await;
        assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);

        let body = error_body(response).await;
        assert_eq!(body.code, "ASSET_PROTOCOL_REQUIRED");
        assert_eq!(body.details.unwrap()["expected"], TJUAE_ASSET_PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn wrong_protocol_header_fails_closed_with_stable_error() {
        let response = request("/api/market/assets", Some("0.9.0")).await;
        assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);

        let body = error_body(response).await;
        assert_eq!(body.code, "ASSET_PROTOCOL_MISMATCH");
        let details = body.details.unwrap();
        assert_eq!(details["expected"], TJUAE_ASSET_PROTOCOL_VERSION);
        assert_eq!(details["actual"], "0.9.0");
    }

    #[tokio::test]
    async fn matching_protocol_header_allows_every_asset_collaboration_namespace() {
        for path in ["/api/assets", "/api/market/assets", "/api/hub/assets/publish"] {
            assert_eq!(
                request(path, Some(TJUAE_ASSET_PROTOCOL_VERSION)).await.status(),
                StatusCode::OK,
                "{path} should accept the matching protocol"
            );
        }
    }

    #[tokio::test]
    async fn similarly_named_and_non_asset_hub_routes_remain_unaffected() {
        for path in [
            "/api/assets/logos/example.svg",
            "/api/assets-logo",
            "/api/hub/publish/connection",
        ] {
            assert_eq!(
                request(path, None).await.status(),
                StatusCode::OK,
                "{path} should not require the asset protocol"
            );
        }
    }
}
