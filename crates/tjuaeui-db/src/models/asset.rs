use serde::{Deserialize, Serialize};
use tjuaeui_common::TimestampMs;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, sqlx::FromRow)]
pub struct AssetRecordRow {
    pub user_id: String,
    pub id: String,
    pub kind: String,
    pub display_name: String,
    pub description: Option<String>,
    pub origin: String,
    pub trust: String,
    pub scope: String,
    pub editability: String,
    pub workspace_key: String,
    pub definition_digest: String,
    pub entry_file: Option<String>,
    pub runtime_id: Option<String>,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, sqlx::FromRow)]
pub struct AssetUpstreamRow {
    pub user_id: String,
    pub asset_id: String,
    pub package_name: String,
    pub remote_asset_id: String,
    pub version: String,
    pub source_revision: String,
    pub remote_digest: String,
    pub tracking_mode: String,
    pub checked_at: Option<TimestampMs>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, sqlx::FromRow)]
pub struct AssetSnapshotRow {
    pub user_id: String,
    pub asset_id: String,
    pub base_digest: String,
    pub object_key: String,
    pub manifest_json: String,
    pub created_at: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, sqlx::FromRow)]
pub struct AssetOperationRow {
    pub user_id: String,
    pub operation_id: String,
    pub idempotency_key: String,
    pub asset_id: String,
    pub kind: String,
    pub state: String,
    pub phase: String,
    pub error_code: Option<String>,
    pub recovery_json: String,
    pub started_at: TimestampMs,
    pub finished_at: Option<TimestampMs>,
    pub updated_at: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, sqlx::FromRow)]
pub struct AssetRuntimeStateRow {
    pub user_id: String,
    pub asset_owner_id: String,
    pub asset_id: String,
    pub state: String,
    pub last_error_code: Option<String>,
    pub updated_at: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, sqlx::FromRow)]
pub struct AssetOverlayRow {
    pub user_id: String,
    pub asset_owner_id: String,
    pub asset_id: String,
    pub kind: String,
    pub overlay_json: String,
    pub version: i64,
    pub updated_at: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, sqlx::FromRow)]
pub struct AssetRuntimeBindingRow {
    pub user_id: String,
    pub asset_owner_id: String,
    pub asset_id: String,
    pub kind: String,
    pub projection_kind: String,
    pub portable_runtime_id: String,
    pub projection_runtime_id: String,
    pub definition_digest: String,
    pub overlay_version: i64,
    pub health_status: String,
    pub try_run_receipt_id: Option<String>,
    pub last_error_code: Option<String>,
    pub projected_at: TimestampMs,
    pub health_checked_at: Option<TimestampMs>,
}

/// 数据库中的资产凭据密文。该类型不实现 `Debug` 或 `Serialize`，
/// 避免诊断输出意外暴露可离线攻击的密文。
#[derive(Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct AssetCredentialRow {
    pub user_id: String,
    pub asset_owner_id: String,
    pub asset_id: String,
    pub slot: String,
    pub ciphertext: String,
    pub key_version: i64,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, sqlx::FromRow)]
pub struct AssetTryRunReceiptRow {
    pub user_id: String,
    pub asset_owner_id: String,
    pub asset_id: String,
    pub receipt_id: String,
    pub idempotency_key: String,
    pub definition_digest: String,
    pub overlay_version: i64,
    pub portable_runtime_id: String,
    pub projection_runtime_id: String,
    pub created_at: TimestampMs,
}
