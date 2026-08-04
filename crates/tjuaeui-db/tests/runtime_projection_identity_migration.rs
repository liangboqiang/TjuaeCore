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

#[tokio::test]
async fn migration_053_invalidates_ambiguous_runtime_bindings_without_touching_builtin_rows() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    run_migrations_through(&pool, 52).await;

    sqlx::query(
        "INSERT INTO users (id, username, password_hash, created_at, updated_at)
         VALUES ('system_default_user', 'system', '', 1, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO skills
            (id, name, description, path, source, enabled, created_at, updated_at)
         VALUES
            ('builtin-skill', 'builtin-skill', 'keep', '/builtin/skill', 'builtin', 1, 1, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    for (asset_id, kind, runtime_id) in [
        ("legacy-skill", "skill", "portable-skill"),
        ("legacy-engine", "engineAdapter", "portable-engine"),
    ] {
        sqlx::query(
            "INSERT INTO asset_records (
                user_id, id, kind, display_name, origin, trust, scope, editability,
                workspace_key, definition_digest, entry_file, runtime_id, created_at, updated_at
             ) VALUES (
                'system_default_user', ?, ?, ?, 'local', 'official', 'user', 'full',
                ?, ?, ?, ?, 1, 1
             )",
        )
        .bind(asset_id)
        .bind(kind)
        .bind(asset_id)
        .bind(format!("workspace/{asset_id}"))
        .bind(format!("sha256-{asset_id}"))
        .bind(if kind == "skill" {
            "SKILL.md"
        } else {
            "engine-adapter.json"
        })
        .bind(runtime_id)
        .execute(&pool)
        .await
        .unwrap();
        if kind == "engineAdapter" {
            sqlx::query(
                "INSERT INTO asset_overlays (
                    user_id, asset_owner_id, asset_id, kind, overlay_json, version, updated_at
                 ) VALUES (
                    'system_default_user', 'system_default_user', ?, ?, '{}', 1, 1
                 )",
            )
            .bind(asset_id)
            .bind(kind)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO asset_runtime_states (
                user_id, asset_owner_id, asset_id, state, last_error_code, updated_at
             ) VALUES (
                'system_default_user', 'system_default_user', ?, 'active', NULL, 1
             )",
        )
        .bind(asset_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO asset_try_run_receipts (
                user_id, asset_owner_id, asset_id, receipt_id, idempotency_key,
                definition_digest, overlay_version, runtime_id, created_at
             ) VALUES (
                'system_default_user', 'system_default_user', ?, ?, ?,
                ?, 0, ?, 1
             )",
        )
        .bind(asset_id)
        .bind(format!("receipt-{asset_id}"))
        .bind(format!("try-{asset_id}"))
        .bind(format!("sha256-{asset_id}"))
        .bind(runtime_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO asset_runtime_bindings (
                user_id, asset_owner_id, asset_id, kind, projection_kind, runtime_id,
                definition_digest, overlay_version, health_status, try_run_receipt_id,
                projected_at
             ) VALUES (
                'system_default_user', 'system_default_user', ?, ?, ?, ?,
                ?, 0, 'healthy', ?, 1
             )",
        )
        .bind(asset_id)
        .bind(kind)
        .bind(kind)
        .bind(runtime_id)
        .bind(format!("sha256-{asset_id}"))
        .bind(format!("receipt-{asset_id}"))
        .execute(&pool)
        .await
        .unwrap();
    }

    run_migrations_through(&pool, 53).await;

    let states: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT asset_id, state, last_error_code
         FROM asset_runtime_states
         WHERE user_id = 'system_default_user'
         ORDER BY asset_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        states,
        vec![
            (
                "legacy-engine".into(),
                "inactive".into(),
                Some("RUNTIME_PROJECTION_ID_MIGRATION_REQUIRED".into()),
            ),
            (
                "legacy-skill".into(),
                "inactive".into(),
                Some("RUNTIME_PROJECTION_ID_MIGRATION_REQUIRED".into()),
            ),
        ]
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM asset_runtime_bindings")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM asset_try_run_receipts")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );

    let binding_columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('asset_runtime_bindings')")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(binding_columns.iter().any(|column| column == "portable_runtime_id"));
    assert!(binding_columns.iter().any(|column| column == "projection_runtime_id"));
    assert!(!binding_columns.iter().any(|column| column == "runtime_id"));
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT description FROM skills WHERE id = 'builtin-skill'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        "keep"
    );
}
