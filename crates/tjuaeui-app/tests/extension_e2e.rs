mod common;

use axum::http::StatusCode;
use serde_json::json;
use tempfile::TempDir;
use tower::ServiceExt;

use tjuaeui_app::{AppConfig, AppServices, build_module_states, create_router_with_states};
use tjuaeui_common::now_ms;
use tjuaeui_db::{IChannelRepository, SqliteChannelRepository};
use tjuaeui_extension::{ExtensionSource, ScanPath};

use common::{body_json, build_app, build_app_with_skill_paths, get_with_token, json_with_token, setup_and_login};

fn write_legacy_extension_fixture(tmp: &TempDir) -> std::path::PathBuf {
    let ext_root = tmp.path().join("extensions");
    let ext_dir = ext_root.join("legacy-suite");
    std::fs::create_dir_all(ext_dir.join("assets")).unwrap();
    std::fs::create_dir_all(ext_dir.join("assistants")).unwrap();
    std::fs::create_dir_all(ext_dir.join("skills")).unwrap();
    std::fs::create_dir_all(ext_dir.join("themes")).unwrap();

    std::fs::write(ext_dir.join("assets/adapter.png"), "adapter").unwrap();
    std::fs::write(ext_dir.join("assets/assistant.png"), "assistant").unwrap();
    std::fs::write(ext_dir.join("assets/theme-cover.png"), "cover").unwrap();
    std::fs::write(ext_dir.join("assets/channel.png"), "channel").unwrap();
    std::fs::write(ext_dir.join("assistants/context.md"), "Assistant context from file.").unwrap();
    std::fs::write(ext_dir.join("skills/review.md"), "# review skill").unwrap();
    std::fs::write(ext_dir.join("themes/dark.css"), ":root { --legacy-bg: #111; }").unwrap();

    std::fs::write(
        ext_dir.join("tjuae-extension.json"),
        serde_json::to_vec_pretty(&json!({
            "name": "legacy-suite",
            "displayName": "Legacy Suite",
            "version": "1.0.0",
            "engine": {
                "tjuae": "^1.0.0"
            },
            "contributes": {
                "acpAdapters": [
                    {
                        "id": "legacy-acp",
                        "name": "Legacy ACP",
                        "connectionType": "cli",
                        "cliCommand": "legacy-cli",
                        "acpArgs": ["--acp"],
                        "icon": "assets/adapter.png",
                        "apiKeyFields": [
                            {
                                "key": "LEGACY_API_KEY",
                                "label": "API Key",
                                "type": "password",
                                "required": true
                            }
                        ],
                        "yoloMode": {
                            "type": "session"
                        }
                    }
                ],
                "skills": [
                    {
                        "name": "review-skill",
                        "description": "Review code",
                        "file": "skills/review.md"
                    }
                ],
                "channelPlugins": [
                    {
                        "id": "legacy-channel",
                        "name": "Legacy Channel",
                        "description": "Legacy channel plugin",
                        "platform": "legacy-chat",
                        "entryPoint": "plugins/legacy-channel.js",
                        "icon": "assets/channel.png",
                        "credentialFields": [
                            {
                                "key": "legacyToken",
                                "label": "Legacy Token",
                                "type": "password",
                                "required": true
                            }
                        ],
                        "configFields": [
                            {
                                "key": "pollingInterval",
                                "label": "Polling Interval",
                                "type": "number",
                                "default": 30
                            }
                        ]
                    }
                ],
                "assistants": [
                    {
                        "id": "legacy-assistant",
                        "name": "Legacy Assistant",
                        "avatar": "assets/assistant.png",
                        "agentId": "cc126dd5",
                        "contextFile": "assistants/context.md",
                        "models": ["gemini-2.0-flash"],
                        "enabledSkills": ["review-skill"],
                        "prompts": ["Review the diff"]
                    }
                ],
                "mcpServers": [
                    {
                        "name": "legacy-mcp",
                        "description": "Legacy MCP",
                        "enabled": false,
                        "transport": {
                            "type": "stdio",
                            "command": "npx",
                            "args": ["-y", "legacy-mcp"]
                        }
                    }
                ],
                "themes": [
                    {
                        "id": "legacy-dark",
                        "name": "Legacy Dark",
                        "file": "themes/dark.css",
                        "cover": "assets/theme-cover.png"
                    }
                ]
            }
        }))
        .unwrap(),
    )
    .unwrap();

    ext_root
}

