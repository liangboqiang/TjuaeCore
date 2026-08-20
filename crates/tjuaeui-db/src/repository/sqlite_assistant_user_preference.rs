use sqlx::SqlitePool;

use crate::error::DbError;
use crate::models::AssistantUserPreferenceRow;
use crate::repository::{IAssistantUserPreferenceRepository, UpsertAssistantUserPreferenceParams};

#[derive(Clone, Debug)]
pub struct SqliteAssistantUserPreferenceRepository {
    pool: SqlitePool,
}

impl SqliteAssistantUserPreferenceRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl IAssistantUserPreferenceRepository for SqliteAssistantUserPreferenceRepository {
    async fn list(&self) -> Result<Vec<AssistantUserPreferenceRow>, DbError> {
        Ok(
            sqlx::query_as("SELECT * FROM assistant_user_preferences ORDER BY sort_order, source, namespace, slug")
                .fetch_all(&self.pool)
                .await?,
        )
    }

    async fn list_enabled(&self) -> Result<Vec<AssistantUserPreferenceRow>, DbError> {
        Ok(sqlx::query_as(
            "SELECT * FROM assistant_user_preferences WHERE enabled = 1 ORDER BY sort_order, source, namespace, slug",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    async fn get(
        &self,
        source: &str,
        namespace: &str,
        slug: &str,
    ) -> Result<Option<AssistantUserPreferenceRow>, DbError> {
        Ok(
            sqlx::query_as("SELECT * FROM assistant_user_preferences WHERE source = ? AND namespace = ? AND slug = ?")
                .bind(source)
                .bind(namespace)
                .bind(slug)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    async fn upsert(
        &self,
        params: UpsertAssistantUserPreferenceParams<'_>,
    ) -> Result<AssistantUserPreferenceRow, DbError> {
        if !matches!(params.source, "mine" | "tjuae-hub") {
            return Err(DbError::Conflict("assistant source must be mine or tjuae-hub".into()));
        }
        if params.enabled && params.activation_status != "ready" {
            return Err(DbError::Conflict("enabled assistant activation must be ready".into()));
        }
        serde_json::from_str::<serde_json::Value>(params.resource_bindings)
            .map_err(|error| DbError::Conflict(format!("invalid assistant resource bindings: {error}")))?;
        serde_json::from_str::<serde_json::Value>(params.runtime_overrides)
            .map_err(|error| DbError::Conflict(format!("invalid assistant runtime overrides: {error}")))?;
        let now = tjuaeui_common::now_ms();
        sqlx::query(
            "INSERT INTO assistant_user_preferences \
             (source, namespace, slug, selected_version, follow_latest, enabled, activation_status, \
              activation_fingerprint, resource_bindings, runtime_overrides, sort_order, last_used_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(source, namespace, slug) DO UPDATE SET \
             selected_version = excluded.selected_version, follow_latest = excluded.follow_latest, \
             enabled = excluded.enabled, activation_status = excluded.activation_status, \
             activation_fingerprint = excluded.activation_fingerprint, resource_bindings = excluded.resource_bindings, \
             runtime_overrides = excluded.runtime_overrides, sort_order = excluded.sort_order, \
             last_used_at = excluded.last_used_at, updated_at = excluded.updated_at",
        )
        .bind(params.source)
        .bind(params.namespace)
        .bind(params.slug)
        .bind(params.selected_version)
        .bind(params.follow_latest)
        .bind(params.enabled)
        .bind(params.activation_status)
        .bind(params.activation_fingerprint)
        .bind(params.resource_bindings)
        .bind(params.runtime_overrides)
        .bind(params.sort_order)
        .bind(params.last_used_at)
        .bind(now)
        .execute(&self.pool)
        .await?;
        self.get(params.source, params.namespace, params.slug)
            .await?
            .ok_or_else(|| DbError::NotFound("assistant user preference".into()))
    }

    async fn delete(&self, source: &str, namespace: &str, slug: &str) -> Result<bool, DbError> {
        Ok(
            sqlx::query("DELETE FROM assistant_user_preferences WHERE source = ? AND namespace = ? AND slug = ?")
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
    use super::*;
    use crate::init_database_memory;

    #[tokio::test]
    async fn identity_is_source_namespace_slug_and_enable_requires_ready_activation() {
        let database = init_database_memory().await.unwrap();
        let repository = SqliteAssistantUserPreferenceRepository::new(database.pool().clone());
        let error = repository
            .upsert(UpsertAssistantUserPreferenceParams {
                source: "tjuae-hub",
                namespace: "official",
                slug: "writer",
                selected_version: Some("1.0.0"),
                follow_latest: false,
                enabled: true,
                activation_status: "pending",
                activation_fingerprint: None,
                resource_bindings: "{}",
                runtime_overrides: "{}",
                sort_order: 0,
                last_used_at: None,
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("activation must be ready"));

        let row = repository
            .upsert(UpsertAssistantUserPreferenceParams {
                source: "tjuae-hub",
                namespace: "official",
                slug: "writer",
                selected_version: Some("1.0.0"),
                follow_latest: false,
                enabled: true,
                activation_status: "ready",
                activation_fingerprint: Some("fingerprint"),
                resource_bindings: r#"{"skill":{"writer":"tjuae-hub:official:writer"}}"#,
                runtime_overrides: "{}",
                sort_order: 8,
                last_used_at: None,
            })
            .await
            .unwrap();
        assert!(row.enabled);
        assert_eq!(repository.list_enabled().await.unwrap(), vec![row]);
    }
}
