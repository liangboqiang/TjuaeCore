use std::borrow::Cow;
use std::path::Path;

use sqlx::migrate::Migrator;

async fn run_migrations_through(pool: &sqlx::SqlitePool, max_version: i64) {
    let full = Migrator::new(Path::new("migrations")).await.unwrap();
    let migrations = full
        .migrations
        .iter()
        .filter(|migration| migration.version <= max_version)
        .cloned()
        .collect();

    Migrator {
        migrations: Cow::Owned(migrations),
        ..full
    }
    .run(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn migration_upgrades_existing_internal_agent_to_canonical_vector_logo() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    run_migrations_through(&pool, 28).await;

    sqlx::query("UPDATE agent_metadata SET icon = '/api/assets/logos/brand/noncanonical.png' WHERE id = '632f31d2'")
        .execute(&pool)
        .await
        .unwrap();

    let full = Migrator::new(Path::new("migrations")).await.unwrap();
    full.run(&pool).await.unwrap();

    let icon: String = sqlx::query_scalar("SELECT icon FROM agent_metadata WHERE id = '632f31d2'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(icon, "/api/assets/logos/brand/tjuae.svg");
}
