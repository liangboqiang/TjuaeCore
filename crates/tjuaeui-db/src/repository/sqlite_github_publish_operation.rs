use sqlx::SqlitePool;

use crate::error::DbError;
use crate::models::GithubPublishOperationRow;
use crate::repository::github_publish_operation::{
    IGithubPublishOperationRepository, StartGithubPublishOperationParams, UpdateGithubPublishOperationParams,
};

#[derive(Clone, Debug)]
pub struct SqliteGithubPublishOperationRepository {
    pool: SqlitePool,
}

impl SqliteGithubPublishOperationRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl IGithubPublishOperationRepository for SqliteGithubPublishOperationRepository {
    async fn get(&self, user_id: &str, idempotency_key: &str) -> Result<Option<GithubPublishOperationRow>, DbError> {
        sqlx::query_as::<_, GithubPublishOperationRow>(
            "SELECT * FROM github_publish_operations WHERE user_id = ? AND idempotency_key = ?",
        )
        .bind(user_id)
        .bind(idempotency_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(DbError::Query)
    }

    async fn start_or_get(
        &self,
        params: StartGithubPublishOperationParams<'_>,
    ) -> Result<GithubPublishOperationRow, DbError> {
        let now = tjuaeui_common::now_ms();
        sqlx::query(
            r#"
INSERT INTO github_publish_operations (
    user_id, idempotency_key, operation_id, request_digest, package_digest,
    asset_id, package_name, version, state, phase, branch_name,
    pull_request_url, last_error_code, created_at, updated_at
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'running', 'validated', ?, NULL, NULL, ?, ?)
ON CONFLICT(user_id, idempotency_key) DO NOTHING
            "#,
        )
        .bind(params.user_id)
        .bind(params.idempotency_key)
        .bind(params.operation_id)
        .bind(params.request_digest)
        .bind(params.package_digest)
        .bind(params.asset_id)
        .bind(params.package_name)
        .bind(params.version)
        .bind(params.branch_name)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(DbError::Query)?;

        self.get(params.user_id, params.idempotency_key)
            .await?
            .ok_or_else(|| DbError::Init("GitHub publish operation insert returned no row".into()))
    }

    async fn update(
        &self,
        params: UpdateGithubPublishOperationParams<'_>,
    ) -> Result<GithubPublishOperationRow, DbError> {
        let now = tjuaeui_common::now_ms();
        sqlx::query(
            r#"
UPDATE github_publish_operations
SET state = ?, phase = ?, branch_name = ?, pull_request_url = ?,
    last_error_code = ?, updated_at = ?
WHERE user_id = ? AND idempotency_key = ?
            "#,
        )
        .bind(params.state)
        .bind(params.phase)
        .bind(params.branch_name)
        .bind(params.pull_request_url)
        .bind(params.last_error_code)
        .bind(now)
        .bind(params.user_id)
        .bind(params.idempotency_key)
        .execute(&self.pool)
        .await
        .map_err(DbError::Query)?;

        self.get(params.user_id, params.idempotency_key)
            .await?
            .ok_or_else(|| DbError::Init("GitHub publish operation update returned no row".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn idempotency_keys_are_scoped_per_user_and_do_not_overwrite_requests() {
        let database = crate::init_database_memory().await.unwrap();
        sqlx::query("INSERT INTO users (id, username, password_hash, created_at, updated_at) VALUES (?, ?, '', 1, 1)")
            .bind("second-user")
            .bind("second-user")
            .execute(database.pool())
            .await
            .unwrap();
        let repo = SqliteGithubPublishOperationRepository::new(database.pool().clone());

        for (user, digest, operation_id) in [
            ("system_default_user", "request-one", "operation-one"),
            ("second-user", "request-two", "operation-two"),
        ] {
            repo.start_or_get(StartGithubPublishOperationParams {
                user_id: user,
                idempotency_key: "same-key",
                operation_id,
                request_digest: digest,
                package_digest: digest,
                asset_id: "skill:demo",
                package_name: "tjuaeasset-demo",
                version: "1.0.0",
                branch_name: "branch",
            })
            .await
            .unwrap();
        }

        let original = repo
            .start_or_get(StartGithubPublishOperationParams {
                user_id: "system_default_user",
                idempotency_key: "same-key",
                operation_id: "replacement",
                request_digest: "must-not-replace",
                package_digest: "must-not-replace",
                asset_id: "skill:other",
                package_name: "tjuaeasset-other",
                version: "2.0.0",
                branch_name: "other",
            })
            .await
            .unwrap();
        assert_eq!(original.request_digest, "request-one");
        assert_eq!(
            repo.get("second-user", "same-key")
                .await
                .unwrap()
                .unwrap()
                .request_digest,
            "request-two"
        );
    }

    #[tokio::test]
    async fn publish_ledger_has_one_recoverable_state_machine() {
        let database = crate::init_database_memory().await.unwrap();
        let repo = SqliteGithubPublishOperationRepository::new(database.pool().clone());
        let running = repo
            .start_or_get(StartGithubPublishOperationParams {
                user_id: "system_default_user",
                idempotency_key: "publish-state-machine",
                operation_id: "publish-operation",
                request_digest: "request-digest",
                package_digest: "package-digest",
                asset_id: "skill:demo",
                package_name: "tjuaeasset-demo",
                version: "1.0.0",
                branch_name: "publish-branch",
            })
            .await
            .unwrap();
        assert_eq!(running.state, "running");
        assert_eq!(running.phase, "validated");

        let failed = repo
            .update(UpdateGithubPublishOperationParams {
                user_id: "system_default_user",
                idempotency_key: "publish-state-machine",
                state: "failed",
                phase: "recoverable",
                branch_name: Some("publish-branch"),
                pull_request_url: None,
                last_error_code: Some("GITHUB_NETWORK_ERROR"),
            })
            .await
            .unwrap();
        assert_eq!(failed.state, "failed");
        assert_eq!(failed.phase, "recoverable");
        assert_eq!(failed.last_error_code.as_deref(), Some("GITHUB_NETWORK_ERROR"));

        let succeeded = repo
            .update(UpdateGithubPublishOperationParams {
                user_id: "system_default_user",
                idempotency_key: "publish-state-machine",
                state: "succeeded",
                phase: "pullRequestCreated",
                branch_name: Some("publish-branch"),
                pull_request_url: Some("https://github.com/liangboqiang/TjuaeHub/pull/1"),
                last_error_code: None,
            })
            .await
            .unwrap();
        assert_eq!(succeeded.state, "succeeded");
        assert_eq!(succeeded.phase, "pullRequestCreated");

        let unsupported_rollback = repo
            .update(UpdateGithubPublishOperationParams {
                user_id: "system_default_user",
                idempotency_key: "publish-state-machine",
                state: "rolledBack",
                phase: "rolledBack",
                branch_name: Some("publish-branch"),
                pull_request_url: None,
                last_error_code: None,
            })
            .await;
        assert!(
            unsupported_rollback.is_err(),
            "remote publishing is recoverable and idempotent; it must not claim an unperformed rollback"
        );
    }
}
