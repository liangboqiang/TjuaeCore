use serde::{Deserialize, Serialize};

/// 可在 Tjuae 本地资产库中编辑和运行的核心资产类型。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum AssetKind {
    Assistant,
    EngineAdapter,
    Skill,
    Mcp,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AssetOrigin {
    Local,
    Hub,
    Seed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AssetTrust {
    Official,
    Verified,
    Community,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AssetScope {
    System,
    User,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AssetEditability {
    ReadOnly,
    Overlay,
    Full,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AssetTrackingMode {
    Tracked,
    Detached,
}

/// 资产 Definition 的内容来源。Overlay 从不属于这些可共享内容。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum AssetContentSource {
    #[default]
    Local,
    Base,
    Remote,
}

/// Core 依据 local/base/remote 三方内容摘要计算的同步状态。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AssetSyncState {
    Synced,
    LocalModified,
    RemoteUpdated,
    Diverged,
    Conflict,
    UpstreamRemoved,
    Incompatible,
    Revoked,
    RemoteUnknown,
}

/// UI 只能渲染 Core 返回的语义动作，不得自行推导危险操作。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AssetAction {
    View,
    Edit,
    Configure,
    Validate,
    TryRun,
    Activate,
    Deactivate,
    Install,
    Uninstall,
    Sync,
    Publish,
    ViewDiff,
    ResolveConflict,
    Detach,
    Restore,
}

/// UI 与 Core 在进入资产协作功能前必须完成的协议握手。
///
/// 此协议与普通 API 版本独立演进。客户端必须同时校验 `protocol_version`、
/// `build_identifier` 和其自身所需的能力；任一项不匹配时不得尝试旧的资产或发布流程。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetCollaborationProtocolResponse {
    pub protocol_version: String,
    /// 可复现的 Core 构建标识。当前使用发布版本，供 UI 与其固定的 Core 版本比对。
    pub build_identifier: String,
    pub capabilities: Vec<AssetCollaborationCapability>,
}

