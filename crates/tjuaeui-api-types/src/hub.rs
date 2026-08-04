use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum HubAssetKind {
    Assistant,
    EngineAdapter,
    Skill,
    Mcp,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalAssetFile {
    pub path: String,
    pub content: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalAssetPackage {
    pub package_name: String,
    pub manifest: serde_json::Value,
    pub files: Vec<CanonicalAssetFile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HubAssetPublishRequest {
    pub asset_kind: HubAssetKind,
    pub asset_id: String,
    pub package_name: String,
    pub version: String,
    /// Legal attribution supplied and explicitly confirmed by the publisher.
    /// Core never infers either field from the application, account, or asset
    /// origin.
    pub author: String,
    pub license: String,
    /// Definition 的公开来源仓库。该字段由发布者明确填写，Core 不把
    /// TjuaeHub 提交目标冒充为资产来源。
    pub source_repository: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub metadata_confirmed: bool,
    /// Retried requests must reuse this key. Core never derives idempotency
    /// from mutable UI state.
    pub idempotency_key: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum HubPublishConnectionState {
    NotConfigured,
    Disconnected,
    AuthorizationPending,
    Connected,
    InsufficientPermissions,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum HubAssetPublishWarningCode {
    SensitiveFieldsRemoved,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HubPublishConnectionStatus {
    pub state: HubPublishConnectionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_uri: Option<String>,
    /// GitHub App installation page. It is returned only when the authenticated
    /// account has not installed the app with the repository access required by
    /// the publish workflow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_after_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HubAssetPublishPreparation {
    pub repository: String,
    pub status: String,
    pub package: CanonicalAssetPackage,
    pub proposed_branch_name: String,
    pub base_branch: String,
    pub manual_contribution_url: String,
    pub requires_user_action: bool,
    pub warning_codes: Vec<HubAssetPublishWarningCode>,
    pub blocked_fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HubAssetPublishResponse {
    pub status: String,
    pub operation_id: String,
    pub branch_name: String,
    pub pull_request_url: String,
    pub repository: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_request_has_no_token_field() {
        let value = serde_json::json!({
            "assetKind": "skill",
            "assetId": "demo",
            "packageName": "tjuaeasset-demo",
            "version": "1.0.0",
            "author": "Demo Author",
            "license": "MIT",
            "sourceRepository": "https://github.com/example/demo",
            "metadataConfirmed": true,
            "idempotencyKey": "publish-demo-1",
            "token": "must-not-be-accepted"
        });
        assert!(serde_json::from_value::<HubAssetPublishRequest>(value).is_err());
    }

    #[test]
    fn publish_request_requires_explicit_legal_metadata() {
        let without_metadata = serde_json::json!({
            "assetKind": "skill",
            "assetId": "demo",
            "packageName": "tjuaeasset-demo",
            "version": "1.0.0",
            "idempotencyKey": "publish-demo-1"
        });
        assert!(serde_json::from_value::<HubAssetPublishRequest>(without_metadata).is_err());

        let request: HubAssetPublishRequest = serde_json::from_value(serde_json::json!({
            "assetKind": "skill",
            "assetId": "demo",
            "packageName": "tjuaeasset-demo",
            "version": "1.0.0",
            "author": "Demo Author",
            "license": "MIT",
            "sourceRepository": "https://github.com/example/demo",
            "metadataConfirmed": true,
            "idempotencyKey": "publish-demo-1"
        }))
        .unwrap();
        assert_eq!(request.author, "Demo Author");
        assert_eq!(request.license, "MIT");
        assert!(request.metadata_confirmed);
    }

    #[test]
    fn publish_connection_status_never_contains_tokens() {
        let status = HubPublishConnectionStatus {
            state: HubPublishConnectionState::AuthorizationPending,
            account: None,
            user_code: Some("ABCD-EFGH".into()),
            verification_uri: Some("https://github.com/login/device".into()),
            installation_uri: None,
            expires_at: Some(123),
            poll_after_ms: Some(5_000),
            reason_code: None,
        };

        assert_eq!(
            serde_json::to_value(status).unwrap(),
            serde_json::json!({
                "state": "authorizationPending",
                "userCode": "ABCD-EFGH",
                "verificationUri": "https://github.com/login/device",
                "expiresAt": 123,
                "pollAfterMs": 5_000
            })
        );
    }

    #[test]
    fn publish_kind_uses_engine_adapter_without_agent_compatibility() {
        assert_eq!(
            serde_json::to_value(HubAssetKind::EngineAdapter).unwrap(),
            serde_json::json!("engineAdapter")
        );
        assert!(serde_json::from_value::<HubAssetKind>(serde_json::json!("agent")).is_err());
    }

    #[test]
    fn publish_warning_code_is_stable_and_machine_readable() {
        assert_eq!(
            serde_json::to_value(HubAssetPublishWarningCode::SensitiveFieldsRemoved).unwrap(),
            serde_json::json!("SENSITIVE_FIELDS_REMOVED")
        );
    }
}
