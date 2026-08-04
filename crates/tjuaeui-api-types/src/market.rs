use serde::{Deserialize, Serialize};

use crate::{AssetAction, AssetKind, AssetSyncState, AssetTrust};

/// 市场资产相对于当前用户本地资产库的存在性。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum MarketPresenceState {
    NotInstalled,
    Installed,
}

/// Hub 中资产的发布生命周期。撤销状态由仓库维护者策略控制。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum MarketAssetStatus {
    Active,
    Deprecated,
    Revoked,
}

/// Hub 包级审核结论。Core 只接受 approved 包进入可安装索引。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum MarketPackageReviewStatus {
    Approved,
    UnderReview,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarketCompatibilityResponse {
    pub compatible: bool,
    pub tjuae: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarketAssetFileResponse {
    pub path: String,
    pub digest: String,
    pub size: u64,
    pub media_type: String,
}

/// Hub 索引中的资产级描述；trust 由 Hub 构建器生成，不能由清单作者提交。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarketAssetDescriptor {
    pub id: String,
    pub kind: AssetKind,
    pub runtime_id: String,
    pub dependencies: Vec<String>,
    pub display_name: String,
    pub description: String,
    pub version: String,
    pub definition_digest: String,
    pub entry_file: String,
    pub package_name: String,
    pub author: String,
    pub license: String,
    pub trust: AssetTrust,
    pub status: MarketAssetStatus,
    pub compatibility: MarketCompatibilityResponse,
    pub source_revision: String,
    pub files: Vec<MarketAssetFileResponse>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarketPackageDescriptor {
    pub name: String,
    pub version: String,
    pub review_status: MarketPackageReviewStatus,
    pub atomic: bool,
    pub asset_ids: Vec<String>,
    pub dependencies: std::collections::BTreeMap<String, String>,
    pub tarball: String,
    pub integrity: String,
    pub archive_integrity: String,
    pub unpacked_size: u64,
    pub repository: String,
    pub source_path: String,
    pub manifest_path: String,
    pub source_revision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarketLocalRelationResponse {
    pub local_asset_id: String,
    pub local_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarketAssetResponse {
    #[serde(flatten)]
    pub asset: MarketAssetDescriptor,
    pub presence_state: MarketPresenceState,
    /// 只有已安装且仍跟踪 Hub 的资产才有同步状态。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_state: Option<AssetSyncState>,
    pub allowed_actions: Vec<AssetAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<MarketLocalRelationResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarketCacheResponse {
    /// TjuaeHub `dist` 分支中承载 index/ZIP 的不可变发布提交。
    /// 它与资产源码的 `source_revision` 是两个独立的 provenance 维度。
    pub distribution_revision: Option<String>,
    pub cached_at: i64,
    pub source_url: String,
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarketIndexResponse {
    pub schema_version: u32,
    pub generated_at: String,
    pub assets: Vec<MarketAssetResponse>,
    pub packages: Vec<MarketPackageDescriptor>,
    pub cache: MarketCacheResponse,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListMarketAssetsQuery {
    #[serde(default)]
    pub kind: Option<AssetKind>,
    #[serde(default)]
    pub search: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallMarketAssetRequest {
    pub remote_asset_id: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadMarketAssetFileQuery {
    pub remote_asset_id: String,
    pub path: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RefreshMarketRequest {
    /// 仅供可复现诊断或回滚使用；省略时解析 Hub dist 分支的最新提交。
    #[serde(default)]
    pub distribution_revision: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn market_asset_keeps_package_and_asset_identity_separate() {
        let descriptor = MarketAssetDescriptor {
            id: "org.tjuae.skill.frontend-design".into(),
            kind: AssetKind::Skill,
            runtime_id: "frontend-design".into(),
            dependencies: Vec::new(),
            display_name: "前端设计".into(),
            description: "demo".into(),
            version: "1.0.0".into(),
            definition_digest: format!("sha256-{}", "a".repeat(64)),
            entry_file: "skills/frontend-design/SKILL.md".into(),
            package_name: "tjuaeasset-frontend-design".into(),
            author: "Tjuae".into(),
            license: "Apache-2.0".into(),
            trust: AssetTrust::Official,
            status: MarketAssetStatus::Active,
            compatibility: MarketCompatibilityResponse {
                compatible: true,
                tjuae: "^1.0.0".into(),
                reason_code: None,
            },
            source_revision: "b".repeat(40),
            files: Vec::new(),
            tags: Vec::new(),
        };
        let json = serde_json::to_value(descriptor).unwrap();
        assert_eq!(json["id"], "org.tjuae.skill.frontend-design");
        assert_eq!(json["packageName"], "tjuaeasset-frontend-design");
        assert_eq!(json["trust"], "official");
        assert_eq!(json["status"], "active");
    }

    #[test]
    fn market_presence_is_separate_from_sync_state() {
        let json = serde_json::json!({
            "id": "org.tjuae.skill.demo",
            "kind": "skill",
            "runtimeId": "demo",
            "dependencies": [],
            "displayName": "演示",
            "description": "demo",
            "version": "1.0.0",
            "definitionDigest": format!("sha256-{}", "a".repeat(64)),
            "entryFile": "skills/demo/SKILL.md",
            "packageName": "tjuaeasset-demo",
            "author": "Tjuae",
            "license": "Apache-2.0",
            "trust": "official",
            "status": "active",
            "compatibility": {"compatible": true, "tjuae": "^1.0.0"},
            "sourceRevision": "b".repeat(40),
            "files": [],
            "tags": [],
            "presenceState": "notInstalled",
            "allowedActions": ["view", "install"]
        });
        let asset: MarketAssetResponse = serde_json::from_value(json).unwrap();
        assert_eq!(asset.presence_state, MarketPresenceState::NotInstalled);
        assert_eq!(asset.sync_state, None);
    }

    #[test]
    fn distribution_and_source_revisions_are_distinct_contract_fields() {
        let distribution_revision = "d".repeat(40);
        let request: RefreshMarketRequest = serde_json::from_value(serde_json::json!({
            "distributionRevision": distribution_revision.clone()
        }))
        .unwrap();
        assert_eq!(
            request.distribution_revision.as_deref(),
            Some(distribution_revision.as_str())
        );
        assert!(
            serde_json::from_value::<RefreshMarketRequest>(serde_json::json!({
                "commitSha": "d".repeat(40)
            }))
            .is_err()
        );
    }
}
