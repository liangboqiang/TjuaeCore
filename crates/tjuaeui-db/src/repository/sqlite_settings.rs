use sqlx::SqlitePool;

use crate::error::DbError;
use crate::models::SystemSettings;
use crate::repository::{ISettingsRepository, UpsertSystemSettingsParams};

/// SQLite-backed implementation of [`ISettingsRepository`].
#[derive(Clone, Debug)]
pub struct SqliteSettingsRepository {
    pool: SqlitePool,
}

impl SqliteSettingsRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl ISettingsRepository for SqliteSettingsRepository {
    async fn get_settings(&self) -> Result<Option<SystemSettings>, DbError> {
        let row = sqlx::query_as::<_, SystemSettings>("SELECT * FROM system_settings WHERE id = 1")
            .fetch_optional(&self.pool)
            .await?;

        Ok(row)
    }

    async fn upsert_settings(&self, params: UpsertSystemSettingsParams<'_>) -> Result<SystemSettings, DbError> {
        let now = tjuaeui_common::now_ms();

        sqlx::query(
            "INSERT INTO system_settings \
                (id, language, notification_enabled, cron_notification_enabled, \
                 command_queue_enabled, save_upload_to_workspace, network_proxy_mode, \
                 network_proxy_url, network_proxy_no_proxy, updated_at) \
             VALUES (1, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET \
                language = excluded.language, \
                notification_enabled = excluded.notification_enabled, \
                cron_notification_enabled = excluded.cron_notification_enabled, \
                command_queue_enabled = excluded.command_queue_enabled, \
                save_upload_to_workspace = excluded.save_upload_to_workspace, \
                network_proxy_mode = excluded.network_proxy_mode, \
                network_proxy_url = excluded.network_proxy_url, \
                network_proxy_no_proxy = excluded.network_proxy_no_proxy, \
                updated_at = excluded.updated_at",
        )
        .bind(params.language)
        .bind(params.notification_enabled)
        .bind(params.cron_notification_enabled)
        .bind(params.command_queue_enabled)
        .bind(params.save_upload_to_workspace)
        .bind(params.network_proxy_mode)
        .bind(params.network_proxy_url)
        .bind(params.network_proxy_no_proxy)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(SystemSettings {
            id: 1,
            language: params.language.to_owned(),
            notification_enabled: params.notification_enabled,
            cron_notification_enabled: params.cron_notification_enabled,
            command_queue_enabled: params.command_queue_enabled,
            save_upload_to_workspace: params.save_upload_to_workspace,
            network_proxy_mode: params.network_proxy_mode.to_owned(),
            network_proxy_url: params.network_proxy_url.map(str::to_owned),
            network_proxy_no_proxy: params.network_proxy_no_proxy.to_owned(),
            updated_at: now,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init_database_memory;

    async fn setup() -> (SqliteSettingsRepository, crate::Database) {
        let db = init_database_memory().await.unwrap();
        let repo = SqliteSettingsRepository::new(db.pool().clone());
        (repo, db)
    }

    fn params(
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

    #[tokio::test]
    async fn get_settings_returns_none_when_empty() {
        let (repo, _db) = setup().await;
        assert!(repo.get_settings().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn upsert_creates_settings() {
        let (repo, _db) = setup().await;
        let s = repo
            .upsert_settings(params("zh-CN", false, true, true, false))
            .await
            .unwrap();

        assert_eq!(s.id, 1);
        assert_eq!(s.language, "zh-CN");
        assert!(!s.notification_enabled);
        assert!(s.cron_notification_enabled);
        assert!(s.command_queue_enabled);
        assert!(!s.save_upload_to_workspace);
        assert_eq!(s.network_proxy_mode, "follow_system");
        assert!(s.updated_at > 0);
    }

    #[tokio::test]
    async fn upsert_then_get_returns_same() {
        let (repo, _db) = setup().await;
        repo.upsert_settings(params("en-US", true, false, false, true))
            .await
            .unwrap();

        let s = repo.get_settings().await.unwrap().unwrap();
        assert_eq!(s.language, "en-US");
        assert!(s.notification_enabled);
        assert!(!s.cron_notification_enabled);
        assert!(!s.command_queue_enabled);
        assert!(s.save_upload_to_workspace);
    }

    #[tokio::test]
    async fn upsert_overwrites_existing() {
        let (repo, _db) = setup().await;
        repo.upsert_settings(params("en-US", true, false, false, false))
            .await
            .unwrap();
        let s = repo
            .upsert_settings(params("ja-JP", false, true, true, true))
            .await
            .unwrap();

        assert_eq!(s.language, "ja-JP");
        assert!(!s.notification_enabled);
        assert!(s.cron_notification_enabled);
        assert!(s.command_queue_enabled);
        assert!(s.save_upload_to_workspace);

        // Verify persisted via get
        let fetched = repo.get_settings().await.unwrap().unwrap();
        assert_eq!(fetched.language, "ja-JP");
    }

    #[tokio::test]
    async fn upsert_persists_manual_proxy_settings() {
        let (repo, _db) = setup().await;
        let mut values = params("zh-CN", true, false, false, false);
        values.network_proxy_mode = "manual";
        values.network_proxy_url = Some("http://127.0.0.1:7897");
        values.network_proxy_no_proxy = "localhost,127.0.0.1,::1,intranet.example";

        let saved = repo.upsert_settings(values).await.unwrap();

        assert_eq!(saved.network_proxy_mode, "manual");
        assert_eq!(saved.network_proxy_url.as_deref(), Some("http://127.0.0.1:7897"));
        assert!(saved.network_proxy_no_proxy.contains("intranet.example"));
    }
}
