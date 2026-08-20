use tjuaeui_db::init_database_memory;

#[tokio::test]
async fn migration_keeps_only_catalog_preferences_and_frozen_snapshots() {
    let db = init_database_memory().await.unwrap();

    let current: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name IN (
            'assistant_user_preferences', 'conversation_assistant_snapshots'
        ) ORDER BY name",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(
        current,
        vec![
            "assistant_user_preferences".to_owned(),
            "conversation_assistant_snapshots".to_owned(),
        ]
    );

    let removed: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name IN (
            'assistant_definitions', 'assistant_overlays', 'assistant_preferences',
            'assistant_overrides', 'assistants'
        ) ORDER BY name",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert!(
        removed.is_empty(),
        "legacy assistant tables must not survive: {removed:?}"
    );
}

#[tokio::test]
async fn assistant_preferences_store_catalog_identity_and_runtime_choices() {
    let db = init_database_memory().await.unwrap();
    let columns: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('assistant_user_preferences')")
        .fetch_all(db.pool())
        .await
        .unwrap();

    for expected in [
        "source",
        "namespace",
        "slug",
        "selected_version",
        "follow_latest",
        "enabled",
        "activation_status",
        "activation_fingerprint",
        "resource_bindings",
        "runtime_overrides",
        "sort_order",
        "last_used_at",
        "updated_at",
    ] {
        assert!(columns.iter().any(|column| column == expected), "missing {expected}");
    }
    assert!(!columns.iter().any(|column| column == "assistant_definition_id"));
}

#[tokio::test]
async fn conversation_snapshot_uses_frozen_catalog_identity() {
    let db = init_database_memory().await.unwrap();
    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('conversation_assistant_snapshots')")
            .fetch_all(db.pool())
            .await
            .unwrap();

    for expected in [
        "conversation_id",
        "assistant_catalog_id",
        "assistant_id",
        "assistant_source",
        "agent_id",
        "rules_content",
        "default_model_mode",
        "resolved_model_id",
        "resolved_skill_ids",
        "resolved_mcp_ids",
    ] {
        assert!(columns.iter().any(|column| column == expected), "missing {expected}");
    }
    assert!(!columns.iter().any(|column| column == "assistant_definition_id"));
    assert!(!columns.iter().any(|column| column == "assistant_name"));
}

#[tokio::test]
async fn assistant_preferences_reject_unknown_catalog_sources() {
    let db = init_database_memory().await.unwrap();
    let error = sqlx::query(
        "INSERT INTO assistant_user_preferences (
            source, namespace, slug, resource_bindings, runtime_overrides, updated_at
        ) VALUES ('extension', '', 'invalid-source', '{}', '{}', 1)",
    )
    .execute(db.pool())
    .await
    .unwrap_err();
    assert!(error.to_string().contains("CHECK constraint failed"));

    for source in ["mine", "tjuae-hub"] {
        sqlx::query(
            "INSERT INTO assistant_user_preferences (
                source, namespace, slug, resource_bindings, runtime_overrides, updated_at
            ) VALUES (?, '', ?, '{}', '{}', 1)",
        )
        .bind(source)
        .bind(format!("{source}-assistant"))
        .execute(db.pool())
        .await
        .unwrap();
    }
}
