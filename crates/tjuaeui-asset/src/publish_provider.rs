use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use tjuaeui_api_types::{
    CanonicalAssetPackage, HubAssetPublishPreparation, HubAssetPublishRequest, HubAssetPublishResponse,
    HubPublishConnectionState, HubPublishConnectionStatus,
};
use tjuaeui_common::{decrypt_string, encrypt_string, now_ms};
use tjuaeui_db::{
    GithubPublishCredentialRow, GithubPublishOperationRow, IGithubPublishCredentialRepository,
    IGithubPublishOperationRepository, StartGithubPublishOperationParams, UpdateGithubPublishOperationParams,
    UpsertGithubPublishCredentialParams,
};

use crate::publish::{
    ASSET_PACKAGE_SCHEMA_URL, safe_relative_path, validate_public_asset_file, validate_publication_metadata,
};
use crate::publish_error::AssetPublishError;

const HUB_OWNER: &str = "liangboqiang";
const HUB_REPOSITORY: &str = "TjuaeHub";
const HUB_REPOSITORY_URL: &str = "https://github.com/liangboqiang/TjuaeHub";
const HUB_MANUAL_CONTRIBUTION_URL: &str = "https://github.com/liangboqiang/TjuaeHub/fork";
const HUB_BASE_BRANCH: &str = "main";
const GITHUB_API_VERSION: &str = "2022-11-28";
const DEFAULT_API_BASE: &str = "https://api.github.com";
const DEFAULT_OAUTH_BASE: &str = "https://github.com";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(45);
const FORK_READY_ATTEMPTS: usize = 8;
const COMPILED_GITHUB_APP_CLIENT_ID: Option<&str> = option_env!("TJUAE_GITHUB_APP_CLIENT_ID");

/// Publishing boundary used by the asset service.
///
/// Production wires [`GitHubRestPublishProvider`]. The trait keeps HTTP and
/// credential persistence mockable without ever falling back to a process
/// based implementation.
#[async_trait]
pub trait HubPublishProvider: Send + Sync {
    async fn connection_status(&self, user_id: &str) -> Result<HubPublishConnectionStatus, AssetPublishError>;
    async fn start_authorization(&self, user_id: &str) -> Result<HubPublishConnectionStatus, AssetPublishError>;
    async fn poll_authorization(&self, user_id: &str) -> Result<HubPublishConnectionStatus, AssetPublishError>;
    async fn disconnect(&self, user_id: &str) -> Result<HubPublishConnectionStatus, AssetPublishError>;
    async fn publish(
        &self,
        user_id: &str,
        request: &HubAssetPublishRequest,
        package: CanonicalAssetPackage,
    ) -> Result<HubAssetPublishResponse, AssetPublishError>;
}

#[derive(Clone)]
pub struct GitHubRestPublishProvider {
    client: reqwest::Client,
    credentials: Arc<dyn IGithubPublishCredentialRepository>,
    operations: Arc<dyn IGithubPublishOperationRepository>,
    encryption_key: [u8; 32],
    client_id: Option<String>,
    oauth_base: String,
    api_base: String,
}

impl GitHubRestPublishProvider {
    pub fn new(
        credentials: Arc<dyn IGithubPublishCredentialRepository>,
        operations: Arc<dyn IGithubPublishOperationRepository>,
        encryption_key: [u8; 32],
    ) -> Result<Self, AssetPublishError> {
        // 正式构建将公开的 GitHub App client ID 编译进 Core。运行时环境变量
        // 只作为本地开发覆盖，避免要求最终用户手工设置环境变量。
        let client_id = std::env::var("TJUAE_GITHUB_APP_CLIENT_ID")
            .ok()
            .as_deref()
            .and_then(normalize_client_id)
            .or_else(|| COMPILED_GITHUB_APP_CLIENT_ID.and_then(normalize_client_id));
        let client = tjuaeui_runtime::build_http_client(Duration::from_secs(10), REQUEST_TIMEOUT)
            .map_err(AssetPublishError::Internal)?;
        Ok(Self {
            client,
            credentials,
            operations,
            encryption_key,
            client_id,
            oauth_base: DEFAULT_OAUTH_BASE.into(),
            api_base: DEFAULT_API_BASE.into(),
        })
    }

    #[cfg(test)]
    fn for_test(
        client: reqwest::Client,
        credentials: Arc<dyn IGithubPublishCredentialRepository>,
        operations: Arc<dyn IGithubPublishOperationRepository>,
        encryption_key: [u8; 32],
        client_id: Option<&str>,
        base: &str,
    ) -> Self {
        Self {
            client,
            credentials,
            operations,
            encryption_key,
            client_id: client_id.map(str::to_owned),
            oauth_base: base.trim_end_matches('/').into(),
            api_base: base.trim_end_matches('/').into(),
        }
    }

    fn configured_client_id(&self) -> Result<&str, AssetPublishError> {
        self.client_id
            .as_deref()
            .ok_or_else(|| AssetPublishError::HubPublishPrerequisite("GITHUB_APP_NOT_CONFIGURED".into()))
    }

    fn status_from_row(&self, row: Option<&GithubPublishCredentialRow>) -> HubPublishConnectionStatus {
        if self.client_id.is_none() {
            return connection_status(HubPublishConnectionState::NotConfigured, None, None);
        }
        let Some(row) = row else {
            return connection_status(HubPublishConnectionState::Disconnected, None, None);
        };
        match row.state.as_str() {
            "authorizationPending" if row.device_expires_at.is_some_and(|expires| expires > now_ms()) => {
                HubPublishConnectionStatus {
                    state: HubPublishConnectionState::AuthorizationPending,
                    account: None,
                    user_code: row.user_code.clone(),
                    verification_uri: row.verification_uri.clone(),
                    expires_at: row.device_expires_at,
                    poll_after_ms: row
                        .next_poll_at
                        .map(|next| next.saturating_sub(now_ms()) as u64)
                        .or_else(|| row.poll_interval_seconds.map(|seconds| seconds.max(1) as u64 * 1_000)),
                    reason_code: row.last_error_code.clone(),
                }
            }
            "connected" => connection_status(
                HubPublishConnectionState::Connected,
                row.account_login.clone(),
                row.last_error_code.clone(),
            ),
            "insufficientPermissions" => connection_status(
                HubPublishConnectionState::InsufficientPermissions,
                row.account_login.clone(),
                row.last_error_code.clone(),
            ),
            "authorizationPending" => connection_status(
                HubPublishConnectionState::Disconnected,
                None,
                Some("GITHUB_DEVICE_CODE_EXPIRED".into()),
            ),
            _ => connection_status(
                HubPublishConnectionState::Disconnected,
                None,
                row.last_error_code.clone(),
            ),
        }
    }

    async fn persist_pending(
        &self,
        user_id: &str,
        response: &DeviceCodeResponse,
    ) -> Result<HubPublishConnectionStatus, AssetPublishError> {
        let now = now_ms();
        let interval = response.interval.max(1);
        let encrypted_device_code =
            encrypt_string(&response.device_code, &self.encryption_key).map_err(crypto_error)?;
        let row = self
            .credentials
            .upsert(UpsertGithubPublishCredentialParams {
                user_id,
                state: "authorizationPending",
                access_token_ciphertext: None,
                refresh_token_ciphertext: None,
                token_type: None,
                access_expires_at: None,
                refresh_expires_at: None,
                account_login: None,
                scopes_json: "[]",
                device_code_ciphertext: Some(&encrypted_device_code),
                user_code: Some(&response.user_code),
                verification_uri: Some(&response.verification_uri),
                device_expires_at: Some(now.saturating_add(response.expires_in.saturating_mul(1_000))),
                poll_interval_seconds: Some(interval),
                next_poll_at: Some(now.saturating_add(interval.saturating_mul(1_000))),
                last_error_code: None,
            })
            .await?;
        Ok(self.status_from_row(Some(&row)))
    }

    async fn persist_connected(
        &self,
        user_id: &str,
        token: &TokenResponse,
        account: &str,
    ) -> Result<HubPublishConnectionStatus, AssetPublishError> {
        validate_github_login(account)?;
        let now = now_ms();
        let encrypted_access = encrypt_string(&token.access_token, &self.encryption_key).map_err(crypto_error)?;
        let encrypted_refresh = token
            .refresh_token
            .as_deref()
            .map(|value| encrypt_string(value, &self.encryption_key))
            .transpose()
            .map_err(crypto_error)?;
        let scopes = parse_scopes(token.scope.as_deref());
        let scopes_json = serde_json::to_string(&scopes)?;
        let row = self
            .credentials
            .upsert(UpsertGithubPublishCredentialParams {
                user_id,
                state: "connected",
                access_token_ciphertext: Some(&encrypted_access),
                refresh_token_ciphertext: encrypted_refresh.as_deref(),
                token_type: Some(token.token_type.as_deref().unwrap_or("bearer")),
                access_expires_at: token
                    .expires_in
                    .map(|seconds| now.saturating_add(seconds.saturating_mul(1_000))),
                refresh_expires_at: token
                    .refresh_token_expires_in
                    .map(|seconds| now.saturating_add(seconds.saturating_mul(1_000))),
                account_login: Some(account),
                scopes_json: &scopes_json,
                device_code_ciphertext: None,
                user_code: None,
                verification_uri: None,
                device_expires_at: None,
                poll_interval_seconds: None,
                next_poll_at: None,
                last_error_code: None,
            })
            .await?;
        Ok(self.status_from_row(Some(&row)))
    }

