use crate::error::DbError;
use crate::models::SkillRow;

/// Runtime skill projection data access abstraction.
#[async_trait::async_trait]
pub trait ISkillRepository: Send + Sync {
    /// Returns active skills ordered by most recent update first.
    async fn list(&self) -> Result<Vec<SkillRow>, DbError>;

    /// Finds an active skill by name.
    async fn find_by_name(&self, name: &str) -> Result<Option<SkillRow>, DbError>;

    /// Finds a skill by name, including soft-deleted rows.
    async fn find_by_name_any(&self, name: &str) -> Result<Option<SkillRow>, DbError>;

    /// Creates or updates a user skill by name and clears soft-delete state.
    async fn upsert(&self, params: UpsertSkillParams<'_>) -> Result<SkillRow, DbError>;

    /// Soft-deletes an active skill by name.
    async fn delete_by_name(&self, name: &str) -> Result<SkillRow, DbError>;
}

/// Parameters for creating or updating a skill row.
#[derive(Debug, Clone)]
pub struct UpsertSkillParams<'a> {
    pub name: &'a str,
    pub description: Option<&'a str>,
    pub path: &'a str,
    pub source: &'a str,
    pub enabled: bool,
}
