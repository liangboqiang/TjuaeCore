//! Final MCP HTTP contract tests.
//!
//! MCP definitions are four-kind assets. The MCP API exposes active Core
//! projections for runtime consumption, but all legacy direct CRUD, toggle,
//! and batch-import writes are deliberately absent.

mod common;

use axum::http::StatusCode;
use serde_json::json;
use tower::ServiceExt;

use common::{body_json, build_app, delete_with_token, get_with_token, json_with_token, setup_and_login};

#[tokio::test]
async fn unauthenticated_access_is_rejected() {
    let (app, _) = build_app().await;
    let resp = app.oneshot(common::get_request("/api/mcp/servers")).await.unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn list_servers_is_read_only_and_empty_without_active_bindings() {
    let (mut app, services) = build_app().await;
    let (token, _) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let resp = app.oneshot(get_with_token("/api/mcp/servers", &token)).await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["success"], true);
    assert_eq!(body["data"], json!([]));
}

#[tokio::test]
async fn get_unknown_projection_returns_not_found() {
    let (mut app, services) = build_app().await;
    let (token, _) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let resp = app
        .oneshot(get_with_token("/api/mcp/servers/missing", &token))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn legacy_create_and_batch_import_routes_are_removed() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    for (path, body) in [
        (
            "/api/mcp/servers",
            json!({
                "name": "legacy-mcp",
                "transport": {"type": "stdio", "command": "npx"}
            }),
        ),
        (
            "/api/mcp/servers/import",
            json!({
                "servers": [{
                    "name": "legacy-mcp",
                    "transport": {"type": "stdio", "command": "npx"}
                }]
            }),
        ),
    ] {
        let resp = app
            .clone()
            .oneshot(json_with_token("POST", path, body, &token, &csrf))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED, "{path}");
    }

    let list = app.oneshot(get_with_token("/api/mcp/servers", &token)).await.unwrap();
    assert_eq!(body_json(list).await["data"], json!([]));
}

#[tokio::test]
async fn legacy_update_toggle_and_delete_routes_are_removed() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let update = app
        .clone()
        .oneshot(json_with_token(
            "PUT",
            "/api/mcp/servers/legacy-mcp",
            json!({"description": "must not write"}),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(update.status(), StatusCode::METHOD_NOT_ALLOWED);

    let toggle = app
        .clone()
        .oneshot(json_with_token(
            "POST",
            "/api/mcp/servers/legacy-mcp/toggle",
            json!({"enabled": true}),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(toggle.status(), StatusCode::NOT_FOUND);

    let delete = app
        .oneshot(delete_with_token("/api/mcp/servers/legacy-mcp", &token, &csrf))
        .await
        .unwrap();
    assert_eq!(delete.status(), StatusCode::METHOD_NOT_ALLOWED);
}
