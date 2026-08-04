use std::borrow::Cow;
use std::path::Path;

use sqlx::migrate::Migrator;
use sqlx::sqlite::SqlitePoolOptions;

async fn run_migrations_through(pool: &sqlx::SqlitePool, max_version: i64) {
    let full = Migrator::new(Path::new("migrations")).await.unwrap();
    let migrations = full
        .migrations
        .iter()
        .filter(|migration| migration.version <= max_version)
        .cloned()
        .collect::<Vec<_>>();
    Migrator {
        migrations: Cow::Owned(migrations),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    }
    .run(pool)
    .await
    .unwrap();
}

async fn table_columns(pool: &sqlx::SqlitePool, table: &str) -> Vec<String> {
    sqlx::query_scalar(&format!("SELECT name FROM pragma_table_info('{table}')"))
        .fetch_all(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn migration_046_discards_exclusions_and_preserves_positive_skill_ids() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    run_migrations_through(&pool, 45).await;

    sqlx::query(
        "INSERT INTO users (id, username, password_hash, created_at, updated_at)
         VALUES ('user-1', 'user-1', 'unused', 1, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO conversations (id, user_id, name, type, extra, status, created_at, updated_at)
         VALUES ('conversation-1', 'user-1', 'test', 'acp', '{}', 'finished', 1, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO assistants
            (id, name, enabled_skills, disabled_builtin_skills, created_at, updated_at)
         VALUES
            ('legacy-assistant', 'Legacy', '[\"skill-a\"]', '[\"skill-b\"]', 1, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO assistant_definitions (
            id, assistant_id, source, owner_type, name, name_i18n,
            description_i18n, avatar_type, agent_id, rule_resource_type,
            recommended_prompts, recommended_prompts_i18n,
            default_model_mode, default_permission_mode,
            default_skills_mode, default_skill_ids, custom_skill_names,
            default_disabled_builtin_skill_ids, default_mcps_mode, default_mcp_ids,
            created_at, updated_at
         ) VALUES (
            'definition-1', 'assistant-1', 'user', 'user', 'Assistant', '{}',
            '{}', 'none', 'opencode', 'none',
            '[]', '{}',
            'auto', 'auto',
            'fixed', '[\"skill-a\"]', '[]',
            '[\"skill-b\"]', 'auto', '[]',
            1, 1
         )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO assistant_preferences (
            assistant_definition_id, last_skill_ids,
            last_disabled_builtin_skill_ids, last_mcp_ids, created_at, updated_at
         ) VALUES (
            'definition-1', '[\"skill-a\"]', '[\"skill-b\"]', '[]', 1, 1
         )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO conversation_assistant_snapshots (
            conversation_id, assistant_definition_id, assistant_id, assistant_source,
            agent_id, rules_content, default_model_mode, default_permission_mode,
            default_skills_mode, resolved_skill_ids,
            resolved_disabled_builtin_skill_ids, default_mcps_mode, resolved_mcp_ids,
            created_at, updated_at
         ) VALUES (
            'conversation-1', 'definition-1', 'assistant-1', 'user',
            'opencode', '', 'auto', 'auto',
            'fixed', '[\"skill-a\"]',
            '[\"skill-b\"]', 'auto', '[]',
            1, 1
         )",
    )
    .execute(&pool)
    .await
    .unwrap();

    run_migrations_through(&pool, 46).await;

    assert!(
        !table_columns(&pool, "assistants")
            .await
            .iter()
            .any(|column| column == "disabled_builtin_skills")
    );
    assert!(
        !table_columns(&pool, "assistant_definitions")
            .await
            .iter()
            .any(|column| column == "default_disabled_builtin_skill_ids")
    );
    assert!(
        !table_columns(&pool, "assistant_preferences")
            .await
            .iter()
            .any(|column| column == "last_disabled_builtin_skill_ids")
    );
    assert!(
        !table_columns(&pool, "conversation_assistant_snapshots")
            .await
            .iter()
            .any(|column| column == "resolved_disabled_builtin_skill_ids")
    );

    let legacy_enabled: Option<String> =
        sqlx::query_scalar("SELECT enabled_skills FROM assistants WHERE id = 'legacy-assistant'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(legacy_enabled.as_deref(), Some("[\"skill-a\"]"));

    let definition_skills: String =
        sqlx::query_scalar("SELECT default_skill_ids FROM assistant_definitions WHERE id = 'definition-1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(definition_skills, "[\"skill-a\"]");

    let preference_skills: String = sqlx::query_scalar(
        "SELECT last_skill_ids FROM assistant_preferences WHERE assistant_definition_id = 'definition-1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(preference_skills, "[\"skill-a\"]");

    let snapshot_skills: String = sqlx::query_scalar(
        "SELECT resolved_skill_ids
         FROM conversation_assistant_snapshots
         WHERE conversation_id = 'conversation-1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(snapshot_skills, "[\"skill-a\"]");
}
