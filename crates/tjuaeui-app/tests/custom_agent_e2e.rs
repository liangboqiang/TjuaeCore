//! E2E contract for the removed custom-engine CRUD surface.
//!
//! User-created engine adapters are Core assets. The old custom-agent routes
//! must stay absent so no caller can bypass Definition, Overlay, validation,
//! try-run receipts or user-scoped runtime projection.

mod common;

use axum::http::StatusCode;
use serde_json::json;
use tower::ServiceExt;

use common::{body_json, build_app, get_with_token, json_with_token, setup_and_login};

async fn agent_count(services: &tjuaeui_app::AppServices) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM agent_metadata")
        .fetch_one(services.database.pool())
        .await
        .expect("count agent metadata")
}

#[tokio::test]
async fn removed_custom_engine_crud_routes_are_not_available_and_do_not_mutate_runtime_rows() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;
    let before = agent_count(&services).await;

    for (method, path, body) in [
        (
            "POST",
            "/api/engines/custom",
            json!({"name": "legacy", "command": "legacy-acp"}),
        ),
        (
            "PUT",
            "/api/engines/custom/legacy-id",
            json!({"name": "legacy", "command": "legacy-acp"}),
        ),
        ("DELETE", "/api/engines/custom/legacy-id", json!(null)),
    ] {
        let response = app
            .clone()
            .oneshot(json_with_token(method, path, body, &token, &csrf))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
    }

    assert_eq!(agent_count(&services).await, before);
}

#[tokio::test]
async fn engine_management_remains_a_read_only_runtime_projection() {
    let (mut app, services) = build_app().await;
    let (token, _) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let response = app
        .oneshot(get_with_token("/api/engines/management", &token))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["success"], true);
    assert!(body["data"].is_array());
}

#[tokio::test]
async fn typed_asset_protocol_is_the_advertised_engine_authoring_path() {
    let (mut app, services) = build_app().await;
    let (token, _) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let response = app
        .oneshot(get_with_token("/api/assets/protocol", &token))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert!(
        body["data"]["capabilities"]
            .as_array()
            .is_some_and(|capabilities| capabilities.iter().any(|value| value == "typedAssetRuntimeV1"))
    );
}
