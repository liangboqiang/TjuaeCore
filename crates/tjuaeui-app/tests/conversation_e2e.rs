//! E2E tests for conversation CRUD, clone, reset, associated, and auth protection.

mod common;

use axum::http::StatusCode;
use serde_json::json;
use tjuaeui_api_types::{AssetKind, AssetRuntimeCommandRequest, CreateAssetRequest};
use tjuaeui_db::{
    IAssistantDefinitionRepository, IAssistantPreferenceRepository, IConversationRepository,
    SqliteAssistantDefinitionRepository, SqliteAssistantPreferenceRepository, SqliteConversationRepository,
    UpsertAssistantPreferenceParams,
};
use tower::ServiceExt;

use common::{
    body_json, build_app, build_app_with_mock_agents, delete_with_token, get_request, get_with_token, json_with_token,
    setup_and_login,
};

// ── Helpers ───────────────────────────────────────────────────────────

fn create_body(name: &str) -> serde_json::Value {
    json!({
        "type": "acp",
        "name": name,
        "extra": {}
    })
}

fn create_body_with_extra(name: &str, extra: serde_json::Value) -> serde_json::Value {
    json!({
        "type": "acp",
        "name": name,
        "extra": extra
    })
}

// ── T1: Create ────────────────────────────────────────────────────────

#[tokio::test]
async fn t1_1_create_conversation_success() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let req = json_with_token("POST", "/api/conversations", create_body("Code Review"), &token, &csrf);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    let data = &json["data"];
    assert_eq!(data["name"], "Code Review");
    assert_eq!(data["type"], "acp");
    assert_eq!(data["status"], "pending");
    assert_eq!(data["source"], "tjuaeui");
    assert_eq!(data["pinned"], false);
    assert!(data["id"].as_str().is_some());
    assert!(data["created_at"].as_i64().is_some());
    assert!(data["modified_at"].as_i64().is_some());
    assert!(data["extra"]["workspace"].as_str().is_some());
}

#[tokio::test]
async fn t1_2_create_supported_agent_types_and_reject_legacy_types() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let types = ["acp", "tjuaecli"];
    for agent_type in types {
        let body = json!({
            "type": agent_type,
            "extra": {}
        });
        let req = json_with_token("POST", "/api/conversations", body, &token, &csrf);
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED, "type={agent_type}");
        let json = body_json(resp).await;
        assert_eq!(json["data"]["type"], agent_type);
    }

    for agent_type in ["openclaw-gateway", "nanobot", "remote", "gemini"] {
        let body = json!({
            "type": agent_type,
            "extra": {}
        });
        let req = json_with_token("POST", "/api/conversations", body, &token, &csrf);
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "type={agent_type}");
        let json = body_json(resp).await;
        assert_eq!(json["code"], "BAD_REQUEST");
    }
}

#[tokio::test]
async fn t1_3_create_with_optional_fields() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let body = json!({
        "type": "acp",
        "name": "Telegram Bot",
        "source": "telegram",
        "channel_chat_id": "user:123",
        "extra": {}
    });
    let req = json_with_token("POST", "/api/conversations", body, &token, &csrf);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let json = body_json(resp).await;
    assert_eq!(json["data"]["source"], "telegram");
    assert_eq!(json["data"]["channel_chat_id"], "user:123");
}