    async fn update_pending_after_poll(
        &self,
        row: &GithubPublishCredentialRow,
        interval_seconds: i64,
        reason_code: Option<&str>,
    ) -> Result<HubPublishConnectionStatus, AssetPublishError> {
        let updated = self
            .credentials
            .upsert(UpsertGithubPublishCredentialParams {
                user_id: &row.user_id,
                state: "authorizationPending",
                access_token_ciphertext: None,
                refresh_token_ciphertext: None,
                token_type: None,
                access_expires_at: None,
                refresh_expires_at: None,
                account_login: None,
                scopes_json: "[]",
                device_code_ciphertext: row.device_code_ciphertext.as_deref(),
                user_code: row.user_code.as_deref(),
                verification_uri: row.verification_uri.as_deref(),
                device_expires_at: row.device_expires_at,
                poll_interval_seconds: Some(interval_seconds),
                next_poll_at: Some(now_ms().saturating_add(interval_seconds.saturating_mul(1_000))),
                last_error_code: reason_code,
            })
            .await?;
        Ok(self.status_from_row(Some(&updated)))
    }

    async fn mark_permission_failure(
        &self,
        row: &GithubPublishCredentialRow,
        reason_code: &str,
    ) -> Result<(), AssetPublishError> {
        self.credentials
            .upsert(UpsertGithubPublishCredentialParams {
                user_id: &row.user_id,
                state: "insufficientPermissions",
                access_token_ciphertext: row.access_token_ciphertext.as_deref(),
                refresh_token_ciphertext: row.refresh_token_ciphertext.as_deref(),
                token_type: row.token_type.as_deref(),
                access_expires_at: row.access_expires_at,
                refresh_expires_at: row.refresh_expires_at,
                account_login: row.account_login.as_deref(),
                scopes_json: &row.scopes_json,
                device_code_ciphertext: None,
                user_code: None,
                verification_uri: None,
                device_expires_at: None,
                poll_interval_seconds: None,
                next_poll_at: None,
                last_error_code: Some(reason_code),
            })
            .await?;
        Ok(())
    }

    async fn update_publish_operation(
        &self,
        operation: &GithubPublishOperationRow,
        state: &str,
        phase: &str,
        pull_request_url: Option<&str>,
        last_error_code: Option<&str>,
    ) -> Result<GithubPublishOperationRow, AssetPublishError> {
        Ok(self
            .operations
            .update(UpdateGithubPublishOperationParams {
                user_id: &operation.user_id,
                idempotency_key: &operation.idempotency_key,
                state,
                phase,
                branch_name: operation.branch_name.as_deref(),
                pull_request_url,
                last_error_code,
            })
            .await?)
    }

    async fn authenticated_identity(&self, token: &str) -> Result<GithubUser, AssetPublishError> {
        let response = self
            .api_request(Method::GET, "/user", token)
            .send()
            .await
            .map_err(network_error)?;
        if response.status() == StatusCode::UNAUTHORIZED {
            return Err(AssetPublishError::HubPublishPrerequisite("GITHUB_AUTH_REVOKED".into()));
        }
        decode_api_response(response, "GITHUB_IDENTITY_FAILED").await
    }

    async fn access_token(&self, user_id: &str) -> Result<(String, GithubPublishCredentialRow), AssetPublishError> {
        let mut row = self
            .credentials
            .get(user_id)
            .await?
            .ok_or_else(|| AssetPublishError::HubPublishPrerequisite("GITHUB_NOT_CONNECTED".into()))?;
        if !matches!(row.state.as_str(), "connected" | "insufficientPermissions") {
            return Err(AssetPublishError::HubPublishPrerequisite("GITHUB_NOT_CONNECTED".into()));
        }

        if row
            .access_expires_at
            .is_some_and(|expires| expires <= now_ms().saturating_add(60_000))
        {
            row = self.refresh_access_token(&row).await?;
        }
        let ciphertext = row
            .access_token_ciphertext
            .as_deref()
            .ok_or_else(|| AssetPublishError::HubPublishPrerequisite("GITHUB_NOT_CONNECTED".into()))?;
        let token = decrypt_string(ciphertext, &self.encryption_key).map_err(crypto_error)?;
        Ok((token, row))
    }

