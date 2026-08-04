use serde::{Deserialize, Serialize};
use tjuaeui_common::TimestampMs;

/// Durable idempotency and recovery state for one user-scoped Hub publish.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct GithubPublishOperationRow {
    pub user_id: String,
    pub idempotency_key: String,
    pub operation_id: String,
    pub request_digest: String,
    pub package_digest: String,
    pub asset_id: String,
    pub package_name: String,
    pub version: String,
    pub state: String,
    pub phase: String,
    pub branch_name: Option<String>,
    pub pull_request_url: Option<String>,
    pub last_error_code: Option<String>,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
}
