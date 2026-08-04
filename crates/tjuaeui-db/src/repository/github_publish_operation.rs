use crate::error::DbError;
use crate::models::GithubPublishOperationRow;

#[derive(Debug)]
pub struct StartGithubPublishOperationParams<'a> {
    pub user_id: &'a str,
    pub idempotency_key: &'a str,
    pub operation_id: &'a str,
    pub request_digest: &'a str,
    pub package_digest: &'a str,
    pub asset_id: &'a str,
    pub package_name: &'a str,
    pub version: &'a str,
    pub branch_name: &'a str,
}

#[derive(Debug)]
pub struct UpdateGithubPublishOperationParams<'a> {
    pub user_id: &'a str,
    pub idempotency_key: &'a str,
    pub state: &'a str,
    pub phase: &'a str,
    pub branch_name: Option<&'a str>,
    pub pull_request_url: Option<&'a str>,
    pub last_error_code: Option<&'a str>,
}

#[async_trait::async_trait]
pub trait IGithubPublishOperationRepository: Send + Sync {
    async fn get(&self, user_id: &str, idempotency_key: &str) -> Result<Option<GithubPublishOperationRow>, DbError>;

    /// Inserts the operation if absent and always returns the durable row.
    /// Callers must compare `request_digest` before performing any remote IO.
    async fn start_or_get(
        &self,
        params: StartGithubPublishOperationParams<'_>,
    ) -> Result<GithubPublishOperationRow, DbError>;

    async fn update(
        &self,
        params: UpdateGithubPublishOperationParams<'_>,
    ) -> Result<GithubPublishOperationRow, DbError>;
}
