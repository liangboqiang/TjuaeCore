//! SQLite-backed assistant repositories.

use sqlx::SqlitePool;
use tjuaeui_common::now_ms;

use crate::error::DbError;
use crate::models::{
    AssistantDefinitionRow, AssistantOverlayRow, AssistantPreferenceRow, UpsertAssistantDefinitionParams,
    UpsertAssistantOverlayParams, UpsertAssistantPreferenceParams,
};
use crate::repository::assistant::{
    IAssistantDefinitionRepository, IAssistantOverlayRepository, IAssistantPreferenceRepository,
};

/// SQLite-backed implementation of [`IAssistantDefinitionRepository`].
#[derive(Clone, Debug)]
pub struct SqliteAssistantDefinitionRepository {
    pool: SqlitePool,
}

impl SqliteAssistantDefinitionRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

/// SQLite-backed implementation of [`IAssistantOverlayRepository`].
#[derive(Clone, Debug)]
pub struct SqliteAssistantOverlayRepository {
    pool: SqlitePool,
}

impl SqliteAssistantOverlayRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

/// SQLite-backed implementation of [`IAssistantPreferenceRepository`].
#[derive(Clone, Debug)]
pub struct SqliteAssistantPreferenceRepository {
    pool: SqlitePool,
}

impl SqliteAssistantPreferenceRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl IAssistantDefinitionRepository for SqliteAssistantDefinitionRepository {
    async fn list(&self) -> Result<Vec<AssistantDefinitionRow>, DbError> {
        let rows = sqlx::query_as::<_, AssistantDefinitionRow>(
            "SELECT * FROM assistant_definitions WHERE deleted_at IS NULL ORDER BY updated_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn list_including_deleted(&self) -> Result<Vec<AssistantDefinitionRow>, DbError> {
        let rows =
            sqlx::query_as::<_, AssistantDefinitionRow>("SELECT * FROM assistant_definitions ORDER BY updated_at DESC")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows)
    }

