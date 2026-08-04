//! MCP read-boundary and removed legacy-route E2E tests.
//!
//! Validation, probes, private overlay credentials, and activation now belong
//! to the typed asset lifecycle. The former ad-hoc connection and OAuth routes
//! must stay absent.

mod common;

use axum::http::StatusCode;
use serde_json::json;
use tower::ServiceExt;

use common::{body_json, build_app, get_with_token, json_with_token, setup_and_login};

// ===========================================================================
// Legacy connection testing is part of asset validate/try-run.
// ===========================================================================

#[tokio::test]
async fn legacy_stdio_connection_test_route_is_removed() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let req = json_with_token(
        "POST",
        "/api/mcp/test-connection",
        json!({
            "name": "enoent-test",
            "transport": {
                "type": "stdio",
                "command": "nonexistent-mcp-command-xyz-12345"
            }
        }),
        &token,
        &csrf,
    );
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ===========================================================================
#[tokio::test]
async fn legacy_http_connection_test_route_is_removed() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let req = json_with_token(
        "POST",
        "/api/mcp/test-connection",
        json!({
            "name": "unreachable-test",
            "transport": {
                "type": "http",
                "url": "http://127.0.0.1:19999/mcp"
            }
        }),
        &token,
        &csrf,
    );
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ===========================================================================
// AS-1: Get agent configs (may return empty in test env)
// ===========================================================================

#[tokio::test]
async fn get_agent_configs() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let resp = app
        .clone()
        .oneshot(get_with_token("/api/mcp/agent-configs", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert!(json["success"].as_bool().unwrap());
    // In test env, data is an array (may be empty or contain tjuaeui adapter)
    assert!(json["data"].is_array());
}

// ===========================================================================
// Legacy MCP-specific OAuth state is replaced by typed private overlay slots.
// ===========================================================================

#[tokio::test]
async fn legacy_oauth_check_status_route_is_removed() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let req = json_with_token(
        "POST",
        "/api/mcp/oauth/check-status",
        json!({ "server_url": "https://unknown-server.example.com" }),
        &token,
        &csrf,
    );
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ===========================================================================
#[tokio::test]
async fn legacy_oauth_authenticated_servers_route_is_removed() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let resp = app
        .clone()
        .oneshot(get_with_token("/api/mcp/oauth/authenticated", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ===========================================================================
#[tokio::test]
async fn legacy_oauth_logout_route_is_removed() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let req = json_with_token(
        "POST",
        "/api/mcp/oauth/logout",
        json!({ "server_url": "https://never-authed.example.com" }),
        &token,
        &csrf,
    );
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ===========================================================================
// AU-1: Unauthenticated access to various MCP endpoints
// ===========================================================================

#[tokio::test]
async fn unauthenticated_get_servers_rejected() {
    let (app, _services) = build_app().await;

    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/api/mcp/servers")
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    // GET bypasses CSRF; auth middleware returns the canonical auth boundary.
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let json = body_json(resp).await;
    assert_eq!(json["code"], "UNAUTHORIZED");
}

#[tokio::test]
async fn unauthenticated_post_server_rejected() {
    let (app, _services) = build_app().await;

    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/api/mcp/servers")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            serde_json::to_vec(&json!({
                "name": "test",
                "transport": { "type": "stdio", "command": "npx" }
            }))
            .unwrap(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// AU-3: Valid token accesses MCP routes successfully
#[tokio::test]
async fn authenticated_access_succeeds() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let resp = app
        .clone()
        .oneshot(get_with_token("/api/mcp/servers", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// AU-2: Invalid Bearer token is rejected by auth middleware.
#[tokio::test]
async fn invalid_token_rejected() {
    let (app, _services) = build_app().await;

    // GET bypasses CSRF; auth middleware sees invalid Bearer.
    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/api/mcp/servers")
        .header("authorization", "Bearer invalid-jwt-token-abc123")
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let json = body_json(resp).await;
    assert_eq!(json["code"], "UNAUTHORIZED");
}
