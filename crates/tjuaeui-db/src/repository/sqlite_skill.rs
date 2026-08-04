use sqlx::SqlitePool;

use crate::error::DbError;
use crate::models::SkillRow;
use crate::repository::skill::{ISkillRepository, UpsertSkillParams};

/// SQLite-backed implementation of [`ISkillRepository`].
#[derive(Clone, Debug)]
pub struct SqliteSkillRepository {
    pool: SqlitePool,
}

impl SqliteSkillRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl ISkillRepository for SqliteSkillRepository {
    async fn list(&self) -> Result<Vec<SkillRow>, DbError> {
        let rows = sqlx::query_as::<_, SkillRow>(
            "SELECT * FROM skills WHERE deleted_at IS NULL AND enabled = 1 ORDER BY updated_at DESC, name ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn find_by_name(&self, name: &str) -> Result<Option<SkillRow>, DbError> {
        let row =
            sqlx::query_as::<_, SkillRow>("SELECT * FROM skills WHERE name = ? AND deleted_at IS NULL AND enabled = 1")
                .bind(name)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row)
    }

    async fn find_by_name_any(&self, name: &str) -> Result<Option<SkillRow>, DbError> {
        let row = sqlx::query_as::<_, SkillRow>("SELECT * FROM skills WHERE name = ?")
            .bind(name)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row)
    }

    async fn upsert(&self, params: UpsertSkillParams<'_>) -> Result<SkillRow, DbError> {
        let now = tjuaeui_common::now_ms();
        let existing = self.find_by_name_any(params.name).await?;
        let id = existing
            .as_ref()
            .map(|row| row.id.clone())
            .unwrap_or_else(|| tjuaeui_common::generate_prefixed_id("skill"));
        let created_at = existing.as_ref().map(|row| row.created_at).unwrap_or(now);

        sqlx::query(
            "INSERT INTO skills \
                (id, name, description, path, source, enabled, deleted_at, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, NULL, ?, ?) \
             ON CONFLICT(name) DO UPDATE SET \
                description = excluded.description, \
                path = excluded.path, \
                source = excluded.source, \
                enabled = excluded.enabled, \
                deleted_at = NULL, \
                updated_at = excluded.updated_at",
        )
        .bind(&id)
        .bind(params.name)
        .bind(params.description)
        .bind(params.path)
        .bind(params.source)
        .bind(params.enabled)
        .bind(created_at)
        .bind(now)
        .execute(&self.pool)
        .await?;

        self.find_by_name_any(params.name)
            .await?
            .ok_or_else(|| DbError::NotFound(format!("skill '{}' was not found after upsert", params.name)))
    }

    async fn delete_by_name(&self, name: &str) -> Result<SkillRow, DbError> {
        let now = tjuaeui_common::now_ms();
        let result = sqlx::query(
            "UPDATE skills SET enabled = 0, deleted_at = ?, updated_at = ? WHERE name = ? AND deleted_at IS NULL",
        )
        .bind(now)
        .bind(now)
        .bind(name)
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            return Err(DbError::NotFound(format!("skill '{name}'")));
        }

        self.find_by_name_any(name)
            .await?
            .ok_or_else(|| DbError::NotFound(format!("skill '{name}'")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init_database_memory;

    async fn setup() -> (SqliteSkillRepository, crate::Database) {
        let db = init_database_memory().await.unwrap();
        let repo = SqliteSkillRepository::new(db.pool().clone());
        (repo, db)
    }

    #[tokio::test]
    async fn upsert_restores_soft_deleted_skill() {
        let (repo, _db) = setup().await;

        let created = repo
            .upsert(UpsertSkillParams {
                name: "sample",
                description: Some("Old"),
                path: "/tmp/old",
                source: "user",
                enabled: true,
            })
            .await
            .unwrap();
        repo.delete_by_name("sample").await.unwrap();

        let restored = repo
            .upsert(UpsertSkillParams {
                name: "sample",
                description: Some("New"),
                path: "/tmp/new",
                source: "user",
                enabled: true,
            })
            .await
            .unwrap();

        assert_eq!(restored.id, created.id);
        assert_eq!(restored.description.as_deref(), Some("New"));
        assert_eq!(restored.path, "/tmp/new");
        assert_eq!(restored.deleted_at, None);
        assert!(repo.find_by_name("sample").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn list_filters_soft_deleted_skills() {
        let (repo, _db) = setup().await;

        repo.upsert(UpsertSkillParams {
            name: "active",
            description: None,
            path: "/tmp/active",
            source: "user",
            enabled: true,
        })
        .await
        .unwrap();
        repo.upsert(UpsertSkillParams {
            name: "deleted",
            description: None,
            path: "/tmp/deleted",
            source: "user",
            enabled: true,
        })
        .await
        .unwrap();
        repo.delete_by_name("deleted").await.unwrap();

        let names: Vec<_> = repo.list().await.unwrap().into_iter().map(|row| row.name).collect();
        assert_eq!(names, vec!["active"]);
        assert!(repo.find_by_name_any("deleted").await.unwrap().is_some());
    }
}
