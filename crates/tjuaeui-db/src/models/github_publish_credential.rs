use serde::{Deserialize, Serialize};
use tjuaeui_common::TimestampMs;

/// Encrypted, user-scoped state for the GitHub App publishing connection.
///
/// Fields ending in `_ciphertext` must never be returned from an HTTP DTO or
/// written to logs. Core is the only component allowed to decrypt them.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct GithubPublishCredentialRow {
    pub user_id: String,
    pub state: String,
    pub access_token_ciphertext: Option<String>,
    pub refresh_token_ciphertext: Option<String>,
    pub token_type: Option<String>,
    pub access_expires_at: Option<TimestampMs>,
    pub refresh_expires_at: Option<TimestampMs>,
    pub account_login: Option<String>,
    pub scopes_json: String,
    pub device_code_ciphertext: Option<String>,
    pub user_code: Option<String>,
    pub verification_uri: Option<String>,
    pub device_expires_at: Option<TimestampMs>,
    pub poll_interval_seconds: Option<i64>,
    pub next_poll_at: Option<TimestampMs>,
    pub last_error_code: Option<String>,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
}
