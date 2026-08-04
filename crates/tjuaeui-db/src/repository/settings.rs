use crate::error::DbError;
use crate::models::SystemSettings;

/// 写入单行系统设置所需的全部字段。
pub struct UpsertSystemSettingsParams<'a> {
    pub language: &'a str,
    pub notification_enabled: bool,
    pub cron_notification_enabled: bool,
    pub command_queue_enabled: bool,
    pub save_upload_to_workspace: bool,
    pub network_proxy_mode: &'a str,
    pub network_proxy_url: Option<&'a str>,
    pub network_proxy_no_proxy: &'a str,
}

/// System settings data access abstraction.
///
/// The `system_settings` table holds a single row (id=1).
/// `get_settings` returns `None` if no row exists yet (caller uses defaults).
/// `upsert_settings` inserts or replaces the single row.
#[async_trait::async_trait]
pub trait ISettingsRepository: Send + Sync {
    /// Returns the settings row, or `None` if no settings have been persisted.
    async fn get_settings(&self) -> Result<Option<SystemSettings>, DbError>;

    /// Inserts or replaces the single settings row.
    async fn upsert_settings(&self, params: UpsertSystemSettingsParams<'_>) -> Result<SystemSettings, DbError>;
}