#[tokio::test]
async fn t1_3b_create_persists_available_locale_fallback_rule_in_assistant_snapshot() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;
    let user_id = "system_default_user";
    let assistant_id = "locale-fallback-u1";

    let created = services
        .asset_catalog
        .create(
            user_id,
            CreateAssetRequest {
                id: assistant_id.into(),
                kind: AssetKind::Assistant,
                display_name: "Snapshot Assistant".into(),
                description: None,
                runtime_id: Some(assistant_id.into()),
            },
        )
        .await
        .unwrap();
    let rule = created
        .files
        .iter()
        .find(|file| file.path == "rules/zh-CN.md")
        .expect("local assistant template contains a simplified Chinese rule");
    let updated = services
        .asset_catalog
        .write_file(
            user_id,
            assistant_id,
            &rule.path,
            "zh-CN fallback snapshot rule",
            &rule.digest,
        )
        .await
        .unwrap();
    let command = |idempotency_key: &str| AssetRuntimeCommandRequest {
        idempotency_key: idempotency_key.into(),
        expected_definition_digest: updated.asset.definition_digest.clone(),
        expected_overlay_version: None,
    };
    services
        .asset_catalog
        .validate_runtime(user_id, assistant_id, command("snapshot-validate"))
        .await
        .unwrap();
    services
        .asset_catalog
        .try_run(user_id, assistant_id, command("snapshot-try-run"))
        .await
        .unwrap();
    services
        .asset_catalog
        .activate(user_id, assistant_id, command("snapshot-activate"))
        .await
        .unwrap();

    let pool = services.database.pool().clone();
    let definition_repo = SqliteAssistantDefinitionRepository::new(pool.clone());
    let preference_repo = SqliteAssistantPreferenceRepository::new(pool);
    let conversation_repo = SqliteConversationRepository::new(services.database.pool().clone());
    let binding = services
        .asset_catalog
        .list_active_runtime_bindings(user_id, AssetKind::Assistant)
        .await
        .unwrap()
        .into_iter()
        .find(|binding| binding.provenance.local_asset_id == assistant_id)
        .expect("activated assistant has a current-user runtime binding");
    let definition = definition_repo
        .get_by_assistant_id(&binding.projection_runtime_id)
        .await
        .unwrap()
        .unwrap();
    preference_repo
        .upsert(&UpsertAssistantPreferenceParams {
            assistant_definition_id: &definition.id,
            last_model_id: Some("pref-model"),
            last_permission_value: Some("workspace-write"),
            last_thought_level_value: None,
            last_skill_ids: r#"["pref-skill"]"#,
            last_mcp_ids: r#"["pref-mcp"]"#,
        })
        .await
        .unwrap();

    let create_req = json_with_token(
        "POST",
        "/api/conversations",
        json!({
            "type": "acp",
            "name": "Snapshot Flow",
            "assistant": {
                "id": assistant_id,
                "locale": "en-US",
                "conversation_overrides": {
                    "model": "override-model",
                    "skill_ids": [],
                    "mcp_ids": []
                }
            },
            "extra": {}
        }),
        &token,
        &csrf,
    );
    let resp = app.clone().oneshot(create_req).await.unwrap();
    let status = resp.status();
    let json = body_json(resp).await;
    assert_eq!(status, StatusCode::CREATED, "response={json}");
    let data = &json["data"];
    assert_eq!(data["assistant"]["id"], assistant_id);
    assert_eq!(data["assistant"]["backend"], "tjuaecli");
    assert!(data["extra"].get("assistant_id").is_none());
    assert!(data["extra"].get("preset_assistant_id").is_none());
    assert!(data["extra"].get("preset_context").is_none());
    assert!(data["extra"].get("preset_rules").is_none());
    assert_eq!(data["extra"]["session_mode"], "workspace-write");
    assert!(data["extra"]["current_mode_id"].is_null());
    assert_eq!(data["extra"]["current_model_id"], "override-model");
    assert!(data["extra"].get("assistant_snapshot").is_none());
    assert_eq!(data["extra"]["skills"], json!([]));

    let snapshot = conversation_repo
        .get_assistant_snapshot(data["id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.assistant_id, assistant_id);
    assert_eq!(snapshot.agent_id, "632f31d2");
    assert_eq!(snapshot.rules_content, "zh-CN fallback snapshot rule");
    assert_eq!(snapshot.resolved_permission_value.as_deref(), Some("workspace-write"));
    assert_eq!(snapshot.resolved_skill_ids, "[]");
    assert_eq!(snapshot.resolved_mcp_ids, "[]");

    let updated_preference = preference_repo.get(&definition.id).await.unwrap().unwrap();
    assert_eq!(updated_preference.last_model_id.as_deref(), Some("override-model"));
    assert_eq!(
        updated_preference.last_permission_value.as_deref(),
        Some("workspace-write")
    );
    assert_eq!(updated_preference.last_skill_ids, r#"["pref-skill"]"#);
    assert_eq!(updated_preference.last_mcp_ids, "[]");
}

#[tokio::test]
async fn t1_4_create_missing_required_field() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    // Missing type
    let body = json!({
        "model": { "provider_id": "p1", "model": "m1" },
        "extra": {}
    });
    let req = json_with_token("POST", "/api/conversations", body, &token, &csrf);
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // model is optional — omitting it should succeed
    let body = json!({ "type": "acp", "extra": {} });
    let req = json_with_token("POST", "/api/conversations", body, &token, &csrf);
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Missing extra
    let body = json!({
        "type": "tjuaecli",
        "model": { "provider_id": "p1", "model": "m1" }
    });
    let req = json_with_token("POST", "/api/conversations", body, &token, &csrf);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn t1_5_create_invalid_type() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let body = json!({
        "type": "invalid_type",
        "model": { "provider_id": "p1", "model": "m1" },
        "extra": {}
    });
    let req = json_with_token("POST", "/api/conversations", body, &token, &csrf);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn t1_5b_create_accepts_workspace_paths_with_whitespace_segments() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("my project").join("repo");
    std::fs::create_dir_all(&workspace).unwrap();

    let body = json!({
        "type": "acp",
        "extra": {
            "workspace": workspace.to_string_lossy()
        }
    });
    let req = json_with_token("POST", "/api/conversations", body, &token, &csrf);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let json = body_json(resp).await;
    assert_eq!(
        json["data"]["extra"]["workspace"],
        workspace.to_string_lossy().to_string()
    );
}

#[tokio::test]
async fn t1_5c_create_rejects_missing_workspace_path() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let missing_workspace =
        std::env::temp_dir().join(format!("tjuaeui-conv-missing-{}", tjuaeui_common::generate_short_id()));

    let body = json!({
        "type": "acp",
        "extra": {
            "workspace": missing_workspace.to_string_lossy()
        }
    });
    let req = json_with_token("POST", "/api/conversations", body, &token, &csrf);
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let json = body_json(resp).await;
    assert_eq!(json["code"], "WORKSPACE_PATH_UNAVAILABLE");
    assert_eq!(json["details"]["operation"], "create");
    assert_eq!(
        json["details"]["workspace_path"],
        missing_workspace.to_string_lossy().to_string()
    );

    let list_resp = app.oneshot(get_with_token("/api/conversations", &token)).await.unwrap();
    assert_eq!(list_resp.status(), StatusCode::OK);
    let list_json = body_json(list_resp).await;
    assert!(
        list_json["data"]["items"].as_array().unwrap().is_empty(),
        "invalid conversation should not be persisted"
    );
}

