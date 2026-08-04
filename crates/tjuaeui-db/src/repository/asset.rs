use crate::{
    DbError,
    models::{
        AssetCredentialRow, AssetOperationRow, AssetOverlayRow, AssetRecordRow, AssetRuntimeBindingRow,
        AssetRuntimeStateRow, AssetSnapshotRow, AssetTryRunReceiptRow, AssetUpstreamRow,
    },
};

#[derive(Debug, Clone)]
pub struct UpsertAssetRecordParams<'a> {
    pub user_id: &'a str,
    pub id: &'a str,
    pub kind: &'a str,
    pub display_name: &'a str,
    pub description: Option<&'a str>,
    pub origin: &'a str,
    pub trust: &'a str,
    pub scope: &'a str,
    pub editability: &'a str,
    pub workspace_key: &'a str,
    pub definition_digest: &'a str,
    pub entry_file: Option<&'a str>,
    pub runtime_id: Option<&'a str>,
    pub now: i64,
}

#[derive(Debug, Clone)]
pub struct UpsertAssetUpstreamParams<'a> {
    pub user_id: &'a str,
    pub asset_id: &'a str,
    pub package_name: &'a str,
    pub remote_asset_id: &'a str,
    pub version: &'a str,
    pub source_revision: &'a str,
    pub remote_digest: &'a str,
    pub tracking_mode: &'a str,
    pub checked_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct CreateAssetSnapshotParams<'a> {
    pub user_id: &'a str,
    pub asset_id: &'a str,
    pub base_digest: &'a str,
    pub object_key: &'a str,
    pub manifest_json: &'a str,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct CommitTrackedAssetParams<'a> {
    pub record: UpsertAssetRecordParams<'a>,
    pub upstream: UpsertAssetUpstreamParams<'a>,
    pub snapshot: CreateAssetSnapshotParams<'a>,
}

#[derive(Debug, Clone)]
pub struct CommitResolvedAssetParams<'a> {
    pub record: UpsertAssetRecordParams<'a>,
    pub upstream: UpsertAssetUpstreamParams<'a>,
    /// The new Base is the verified remote Definition. For keep-local and
    /// auto-merge the record digest intentionally differs from this digest.
    pub snapshot: CreateAssetSnapshotParams<'a>,
    pub operation_id: &'a str,
    pub recovery_json: &'a str,
    pub finished_at: i64,
}

#[derive(Debug, Clone)]
pub struct StartAssetOperationParams<'a> {
    pub user_id: &'a str,
    pub operation_id: &'a str,
    pub idempotency_key: &'a str,
    pub asset_id: &'a str,
    pub kind: &'a str,
    pub phase: &'a str,
    pub recovery_json: &'a str,
    pub started_at: i64,
}

#[derive(Debug, Clone)]
pub struct UpdateAssetOperationParams<'a> {
    pub state: &'a str,
    pub phase: &'a str,
    pub error_code: Option<&'a str>,
    pub recovery_json: &'a str,
    pub finished_at: Option<i64>,
    pub updated_at: i64,
}

#[derive(Clone)]
pub struct ConfigureAssetOverlayParams<'a> {
    pub user_id: &'a str,
    pub asset_id: &'a str,
    pub kind: &'a str,
    pub overlay_json: &'a str,
    pub expected_version: Option<i64>,
    /// 已由领域服务加密的凭据变更；数据库层从不接收明文。
    pub secret_updates: &'a [EncryptedAssetSecretUpdate<'a>],
    pub now: i64,
}

/// 单个凭据槽的密文更新。刻意不实现 `Debug`，避免密文进入日志。
pub enum EncryptedAssetSecretUpdate<'a> {
    Set {
        slot: &'a str,
        ciphertext: &'a str,
        key_version: i64,
    },
    Clear {
        slot: &'a str,
    },
}

#[derive(Debug, Clone)]
pub struct CreateAssetTryRunReceiptParams<'a> {
    pub user_id: &'a str,
    pub asset_id: &'a str,
    pub receipt_id: &'a str,
    pub idempotency_key: &'a str,
    pub definition_digest: &'a str,
    pub overlay_version: i64,
    pub portable_runtime_id: &'a str,
    pub projection_runtime_id: &'a str,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct SetAssetRuntimeStateParams<'a> {
    pub user_id: &'a str,
    pub asset_id: &'a str,
    pub state: &'a str,
    pub last_error_code: Option<&'a str>,
    pub now: i64,
}

