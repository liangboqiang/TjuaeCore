//! HTTP integration tests for the read-only assistant runtime projection.
//!
//! Assistant definitions are created and edited exclusively through the
//! AssetCatalog lifecycle. The legacy `/api/assistants` CRUD and
//! `/api/skills/assistant-rule` file APIs must never become a fallback writer.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;
use tower::ServiceExt;

use common::{build_app, delete_with_token, get_with_token, json_with_token, setup_and_login};

async fn authenticated_app() -> (axum::Router, tjuaeui_app::AppServices, String, String) {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "password").await;
    (app, services, token, csrf)
}

#[tokio::test]
async fn assistant_projection_list_requires_authentication() {
    let (app, _services) = build_app().await;
    let response = app
        .oneshot(Request::builder().uri("/api/assistants").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn authenticated_users_can_read_the_assistant_projection() {
    let (app, _services, token, _csrf) = authenticated_app().await;
    let response = app.oneshot(get_with_token("/api/assistants", &token)).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn legacy_assistant_crud_is_method_not_allowed_and_cannot_write_projection_tables() {
    let (app, services, token, csrf) = authenticated_app().await;
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM assistant_definitions")
        .fetch_one(services.database.pool())
        .await
        .unwrap();

    let create = app
        .clone()
        .oneshot(json_with_token(
            "POST",
            "/api/assistants",
            json!({"id": "legacy", "name": "Legacy"}),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    let update = app
        .clone()
        .oneshot(json_with_token(
            "PUT",
            "/api/assistants/legacy",
            json!({"name": "Legacy"}),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    let delete = app
        .oneshot(delete_with_token("/api/assistants/legacy", &token, &csrf))
        .await
        .unwrap();

    assert_eq!(create.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(update.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(delete.status(), StatusCode::METHOD_NOT_ALLOWED);

    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM assistant_definitions")
        .fetch_one(services.database.pool())
        .await
        .unwrap();
    assert_eq!(after, before);
}

#[tokio::test]
async fn legacy_assistant_state_and_rule_file_routes_are_absent() {
    let (app, _services, token, csrf) = authenticated_app().await;

    let set_state = app
        .clone()
        .oneshot(json_with_token(
            "PUT",
            "/api/assistants/legacy/state",
            json!({"enabled": true}),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    let read_rule = app
        .clone()
        .oneshot(get_with_token("/api/skills/assistant-rule/legacy?locale=zh-CN", &token))
        .await
        .unwrap();
    let write_rule = app
        .clone()
        .oneshot(json_with_token(
            "PUT",
            "/api/skills/assistant-rule/legacy?locale=zh-CN",
            json!({"content": "legacy"}),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    let delete_rule = app
        .oneshot(delete_with_token(
            "/api/skills/assistant-rule/legacy?locale=zh-CN",
            &token,
            &csrf,
        ))
        .await
        .unwrap();

    assert_eq!(set_state.status(), StatusCode::NOT_FOUND);
    assert_eq!(read_rule.status(), StatusCode::NOT_FOUND);
    assert_eq!(write_rule.status(), StatusCode::NOT_FOUND);
    assert_eq!(delete_rule.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn missing_projection_avatar_returns_not_found() {
    let (app, _services, token, _csrf) = authenticated_app().await;
    let response = app
        .oneshot(get_with_token("/api/assistants/missing/avatar", &token))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
