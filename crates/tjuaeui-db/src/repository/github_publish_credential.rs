use crate::error::DbError;
use crate::models::GithubPublishCredentialRow;

#[derive(Debug)]
pub struct UpsertGithubPublishCredentialParams<'a> {
    pub user_id: &'a str,
    pub state: &'a str,
    pub access_token_ciphertext: Option<&'a str>,
    pub refresh_token_ciphertext: Option<&'a str>,
    pub token_type: Option<&'a str>,
    pub access_expires_at: Option<i64>,
    pub refresh_expires_at: Option<i64>,
    pub account_login: Option<&'a str>,
    pub scopes_json: &'a str,
    pub device_code_ciphertext: Option<&'a str>,
    pub user_code: Option<&'a str>,
    pub verification_uri: Option<&'a str>,
    pub device_expires_at: Option<i64>,
    pub poll_interval_seconds: Option<i64>,
    pub next_poll_at: Option<i64>,
    pub last_error_code: Option<&'a str>,
}

/// Persistence boundary for user-scoped GitHub publishing credentials.
///
/// Encryption and decryption deliberately live above this repository so the
/// database layer never receives plaintext tokens.
#[async_trait::async_trait]
pub trait IGithubPublishCredentialRepository: Send + Sync {
    async fn get(&self, user_id: &str) -> Result<Option<GithubPublishCredentialRow>, DbError>;

    async fn upsert(
        &self,
        params: UpsertGithubPublishCredentialParams<'_>,
    ) -> Result<GithubPublishCredentialRow, DbError>;

    async fn delete(&self, user_id: &str) -> Result<(), DbError>;
}