/// Asset-collaboration protocol v1 的显式能力声明。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum AssetCollaborationCapability {
    LocalAssetCatalogV1,
    RemoteMarketV2,
    HubPullRequestPublishV1,
    RuntimeAssetReceiptV1,
    TypedAssetRuntimeV1,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetUpstreamResponse {
    pub package_name: String,
    pub remote_asset_id: String,
    pub version: String,
    pub source_revision: String,
    pub remote_digest: String,
    pub tracking_mode: AssetTrackingMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetSummaryResponse {
    pub id: String,
    pub kind: AssetKind,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub origin: AssetOrigin,
    pub trust: AssetTrust,
    pub scope: AssetScope,
    pub editability: AssetEditability,
    pub definition_digest: String,
    pub runtime_state: crate::AssetRuntimeState,
    /// 仅在资产仍跟踪 Hub 上游时存在。本地原创或已解除跟踪的资产没有同步状态。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_state: Option<AssetSyncState>,
    pub allowed_actions: Vec<AssetAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<AssetUpstreamResponse>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetFileEntryResponse {
    pub path: String,
    pub digest: String,
    pub size: u64,
    pub media_type: String,
    pub text: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetDetailResponse {
    #[serde(flatten)]
    pub asset: AssetSummaryResponse,
    pub files: Vec<AssetFileEntryResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_file: Option<String>,
    pub content_source: AssetContentSource,
    pub source_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_binding: Option<crate::AssetRuntimeBindingResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetFileResponse {
    pub asset_id: String,
    pub path: String,
    pub digest: String,
    pub media_type: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetDiffFileResponse {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<AssetFileEntryResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<AssetFileEntryResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<AssetFileEntryResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_digest: Option<String>,
    pub status: AssetDiffFileStatus,
    pub auto_mergeable: bool,
}

/// 单个 Definition 文件相对于最近 Base 的三方状态。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AssetDiffFileStatus {
    Unchanged,
    LocalAdded,
    LocalModified,
    LocalDeleted,
    RemoteAdded,
    RemoteModified,
    RemoteDeleted,
    /// 本地和远程进行了相同修改。
    Converged,
    /// 两侧修改不同，但可安全自动合并。
    Diverged,
    /// 二进制、删除/修改或同一区域修改，必须由用户选择。
    Conflict,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetDiffResponse {
    pub asset_id: String,
    pub sync_state: AssetSyncState,
    pub local_digest: String,
    pub base_digest: String,
    pub remote_digest: String,
    pub files: Vec<AssetDiffFileResponse>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AssetResolveStrategy {
    AutoMerge,
    KeepLocal,
    UseRemote,
    Detach,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolveAssetRequest {
    pub strategy: AssetResolveStrategy,
    pub expected_local_digest: String,
    pub expected_base_digest: String,
    pub expected_remote_digest: String,
    pub idempotency_key: String,
    /// “采用远程”会替换本地 Definition，Core 也必须验证显式确认。
    #[serde(default)]
    pub confirm_destructive: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RestoreAssetRequest {
    pub recovery_operation_id: String,
    pub expected_local_digest: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetResolveResponse {
    pub asset: AssetSummaryResponse,
    pub operation: AssetOperationResponse,
    pub strategy: AssetResolveStrategy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetRestoreResponse {
    pub asset: AssetSummaryResponse,
    pub operation: AssetOperationResponse,
    pub recovered_digest: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListAssetsQuery {
    #[serde(default)]
    pub kind: Option<AssetKind>,
    #[serde(default)]
    pub scope: Option<AssetScope>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GetAssetQuery {
    #[serde(default)]
    pub source: AssetContentSource,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadAssetFileQuery {
    pub path: String,
    #[serde(default)]
    pub source: AssetContentSource,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WriteAssetFileRequest {
    pub path: String,
    pub content: String,
    /// 乐观并发控制：必须等于写入前文件摘要。
    pub expected_digest: String,
}

/// 从 Core 内置的安全模板创建一个全新的本地 Definition。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateAssetRequest {
    pub id: String,
    pub kind: AssetKind,
    pub display_name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// 省略时使用资产 ID。Core 仍会执行独立的 runtimeId 安全校验。
    #[serde(default)]
    pub runtime_id: Option<String>,
}

/// 将可见 Definition 复制为独立的本地资产。
///
/// 复制只继承 Definition 内容，不继承 Hub 上游、Base、Overlay、凭据、
/// 试跑回执或运行 binding。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DuplicateAssetRequest {
    pub id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub runtime_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetOperationRequest {
    /// 客户端重试必须复用同一个键。
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AssetOperationKind {
    Install,
    Configure,
    Validate,
    TryRun,
    Activate,
    Deactivate,
    Uninstall,
    Sync,
    Resolve,
    Detach,
    Restore,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AssetOperationState {
    Queued,
    Running,
    Succeeded,
    Failed,
    RolledBack,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetOperationResponse {
    pub operation_id: String,
    pub idempotency_key: String,
    pub asset_id: String,
    pub kind: AssetOperationKind,
    pub state: AssetOperationState,
    pub phase: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    pub started_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_contract_is_strict_and_camel_case() {
        let value = serde_json::json!({
            "id": "skill:frontend-design",
            "kind": "skill",
            "displayName": "前端设计",
            "origin": "hub",
            "trust": "official",
            "scope": "user",
            "editability": "full",
            "definitionDigest": format!("sha256-{}", "a".repeat(64)),
            "runtimeState": "inactive",
            "syncState": "localModified",
            "allowedActions": ["view", "edit", "publish", "viewDiff"],
            "createdAt": 1,
            "updatedAt": 2
        });
        let asset: AssetSummaryResponse = serde_json::from_value(value).unwrap();
        assert_eq!(asset.kind, AssetKind::Skill);
        assert_eq!(asset.sync_state, Some(AssetSyncState::LocalModified));
        assert!(asset.allowed_actions.contains(&AssetAction::ViewDiff));
    }

    #[test]
    fn local_asset_has_no_fake_sync_state() {
        let value = serde_json::json!({
            "id": "skill:local",
            "kind": "skill",
            "displayName": "本地技能",
            "origin": "local",
            "trust": "community",
            "scope": "user",
            "editability": "full",
            "definitionDigest": format!("sha256-{}", "a".repeat(64)),
            "runtimeState": "inactive",
            "allowedActions": ["view", "edit", "publish"],
            "createdAt": 1,
            "updatedAt": 2
        });
        let asset: AssetSummaryResponse = serde_json::from_value(value).unwrap();
        assert_eq!(asset.sync_state, None);
        assert!(serde_json::from_str::<AssetSyncState>(r#""localOnly""#).is_err());
        assert!(serde_json::from_str::<AssetSyncState>(r#""remoteOnly""#).is_err());
        assert!(serde_json::from_str::<AssetSyncState>(r#""failed""#).is_err());
        assert!(serde_json::from_str::<AssetOrigin>(r#""legacy""#).is_err());
        assert!(
            serde_json::from_str::<AssetScope>(r#""workspace""#).is_err(),
            "V1 没有稳定工作区身份与授权契约，不能暴露伪 workspace scope"
        );
    }

    #[test]
    fn asset_contract_rejects_unknown_fields() {
        let value = serde_json::json!({
            "path": "SKILL.md",
            "content": "# Demo",
            "expectedDigest": "sha256-old",
            "absolutePath": "C:/Users/example"
        });
        assert!(serde_json::from_value::<WriteAssetFileRequest>(value).is_err());
    }

    #[test]
    fn asset_collaboration_protocol_contract_is_strict_and_explicit() {
        let value = serde_json::json!({
            "protocolVersion": "1.0.0",
            "buildIdentifier": "0.2.0",
            "capabilities": [
                "localAssetCatalogV1",
                "remoteMarketV2",
                "hubPullRequestPublishV1",
                "runtimeAssetReceiptV1",
                "typedAssetRuntimeV1"
            ]
        });
        let protocol: AssetCollaborationProtocolResponse = serde_json::from_value(value).unwrap();
        assert_eq!(protocol.protocol_version, "1.0.0");
        assert_eq!(protocol.build_identifier, "0.2.0");
        assert_eq!(
            protocol.capabilities,
            vec![
                AssetCollaborationCapability::LocalAssetCatalogV1,
                AssetCollaborationCapability::RemoteMarketV2,
                AssetCollaborationCapability::HubPullRequestPublishV1,
                AssetCollaborationCapability::RuntimeAssetReceiptV1,
                AssetCollaborationCapability::TypedAssetRuntimeV1,
            ]
        );

        assert!(
            serde_json::from_value::<AssetCollaborationProtocolResponse>(serde_json::json!({
                "protocolVersion": "1.0.0",
                "buildIdentifier": "0.2.0",
                "capabilities": [],
                "legacyGithubFallback": true
            }))
            .is_err()
        );

        assert!(
            serde_json::from_value::<AssetCollaborationProtocolResponse>(serde_json::json!({
                "protocolVersion": "1.0.0",
                "capabilities": []
            }))
            .is_err(),
            "构建标识是握手必填项，旧 Core 不得被误判为兼容"
        );
    }
}