async fn build_app_with_extension_root(ext_root: &std::path::Path) -> (axum::Router, AppServices) {
    let db = tjuaeui_db::init_database_memory().await.unwrap();
    let data_dir = ext_root.join("..").join("data");
    let config = AppConfig {
        data_dir: data_dir.clone(),
        work_dir: data_dir,
        app_version: "1.0.0".to_string(),
        ..Default::default()
    };
    let services = AppServices::from_config(db, &config).await.unwrap();
    let (states, _) = build_module_states(&services).await.expect("build module states");
    states
        .extension
        .registry
        .initialize_with_scan_paths(vec![ScanPath {
            path: ext_root.to_path_buf(),
            source: ExtensionSource::Local,
        }])
        .await
        .unwrap();
    let router = create_router_with_states(&services, states);
    (router, services)
}
// ---------------------------------------------------------------------------
// EQ — Extension query (unauthenticated → rejected)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn eq_unauthenticated_access_rejected() {
    let (app, _) = build_app().await;
    let resp = app.oneshot(common::get_request("/api/extensions")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let json = body_json(resp).await;
    assert_eq!(json["code"], "UNAUTHORIZED");
}

// ---------------------------------------------------------------------------
// EQ — Extension query (authenticated)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn eq1_get_loaded_extensions_empty() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app.oneshot(get_with_token("/api/extensions", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    assert!(json["data"].is_array());
}

