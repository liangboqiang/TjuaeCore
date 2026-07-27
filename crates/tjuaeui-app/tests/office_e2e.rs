//! 文档快照和格式转换 HTTP 接口的端到端测试。

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::json;
use tower::ServiceExt;

use common::{body_json, get_request, json_with_token, setup_and_login};
use tjuaeui_app::{AppConfig, AppServices, build_module_states, create_router_with_states};
use tjuaeui_office::{ConversionService, OfficeRouterState, SnapshotService};

async fn build_office_app() -> (axum::Router, AppServices, tempfile::TempDir) {
    let default_roots = vec![
        std::env::temp_dir(),
        dirs::home_dir().unwrap_or_else(std::env::temp_dir),
    ];
    build_office_app_with_roots(default_roots).await
}

async fn build_office_app_with_roots(
    allowed_roots: Vec<std::path::PathBuf>,
) -> (axum::Router, AppServices, tempfile::TempDir) {
    let temporary = tempfile::TempDir::new().unwrap();
    let data_dir = temporary.path().to_path_buf();
    let database = tjuaeui_db::init_database_memory().await.unwrap();
    let config = AppConfig {
        data_dir: data_dir.clone(),
        work_dir: data_dir,
        ..Default::default()
    };
    let services = AppServices::from_config(database, &config).await.unwrap();
    let (mut states, _) = build_module_states(&services).await.expect("build module states");
    states.office = OfficeRouterState {
        snapshot_service: Arc::new(SnapshotService::new(temporary.path())),
        conversion_service: Arc::new(ConversionService::new()),
        allowed_roots,
    };
    let router = create_router_with_states(&services, states);
    (router, services, temporary)
}

fn snapshot_target() -> serde_json::Value {
    json!({"content_type": "markdown", "file_path": "/a.md"})
}

#[tokio::test]
async fn unauthenticated_document_routes_are_rejected() {
    for path in [
        "/api/preview-history/list",
        "/api/preview-history/save",
        "/api/document/convert",
    ] {
        let (app, _services, _temporary) = build_office_app().await;
        let response = app.oneshot(get_request(path)).await.unwrap();
        assert!(
            response.status() == StatusCode::UNAUTHORIZED || response.status() == StatusCode::FORBIDDEN,
            "{path} returned {}",
            response.status()
        );
    }
}

#[tokio::test]
async fn preview_runtime_routes_are_not_registered() {
    for path in [
        "/api/word-preview/start",
        "/api/excel-preview/start",
        "/api/ppt-preview/start",
        "/api/ppt-proxy/8080",
        "/api/office-watch-proxy/8080",
    ] {
        let (app, _services, _temporary) = build_office_app().await;
        let response = app.oneshot(get_request(path)).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn snapshots_can_be_saved_listed_and_read() {
    let (mut app, services, _temporary) = build_office_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass123").await;

    let save = json_with_token(
        "POST",
        "/api/preview-history/save",
        json!({
            "target": snapshot_target(),
            "content": "# Hello World"
        }),
        &token,
        &csrf,
    );
    let save_response = app.clone().oneshot(save).await.unwrap();
    assert_eq!(save_response.status(), StatusCode::OK);
    let saved = body_json(save_response).await;
    let snapshot_id = saved["data"]["id"].as_str().unwrap().to_owned();

    let list = json_with_token(
        "POST",
        "/api/preview-history/list",
        json!({"target": snapshot_target()}),
        &token,
        &csrf,
    );
    let list_response = app.clone().oneshot(list).await.unwrap();
    assert_eq!(list_response.status(), StatusCode::OK);
    let listed = body_json(list_response).await;
    assert_eq!(listed["data"].as_array().unwrap().len(), 1);

    let get = json_with_token(
        "POST",
        "/api/preview-history/get-content",
        json!({
            "target": snapshot_target(),
            "snapshot_id": snapshot_id
        }),
        &token,
        &csrf,
    );
    let get_response = app.oneshot(get).await.unwrap();
    assert_eq!(get_response.status(), StatusCode::OK);
    let content = body_json(get_response).await;
    assert_eq!(content["data"]["content"], "# Hello World");
}

#[tokio::test]
async fn missing_snapshot_returns_null() {
    let (mut app, services, _temporary) = build_office_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user2", "pass123").await;
    let request = json_with_token(
        "POST",
        "/api/preview-history/get-content",
        json!({
            "target": snapshot_target(),
            "snapshot_id": "missing"
        }),
        &token,
        &csrf,
    );
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert!(body["data"].is_null());
}

#[tokio::test]
async fn excel_workbook_converts_to_json() {
    let (mut app, services, temporary) = build_office_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user3", "pass123").await;
    let workbook_path = temporary.path().join("test.xlsx");
    create_test_xlsx(&workbook_path);

    let request = json_with_token(
        "POST",
        "/api/document/convert",
        json!({
            "file_path": workbook_path,
            "to": "excel-json"
        }),
        &token,
        &csrf,
    );
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["data"]["to"], "excel-json");
    assert_eq!(body["data"]["result"]["success"], true);
    assert_eq!(body["data"]["result"]["data"]["sheets"][0]["data"][1][0], "Alice");
}

#[tokio::test]
async fn conversion_rejects_path_outside_sandbox() {
    let sandbox = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let workbook_path = outside.path().join("test.xlsx");
    create_test_xlsx(&workbook_path);

    let (mut app, services, _temporary) = build_office_app_with_roots(vec![sandbox.path().to_path_buf()]).await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user4", "pass123").await;
    let request = json_with_token(
        "POST",
        "/api/document/convert",
        json!({
            "file_path": workbook_path,
            "to": "excel-json"
        }),
        &token,
        &csrf,
    );
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = body_json(response).await;
    assert_eq!(body["code"], "PATH_OUTSIDE_SANDBOX");
}

#[tokio::test]
async fn unsupported_conversion_target_is_rejected() {
    let (mut app, services, _temporary) = build_office_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user5", "pass123").await;
    let request = json_with_token(
        "POST",
        "/api/document/convert",
        json!({
            "file_path": "/path/to/file.pptx",
            "to": "ppt-json"
        }),
        &token,
        &csrf,
    );
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

fn create_test_xlsx(path: &std::path::Path) {
    use rust_xlsxwriter::Workbook;

    let mut workbook = Workbook::new();
    let worksheet = workbook.add_worksheet();
    worksheet.write_string(0, 0, "Name").unwrap();
    worksheet.write_string(0, 1, "Age").unwrap();
    worksheet.write_string(1, 0, "Alice").unwrap();
    worksheet.write_number(1, 1, 30.0).unwrap();
    workbook.save(path).unwrap();
}
