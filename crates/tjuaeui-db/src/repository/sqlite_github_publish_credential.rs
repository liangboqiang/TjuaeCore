use sqlx::SqlitePool;

use crate::error::DbError;
use crate::models::GithubPublishCredentialRow;
use crate::repository::github_publish_credential::{
    IGithubPublishCredentialRepository, UpsertGithubPublishCredentialParams,
};

#[derive(Clone, Debug)]
pub struct SqliteGithubPublishCredentialRepository {
    pool: SqlitePool,
}

impl SqliteGithubPublishCredentialRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl IGithubPublishCredentialRepository for SqliteGithubPublishCredentialRepository {
    async fn get(&self, user_id: &str) -> Result<Option<GithubPublishCredentialRow>, DbError> {
        sqlx::query_as::<_, GithubPublishCredentialRow>("SELECT * FROM github_publish_credentials WHERE user_id = ?")
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(DbError::Query)
    }

    async fn upsert(
        &self,
        params: UpsertGithubPublishCredentialParams<'_>,
    ) -> Result<GithubPublishCredentialRow, DbError> {
        let now = tjuaeui_common::now_ms();
        sqlx::query(
            r#"
INSERT INTO github_publish_credentials (
    user_id, state, access_token_ciphertext, refresh_token_ciphertext,
    token_type, access_expires_at, refresh_expires_at, account_login,
    scopes_json, device_code_ciphertext, user_code, verification_uri,
    device_expires_at, poll_interval_seconds, next_poll_at, last_error_code,
    created_at, updated_at
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(user_id) DO UPDATE SET
    state = excluded.state,
    access_token_ciphertext = excluded.access_token_ciphertext,
    refresh_token_ciphertext = excluded.refresh_token_ciphertext,
    token_type = excluded.token_type,
    access_expires_at = excluded.access_expires_at,
    refresh_expires_at = excluded.refresh_expires_at,
    account_login = excluded.account_login,
    scopes_json = excluded.scopes_json,
    device_code_ciphertext = excluded.device_code_ciphertext,
    user_code = excluded.user_code,
    verification_uri = excluded.verification_uri,
    device_expires_at = excluded.device_expires_at,
    poll_interval_seconds = excluded.poll_interval_seconds,
    next_poll_at = excluded.next_poll_at,
    last_error_code = excluded.last_error_code,
    updated_at = excluded.updated_at
            "#,
        )
        .bind(params.user_id)
        .bind(params.state)
        .bind(params.access_token_ciphertext)
        .bind(params.refresh_token_ciphertext)
        .bind(params.token_type)
        .bind(params.access_expires_at)
        .bind(params.refresh_expires_at)
        .bind(params.account_login)
        .bind(params.scopes_json)
        .bind(params.device_code_ciphertext)
        .bind(params.user_code)
        .bind(params.verification_uri)
        .bind(params.device_expires_at)
        .bind(params.poll_interval_seconds)
        .bind(params.next_poll_at)
        .bind(params.last_error_code)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(DbError::Query)?;

        self.get(params.user_id)
            .await?
            .ok_or_else(|| DbError::Init("GitHub publishing credential upsert returned no row".into()))
    }

    async fn delete(&self, user_id: &str) -> Result<(), DbError> {
        sqlx::query("DELETE FROM github_publish_credentials WHERE user_id = ?")
            .bind(user_id)
            .execute(&self.pool)
            .await
            .map_err(DbError::Query)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn credentials_are_strictly_isolated_by_user() {
        let database = crate::init_database_memory().await.unwrap();
        sqlx::query("INSERT INTO users (id, username, password_hash, created_at, updated_at) VALUES (?, ?, '', 1, 1)")
            .bind("second-user")
            .bind("second-user")
            .execute(database.pool())
            .await
            .unwrap();
        let repo = SqliteGithubPublishCredentialRepository::new(database.pool().clone());

        for (user, token) in [("system_default_user", "cipher-one"), ("second-user", "cipher-two")] {
            repo.upsert(UpsertGithubPublishCredentialParams {
                user_id: user,
                state: "connected",
                access_token_ciphertext: Some(token),
                refresh_token_ciphertext: None,
                token_type: Some("bearer"),
                access_expires_at: None,
                refresh_expires_at: None,
                account_login: Some(user),
                scopes_json: "[]",
                device_code_ciphertext: None,
                user_code: None,
                verification_uri: None,
                device_expires_at: None,
                poll_interval_seconds: None,
                next_poll_at: None,
                last_error_code: None,
            })
            .await
            .unwrap();
        }

        assert_eq!(
            repo.get("system_default_user")
                .await
                .unwrap()
                .unwrap()
                .access_token_ciphertext
                .as_deref(),
            Some("cipher-one")
        );
        assert_eq!(
            repo.get("second-user")
                .await
                .unwrap()
                .unwrap()
                .access_token_ciphertext
                .as_deref(),
            Some("cipher-two")
        );
    }
}
