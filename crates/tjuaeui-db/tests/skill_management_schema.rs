use tjuaeui_db::init_database_memory;

#[tokio::test]
async fn migration_removes_the_legacy_skill_catalog() {
    let db = init_database_memory().await.unwrap();
    let pool = db.pool();

    let skill_columns: Vec<(String,)> = sqlx::query_as("SELECT name FROM pragma_table_info('skills') ORDER BY cid")
        .fetch_all(pool)
        .await
        .unwrap();
    let skill_columns: Vec<String> = skill_columns.into_iter().map(|row| row.0).collect();
    assert!(skill_columns.is_empty());

    let import_columns: Vec<(String,)> =
        sqlx::query_as("SELECT name FROM pragma_table_info('skill_import_records') ORDER BY cid")
            .fetch_all(pool)
            .await
            .unwrap();
    let import_columns: Vec<String> = import_columns.into_iter().map(|row| row.0).collect();
    assert!(import_columns.is_empty());
}