    async fn refresh_access_token(
        &self,
        row: &GithubPublishCredentialRow,
    ) -> Result<GithubPublishCredentialRow, AssetPublishError> {
        let client_id = self.configured_client_id()?;
        let refresh_ciphertext = row
            .refresh_token_ciphertext
            .as_deref()
            .ok_or_else(|| AssetPublishError::HubPublishPrerequisite("GITHUB_AUTH_EXPIRED".into()))?;
        if row.refresh_expires_at.is_some_and(|expires| expires <= now_ms()) {
            self.credentials.delete(&row.user_id).await?;
            return Err(AssetPublishError::HubPublishPrerequisite("GITHUB_AUTH_EXPIRED".into()));
        }
        let refresh = decrypt_string(refresh_ciphertext, &self.encryption_key).map_err(crypto_error)?;
        let response = self
            .client
            .post(format!("{}/login/oauth/access_token", self.oauth_base))
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&[
                ("client_id", client_id),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh.as_str()),
            ])
            .send()
            .await
            .map_err(network_error)?;
        let token: TokenResponse = decode_oauth_response(response, "GITHUB_TOKEN_REFRESH_FAILED").await?;
        let account = row
            .account_login
            .as_deref()
            .ok_or_else(|| AssetPublishError::HubPublishPrerequisite("GITHUB_NOT_CONNECTED".into()))?;
        self.persist_connected(&row.user_id, &token, account).await?;
        self.credentials
            .get(&row.user_id)
            .await?
            .ok_or_else(|| AssetPublishError::Internal("刷新发布凭据后找不到记录".into()))
    }

    fn api_request(&self, method: Method, path: &str, token: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{}", self.api_base, path))
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header(
                reqwest::header::USER_AGENT,
                concat!("TjuaeCore/", env!("CARGO_PKG_VERSION")),
            )
            .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
            .bearer_auth(token)
    }

    async fn get_optional<T: DeserializeOwned>(&self, path: &str, token: &str) -> Result<Option<T>, AssetPublishError> {
        let response = self
            .api_request(Method::GET, path, token)
            .send()
            .await
            .map_err(network_error)?;
        match response.status() {
            StatusCode::NOT_FOUND => Ok(None),
            StatusCode::UNAUTHORIZED => Err(AssetPublishError::HubPublishPrerequisite("GITHUB_AUTH_REVOKED".into())),
            _ => decode_api_response(response, "GITHUB_API_READ_FAILED").await.map(Some),
        }
    }

    async fn send_json<T: DeserializeOwned, B: Serialize + ?Sized>(
        &self,
        method: Method,
        path: &str,
        token: &str,
        body: &B,
        failure_code: &'static str,
    ) -> Result<T, AssetPublishError> {
        let response = self
            .api_request(method, path, token)
            .json(body)
            .send()
            .await
            .map_err(network_error)?;
        if response.status() == StatusCode::UNAUTHORIZED {
            return Err(AssetPublishError::HubPublishPrerequisite("GITHUB_AUTH_REVOKED".into()));
        }
        decode_api_response(response, failure_code).await
    }

    async fn ensure_fork(&self, login: &str, token: &str) -> Result<(), AssetPublishError> {
        let fork_path = format!("/repos/{login}/{HUB_REPOSITORY}");
        if self
            .get_optional::<GithubRepository>(&fork_path, token)
            .await?
            .is_some()
        {
            self.sync_fork(login, token).await?;
            return Ok(());
        }

        let _: GithubRepository = self
            .send_json(
                Method::POST,
                &format!("/repos/{HUB_OWNER}/{HUB_REPOSITORY}/forks"),
                token,
                &json!({"default_branch_only": true}),
                "GITHUB_FORK_CREATE_FAILED",
            )
            .await?;
        for attempt in 0..FORK_READY_ATTEMPTS {
            if self
                .get_optional::<GithubRepository>(&fork_path, token)
                .await?
                .is_some()
            {
                self.sync_fork(login, token).await?;
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250 * (attempt as u64 + 1))).await;
        }
        Err(AssetPublishError::HubPublishFailed("GITHUB_FORK_NOT_READY".into()))
    }

    async fn sync_fork(&self, login: &str, token: &str) -> Result<(), AssetPublishError> {
        let path = format!("/repos/{login}/{HUB_REPOSITORY}/merge-upstream");
        let response = self
            .api_request(Method::POST, &path, token)
            .json(&json!({"branch": HUB_BASE_BRANCH}))
            .send()
            .await
            .map_err(network_error)?;
        match response.status() {
            StatusCode::OK | StatusCode::CREATED | StatusCode::CONFLICT | StatusCode::UNPROCESSABLE_ENTITY => Ok(()),
            StatusCode::UNAUTHORIZED => Err(AssetPublishError::HubPublishPrerequisite("GITHUB_AUTH_REVOKED".into())),
            StatusCode::FORBIDDEN => Err(AssetPublishError::HubPublishPrerequisite(
                "GITHUB_INSUFFICIENT_PERMISSIONS".into(),
            )),
            _ => Err(AssetPublishError::HubPublishFailed("GITHUB_FORK_SYNC_FAILED".into())),
        }
    }

    async fn find_open_pull_request(
        &self,
        login: &str,
        branch: &str,
        token: &str,
    ) -> Result<Option<GithubPullRequest>, AssetPublishError> {
        let head = format!("{login}:{branch}");
        let path = format!(
            "/repos/{HUB_OWNER}/{HUB_REPOSITORY}/pulls?state=open&base={HUB_BASE_BRANCH}&head={}",
            url_encode_query(&head)
        );
        Ok(self
            .get_optional::<Vec<GithubPullRequest>>(&path, token)
            .await?
            .and_then(|mut values| values.drain(..).next()))
    }

    async fn branch_matches_package(
        &self,
        login: &str,
        reference: &GithubReference,
        package: &CanonicalAssetPackage,
        token: &str,
    ) -> Result<bool, AssetPublishError> {
        let Some(commit) = self
            .get_optional::<GithubCommit>(
                &format!("/repos/{login}/{HUB_REPOSITORY}/git/commits/{}", reference.object.sha),
                token,
            )
            .await?
        else {
            return Ok(false);
        };
        let Some(tree) = self
            .get_optional::<GithubTree>(
                &format!(
                    "/repos/{login}/{HUB_REPOSITORY}/git/trees/{}?recursive=1",
                    commit.tree.sha
                ),
                token,
            )
            .await?
        else {
            return Ok(false);
        };
        if tree.truncated {
            return Ok(false);
        }
        let expected = package_repository_files(package)?
            .into_iter()
            .map(|(path, content)| (path, git_blob_sha(&content)))
            .collect::<BTreeMap<_, _>>();
        let prefix = format!("submissions/{}/", package.package_name);
        let actual = tree
            .tree
            .into_iter()
            .filter(|entry| entry.path.starts_with(&prefix))
            .map(|entry| (entry.path.clone(), entry))
            .collect::<BTreeMap<_, _>>();
        if actual.len() != expected.len() {
            return Ok(false);
        }
        Ok(expected.into_iter().all(|(path, sha)| {
            actual
                .get(&path)
                .is_some_and(|entry| entry.kind == "blob" && entry.mode == "100644" && entry.sha == sha)
        }))
    }

    async fn create_pull_request(
        &self,
        request: &HubAssetPublishRequest,
        operation_id: &str,
        login: &str,
        branch: &str,
        token: &str,
    ) -> Result<HubAssetPublishResponse, AssetPublishError> {
        let pull_response = self
            .api_request(
                Method::POST,
                &format!("/repos/{HUB_OWNER}/{HUB_REPOSITORY}/pulls"),
                token,
            )
            .json(&json!({
                "title": publish_title(request),
                "body": publish_body(request),
                "head": format!("{login}:{branch}"),
                "base": HUB_BASE_BRANCH,
                "maintainer_can_modify": true
            }))
            .send()
            .await
            .map_err(network_error)?;
        if pull_response.status() == StatusCode::UNPROCESSABLE_ENTITY
            && let Some(pr) = self.find_open_pull_request(login, branch, token).await?
        {
            return Ok(publish_response(operation_id, branch.into(), pr.html_url));
        }
        let pull: GithubPullRequest = decode_api_response(pull_response, "GITHUB_PULL_REQUEST_CREATE_FAILED").await?;
        Ok(publish_response(operation_id, branch.into(), pull.html_url))
    }

    async fn publish_package(
        &self,
        request: &HubAssetPublishRequest,
        package: &CanonicalAssetPackage,
        operation_id: &str,
        package_digest: &str,
        token: &str,
        login: &str,
    ) -> Result<HubAssetPublishResponse, AssetPublishError> {
        self.ensure_fork(login, token).await?;
        let branch = branch_name(&request.package_name, package_digest);
        if let Some(existing_ref) = self
            .get_optional::<GithubReference>(
                &format!("/repos/{login}/{HUB_REPOSITORY}/git/ref/heads/{branch}"),
                token,
            )
            .await?
        {
            if !self
                .branch_matches_package(login, &existing_ref, package, token)
                .await?
            {
                return Err(AssetPublishError::HubPublishConflict(
                    "GITHUB_PUBLISH_BRANCH_CONFLICT".into(),
                ));
            }
            if let Some(pr) = self.find_open_pull_request(login, &branch, token).await? {
                return Ok(publish_response(operation_id, branch, pr.html_url));
            }
            return self
                .create_pull_request(request, operation_id, login, &branch, token)
                .await;
        }

        let upstream_ref: GithubReference = self
            .get_optional(
                &format!("/repos/{HUB_OWNER}/{HUB_REPOSITORY}/git/ref/heads/{HUB_BASE_BRANCH}"),
                token,
            )
            .await?
            .ok_or_else(|| AssetPublishError::HubPublishFailed("GITHUB_UPSTREAM_REF_MISSING".into()))?;
        let upstream_commit: GithubCommit = self
            .get_optional(
                &format!(
                    "/repos/{HUB_OWNER}/{HUB_REPOSITORY}/git/commits/{}",
                    upstream_ref.object.sha
                ),
                token,
            )
            .await?
            .ok_or_else(|| AssetPublishError::HubPublishFailed("GITHUB_UPSTREAM_COMMIT_MISSING".into()))?;

        let files = package_repository_files(package)?;
        let mut tree_entries = Vec::with_capacity(files.len());
        for (path, content) in files {
            let blob: GithubObject = self
                .send_json(
                    Method::POST,
                    &format!("/repos/{login}/{HUB_REPOSITORY}/git/blobs"),
                    token,
                    &json!({
                        "content": base64::engine::general_purpose::STANDARD.encode(content.as_bytes()),
                        "encoding": "base64"
                    }),
                    "GITHUB_BLOB_CREATE_FAILED",
                )
                .await?;
            tree_entries.push(json!({
                "path": path,
                "mode": "100644",
                "type": "blob",
                "sha": blob.sha
            }));
        }

        let tree: GithubObject = self
            .send_json(
                Method::POST,
                &format!("/repos/{login}/{HUB_REPOSITORY}/git/trees"),
                token,
                &json!({
                    "base_tree": upstream_commit.tree.sha,
                    "tree": tree_entries
                }),
                "GITHUB_TREE_CREATE_FAILED",
            )
            .await?;
        let commit: GithubObject = self
            .send_json(
                Method::POST,
                &format!("/repos/{login}/{HUB_REPOSITORY}/git/commits"),
                token,
                &json!({
                    "message": format!("feat(assets): 提交 {}", package.package_name),
                    "tree": tree.sha,
                    "parents": [upstream_ref.object.sha]
                }),
                "GITHUB_COMMIT_CREATE_FAILED",
            )
            .await?;

        let reference_response = self
            .api_request(
                Method::POST,
                &format!("/repos/{login}/{HUB_REPOSITORY}/git/refs"),
                token,
            )
            .json(&json!({
                "ref": format!("refs/heads/{branch}"),
                "sha": commit.sha
            }))
            .send()
            .await
            .map_err(network_error)?;
        if reference_response.status() == StatusCode::UNPROCESSABLE_ENTITY {
            let existing_ref = self
                .get_optional::<GithubReference>(
                    &format!("/repos/{login}/{HUB_REPOSITORY}/git/ref/heads/{branch}"),
                    token,
                )
                .await?
                .ok_or_else(|| AssetPublishError::HubPublishFailed("GITHUB_REF_CREATE_FAILED".into()))?;
            if !self
                .branch_matches_package(login, &existing_ref, package, token)
                .await?
            {
                return Err(AssetPublishError::HubPublishConflict(
                    "GITHUB_PUBLISH_BRANCH_CONFLICT".into(),
                ));
            }
            if let Some(pr) = self.find_open_pull_request(login, &branch, token).await? {
                return Ok(publish_response(operation_id, branch, pr.html_url));
            }
            return self
                .create_pull_request(request, operation_id, login, &branch, token)
                .await;
        }
        let _: GithubReference = decode_api_response(reference_response, "GITHUB_REF_CREATE_FAILED").await?;
        self.create_pull_request(request, operation_id, login, &branch, token)
            .await
    }
}