#[derive(Debug, Clone)]
pub struct CommitAssetRuntimeBindingParams<'a> {
    pub user_id: &'a str,
    pub asset_id: &'a str,
    pub kind: &'a str,
    pub projection_kind: &'a str,
    pub portable_runtime_id: &'a str,
    pub projection_runtime_id: &'a str,
    pub definition_digest: &'a str,
    pub overlay_version: i64,
    pub try_run_receipt_id: &'a str,
    pub health_status: &'a str,
    pub last_error_code: Option<&'a str>,
    pub projected_at: i64,
    pub health_checked_at: Option<i64>,
}

/// Metadata persistence boundary for the Core-managed local asset repository.
#[async_trait::async_trait]
pub trait IAssetRepository: Send + Sync {
    /// Lists the user's assets plus read-only system-scoped seed assets.
    async fn list(&self, user_id: &str, kind: Option<&str>) -> Result<Vec<AssetRecordRow>, DbError>;
    async fn get(&self, user_id: &str, asset_id: &str) -> Result<Option<AssetRecordRow>, DbError>;
    async fn upsert_record(&self, params: UpsertAssetRecordParams<'_>) -> Result<AssetRecordRow, DbError>;
    /// Atomically persists a tracked asset, its upstream and the complete base
    /// snapshot reference. Install/sync code must use this instead of partial
    /// writes.
    async fn commit_tracked_asset(
        &self,
        record: UpsertAssetRecordParams<'_>,
        upstream: UpsertAssetUpstreamParams<'_>,
        snapshot: CreateAssetSnapshotParams<'_>,
    ) -> Result<AssetRecordRow, DbError>;
    /// Atomically persists every member of an explicit Bundle.
    async fn commit_tracked_assets(
        &self,
        assets: &[CommitTrackedAssetParams<'_>],
    ) -> Result<Vec<AssetRecordRow>, DbError>;
    /// Atomically advances upstream/Base, stores the resolved local digest and
    /// completes the resolution operation.
    async fn commit_resolved_asset(&self, params: CommitResolvedAssetParams<'_>) -> Result<AssetOperationRow, DbError>;
    /// Atomically detaches tracking and completes the resolution operation.
    async fn commit_detach_resolution(
        &self,
        user_id: &str,
        asset_ids: &[String],
        operation_asset_id: &str,
        operation_id: &str,
        finished_at: i64,
    ) -> Result<AssetOperationRow, DbError>;
    /// Atomically converts every member of a tracked Bundle into an
    /// independent local asset. Definition/runtime content is intentionally
    /// outside this metadata-only transaction and must remain untouched.
    async fn detach_assets(
        &self,
        user_id: &str,
        asset_ids: &[String],
        updated_at: i64,
    ) -> Result<Vec<AssetRecordRow>, DbError>;
    /// Atomically restores a recovery object as the editable local Definition
    /// while retaining the current remote/Base relation.
    async fn commit_restored_asset(
        &self,
        record: UpsertAssetRecordParams<'_>,
        operation_id: &str,
        finished_at: i64,
    ) -> Result<AssetOperationRow, DbError>;
    /// Atomically removes the asset metadata and marks its uninstall
    /// operation complete. Definition files are guarded separately by the
    /// content-store rollback handle.
    async fn commit_uninstall(
        &self,
        user_id: &str,
        asset_id: &str,
        operation_id: &str,
        finished_at: i64,
    ) -> Result<AssetOperationRow, DbError>;
    /// Atomically removes every member of an explicit Bundle and completes
    /// the single package-level operation.
    async fn commit_uninstall_assets(
        &self,
        user_id: &str,
        asset_ids: &[String],
        operation_id: &str,
        operation_asset_id: &str,
        finished_at: i64,
    ) -> Result<AssetOperationRow, DbError>;
    async fn delete(&self, user_id: &str, asset_id: &str) -> Result<bool, DbError>;

    async fn get_upstream(&self, user_id: &str, asset_id: &str) -> Result<Option<AssetUpstreamRow>, DbError>;
    async fn upsert_upstream(&self, params: UpsertAssetUpstreamParams<'_>) -> Result<AssetUpstreamRow, DbError>;

    async fn create_snapshot(&self, params: CreateAssetSnapshotParams<'_>) -> Result<AssetSnapshotRow, DbError>;
    async fn latest_snapshot(&self, user_id: &str, asset_id: &str) -> Result<Option<AssetSnapshotRow>, DbError>;

    async fn start_operation(&self, params: StartAssetOperationParams<'_>) -> Result<AssetOperationRow, DbError>;
    async fn get_operation_by_idempotency(
        &self,
        user_id: &str,
        idempotency_key: &str,
    ) -> Result<Option<AssetOperationRow>, DbError>;
    async fn get_operation(&self, user_id: &str, operation_id: &str) -> Result<Option<AssetOperationRow>, DbError>;
    async fn update_operation(
        &self,
        user_id: &str,
        operation_id: &str,
        params: UpdateAssetOperationParams<'_>,
    ) -> Result<Option<AssetOperationRow>, DbError>;
    async fn list_recoverable_operations(&self) -> Result<Vec<AssetOperationRow>, DbError>;

    /// 返回当前用户针对该本地 Definition 的独立运行状态。
    async fn get_runtime_state(&self, user_id: &str, asset_id: &str) -> Result<Option<AssetRuntimeStateRow>, DbError>;
    /// 读取当前用户的私有 Overlay；system seed 的 Overlay 仍归当前用户所有。
    async fn get_overlay(&self, user_id: &str, asset_id: &str) -> Result<Option<AssetOverlayRow>, DbError>;
    /// 原子写入类型化 Overlay、设置/清除密文凭据并使现有投影进入
    /// `needsRepair`；乐观版本冲突不得部分写入。
    async fn configure_overlay(&self, params: ConfigureAssetOverlayParams<'_>) -> Result<AssetOverlayRow, DbError>;
    async fn list_credentials(&self, user_id: &str, asset_id: &str) -> Result<Vec<AssetCredentialRow>, DbError>;
    async fn get_try_run_receipt(
        &self,
        user_id: &str,
        asset_id: &str,
    ) -> Result<Option<AssetTryRunReceiptRow>, DbError>;
    async fn get_try_run_receipt_by_idempotency(
        &self,
        user_id: &str,
        idempotency_key: &str,
    ) -> Result<Option<AssetTryRunReceiptRow>, DbError>;
    /// 只有 Definition 摘要和 Overlay 版本仍匹配时才提交成功试跑回执。
    async fn commit_try_run_receipt(
        &self,
        params: CreateAssetTryRunReceiptParams<'_>,
    ) -> Result<AssetTryRunReceiptRow, DbError>;
    async fn set_runtime_state(&self, params: SetAssetRuntimeStateParams<'_>) -> Result<AssetRuntimeStateRow, DbError>;
    async fn get_runtime_binding(
        &self,
        user_id: &str,
        asset_id: &str,
    ) -> Result<Option<AssetRuntimeBindingRow>, DbError>;
    /// 列出当前用户自己的运行绑定；调用方仍必须与 runtime state、Definition
    /// 摘要和派生 projection ID 交叉校验后才能交给实际运行时。
    async fn list_runtime_bindings(
        &self,
        user_id: &str,
        kind: Option<&str>,
    ) -> Result<Vec<AssetRuntimeBindingRow>, DbError>;
    /// 原子校验当前试跑回执并提交 binding 与 `active` 状态；
    /// Definition/Overlay/回执任一不匹配时整体回滚。
    async fn commit_runtime_binding(
        &self,
        params: CommitAssetRuntimeBindingParams<'_>,
    ) -> Result<AssetRuntimeBindingRow, DbError>;
    /// 原子删除 binding，并根据 Overlay 是否存在回到 `inactive` 或 `notConfigured`。
    async fn deactivate_runtime(
        &self,
        user_id: &str,
        asset_id: &str,
        now: i64,
    ) -> Result<AssetRuntimeStateRow, DbError>;
}
