use sqlx::SqlitePool;

use crate::error::DbError;
use crate::models::SkillUserPreferenceRow;
use crate::repository::{ISkillUserPreferenceRepository, UpsertSkillUserPreferenceParams};

#[derive(Clone, Debug)]
pub struct SqliteSkillUserPreferenceRepository {
    pool: SqlitePool,
}

impl SqliteSkillUserPreferenceRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl ISkillUserPreferenceRepository for SqliteSkillUserPreferenceRepository {
    async fn list(&self) -> Result<Vec<SkillUserPreferenceRow>, DbError> {
        Ok(
            sqlx::query_as("SELECT * FROM skill_user_preferences ORDER BY source, namespace, slug")
                .fetch_all(&self.pool)
                .await?,
        )
    }

    async fn list_enabled(&self) -> Result<Vec<SkillUserPreferenceRow>, DbError> {
        Ok(
            sqlx::query_as("SELECT * FROM skill_user_preferences WHERE enabled = 1 ORDER BY source, namespace, slug")
                .fetch_all(&self.pool)
                .await?,
        )
    }

    async fn list_auto_inject(&self) -> Result<Vec<SkillUserPreferenceRow>, DbError> {
        Ok(sqlx::query_as("SELECT * FROM skill_user_preferences WHERE enabled = 1 AND auto_inject = 1 ORDER BY source, namespace, slug")
            .fetch_all(&self.pool).await?)
    }

    async fn get(&self, source: &str, namespace: &str, slug: &str) -> Result<Option<SkillUserPreferenceRow>, DbError> {
        Ok(
            sqlx::query_as("SELECT * FROM skill_user_preferences WHERE source = ? AND namespace = ? AND slug = ?")
                .bind(source)
                .bind(namespace)
                .bind(slug)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    async fn upsert(&self, params: UpsertSkillUserPreferenceParams<'_>) -> Result<SkillUserPreferenceRow, DbError> {
        if params.auto_inject && !params.enabled {
            return Err(DbError::Conflict("auto-inject skill must be enabled".into()));
        }
        let now = tjuaeui_common::now_ms();
        sqlx::query(
            "INSERT INTO skill_user_preferences \
             (source, namespace, slug, selected_version, follow_latest, enabled, auto_inject, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(source, namespace, slug) DO UPDATE SET \
             selected_version = excluded.selected_version, follow_latest = excluded.follow_latest, \
             enabled = excluded.enabled, auto_inject = excluded.auto_inject, updated_at = excluded.updated_at",
        )
        .bind(params.source)
        .bind(params.namespace)
        .bind(params.slug)
        .bind(params.selected_version)
        .bind(params.follow_latest)
        .bind(params.enabled)
        .bind(params.auto_inject)
        .bind(now)
        .execute(&self.pool)
        .await?;
        self.get(params.source, params.namespace, params.slug)
            .await?
            .ok_or_else(|| DbError::NotFound("skill user preference".into()))
    }

    async fn delete(&self, source: &str, namespace: &str, slug: &str) -> Result<bool, DbError> {
        Ok(
            sqlx::query("DELETE FROM skill_user_preferences WHERE source = ? AND namespace = ? AND slug = ?")
                .bind(source)
                .bind(namespace)
                .bind(slug)
                .execute(&self.pool)
                .await?
                .rows_affected()
                > 0,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::init_database_memory;

    #[tokio::test]
    async fn initial_hub_preset_is_explicit_and_future_skills_have_no_preference() {
        let database = init_database_memory().await.unwrap();
        let repository = SqliteSkillUserPreferenceRepository::new(database.pool().clone());

        let rows = repository
            .list()
            .await
            .unwrap()
            .into_iter()
            .filter(|row| row.source == "tjuae-hub" && row.namespace == "official")
            .collect::<Vec<_>>();
        let slugs = rows.iter().map(|row| row.slug.as_str()).collect::<BTreeSet<_>>();
        assert_eq!(
            slugs,
            BTreeSet::from([
                "cron",
                "mermaid",
                "pdf",
                "skill-creator",
                "story-roleplay",
                "tjuaeui-config",
                "tjuaeui-troubleshooting",
                "tjuaeui-webui-public",
                "tjuaeui-webui-setup",
                "weixin-file-send",
                "x-recruiter",
                "xiaohongshu-recruiter",
            ])
        );
        assert!(rows.iter().all(|row| row.enabled));
        assert_eq!(
            rows.iter()
                .filter(|row| row.auto_inject)
                .map(|row| row.slug.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["cron", "skill-creator", "tjuaeui-config"])
        );
        assert!(
            repository
                .get("tjuae-hub", "official", "future-skill")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn identity_includes_provider_and_auto_inject_requires_enabled() {
        let database = init_database_memory().await.unwrap();
        let repository = SqliteSkillUserPreferenceRepository::new(database.pool().clone());
        for (source, inject) in [("skillhub", true), ("tjuae-hub", false)] {
            repository
                .upsert(UpsertSkillUserPreferenceParams {
                    source,
                    namespace: "official",
                    slug: "writer",
                    selected_version: Some("1.2.0"),
                    follow_latest: false,
                    enabled: true,
                    auto_inject: inject,
                })
                .await
                .unwrap();
        }
        let enabled = repository.list_enabled().await.unwrap();
        assert_eq!(enabled.iter().filter(|row| row.slug == "writer").count(), 2);
        let injected = repository.list_auto_inject().await.unwrap();
        assert_eq!(injected.iter().filter(|row| row.slug == "writer").count(), 1);
        assert!(
            enabled
                .iter()
                .any(|row| { row.source == "tjuae-hub" && row.slug == "cron" && row.auto_inject })
        );
        assert!(
            repository
                .upsert(UpsertSkillUserPreferenceParams {
                    source: "mine",
                    namespace: "",
                    slug: "broken",
                    selected_version: None,
                    follow_latest: true,
                    enabled: false,
                    auto_inject: true,
                })
                .await
                .is_err()
        );
    }
}