fn normalize_client_id(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

#[async_trait]
impl HubPublishProvider for GitHubRestPublishProvider {
    async fn connection_status(&self, user_id: &str) -> Result<HubPublishConnectionStatus, AssetPublishError> {
        let row = self.credentials.get(user_id).await?;
        Ok(self.status_from_row(row.as_ref()))
    }

    async fn start_authorization(&self, user_id: &str) -> Result<HubPublishConnectionStatus, AssetPublishError> {
        let client_id = self.configured_client_id()?;
        let response = self
            .client
            .post(format!("{}/login/device/code", self.oauth_base))
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&[("client_id", client_id)])
            .send()
            .await
            .map_err(network_error)?;
        let device: DeviceCodeResponse = decode_oauth_response(response, "GITHUB_DEVICE_FLOW_START_FAILED").await?;
        self.persist_pending(user_id, &device).await
    }

    async fn poll_authorization(&self, user_id: &str) -> Result<HubPublishConnectionStatus, AssetPublishError> {
        let client_id = self.configured_client_id()?;
        let row = self
            .credentials
            .get(user_id)
            .await?
            .ok_or_else(|| AssetPublishError::HubPublishPrerequisite("GITHUB_AUTHORIZATION_NOT_STARTED".into()))?;
        if row.state != "authorizationPending" {
            return Ok(self.status_from_row(Some(&row)));
        }
        if row.device_expires_at.is_none_or(|expires| expires <= now_ms()) {
            self.credentials.delete(user_id).await?;
            return Ok(connection_status(
                HubPublishConnectionState::Disconnected,
                None,
                Some("GITHUB_DEVICE_CODE_EXPIRED".into()),
            ));
        }
        if row.next_poll_at.is_some_and(|next| next > now_ms()) {
            return Ok(self.status_from_row(Some(&row)));
        }
        let device_code = row
            .device_code_ciphertext
            .as_deref()
            .ok_or_else(|| AssetPublishError::HubPublishPrerequisite("GITHUB_AUTHORIZATION_NOT_STARTED".into()))
            .and_then(|value| decrypt_string(value, &self.encryption_key).map_err(crypto_error))?;
        let response = self
            .client
            .post(format!("{}/login/oauth/access_token", self.oauth_base))
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&[
                ("client_id", client_id),
                ("device_code", device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await
            .map_err(network_error)?;
        let status = response.status();
        let body: TokenOrErrorResponse = response
            .json()
            .await
            .map_err(|_| AssetPublishError::HubPublishFailed("GITHUB_DEVICE_FLOW_RESPONSE_INVALID".into()))?;
        if !status.is_success() {
            return Err(AssetPublishError::HubPublishFailed(
                "GITHUB_DEVICE_FLOW_REQUEST_FAILED".into(),
            ));
        }
        match body {
            TokenOrErrorResponse::Token(token) => {
                let user = self.authenticated_identity(&token.access_token).await?;
                let upstream_response = self
                    .api_request(
                        Method::GET,
                        &format!("/repos/{HUB_OWNER}/{HUB_REPOSITORY}"),
                        &token.access_token,
                    )
                    .send()
                    .await
                    .map_err(network_error)?;
                if upstream_response.status() == StatusCode::FORBIDDEN {
                    let connected = self.persist_connected(user_id, &token, &user.login).await?;
                    let refreshed = self
                        .credentials
                        .get(user_id)
                        .await?
                        .ok_or_else(|| AssetPublishError::Internal("保存 GitHub 发布凭据后找不到记录".into()))?;
                    self.mark_permission_failure(&refreshed, "GITHUB_INSUFFICIENT_PERMISSIONS")
                        .await?;
                    return Ok(HubPublishConnectionStatus {
                        state: HubPublishConnectionState::InsufficientPermissions,
                        reason_code: Some("GITHUB_INSUFFICIENT_PERMISSIONS".into()),
                        ..connected
                    });
                }
                let _: GithubRepository =
                    decode_api_response(upstream_response, "GITHUB_REPOSITORY_ACCESS_FAILED").await?;
                self.persist_connected(user_id, &token, &user.login).await
            }
            TokenOrErrorResponse::Error(error) => match error.error.as_str() {
                "authorization_pending" => {
                    let interval = row.poll_interval_seconds.unwrap_or(5).max(1);
                    self.update_pending_after_poll(&row, interval, None).await
                }
                "slow_down" => {
                    let interval = row.poll_interval_seconds.unwrap_or(5).max(1).saturating_add(5);
                    self.update_pending_after_poll(&row, interval, Some("GITHUB_DEVICE_SLOW_DOWN"))
                        .await
                }
                "access_denied" => {
                    self.credentials.delete(user_id).await?;
                    Ok(connection_status(
                        HubPublishConnectionState::Disconnected,
                        None,
                        Some("GITHUB_AUTHORIZATION_DENIED".into()),
                    ))
                }
                "expired_token" | "bad_verification_code" => {
                    self.credentials.delete(user_id).await?;
                    Ok(connection_status(
                        HubPublishConnectionState::Disconnected,
                        None,
                        Some("GITHUB_DEVICE_CODE_EXPIRED".into()),
                    ))
                }
                _ => Err(AssetPublishError::HubPublishFailed(
                    "GITHUB_DEVICE_FLOW_REJECTED".into(),
                )),
            },
        }
    }

    async fn disconnect(&self, user_id: &str) -> Result<HubPublishConnectionStatus, AssetPublishError> {
        self.credentials.delete(user_id).await?;
        Ok(if self.client_id.is_some() {
            connection_status(HubPublishConnectionState::Disconnected, None, None)
        } else {
            connection_status(HubPublishConnectionState::NotConfigured, None, None)
        })
    }

    async fn publish(
        &self,
        user_id: &str,
        request: &HubAssetPublishRequest,
        package: CanonicalAssetPackage,
    ) -> Result<HubAssetPublishResponse, AssetPublishError> {
        // All validation precedes the first network request, so a rejected
        // package cannot create a fork, object, branch or pull request.
        validate_publish_request(request)?;
        validate_package_identity(request, &package)?;
        validate_package_contents(&package)?;
        let package_digest = canonical_package_digest(&package)?;
        let request_digest = publish_request_digest(request, &package_digest)?;
        let operation_id = publish_operation_id(user_id, &request.idempotency_key);
        let branch = branch_name(&request.package_name, &package_digest);
        let operation = self
            .operations
            .start_or_get(StartGithubPublishOperationParams {
                user_id,
                idempotency_key: &request.idempotency_key,
                operation_id: &operation_id,
                request_digest: &request_digest,
                package_digest: &package_digest,
                asset_id: &request.asset_id,
                package_name: &request.package_name,
                version: &request.version,
                branch_name: &branch,
            })
            .await?;
        if operation.request_digest != request_digest {
            return Err(AssetPublishError::HubPublishConflict(
                "GITHUB_IDEMPOTENCY_KEY_REUSED".into(),
            ));
        }
        if operation.state == "succeeded" {
            let branch_name = operation
                .branch_name
                .clone()
                .ok_or_else(|| AssetPublishError::Internal("发布成功记录缺少分支名".into()))?;
            let pull_request_url = operation
                .pull_request_url
                .clone()
                .ok_or_else(|| AssetPublishError::Internal("发布成功记录缺少 PR 地址".into()))?;
            return Ok(publish_response(&operation.operation_id, branch_name, pull_request_url));
        }

        let operation = self
            .update_publish_operation(&operation, "running", "authorizing", None, None)
            .await?;
        let outcome = async {
            let _ = self.configured_client_id()?;
            let (token, row) = self.access_token(user_id).await?;
            let identity = match self.authenticated_identity(&token).await {
                Ok(identity) => identity,
                Err(AssetPublishError::HubPublishPrerequisite(code)) if code == "GITHUB_AUTH_REVOKED" => {
                    self.credentials.delete(user_id).await?;
                    return Err(AssetPublishError::HubPublishPrerequisite(code));
                }
                Err(error) => return Err(error),
            };
            if row
                .account_login
                .as_deref()
                .is_some_and(|stored| stored != identity.login)
            {
                self.credentials.delete(user_id).await?;
                return Err(AssetPublishError::HubPublishPrerequisite(
                    "GITHUB_ACCOUNT_CHANGED".into(),
                ));
            }
            match self
                .publish_package(
                    request,
                    &package,
                    &operation.operation_id,
                    &operation.package_digest,
                    &token,
                    &identity.login,
                )
                .await
            {
                Err(AssetPublishError::HubPublishPrerequisite(code)) if code == "GITHUB_AUTH_REVOKED" => {
                    self.credentials.delete(user_id).await?;
                    Err(AssetPublishError::HubPublishPrerequisite(code))
                }
                Err(AssetPublishError::HubPublishPrerequisite(code)) if code == "GITHUB_INSUFFICIENT_PERMISSIONS" => {
                    self.mark_permission_failure(&row, &code).await?;
                    Err(AssetPublishError::HubPublishPrerequisite(code))
                }
                result => result,
            }
        }
        .await;

        match outcome {
            Ok(response) => {
                self.update_publish_operation(
                    &operation,
                    "succeeded",
                    "pullRequestCreated",
                    Some(&response.pull_request_url),
                    None,
                )
                .await?;
                Ok(response)
            }
            Err(error) => {
                let error_code = publish_error_code(&error);
                self.update_publish_operation(&operation, "failed", "recoverable", None, Some(error_code))
                    .await?;
                Err(error)
            }
        }
    }
}

#[derive(Clone)]
pub struct HubPublisher {
    provider: Arc<dyn HubPublishProvider>,
}

impl HubPublisher {
    pub fn new(provider: Arc<dyn HubPublishProvider>) -> Self {
        Self { provider }
    }

    pub fn publish_request(
        &self,
        request: &HubAssetPublishRequest,
        package: CanonicalAssetPackage,
    ) -> Result<HubAssetPublishPreparation, AssetPublishError> {
        validate_publish_request(request)?;
        validate_package_identity(request, &package)?;
        validate_package_contents(&package)?;
        Ok(HubAssetPublishPreparation {
            repository: HUB_REPOSITORY_URL.into(),
            status: "notPushed".into(),
            package,
            proposed_branch_name: branch_name(&request.package_name, "preview"),
            base_branch: HUB_BASE_BRANCH.into(),
            manual_contribution_url: HUB_MANUAL_CONTRIBUTION_URL.into(),
            requires_user_action: true,
            warning_codes: Vec::new(),
            blocked_fields: Vec::new(),
        })
    }

    pub async fn connection_status(&self, user_id: &str) -> Result<HubPublishConnectionStatus, AssetPublishError> {
        self.provider.connection_status(user_id).await
    }

    pub async fn start_authorization(&self, user_id: &str) -> Result<HubPublishConnectionStatus, AssetPublishError> {
        self.provider.start_authorization(user_id).await
    }

    pub async fn poll_authorization(&self, user_id: &str) -> Result<HubPublishConnectionStatus, AssetPublishError> {
        self.provider.poll_authorization(user_id).await
    }

    pub async fn disconnect(&self, user_id: &str) -> Result<HubPublishConnectionStatus, AssetPublishError> {
        self.provider.disconnect(user_id).await
    }

    pub async fn publish(
        &self,
        user_id: &str,
        request: &HubAssetPublishRequest,
        package: CanonicalAssetPackage,
    ) -> Result<HubAssetPublishResponse, AssetPublishError> {
        self.provider.publish(user_id, request, package).await
    }
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: i64,
    interval: i64,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum TokenOrErrorResponse {
    Token(TokenResponse),
    Error(OAuthErrorResponse),
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    refresh_token_expires_in: Option<i64>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    token_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OAuthErrorResponse {
    error: String,
}

#[derive(Debug, Deserialize)]
struct GithubUser {
    login: String,
}

#[derive(Debug, Deserialize)]
struct GithubRepository {
    #[allow(dead_code)]
    name: String,
}

#[derive(Debug, Deserialize)]
struct GithubObject {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct GithubReference {
    object: GithubObject,
}

#[derive(Debug, Deserialize)]
struct GithubCommit {
    tree: GithubObject,
}

#[derive(Debug, Deserialize)]
struct GithubTree {
    tree: Vec<GithubTreeEntry>,
    #[serde(default)]
    truncated: bool,
}

#[derive(Debug, Deserialize)]
struct GithubTreeEntry {
    path: String,
    mode: String,
    #[serde(rename = "type")]
    kind: String,
    sha: String,
}

#[derive(Debug, Deserialize)]
struct GithubPullRequest {
    html_url: String,
}

fn connection_status(
    state: HubPublishConnectionState,
    account: Option<String>,
    reason_code: Option<String>,
) -> HubPublishConnectionStatus {
    HubPublishConnectionStatus {
        state,
        account,
        user_code: None,
        verification_uri: None,
        expires_at: None,
        poll_after_ms: None,
        reason_code,
    }
}

async fn decode_oauth_response<T: DeserializeOwned>(
    response: reqwest::Response,
    failure_code: &'static str,
) -> Result<T, AssetPublishError> {
    if !response.status().is_success() {
        return Err(AssetPublishError::HubPublishFailed(failure_code.into()));
    }
    response
        .json()
        .await
        .map_err(|_| AssetPublishError::HubPublishFailed(format!("{failure_code}_INVALID_RESPONSE")))
}

async fn decode_api_response<T: DeserializeOwned>(
    response: reqwest::Response,
    failure_code: &'static str,
) -> Result<T, AssetPublishError> {
    let status = response.status();
    if status == StatusCode::UNAUTHORIZED {
        return Err(AssetPublishError::HubPublishPrerequisite("GITHUB_AUTH_REVOKED".into()));
    }
    if status == StatusCode::FORBIDDEN {
        return Err(AssetPublishError::HubPublishPrerequisite(
            "GITHUB_INSUFFICIENT_PERMISSIONS".into(),
        ));
    }
    if !status.is_success() {
        return Err(AssetPublishError::HubPublishFailed(failure_code.into()));
    }
    response
        .json()
        .await
        .map_err(|_| AssetPublishError::HubPublishFailed(format!("{failure_code}_INVALID_RESPONSE")))
}

fn network_error(_error: reqwest::Error) -> AssetPublishError {
    // Never include a debug rendering of a request because it may carry the
    // Authorization header. The public error code is sufficient for support.
    AssetPublishError::HubNetwork("GITHUB_NETWORK_ERROR".into())
}

fn crypto_error(_error: tjuaeui_common::CryptoError) -> AssetPublishError {
    AssetPublishError::Internal("GITHUB_CREDENTIAL_CRYPTO_FAILED".into())
}

fn parse_scopes(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

fn package_repository_files(package: &CanonicalAssetPackage) -> Result<BTreeMap<String, String>, AssetPublishError> {
    validate_package_contents(package)?;
    let prefix = format!("submissions/{}/", package.package_name);
    let mut files = BTreeMap::new();
    files.insert(
        format!("{prefix}asset-package.json"),
        serde_json::to_string_pretty(&package.manifest)?,
    );
    for file in &package.files {
        files.insert(format!("{prefix}{}", file.path), file.content.clone());
    }
    Ok(files)
}

fn canonical_package_digest(package: &CanonicalAssetPackage) -> Result<String, AssetPublishError> {
    let files = package_repository_files(package)?;
    let mut hasher = Sha256::new();
    for (path, content) in files {
        hasher.update((path.len() as u64).to_be_bytes());
        hasher.update(path.as_bytes());
        hasher.update((content.len() as u64).to_be_bytes());
        hasher.update(content.as_bytes());
    }
    Ok(hex::encode(hasher.finalize()))
}

fn git_blob_sha(content: &str) -> String {
    let bytes = content.as_bytes();
    let mut hasher = Sha1::new();
    hasher.update(format!("blob {}\0", bytes.len()).as_bytes());
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn publish_request_digest(request: &HubAssetPublishRequest, package_digest: &str) -> Result<String, AssetPublishError> {
    let canonical = serde_json::to_vec(&json!({
        "assetKind": request.asset_kind,
        "assetId": request.asset_id,
        "packageName": request.package_name,
        "version": request.version,
        "author": request.author,
        "license": request.license,
        "sourceRepository": request.source_repository,
        "tags": request.tags,
        "metadataConfirmed": request.metadata_confirmed,
        "title": request.title,
        "body": request.body,
        "packageDigest": package_digest,
    }))?;
    Ok(hex::encode(Sha256::digest(canonical)))
}

fn publish_operation_id(user_id: &str, idempotency_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(user_id.as_bytes());
    hasher.update([0]);
    hasher.update(idempotency_key.as_bytes());
    format!("publish-{}", &hex::encode(hasher.finalize())[..24])
}

fn publish_error_code(error: &AssetPublishError) -> &str {
    match error {
        AssetPublishError::HubPublishPrerequisite(code)
        | AssetPublishError::HubPublishFailed(code)
        | AssetPublishError::HubPublishConflict(code)
        | AssetPublishError::HubNetwork(code) => code,
        AssetPublishError::AssetSanitization(_) => "HUB_ASSET_UNSAFE",
        AssetPublishError::HubIntegrity(_) => "HUB_INTEGRITY_FAILED",
        AssetPublishError::InvalidRequest(_) | AssetPublishError::InvalidVersion { .. } => "HUB_INVALID_REQUEST",
        AssetPublishError::Database(_) => "HUB_PUBLISH_STATE_FAILED",
        _ => "HUB_PUBLISH_FAILED",
    }
}

fn validate_package_identity(
    request: &HubAssetPublishRequest,
    package: &CanonicalAssetPackage,
) -> Result<(), AssetPublishError> {
    let mut expected_tags = request.tags.clone();
    expected_tags.sort();
    expected_tags.dedup();
    let expected_tags = expected_tags.into_iter().map(Value::String).collect::<Vec<_>>();
    if package.package_name != request.package_name
        || package.manifest.get("name").and_then(Value::as_str) != Some(request.package_name.as_str())
        || package.manifest.get("version").and_then(Value::as_str) != Some(request.version.as_str())
        || package.manifest.get("author").and_then(Value::as_str) != Some(request.author.trim())
        || package.manifest.get("license").and_then(Value::as_str) != Some(request.license.trim())
        || package.manifest.pointer("/source/repository").and_then(Value::as_str)
            != Some(request.source_repository.trim())
        || package.manifest.get("tags").and_then(Value::as_array) != Some(&expected_tags)
    {
        return Err(AssetPublishError::HubIntegrity("规范包身份与发布请求不一致".into()));
    }
    Ok(())
}

fn validate_package_contents(package: &CanonicalAssetPackage) -> Result<(), AssetPublishError> {
    validate_package_name(&package.package_name)?;
    validate_asset_package_manifest(&package.manifest)?;
    let manifest = serde_json::to_string(&package.manifest)?;
    validate_public_asset_file("asset-package.json", &manifest)?;
    let mut paths = HashSet::new();
    for file in &package.files {
        let _ = safe_relative_path(&file.path)?;
        if file.path.eq_ignore_ascii_case("asset-package.json") {
            return Err(AssetPublishError::AssetSanitization(
                "规范文件不能覆盖 asset-package.json".into(),
            ));
        }
        if !paths.insert(file.path.to_ascii_lowercase()) {
            return Err(AssetPublishError::AssetSanitization(format!(
                "规范包包含重复或大小写冲突路径：{}",
                file.path
            )));
        }
        validate_public_asset_file(&file.path, &file.content)?;
        let actual_size = file.content.len() as u64;
        let actual_sha256 = format!("sha256-{}", hex::encode(Sha256::digest(file.content.as_bytes())));
        if file.size != actual_size || file.sha256 != actual_sha256 {
            return Err(AssetPublishError::HubIntegrity(format!(
                "规范文件 {} 的摘要或大小不一致",
                file.path
            )));
        }
    }
    let definition_file = package
        .manifest
        .pointer("/assets/0/definitionFile")
        .and_then(Value::as_str)
        .ok_or_else(|| AssetPublishError::HubIntegrity("规范包缺少 Definition 入口".into()))?;
    if !package.files.iter().any(|file| file.path == definition_file) {
        return Err(AssetPublishError::HubIntegrity(format!(
            "规范包缺少声明的 Definition 文件：{definition_file}"
        )));
    }
    Ok(())
}

fn validate_asset_package_manifest(manifest: &Value) -> Result<(), AssetPublishError> {
    let object = manifest
        .as_object()
        .ok_or_else(|| AssetPublishError::HubIntegrity("asset-package.json 顶层必须是对象".into()))?;
    let required = [
        "$schema",
        "schemaVersion",
        "name",
        "version",
        "displayName",
        "description",
        "author",
        "license",
        "compatibility",
        "source",
        "tags",
        "assets",
    ];
    if required.iter().any(|field| !object.contains_key(*field)) {
        return Err(AssetPublishError::HubIntegrity(
            "asset-package.json 缺少原子资产包必填字段".into(),
        ));
    }
    let allowed = [
        "$schema",
        "schemaVersion",
        "name",
        "version",
        "displayName",
        "description",
        "author",
        "license",
        "compatibility",
        "publisher",
        "source",
        "status",
        "review",
        "tags",
        "assets",
    ];
    if object.keys().any(|field| !allowed.contains(&field.as_str()))
        || manifest.get("$schema").and_then(Value::as_str) != Some(ASSET_PACKAGE_SCHEMA_URL)
        || manifest.get("schemaVersion").and_then(Value::as_u64) != Some(1)
    {
        return Err(AssetPublishError::HubIntegrity(
            "asset-package.json 不符合 v1 纯声明契约".into(),
        ));
    }
    let assets = manifest
        .get("assets")
        .and_then(Value::as_array)
        .filter(|assets| assets.len() == 1)
        .ok_or_else(|| AssetPublishError::HubIntegrity("原子资产包必须且只能声明一个资产".into()))?;
    let asset = assets[0]
        .as_object()
        .ok_or_else(|| AssetPublishError::HubIntegrity("assets[0] 必须是对象".into()))?;
    let asset_fields = ["kind", "id", "runtimeId", "definitionFile", "dependencies"];
    if asset.len() != asset_fields.len() || asset_fields.iter().any(|field| !asset.contains_key(*field)) {
        return Err(AssetPublishError::HubIntegrity(
            "assets[0] 必须使用固定的原子资产字段".into(),
        ));
    }
    let expected_entry = match asset.get("kind").and_then(Value::as_str) {
        Some("assistant") => "assistant.json",
        Some("engineAdapter") => "engine-adapter.json",
        Some("skill") => "SKILL.md",
        Some("mcp") => "mcp.json",
        _ => return Err(AssetPublishError::HubIntegrity("assets[0].kind 无效".into())),
    };
    if asset.get("definitionFile").and_then(Value::as_str) != Some(expected_entry)
        || asset.get("id").and_then(Value::as_str).is_none()
        || asset.get("runtimeId").and_then(Value::as_str).is_none()
        || asset.get("dependencies").and_then(Value::as_array).is_none()
    {
        return Err(AssetPublishError::HubIntegrity(
            "assets[0] 的身份、入口或依赖无效".into(),
        ));
    }
    Ok(())
}

fn validate_publish_request(request: &HubAssetPublishRequest) -> Result<(), AssetPublishError> {
    validate_package_name(&request.package_name)?;
    semver::Version::parse(&request.version).map_err(|_| AssetPublishError::InvalidVersion {
        version: request.version.clone(),
        reason: "必须是完整 SemVer".into(),
    })?;
    if request.asset_id.trim().is_empty() {
        return Err(AssetPublishError::InvalidRequest("assetId 不能为空".into()));
    }
    validate_publication_metadata(
        &request.author,
        &request.license,
        &request.source_repository,
        request.metadata_confirmed,
    )?;
    if request.idempotency_key.trim().is_empty() || request.idempotency_key.len() > 200 {
        return Err(AssetPublishError::InvalidRequest("idempotencyKey 无效".into()));
    }
    if request.title.as_ref().is_some_and(|title| title.chars().count() > 200)
        || request.body.as_ref().is_some_and(|body| body.chars().count() > 16_000)
    {
        return Err(AssetPublishError::InvalidRequest("PR 标题或正文过长".into()));
    }
    validate_public_asset_file("pull-request/title.txt", &publish_title(request))?;
    validate_public_asset_file("pull-request/body.md", &publish_body(request))?;
    Ok(())
}

fn validate_package_name(value: &str) -> Result<(), AssetPublishError> {
    if !(12..=96).contains(&value.len()) || !value.starts_with("tjuaeasset-") {
        return Err(AssetPublishError::InvalidRequest(
            "packageName 必须使用 tjuaeasset- 前缀".into(),
        ));
    }
    let suffix = &value["tjuaeasset-".len()..];
    if suffix.is_empty()
        || suffix.starts_with('-')
        || suffix.ends_with('-')
        || suffix.contains("--")
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(AssetPublishError::InvalidRequest(
            "packageName 不是合法 kebab-case".into(),
        ));
    }
    Ok(())
}

fn validate_github_login(value: &str) -> Result<(), AssetPublishError> {
    if value.is_empty()
        || value.len() > 39
        || value.starts_with('-')
        || value.ends_with('-')
        || value.contains("--")
        || !value.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(AssetPublishError::HubPublishPrerequisite(
            "GITHUB_ACCOUNT_INVALID".into(),
        ));
    }
    Ok(())
}

fn branch_name(package_name: &str, package_digest: &str) -> String {
    let suffix = package_digest.get(..12).unwrap_or(package_digest);
    format!("tjuae-publish-{package_name}-{suffix}")
        .chars()
        .take(120)
        .collect()
}

fn publish_title(request: &HubAssetPublishRequest) -> String {
    request
        .title
        .clone()
        .unwrap_or_else(|| format!("提交资产 {} {}", request.package_name, request.version))
}

fn publish_body(request: &HubAssetPublishRequest) -> String {
    request.body.clone().unwrap_or_else(|| {
        format!(
            "## 资产发布\n\n- 资产 ID：`{}`\n- 类型：`{:?}`\n- 版本：`{}`\n- 作者：{}\n- 许可证：`{}`\n\n作者与许可证由发布者明确提供并确认；该内容由 Tjuae Core 安全发布流程生成。",
            request.asset_id,
            request.asset_kind,
            request.version,
            request.author.trim(),
            request.license.trim()
        )
    })
}

fn publish_response(operation_id: &str, branch_name: String, pull_request_url: String) -> HubAssetPublishResponse {
    HubAssetPublishResponse {
        status: "published".into(),
        operation_id: operation_id.into(),
        branch_name,
        pull_request_url,
        repository: HUB_REPOSITORY_URL.into(),
    }
}

fn url_encode_query(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                vec![byte as char]
            } else {
                format!("%{byte:02X}").chars().collect()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tjuaeui_db::{SqliteGithubPublishCredentialRepository, SqliteGithubPublishOperationRepository};
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    struct TestContext {
        provider: GitHubRestPublishProvider,
        credentials: Arc<SqliteGithubPublishCredentialRepository>,
        operations: Arc<SqliteGithubPublishOperationRepository>,
        _database: tjuaeui_db::Database,
    }

    #[test]
    fn github_app_client_id_is_trimmed_and_empty_values_are_rejected() {
        assert_eq!(
            normalize_client_id("  Iv1.public-client-id  ").as_deref(),
            Some("Iv1.public-client-id")
        );
        assert_eq!(normalize_client_id(" \t\r\n "), None);
    }

    async fn provider_context(server: &MockServer, client_id: Option<&str>) -> TestContext {
        let database = tjuaeui_db::init_database_memory().await.unwrap();
        let credentials = Arc::new(SqliteGithubPublishCredentialRepository::new(database.pool().clone()));
        let operations = Arc::new(SqliteGithubPublishOperationRepository::new(database.pool().clone()));
        let provider = GitHubRestPublishProvider::for_test(
            reqwest::Client::builder()
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap(),
            credentials.clone(),
            operations.clone(),
            [7; 32],
            client_id,
            &server.uri(),
        );
        TestContext {
            provider,
            credentials,
            operations,
            _database: database,
        }
    }

    async fn connect(provider: &GitHubRestPublishProvider, access_token: &str) {
        provider
            .persist_connected(
                "system_default_user",
                &TokenResponse {
                    access_token: access_token.into(),
                    expires_in: Some(28_800),
                    refresh_token: Some("refresh-secret".into()),
                    refresh_token_expires_in: Some(15_897_600),
                    scope: Some(String::new()),
                    token_type: Some("bearer".into()),
                },
                "octocat",
            )
            .await
            .unwrap();
    }

    async fn mount_identity_and_fork(server: &MockServer, access_token: &str) {
        Mock::given(method("GET"))
            .and(path("/user"))
            .and(header("authorization", format!("Bearer {access_token}")))
            .and(header("x-github-api-version", GITHUB_API_VERSION))
            .and(header("user-agent", concat!("TjuaeCore/", env!("CARGO_PKG_VERSION"))))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"login": "octocat"})))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/octocat/TjuaeHub"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"name": "TjuaeHub"})))
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/repos/octocat/TjuaeHub/merge-upstream"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message": "synced"})))
            .mount(server)
            .await;
    }

    fn request() -> HubAssetPublishRequest {
        HubAssetPublishRequest {
            asset_kind: tjuaeui_api_types::HubAssetKind::Skill,
            asset_id: "skill:demo".into(),
            package_name: "tjuaeasset-demo".into(),
            version: "1.0.0".into(),
            author: "Demo Author".into(),
            license: "MIT".into(),
            source_repository: "https://github.com/example/demo".into(),
            tags: vec!["skill".into()],
            metadata_confirmed: true,
            idempotency_key: "retry-key".into(),
            title: None,
            body: None,
        }
    }

    fn package() -> CanonicalAssetPackage {
        let content = "# Demo";
        CanonicalAssetPackage {
            package_name: "tjuaeasset-demo".into(),
            manifest: json!({
                "$schema": ASSET_PACKAGE_SCHEMA_URL,
                "schemaVersion": 1,
                "name": "tjuaeasset-demo",
                "version": "1.0.0",
                "displayName": "Demo",
                "description": "Demo skill",
                "author": "Demo Author",
                "license": "MIT",
                "compatibility": {"tjuae": "^1.0.0"},
                "source": {
                    "repository": "https://github.com/example/demo",
                    "license": "MIT"
                },
                "tags": ["skill"],
                "assets": [{
                    "kind": "skill",
                    "id": "skill:demo",
                    "runtimeId": "demo",
                    "definitionFile": "SKILL.md",
                    "dependencies": []
                }]
            }),
            files: vec![tjuaeui_api_types::CanonicalAssetFile {
                path: "SKILL.md".into(),
                content: content.into(),
                sha256: format!("sha256-{}", hex::encode(Sha256::digest(content.as_bytes()))),
                size: content.len() as u64,
            }],
        }
    }

    #[test]
    fn publish_validation_rejects_unconfirmed_or_mismatched_legal_metadata() {
        let mut unconfirmed = request();
        unconfirmed.metadata_confirmed = false;
        assert!(matches!(
            validate_publish_request(&unconfirmed),
            Err(AssetPublishError::InvalidRequest(_))
        ));

        let mut blank_author = request();
        blank_author.author = "   ".into();
        assert!(matches!(
            validate_publish_request(&blank_author),
            Err(AssetPublishError::InvalidRequest(_))
        ));

        let mut mismatched = package();
        mismatched.manifest["license"] = json!("Apache-2.0");
        assert!(matches!(
            validate_package_identity(&request(), &mismatched),
            Err(AssetPublishError::HubIntegrity(_))
        ));
    }

    #[tokio::test]
    async fn missing_client_id_is_explicit_and_does_not_call_network() {
        let server = MockServer::start().await;
        let context = provider_context(&server, None).await;
        let provider = &context.provider;
        let status = provider.connection_status("system_default_user").await.unwrap();
        assert_eq!(status.state, HubPublishConnectionState::NotConfigured);
        assert!(provider.start_authorization("system_default_user").await.is_err());
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn device_flow_success_encrypts_tokens_and_returns_only_account() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/login/device/code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "device_code": "device-secret",
                "user_code": "ABCD-EFGH",
                "verification_uri": "https://github.com/login/device",
                "expires_in": 900,
                "interval": 0
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/login/oauth/access_token"))
            .and(body_string_contains("device-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "ghu_super_secret",
                "expires_in": 28800,
                "refresh_token": "ghr_refresh_secret",
                "refresh_token_expires_in": 15897600,
                "token_type": "bearer",
                "scope": ""
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/user"))
            .and(header("authorization", "Bearer ghu_super_secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"login": "octocat"})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/liangboqiang/TjuaeHub"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"name": "TjuaeHub"})))
            .mount(&server)
            .await;

        let context = provider_context(&server, Some("client-id")).await;
        let provider = &context.provider;
        let pending = provider.start_authorization("system_default_user").await.unwrap();
        assert_eq!(pending.user_code.as_deref(), Some("ABCD-EFGH"));
        tokio::time::sleep(Duration::from_millis(1_050)).await;
        let connected = provider.poll_authorization("system_default_user").await.unwrap();
        assert_eq!(connected.state, HubPublishConnectionState::Connected);
        assert_eq!(connected.account.as_deref(), Some("octocat"));
        let serialized = serde_json::to_string(&connected).unwrap();
        assert!(!serialized.contains("ghu_"));
        assert!(!serialized.contains("ghr_"));
        assert!(!serialized.contains("device-secret"));
        let persisted = context.credentials.get("system_default_user").await.unwrap().unwrap();
        assert!(
            !persisted
                .access_token_ciphertext
                .as_deref()
                .unwrap()
                .contains("ghu_super_secret")
        );
        assert!(
            !persisted
                .refresh_token_ciphertext
                .as_deref()
                .unwrap()
                .contains("ghr_refresh_secret")
        );
        assert!(context.credentials.get("other-user").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn device_flow_slow_down_increases_the_persisted_poll_interval() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/login/device/code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "device_code": "device-secret",
                "user_code": "ABCD-EFGH",
                "verification_uri": "https://github.com/login/device",
                "expires_in": 900,
                "interval": 0
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/login/oauth/access_token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"error": "slow_down"})))
            .mount(&server)
            .await;
        let context = provider_context(&server, Some("client-id")).await;
        context
            .provider
            .start_authorization("system_default_user")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(1_050)).await;
        let pending = context
            .provider
            .poll_authorization("system_default_user")
            .await
            .unwrap();
        assert_eq!(pending.state, HubPublishConnectionState::AuthorizationPending);
        assert_eq!(pending.reason_code.as_deref(), Some("GITHUB_DEVICE_SLOW_DOWN"));
        assert!(
            pending
                .poll_after_ms
                .is_some_and(|delay| (5_000..=6_000).contains(&delay))
        );
        let row = context.credentials.get("system_default_user").await.unwrap().unwrap();
        assert_eq!(row.poll_interval_seconds, Some(6));
    }

    #[tokio::test]
    async fn device_flow_denial_clears_the_pending_secret() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/login/device/code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "device_code": "device-secret",
                "user_code": "ABCD-EFGH",
                "verification_uri": "https://github.com/login/device",
                "expires_in": 900,
                "interval": 0
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/login/oauth/access_token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"error": "access_denied"})))
            .mount(&server)
            .await;
        let context = provider_context(&server, Some("client-id")).await;
        context
            .provider
            .start_authorization("system_default_user")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(1_050)).await;
        let denied = context
            .provider
            .poll_authorization("system_default_user")
            .await
            .unwrap();
        assert_eq!(denied.state, HubPublishConnectionState::Disconnected);
        assert_eq!(denied.reason_code.as_deref(), Some("GITHUB_AUTHORIZATION_DENIED"));
        assert!(context.credentials.get("system_default_user").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn expired_device_code_is_removed_without_polling_github() {
        let server = MockServer::start().await;
        let context = provider_context(&server, Some("client-id")).await;
        let encrypted_device = encrypt_string("device-secret", &[7; 32]).unwrap();
        context
            .credentials
            .upsert(UpsertGithubPublishCredentialParams {
                user_id: "system_default_user",
                state: "authorizationPending",
                access_token_ciphertext: None,
                refresh_token_ciphertext: None,
                token_type: None,
                access_expires_at: None,
                refresh_expires_at: None,
                account_login: None,
                scopes_json: "[]",
                device_code_ciphertext: Some(&encrypted_device),
                user_code: Some("ABCD-EFGH"),
                verification_uri: Some("https://github.com/login/device"),
                device_expires_at: Some(now_ms() - 1),
                poll_interval_seconds: Some(5),
                next_poll_at: Some(now_ms() - 1),
                last_error_code: None,
            })
            .await
            .unwrap();
        let expired = context
            .provider
            .poll_authorization("system_default_user")
            .await
            .unwrap();
        assert_eq!(expired.state, HubPublishConnectionState::Disconnected);
        assert_eq!(expired.reason_code.as_deref(), Some("GITHUB_DEVICE_CODE_EXPIRED"));
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn unsafe_package_fails_before_any_remote_write() {
        let server = MockServer::start().await;
        let context = provider_context(&server, Some("client-id")).await;
        let provider = &context.provider;
        let mut unsafe_package = package();
        unsafe_package.files[0].content = "token = ghp_abcdefghijklmnopqrstuvwxyz123456".into();
        unsafe_package.files[0].size = unsafe_package.files[0].content.len() as u64;
        unsafe_package.files[0].sha256 = format!(
            "sha256-{}",
            hex::encode(Sha256::digest(unsafe_package.files[0].content.as_bytes()))
        );
        assert!(
            provider
                .publish("system_default_user", &request(), unsafe_package)
                .await
                .is_err()
        );
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn rest_publish_creates_one_commit_and_replays_the_durable_result() {
        let server = MockServer::start().await;
        let context = provider_context(&server, Some("client-id")).await;
        connect(&context.provider, "access-secret").await;
        mount_identity_and_fork(&server, "access-secret").await;
        let request = request();
        let package = package();
        let digest = canonical_package_digest(&package).unwrap();
        let branch = branch_name(&request.package_name, &digest);

        Mock::given(method("GET"))
            .and(path(format!("/repos/octocat/TjuaeHub/git/ref/heads/{branch}")))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/liangboqiang/TjuaeHub/git/ref/heads/main"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"object": {"sha": "base-commit"}})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/liangboqiang/TjuaeHub/git/commits/base-commit"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"tree": {"sha": "base-tree"}})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/repos/octocat/TjuaeHub/git/blobs"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"sha": "blob-sha"})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/repos/octocat/TjuaeHub/git/trees"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"sha": "tree-sha"})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/repos/octocat/TjuaeHub/git/commits"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"sha": "commit-sha"})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/repos/octocat/TjuaeHub/git/refs"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"object": {"sha": "commit-sha"}})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/repos/liangboqiang/TjuaeHub/pulls"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "html_url": "https://github.com/liangboqiang/TjuaeHub/pull/42"
            })))
            .mount(&server)
            .await;

        let first = context
            .provider
            .publish("system_default_user", &request, package.clone())
            .await
            .unwrap();
        assert_eq!(first.branch_name, branch);
        assert_eq!(
            first.pull_request_url,
            "https://github.com/liangboqiang/TjuaeHub/pull/42"
        );
        let request_count = server.received_requests().await.unwrap().len();
        let replay = context
            .provider
            .publish("system_default_user", &request, package.clone())
            .await
            .unwrap();
        assert_eq!(replay, first);
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            request_count,
            "a succeeded idempotency key must replay from Core without remote IO"
        );
        let operation = context
            .operations
            .get("system_default_user", &request.idempotency_key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(operation.state, "succeeded");
        assert_eq!(
            operation.pull_request_url.as_deref(),
            Some("https://github.com/liangboqiang/TjuaeHub/pull/42")
        );

        let mut changed_request = request.clone();
        changed_request.version = "1.0.1".into();
        let mut changed_package = package;
        changed_package.manifest["version"] = json!("1.0.1");
        let error = context
            .provider
            .publish("system_default_user", &changed_request, changed_package)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AssetPublishError::HubPublishConflict(ref code) if code == "GITHUB_IDEMPOTENCY_KEY_REUSED"
        ));
        assert_eq!(server.received_requests().await.unwrap().len(), request_count);
    }

    #[tokio::test]
    async fn retry_after_pr_response_timeout_recovers_the_existing_branch_and_pr() {
        let server = MockServer::start().await;
        let context = provider_context(&server, Some("client-id")).await;
        connect(&context.provider, "access-secret").await;
        mount_identity_and_fork(&server, "access-secret").await;
        let request = request();
        let package = package();
        let digest = canonical_package_digest(&package).unwrap();
        let branch = branch_name(&request.package_name, &digest);
        let branch_path = format!("/repos/octocat/TjuaeHub/git/ref/heads/{branch}");

        let missing_branch = Mock::given(method("GET"))
            .and(path(branch_path.clone()))
            .respond_with(ResponseTemplate::new(404))
            .mount_as_scoped(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/liangboqiang/TjuaeHub/git/ref/heads/main"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"object": {"sha": "base-commit"}})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/liangboqiang/TjuaeHub/git/commits/base-commit"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"tree": {"sha": "base-tree"}})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/repos/octocat/TjuaeHub/git/blobs"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"sha": "blob-sha"})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/repos/octocat/TjuaeHub/git/trees"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"sha": "tree-sha"})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/repos/octocat/TjuaeHub/git/commits"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"sha": "commit-sha"})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/repos/octocat/TjuaeHub/git/refs"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"object": {"sha": "commit-sha"}})))
            .mount(&server)
            .await;
        // The request reaches GitHub, but the response arrives after Core's
        // client timeout. A real GitHub deployment may already have created
        // the PR, so the retry must query before attempting another POST.
        let timed_out_pull = Mock::given(method("POST"))
            .and(path("/repos/liangboqiang/TjuaeHub/pulls"))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_delay(Duration::from_millis(2_500))
                    .set_body_json(json!({
                        "html_url": "https://github.com/liangboqiang/TjuaeHub/pull/77"
                    })),
            )
            .mount_as_scoped(&server)
            .await;

        let first_error = context
            .provider
            .publish("system_default_user", &request, package.clone())
            .await
            .unwrap_err();
        assert!(matches!(first_error, AssetPublishError::HubNetwork(_)));
        let failed = context
            .operations
            .get("system_default_user", &request.idempotency_key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(failed.state, "failed");
        drop(missing_branch);
        drop(timed_out_pull);
        tokio::time::sleep(Duration::from_millis(600)).await;

        Mock::given(method("GET"))
            .and(path(branch_path))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"object": {"sha": "commit-sha"}})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/octocat/TjuaeHub/git/commits/commit-sha"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"tree": {"sha": "tree-sha"}})))
            .mount(&server)
            .await;
        let expected_tree = package_repository_files(&package)
            .unwrap()
            .into_iter()
            .map(|(path, content)| {
                json!({
                    "path": path,
                    "mode": "100644",
                    "type": "blob",
                    "sha": git_blob_sha(&content)
                })
            })
            .collect::<Vec<_>>();
        Mock::given(method("GET"))
            .and(path("/repos/octocat/TjuaeHub/git/trees/tree-sha"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"tree": expected_tree})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/liangboqiang/TjuaeHub/pulls"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
                "html_url": "https://github.com/liangboqiang/TjuaeHub/pull/77"
            }])))
            .mount(&server)
            .await;

        let recovered = context
            .provider
            .publish("system_default_user", &request, package)
            .await
            .unwrap();
        assert_eq!(
            recovered.pull_request_url,
            "https://github.com/liangboqiang/TjuaeHub/pull/77"
        );
        let received = server.received_requests().await.unwrap();
        let pull_posts = received
            .iter()
            .filter(|request| {
                request.method.as_str() == "POST" && request.url.path() == "/repos/liangboqiang/TjuaeHub/pulls"
            })
            .count();
        assert_eq!(
            pull_posts, 1,
            "recovery must find the PR whose response timed out, not create a duplicate"
        );
    }

    #[tokio::test]
    async fn revoked_token_is_deleted_and_never_appears_in_the_error() {
        let server = MockServer::start().await;
        let context = provider_context(&server, Some("client-id")).await;
        connect(&context.provider, "top-secret-access-token").await;
        Mock::given(method("GET"))
            .and(path("/user"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let error = context
            .provider
            .publish("system_default_user", &request(), package())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AssetPublishError::HubPublishPrerequisite(ref code) if code == "GITHUB_AUTH_REVOKED"
        ));
        assert!(!error.to_string().contains("top-secret-access-token"));
        assert!(context.credentials.get("system_default_user").await.unwrap().is_none());
    }
}