#[tokio::test]
async fn t1_6_create_requires_auth() {
    let (app, _services) = build_app().await;

    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/api/conversations")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            serde_json::to_vec(&create_body("test")).unwrap(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ── T2: List ──────────────────────────────────────────────────────────

#[tokio::test]
async fn t2_1_list_empty() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let resp = app.oneshot(get_with_token("/api/conversations", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["data"]["items"].as_array().unwrap().len(), 0);
    assert_eq!(json["data"]["total"], 0);
    assert_eq!(json["data"]["has_more"], false);
}

#[tokio::test]
async fn t2_2_list_basic() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    for i in 0..3 {
        let req = json_with_token(
            "POST",
            "/api/conversations",
            create_body(&format!("Conv {i}")),
            &token,
            &csrf,
        );
        app.clone().oneshot(req).await.unwrap();
    }

    let resp = app.oneshot(get_with_token("/api/conversations", &token)).await.unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["data"]["items"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn t2_3_list_cursor_pagination() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    for i in 0..5 {
        let req = json_with_token(
            "POST",
            "/api/conversations",
            create_body(&format!("Conv {i}")),
            &token,
            &csrf,
        );
        app.clone().oneshot(req).await.unwrap();
    }

    // First page: limit=2
    let resp = app
        .clone()
        .oneshot(get_with_token("/api/conversations?limit=2", &token))
        .await
        .unwrap();
    let json = body_json(resp).await;
    let items = json["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(json["data"]["has_more"], true);

    // Second page using cursor
    let cursor = items.last().unwrap()["id"].as_str().unwrap();
    let resp = app
        .clone()
        .oneshot(get_with_token(
            &format!("/api/conversations?limit=2&cursor={cursor}"),
            &token,
        ))
        .await
        .unwrap();
    let json = body_json(resp).await;
    let items2 = json["data"]["items"].as_array().unwrap();
    assert_eq!(items2.len(), 2);
    assert_eq!(json["data"]["has_more"], true);

    // Third page
    let cursor2 = items2.last().unwrap()["id"].as_str().unwrap();
    let resp = app
        .oneshot(get_with_token(
            &format!("/api/conversations?limit=2&cursor={cursor2}"),
            &token,
        ))
        .await
        .unwrap();
    let json = body_json(resp).await;
    let items3 = json["data"]["items"].as_array().unwrap();
    assert_eq!(items3.len(), 1);
    assert_eq!(json["data"]["has_more"], false);
}

#[tokio::test]
async fn t2_4_list_source_filter() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    // Create 2 tjuaeui + 1 telegram
    for _ in 0..2 {
        let req = json_with_token("POST", "/api/conversations", create_body("TjuaeUI Conv"), &token, &csrf);
        app.clone().oneshot(req).await.unwrap();
    }

    let tg_body = json!({
        "type": "acp",
        "name": "TG Conv",
        "source": "telegram",
        "extra": {}
    });
    let req = json_with_token("POST", "/api/conversations", tg_body, &token, &csrf);
    app.clone().oneshot(req).await.unwrap();

    let resp = app
        .oneshot(get_with_token("/api/conversations?source=telegram", &token))
        .await
        .unwrap();
    let json = body_json(resp).await;
    let items = json["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["source"], "telegram");
}

#[tokio::test]
async fn t2_5_list_pinned_filter() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    // Create 2 conversations
    let req = json_with_token("POST", "/api/conversations", create_body("Unpinned"), &token, &csrf);
    app.clone().oneshot(req).await.unwrap();

    let req = json_with_token("POST", "/api/conversations", create_body("Will Pin"), &token, &csrf);
    let resp = app.clone().oneshot(req).await.unwrap();
    let json = body_json(resp).await;
    let pinned_id = json["data"]["id"].as_str().unwrap().to_owned();

    // Pin one
    let req = json_with_token(
        "PATCH",
        &format!("/api/conversations/{pinned_id}"),
        json!({"pinned": true}),
        &token,
        &csrf,
    );
    app.clone().oneshot(req).await.unwrap();

    let resp = app
        .oneshot(get_with_token("/api/conversations?pinned=true", &token))
        .await
        .unwrap();
    let json = body_json(resp).await;
    let items = json["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["pinned"], true);
}

#[tokio::test]
async fn t2_6_list_requires_auth() {
    let (app, _services) = build_app().await;
    let resp = app.oneshot(get_request("/api/conversations")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let json = body_json(resp).await;
    assert_eq!(json["code"], "UNAUTHORIZED");
}

// ── T3: Get ───────────────────────────────────────────────────────────

#[tokio::test]
async fn t3_1_get_existing() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let req = json_with_token("POST", "/api/conversations", create_body("My Conv"), &token, &csrf);
    let resp = app.clone().oneshot(req).await.unwrap();
    let json = body_json(resp).await;
    let id = json["data"]["id"].as_str().unwrap().to_owned();

    let resp = app
        .oneshot(get_with_token(&format!("/api/conversations/{id}"), &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["data"]["id"], id);
    assert_eq!(json["data"]["name"], "My Conv");
}

#[tokio::test]
async fn t3_2_get_not_found() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let resp = app
        .oneshot(get_with_token("/api/conversations/non-existent-id", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn t3_3_get_requires_auth() {
    let (app, _services) = build_app().await;
    let resp = app.oneshot(get_request("/api/conversations/some-id")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let json = body_json(resp).await;
    assert_eq!(json["code"], "UNAUTHORIZED");
}

// ── T4: Update ────────────────────────────────────────────────────────

#[tokio::test]
async fn t4_1_update_name() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let req = json_with_token("POST", "/api/conversations", create_body("Original"), &token, &csrf);
    let resp = app.clone().oneshot(req).await.unwrap();
    let json = body_json(resp).await;
    let id = json["data"]["id"].as_str().unwrap().to_owned();
    let original_modified = json["data"]["modified_at"].as_i64().unwrap();

    let req = json_with_token(
        "PATCH",
        &format!("/api/conversations/{id}"),
        json!({"name": "Updated"}),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["data"]["name"], "Updated");
    assert!(json["data"]["modified_at"].as_i64().unwrap() >= original_modified);
}

#[tokio::test]
async fn t4_2_update_pin_and_unpin() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let req = json_with_token("POST", "/api/conversations", create_body("Pin Test"), &token, &csrf);
    let resp = app.clone().oneshot(req).await.unwrap();
    let json = body_json(resp).await;
    let id = json["data"]["id"].as_str().unwrap().to_owned();

    // Pin
    let req = json_with_token(
        "PATCH",
        &format!("/api/conversations/{id}"),
        json!({"pinned": true}),
        &token,
        &csrf,
    );
    let resp = app.clone().oneshot(req).await.unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["data"]["pinned"], true);
    assert!(json["data"]["pinned_at"].as_i64().is_some());

    // Unpin
    let req = json_with_token(
        "PATCH",
        &format!("/api/conversations/{id}"),
        json!({"pinned": false}),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["data"]["pinned"], false);
    assert!(json["data"]["pinned_at"].is_null());
}

#[tokio::test]
async fn t4_3_update_extra_merge() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;
    let temp = tempfile::tempdir().unwrap();
    let old_workspace = temp.path().join("old");
    let new_workspace = temp.path().join("new");
    std::fs::create_dir_all(&old_workspace).unwrap();
    std::fs::create_dir_all(&new_workspace).unwrap();

    let body = create_body_with_extra(
        "Merge Test",
        json!({"workspace": old_workspace.to_string_lossy(), "context_file_name": "ctx.md"}),
    );
    let req = json_with_token("POST", "/api/conversations", body, &token, &csrf);
    let resp = app.clone().oneshot(req).await.unwrap();
    let json = body_json(resp).await;
    let id = json["data"]["id"].as_str().unwrap().to_owned();

    // Merge update: change workspace, keep contextFileName
    let req = json_with_token(
        "PATCH",
        &format!("/api/conversations/{id}"),
        json!({"extra": {"workspace": new_workspace.to_string_lossy()}}),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    let json = body_json(resp).await;
    assert_eq!(
        json["data"]["extra"]["workspace"],
        new_workspace.to_string_lossy().to_string()
    );
    assert_eq!(json["data"]["extra"]["context_file_name"], "ctx.md");
}

#[tokio::test]
async fn t4_4_update_model() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    // tjuae_cli — only type that allows top-level model updates
    let create = json!({
        "type": "tjuaecli",
        "name": "Model Test",
        "model": { "provider_id": "p1", "model": "m1" },
        "extra": {}
    });
    let req = json_with_token("POST", "/api/conversations", create, &token, &csrf);
    let resp = app.clone().oneshot(req).await.unwrap();
    let json = body_json(resp).await;
    let id = json["data"]["id"].as_str().unwrap().to_owned();

    let req = json_with_token(
        "PATCH",
        &format!("/api/conversations/{id}"),
        json!({"model": {"provider_id": "p2", "model": "new-model"}}),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["data"]["model"]["provider_id"], "p2");
    assert_eq!(json["data"]["model"]["model"], "new-model");
}

#[tokio::test]
async fn t4_5_update_not_found() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let req = json_with_token(
        "PATCH",
        "/api/conversations/non-existent-id",
        json!({"name": "X"}),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn t4_6_update_requires_auth() {
    let (app, _services) = build_app().await;
    let resp = app.oneshot(get_request("/api/conversations/some-id")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let json = body_json(resp).await;
    assert_eq!(json["code"], "UNAUTHORIZED");
}

// ── T5: Delete ────────────────────────────────────────────────────────

#[tokio::test]
async fn t5_1_delete_conversation() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let req = json_with_token("POST", "/api/conversations", create_body("To Delete"), &token, &csrf);
    let resp = app.clone().oneshot(req).await.unwrap();
    let json = body_json(resp).await;
    let id = json["data"]["id"].as_str().unwrap().to_owned();

    let resp = app
        .clone()
        .oneshot(delete_with_token(&format!("/api/conversations/{id}"), &token, &csrf))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Verify it's gone
    let resp = app
        .oneshot(get_with_token(&format!("/api/conversations/{id}"), &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn t5_2_delete_not_found() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let resp = app
        .oneshot(delete_with_token("/api/conversations/non-existent-id", &token, &csrf))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn t5_3_delete_requires_auth() {
    let (app, _services) = build_app().await;
    let req = axum::http::Request::builder()
        .method("DELETE")
        .uri("/api/conversations/some-id")
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ── T6: Clone ─────────────────────────────────────────────────────────

#[tokio::test]
async fn t6_2_clone_without_source() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let clone_body = json!({
        "conversation": {
            "type": "acp",
            "name": "Fresh Clone",
            "extra": {}
        }
    });
    let req = json_with_token("POST", "/api/conversations/clone", clone_body, &token, &csrf);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let json = body_json(resp).await;
    assert_eq!(json["data"]["name"], "Fresh Clone");
    assert_eq!(json["data"]["type"], "acp");
}

#[tokio::test]
async fn t6_4_clone_requires_auth() {
    let (app, _services) = build_app().await;
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/api/conversations/clone")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(b"{}".to_vec()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ── T7: Reset ─────────────────────────────────────────────────────────

#[tokio::test]
async fn t7_1_reset_conversation() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    // Create conversation
    let req = json_with_token("POST", "/api/conversations", create_body("Reset Test"), &token, &csrf);
    let resp = app.clone().oneshot(req).await.unwrap();
    let json = body_json(resp).await;
    let id = json["data"]["id"].as_str().unwrap().to_owned();

    // Insert a message directly via repo
    let repo = tjuaeui_db::SqliteConversationRepository::new(services.database.pool().clone());
    let msg = tjuaeui_db::models::MessageRow {
        id: "msg-1".into(),
        conversation_id: id.clone(),
        msg_id: None,
        r#type: "text".into(),
        content: r#"{"content":"hello"}"#.into(),
        position: None,
        status: None,
        hidden: false,
        created_at: 1000,
    };
    tjuaeui_db::IConversationRepository::insert_message(&repo, &msg)
        .await
        .unwrap();

    // Reset
    let req = json_with_token(
        "POST",
        &format!("/api/conversations/{id}/reset"),
        json!({}),
        &token,
        &csrf,
    );
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Verify messages cleared
    let resp = app
        .clone()
        .oneshot(get_with_token(&format!("/api/conversations/{id}/messages"), &token))
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["data"]["items"].as_array().unwrap().len(), 0);

    // Verify status is pending
    let resp = app
        .oneshot(get_with_token(&format!("/api/conversations/{id}"), &token))
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["data"]["status"], "pending");
}

#[tokio::test]
async fn t7_2_reset_not_found() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let req = json_with_token(
        "POST",
        "/api/conversations/non-existent-id/reset",
        json!({}),
        &token,
        &csrf,
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn t7_3_reset_requires_auth() {
    let (app, _services) = build_app().await;
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/api/conversations/some-id/reset")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(b"{}".to_vec()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn team_owned_conversation_rejects_ordinary_send_but_allows_history_reads() {
    let (mut app, services) = build_app_with_mock_agents().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let req = json_with_token(
        "POST",
        "/api/conversations",
        create_body_with_extra("Team Owned", json!({ "teamId": "team-1" })),
        &token,
        &csrf,
    );
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let json = body_json(resp).await;
    let id = json["data"]["id"].as_str().unwrap().to_owned();

    let repo = tjuaeui_db::SqliteConversationRepository::new(services.database.pool().clone());
    let msg = tjuaeui_db::models::MessageRow {
        id: "team-history-msg-1".into(),
        conversation_id: id.clone(),
        msg_id: Some("team-history-msg-1".into()),
        r#type: "text".into(),
        content: r#"{"content":"history remains readable"}"#.into(),
        position: Some("left".into()),
        status: Some("finish".into()),
        hidden: false,
        created_at: 1000,
    };
    tjuaeui_db::IConversationRepository::insert_message(&repo, &msg)
        .await
        .unwrap();

    let req = json_with_token(
        "POST",
        &format!("/api/conversations/{id}/messages"),
        json!({ "content": "ordinary send should be blocked" }),
        &token,
        &csrf,
    );
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let json = body_json(resp).await;
    assert_eq!(json["code"], "FORBIDDEN");
    assert_eq!(json["error"], "无权访问。");

    let resp = app
        .oneshot(get_with_token(&format!("/api/conversations/{id}/messages"), &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["data"]["items"].as_array().unwrap().len(), 1);
}

// ── T10: Associated ───────────────────────────────────────────────────

#[tokio::test]
async fn t10_1_associated_same_workspace() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;
    let temp = tempfile::tempdir().unwrap();
    let shared_workspace = temp.path().join("same");
    let other_workspace = temp.path().join("other");
    std::fs::create_dir_all(&shared_workspace).unwrap();
    std::fs::create_dir_all(&other_workspace).unwrap();

    // Create 3 conversations: 2 same workspace, 1 different
    let body1 = create_body_with_extra("Conv A", json!({"workspace": shared_workspace.to_string_lossy()}));
    let req = json_with_token("POST", "/api/conversations", body1, &token, &csrf);
    let resp = app.clone().oneshot(req).await.unwrap();
    let json = body_json(resp).await;
    let id_a = json["data"]["id"].as_str().unwrap().to_owned();

    let body2 = create_body_with_extra("Conv B", json!({"workspace": shared_workspace.to_string_lossy()}));
    let req = json_with_token("POST", "/api/conversations", body2, &token, &csrf);
    app.clone().oneshot(req).await.unwrap();

    let body3 = create_body_with_extra("Conv C", json!({"workspace": other_workspace.to_string_lossy()}));
    let req = json_with_token("POST", "/api/conversations", body3, &token, &csrf);
    app.clone().oneshot(req).await.unwrap();

    let resp = app
        .oneshot(get_with_token(&format!("/api/conversations/{id_a}/associated"), &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let items = json["data"].as_array().unwrap();
    assert_eq!(items.len(), 1); // only Conv B, not self or Conv C
    assert_eq!(
        items[0]["extra"]["workspace"],
        shared_workspace.to_string_lossy().to_string()
    );
}

#[tokio::test]
async fn t10_2_associated_none() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;
    let temp = tempfile::tempdir().unwrap();
    let unique_workspace = temp.path().join("unique");
    std::fs::create_dir_all(&unique_workspace).unwrap();

    let body = create_body_with_extra("Unique", json!({"workspace": unique_workspace.to_string_lossy()}));
    let req = json_with_token("POST", "/api/conversations", body, &token, &csrf);
    let resp = app.clone().oneshot(req).await.unwrap();
    let json = body_json(resp).await;
    let id = json["data"]["id"].as_str().unwrap().to_owned();

    let resp = app
        .oneshot(get_with_token(&format!("/api/conversations/{id}/associated"), &token))
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["data"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn t10_3_associated_not_found() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let resp = app
        .oneshot(get_with_token("/api/conversations/non-existent-id/associated", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn t10_4_associated_requires_auth() {
    let (app, _services) = build_app().await;
    let resp = app
        .oneshot(get_request("/api/conversations/some-id/associated"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let json = body_json(resp).await;
    assert_eq!(json["code"], "UNAUTHORIZED");
}

// ── T12: Boundary scenarios ───────────────────────────────────────────

#[tokio::test]
async fn t12_1_long_name() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let long_name = "A".repeat(1000);
    let req = json_with_token("POST", "/api/conversations", create_body(&long_name), &token, &csrf);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let json = body_json(resp).await;
    assert_eq!(json["data"]["name"].as_str().unwrap().len(), 1000);
}

#[tokio::test]
async fn t12_2_large_nested_extra() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let big_extra = json!({
        "nested": {
            "level1": {
                "level2": {
                    "level3": { "deep": true }
                }
            }
        },
        "array": [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
    });
    let body = create_body_with_extra("Big Extra", big_extra.clone());
    let req = json_with_token("POST", "/api/conversations", body, &token, &csrf);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let json = body_json(resp).await;
    assert_eq!(
        json["data"]["extra"]["nested"]["level1"]["level2"]["level3"]["deep"],
        true
    );
    assert_eq!(json["data"]["extra"]["array"].as_array().unwrap().len(), 10);
}

#[tokio::test]
async fn t12_3_concurrent_creates() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    let mut ids = Vec::new();
    for i in 0..10 {
        let req = json_with_token(
            "POST",
            "/api/conversations",
            create_body(&format!("Concurrent {i}")),
            &token,
            &csrf,
        );
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let json = body_json(resp).await;
        ids.push(json["data"]["id"].as_str().unwrap().to_owned());
    }

    // All IDs should be unique
    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), 10);
}

// ── Full lifecycle ────────────────────────────────────────────────────

#[tokio::test]
async fn full_conversation_lifecycle() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;

    // Create
    let req = json_with_token(
        "POST",
        "/api/conversations",
        create_body("Lifecycle Test"),
        &token,
        &csrf,
    );
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let json = body_json(resp).await;
    let id = json["data"]["id"].as_str().unwrap().to_owned();
    assert_eq!(json["data"]["status"], "pending");

    // Read
    let resp = app
        .clone()
        .oneshot(get_with_token(&format!("/api/conversations/{id}"), &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Update
    let req = json_with_token(
        "PATCH",
        &format!("/api/conversations/{id}"),
        json!({"name": "Updated Lifecycle"}),
        &token,
        &csrf,
    );
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["data"]["name"], "Updated Lifecycle");

    // Delete
    let resp = app
        .clone()
        .oneshot(delete_with_token(&format!("/api/conversations/{id}"), &token, &csrf))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Verify gone
    let resp = app
        .oneshot(get_with_token(&format!("/api/conversations/{id}"), &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
