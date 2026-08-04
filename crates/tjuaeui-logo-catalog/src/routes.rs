#![allow(clippy::disallowed_types)]

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use axum::routing::get;

use tjuaeui_common::ApiError;

use crate::error::LogoAssetError;
use crate::state::LogoCatalogRouterState;

const CACHE_CONTROL_VALUE: &str = "public, max-age=31536000, immutable";

/// Build the public `/api/assets/*` router.
pub fn logo_asset_routes(state: LogoCatalogRouterState) -> Router {
    Router::new()
        .route("/api/assets/logos/{*asset_path}", get(get_logo_asset))
        .with_state(state)
}

impl From<LogoAssetError> for ApiError {
    fn from(error: LogoAssetError) -> Self {
        match error {
            LogoAssetError::NotFound(message) => Self::NotFound(message),
            LogoAssetError::Forbidden(message) => Self::Forbidden(message),
            LogoAssetError::Internal(message) => Self::Internal(message),
        }
    }
}

async fn get_logo_asset(
    State(state): State<LogoCatalogRouterState>,
    Path(asset_path): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let asset = state.service.get_logo(&asset_path)?;

    if state
        .service
        .etag_matches(headers.get(header::IF_NONE_MATCH), &asset.etag)
    {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::CACHE_CONTROL, CACHE_CONTROL_VALUE)
            .header(header::ETAG, asset.etag)
            .body(Body::empty())
            .map_err(|error| ApiError::Internal(error.to_string()));
    }

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, asset.content_type)
        .header(header::CACHE_CONTROL, CACHE_CONTROL_VALUE)
        .header(header::ETAG, asset.etag)
        .body(Body::from(asset.bytes))
        .map_err(|error| ApiError::Internal(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn get_logo_asset_serves_embedded_logo() {
        let router = logo_asset_routes(LogoCatalogRouterState::default());
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/assets/logos/ai-major/claude.svg")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "image/svg+xml");
        assert_eq!(response.headers()[header::CACHE_CONTROL], CACHE_CONTROL_VALUE);
        assert!(response.headers().contains_key(header::ETAG));
        assert!(!response.into_body().collect().await.unwrap().to_bytes().is_empty());
    }

    #[tokio::test]
    async fn get_brand_logo_asset_serves_canonical_svg() {
        let router = logo_asset_routes(LogoCatalogRouterState::default());
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/assets/logos/brand/tjuae.svg")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "image/svg+xml");
        assert_eq!(response.headers()[header::CACHE_CONTROL], CACHE_CONTROL_VALUE);
    }

    #[tokio::test]
    async fn get_logo_asset_returns_not_modified_for_matching_etag() {
        let router = logo_asset_routes(LogoCatalogRouterState::default());
        let first = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/assets/logos/ai-major/claude.svg")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let etag = first.headers()[header::ETAG].clone();

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/assets/logos/ai-major/claude.svg")
                    .header(header::IF_NONE_MATCH, etag)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(response.headers()[header::CACHE_CONTROL], CACHE_CONTROL_VALUE);
        assert_eq!(response.into_body().collect().await.unwrap().to_bytes().len(), 0);
    }

    #[tokio::test]
    async fn get_logo_asset_rejects_traversal() {
        let router = logo_asset_routes(LogoCatalogRouterState::default());
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/assets/logos/%2E%2E%2Fbrand%2Ftjuae.svg")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn get_logo_asset_returns_not_found_for_missing_file() {
        let router = logo_asset_routes(LogoCatalogRouterState::default());
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/assets/logos/ai-major/missing.svg")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