    async fn get_by_assistant_id(&self, assistant_id: &str) -> Result<Option<AssistantDefinitionRow>, DbError> {
        let row = sqlx::query_as::<_, AssistantDefinitionRow>(
            "SELECT * FROM assistant_definitions WHERE assistant_id = ? AND deleted_at IS NULL",
        )
        .bind(assistant_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn get_by_assistant_id_including_deleted(
        &self,
        assistant_id: &str,
    ) -> Result<Option<AssistantDefinitionRow>, DbError> {
        let row =
            sqlx::query_as::<_, AssistantDefinitionRow>("SELECT * FROM assistant_definitions WHERE assistant_id = ?")
                .bind(assistant_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row)
    }

    async fn get_by_id(&self, id: &str) -> Result<Option<AssistantDefinitionRow>, DbError> {
        let row = sqlx::query_as::<_, AssistantDefinitionRow>(
            "SELECT * FROM assistant_definitions WHERE id = ? AND deleted_at IS NULL",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn get_by_source_ref(
        &self,
        source: &str,
        source_ref: &str,
    ) -> Result<Option<AssistantDefinitionRow>, DbError> {
        let row = sqlx::query_as::<_, AssistantDefinitionRow>(
            "SELECT * FROM assistant_definitions WHERE source = ? AND source_ref = ? AND deleted_at IS NULL",
        )
        .bind(source)
        .bind(source_ref)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn get_by_source_ref_including_deleted(
        &self,
        source: &str,
        source_ref: &str,
    ) -> Result<Option<AssistantDefinitionRow>, DbError> {
        let row = sqlx::query_as::<_, AssistantDefinitionRow>(
            "SELECT * FROM assistant_definitions WHERE source = ? AND source_ref = ?",
        )
        .bind(source)
        .bind(source_ref)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn upsert(&self, params: &UpsertAssistantDefinitionParams<'_>) -> Result<AssistantDefinitionRow, DbError> {
        let now = now_ms();

        sqlx::query(
            "INSERT INTO assistant_definitions (
                id, assistant_id, source, owner_type, source_ref,
                name, name_i18n, description, description_i18n, avatar_type, avatar_value,
                agent_id, rule_resource_type, rule_resource_ref,
                recommended_prompts, recommended_prompts_i18n,
                default_model_mode, default_model_value,
                default_permission_mode, default_permission_value,
                default_thought_level_mode, default_thought_level_value,
                default_skills_mode, default_skill_ids, custom_skill_names,
                default_mcps_mode, default_mcp_ids,
                created_at, updated_at, deleted_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL)
            ON CONFLICT(id) DO UPDATE SET
                assistant_id = excluded.assistant_id,
                source = excluded.source,
                owner_type = excluded.owner_type,
                source_ref = excluded.source_ref,
                name = excluded.name,
                name_i18n = excluded.name_i18n,
                description = excluded.description,
                description_i18n = excluded.description_i18n,
                avatar_type = excluded.avatar_type,
                avatar_value = excluded.avatar_value,
                agent_id = excluded.agent_id,
                rule_resource_type = excluded.rule_resource_type,
                rule_resource_ref = excluded.rule_resource_ref,
                recommended_prompts = excluded.recommended_prompts,
                recommended_prompts_i18n = excluded.recommended_prompts_i18n,
                default_model_mode = excluded.default_model_mode,
                default_model_value = excluded.default_model_value,
                default_permission_mode = excluded.default_permission_mode,
                default_permission_value = excluded.default_permission_value,
                default_thought_level_mode = excluded.default_thought_level_mode,
                default_thought_level_value = excluded.default_thought_level_value,
                default_skills_mode = excluded.default_skills_mode,
                default_skill_ids = excluded.default_skill_ids,
                custom_skill_names = excluded.custom_skill_names,
                default_mcps_mode = excluded.default_mcps_mode,
                default_mcp_ids = excluded.default_mcp_ids,
                updated_at = excluded.updated_at,
                deleted_at = NULL",
        )
        .bind(params.id)
        .bind(params.assistant_id)
        .bind(params.source)
        .bind(params.owner_type)
        .bind(params.source_ref)
        .bind(params.name)
        .bind(params.name_i18n)
        .bind(params.description)
        .bind(params.description_i18n)
        .bind(params.avatar_type)
        .bind(params.avatar_value)
        .bind(params.agent_id)
        .bind(params.rule_resource_type)
        .bind(params.rule_resource_ref)
        .bind(params.recommended_prompts)
        .bind(params.recommended_prompts_i18n)
        .bind(params.default_model_mode)
        .bind(params.default_model_value)
        .bind(params.default_permission_mode)
        .bind(params.default_permission_value)
        .bind(params.default_thought_level_mode)
        .bind(params.default_thought_level_value)
        .bind(params.default_skills_mode)
        .bind(params.default_skill_ids)
        .bind(params.custom_skill_names)
        .bind(params.default_mcps_mode)
        .bind(params.default_mcp_ids)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;

        self.get_by_id(params.id).await?.ok_or_else(|| {
            DbError::Init(format!(
                "upsert did not produce assistant definition row for id '{}'",
                params.id
            ))
        })
    }

    async fn update_avatar_fields_preserving_deleted(
        &self,
        id: &str,
        avatar_type: &str,
        avatar_value: Option<&str>,
    ) -> Result<Option<AssistantDefinitionRow>, DbError> {
        let row = sqlx::query_as::<_, AssistantDefinitionRow>(
            "UPDATE assistant_definitions
             SET avatar_type = ?, avatar_value = ?, updated_at = ?
             WHERE id = ?
             RETURNING *",
        )
        .bind(avatar_type)
        .bind(avatar_value)
        .bind(now_ms())
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn soft_delete(&self, id: &str, deleted_at: i64) -> Result<bool, DbError> {
        let result = sqlx::query(
            "UPDATE assistant_definitions
             SET deleted_at = ?, updated_at = ?
             WHERE id = ? AND deleted_at IS NULL",
        )
        .bind(deleted_at)
        .bind(now_ms())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }
}

#[async_trait::async_trait]
impl IAssistantOverlayRepository for SqliteAssistantOverlayRepository {
    async fn get(&self, assistant_definition_id: &str) -> Result<Option<AssistantOverlayRow>, DbError> {
        let row = sqlx::query_as::<_, AssistantOverlayRow>(
            "SELECT * FROM assistant_overlays WHERE assistant_definition_id = ?",
        )
        .bind(assistant_definition_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn list(&self) -> Result<Vec<AssistantOverlayRow>, DbError> {
        let rows = sqlx::query_as::<_, AssistantOverlayRow>(
            "SELECT * FROM assistant_overlays ORDER BY sort_order, updated_at",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn upsert(&self, params: &UpsertAssistantOverlayParams<'_>) -> Result<AssistantOverlayRow, DbError> {
        let now = now_ms();
        sqlx::query(
            "INSERT INTO assistant_overlays (
                assistant_definition_id, enabled, sort_order, agent_id_override, last_used_at, created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(assistant_definition_id) DO UPDATE SET
                enabled = excluded.enabled,
                sort_order = excluded.sort_order,
                agent_id_override = excluded.agent_id_override,
                last_used_at = excluded.last_used_at,
                updated_at = excluded.updated_at",
        )
        .bind(params.assistant_definition_id)
        .bind(params.enabled)
        .bind(params.sort_order)
        .bind(params.agent_id_override)
        .bind(params.last_used_at)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;

        self.get(params.assistant_definition_id).await?.ok_or_else(|| {
            DbError::Init(format!(
                "upsert did not produce overlay row for assistant_definition_id '{}'",
                params.assistant_definition_id
            ))
        })
    }

    async fn delete(&self, assistant_definition_id: &str) -> Result<bool, DbError> {
        let result = sqlx::query("DELETE FROM assistant_overlays WHERE assistant_definition_id = ?")
            .bind(assistant_definition_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }
}

#[async_trait::async_trait]
impl IAssistantPreferenceRepository for SqliteAssistantPreferenceRepository {
    async fn get(&self, assistant_definition_id: &str) -> Result<Option<AssistantPreferenceRow>, DbError> {
        let row = sqlx::query_as::<_, AssistantPreferenceRow>(
            "SELECT * FROM assistant_preferences WHERE assistant_definition_id = ?",
        )
        .bind(assistant_definition_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn upsert(&self, params: &UpsertAssistantPreferenceParams<'_>) -> Result<AssistantPreferenceRow, DbError> {
        let now = now_ms();
        sqlx::query(
            "INSERT INTO assistant_preferences (
                assistant_definition_id, last_model_id, last_permission_value, last_thought_level_value, last_skill_ids,
                last_mcp_ids, created_at, updated_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(assistant_definition_id) DO UPDATE SET
                last_model_id = excluded.last_model_id,
                last_permission_value = excluded.last_permission_value,
                last_thought_level_value = excluded.last_thought_level_value,
                last_skill_ids = excluded.last_skill_ids,
                last_mcp_ids = excluded.last_mcp_ids,
                updated_at = excluded.updated_at",
        )
        .bind(params.assistant_definition_id)
        .bind(params.last_model_id)
        .bind(params.last_permission_value)
        .bind(params.last_thought_level_value)
        .bind(params.last_skill_ids)
        .bind(params.last_mcp_ids)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;

        self.get(params.assistant_definition_id).await?.ok_or_else(|| {
            DbError::Init(format!(
                "upsert did not produce preference row for assistant_definition_id '{}'",
                params.assistant_definition_id
            ))
        })
    }

    async fn delete(&self, assistant_definition_id: &str) -> Result<bool, DbError> {
        let result = sqlx::query("DELETE FROM assistant_preferences WHERE assistant_definition_id = ?")
            .bind(assistant_definition_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init_database_memory;

    async fn setup_v2() -> (
        SqliteAssistantDefinitionRepository,
        SqliteAssistantOverlayRepository,
        SqliteAssistantPreferenceRepository,
        crate::Database,
    ) {
        let db = init_database_memory().await.unwrap();
        let d = SqliteAssistantDefinitionRepository::new(db.pool().clone());
        let s = SqliteAssistantOverlayRepository::new(db.pool().clone());
        let p = SqliteAssistantPreferenceRepository::new(db.pool().clone());
        (d, s, p, db)
    }

    fn definition_params<'a>(id: &'a str, name: &'a str) -> UpsertAssistantDefinitionParams<'a> {
        UpsertAssistantDefinitionParams {
            id: "asstdef_u1",
            assistant_id: id,
            source: "user",
            owner_type: "user",
            source_ref: Some(id),
            name,
            name_i18n: r#"{"zh-CN":"助手"}"#,
            description: Some("desc"),
            description_i18n: "{}",
            avatar_type: "emoji",
            avatar_value: Some("🤖"),
            agent_id: "gemini",
            rule_resource_type: "user_file",
            rule_resource_ref: None,
            recommended_prompts: r#"["hello"]"#,
            recommended_prompts_i18n: "{}",
            default_model_mode: "auto",
            default_model_value: None,
            default_permission_mode: "fixed",
            default_permission_value: Some("workspace-write"),
            default_thought_level_mode: "auto",
            default_thought_level_value: None,
            default_skills_mode: "fixed",
            default_skill_ids: r#"["pdf","cron"]"#,
            custom_skill_names: r#"["my-custom-skill"]"#,
            default_mcps_mode: "auto",
            default_mcp_ids: "[]",
        }
    }

    #[tokio::test]
    async fn definition_upsert_then_get() {
        let (d, _s, _p, _db) = setup_v2().await;
        let row = d.upsert(&definition_params("u1", "User One")).await.unwrap();
        assert_eq!(row.assistant_id, "u1");
        assert_eq!(row.id, "asstdef_u1");
        assert_eq!(row.source, "user");
        assert_eq!(row.default_permission_mode, "fixed");

        let fetched = d.get_by_assistant_id("u1").await.unwrap().unwrap();
        assert_eq!(fetched.name, "User One");
        assert_eq!(fetched.rule_resource_type, "user_file");
        assert_eq!(fetched.avatar_type, "emoji");
        assert_eq!(fetched.avatar_value.as_deref(), Some("🤖"));
    }

    #[tokio::test]
    async fn state_upsert_then_list() {
        let (d, s, _p, _db) = setup_v2().await;
        let definition = d.upsert(&definition_params("u1", "User One")).await.unwrap();
        s.upsert(&UpsertAssistantOverlayParams {
            assistant_definition_id: &definition.id,
            enabled: false,
            sort_order: 9,
            agent_id_override: Some("claude"),
            last_used_at: Some(1234),
        })
        .await
        .unwrap();

        let list = s.list().await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].assistant_definition_id, definition.id);
        assert!(!list[0].enabled);
        assert_eq!(list[0].sort_order, 9);
        assert_eq!(list[0].agent_id_override.as_deref(), Some("claude"));
    }

    #[tokio::test]
    async fn preference_upsert_then_get() {
        let (d, _s, p, _db) = setup_v2().await;
        let definition = d.upsert(&definition_params("u1", "User One")).await.unwrap();
        let row = p
            .upsert(&UpsertAssistantPreferenceParams {
                assistant_definition_id: &definition.id,
                last_model_id: Some("gpt-4.1"),
                last_permission_value: Some("workspace-write"),
                last_thought_level_value: Some("high"),
                last_skill_ids: r#"["pdf"]"#,
                last_mcp_ids: r#"["mcp-1"]"#,
            })
            .await
            .unwrap();
        assert_eq!(row.last_model_id.as_deref(), Some("gpt-4.1"));
        assert_eq!(row.last_thought_level_value.as_deref(), Some("high"));

        let fetched = p.get(&definition.id).await.unwrap().unwrap();
        assert_eq!(fetched.last_skill_ids, r#"["pdf"]"#);
        assert_eq!(fetched.last_thought_level_value.as_deref(), Some("high"));
    }
}