#[tokio::test]
async fn eq3_get_themes_empty() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(get_with_token("/api/extensions/themes", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
}

#[tokio::test]
async fn eq4_get_assistants_empty() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(get_with_token("/api/extensions/assistants", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn eq5_get_acp_adapters_empty() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(get_with_token("/api/extensions/acp-adapters", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn eq7_get_mcp_servers_empty() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(get_with_token("/api/extensions/mcp-servers", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn eq8_get_skills_empty() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(get_with_token("/api/extensions/skills", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn eq8b_get_channel_plugins_empty() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(get_with_token("/api/extensions/channel-plugins", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    assert_eq!(json["data"], json!([]));
}

#[tokio::test]
async fn eq9_get_settings_tabs_empty() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(get_with_token("/api/extensions/settings-tabs", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn eq10_get_webui_empty() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(get_with_token("/api/extensions/webui", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn eq11_get_agent_activity() {
    let (mut app, services) = build_app().await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(get_with_token("/api/extensions/agent-activity", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
}

// ---------------------------------------------------------------------------
// EQ-12: i18n
// ---------------------------------------------------------------------------

#[tokio::test]
async fn eq12_get_i18n_for_locale() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .clone()
        .oneshot(json_with_token(
            "POST",
            "/api/extensions/i18n",
            json!({"locale": "zh-CN"}),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    // With no extensions loaded, i18n data should be an empty object
    assert!(json["data"].is_object());
}

// ---------------------------------------------------------------------------
// EQ-13, EQ-14: Permissions / risk level for nonexistent → 404
// ---------------------------------------------------------------------------

#[tokio::test]
async fn eq13_permissions_not_found() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .clone()
        .oneshot(json_with_token(
            "POST",
            "/api/extensions/permissions",
            json!({"name": "nonexistent-ext"}),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn eq14_risk_level_not_found() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(json_with_token(
            "POST",
            "/api/extensions/risk-level",
            json!({"name": "nonexistent-ext"}),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn eq15_legacy_asset_contribution_endpoints_are_removed() {
    let tmp = TempDir::new().unwrap();
    let ext_root = write_legacy_extension_fixture(&tmp);
    let (mut app, services) = build_app_with_extension_root(&ext_root).await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let skills_resp = app
        .clone()
        .oneshot(get_with_token("/api/extensions/skills", &token))
        .await
        .unwrap();
    assert_eq!(skills_resp.status(), StatusCode::NOT_FOUND);

    let acp_resp = app
        .clone()
        .oneshot(get_with_token("/api/extensions/acp-adapters", &token))
        .await
        .unwrap();
    assert_eq!(acp_resp.status(), StatusCode::NOT_FOUND);

    let mcp_resp = app
        .oneshot(get_with_token("/api/extensions/mcp-servers", &token))
        .await
        .unwrap();
    assert_eq!(mcp_resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn eq16_legacy_assistant_and_theme_contributions_are_ignored() {
    let tmp = TempDir::new().unwrap();
    let ext_root = write_legacy_extension_fixture(&tmp);
    let (mut app, services) = build_app_with_extension_root(&ext_root).await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let assistant_resp = app
        .clone()
        .oneshot(get_with_token("/api/extensions/assistants", &token))
        .await
        .unwrap();
    assert_eq!(assistant_resp.status(), StatusCode::NOT_FOUND);

    let theme_resp = app
        .oneshot(get_with_token("/api/extensions/themes", &token))
        .await
        .unwrap();
    assert_eq!(theme_resp.status(), StatusCode::OK);
    let theme_json = body_json(theme_resp).await;
    let themes = theme_json["data"].as_array().unwrap();
    assert!(themes.is_empty());
}

#[tokio::test]
async fn eq17_legacy_channel_plugin_contribution_is_ignored() {
    let tmp = TempDir::new().unwrap();
    let ext_root = write_legacy_extension_fixture(&tmp);
    let (mut app, services) = build_app_with_extension_root(&ext_root).await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(get_with_token("/api/extensions/channel-plugins", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["data"], json!([]));
}

#[tokio::test]
async fn eq18_channel_status_lists_only_builtin_plugins() {
    let tmp = TempDir::new().unwrap();
    let ext_root = write_legacy_extension_fixture(&tmp);
    let (mut app, services) = build_app_with_extension_root(&ext_root).await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(get_with_token("/api/channel/plugins", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let plugins = json["data"].as_array().unwrap();

    let telegram = plugins.iter().find(|plugin| plugin["type"] == "telegram").unwrap();
    assert_eq!(telegram["enabled"], false);
    assert_eq!(telegram["connected"], false);
    assert_eq!(telegram["is_extension"], false);

    assert!(plugins.iter().all(|plugin| plugin["type"] != "legacy-channel"));
}

#[tokio::test]
async fn eq19_channel_status_ignores_unknown_legacy_persisted_row() {
    let tmp = TempDir::new().unwrap();
    let ext_root = write_legacy_extension_fixture(&tmp);
    let (mut app, services) = build_app_with_extension_root(&ext_root).await;
    let repo = SqliteChannelRepository::new(services.database.pool().clone());
    let now = now_ms();
    repo.upsert_plugin(&tjuaeui_db::models::ChannelPluginRow {
        id: "legacy-channel".to_string(),
        r#type: "legacy-channel".to_string(),
        name: "Legacy Channel Persisted".to_string(),
        enabled: true,
        config: "{\"token\":\"secret\"}".to_string(),
        status: Some("running".to_string()),
        last_connected: Some(now),
        created_at: now,
        updated_at: now,
    })
    .await
    .unwrap();

    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;
    let resp = app
        .oneshot(get_with_token("/api/channel/plugins", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let plugins = json["data"].as_array().unwrap();
    assert!(plugins.iter().all(|plugin| plugin["type"] != "legacy-channel"));
}

#[tokio::test]
async fn eq20_enable_legacy_extension_channel_fails_closed() {
    let tmp = TempDir::new().unwrap();
    let ext_root = write_legacy_extension_fixture(&tmp);
    let (mut app, services) = build_app_with_extension_root(&ext_root).await;
    let repo = SqliteChannelRepository::new(services.database.pool().clone());
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let enable_resp = app
        .clone()
        .oneshot(json_with_token(
            "POST",
            "/api/channel/plugins/enable",
            json!({
                "plugin_id": "legacy-channel",
                "config": {
                    "legacyToken": "secret-token",
                    "pollingInterval": 42
                }
            }),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(enable_resp.status(), StatusCode::OK);
    let enable_json = body_json(enable_resp).await;
    assert_eq!(enable_json["data"]["success"], false);
    assert!(repo.get_plugin("legacy-channel").await.unwrap().is_none());

    let status_resp = app
        .oneshot(get_with_token("/api/channel/plugins", &token))
        .await
        .unwrap();
    assert_eq!(status_resp.status(), StatusCode::OK);
    let status_json = body_json(status_resp).await;
    let plugins = status_json["data"].as_array().unwrap();
    assert!(plugins.iter().all(|plugin| plugin["type"] != "legacy-channel"));
}

#[tokio::test]
async fn eq21_disable_unknown_legacy_extension_channel_fails_closed() {
    let tmp = TempDir::new().unwrap();
    let ext_root = write_legacy_extension_fixture(&tmp);
    let (mut app, services) = build_app_with_extension_root(&ext_root).await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let _ = app
        .clone()
        .oneshot(json_with_token(
            "POST",
            "/api/channel/plugins/enable",
            json!({
                "plugin_id": "legacy-channel",
                "config": {
                    "legacyToken": "secret-token"
                }
            }),
            &token,
            &csrf,
        ))
        .await
        .unwrap();

    let disable_resp = app
        .clone()
        .oneshot(json_with_token(
            "POST",
            "/api/channel/plugins/disable",
            json!({
                "plugin_id": "legacy-channel"
            }),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(disable_resp.status(), StatusCode::OK);
    let disable_json = body_json(disable_resp).await;
    assert_eq!(disable_json["data"]["success"], false);

    let status_resp = app
        .oneshot(get_with_token("/api/channel/plugins", &token))
        .await
        .unwrap();
    assert_eq!(status_resp.status(), StatusCode::OK);
    let status_json = body_json(status_resp).await;
    let plugins = status_json["data"].as_array().unwrap();
    assert!(plugins.iter().all(|plugin| plugin["type"] != "legacy-channel"));
}

// ---------------------------------------------------------------------------
// EM — Extension management
// ---------------------------------------------------------------------------

#[tokio::test]
async fn em3_enable_nonexistent_returns_not_found() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(json_with_token(
            "POST",
            "/api/extensions/enable",
            json!({"name": "nonexistent"}),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn em4_disable_nonexistent_returns_not_found() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app
        .oneshot(json_with_token(
            "POST",
            "/api/extensions/disable",
            json!({"name": "nonexistent"}),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// AUTH — Auth protection on skill routes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn auth_skills_unauthenticated() {
    let (app, _) = build_app().await;
    let resp = app.oneshot(common::get_request("/api/skills")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let json = body_json(resp).await;
    assert_eq!(json["code"], "UNAUTHORIZED");
}

// ---------------------------------------------------------------------------
// SL — Skill listing (E1 / `GET /api/skills`)
// ---------------------------------------------------------------------------

fn write_skill(dir: &std::path::Path, name: &str, description: &str) {
    let skill = dir.join(name);
    std::fs::create_dir_all(&skill).unwrap();
    let frontmatter = format!("---\nname: {name}\ndescription: {description}\n---\nBody");
    std::fs::write(skill.join("SKILL.md"), frontmatter).unwrap();
}

#[tokio::test]
async fn sl1_raw_runtime_directories_are_not_auto_adopted() {
    let tmp = TempDir::new().unwrap();
    let (mut app, services, paths) = build_app_with_skill_paths(tmp.path()).await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    write_skill(&paths.user_skills_dir, "review", "Local review skill");

    let resp = app.oneshot(get_with_token("/api/skills", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    let arr = json["data"].as_array().unwrap();
    assert!(arr.is_empty());
}

#[tokio::test]
async fn sl3_list_skills_returns_empty_array_when_no_skills() {
    let tmp = TempDir::new().unwrap();
    let (mut app, services, _paths) = build_app_with_skill_paths(tmp.path()).await;
    let (token, _csrf) = setup_and_login(&mut app, &services, "user1", "pass1").await;

    let resp = app.oneshot(get_with_token("/api/skills", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    assert_eq!(json["data"].as_array().unwrap().len(), 0);
}
