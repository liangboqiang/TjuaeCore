use crate::error::DbError;
use crate::models::SkillUserPreferenceRow;

pub struct UpsertSkillUserPreferenceParams<'a> {
    pub source: &'a str,
    pub namespace: &'a str,
    pub slug: &'a str,
    pub selected_version: Option<&'a str>,
    pub follow_latest: bool,
    pub enabled: bool,
    pub auto_inject: bool,
}

#[async_trait::async_trait]
pub trait ISkillUserPreferenceRepository: Send + Sync {
    async fn list(&self) -> Result<Vec<SkillUserPreferenceRow>, DbError>;
    async fn list_enabled(&self) -> Result<Vec<SkillUserPreferenceRow>, DbError>;
    async fn list_auto_inject(&self) -> Result<Vec<SkillUserPreferenceRow>, DbError>;
    async fn get(&self, source: &str, namespace: &str, slug: &str) -> Result<Option<SkillUserPreferenceRow>, DbError>;
    async fn upsert(&self, params: UpsertSkillUserPreferenceParams<'_>) -> Result<SkillUserPreferenceRow, DbError>;
    async fn delete(&self, source: &str, namespace: &str, slug: &str) -> Result<bool, DbError>;
}
