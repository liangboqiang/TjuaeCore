//! Black-box integration tests for ISettingsRepository.
//!
//! Tests exercise the public trait interface against an in-memory SQLite database.

use std::sync::Arc;

use tjuaeui_db::{ISettingsRepository, SqliteSettingsRepository, UpsertSystemSettingsParams, init_database_memory};

async fn repo() -> Arc<dyn ISettingsRepository> {
    let db = init_database_memory().await.unwrap();
    Arc::new(SqliteSettingsRepository::new(db.pool().clone()))
}

fn settings(
    language: &str,
    notification_enabled: bool,
    cron_notification_enabled: bool,
    command_queue_enabled: bool,
    save_upload_to_workspace: bool,
) -> UpsertSystemSettingsParams<'_> {
    UpsertSystemSettingsParams {
        language,
        notification_enabled,
        cron_notification_enabled,
        command_queue_enabled,
        save_upload_to_workspace,
        network_proxy_mode: "follow_system",
        network_proxy_url: None,
        network_proxy_no_proxy: "localhost,127.0.0.1,::1",
    }
}

// -- Get default state --

#[tokio::test]
async fn get_settings_returns_none_when_no_row_exists() {
    let r = repo().await;
    assert!(r.get_settings().await.unwrap().is_none());
}

// -- Upsert creates a row --

#[tokio::test]
async fn upsert_creates_settings_with_given_values() {
    let r = repo().await;
    let s = r
        .upsert_settings(settings("zh-CN", false, true, true, false))
        .await
        .unwrap();

    assert_eq!(s.language, "zh-CN");
    assert!(!s.notification_enabled);
    assert!(s.cron_notification_enabled);
    assert!(s.command_queue_enabled);
    assert!(!s.save_upload_to_workspace);
    assert!(s.updated_at > 0);
}

// -- Upsert then get round-trip --

#[tokio::test]
async fn upsert_then_get_returns_consistent_data() {
    let r = repo().await;
    r.upsert_settings(settings("en-US", true, false, false, true))
        .await
        .unwrap();

    let s = r.get_settings().await.unwrap().unwrap();
    assert_eq!(s.language, "en-US");
    assert!(s.notification_enabled);
    assert!(!s.cron_notification_enabled);
    assert!(!s.command_queue_enabled);
    assert!(s.save_upload_to_workspace);
}

// -- Upsert overwrites --

#[tokio::test]
async fn upsert_overwrites_previous_settings() {
    let r = repo().await;
    r.upsert_settings(settings("en-US", true, false, false, false))
        .await
        .unwrap();
    r.upsert_settings(settings("ja-JP", false, true, true, true))
        .await
        .unwrap();

    let s = r.get_settings().await.unwrap().unwrap();
    assert_eq!(s.language, "ja-JP");
    assert!(!s.notification_enabled);
    assert!(s.cron_notification_enabled);
    assert!(s.command_queue_enabled);
    assert!(s.save_upload_to_workspace);
}

// -- updated_at advances on each upsert --

#[tokio::test]
async fn upsert_advances_updated_at() {
    let r = repo().await;
    let first = r
        .upsert_settings(settings("en-US", true, false, false, false))
        .await
        .unwrap();
    let second = r
        .upsert_settings(settings("en-US", true, false, false, false))
        .await
        .unwrap();

    assert!(second.updated_at >= first.updated_at);
}
