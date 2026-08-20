use crate::error::DbError;
use crate::models::AssistantUserPreferenceRow;

pub struct UpsertAssistantUserPreferenceParams<'a> {
    pub source: &'a str,
    pub namespace: &'a str,
    pub slug: &'a str,
    pub selected_version: Option<&'a str>,
    pub follow_latest: bool,
    pub enabled: bool,
    pub activation_status: &'a str,
    pub activation_fingerprint: Option<&'a str>,
    pub resource_bindings: &'a str,
    pub runtime_overrides: &'a str,
    pub sort_order: i32,
    pub last_used_at: Option<tjuaeui_common::TimestampMs>,
}

#[async_trait::async_trait]
pub trait IAssistantUserPreferenceRepository: Send + Sync {
    async fn list(&self) -> Result<Vec<AssistantUserPreferenceRow>, DbError>;
    async fn list_enabled(&self) -> Result<Vec<AssistantUserPreferenceRow>, DbError>;
    async fn get(
        &self,
        source: &str,
        namespace: &str,
        slug: &str,
    ) -> Result<Option<AssistantUserPreferenceRow>, DbError>;
    async fn upsert(
        &self,
        params: UpsertAssistantUserPreferenceParams<'_>,
    ) -> Result<AssistantUserPreferenceRow, DbError>;
    async fn delete(&self, source: &str, namespace: &str, slug: &str) -> Result<bool, DbError>;
}
