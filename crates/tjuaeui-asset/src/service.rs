use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tjuaeui_api_types::{
    AssetAction, AssetConfigurationSchemaDefinition, AssetConfigurationValue, AssetConfigurationValueType,
    AssetContentSource, AssetDetailResponse, AssetDiffResponse, AssetEditability, AssetFileResponse, AssetKind,
    AssetOperationKind, AssetOperationResponse, AssetOperationState, AssetOrigin, AssetOverlayResponse,
    AssetPrimitiveValue, AssetPublicConfiguration, AssetResolveResponse, AssetResolveStrategy, AssetRestoreResponse,
    AssetRuntimeBindingResponse, AssetRuntimeCommandRequest, AssetRuntimeHealthStatus, AssetRuntimeProjectionKind,
    AssetRuntimeState, AssetRuntimeStatusResponse, AssetScope, AssetSecretSlotResponse, AssetSecretUpdate,
    AssetSummaryResponse, AssetSyncState, AssetTrackingMode, AssetTrust, AssetUpstreamResponse, ConfigureAssetRequest,
    CreateAssetRequest, DuplicateAssetRequest, EngineAdapterAssetConfiguration, MarketLocalRelationResponse,
    McpAssetConfiguration, McpAssetTransport, ResolveAssetRequest, RestoreAssetRequest,
};
use tjuaeui_common::{decrypt_string, encrypt_string, now_ms};
use tjuaeui_db::{
    CommitAssetRuntimeBindingParams, CommitResolvedAssetParams, CommitTrackedAssetParams, ConfigureAssetOverlayParams,
    CreateAssetSnapshotParams, CreateAssetTryRunReceiptParams, EncryptedAssetSecretUpdate, IAssetRepository,
    SetAssetRuntimeStateParams, StartAssetOperationParams, UpdateAssetOperationParams, UpsertAssetRecordParams,
    UpsertAssetUpstreamParams,
    models::{
        AssetCredentialRow, AssetOperationRow, AssetOverlayRow, AssetRecordRow, AssetRuntimeBindingRow,
        AssetSnapshotRow, AssetUpstreamRow,
    },
};
use uuid::Uuid;

use crate::AssetError;
use crate::definition::{
    AssetDefinitionFile, MAX_DEFINITION_FILE_BYTES, ScannedDefinition, digest_bytes, join_safe, load_definition,
    normalize_relative_path, prepare_definition, scan_definition, validate_entry_file,
};
use crate::store::AssetContentStore;
use crate::typed_definition::validate_typed_definition;
use crate::{
    AssetRuntimeProjector, FailClosedRuntimeProjector, RuntimeAssetConfigurationResolver, RuntimeAssetDefinition,
    RuntimeProjectionTransaction, RuntimeResolvedConfiguration, derive_projection_runtime_id,
};

const ASSET_CREDENTIAL_KEY_VERSION: i64 = 1;
const ASSET_CREDENTIAL_MASK: &str = "••••••";

/// 已加密、尚未借用为数据库参数的凭据变更。
/// 刻意不实现 `Debug`，避免密文进入诊断输出。
struct PreparedSecretUpdate {
    slot: String,
    ciphertext: Option<String>,
    key_version: i64,
}

#[derive(Debug, Clone)]
pub struct LocalAssetInput {
    pub id: String,
    pub kind: AssetKind,
    pub display_name: String,
    pub description: Option<String>,
    pub origin: AssetOrigin,
    pub trust: AssetTrust,
    pub scope: AssetScope,
    pub editability: AssetEditability,
    pub entry_file: Option<String>,
    pub runtime_id: Option<String>,
    pub files: Vec<AssetDefinitionFile>,
    pub dependency_runtime_ids: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct TrackedAssetInput {
    pub local: LocalAssetInput,
    pub package_name: String,
    pub remote_asset_id: String,
    pub version: String,
    pub source_revision: String,
    pub remote_digest: String,
}

/// Strict, read-only provenance for one locally runnable asset.
///
/// The catalog identity and Definition digest come from the user-visible local
/// repository. Hub fields are present only while the local asset is actively
/// tracking an upstream; detached and local-only assets never expose stale
/// upstream metadata through this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAssetProvenance {
    pub local_asset_id: String,
    pub kind: AssetKind,
    pub local_definition_digest: String,
    pub runtime_id: String,
    pub upstream_package: Option<String>,
    pub upstream_asset_id: Option<String>,
    pub upstream_version: Option<String>,
    pub upstream_revision: Option<String>,
}

/// 当前用户已激活资产的精确运行来源。
///
/// `workspace_path` 只供 Core 内部运行时接线使用；资产身份仍由
/// `provenance.local_asset_id` 与 Definition 摘要确定，调用方不得从目录名
/// 反推资产身份。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundRuntimeAsset {
    pub provenance: RuntimeAssetProvenance,
    /// Core-only identity for exact legacy projection reads.
    pub projection_runtime_id: String,
    pub overlay_version: i64,
    pub entry_file: String,
    pub workspace_path: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct TrackedAssetReference {
    pub package_name: String,
    pub remote_asset_id: String,
    pub version: String,
    pub remote_digest: String,
}

struct PreparedTrackedAsset {
    input: TrackedAssetInput,
    files: Vec<AssetDefinitionFile>,
    scanned: ScannedDefinition,
    object_key: String,
    workspace_key: String,
    manifest_json: String,
}

struct LoadedThreeWay {
    record: AssetRecordRow,
    local_files: Vec<AssetDefinitionFile>,
    local: ScannedDefinition,
    base_files: Vec<AssetDefinitionFile>,
    base_scanned: ScannedDefinition,
    remote_files: Vec<AssetDefinitionFile>,
    remote: ScannedDefinition,
}

pub(crate) struct MarketLocalRelation {
    pub local: MarketLocalRelationResponse,
    pub sync_state: AssetSyncState,
    pub allowed_actions: Vec<AssetAction>,
}

struct RuntimeView {
    state: AssetRuntimeState,
    binding: Option<AssetRuntimeBindingResponse>,
    overlay_version: Option<i64>,
    has_current_try_run_receipt: bool,
}

struct AssetDetailParts {
    record: AssetRecordRow,
    upstream: Option<AssetUpstreamRow>,
    base: Option<AssetSnapshotRow>,
    local_digest: String,
    content_source: AssetContentSource,
    scanned: ScannedDefinition,
}

struct NoopRuntimeProjectionTransaction;

#[async_trait::async_trait]
impl RuntimeProjectionTransaction for NoopRuntimeProjectionTransaction {
    async fn apply(&mut self) -> Result<(), AssetError> {
        Ok(())
    }

    async fn rollback(&mut self) -> Result<(), AssetError> {
        Ok(())
    }

    async fn finalize(self: Box<Self>) {}
}

#[derive(Clone)]
pub struct AssetCatalogService {
    repo: Arc<dyn IAssetRepository>,
    store: AssetContentStore,
    runtime_projector: Arc<dyn AssetRuntimeProjector>,
    credential_master_key: Option<[u8; 32]>,
}

impl AssetCatalogService {
    pub fn new(repo: Arc<dyn IAssetRepository>, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            repo,
            store: AssetContentStore::new(data_dir),
            runtime_projector: Arc::new(FailClosedRuntimeProjector),
            credential_master_key: None,
        }
    }

    pub fn with_runtime_projector(mut self, runtime_projector: Arc<dyn AssetRuntimeProjector>) -> Self {
        self.runtime_projector = runtime_projector;
        self
    }

    /// 注入应用级主密钥。实际加解密会再按 user/asset/slot/version 派生，
    /// 不会直接复用该主密钥。
    pub fn with_credential_encryption_key(mut self, key: [u8; 32]) -> Self {
        self.credential_master_key = Some(key);
        self
    }

    pub fn content_store(&self) -> &AssetContentStore {
        &self.store
    }

    pub async fn get_overlay(&self, user_id: &str, asset_id: &str) -> Result<AssetOverlayResponse, AssetError> {
        let record = self
            .repo
            .get(user_id, asset_id)
            .await?
            .ok_or_else(|| AssetError::NotFound(asset_id.into()))?;
        let row = self
            .repo
            .get_overlay(user_id, asset_id)
            .await?
            .ok_or(AssetError::OverlayNotConfigured)?;
        let credentials = self.repo.list_credentials(user_id, asset_id).await?;
        overlay_response(&record, row, &credentials)
    }

    pub async fn configure(
        &self,
        user_id: &str,
        asset_id: &str,
        request: ConfigureAssetRequest,
    ) -> Result<AssetOverlayResponse, AssetError> {
        let _lock = self.store.lock_asset(user_id, asset_id)?;
        let record = self
            .repo
            .get(user_id, asset_id)
            .await?
            .ok_or_else(|| AssetError::NotFound(asset_id.into()))?;
        let kind = parse_kind(&record.kind)?;
        let ConfigureAssetRequest {
            configuration,
            secret_updates,
            expected_version,
        } = request;
        if configuration.kind() != kind {
            return Err(AssetError::InvalidMetadata("Overlay 类型与本地资产类型不一致".into()));
        }
        if parse_editability(&record.editability)? == AssetEditability::ReadOnly {
            return Err(AssetError::InvalidState("该资产不允许配置 Overlay".into()));
        }
        validate_public_configuration(&configuration)?;
        self.validate_asset_references(user_id, &configuration).await?;
        let files = load_definition(&self.store.workspace_path(&record.workspace_key)?)?.0;
        let schema = configuration_schema_for(kind, record.entry_file.as_deref(), &files)?;
        let existing_credentials = self.repo.list_credentials(user_id, asset_id).await?;
        let existing_slots = existing_credentials
            .iter()
            .map(|credential| credential.slot.clone())
            .collect::<BTreeSet<_>>();
        let referenced_slots = referenced_secret_slots(&configuration);
        let mut configured_slots = existing_credentials
            .iter()
            .map(|credential| credential.slot.clone())
            .collect::<BTreeSet<_>>();
        let prepared_updates = self.prepare_secret_updates(user_id, asset_id, secret_updates)?;
        for update in &prepared_updates {
            if update.ciphertext.is_some() {
                if !referenced_slots.contains(&update.slot) {
                    return Err(AssetError::InvalidMetadata("只能设置当前公开配置所引用的凭据槽".into()));
                }
                configured_slots.insert(update.slot.clone());
            } else {
                if !referenced_slots.contains(&update.slot) && !existing_slots.contains(&update.slot) {
                    return Err(AssetError::InvalidMetadata("不能清除不存在的凭据槽".into()));
                }
                configured_slots.remove(&update.slot);
            }
        }
        validate_configuration_schema_values(&configuration, schema.as_ref(), &configured_slots)?;
        let overlay_json = serde_json::to_string(&configuration)?;
        let encrypted_updates = prepared_updates
            .iter()
            .map(|update| match update.ciphertext.as_deref() {
                Some(ciphertext) => EncryptedAssetSecretUpdate::Set {
                    slot: &update.slot,
                    ciphertext,
                    key_version: update.key_version,
                },
                None => EncryptedAssetSecretUpdate::Clear { slot: &update.slot },
            })
            .collect::<Vec<_>>();
        let row = self
            .repo
            .configure_overlay(ConfigureAssetOverlayParams {
                user_id,
                asset_id,
                kind: kind_to_db(kind),
                overlay_json: &overlay_json,
                expected_version,
                secret_updates: &encrypted_updates,
                now: now_ms(),
            })
            .await
            .map_err(|error| match error {
                tjuaeui_db::DbError::Conflict(_) => AssetError::ConcurrentModification,
                other => other.into(),
            })?;
        let credentials = self.repo.list_credentials(user_id, asset_id).await?;
        overlay_response(&record, row, &credentials)
    }

    fn prepare_secret_updates(
        &self,
        user_id: &str,
        asset_id: &str,
        updates: Vec<AssetSecretUpdate>,
    ) -> Result<Vec<PreparedSecretUpdate>, AssetError> {
        let mut slots = BTreeSet::new();
        let mut prepared = Vec::with_capacity(updates.len());
        for update in updates {
            let (slot, value) = match update {
                AssetSecretUpdate::Set { slot, value } => (slot, Some(value)),
                AssetSecretUpdate::Clear { slot } => (slot, None),
            };
            validate_secret_slot(&slot)?;
            if !slots.insert(slot.clone()) {
                return Err(AssetError::InvalidMetadata("同一凭据槽不能在一次请求中重复更新".into()));
            }
            let ciphertext = match value {
                Some(value) => {
                    if value.is_empty() || value.len() > 65_536 {
                        return Err(AssetError::InvalidMetadata("凭据值长度无效".into()));
                    }
                    let master_key = self
                        .credential_master_key
                        .ok_or_else(|| AssetError::InvalidState("Core 未配置资产凭据加密密钥，拒绝保存明文".into()))?;
                    let scoped_key = derive_asset_credential_key(
                        &master_key,
                        user_id,
                        asset_id,
                        &slot,
                        ASSET_CREDENTIAL_KEY_VERSION,
                    );
                    Some(encrypt_string(&value, &scoped_key)?)
                }
                None => None,
            };
            prepared.push(PreparedSecretUpdate {
                slot,
                ciphertext,
                key_version: ASSET_CREDENTIAL_KEY_VERSION,
            });
        }
        Ok(prepared)
    }

    async fn validate_asset_references(
        &self,
        user_id: &str,
        configuration: &AssetPublicConfiguration,
    ) -> Result<(), AssetError> {
        let (asset_ids, expected_kind) = match configuration {
            AssetPublicConfiguration::Assistant(value) => (
                value.engine_asset_id.iter().cloned().collect::<Vec<_>>(),
                AssetKind::EngineAdapter,
            ),
            AssetPublicConfiguration::Skill(_)
            | AssetPublicConfiguration::EngineAdapter(_)
            | AssetPublicConfiguration::Mcp(_) => return Ok(()),
        };
        for referenced_id in asset_ids {
            let referenced = self
                .repo
                .get(user_id, &referenced_id)
                .await?
                .ok_or_else(|| AssetError::InvalidMetadata(format!("引用的资产不存在：{referenced_id}")))?;
            if parse_kind(&referenced.kind)? != expected_kind || referenced.runtime_id.is_none() {
                return Err(AssetError::InvalidMetadata(format!(
                    "引用资产 {referenced_id} 的类型或 runtimeId 无效"
                )));
            }
        }
        Ok(())
    }

    pub async fn runtime_status(
        &self,
        user_id: &str,
        asset_id: &str,
    ) -> Result<AssetRuntimeStatusResponse, AssetError> {
        let record = self
            .repo
            .get(user_id, asset_id)
            .await?
            .ok_or_else(|| AssetError::NotFound(asset_id.into()))?;
        let view = self.runtime_view(user_id, &record).await?;
        Ok(AssetRuntimeStatusResponse {
            asset_id: record.id,
            kind: parse_kind(&record.kind)?,
            runtime_state: view.state,
            overlay_version: view.overlay_version,
            runtime_binding: view.binding,
            code: None,
        })
    }

    pub async fn validate_runtime(
        &self,
        user_id: &str,
        asset_id: &str,
        request: AssetRuntimeCommandRequest,
    ) -> Result<AssetRuntimeStatusResponse, AssetError> {
        let _lock = self.store.lock_asset(user_id, asset_id)?;
        let record = self.validate_runtime_command(user_id, asset_id, &request).await?;
        let files = load_definition(&self.store.workspace_path(&record.workspace_key)?)?.0;
        let definition = self
            .runtime_definition_from_catalog_record(user_id, &record, files)
            .await?;
        self.runtime_projector.validate(user_id, vec![definition]).await?;
        let mut status = self.runtime_status(user_id, asset_id).await?;
        status.code = Some("ASSET_RUNTIME_VALIDATED".into());
        Ok(status)
    }

    pub async fn try_run(
        &self,
        user_id: &str,
        asset_id: &str,
        request: AssetRuntimeCommandRequest,
    ) -> Result<AssetRuntimeStatusResponse, AssetError> {
        let _lock = self.store.lock_asset(user_id, asset_id)?;
        let record = self.validate_runtime_command(user_id, asset_id, &request).await?;
        let overlay_version = self
            .repo
            .get_overlay(user_id, asset_id)
            .await?
            .map(|overlay| overlay.version)
            .unwrap_or(0);
        let portable_runtime_id = record
            .runtime_id
            .as_deref()
            .ok_or_else(|| AssetError::InvalidMetadata("核心资产缺少 runtimeId".into()))?;
        let projection_runtime_id =
            derive_projection_runtime_id(user_id, &record.user_id, &record.id, parse_kind(&record.kind)?)?;
        if let Some(receipt) = self
            .repo
            .get_try_run_receipt_by_idempotency(user_id, &request.idempotency_key)
            .await?
        {
            if receipt.asset_id != asset_id
                || receipt.definition_digest != record.definition_digest
                || receipt.overlay_version != overlay_version
                || receipt.portable_runtime_id != portable_runtime_id
                || receipt.projection_runtime_id != projection_runtime_id
            {
                return Err(AssetError::ConcurrentModification);
            }
            let mut status = self.runtime_status(user_id, asset_id).await?;
            status.code = Some("ASSET_RUNTIME_TRY_RUN_SUCCEEDED".into());
            return Ok(status);
        }
        let files = load_definition(&self.store.workspace_path(&record.workspace_key)?)?.0;
        let definition = self
            .runtime_definition_from_catalog_record(user_id, &record, files)
            .await?;
        self.runtime_projector.try_run(user_id, vec![definition]).await?;
        let receipt_id = Uuid::now_v7().to_string();
        self.repo
            .commit_try_run_receipt(CreateAssetTryRunReceiptParams {
                user_id,
                asset_id,
                receipt_id: &receipt_id,
                idempotency_key: &request.idempotency_key,
                definition_digest: &record.definition_digest,
                overlay_version,
                portable_runtime_id,
                projection_runtime_id: &projection_runtime_id,
                created_at: now_ms(),
            })
            .await
            .map_err(|error| match error {
                tjuaeui_db::DbError::Conflict(_) => AssetError::ConcurrentModification,
                other => other.into(),
            })?;
        let mut status = self.runtime_status(user_id, asset_id).await?;
        status.code = Some("ASSET_RUNTIME_TRY_RUN_SUCCEEDED".into());
        Ok(status)
    }

    pub async fn activate(
        &self,
        user_id: &str,
        asset_id: &str,
        request: AssetRuntimeCommandRequest,
    ) -> Result<AssetRuntimeStatusResponse, AssetError> {
        let _lock = self.store.lock_asset(user_id, asset_id)?;
        let record = self.validate_runtime_command(user_id, asset_id, &request).await?;
        let before = self.runtime_view(user_id, &record).await?;
        let overlay_version = before.overlay_version.unwrap_or(0);
        let portable_runtime_id = record
            .runtime_id
            .as_deref()
            .ok_or_else(|| AssetError::InvalidMetadata("核心资产缺少 runtimeId".into()))?;
        let projection_runtime_id =
            derive_projection_runtime_id(user_id, &record.user_id, &record.id, parse_kind(&record.kind)?)?;
        let receipt = self
            .repo
            .get_try_run_receipt(user_id, asset_id)
            .await?
            .filter(|receipt| {
                receipt.definition_digest == record.definition_digest
                    && receipt.overlay_version == overlay_version
                    && receipt.portable_runtime_id == portable_runtime_id
                    && receipt.projection_runtime_id == projection_runtime_id
            })
            .ok_or_else(|| AssetError::InvalidState("启用前必须完成当前版本的成功试跑".into()))?;
        if let Some(binding) = before.binding.as_ref()
            && before.state == AssetRuntimeState::Active
            && binding.definition_digest == record.definition_digest
            && binding.overlay_version == overlay_version
            && binding.try_run_receipt_id.as_deref() == Some(receipt.receipt_id.as_str())
        {
            return self.runtime_status(user_id, asset_id).await;
        }
        self.repo
            .set_runtime_state(SetAssetRuntimeStateParams {
                user_id,
                asset_id,
                state: "activating",
                last_error_code: None,
                now: now_ms(),
            })
            .await?;
        let files = load_definition(&self.store.workspace_path(&record.workspace_key)?)?.0;
        let definition = self
            .runtime_definition_from_catalog_record(user_id, &record, files)
            .await?;
        let mut runtime = match self.runtime_projector.prepare_replace(user_id, vec![definition]).await {
            Ok(runtime) => runtime,
            Err(error) => {
                self.restore_runtime_state_after_activation_failure(user_id, asset_id, before.state, false)
                    .await;
                return Err(error);
            }
        };
        if let Err(error) = runtime.apply().await {
            let rollback = runtime.rollback().await;
            self.restore_runtime_state_after_activation_failure(
                user_id,
                asset_id,
                before.state,
                rollback.is_err() || runtime_error_requires_repair(&error),
            )
            .await;
            if rollback.is_err() {
                return Err(AssetError::RuntimeProjectionFailed {
                    code: "RUNTIME_ROLLBACK_FAILED",
                    message: "启用失败后无法恢复旧运行投影".into(),
                });
            }
            return Err(error);
        }
        let committed = self
            .repo
            .commit_runtime_binding(CommitAssetRuntimeBindingParams {
                user_id,
                asset_id,
                kind: &record.kind,
                projection_kind: kind_to_db(parse_kind(&record.kind)?),
                portable_runtime_id,
                projection_runtime_id: &projection_runtime_id,
                definition_digest: &record.definition_digest,
                overlay_version,
                try_run_receipt_id: &receipt.receipt_id,
                health_status: "healthy",
                last_error_code: None,
                projected_at: now_ms(),
                health_checked_at: Some(now_ms()),
            })
            .await;
        if let Err(error) = committed {
            let rollback = runtime.rollback().await;
            self.restore_runtime_state_after_activation_failure(user_id, asset_id, before.state, rollback.is_err())
                .await;
            if rollback.is_err() {
                return Err(AssetError::RuntimeProjectionFailed {
                    code: "RUNTIME_ROLLBACK_FAILED",
                    message: "数据库提交失败后无法恢复旧运行投影".into(),
                });
            }
            return Err(match error {
                tjuaeui_db::DbError::Conflict(_) => AssetError::ConcurrentModification,
                other => other.into(),
            });
        }
        runtime.finalize().await;
        let mut status = self.runtime_status(user_id, asset_id).await?;
        status.code = Some("ASSET_RUNTIME_ACTIVATED".into());
        Ok(status)
    }

    async fn restore_runtime_state_after_activation_failure(
        &self,
        user_id: &str,
        asset_id: &str,
        previous: AssetRuntimeState,
        repair_required: bool,
    ) {
        let (state, code) = if repair_required {
            ("needsRepair", Some("RUNTIME_ROLLBACK_FAILED"))
        } else {
            (
                match previous {
                    AssetRuntimeState::NotConfigured => "notConfigured",
                    AssetRuntimeState::Inactive => "inactive",
                    AssetRuntimeState::Active => "active",
                    AssetRuntimeState::Degraded => "degraded",
                    AssetRuntimeState::NeedsRepair | AssetRuntimeState::Activating => "needsRepair",
                },
                None,
            )
        };
        if let Err(error) = self
            .repo
            .set_runtime_state(SetAssetRuntimeStateParams {
                user_id,
                asset_id,
                state,
                last_error_code: code,
                now: now_ms(),
            })
            .await
        {
            tracing::error!(asset_id, error = %error, "无法恢复资产运行状态");
        }
    }

    async fn rollback_runtime_change_or_mark_repair(
        &self,
        user_id: &str,
        asset_id: &str,
        runtime: &mut dyn RuntimeProjectionTransaction,
        primary_error: Option<&AssetError>,
    ) -> Result<(), AssetError> {
        let rollback = runtime.rollback().await;
        if rollback.is_err() || primary_error.is_some_and(runtime_error_requires_repair) {
            let error_message = rollback
                .as_ref()
                .err()
                .map_or_else(|| "投影器报告内部补偿失败".into(), ToString::to_string);
            tracing::error!(asset_id, error = %error_message, "Definition 变更失败后无法恢复运行投影");
            if let Err(state_error) = self
                .repo
                .set_runtime_state(SetAssetRuntimeStateParams {
                    user_id,
                    asset_id,
                    state: "needsRepair",
                    last_error_code: Some("RUNTIME_ROLLBACK_FAILED"),
                    now: now_ms(),
                })
                .await
            {
                tracing::error!(asset_id, error = %state_error, "无法标记待修复运行状态");
            }
            return Err(AssetError::RuntimeProjectionFailed {
                code: "RUNTIME_ROLLBACK_FAILED",
                message: format!("Definition 修改失败后无法恢复运行时投影：{error_message}"),
            });
        }
        Ok(())
    }

    async fn rollback_bundle_runtime_or_mark_repair(
        &self,
        user_id: &str,
        asset_ids: &[String],
        operation: &AssetOperationRow,
        runtime: &mut dyn RuntimeProjectionTransaction,
        primary_error: Option<&AssetError>,
    ) -> Result<(), AssetError> {
        let rollback = runtime.rollback().await;
        if rollback.is_err() || primary_error.is_some_and(runtime_error_requires_repair) {
            let error_message = rollback
                .as_ref()
                .err()
                .map_or_else(|| "投影器报告内部补偿失败".into(), ToString::to_string);
            tracing::error!(
                operation_id = operation.operation_id,
                asset_id = operation.asset_id,
                error = %error_message,
                "Bundle 运行投影回滚失败"
            );
            for asset_id in asset_ids {
                if let Err(state_error) = self
                    .repo
                    .set_runtime_state(SetAssetRuntimeStateParams {
                        user_id,
                        asset_id,
                        state: "needsRepair",
                        last_error_code: Some("RUNTIME_ROLLBACK_FAILED"),
                        now: now_ms(),
                    })
                    .await
                {
                    tracing::error!(asset_id, error = %state_error, "无法标记 Bundle 成员待修复");
                }
            }
            return Err(AssetError::RuntimeProjectionFailed {
                code: "RUNTIME_ROLLBACK_FAILED",
                message: "Bundle 运行投影回滚失败".into(),
            });
        }
        Ok(())
    }

    pub async fn deactivate(
        &self,
        user_id: &str,
        asset_id: &str,
        request: AssetRuntimeCommandRequest,
    ) -> Result<AssetRuntimeStatusResponse, AssetError> {
        let _lock = self.store.lock_asset(user_id, asset_id)?;
        let record = self.validate_runtime_command(user_id, asset_id, &request).await?;
        let before = self.runtime_view(user_id, &record).await?;
        let Some(_binding) = self.repo.get_runtime_binding(user_id, asset_id).await? else {
            self.repo.deactivate_runtime(user_id, asset_id, now_ms()).await?;
            return self.runtime_status(user_id, asset_id).await;
        };
        let files = load_definition(&self.store.workspace_path(&record.workspace_key)?)?.0;
        let definition = self
            .runtime_definition_from_catalog_record(user_id, &record, files)
            .await?;
        let mut runtime = self.runtime_projector.prepare_remove(user_id, vec![definition]).await?;
        if let Err(error) = runtime.apply().await {
            let rollback = runtime.rollback().await;
            self.restore_runtime_state_after_activation_failure(
                user_id,
                asset_id,
                before.state,
                rollback.is_err() || runtime_error_requires_repair(&error),
            )
            .await;
            if rollback.is_err() {
                return Err(AssetError::RuntimeProjectionFailed {
                    code: "RUNTIME_ROLLBACK_FAILED",
                    message: "停用失败后无法恢复旧运行投影".into(),
                });
            }
            return Err(error);
        }
        if let Err(error) = self.repo.deactivate_runtime(user_id, asset_id, now_ms()).await {
            let rollback = runtime.rollback().await;
            self.restore_runtime_state_after_activation_failure(user_id, asset_id, before.state, rollback.is_err())
                .await;
            if rollback.is_err() {
                return Err(AssetError::RuntimeProjectionFailed {
                    code: "RUNTIME_ROLLBACK_FAILED",
                    message: "停用提交失败后无法恢复旧运行投影".into(),
                });
            }
            return Err(error.into());
        }
        runtime.finalize().await;
        let mut status = self.runtime_status(user_id, asset_id).await?;
        status.code = Some("ASSET_RUNTIME_DEACTIVATED".into());
        Ok(status)
    }

    async fn validate_runtime_command(
        &self,
        user_id: &str,
        asset_id: &str,
        request: &AssetRuntimeCommandRequest,
    ) -> Result<AssetRecordRow, AssetError> {
        if request.idempotency_key.trim().is_empty() || request.idempotency_key.len() > 128 {
            return Err(AssetError::InvalidMetadata("运行操作幂等键无效".into()));
        }
        let record = self
            .repo
            .get(user_id, asset_id)
            .await?
            .ok_or_else(|| AssetError::NotFound(asset_id.into()))?;
        if record.definition_digest != request.expected_definition_digest {
            return Err(AssetError::ConcurrentModification);
        }
        let overlay = self.repo.get_overlay(user_id, asset_id).await?;
        match (overlay.as_ref(), request.expected_overlay_version) {
            (Some(overlay), Some(expected)) if overlay.version == expected => {}
            (None, None) if matches!(parse_kind(&record.kind)?, AssetKind::Assistant | AssetKind::Skill) => {}
            _ => return Err(AssetError::ConcurrentModification),
        }
        Ok(record)
    }

    async fn runtime_view(&self, user_id: &str, record: &AssetRecordRow) -> Result<RuntimeView, AssetError> {
        let kind = parse_kind(&record.kind)?;
        let overlay = self.repo.get_overlay(user_id, &record.id).await?;
        if let Some(overlay) = overlay.as_ref()
            && parse_kind(&overlay.kind)? != kind
        {
            return Err(AssetError::InvalidMetadata("Overlay 类型与资产类型不一致".into()));
        }
        let overlay_version = overlay.as_ref().map(|value| value.version);
        let stored_state = self
            .repo
            .get_runtime_state(user_id, &record.id)
            .await?
            .map(|value| parse_runtime_state(&value.state))
            .transpose()?
            .unwrap_or_else(|| initial_runtime_state(kind));
        let binding_row = self.repo.get_runtime_binding(user_id, &record.id).await?;
        let binding = binding_row
            .as_ref()
            .map(|value| runtime_binding_response(record, value))
            .transpose()?;
        let expected_projection_runtime_id = derive_projection_runtime_id(user_id, &record.user_id, &record.id, kind)?;
        let has_current_try_run_receipt =
            self.repo
                .get_try_run_receipt(user_id, &record.id)
                .await?
                .is_some_and(|receipt| {
                    receipt.definition_digest == record.definition_digest
                        && receipt.overlay_version == overlay_version.unwrap_or(0)
                        && record.runtime_id.as_deref() == Some(receipt.portable_runtime_id.as_str())
                        && receipt.projection_runtime_id == expected_projection_runtime_id
                });

        let state = match binding_row.as_ref() {
            Some(_) if stored_state == AssetRuntimeState::Activating => AssetRuntimeState::Activating,
            Some(binding)
                if binding.definition_digest != record.definition_digest
                    || binding.overlay_version != overlay_version.unwrap_or(0) =>
            {
                AssetRuntimeState::NeedsRepair
            }
            Some(binding) if binding.health_status == "unhealthy" => AssetRuntimeState::Degraded,
            Some(_) if matches!(stored_state, AssetRuntimeState::Active | AssetRuntimeState::Degraded) => {
                AssetRuntimeState::Active
            }
            Some(_) => AssetRuntimeState::NeedsRepair,
            None if stored_state == AssetRuntimeState::Activating => AssetRuntimeState::Activating,
            None if stored_state == AssetRuntimeState::NeedsRepair => AssetRuntimeState::NeedsRepair,
            None if overlay.is_none() && matches!(kind, AssetKind::EngineAdapter | AssetKind::Mcp) => {
                AssetRuntimeState::NotConfigured
            }
            None => AssetRuntimeState::Inactive,
        };
        Ok(RuntimeView {
            state,
            binding,
            overlay_version,
            has_current_try_run_receipt,
        })
    }

    async fn summary_from_parts(
        &self,
        user_id: &str,
        record: AssetRecordRow,
        upstream: Option<AssetUpstreamRow>,
        base: Option<AssetSnapshotRow>,
    ) -> Result<AssetSummaryResponse, AssetError> {
        let runtime = self.runtime_view(user_id, &record).await?;
        build_summary_from_parts(
            record,
            upstream,
            base,
            runtime.state,
            runtime.has_current_try_run_receipt,
        )
    }

    async fn summary_from_parts_with_remote(
        &self,
        user_id: &str,
        record: AssetRecordRow,
        upstream: Option<AssetUpstreamRow>,
        base: Option<AssetSnapshotRow>,
        remote_assets: &BTreeMap<String, (String, bool)>,
        remote_available: bool,
    ) -> Result<AssetSummaryResponse, AssetError> {
        let runtime = self.runtime_view(user_id, &record).await?;
        build_summary_from_parts_with_remote(
            record,
            upstream,
            base,
            remote_assets,
            remote_available,
            runtime.state,
            runtime.has_current_try_run_receipt,
        )
    }

    async fn prepare_replace_for_bound(
        &self,
        user_id: &str,
        assets: Vec<RuntimeAssetDefinition>,
    ) -> Result<Box<dyn RuntimeProjectionTransaction>, AssetError> {
        let mut bound = Vec::new();
        for asset in assets {
            if self
                .repo
                .get_runtime_binding(user_id, &asset.local_asset_id)
                .await?
                .is_some()
            {
                bound.push(asset);
            }
        }
        if bound.is_empty() {
            Ok(Box::new(NoopRuntimeProjectionTransaction))
        } else {
            self.runtime_projector.prepare_replace(user_id, bound).await
        }
    }

    async fn prepare_remove_for_bound(
        &self,
        user_id: &str,
        assets: Vec<RuntimeAssetDefinition>,
    ) -> Result<Box<dyn RuntimeProjectionTransaction>, AssetError> {
        let mut bound = Vec::new();
        for asset in assets {
            if self
                .repo
                .get_runtime_binding(user_id, &asset.local_asset_id)
                .await?
                .is_some()
            {
                bound.push(asset);
            }
        }
        if bound.is_empty() {
            Ok(Box::new(NoopRuntimeProjectionTransaction))
        } else {
            self.runtime_projector.prepare_remove(user_id, bound).await
        }
    }

    pub async fn register_local(
        &self,
        user_id: &str,
        input: LocalAssetInput,
    ) -> Result<AssetDetailResponse, AssetError> {
        self.register_local_with_runtime_configuration(user_id, input, None)
            .await
    }

    /// 兼容领域调用方的本地 Definition 注册入口。
    ///
    /// 新资产只写 Workspace/Catalog，绝不自动创建运行投影或 binding。
    /// 已 active 的同名资产更新时才通过补偿事务替换既有投影。旧调用方传入
    /// 的自由 JSON 不再进入投影；运行配置只能来自 Core 管理的类型化 Overlay。
    pub async fn register_local_with_runtime_configuration(
        &self,
        user_id: &str,
        input: LocalAssetInput,
        _legacy_runtime_configuration: Option<serde_json::Value>,
    ) -> Result<AssetDetailResponse, AssetError> {
        let _lock = self.store.lock_asset(user_id, &input.id)?;
        let (files, scanned) = prepare_definition(input.files.clone())?;
        validate_entry_file(input.kind, input.entry_file.as_deref(), &scanned.files)?;
        validate_typed_definition(
            input.kind,
            input.entry_file.as_deref(),
            input.runtime_id.as_deref(),
            &files,
        )?;
        let workspace_key = self.store.workspace_key(user_id, &input.id);
        let activation = self.store.activate_workspace(&workspace_key, &files)?;
        let existing = self.repo.get(user_id, &input.id).await?;
        let runtime_configuration = match existing.as_ref() {
            Some(record) => self.resolved_runtime_configuration(user_id, record, &files).await?,
            None => None,
        };
        let mut runtime = self
            .prepare_replace_for_bound(
                user_id,
                vec![runtime_definition_from_local(
                    &self.store,
                    user_id,
                    user_id,
                    &input,
                    &workspace_key,
                    files.clone(),
                    runtime_configuration,
                )?],
            )
            .await?;
        if let Err(error) = runtime.apply().await {
            self.rollback_runtime_change_or_mark_repair(user_id, &input.id, &mut *runtime, Some(&error))
                .await?;
            return Err(error);
        }
        let now = now_ms();
        let stored = match self
            .repo
            .upsert_record(record_params(user_id, &input, &workspace_key, &scanned.digest, now))
            .await
        {
            Ok(stored) => stored,
            Err(error) => {
                self.rollback_runtime_change_or_mark_repair(user_id, &input.id, &mut *runtime, None)
                    .await?;
                return Err(error.into());
            }
        };
        activation.commit();
        runtime.finalize().await;
        self.detail_from_parts(
            user_id,
            AssetDetailParts {
                record: stored,
                upstream: None,
                base: None,
                local_digest: scanned.digest.clone(),
                content_source: AssetContentSource::Local,
                scanned,
            },
        )
        .await
    }

    /// 使用四类固定安全模板创建纯本地资产。
    pub async fn create(&self, user_id: &str, request: CreateAssetRequest) -> Result<AssetDetailResponse, AssetError> {
        validate_new_asset_identity(&request.id, &request.display_name)?;
        if self.repo.get(user_id, &request.id).await?.is_some() {
            return Err(AssetError::InvalidState("资产 ID 已存在".into()));
        }
        let runtime_id = request.runtime_id.unwrap_or_else(|| request.id.clone());
        validate_runtime_id(&runtime_id)?;
        let input = safe_template_input(
            request.id,
            request.kind,
            request.display_name,
            request.description,
            runtime_id,
        )?;
        self.register_local(user_id, input).await
    }

    /// 将可见 Definition 复制成不带任何远程或运行状态的独立本地资产。
    pub async fn duplicate(
        &self,
        user_id: &str,
        source_id: &str,
        request: DuplicateAssetRequest,
    ) -> Result<AssetDetailResponse, AssetError> {
        let source = self
            .repo
            .get(user_id, source_id)
            .await?
            .ok_or_else(|| AssetError::NotFound(source_id.into()))?;
        if self.repo.get(user_id, &request.id).await?.is_some() {
            return Err(AssetError::InvalidState("目标资产 ID 已存在".into()));
        }
        let display_name = request
            .display_name
            .unwrap_or_else(|| format!("{} 副本", source.display_name));
        validate_new_asset_identity(&request.id, &display_name)?;
        let runtime_id = request.runtime_id.unwrap_or_else(|| request.id.clone());
        validate_runtime_id(&runtime_id)?;
        let description = request.description.or(source.description.clone());
        let root = self.store.workspace_path(&source.workspace_key)?;
        let (files, _) = load_definition(&root)?;
        let kind = parse_kind(&source.kind)?;
        let files = rewrite_duplicated_definition(
            kind,
            source.entry_file.as_deref(),
            files,
            &request.id,
            &runtime_id,
            &display_name,
            description.as_deref(),
        )?;
        self.register_local(
            user_id,
            LocalAssetInput {
                id: request.id,
                kind,
                display_name,
                description,
                origin: AssetOrigin::Local,
                trust: AssetTrust::Community,
                scope: AssetScope::User,
                editability: AssetEditability::Full,
                entry_file: source.entry_file,
                runtime_id: Some(runtime_id),
                files,
                dependency_runtime_ids: BTreeMap::new(),
            },
        )
        .await
    }

    pub async fn install_tracked(
        &self,
        user_id: &str,
        idempotency_key: &str,
        input: TrackedAssetInput,
    ) -> Result<AssetOperationResponse, AssetError> {
        self.install_tracked_bundle(user_id, idempotency_key, vec![input]).await
    }

    pub async fn install_tracked_bundle(
        &self,
        user_id: &str,
        idempotency_key: &str,
        inputs: Vec<TrackedAssetInput>,
    ) -> Result<AssetOperationResponse, AssetError> {
        let operation_asset_id = inputs
            .first()
            .map(|input| input.local.id.clone())
            .ok_or_else(|| AssetError::BundleInvariant("Bundle 不能为空".into()))?;
        self.install_tracked_closure(user_id, idempotency_key, &operation_asset_id, inputs)
            .await
    }

    /// 只安装一个依赖闭包的 Definition、upstream 与完整 Base。
    ///
    /// 安装不会调用运行投影器。用户必须在独立的配置、校验、试跑和启用流程中
    /// 明确创建运行投影。
    pub(crate) async fn install_tracked_closure(
        &self,
        user_id: &str,
        idempotency_key: &str,
        operation_asset_id: &str,
        inputs: Vec<TrackedAssetInput>,
    ) -> Result<AssetOperationResponse, AssetError> {
        validate_closure_inputs(&inputs)?;
        let mut lock_ids = inputs.iter().map(|input| input.local.id.clone()).collect::<Vec<_>>();
        lock_ids.sort();
        let mut locks = Vec::with_capacity(lock_ids.len());
        for asset_id in &lock_ids {
            locks.push(self.store.lock_asset(user_id, asset_id)?);
        }
        if let Some(existing) = self.repo.get_operation_by_idempotency(user_id, idempotency_key).await? {
            return operation_response(existing);
        }
        let operation_id = Uuid::now_v7().to_string();
        let started_at = now_ms();
        let operation = self
            .repo
            .start_operation(StartAssetOperationParams {
                user_id,
                operation_id: &operation_id,
                idempotency_key,
                asset_id: operation_asset_id,
                kind: "install",
                phase: "validate",
                recovery_json: &bundle_recovery_json(&inputs)?,
                started_at,
            })
            .await?;
        let result = self.install_tracked_bundle_inner(user_id, &operation, inputs).await;
        self.finish_operation(user_id, operation, result).await
    }

    async fn install_tracked_bundle_inner(
        &self,
        user_id: &str,
        operation: &AssetOperationRow,
        inputs: Vec<TrackedAssetInput>,
    ) -> Result<(), AssetError> {
        let package_names = closure_package_names(&inputs);
        for package_name in &package_names {
            let existing_package_members = self.package_member_records(user_id, package_name).await?;
            if !existing_package_members.is_empty() {
                return Err(AssetError::BundleInvariant(format!(
                    "原子包 {package_name} 已存在部分或全部本地成员"
                )));
            }
        }
        let mut prepared = Vec::with_capacity(inputs.len());
        for input in inputs {
            if self.repo.get(user_id, &input.local.id).await?.is_some() {
                return Err(AssetError::InvalidState(format!("本地资产 {} 已存在", input.local.id)));
            }
            prepared.push(self.prepare_tracked_asset(user_id, input)?);
        }

        let mut activations = Vec::with_capacity(prepared.len());
        for asset in &prepared {
            activations.push(self.store.activate_workspace(&asset.workspace_key, &asset.files)?);
        }
        let now = now_ms();
        let commits = prepared
            .iter()
            .map(|asset| CommitTrackedAssetParams {
                record: record_params(
                    user_id,
                    &asset.input.local,
                    &asset.workspace_key,
                    &asset.scanned.digest,
                    now,
                ),
                upstream: UpsertAssetUpstreamParams {
                    user_id,
                    asset_id: &asset.input.local.id,
                    package_name: &asset.input.package_name,
                    remote_asset_id: &asset.input.remote_asset_id,
                    version: &asset.input.version,
                    source_revision: &asset.input.source_revision,
                    remote_digest: &asset.scanned.digest,
                    tracking_mode: "tracked",
                    checked_at: Some(now),
                },
                snapshot: CreateAssetSnapshotParams {
                    user_id,
                    asset_id: &asset.input.local.id,
                    base_digest: &asset.scanned.digest,
                    object_key: &asset.object_key,
                    manifest_json: &asset.manifest_json,
                    created_at: now,
                },
            })
            .collect::<Vec<_>>();
        self.repo.commit_tracked_assets(&commits).await?;
        for activation in activations {
            activation.commit();
        }
        tracing::info!(
            package_names = ?package_names,
            asset_count = prepared.len(),
            source_revision = prepared[0].input.source_revision,
            operation_id = operation.operation_id,
            "dependency-closed asset packages installed without runtime side effects"
        );
        Ok(())
    }

    pub async fn sync_fast_forward(
        &self,
        user_id: &str,
        idempotency_key: &str,
        input: TrackedAssetInput,
    ) -> Result<AssetOperationResponse, AssetError> {
        self.sync_fast_forward_bundle(user_id, idempotency_key, vec![input])
            .await
    }

    pub async fn sync_fast_forward_bundle(
        &self,
        user_id: &str,
        idempotency_key: &str,
        inputs: Vec<TrackedAssetInput>,
    ) -> Result<AssetOperationResponse, AssetError> {
        let operation_asset_id = inputs
            .first()
            .map(|input| input.local.id.clone())
            .ok_or_else(|| AssetError::BundleInvariant("Bundle 不能为空".into()))?;
        self.sync_fast_forward_closure(user_id, idempotency_key, &operation_asset_id, inputs, Vec::new())
            .await
    }

    /// Fast-forwards an installed target package while installing all missing
    /// dependency packages in the same workspace/runtime/database transaction.
    pub(crate) async fn sync_fast_forward_closure(
        &self,
        user_id: &str,
        idempotency_key: &str,
        operation_asset_id: &str,
        sync_inputs: Vec<TrackedAssetInput>,
        install_inputs: Vec<TrackedAssetInput>,
    ) -> Result<AssetOperationResponse, AssetError> {
        validate_bundle_inputs(&sync_inputs)?;
        if !install_inputs.is_empty() {
            validate_closure_inputs(&install_inputs)?;
        }
        let mut combined_ids = BTreeSet::new();
        for input in sync_inputs.iter().chain(&install_inputs) {
            if !combined_ids.insert(input.local.id.as_str()) {
                return Err(AssetError::BundleInvariant("同步闭包包含重复的本地资产 ID".into()));
            }
        }
        let mut lock_ids = sync_inputs
            .iter()
            .chain(&install_inputs)
            .map(|input| input.local.id.clone())
            .collect::<Vec<_>>();
        lock_ids.sort();
        let mut locks = Vec::with_capacity(lock_ids.len());
        for asset_id in &lock_ids {
            locks.push(self.store.lock_asset(user_id, asset_id)?);
        }
        if let Some(existing) = self.repo.get_operation_by_idempotency(user_id, idempotency_key).await? {
            return operation_response(existing);
        }
        let operation_id = Uuid::now_v7().to_string();
        let started_at = now_ms();
        let operation = self
            .repo
            .start_operation(StartAssetOperationParams {
                user_id,
                operation_id: &operation_id,
                idempotency_key,
                asset_id: operation_asset_id,
                kind: "sync",
                phase: "compare",
                recovery_json: &bundle_recovery_json(
                    &sync_inputs.iter().chain(&install_inputs).cloned().collect::<Vec<_>>(),
                )?,
                started_at,
            })
            .await?;
        let result = self
            .sync_fast_forward_bundle_inner(user_id, &operation, sync_inputs, install_inputs)
            .await;
        self.finish_operation(user_id, operation, result).await
    }

    async fn sync_fast_forward_bundle_inner(
        &self,
        user_id: &str,
        operation: &AssetOperationRow,
        sync_inputs: Vec<TrackedAssetInput>,
        install_inputs: Vec<TrackedAssetInput>,
    ) -> Result<(), AssetError> {
        let package_name = sync_inputs[0].package_name.clone();
        let expected_ids = sync_inputs
            .iter()
            .map(|input| input.local.id.clone())
            .collect::<BTreeSet<_>>();
        let actual_ids = self
            .package_member_records(user_id, &package_name)
            .await?
            .into_iter()
            .map(|record| record.id)
            .collect::<BTreeSet<_>>();
        if actual_ids != expected_ids {
            return Err(AssetError::BundleInvariant(format!(
                "原子包 {package_name} 的本地成员集合不完整"
            )));
        }

        let mut current_by_id = BTreeMap::new();
        for input in &sync_inputs {
            let current = self
                .repo
                .get(user_id, &input.local.id)
                .await?
                .ok_or_else(|| AssetError::NotFound(input.local.id.clone()))?;
            let upstream = self
                .repo
                .get_upstream(user_id, &input.local.id)
                .await?
                .ok_or(AssetError::UpstreamMismatch)?;
            if upstream.package_name != input.package_name || upstream.remote_asset_id != input.remote_asset_id {
                return Err(AssetError::UpstreamMismatch);
            }
            let base = self
                .repo
                .latest_snapshot(user_id, &input.local.id)
                .await?
                .ok_or(AssetError::MissingBaseSnapshot)?;
            let state = calculate_sync_state(
                &current.definition_digest,
                Some(&base.base_digest),
                Some(&input.remote_digest),
                true,
            )?;
            match state {
                AssetSyncState::RemoteUpdated | AssetSyncState::Synced => {}
                AssetSyncState::LocalModified | AssetSyncState::Diverged | AssetSyncState::Conflict => {
                    return Err(AssetError::LocalChanges);
                }
                other => return Err(AssetError::InvalidState(format!("{other:?}"))),
            }
            current_by_id.insert(input.local.id.clone(), current);
        }

        let install_package_names = closure_package_names(&install_inputs);
        for install_package in &install_package_names {
            if !self.package_member_records(user_id, install_package).await?.is_empty() {
                return Err(AssetError::BundleInvariant(format!(
                    "依赖原子包 {install_package} 已存在部分或全部本地成员"
                )));
            }
        }

        let mut prepared = Vec::with_capacity(sync_inputs.len() + install_inputs.len());
        for input in sync_inputs {
            let current = current_by_id
                .get(&input.local.id)
                .ok_or_else(|| AssetError::NotFound(input.local.id.clone()))?;
            let mut asset = self.prepare_tracked_asset(user_id, input)?;
            asset.workspace_key = current.workspace_key.clone();
            prepared.push(asset);
        }
        for input in install_inputs {
            if self.repo.get(user_id, &input.local.id).await?.is_some() {
                return Err(AssetError::InvalidState(format!("本地资产 {} 已存在", input.local.id)));
            }
            prepared.push(self.prepare_tracked_asset(user_id, input)?);
        }
        let prepared_asset_ids = prepared
            .iter()
            .map(|asset| asset.input.local.id.clone())
            .collect::<Vec<_>>();

        let mut activations = Vec::with_capacity(prepared.len());
        for asset in &prepared {
            activations.push(self.store.activate_workspace(&asset.workspace_key, &asset.files)?);
        }
        let mut runtime = self
            .prepare_replace_for_bound(
                user_id,
                self.runtime_definitions_for_prepared(user_id, &prepared).await?,
            )
            .await?;
        if let Err(error) = runtime.apply().await {
            self.rollback_bundle_runtime_or_mark_repair(
                user_id,
                &prepared_asset_ids,
                operation,
                &mut *runtime,
                Some(&error),
            )
            .await?;
            return Err(error);
        }

        let now = now_ms();
        let commits = prepared
            .iter()
            .map(|asset| CommitTrackedAssetParams {
                record: record_params(
                    user_id,
                    &asset.input.local,
                    &asset.workspace_key,
                    &asset.scanned.digest,
                    now,
                ),
                upstream: UpsertAssetUpstreamParams {
                    user_id,
                    asset_id: &asset.input.local.id,
                    package_name: &asset.input.package_name,
                    remote_asset_id: &asset.input.remote_asset_id,
                    version: &asset.input.version,
                    source_revision: &asset.input.source_revision,
                    remote_digest: &asset.scanned.digest,
                    tracking_mode: "tracked",
                    checked_at: Some(now),
                },
                snapshot: CreateAssetSnapshotParams {
                    user_id,
                    asset_id: &asset.input.local.id,
                    base_digest: &asset.scanned.digest,
                    object_key: &asset.object_key,
                    manifest_json: &asset.manifest_json,
                    created_at: now,
                },
            })
            .collect::<Vec<_>>();
        if let Err(error) = self.repo.commit_tracked_assets(&commits).await {
            self.rollback_bundle_runtime_or_mark_repair(user_id, &prepared_asset_ids, operation, &mut *runtime, None)
                .await?;
            return Err(error.into());
        }
        for activation in activations {
            activation.commit();
        }
        runtime.finalize().await;
        tracing::info!(
            package_name,
            installed_dependency_packages = ?install_package_names,
            asset_count = prepared.len(),
            source_revision = prepared[0].input.source_revision,
            operation_id = operation.operation_id,
            "asset package and missing dependencies reconciled and projected atomically"
        );
        Ok(())
    }

    pub async fn list(
        &self,
        user_id: &str,
        kind: Option<AssetKind>,
        scope: Option<AssetScope>,
    ) -> Result<Vec<AssetSummaryResponse>, AssetError> {
        let remote_assets = BTreeMap::new();
        self.list_with_remote_index(user_id, kind, scope, &remote_assets, false)
            .await
    }

    /// Resolve a runtime reference to one exact local catalog identity.
    ///
    /// Resolution is deliberately narrow: an accessible local asset ID wins;
    /// otherwise `reference` must equal exactly one `runtime_id` for the
    /// requested kind. Display names, directory names, remote slugs and other
    /// derived values are never considered. The workspace is scanned before a
    /// result is returned so a catalog/file-system mismatch fails closed.
    pub async fn resolve_runtime_provenance(
        &self,
        user_id: &str,
        kind: AssetKind,
        reference: &str,
    ) -> Result<RuntimeAssetProvenance, AssetError> {
        let reference = reference.trim();
        if reference.is_empty() {
            return Err(AssetError::InvalidMetadata("运行时资产引用不能为空".into()));
        }

        let record = match self.repo.get(user_id, reference).await? {
            Some(record) => {
                let actual_kind = parse_kind(&record.kind)?;
                if actual_kind != kind {
                    return Err(AssetError::InvalidMetadata(format!(
                        "本地资产 {reference} 的类型与请求的运行时类型不一致"
                    )));
                }
                record
            }
            None => {
                let candidates = self
                    .repo
                    .list(user_id, Some(kind_to_db(kind)))
                    .await?
                    .into_iter()
                    .filter(|record| record.runtime_id.as_deref() == Some(reference))
                    .collect::<Vec<_>>();
                match candidates.as_slice() {
                    [] => {
                        return Err(AssetError::NotFound(format!(
                            "找不到类型为 {}、runtimeId 为 {reference} 的本地资产",
                            kind_to_db(kind)
                        )));
                    }
                    [record] => record.clone(),
                    _ => {
                        return Err(AssetError::BundleInvariant(format!(
                            "运行时身份 {}:{reference} 对应多个本地资产",
                            kind_to_db(kind)
                        )));
                    }
                }
            }
        };

        let actual_kind = parse_kind(&record.kind)?;
        if actual_kind != kind {
            return Err(AssetError::InvalidMetadata(format!(
                "本地资产 {} 的类型与请求的运行时类型不一致",
                record.id
            )));
        }
        let runtime_id = record
            .runtime_id
            .as_deref()
            .ok_or_else(|| AssetError::InvalidMetadata(format!("资产 {} 缺少 runtimeId", record.id)))?;
        validate_runtime_id(runtime_id)?;

        let workspace = self.store.workspace_path(&record.workspace_key)?;
        let actual_definition = scan_definition(&workspace)?;
        if actual_definition.digest != record.definition_digest {
            return Err(AssetError::DigestMismatch {
                expected: record.definition_digest,
                actual: actual_definition.digest,
            });
        }

        let upstream = self.repo.get_upstream(&record.user_id, &record.id).await?;
        let (upstream_package, upstream_asset_id, upstream_version, upstream_revision) = match upstream {
            None => (None, None, None, None),
            Some(upstream) => {
                if upstream.user_id != record.user_id || upstream.asset_id != record.id {
                    return Err(AssetError::UpstreamMismatch);
                }
                match parse_tracking_mode(&upstream.tracking_mode)? {
                    AssetTrackingMode::Detached => (None, None, None, None),
                    AssetTrackingMode::Tracked => {
                        for (field, value) in [
                            ("packageName", upstream.package_name.as_str()),
                            ("remoteAssetId", upstream.remote_asset_id.as_str()),
                            ("version", upstream.version.as_str()),
                            ("sourceRevision", upstream.source_revision.as_str()),
                        ] {
                            if value.trim().is_empty() {
                                return Err(AssetError::InvalidMetadata(format!(
                                    "已跟踪资产 {} 的上游 {field} 不能为空",
                                    record.id
                                )));
                            }
                        }
                        (
                            Some(upstream.package_name),
                            Some(upstream.remote_asset_id),
                            Some(upstream.version),
                            Some(upstream.source_revision),
                        )
                    }
                }
            }
        };

        Ok(RuntimeAssetProvenance {
            local_asset_id: record.id,
            kind: actual_kind,
            local_definition_digest: actual_definition.digest,
            runtime_id: runtime_id.to_owned(),
            upstream_package,
            upstream_asset_id,
            upstream_version,
            upstream_revision,
        })
    }

    /// 通过已提交的 RuntimeBinding 精确解析运行来源。
    ///
    /// `reference` 只能是本地资产 ID，或绑定表中唯一的 runtime_id。目录记录
    /// 与绑定的种类、runtime_id、Definition 摘要必须完全一致；名称和目录名
    /// 均不参与解析。
    pub async fn resolve_bound_runtime_provenance(
        &self,
        user_id: &str,
        kind: AssetKind,
        reference: &str,
    ) -> Result<RuntimeAssetProvenance, AssetError> {
        let reference = reference.trim();
        if reference.is_empty() {
            return Err(AssetError::InvalidMetadata("运行绑定引用不能为空".into()));
        }
        let records = self.repo.list(user_id, Some(kind_to_db(kind))).await?;
        let direct = records.iter().find(|record| record.id == reference);
        let record = if let Some(record) = direct {
            record.clone()
        } else {
            let mut candidates = Vec::new();
            for record in records {
                if self
                    .repo
                    .get_runtime_binding(user_id, &record.id)
                    .await?
                    .is_some_and(|binding| binding.portable_runtime_id == reference)
                {
                    candidates.push(record);
                }
            }
            match candidates.as_slice() {
                [] => {
                    return Err(AssetError::NotFound(format!(
                        "找不到类型为 {}、runtimeId 为 {reference} 的运行绑定",
                        kind_to_db(kind)
                    )));
                }
                [record] => record.clone(),
                _ => {
                    return Err(AssetError::BundleInvariant(format!(
                        "运行绑定 {}:{reference} 对应多个本地资产",
                        kind_to_db(kind)
                    )));
                }
            }
        };
        let binding = self
            .repo
            .get_runtime_binding(user_id, &record.id)
            .await?
            .ok_or_else(|| AssetError::NotFound(format!("资产 {} 尚无运行绑定", record.id)))?;
        let runtime_state = self
            .repo
            .get_runtime_state(user_id, &record.id)
            .await?
            .ok_or_else(|| AssetError::NotFound(format!("资产 {} 尚无运行状态", record.id)))?;
        if runtime_state.state != "active" {
            return Err(AssetError::InvalidState(format!(
                "资产 {} 的运行绑定未处于 active 状态",
                record.id
            )));
        }
        let overlay_version = self
            .repo
            .get_overlay(user_id, &record.id)
            .await?
            .map(|overlay| overlay.version)
            .unwrap_or(0);
        let expected_projection_runtime_id = derive_projection_runtime_id(user_id, &record.user_id, &record.id, kind)?;
        if binding.asset_id != record.id
            || binding.user_id != user_id
            || binding.asset_owner_id != record.user_id
            || binding.kind != record.kind
            || binding.projection_kind != kind_to_db(kind)
            || record.runtime_id.as_deref() != Some(binding.portable_runtime_id.as_str())
            || binding.projection_runtime_id != expected_projection_runtime_id
            || binding.definition_digest != record.definition_digest
            || binding.overlay_version != overlay_version
        {
            return Err(AssetError::InvalidMetadata(format!(
                "资产 {} 的运行绑定与目录 Definition 不一致",
                record.id
            )));
        }
        let provenance = self.resolve_runtime_provenance(user_id, kind, &record.id).await?;
        if provenance.runtime_id != binding.portable_runtime_id
            || provenance.local_definition_digest != binding.definition_digest
        {
            return Err(AssetError::DigestMismatch {
                expected: binding.definition_digest,
                actual: provenance.local_definition_digest,
            });
        }
        Ok(provenance)
    }

    /// 解析当前用户已提交且仍与 Definition 一致的运行来源与工作区。
    ///
    /// 该方法是技能注入等运行时消费者的窄入口。它不会回退到旧全局运行表，
    /// 也不会接受显示名、目录名或远程 slug。
    pub async fn resolve_bound_runtime_asset(
        &self,
        user_id: &str,
        kind: AssetKind,
        reference: &str,
    ) -> Result<BoundRuntimeAsset, AssetError> {
        let provenance = self.resolve_bound_runtime_provenance(user_id, kind, reference).await?;
        let record = self
            .repo
            .get(user_id, &provenance.local_asset_id)
            .await?
            .ok_or_else(|| AssetError::NotFound(provenance.local_asset_id.clone()))?;
        let workspace_path = self.store.workspace_path(&record.workspace_key)?;
        let entry_file = runtime_entry_file(kind, record.entry_file.as_deref())?;
        let entry_path = join_safe(&workspace_path, &entry_file)?;
        if !entry_path.is_file() {
            return Err(AssetError::NotFound(format!(
                "资产 {} 的运行入口不存在：{entry_file}",
                provenance.local_asset_id
            )));
        }
        Ok(BoundRuntimeAsset {
            provenance,
            projection_runtime_id: derive_projection_runtime_id(user_id, &record.user_id, &record.id, kind)?,
            overlay_version: self
                .repo
                .get_overlay(user_id, &record.id)
                .await?
                .map(|overlay| overlay.version)
                .unwrap_or(0),
            entry_file,
            workspace_path,
        })
    }

    /// 列出当前用户指定类型的全部 active 绑定。返回值已经校验 Definition、
    /// Overlay 与稳定 projection ID，消费者不得再从旧全局表反向枚举。
    pub async fn list_active_runtime_bindings(
        &self,
        user_id: &str,
        kind: AssetKind,
    ) -> Result<Vec<BoundRuntimeAsset>, AssetError> {
        let bindings = self.repo.list_runtime_bindings(user_id, Some(kind_to_db(kind))).await?;
        let mut resolved = Vec::with_capacity(bindings.len());
        for binding in bindings {
            resolved.push(
                self.resolve_bound_runtime_asset(user_id, kind, &binding.asset_id)
                    .await?,
            );
        }
        resolved.sort_by(|left, right| left.provenance.local_asset_id.cmp(&right.provenance.local_asset_id));
        Ok(resolved)
    }

    /// 将助手 API 中的显式技能引用规范化为本地资产身份。
    ///
    /// 新 Definition 始终保存 `local asset id`。为承接当前助手 API 仍返回
    /// runtime id 的字段，边界层允许以数据库中精确、唯一的 runtime id
    /// 进行一次规范化；这里不读取技能名称、目录名或远程 slug，也不做模糊匹配。
    pub async fn resolve_local_skill_dependencies(
        &self,
        user_id: &str,
        references: &[String],
    ) -> Result<BTreeMap<String, String>, AssetError> {
        let records = self.repo.list(user_id, Some("skill")).await?;
        let mut resolved = BTreeMap::new();
        for reference in references {
            let reference = reference.trim();
            if reference.is_empty() {
                return Err(AssetError::InvalidMetadata("技能资产引用不能为空".into()));
            }
            let direct = records
                .iter()
                .filter(|record| record.id == reference)
                .collect::<Vec<_>>();
            let candidates = if direct.is_empty() {
                records
                    .iter()
                    .filter(|record| record.runtime_id.as_deref() == Some(reference))
                    .collect::<Vec<_>>()
            } else {
                direct
            };
            let [record] = candidates.as_slice() else {
                return Err(if candidates.is_empty() {
                    AssetError::NotFound(format!("未跟踪技能资产：{reference}"))
                } else {
                    AssetError::BundleInvariant(format!("技能运行时身份 {reference} 对应多个本地资产"))
                });
            };
            let runtime_id = record
                .runtime_id
                .as_ref()
                .ok_or_else(|| AssetError::InvalidMetadata(format!("技能资产 {} 缺少 runtimeId", record.id)))?;
            validate_runtime_id(runtime_id)?;
            if let Some(previous) = resolved.insert(record.id.clone(), runtime_id.clone())
                && previous != *runtime_id
            {
                return Err(AssetError::BundleInvariant(format!(
                    "本地技能资产 {} 的 runtimeId 映射不唯一",
                    record.id
                )));
            }
        }
        Ok(resolved)
    }

    /// 将本地技能资产依赖转换为 TjuaeHub 远程身份。
    ///
    /// 未跟踪的本地技能不能被静默按名称发布；调用方必须先把依赖发布并
    /// 建立 upstream，或者改用包含依赖的原子 Bundle 发布流程。
    pub async fn remote_skill_asset_ids(
        &self,
        user_id: &str,
        local_asset_ids: &[String],
    ) -> Result<BTreeMap<String, String>, AssetError> {
        let mut resolved = BTreeMap::new();
        for local_asset_id in local_asset_ids {
            let record = self
                .repo
                .get(user_id, local_asset_id)
                .await?
                .ok_or_else(|| AssetError::NotFound(format!("未跟踪技能资产：{local_asset_id}")))?;
            if record.user_id != user_id || parse_kind(&record.kind)? != AssetKind::Skill {
                return Err(AssetError::InvalidMetadata(format!(
                    "{local_asset_id} 不是当前用户的技能资产"
                )));
            }
            let upstream =
                self.repo.get_upstream(user_id, local_asset_id).await?.ok_or_else(|| {
                    AssetError::InvalidState(format!("技能资产 {local_asset_id} 尚未跟踪到 TjuaeHub"))
                })?;
            resolved.insert(local_asset_id.clone(), upstream.remote_asset_id);
        }
        Ok(resolved)
    }

    /// 使用一次明确的 Hub 索引观察计算本地资产状态。
    ///
    /// 本地仓库本身不把历史 upstream 摘要冒充当前远端；只有调用方确认
    /// 当前索引来自可达的 Hub 时，才允许得出 remoteUpdated、
    /// upstreamRemoved 或 synced。缓存、离线种子和未知网络状态统一降级为
    /// remoteUnknown。
    pub(crate) async fn list_with_remote_index(
        &self,
        user_id: &str,
        kind: Option<AssetKind>,
        scope: Option<AssetScope>,
        remote_assets: &BTreeMap<String, (String, bool)>,
        remote_available: bool,
    ) -> Result<Vec<AssetSummaryResponse>, AssetError> {
        let kind_value = kind.map(kind_to_db);
        let records = self.repo.list(user_id, kind_value).await?;
        let mut output = Vec::with_capacity(records.len());
        for record in records {
            if scope.is_some_and(|expected| parse_scope(&record.scope).ok() != Some(expected)) {
                continue;
            }
            let upstream = self.repo.get_upstream(user_id, &record.id).await?;
            let base = self.repo.latest_snapshot(user_id, &record.id).await?;
            output.push(
                self.summary_from_parts_with_remote(user_id, record, upstream, base, remote_assets, remote_available)
                    .await?,
            );
        }
        Ok(output)
    }

    /// 将远程索引中的资产与当前用户可见的本地仓库建立真实关系。
    ///
    /// 同步状态使用“当前远程摘要”重新计算，不能复用上次刷新时缓存的远程状态。
    pub(crate) async fn relation_for_remote(
        &self,
        user_id: &str,
        remote_asset_id: &str,
        current_remote_digest: &str,
        compatible: bool,
        remote_available: bool,
    ) -> Result<Option<MarketLocalRelation>, AssetError> {
        let requested = BTreeMap::from([(
            remote_asset_id.to_owned(),
            (current_remote_digest.to_owned(), compatible),
        )]);
        Ok(self
            .relations_for_remotes(user_id, &requested, remote_available)
            .await?
            .remove(remote_asset_id))
    }

    /// 批量关联远程资产，避免市场列表按资产数重复扫描本地仓库。
    pub(crate) async fn relations_for_remotes(
        &self,
        user_id: &str,
        remote_assets: &BTreeMap<String, (String, bool)>,
        remote_available: bool,
    ) -> Result<BTreeMap<String, MarketLocalRelation>, AssetError> {
        let mut relations = BTreeMap::new();
        for record in self.repo.list(user_id, None).await? {
            let Some(upstream) = self.repo.get_upstream(user_id, &record.id).await? else {
                continue;
            };
            let base = self.repo.latest_snapshot(user_id, &record.id).await?;
            let editability = parse_editability(&record.editability)?;
            let scope = parse_scope(&record.scope)?;
            let kind = parse_kind(&record.kind)?;
            let runtime = self.runtime_view(user_id, &record).await?;
            let sync_state =
                sync_state_from_remote_index(&record, Some(&upstream), base.as_ref(), remote_assets, remote_available)?
                    .ok_or_else(|| AssetError::InvalidState("远程关系缺少跟踪状态".into()))?;
            relations
                .entry(upstream.remote_asset_id)
                .or_insert_with(|| MarketLocalRelation {
                    sync_state,
                    allowed_actions: allowed_actions(
                        Some(sync_state),
                        editability,
                        scope,
                        kind,
                        runtime.state,
                        runtime.has_current_try_run_receipt,
                    ),
                    local: MarketLocalRelationResponse {
                        local_asset_id: record.id,
                        local_digest: record.definition_digest,
                        base_digest: base.map(|value| value.base_digest),
                    },
                });
        }
        Ok(relations)
    }

    pub async fn get(&self, user_id: &str, asset_id: &str) -> Result<AssetDetailResponse, AssetError> {
        self.get_from_source(user_id, asset_id, AssetContentSource::Local).await
    }

    pub async fn get_from_source(
        &self,
        user_id: &str,
        asset_id: &str,
        source: AssetContentSource,
    ) -> Result<AssetDetailResponse, AssetError> {
        let record = self
            .repo
            .get(user_id, asset_id)
            .await?
            .ok_or_else(|| AssetError::NotFound(asset_id.into()))?;
        let upstream = self.repo.get_upstream(user_id, asset_id).await?;
        let base = self.repo.latest_snapshot(user_id, asset_id).await?;
        let local_root = self.store.workspace_path(&record.workspace_key)?;
        let local = scan_definition(&local_root)?;
        if local.digest != record.definition_digest {
            tracing::debug!(
                asset_id,
                recorded_digest = record.definition_digest,
                actual_digest = local.digest,
                "asset definition changed outside Core"
            );
        }
        let scanned = match source {
            AssetContentSource::Local => local.clone(),
            AssetContentSource::Base => {
                let snapshot = base.as_ref().ok_or(AssetError::MissingBaseSnapshot)?;
                let root = self.store.object_path(&snapshot.object_key)?;
                if !root.is_dir() {
                    return Err(AssetError::SourceUnavailable("base".into()));
                }
                scan_definition(&root)?
            }
            AssetContentSource::Remote => {
                let remote = upstream
                    .as_ref()
                    .ok_or_else(|| AssetError::SourceUnavailable("remote".into()))?;
                let object_key = remote
                    .remote_digest
                    .strip_prefix("sha256-")
                    .ok_or_else(|| AssetError::InvalidMetadata("远程摘要必须使用 sha256- 前缀".into()))?;
                let root = self.store.object_path(object_key)?;
                if !root.is_dir() {
                    return Err(AssetError::SourceUnavailable("remote".into()));
                }
                scan_definition(&root)?
            }
        };
        self.detail_from_parts(
            user_id,
            AssetDetailParts {
                record,
                upstream,
                base,
                local_digest: local.digest,
                content_source: source,
                scanned,
            },
        )
        .await
    }

    async fn detail_from_parts(
        &self,
        runtime_user_id: &str,
        parts: AssetDetailParts,
    ) -> Result<AssetDetailResponse, AssetError> {
        let mut effective_record = parts.record;
        effective_record.definition_digest = parts.local_digest;
        let entry_file = effective_record.entry_file.clone();
        let runtime = self.runtime_view(runtime_user_id, &effective_record).await?;
        Ok(AssetDetailResponse {
            asset: build_summary_from_parts(
                effective_record,
                parts.upstream,
                parts.base,
                runtime.state,
                runtime.has_current_try_run_receipt,
            )?,
            files: parts.scanned.api_files(),
            entry_file,
            content_source: parts.content_source,
            source_digest: parts.scanned.digest,
            runtime_binding: runtime.binding,
        })
    }

    pub async fn read_file(
        &self,
        user_id: &str,
        asset_id: &str,
        path: &str,
        source: AssetContentSource,
    ) -> Result<AssetFileResponse, AssetError> {
        let record = self
            .repo
            .get(user_id, asset_id)
            .await?
            .ok_or_else(|| AssetError::NotFound(asset_id.into()))?;
        let normalized = normalize_relative_path(path)?;
        let upstream = self.repo.get_upstream(user_id, asset_id).await?;
        let base = self.repo.latest_snapshot(user_id, asset_id).await?;
        let root = match source {
            AssetContentSource::Local => self.store.workspace_path(&record.workspace_key)?,
            AssetContentSource::Base => {
                let snapshot = base.as_ref().ok_or(AssetError::MissingBaseSnapshot)?;
                let root = self.store.object_path(&snapshot.object_key)?;
                if !root.is_dir() {
                    return Err(AssetError::SourceUnavailable("base".into()));
                }
                root
            }
            AssetContentSource::Remote => {
                let remote = upstream
                    .as_ref()
                    .ok_or_else(|| AssetError::SourceUnavailable("remote".into()))?;
                let object_key = remote
                    .remote_digest
                    .strip_prefix("sha256-")
                    .ok_or_else(|| AssetError::InvalidMetadata("远程摘要必须使用 sha256- 前缀".into()))?;
                let root = self.store.object_path(object_key)?;
                if !root.is_dir() {
                    return Err(AssetError::SourceUnavailable("remote".into()));
                }
                root
            }
        };
        let target = join_safe(&root, &normalized)?;
        let content = std::fs::read(&target)?;
        let content = String::from_utf8(content).map_err(|_| AssetError::BinaryFile(normalized.clone()))?;
        Ok(AssetFileResponse {
            asset_id: asset_id.into(),
            path: normalized.clone(),
            digest: digest_bytes(content.as_bytes()),
            media_type: mime_guess::from_path(&normalized)
                .first_raw()
                .unwrap_or("text/plain")
                .into(),
            content,
        })
    }

    /// Read the complete local Definition as bytes.
    ///
    /// Domain services use this when an edit changes the file set (for
    /// example replacing an assistant avatar or adding a localized rule).
    /// The catalog still resolves the workspace from the user-scoped record;
    /// callers never receive a filesystem path.
    pub async fn definition_files(
        &self,
        user_id: &str,
        asset_id: &str,
    ) -> Result<Vec<AssetDefinitionFile>, AssetError> {
        let record = self
            .repo
            .get(user_id, asset_id)
            .await?
            .ok_or_else(|| AssetError::NotFound(asset_id.into()))?;
        let root = self.store.workspace_path(&record.workspace_key)?;
        let (files, _) = load_definition(&root)?;
        Ok(files)
    }

    pub async fn write_file(
        &self,
        user_id: &str,
        asset_id: &str,
        path: &str,
        content: &str,
        expected_digest: &str,
    ) -> Result<AssetDetailResponse, AssetError> {
        let _lock = self.store.lock_asset(user_id, asset_id)?;
        let record = self
            .repo
            .get(user_id, asset_id)
            .await?
            .ok_or_else(|| AssetError::NotFound(asset_id.into()))?;
        if parse_editability(&record.editability)? != AssetEditability::Full {
            return Err(AssetError::InvalidState("资产 Definition 只读".into()));
        }
        let normalized = normalize_relative_path(path)?;
        if content.len() as u64 > MAX_DEFINITION_FILE_BYTES {
            return Err(AssetError::FileTooLarge {
                path: normalized,
                actual: content.len() as u64,
                limit: MAX_DEFINITION_FILE_BYTES,
            });
        }
        let root = self.store.workspace_path(&record.workspace_key)?;
        let target = join_safe(&root, &normalized)?;
        let current = std::fs::read(&target)?;
        if digest_bytes(&current) != expected_digest {
            return Err(AssetError::ConcurrentModification);
        }
        let (mut files, _) = load_definition(&root)?;
        let file = files
            .iter_mut()
            .find(|file| file.path == normalized)
            .ok_or_else(|| AssetError::NotFound(normalized.clone()))?;
        file.content = content.as_bytes().to_vec();
        let (_, scanned) = prepare_definition(files.clone())?;
        validate_entry_file(parse_kind(&record.kind)?, record.entry_file.as_deref(), &scanned.files)?;
        validate_typed_definition(
            parse_kind(&record.kind)?,
            record.entry_file.as_deref(),
            record.runtime_id.as_deref(),
            &files,
        )?;
        let activation = self.store.activate_workspace(&record.workspace_key, &files)?;
        let mut runtime = self
            .prepare_replace_for_bound(
                user_id,
                vec![
                    self.runtime_definition_from_catalog_record(user_id, &record, files.clone())
                        .await?,
                ],
            )
            .await?;
        if let Err(error) = runtime.apply().await {
            self.rollback_runtime_change_or_mark_repair(user_id, asset_id, &mut *runtime, Some(&error))
                .await?;
            return Err(error);
        }
        let updated = match self
            .repo
            .upsert_record(UpsertAssetRecordParams {
                user_id,
                id: &record.id,
                kind: &record.kind,
                display_name: &record.display_name,
                description: record.description.as_deref(),
                origin: &record.origin,
                trust: &record.trust,
                scope: &record.scope,
                editability: &record.editability,
                workspace_key: &record.workspace_key,
                definition_digest: &scanned.digest,
                entry_file: record.entry_file.as_deref(),
                runtime_id: record.runtime_id.as_deref(),
                now: now_ms(),
            })
            .await
        {
            Ok(updated) => updated,
            Err(error) => {
                self.rollback_runtime_change_or_mark_repair(user_id, asset_id, &mut *runtime, None)
                    .await?;
                return Err(error.into());
            }
        };
        activation.commit();
        runtime.finalize().await;
        let upstream = self.repo.get_upstream(user_id, asset_id).await?;
        let base = self.repo.latest_snapshot(user_id, asset_id).await?;
        self.detail_from_parts(
            user_id,
            AssetDetailParts {
                record: updated,
                upstream,
                base,
                local_digest: scanned.digest.clone(),
                content_source: AssetContentSource::Local,
                scanned,
            },
        )
        .await
    }

    pub async fn diff(&self, user_id: &str, asset_id: &str) -> Result<AssetDiffResponse, AssetError> {
        let record = self
            .repo
            .get(user_id, asset_id)
            .await?
            .ok_or_else(|| AssetError::NotFound(asset_id.into()))?;
        let upstream = self
            .repo
            .get_upstream(user_id, asset_id)
            .await?
            .ok_or_else(|| AssetError::SourceUnavailable("remote".into()))?;
        let base = self
            .repo
            .latest_snapshot(user_id, asset_id)
            .await?
            .ok_or(AssetError::MissingBaseSnapshot)?;
        let (local_files, local) = load_definition(&self.store.workspace_path(&record.workspace_key)?)?;
        let base_root = self.store.object_path(&base.object_key)?;
        if !base_root.is_dir() {
            return Err(AssetError::SourceUnavailable("base".into()));
        }
        let (base_files, base_scanned) = load_definition(&base_root)?;
        let remote_key = upstream
            .remote_digest
            .strip_prefix("sha256-")
            .ok_or_else(|| AssetError::InvalidMetadata("远程摘要必须使用 sha256- 前缀".into()))?;
        let remote_root = self.store.object_path(remote_key)?;
        if !remote_root.is_dir() {
            // Never turn an absent remote object into an empty Definition:
            // the market coordinator must fetch and verify it first.
            return Err(AssetError::SourceUnavailable("remote".into()));
        }
        let (remote_files, remote) = load_definition(&remote_root)?;
        let files = crate::three_way::compare_definitions(&base_files, &local_files, &remote_files);
        let state = if files
            .iter()
            .any(|file| file.status == tjuaeui_api_types::AssetDiffFileStatus::Conflict)
        {
            AssetSyncState::Conflict
        } else {
            calculate_sync_state(&local.digest, Some(&base_scanned.digest), Some(&remote.digest), true)?
        };
        Ok(AssetDiffResponse {
            asset_id: asset_id.into(),
            sync_state: state,
            local_digest: local.digest,
            base_digest: base_scanned.digest,
            remote_digest: remote.digest,
            files,
        })
    }

    pub(crate) async fn tracked_reference(
        &self,
        user_id: &str,
        asset_id: &str,
    ) -> Result<TrackedAssetReference, AssetError> {
        self.repo
            .get(user_id, asset_id)
            .await?
            .ok_or_else(|| AssetError::NotFound(asset_id.into()))?;
        let upstream = self
            .repo
            .get_upstream(user_id, asset_id)
            .await?
            .ok_or(AssetError::UpstreamMismatch)?;
        if upstream.tracking_mode != "tracked" {
            return Err(AssetError::InvalidState("资产已解除远程跟踪".into()));
        }
        Ok(TrackedAssetReference {
            package_name: upstream.package_name,
            remote_asset_id: upstream.remote_asset_id,
            version: upstream.version,
            remote_digest: upstream.remote_digest,
        })
    }

    /// Compares against the verified Definition supplied by the current,
    /// commit-pinned market index. This is the only diff used by the UI.
    pub(crate) async fn diff_against_remote(
        &self,
        user_id: &str,
        asset_id: &str,
        input: &TrackedAssetInput,
    ) -> Result<AssetDiffResponse, AssetError> {
        let _lock = self.store.lock_asset(user_id, asset_id)?;
        self.validate_remote_identity(user_id, asset_id, input).await?;
        let (remote_files, remote) = prepare_definition(input.local.files.clone())?;
        if remote.digest != input.remote_digest {
            return Err(AssetError::DigestMismatch {
                expected: input.remote_digest.clone(),
                actual: remote.digest,
            });
        }
        self.store.ensure_object(remote_files, &input.remote_digest)?;
        let loaded = self.load_three_way(user_id, asset_id, &input.remote_digest).await?;
        let files =
            crate::three_way::compare_definitions(&loaded.base_files, &loaded.local_files, &loaded.remote_files);
        let sync_state = if files
            .iter()
            .any(|file| file.status == tjuaeui_api_types::AssetDiffFileStatus::Conflict)
        {
            AssetSyncState::Conflict
        } else {
            calculate_sync_state(
                &loaded.local.digest,
                Some(&loaded.base_scanned.digest),
                Some(&loaded.remote.digest),
                true,
            )?
        };
        Ok(AssetDiffResponse {
            asset_id: asset_id.into(),
            sync_state,
            local_digest: loaded.local.digest,
            base_digest: loaded.base_scanned.digest,
            remote_digest: loaded.remote.digest,
            files,
        })
    }

    pub(crate) async fn resolve_against_remote(
        &self,
        user_id: &str,
        asset_id: &str,
        input: TrackedAssetInput,
        request: ResolveAssetRequest,
    ) -> Result<AssetResolveResponse, AssetError> {
        if let Some(existing) = self
            .repo
            .get_operation_by_idempotency(user_id, &request.idempotency_key)
            .await?
        {
            if existing.asset_id != asset_id || existing.kind != "resolve" {
                return Err(AssetError::InvalidState("幂等键已用于其他资产操作".into()));
            }
            let asset = self
                .list(user_id, None, None)
                .await?
                .into_iter()
                .find(|asset| asset.id == asset_id)
                .ok_or_else(|| AssetError::NotFound(asset_id.into()))?;
            let recovery = recovery_metadata(&existing.recovery_json)?;
            return Ok(AssetResolveResponse {
                asset,
                operation: operation_response(existing.clone())?,
                strategy: request.strategy,
                recovery_operation_id: recovery.as_ref().map(|_| existing.operation_id),
                recovery_digest: recovery.map(|value| value.1),
            });
        }
        let detach_records = if request.strategy == AssetResolveStrategy::Detach {
            Some(self.tracked_package_records(user_id, asset_id).await?)
        } else {
            None
        };
        let mut _asset_locks = Vec::new();
        if let Some(records) = detach_records.as_ref() {
            _asset_locks.reserve(records.len());
            for member in records {
                _asset_locks.push(self.store.lock_asset(user_id, &member.id)?);
            }
        } else {
            _asset_locks.push(self.store.lock_asset(user_id, asset_id)?);
        }
        self.validate_remote_identity(user_id, asset_id, &input).await?;
        let (remote_files, remote) = prepare_definition(input.local.files.clone())?;
        if remote.digest != input.remote_digest {
            return Err(AssetError::DigestMismatch {
                expected: input.remote_digest,
                actual: remote.digest,
            });
        }
        let (remote_object_key, _) = self.store.ensure_object(remote_files.clone(), &remote.digest)?;
        let loaded = self.load_three_way(user_id, asset_id, &remote.digest).await?;
        if loaded.local.digest != request.expected_local_digest
            || loaded.base_scanned.digest != request.expected_base_digest
            || loaded.remote.digest != request.expected_remote_digest
        {
            return Err(AssetError::ConcurrentModification);
        }
        if parse_editability(&loaded.record.editability)? != AssetEditability::Full
            || parse_scope(&loaded.record.scope)? == AssetScope::System
        {
            return Err(AssetError::InvalidState("此资产不能处理远程分叉".into()));
        }

        let operation_id = Uuid::now_v7().to_string();
        let started_at = now_ms();
        if request.strategy == AssetResolveStrategy::Detach {
            let records = detach_records
                .as_ref()
                .ok_or_else(|| AssetError::InvalidState("解除跟踪缺少 Bundle 上下文".into()))?;
            let asset_ids = records.iter().map(|record| record.id.clone()).collect::<Vec<_>>();
            let operation = self
                .repo
                .start_operation(StartAssetOperationParams {
                    user_id,
                    operation_id: &operation_id,
                    idempotency_key: &request.idempotency_key,
                    asset_id,
                    kind: "resolve",
                    phase: "detach",
                    recovery_json: "{}",
                    started_at,
                })
                .await?;
            let stored = match self
                .repo
                .commit_detach_resolution(user_id, &asset_ids, asset_id, &operation.operation_id, now_ms())
                .await
            {
                Ok(stored) => stored,
                Err(error) => {
                    return self.fail_started_operation(user_id, operation, error.into()).await;
                }
            };
            let asset = self
                .list(user_id, None, None)
                .await?
                .into_iter()
                .find(|asset| asset.id == asset_id)
                .ok_or_else(|| AssetError::NotFound(asset_id.into()))?;
            return Ok(AssetResolveResponse {
                asset,
                operation: operation_response(stored)?,
                strategy: request.strategy,
                recovery_operation_id: None,
                recovery_digest: None,
            });
        }

        let next_files = match request.strategy {
            AssetResolveStrategy::AutoMerge => {
                crate::three_way::merge_definitions(&loaded.base_files, &loaded.local_files, &loaded.remote_files)?
            }
            AssetResolveStrategy::KeepLocal => loaded.local_files.clone(),
            AssetResolveStrategy::UseRemote => {
                if !request.confirm_destructive {
                    return Err(AssetError::DestructiveConfirmationRequired);
                }
                loaded.remote_files.clone()
            }
            AssetResolveStrategy::Detach => unreachable!("detach returned above"),
        };
        let (next_files, next) = prepare_definition(next_files)?;
        validate_entry_file(
            parse_kind(&loaded.record.kind)?,
            loaded.record.entry_file.as_deref(),
            &next.files,
        )?;
        validate_typed_definition(
            parse_kind(&loaded.record.kind)?,
            loaded.record.entry_file.as_deref(),
            loaded.record.runtime_id.as_deref(),
            &next_files,
        )?;

        let recovery = if request.strategy == AssetResolveStrategy::UseRemote {
            let (key, _) = self
                .store
                .ensure_object(loaded.local_files.clone(), &loaded.local.digest)?;
            Some((key, loaded.local.digest.clone()))
        } else {
            None
        };
        let recovery_json = if let Some((object_key, digest)) = &recovery {
            serde_json::to_string(&serde_json::json!({
                "kind": "useRemote",
                "objectKey": object_key,
                "digest": digest,
            }))?
        } else {
            "{}".into()
        };
        let runtime_definition = if next.digest != loaded.local.digest {
            Some(
                self.runtime_definition_from_catalog_record(user_id, &loaded.record, next_files.clone())
                    .await?,
            )
        } else {
            None
        };
        let operation = self
            .repo
            .start_operation(StartAssetOperationParams {
                user_id,
                operation_id: &operation_id,
                idempotency_key: &request.idempotency_key,
                asset_id,
                kind: "resolve",
                phase: "activate",
                recovery_json: &recovery_json,
                started_at,
            })
            .await?;
        let activation = if next.digest != loaded.local.digest {
            match self.store.activate_workspace(&loaded.record.workspace_key, &next_files) {
                Ok(activation) => Some(activation),
                Err(error) => {
                    return self.fail_started_operation(user_id, operation, error).await;
                }
            }
        } else {
            None
        };
        let mut runtime = if let Some(definition) = runtime_definition {
            match self.prepare_replace_for_bound(user_id, vec![definition]).await {
                Ok(runtime) => Some(runtime),
                Err(error) => {
                    return self.fail_started_operation(user_id, operation, error).await;
                }
            }
        } else {
            None
        };
        if let Some(runtime) = runtime.as_mut()
            && let Err(error) = runtime.apply().await
        {
            let failure = match self
                .rollback_runtime_change_or_mark_repair(user_id, asset_id, &mut **runtime, Some(&error))
                .await
            {
                Ok(()) => error,
                Err(rollback_error) => rollback_error,
            };
            return self.fail_started_operation(user_id, operation, failure).await;
        }

        let now = now_ms();
        let remote_manifest_json = serde_json::to_string(&remote.files)?;
        let stored = self
            .repo
            .commit_resolved_asset(CommitResolvedAssetParams {
                record: record_params_from_row(user_id, &loaded.record, &next.digest, now),
                upstream: UpsertAssetUpstreamParams {
                    user_id,
                    asset_id,
                    package_name: &input.package_name,
                    remote_asset_id: &input.remote_asset_id,
                    version: &input.version,
                    source_revision: &input.source_revision,
                    remote_digest: &remote.digest,
                    tracking_mode: "tracked",
                    checked_at: Some(now),
                },
                snapshot: CreateAssetSnapshotParams {
                    user_id,
                    asset_id,
                    base_digest: &remote.digest,
                    object_key: &remote_object_key,
                    manifest_json: &remote_manifest_json,
                    created_at: now,
                },
                operation_id: &operation.operation_id,
                recovery_json: &recovery_json,
                finished_at: now,
            })
            .await;
        let stored = match stored {
            Ok(stored) => stored,
            Err(error) => {
                let failure = if let Some(runtime) = runtime.as_mut() {
                    match self
                        .rollback_runtime_change_or_mark_repair(user_id, asset_id, &mut **runtime, None)
                        .await
                    {
                        Ok(()) => error.into(),
                        Err(rollback_error) => rollback_error,
                    }
                } else {
                    error.into()
                };
                return self.fail_started_operation(user_id, operation, failure).await;
            }
        };
        if let Some(activation) = activation {
            activation.commit();
        }
        if let Some(runtime) = runtime {
            runtime.finalize().await;
        }
        let record = self
            .repo
            .get(user_id, asset_id)
            .await?
            .ok_or_else(|| AssetError::NotFound(asset_id.into()))?;
        let upstream = self.repo.get_upstream(user_id, asset_id).await?;
        let base = self.repo.latest_snapshot(user_id, asset_id).await?;
        let remote_assets = BTreeMap::from([(input.remote_asset_id.clone(), (input.remote_digest.clone(), true))]);
        Ok(AssetResolveResponse {
            asset: self
                .summary_from_parts_with_remote(user_id, record, upstream, base, &remote_assets, true)
                .await?,
            operation: operation_response(stored)?,
            strategy: request.strategy,
            recovery_operation_id: recovery.as_ref().map(|_| operation_id),
            recovery_digest: recovery.map(|value| value.1),
        })
    }

    pub async fn restore_resolution(
        &self,
        user_id: &str,
        asset_id: &str,
        request: RestoreAssetRequest,
    ) -> Result<AssetRestoreResponse, AssetError> {
        let _lock = self.store.lock_asset(user_id, asset_id)?;
        if let Some(existing) = self
            .repo
            .get_operation_by_idempotency(user_id, &request.idempotency_key)
            .await?
        {
            if existing.asset_id != asset_id || existing.kind != "restore" {
                return Err(AssetError::InvalidState("幂等键已用于其他资产操作".into()));
            }
            let record = self
                .repo
                .get(user_id, asset_id)
                .await?
                .ok_or_else(|| AssetError::NotFound(asset_id.into()))?;
            let digest = record.definition_digest.clone();
            let upstream = self.repo.get_upstream(user_id, asset_id).await?;
            let base = self.repo.latest_snapshot(user_id, asset_id).await?;
            return Ok(AssetRestoreResponse {
                asset: self.summary_from_parts(user_id, record, upstream, base).await?,
                operation: operation_response(existing)?,
                recovered_digest: digest,
            });
        }
        let record = self
            .repo
            .get(user_id, asset_id)
            .await?
            .ok_or_else(|| AssetError::NotFound(asset_id.into()))?;
        let workspace = self.store.workspace_path(&record.workspace_key)?;
        let (_, current) = load_definition(&workspace)?;
        if current.digest != request.expected_local_digest {
            return Err(AssetError::ConcurrentModification);
        }
        let recovery_operation = self
            .repo
            .get_operation(user_id, &request.recovery_operation_id)
            .await?
            .ok_or_else(|| AssetError::NotFound(request.recovery_operation_id.clone()))?;
        if recovery_operation.asset_id != asset_id
            || recovery_operation.kind != "resolve"
            || recovery_operation.state != "succeeded"
        {
            return Err(AssetError::InvalidState("恢复来源不是成功的远程覆盖操作".into()));
        }
        let Some((object_key, recovery_digest)) = recovery_metadata(&recovery_operation.recovery_json)? else {
            return Err(AssetError::InvalidState("此操作没有可恢复快照".into()));
        };
        let recovery_root = self.store.object_path(&object_key)?;
        if !recovery_root.is_dir() {
            return Err(AssetError::SourceUnavailable("recovery".into()));
        }
        let (recovery_files, recovered) = load_definition(&recovery_root)?;
        if recovered.digest != recovery_digest {
            return Err(AssetError::CorruptObject(recovery_root));
        }
        validate_entry_file(
            parse_kind(&record.kind)?,
            record.entry_file.as_deref(),
            &recovered.files,
        )?;
        validate_typed_definition(
            parse_kind(&record.kind)?,
            record.entry_file.as_deref(),
            record.runtime_id.as_deref(),
            &recovery_files,
        )?;
        let operation_id = Uuid::now_v7().to_string();
        let runtime_definition = self
            .runtime_definition_from_catalog_record(user_id, &record, recovery_files.clone())
            .await?;
        let operation = self
            .repo
            .start_operation(StartAssetOperationParams {
                user_id,
                operation_id: &operation_id,
                idempotency_key: &request.idempotency_key,
                asset_id,
                kind: "restore",
                phase: "activate",
                recovery_json: "{}",
                started_at: now_ms(),
            })
            .await?;
        let activation = match self.store.activate_workspace(&record.workspace_key, &recovery_files) {
            Ok(activation) => activation,
            Err(error) => {
                return self.fail_started_operation(user_id, operation, error).await;
            }
        };
        let mut runtime = match self.prepare_replace_for_bound(user_id, vec![runtime_definition]).await {
            Ok(runtime) => runtime,
            Err(error) => {
                return self.fail_started_operation(user_id, operation, error).await;
            }
        };
        if let Err(error) = runtime.apply().await {
            let failure = match self
                .rollback_runtime_change_or_mark_repair(user_id, asset_id, &mut *runtime, Some(&error))
                .await
            {
                Ok(()) => error,
                Err(rollback_error) => rollback_error,
            };
            return self.fail_started_operation(user_id, operation, failure).await;
        }
        let now = now_ms();
        let stored = self
            .repo
            .commit_restored_asset(
                record_params_from_row(user_id, &record, &recovered.digest, now),
                &operation.operation_id,
                now,
            )
            .await;
        let stored = match stored {
            Ok(stored) => stored,
            Err(error) => {
                let failure = match self
                    .rollback_runtime_change_or_mark_repair(user_id, asset_id, &mut *runtime, None)
                    .await
                {
                    Ok(()) => error.into(),
                    Err(rollback_error) => rollback_error,
                };
                return self.fail_started_operation(user_id, operation, failure).await;
            }
        };
        activation.commit();
        runtime.finalize().await;
        let record = self
            .repo
            .get(user_id, asset_id)
            .await?
            .ok_or_else(|| AssetError::NotFound(asset_id.into()))?;
        let upstream = self.repo.get_upstream(user_id, asset_id).await?;
        let base = self.repo.latest_snapshot(user_id, asset_id).await?;
        Ok(AssetRestoreResponse {
            asset: self.summary_from_parts(user_id, record, upstream, base).await?,
            operation: operation_response(stored)?,
            recovered_digest: recovered.digest,
        })
    }

    async fn validate_remote_identity(
        &self,
        user_id: &str,
        asset_id: &str,
        input: &TrackedAssetInput,
    ) -> Result<(), AssetError> {
        if input.local.id != asset_id {
            return Err(AssetError::UpstreamMismatch);
        }
        let upstream = self
            .repo
            .get_upstream(user_id, asset_id)
            .await?
            .ok_or(AssetError::UpstreamMismatch)?;
        if upstream.tracking_mode != "tracked"
            || upstream.package_name != input.package_name
            || upstream.remote_asset_id != input.remote_asset_id
        {
            return Err(AssetError::UpstreamMismatch);
        }
        Ok(())
    }

    async fn load_three_way(
        &self,
        user_id: &str,
        asset_id: &str,
        remote_digest: &str,
    ) -> Result<LoadedThreeWay, AssetError> {
        let record = self
            .repo
            .get(user_id, asset_id)
            .await?
            .ok_or_else(|| AssetError::NotFound(asset_id.into()))?;
        let base = self
            .repo
            .latest_snapshot(user_id, asset_id)
            .await?
            .ok_or(AssetError::MissingBaseSnapshot)?;
        let (local_files, local) = load_definition(&self.store.workspace_path(&record.workspace_key)?)?;
        let base_root = self.store.object_path(&base.object_key)?;
        if !base_root.is_dir() {
            return Err(AssetError::SourceUnavailable("base".into()));
        }
        let (base_files, base_scanned) = load_definition(&base_root)?;
        if base_scanned.digest != base.base_digest {
            return Err(AssetError::CorruptObject(base_root));
        }
        let remote_key = remote_digest
            .strip_prefix("sha256-")
            .ok_or_else(|| AssetError::InvalidMetadata("远程摘要必须使用 sha256- 前缀".into()))?;
        let remote_root = self.store.object_path(remote_key)?;
        if !remote_root.is_dir() {
            return Err(AssetError::SourceUnavailable("remote".into()));
        }
        let (remote_files, remote) = load_definition(&remote_root)?;
        if remote.digest != remote_digest {
            return Err(AssetError::CorruptObject(remote_root));
        }
        Ok(LoadedThreeWay {
            record,
            local_files,
            local,
            base_files,
            base_scanned,
            remote_files,
            remote,
        })
    }

    pub async fn uninstall(
        &self,
        user_id: &str,
        asset_id: &str,
        idempotency_key: &str,
    ) -> Result<AssetOperationResponse, AssetError> {
        if let Some(existing) = self.repo.get_operation_by_idempotency(user_id, idempotency_key).await? {
            return operation_response(existing);
        }
        let record = self
            .repo
            .get(user_id, asset_id)
            .await?
            .ok_or_else(|| AssetError::NotFound(asset_id.into()))?;
        if record.user_id != user_id || parse_scope(&record.scope)? == AssetScope::System {
            return Err(AssetError::InvalidState("系统资产不能卸载".into()));
        }
        let upstream = self.repo.get_upstream(user_id, asset_id).await?;
        let mut records = if let Some(upstream) = upstream {
            self.package_member_records(user_id, &upstream.package_name).await?
        } else {
            vec![record]
        };
        records.sort_by(|left, right| left.id.cmp(&right.id));
        if records.is_empty() || !records.iter().any(|member| member.id == asset_id) {
            return Err(AssetError::BundleInvariant("找不到完整的本地 Bundle".into()));
        }
        if records
            .iter()
            .any(|member| member.user_id != user_id || member.scope == "system")
        {
            return Err(AssetError::InvalidState("系统资产不能卸载".into()));
        }
        let mut _locks = Vec::with_capacity(records.len());
        for member in &records {
            _locks.push(self.store.lock_asset(user_id, &member.id)?);
        }
        let operation_id = Uuid::now_v7().to_string();
        let started_at = now_ms();
        let recovery_json = serde_json::to_string(&serde_json::json!({
            "assetIds": records.iter().map(|member| member.id.as_str()).collect::<Vec<_>>(),
            "workspaceKeys": records.iter().map(|member| member.workspace_key.as_str()).collect::<Vec<_>>(),
        }))?;
        let operation = self
            .repo
            .start_operation(StartAssetOperationParams {
                user_id,
                operation_id: &operation_id,
                idempotency_key,
                asset_id,
                kind: "uninstall",
                phase: "staging",
                recovery_json: &recovery_json,
                started_at,
            })
            .await?;
        let mut runtime_definitions = Vec::with_capacity(records.len());
        for member in &records {
            runtime_definitions.push(runtime_definition_from_record(&self.store, user_id, member)?);
        }
        let mut runtime = match self.prepare_remove_for_bound(user_id, runtime_definitions).await {
            Ok(runtime) => runtime,
            Err(error) => return self.finish_operation(user_id, operation, Err(error)).await,
        };
        let mut removals = Vec::with_capacity(records.len());
        for member in &records {
            match self.store.deactivate_workspace(&member.workspace_key) {
                Ok(removal) => removals.push(removal),
                Err(error) => return self.finish_operation(user_id, operation, Err(error)).await,
            }
        }
        if let Err(error) = runtime.apply().await {
            let _ = runtime.rollback().await;
            return self.finish_operation(user_id, operation, Err(error)).await;
        }
        let asset_ids = records.iter().map(|member| member.id.clone()).collect::<Vec<_>>();
        match self
            .repo
            .commit_uninstall_assets(user_id, &asset_ids, &operation.operation_id, asset_id, now_ms())
            .await
        {
            Ok(stored) => {
                for removal in removals {
                    removal.commit();
                }
                runtime.finalize().await;
                tracing::info!(
                    asset_id,
                    asset_count = asset_ids.len(),
                    operation_id = stored.operation_id,
                    "atomic asset package uninstalled"
                );
                operation_response(stored)
            }
            Err(error) => {
                if let Err(rollback_error) = runtime.rollback().await {
                    tracing::error!(
                        asset_id,
                        error = %rollback_error,
                        "runtime projection rollback failed after catalog uninstall failure"
                    );
                    return self
                        .finish_operation(
                            user_id,
                            operation,
                            Err(AssetError::RuntimeProjectionFailed {
                                code: "RUNTIME_ROLLBACK_FAILED",
                                message: "卸载事务回滚运行时失败".into(),
                            }),
                        )
                        .await;
                }
                drop(removals);
                self.finish_operation(user_id, operation, Err(error.into())).await
            }
        }
    }

    pub async fn detach(&self, user_id: &str, asset_id: &str) -> Result<AssetSummaryResponse, AssetError> {
        let records = self.tracked_package_records(user_id, asset_id).await?;
        let mut locks = Vec::with_capacity(records.len());
        for member in &records {
            locks.push(self.store.lock_asset(user_id, &member.id)?);
        }
        let asset_ids = records.iter().map(|record| record.id.clone()).collect::<Vec<_>>();
        self.repo.detach_assets(user_id, &asset_ids, now_ms()).await?;
        let record = self
            .repo
            .get(user_id, asset_id)
            .await?
            .ok_or_else(|| AssetError::NotFound(asset_id.into()))?;
        let upstream = self.repo.get_upstream(user_id, asset_id).await?;
        let base = self.repo.latest_snapshot(user_id, asset_id).await?;
        self.summary_from_parts(user_id, record, upstream, base).await
    }

    fn prepare_tracked_asset(
        &self,
        user_id: &str,
        input: TrackedAssetInput,
    ) -> Result<PreparedTrackedAsset, AssetError> {
        let (files, scanned) = prepare_definition(input.local.files.clone())?;
        if scanned.digest != input.remote_digest {
            return Err(AssetError::DigestMismatch {
                expected: input.remote_digest,
                actual: scanned.digest,
            });
        }
        validate_entry_file(input.local.kind, input.local.entry_file.as_deref(), &scanned.files)?;
        validate_typed_definition(
            input.local.kind,
            input.local.entry_file.as_deref(),
            input.local.runtime_id.as_deref(),
            &files,
        )?;
        let runtime_id = input
            .local
            .runtime_id
            .as_deref()
            .ok_or_else(|| AssetError::InvalidMetadata("远程核心资产缺少 runtimeId".into()))?;
        validate_runtime_id(runtime_id)?;
        let (object_key, _) = self.store.ensure_object(files.clone(), &scanned.digest)?;
        let workspace_key = self.store.workspace_key(user_id, &input.local.id);
        let manifest_json = serde_json::to_string(&scanned.files)?;
        Ok(PreparedTrackedAsset {
            input,
            files,
            scanned,
            object_key,
            workspace_key,
            manifest_json,
        })
    }

    async fn package_member_records(
        &self,
        user_id: &str,
        package_name: &str,
    ) -> Result<Vec<AssetRecordRow>, AssetError> {
        let mut members = Vec::new();
        for record in self.repo.list(user_id, None).await? {
            if record.user_id != user_id {
                continue;
            }
            if self
                .repo
                .get_upstream(user_id, &record.id)
                .await?
                .is_some_and(|upstream| upstream.package_name == package_name)
            {
                members.push(record);
            }
        }
        members.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(members)
    }

    async fn tracked_package_records(&self, user_id: &str, asset_id: &str) -> Result<Vec<AssetRecordRow>, AssetError> {
        let target = self
            .repo
            .get(user_id, asset_id)
            .await?
            .ok_or_else(|| AssetError::NotFound(asset_id.into()))?;
        if target.user_id != user_id || parse_scope(&target.scope)? == AssetScope::System {
            return Err(AssetError::InvalidState("系统资产不能解除远程跟踪".into()));
        }
        let target_upstream = self
            .repo
            .get_upstream(user_id, asset_id)
            .await?
            .ok_or(AssetError::UpstreamMismatch)?;
        if target_upstream.tracking_mode != "tracked" {
            return Err(AssetError::InvalidState("资产未处于远程跟踪状态".into()));
        }

        let records = self
            .package_member_records(user_id, &target_upstream.package_name)
            .await?;
        if records.is_empty() || !records.iter().any(|record| record.id == asset_id) {
            return Err(AssetError::BundleInvariant("找不到完整的本地 Bundle".into()));
        }
        for record in &records {
            if record.user_id != user_id || parse_scope(&record.scope)? == AssetScope::System {
                return Err(AssetError::InvalidState("系统资产不能解除远程跟踪".into()));
            }
            let upstream = self
                .repo
                .get_upstream(user_id, &record.id)
                .await?
                .ok_or_else(|| AssetError::BundleInvariant(format!("Bundle 成员 {} 缺少上游", record.id)))?;
            if upstream.package_name != target_upstream.package_name || upstream.tracking_mode != "tracked" {
                return Err(AssetError::BundleInvariant(format!(
                    "Bundle 成员 {} 的远程跟踪关系不一致",
                    record.id
                )));
            }
            if self.repo.latest_snapshot(user_id, &record.id).await?.is_none() {
                return Err(AssetError::BundleInvariant(format!(
                    "Bundle 成员 {} 缺少完整 Base",
                    record.id
                )));
            }
        }
        Ok(records)
    }

    async fn runtime_definition_from_catalog_record(
        &self,
        user_id: &str,
        record: &AssetRecordRow,
        files: Vec<AssetDefinitionFile>,
    ) -> Result<RuntimeAssetDefinition, AssetError> {
        let runtime_configuration = self.resolved_runtime_configuration(user_id, record, &files).await?;
        let mut definition = runtime_definition_from_record_and_files(&self.store, user_id, record, files)?;
        definition.dependency_portable_runtime_ids = self.installed_asset_runtime_ids(user_id).await?;
        definition.dependency_projection_runtime_ids = self.installed_asset_projection_ids(user_id).await?;
        definition.runtime_configuration = runtime_configuration;
        Ok(definition)
    }

    async fn runtime_definitions_for_prepared(
        &self,
        user_id: &str,
        assets: &[PreparedTrackedAsset],
    ) -> Result<Vec<RuntimeAssetDefinition>, AssetError> {
        let installed = self.installed_asset_runtime_ids(user_id).await?;
        let mut installed_projections = self.installed_asset_projection_ids(user_id).await?;
        for asset in assets {
            let projection_runtime_id =
                derive_projection_runtime_id(user_id, user_id, &asset.input.local.id, asset.input.local.kind)?;
            installed_projections.insert(asset.input.local.id.clone(), projection_runtime_id.clone());
            installed_projections.insert(asset.input.remote_asset_id.clone(), projection_runtime_id);
        }
        let mut definitions = Vec::with_capacity(assets.len());
        for asset in assets {
            let runtime_configuration = match self.repo.get(user_id, &asset.input.local.id).await? {
                Some(record) => {
                    self.resolved_runtime_configuration(user_id, &record, &asset.files)
                        .await?
                }
                None => None,
            };
            let mut definition = runtime_definition_from_local(
                &self.store,
                user_id,
                user_id,
                &asset.input.local,
                &asset.workspace_key,
                asset.files.clone(),
                runtime_configuration,
            )?;
            for (asset_id, runtime_id) in &installed {
                definition
                    .dependency_portable_runtime_ids
                    .entry(asset_id.clone())
                    .or_insert_with(|| runtime_id.clone());
            }
            definition.dependency_projection_runtime_ids = installed_projections.clone();
            definitions.push(definition);
        }
        Ok(definitions)
    }

    async fn resolved_runtime_configuration(
        &self,
        user_id: &str,
        record: &AssetRecordRow,
        files: &[AssetDefinitionFile],
    ) -> Result<Option<RuntimeResolvedConfiguration>, AssetError> {
        let kind = parse_kind(&record.kind)?;
        let Some(overlay) = self.repo.get_overlay(user_id, &record.id).await? else {
            return match kind {
                AssetKind::Assistant | AssetKind::Skill => Ok(None),
                AssetKind::EngineAdapter | AssetKind::Mcp => Err(AssetError::OverlayNotConfigured),
            };
        };
        let configuration: AssetPublicConfiguration = serde_json::from_str(&overlay.overlay_json)?;
        if configuration.kind() != kind {
            return Err(AssetError::InvalidMetadata("Overlay 内容类型与资产类型不一致".into()));
        }
        validate_public_configuration(&configuration)?;
        self.validate_asset_references(user_id, &configuration).await?;
        let credentials = self.repo.list_credentials(user_id, &record.id).await?;
        let configured_slots = credentials
            .iter()
            .map(|credential| credential.slot.clone())
            .collect::<BTreeSet<_>>();
        let schema = configuration_schema_for(kind, record.entry_file.as_deref(), files)?;
        validate_configuration_schema_values(&configuration, schema.as_ref(), &configured_slots)?;
        let required_slots = referenced_secret_slots(&configuration);
        let mut secrets = BTreeMap::new();
        if !required_slots.is_empty() {
            let master_key = self
                .credential_master_key
                .ok_or_else(|| AssetError::InvalidState("Core 未配置资产凭据解密密钥".into()))?;
            let by_slot = credentials
                .iter()
                .map(|credential| (credential.slot.as_str(), credential))
                .collect::<BTreeMap<_, _>>();
            for slot in required_slots {
                let credential = by_slot
                    .get(slot.as_str())
                    .ok_or_else(|| AssetError::InvalidMetadata(format!("凭据槽 {slot} 尚未配置")))?;
                if credential.key_version != ASSET_CREDENTIAL_KEY_VERSION {
                    return Err(AssetError::InvalidState(format!(
                        "凭据槽 {slot} 使用不受支持的密钥版本"
                    )));
                }
                let key = derive_asset_credential_key(&master_key, user_id, &record.id, &slot, credential.key_version);
                let plaintext = decrypt_string(&credential.ciphertext, &key)?;
                secrets.insert(slot, plaintext);
            }
        }
        Ok(Some(RuntimeResolvedConfiguration {
            configuration,
            configuration_schema: schema.unwrap_or_default(),
            secrets,
        }))
    }

    async fn installed_asset_runtime_ids(&self, user_id: &str) -> Result<BTreeMap<String, String>, AssetError> {
        let mut resolved = BTreeMap::new();
        let mut runtime_owners = BTreeMap::<(String, String), String>::new();
        for record in self.repo.list(user_id, None).await? {
            let Some(runtime_id) = record.runtime_id.as_ref() else {
                continue;
            };
            validate_runtime_id(runtime_id)?;
            if let Some(previous_owner) =
                runtime_owners.insert((record.kind.clone(), runtime_id.clone()), record.id.clone())
                && previous_owner != record.id
            {
                return Err(AssetError::BundleInvariant(format!(
                    "多个同类已安装资产映射到同一 runtimeId {runtime_id}"
                )));
            }
            if let Some(previous_runtime) = resolved.insert(record.id.clone(), runtime_id.clone())
                && previous_runtime != *runtime_id
            {
                return Err(AssetError::BundleInvariant(format!(
                    "资产 {} 的 runtimeId 映射不唯一",
                    record.id
                )));
            }
            if let Some(upstream) = self.repo.get_upstream(user_id, &record.id).await?
                && let Some(previous_runtime) = resolved.insert(upstream.remote_asset_id.clone(), runtime_id.clone())
                && previous_runtime != *runtime_id
            {
                return Err(AssetError::BundleInvariant(format!(
                    "技能资产 {} 的 runtimeId 映射不唯一",
                    upstream.remote_asset_id
                )));
            }
        }
        Ok(resolved)
    }

    async fn installed_asset_projection_ids(&self, user_id: &str) -> Result<BTreeMap<String, String>, AssetError> {
        let mut resolved = BTreeMap::new();
        for record in self.repo.list(user_id, None).await? {
            if record.runtime_id.is_none() {
                continue;
            }
            let projection_runtime_id =
                derive_projection_runtime_id(user_id, &record.user_id, &record.id, parse_kind(&record.kind)?)?;
            if let Some(previous) = resolved.insert(record.id.clone(), projection_runtime_id.clone())
                && previous != projection_runtime_id
            {
                return Err(AssetError::BundleInvariant(format!(
                    "资产 {} 的投影身份映射不唯一",
                    record.id
                )));
            }
            if let Some(upstream) = self.repo.get_upstream(user_id, &record.id).await?
                && let Some(previous) = resolved.insert(upstream.remote_asset_id.clone(), projection_runtime_id.clone())
                && previous != projection_runtime_id
            {
                return Err(AssetError::BundleInvariant(format!(
                    "远程资产 {} 的投影身份映射不唯一",
                    upstream.remote_asset_id
                )));
            }
        }
        Ok(resolved)
    }

    async fn fail_started_operation<T>(
        &self,
        user_id: &str,
        operation: AssetOperationRow,
        error: AssetError,
    ) -> Result<T, AssetError> {
        match self.finish_operation(user_id, operation, Err(error)).await {
            Err(error) => Err(error),
            Ok(_) => Err(AssetError::InvalidState("失败的资产操作被错误地标记为成功".into())),
        }
    }

    async fn finish_operation(
        &self,
        user_id: &str,
        operation: AssetOperationRow,
        result: Result<(), AssetError>,
    ) -> Result<AssetOperationResponse, AssetError> {
        let now = now_ms();
        match result {
            Ok(()) => {
                let stored = self
                    .repo
                    .update_operation(
                        user_id,
                        &operation.operation_id,
                        UpdateAssetOperationParams {
                            state: "succeeded",
                            phase: "complete",
                            error_code: None,
                            recovery_json: "{}",
                            finished_at: Some(now),
                            updated_at: now,
                        },
                    )
                    .await?
                    .ok_or_else(|| AssetError::NotFound(operation.operation_id))?;
                operation_response(stored)
            }
            Err(error) => {
                let error_code = public_error_code(&error);
                let _ = self
                    .repo
                    .update_operation(
                        user_id,
                        &operation.operation_id,
                        UpdateAssetOperationParams {
                            state: "failed",
                            phase: "rolled-back",
                            error_code: Some(error_code),
                            recovery_json: "{}",
                            finished_at: Some(now),
                            updated_at: now,
                        },
                    )
                    .await;
                Err(error)
            }
        }
    }
}

#[async_trait::async_trait]
impl RuntimeAssetConfigurationResolver for AssetCatalogService {
    async fn resolve(
        &self,
        user_id: &str,
        local_asset_id: &str,
    ) -> Result<Option<RuntimeResolvedConfiguration>, AssetError> {
        let record = self
            .repo
            .get(user_id, local_asset_id)
            .await?
            .ok_or_else(|| AssetError::NotFound(local_asset_id.into()))?;
        let files = load_definition(&self.store.workspace_path(&record.workspace_key)?)?.0;
        self.resolved_runtime_configuration(user_id, &record, &files).await
    }
}

fn validate_new_asset_identity(id: &str, display_name: &str) -> Result<(), AssetError> {
    if id.is_empty() || id.len() > 128 {
        return Err(AssetError::InvalidMetadata("资产 ID 长度无效".into()));
    }
    let mut after_separator = true;
    for byte in id.bytes() {
        let alphanumeric = byte.is_ascii_lowercase() || byte.is_ascii_digit();
        if after_separator {
            if !alphanumeric {
                return Err(AssetError::InvalidMetadata("资产 ID 必须使用小写可移植标识符".into()));
            }
            after_separator = false;
        } else if matches!(byte, b'.' | b'_' | b':' | b'-') {
            after_separator = true;
        } else if !alphanumeric {
            return Err(AssetError::InvalidMetadata("资产 ID 必须使用小写可移植标识符".into()));
        }
    }
    if after_separator {
        return Err(AssetError::InvalidMetadata("资产 ID 不能以分隔符结尾".into()));
    }
    validate_overlay_text(display_name, 128, "资产名称")
}

fn safe_template_input(
    id: String,
    kind: AssetKind,
    display_name: String,
    description: Option<String>,
    runtime_id: String,
) -> Result<LocalAssetInput, AssetError> {
    let description_json = description.as_ref().map_or(serde_json::Value::Null, |value| {
        serde_json::Value::String(value.clone())
    });
    let (entry_file, files) = match kind {
        AssetKind::Assistant => {
            let definition = serde_json::json!({
                "$schema": "https://raw.githubusercontent.com/liangboqiang/TjuaeCore/main/schemas/local-assistant-definition.v1.schema.json",
                "schemaVersion": 1,
                "kind": "assistant",
                "runtimeId": runtime_id,
                "name": display_name,
                "nameI18n": {},
                "description": description_json,
                "descriptionI18n": {},
                "rules": {"zh-CN": "rules/zh-CN.md"},
                "recommendedPrompts": [],
                "recommendedPromptsI18n": {},
                "skillDependencies": [],
                "avatar": {"type": "none"}
            });
            let descriptor = serde_json::json!({
                "$schema": "tjuae://schemas/local-asset-descriptor.v1",
                "schemaVersion": 1,
                "kind": "assistant",
                "assetId": id,
                "contributionKey": "assistants",
                "contribution": {
                    "id": runtime_id,
                    "runtimeId": runtime_id,
                    "name": display_name,
                    "description": description.clone().unwrap_or_default(),
                    "file": "assistant.local.json",
                    "dependencies": []
                }
            });
            (
                Some("assistant.local.json".into()),
                vec![
                    AssetDefinitionFile::text("assistant.local.json", serde_json::to_string_pretty(&definition)?),
                    AssetDefinitionFile::text("tjuae.asset.json", serde_json::to_string_pretty(&descriptor)?),
                    AssetDefinitionFile::text("rules/zh-CN.md", "# 助手规则\n\n请在此编写助手的系统规则。\n"),
                ],
            )
        }
        AssetKind::EngineAdapter => {
            let definition = serde_json::json!({
                "$schema": crate::ENGINE_ADAPTER_DEFINITION_SCHEMA_URL,
                "schemaVersion": 1,
                "kind": "engineAdapter",
                "id": id,
                "runtimeId": runtime_id,
                "displayName": display_name,
                "description": description_json,
                "protocol": {
                    "type": "acp",
                    "transport": "stdio",
                    "arguments": []
                },
                "runtime": {
                    "commandName": "tjuae-adapter"
                },
                "configurationSchema": {
                    "fields": []
                }
            });
            (
                Some("engine-adapter.json".into()),
                vec![AssetDefinitionFile::text(
                    "engine-adapter.json",
                    serde_json::to_string_pretty(&definition)?,
                )],
            )
        }
        AssetKind::Skill => {
            let name = serde_json::to_string(&id)?;
            let description_text = description.clone().unwrap_or_else(|| format!("{display_name} 技能。"));
            let description_yaml = serde_json::to_string(&description_text)?;
            let content = format!(
                "---\nname: {name}\ndescription: {description_yaml}\n---\n\n# {display_name}\n\n请在此编写技能说明和执行步骤。\n"
            );
            (
                Some("SKILL.md".into()),
                vec![AssetDefinitionFile::text("SKILL.md", content)],
            )
        }
        AssetKind::Mcp => {
            let definition = serde_json::json!({
                "$schema": crate::MCP_DEFINITION_SCHEMA_URL,
                "schemaVersion": 1,
                "kind": "mcp",
                "id": id,
                "runtimeId": runtime_id,
                "displayName": display_name,
                "description": description_json,
                "transport": {
                    "type": "streamableHttp"
                },
                "capabilities": {
                    "tools": true,
                    "resources": false,
                    "prompts": false,
                    "sampling": false,
                    "logging": false,
                    "completions": false
                },
                "configurationSchema": {
                    "fields": []
                }
            });
            (
                Some("mcp.json".into()),
                vec![AssetDefinitionFile::text(
                    "mcp.json",
                    serde_json::to_string_pretty(&definition)?,
                )],
            )
        }
    };
    Ok(LocalAssetInput {
        id,
        kind,
        display_name,
        description,
        origin: AssetOrigin::Local,
        trust: AssetTrust::Community,
        scope: AssetScope::User,
        editability: AssetEditability::Full,
        entry_file,
        runtime_id: Some(runtime_id),
        files,
        dependency_runtime_ids: BTreeMap::new(),
    })
}

#[allow(clippy::too_many_arguments)]
fn rewrite_duplicated_definition(
    kind: AssetKind,
    entry_file: Option<&str>,
    mut files: Vec<AssetDefinitionFile>,
    asset_id: &str,
    runtime_id: &str,
    display_name: &str,
    description: Option<&str>,
) -> Result<Vec<AssetDefinitionFile>, AssetError> {
    let entry_file = runtime_entry_file(kind, entry_file)?;
    let entry = files
        .iter_mut()
        .find(|file| file.path == entry_file)
        .ok_or_else(|| AssetError::InvalidMetadata("复制源缺少入口文件".into()))?;
    if kind == AssetKind::Skill {
        let text = std::str::from_utf8(&entry.content).map_err(|_| AssetError::BinaryFile(entry_file.clone()))?;
        let body = strip_skill_front_matter(text);
        let name = serde_json::to_string(asset_id)?;
        let description = serde_json::to_string(description.unwrap_or(display_name))?;
        entry.content = format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}").into_bytes();
        return Ok(files);
    }

    let mut definition: serde_json::Value = serde_json::from_slice(&entry.content)?;
    let object = definition
        .as_object_mut()
        .ok_or_else(|| AssetError::InvalidMetadata("复制源入口必须是 JSON 对象".into()))?;
    if object.contains_key("id") {
        object.insert("id".into(), asset_id.into());
    }
    object.insert("runtimeId".into(), runtime_id.into());
    if object.contains_key("displayName") {
        object.insert("displayName".into(), display_name.into());
    } else if object.contains_key("name") {
        object.insert("name".into(), display_name.into());
    }
    if let Some(description) = description {
        object.insert("description".into(), description.into());
    }
    entry.content = serde_json::to_vec_pretty(&definition)?;

    if kind == AssetKind::Assistant
        && let Some(descriptor) = files.iter_mut().find(|file| file.path == "tjuae.asset.json")
    {
        let mut value: serde_json::Value = serde_json::from_slice(&descriptor.content)?;
        if let Some(object) = value.as_object_mut() {
            object.insert("assetId".into(), asset_id.into());
            if let Some(contribution) = object
                .get_mut("contribution")
                .and_then(serde_json::Value::as_object_mut)
            {
                contribution.insert("id".into(), runtime_id.into());
                contribution.insert("runtimeId".into(), runtime_id.into());
                contribution.insert("name".into(), display_name.into());
                if let Some(description) = description {
                    contribution.insert("description".into(), description.into());
                }
            }
        }
        descriptor.content = serde_json::to_vec_pretty(&value)?;
    }
    Ok(files)
}

fn strip_skill_front_matter(content: &str) -> &str {
    let Some(rest) = content.strip_prefix("---") else {
        return content;
    };
    let Some(end) = rest.find("\n---") else {
        return content;
    };
    rest[end + 4..].trim_start_matches(['\r', '\n'])
}

fn validate_bundle_inputs(inputs: &[TrackedAssetInput]) -> Result<(), AssetError> {
    let Some(first) = inputs.first() else {
        return Err(AssetError::BundleInvariant("Bundle 不能为空".into()));
    };
    let mut local_ids = BTreeSet::new();
    let mut remote_ids = BTreeSet::new();
    for input in inputs {
        if input.package_name != first.package_name
            || input.version != first.version
            || input.source_revision != first.source_revision
        {
            return Err(AssetError::BundleInvariant(
                "Bundle 成员必须来自同一包、版本和固定 revision".into(),
            ));
        }
        if !local_ids.insert(input.local.id.as_str()) || !remote_ids.insert(input.remote_asset_id.as_str()) {
            return Err(AssetError::BundleInvariant("Bundle 包含重复资产 ID".into()));
        }
    }
    Ok(())
}

fn validate_closure_inputs(inputs: &[TrackedAssetInput]) -> Result<(), AssetError> {
    let Some(first) = inputs.first() else {
        return Err(AssetError::BundleInvariant("依赖闭包不能为空".into()));
    };
    let mut local_ids = BTreeSet::new();
    let mut remote_ids = BTreeSet::new();
    let mut packages = BTreeMap::<&str, (&str, &str)>::new();
    for input in inputs {
        if input.source_revision != first.source_revision {
            return Err(AssetError::BundleInvariant(
                "依赖闭包成员必须来自同一固定 revision".into(),
            ));
        }
        match packages.get(input.package_name.as_str()) {
            Some((version, revision))
                if *version != input.version.as_str() || *revision != input.source_revision.as_str() =>
            {
                return Err(AssetError::BundleInvariant(format!(
                    "原子包 {} 的成员版本或 revision 不一致",
                    input.package_name
                )));
            }
            None => {
                packages.insert(
                    input.package_name.as_str(),
                    (input.version.as_str(), input.source_revision.as_str()),
                );
            }
            _ => {}
        }
        if !local_ids.insert(input.local.id.as_str()) || !remote_ids.insert(input.remote_asset_id.as_str()) {
            return Err(AssetError::BundleInvariant("依赖闭包包含重复资产 ID".into()));
        }
    }
    Ok(())
}

fn closure_package_names(inputs: &[TrackedAssetInput]) -> Vec<String> {
    inputs
        .iter()
        .map(|input| input.package_name.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn bundle_recovery_json(inputs: &[TrackedAssetInput]) -> Result<String, AssetError> {
    Ok(serde_json::to_string(&serde_json::json!({
        "packageNames": closure_package_names(inputs),
        "assetIds": inputs.iter().map(|input| input.local.id.as_str()).collect::<Vec<_>>(),
    }))?)
}

fn validate_runtime_id(runtime_id: &str) -> Result<(), AssetError> {
    if runtime_id.is_empty()
        || runtime_id.len() > 128
        || runtime_id == "."
        || runtime_id == ".."
        || runtime_id.contains('/')
        || runtime_id.contains('\\')
        || runtime_id.contains('\0')
        || !runtime_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(AssetError::InvalidMetadata("runtimeId 不安全".into()));
    }
    Ok(())
}

fn runtime_definition_from_local(
    store: &AssetContentStore,
    user_id: &str,
    asset_owner_id: &str,
    input: &LocalAssetInput,
    workspace_key: &str,
    files: Vec<AssetDefinitionFile>,
    runtime_configuration: Option<RuntimeResolvedConfiguration>,
) -> Result<RuntimeAssetDefinition, AssetError> {
    let portable_runtime_id = input
        .runtime_id
        .clone()
        .ok_or_else(|| AssetError::InvalidMetadata("核心资产缺少 runtimeId".into()))?;
    validate_runtime_id(&portable_runtime_id)?;
    let projection_runtime_id = derive_projection_runtime_id(user_id, asset_owner_id, &input.id, input.kind)?;
    Ok(RuntimeAssetDefinition {
        local_asset_id: input.id.clone(),
        kind: input.kind,
        portable_runtime_id,
        projection_runtime_id,
        entry_file: runtime_entry_file(input.kind, input.entry_file.as_deref())?,
        workspace_path: store.workspace_path(workspace_key)?,
        files,
        dependency_portable_runtime_ids: input.dependency_runtime_ids.clone(),
        dependency_projection_runtime_ids: BTreeMap::new(),
        runtime_configuration,
    })
}

fn runtime_definition_from_record_and_files(
    store: &AssetContentStore,
    user_id: &str,
    record: &AssetRecordRow,
    files: Vec<AssetDefinitionFile>,
) -> Result<RuntimeAssetDefinition, AssetError> {
    let portable_runtime_id = record
        .runtime_id
        .clone()
        .ok_or_else(|| AssetError::InvalidMetadata("核心资产缺少 runtimeId".into()))?;
    validate_runtime_id(&portable_runtime_id)?;
    let kind = parse_kind(&record.kind)?;
    let projection_runtime_id = derive_projection_runtime_id(user_id, &record.user_id, &record.id, kind)?;
    Ok(RuntimeAssetDefinition {
        local_asset_id: record.id.clone(),
        kind,
        portable_runtime_id,
        projection_runtime_id,
        entry_file: runtime_entry_file(kind, record.entry_file.as_deref())?,
        workspace_path: store.workspace_path(&record.workspace_key)?,
        files,
        dependency_portable_runtime_ids: BTreeMap::new(),
        dependency_projection_runtime_ids: BTreeMap::new(),
        runtime_configuration: None,
    })
}

fn runtime_definition_from_record(
    store: &AssetContentStore,
    user_id: &str,
    record: &AssetRecordRow,
) -> Result<RuntimeAssetDefinition, AssetError> {
    let workspace_path = store.workspace_path(&record.workspace_key)?;
    let (files, _) = load_definition(&workspace_path)?;
    let mut definition = runtime_definition_from_record_and_files(store, user_id, record, files)?;
    definition.workspace_path = workspace_path;
    Ok(definition)
}

fn runtime_entry_file(kind: AssetKind, entry_file: Option<&str>) -> Result<String, AssetError> {
    match (kind, entry_file) {
        (AssetKind::Skill, None) => Ok("SKILL.md".into()),
        (_, Some(entry_file)) => normalize_relative_path(entry_file),
        (_, None) => Err(AssetError::InvalidMetadata("核心资产缺少入口文件".into())),
    }
}

fn runtime_error_requires_repair(error: &AssetError) -> bool {
    matches!(
        error,
        AssetError::RuntimeProjectionFailed { code, .. }
            | AssetError::RuntimeProjectionUnsupported { code, .. }
            if code.ends_with("_ROLLBACK_FAILED")
    )
}

pub fn calculate_sync_state(
    local_digest: &str,
    base_digest: Option<&str>,
    remote_digest: Option<&str>,
    remote_available: bool,
) -> Result<AssetSyncState, AssetError> {
    if !remote_available {
        return Ok(AssetSyncState::RemoteUnknown);
    }
    let base = base_digest.ok_or(AssetError::MissingBaseSnapshot)?;
    let remote = remote_digest.ok_or_else(|| AssetError::SourceUnavailable("remote".into()))?;
    Ok(if local_digest == base && remote == base {
        AssetSyncState::Synced
    } else if remote == base {
        AssetSyncState::LocalModified
    } else if local_digest == base || local_digest == remote {
        // L=R!=B 仍需显式同步来推进 Base，不能仅凭内容偶然相同就宣称
        // 已完成同步。
        AssetSyncState::RemoteUpdated
    } else {
        AssetSyncState::Diverged
    })
}

fn build_summary_from_parts(
    record: AssetRecordRow,
    upstream: Option<AssetUpstreamRow>,
    base: Option<AssetSnapshotRow>,
    runtime_state: AssetRuntimeState,
    has_current_try_run_receipt: bool,
) -> Result<AssetSummaryResponse, AssetError> {
    build_summary_from_parts_with_remote(
        record,
        upstream,
        base,
        &BTreeMap::new(),
        false,
        runtime_state,
        has_current_try_run_receipt,
    )
}

fn build_summary_from_parts_with_remote(
    record: AssetRecordRow,
    upstream: Option<AssetUpstreamRow>,
    base: Option<AssetSnapshotRow>,
    remote_assets: &BTreeMap<String, (String, bool)>,
    remote_available: bool,
    runtime_state: AssetRuntimeState,
    has_current_try_run_receipt: bool,
) -> Result<AssetSummaryResponse, AssetError> {
    let sync_state = sync_state_from_remote_index(
        &record,
        upstream.as_ref(),
        base.as_ref(),
        remote_assets,
        remote_available,
    )?;
    let editability = parse_editability(&record.editability)?;
    let scope = parse_scope(&record.scope)?;
    let kind = parse_kind(&record.kind)?;
    Ok(AssetSummaryResponse {
        id: record.id,
        kind,
        display_name: record.display_name,
        description: record.description,
        origin: parse_origin(&record.origin)?,
        trust: parse_trust(&record.trust)?,
        scope,
        editability,
        definition_digest: record.definition_digest,
        runtime_state,
        sync_state,
        allowed_actions: allowed_actions(
            sync_state,
            editability,
            scope,
            kind,
            runtime_state,
            has_current_try_run_receipt,
        ),
        runtime_id: record.runtime_id,
        upstream: upstream
            .map(|value| {
                Ok::<AssetUpstreamResponse, AssetError>(AssetUpstreamResponse {
                    package_name: value.package_name,
                    remote_asset_id: value.remote_asset_id,
                    version: value.version,
                    source_revision: value.source_revision,
                    remote_digest: value.remote_digest,
                    tracking_mode: parse_tracking_mode(&value.tracking_mode)?,
                    checked_at: value.checked_at,
                })
            })
            .transpose()?,
        created_at: record.created_at,
        updated_at: record.updated_at,
    })
}

fn sync_state_from_remote_index(
    record: &AssetRecordRow,
    upstream: Option<&AssetUpstreamRow>,
    base: Option<&AssetSnapshotRow>,
    remote_assets: &BTreeMap<String, (String, bool)>,
    remote_available: bool,
) -> Result<Option<AssetSyncState>, AssetError> {
    let Some(upstream) = upstream else {
        return Ok(None);
    };
    if upstream.tracking_mode != "tracked" {
        return Ok(None);
    }
    if !remote_available {
        return Ok(Some(AssetSyncState::RemoteUnknown));
    }
    let Some((remote_digest, compatible)) = remote_assets.get(&upstream.remote_asset_id) else {
        return Ok(Some(AssetSyncState::UpstreamRemoved));
    };
    if !compatible {
        return Ok(Some(AssetSyncState::Incompatible));
    }
    calculate_sync_state(
        &record.definition_digest,
        base.map(|value| value.base_digest.as_str()),
        Some(remote_digest),
        true,
    )
    .map(Some)
}

fn allowed_actions(
    state: Option<AssetSyncState>,
    editability: AssetEditability,
    scope: AssetScope,
    _kind: AssetKind,
    runtime_state: AssetRuntimeState,
    has_current_try_run_receipt: bool,
) -> Vec<AssetAction> {
    let mut actions = vec![AssetAction::View];
    if editability != AssetEditability::ReadOnly {
        actions.push(AssetAction::Configure);
    }
    if !matches!(
        runtime_state,
        AssetRuntimeState::NotConfigured | AssetRuntimeState::Activating
    ) {
        actions.push(AssetAction::Validate);
    }
    if matches!(
        runtime_state,
        AssetRuntimeState::Inactive
            | AssetRuntimeState::Active
            | AssetRuntimeState::Degraded
            | AssetRuntimeState::NeedsRepair
    ) {
        actions.push(AssetAction::TryRun);
    }
    if has_current_try_run_receipt
        && matches!(
            runtime_state,
            AssetRuntimeState::Inactive | AssetRuntimeState::Degraded | AssetRuntimeState::NeedsRepair
        )
    {
        actions.push(AssetAction::Activate);
    }
    if matches!(runtime_state, AssetRuntimeState::Active | AssetRuntimeState::Degraded) {
        actions.push(AssetAction::Deactivate);
    }
    let removable = scope != AssetScope::System;
    if editability == AssetEditability::Full {
        actions.push(AssetAction::Edit);
    }
    match state {
        None => {
            if removable {
                actions.push(AssetAction::Uninstall);
            }
            if editability == AssetEditability::Full {
                actions.push(AssetAction::Publish);
            }
        }
        Some(AssetSyncState::Synced) => {
            if removable {
                actions.extend([AssetAction::Uninstall, AssetAction::Detach]);
            }
            if editability == AssetEditability::Full {
                actions.push(AssetAction::Publish);
            }
        }
        Some(AssetSyncState::LocalModified) => {
            actions.push(AssetAction::ViewDiff);
            if removable {
                actions.extend([AssetAction::Uninstall, AssetAction::Detach]);
            }
            if editability == AssetEditability::Full {
                actions.push(AssetAction::Publish);
            }
        }
        Some(AssetSyncState::RemoteUpdated) => {
            actions.push(AssetAction::ViewDiff);
            if removable {
                actions.extend([AssetAction::Sync, AssetAction::Uninstall, AssetAction::Detach]);
            }
        }
        Some(AssetSyncState::Diverged | AssetSyncState::Conflict) => {
            actions.push(AssetAction::ViewDiff);
            if removable {
                actions.extend([AssetAction::Uninstall, AssetAction::Detach]);
                if editability == AssetEditability::Full {
                    actions.push(AssetAction::ResolveConflict);
                }
            }
        }
        Some(AssetSyncState::RemoteUnknown) => {
            actions.push(AssetAction::ViewDiff);
            if removable {
                actions.extend([AssetAction::Uninstall, AssetAction::Detach]);
            }
        }
        Some(AssetSyncState::UpstreamRemoved | AssetSyncState::Incompatible | AssetSyncState::Revoked) => {
            actions.push(AssetAction::ViewDiff);
            if removable {
                actions.extend([AssetAction::Uninstall, AssetAction::Detach]);
            }
        }
    }
    actions
}

fn record_params_from_row<'a>(
    user_id: &'a str,
    record: &'a AssetRecordRow,
    definition_digest: &'a str,
    now: i64,
) -> UpsertAssetRecordParams<'a> {
    UpsertAssetRecordParams {
        user_id,
        id: &record.id,
        kind: &record.kind,
        display_name: &record.display_name,
        description: record.description.as_deref(),
        origin: &record.origin,
        trust: &record.trust,
        scope: &record.scope,
        editability: &record.editability,
        workspace_key: &record.workspace_key,
        definition_digest,
        entry_file: record.entry_file.as_deref(),
        runtime_id: record.runtime_id.as_deref(),
        now,
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecoveryMetadata {
    kind: String,
    object_key: String,
    digest: String,
}

fn recovery_metadata(value: &str) -> Result<Option<(String, String)>, AssetError> {
    let raw: serde_json::Value = serde_json::from_str(value)?;
    if raw.as_object().is_some_and(serde_json::Map::is_empty) {
        return Ok(None);
    }
    let metadata: RecoveryMetadata = serde_json::from_value(raw)?;
    if metadata.kind != "useRemote"
        || !metadata.digest.starts_with("sha256-")
        || metadata.object_key.len() != 64
        || !metadata.object_key.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(AssetError::InvalidMetadata("恢复快照元数据无效".into()));
    }
    Ok(Some((metadata.object_key, metadata.digest)))
}

fn record_params<'a>(
    user_id: &'a str,
    input: &'a LocalAssetInput,
    workspace_key: &'a str,
    digest: &'a str,
    now: i64,
) -> UpsertAssetRecordParams<'a> {
    UpsertAssetRecordParams {
        user_id,
        id: &input.id,
        kind: kind_to_db(input.kind),
        display_name: &input.display_name,
        description: input.description.as_deref(),
        origin: origin_to_db(input.origin),
        trust: trust_to_db(input.trust),
        scope: scope_to_db(input.scope),
        editability: editability_to_db(input.editability),
        workspace_key,
        definition_digest: digest,
        entry_file: input.entry_file.as_deref(),
        runtime_id: input.runtime_id.as_deref(),
        now,
    }
}

fn operation_response(row: AssetOperationRow) -> Result<AssetOperationResponse, AssetError> {
    Ok(AssetOperationResponse {
        operation_id: row.operation_id,
        idempotency_key: row.idempotency_key,
        asset_id: row.asset_id,
        kind: match row.kind.as_str() {
            "install" => AssetOperationKind::Install,
            "uninstall" => AssetOperationKind::Uninstall,
            "sync" => AssetOperationKind::Sync,
            "resolve" => AssetOperationKind::Resolve,
            "detach" => AssetOperationKind::Detach,
            "restore" => AssetOperationKind::Restore,
            value => return Err(AssetError::InvalidMetadata(format!("未知操作类型：{value}"))),
        },
        state: match row.state.as_str() {
            "queued" => AssetOperationState::Queued,
            "running" => AssetOperationState::Running,
            "succeeded" => AssetOperationState::Succeeded,
            "failed" => AssetOperationState::Failed,
            "rolledBack" | "rolled_back" | "rolled-back" => AssetOperationState::RolledBack,
            value => return Err(AssetError::InvalidMetadata(format!("未知操作状态：{value}"))),
        },
        phase: row.phase,
        error_code: row.error_code,
        started_at: row.started_at,
        finished_at: row.finished_at,
    })
}

fn overlay_response(
    record: &AssetRecordRow,
    row: AssetOverlayRow,
    credentials: &[AssetCredentialRow],
) -> Result<AssetOverlayResponse, AssetError> {
    let kind = parse_kind(&record.kind)?;
    if row.asset_id != record.id || parse_kind(&row.kind)? != kind {
        return Err(AssetError::InvalidMetadata("Overlay 与本地资产身份不一致".into()));
    }
    let configuration: AssetPublicConfiguration = serde_json::from_str(&row.overlay_json)?;
    if configuration.kind() != kind {
        return Err(AssetError::InvalidMetadata(
            "Overlay 内容类型与本地资产类型不一致".into(),
        ));
    }
    let configured = credentials
        .iter()
        .map(|credential| credential.slot.as_str())
        .collect::<BTreeSet<_>>();
    let mut slots = referenced_secret_slots(&configuration);
    slots.extend(configured.iter().map(|slot| (*slot).to_owned()));
    let secret_slots = slots
        .into_iter()
        .map(|slot| {
            let is_configured = configured.contains(slot.as_str());
            AssetSecretSlotResponse {
                slot,
                configured: is_configured,
                masked_value: is_configured.then(|| ASSET_CREDENTIAL_MASK.into()),
            }
        })
        .collect();
    Ok(AssetOverlayResponse {
        asset_id: row.asset_id,
        kind,
        configuration,
        secret_slots,
        version: row.version,
        updated_at: row.updated_at,
    })
}

fn runtime_binding_response(
    record: &AssetRecordRow,
    row: &AssetRuntimeBindingRow,
) -> Result<AssetRuntimeBindingResponse, AssetError> {
    let kind = parse_kind(&record.kind)?;
    let expected_projection_runtime_id = derive_projection_runtime_id(&row.user_id, &record.user_id, &record.id, kind)?;
    if row.asset_id != record.id
        || row.asset_owner_id != record.user_id
        || parse_kind(&row.kind)? != kind
        || record.runtime_id.as_deref() != Some(row.portable_runtime_id.as_str())
        || row.projection_runtime_id != expected_projection_runtime_id
    {
        return Err(AssetError::InvalidMetadata("运行投影绑定与本地资产身份不一致".into()));
    }
    let projection_kind = match row.projection_kind.as_str() {
        "assistant" => AssetRuntimeProjectionKind::Assistant,
        "engineAdapter" => AssetRuntimeProjectionKind::EngineAdapter,
        "skill" => AssetRuntimeProjectionKind::Skill,
        "mcp" => AssetRuntimeProjectionKind::Mcp,
        value => {
            return Err(AssetError::InvalidMetadata(format!("未知运行投影类型：{value}")));
        }
    };
    let health_status = match row.health_status.as_str() {
        "unknown" => AssetRuntimeHealthStatus::Unknown,
        "healthy" => AssetRuntimeHealthStatus::Healthy,
        "unhealthy" => AssetRuntimeHealthStatus::Unhealthy,
        value => {
            return Err(AssetError::InvalidMetadata(format!("未知运行健康状态：{value}")));
        }
    };
    Ok(AssetRuntimeBindingResponse {
        asset_id: row.asset_id.clone(),
        kind,
        projection_kind,
        portable_runtime_id: row.portable_runtime_id.clone(),
        definition_digest: row.definition_digest.clone(),
        overlay_version: row.overlay_version,
        health_status,
        try_run_receipt_id: row.try_run_receipt_id.clone(),
        last_error_code: row.last_error_code.clone(),
        projected_at: row.projected_at,
        health_checked_at: row.health_checked_at,
    })
}

fn initial_runtime_state(kind: AssetKind) -> AssetRuntimeState {
    match kind {
        AssetKind::EngineAdapter | AssetKind::Mcp => AssetRuntimeState::NotConfigured,
        AssetKind::Assistant | AssetKind::Skill => AssetRuntimeState::Inactive,
    }
}

fn parse_runtime_state(value: &str) -> Result<AssetRuntimeState, AssetError> {
    match value {
        "notConfigured" => Ok(AssetRuntimeState::NotConfigured),
        "inactive" => Ok(AssetRuntimeState::Inactive),
        "activating" => Ok(AssetRuntimeState::Activating),
        "active" => Ok(AssetRuntimeState::Active),
        "degraded" => Ok(AssetRuntimeState::Degraded),
        "needsRepair" => Ok(AssetRuntimeState::NeedsRepair),
        value => Err(AssetError::InvalidMetadata(format!("未知资产运行状态：{value}"))),
    }
}

fn validate_public_configuration(configuration: &AssetPublicConfiguration) -> Result<(), AssetError> {
    match configuration {
        AssetPublicConfiguration::Assistant(value) => {
            validate_optional_overlay_text(value.default_model_id.as_deref(), 256, "默认模型 ID")?;
            validate_optional_overlay_text(value.engine_asset_id.as_deref(), 256, "引擎资产 ID")?;
        }
        AssetPublicConfiguration::Skill(_) => {}
        AssetPublicConfiguration::EngineAdapter(value) => validate_engine_configuration(value)?,
        AssetPublicConfiguration::Mcp(value) => validate_mcp_configuration(value)?,
    }
    Ok(())
}

fn validate_engine_configuration(value: &EngineAdapterAssetConfiguration) -> Result<(), AssetError> {
    validate_optional_overlay_text(value.executable_path.as_deref(), 4096, "可执行文件路径")?;
    validate_optional_overlay_text(value.command.as_deref(), 256, "命令")?;
    validate_optional_overlay_text(value.working_directory.as_deref(), 4096, "工作目录")?;
    if value.executable_path.is_some() && value.command.is_some() {
        return Err(AssetError::InvalidMetadata(
            "引擎 Overlay 不能同时覆盖 executablePath 和 command".into(),
        ));
    }
    validate_arguments(&value.arguments)?;
    validate_named_secret_slots(&value.environment, "环境变量")?;
    validate_configuration_values(&value.values)?;
    validate_keyed_secret_slots(&value.secrets)?;
    Ok(())
}

fn validate_mcp_configuration(value: &McpAssetConfiguration) -> Result<(), AssetError> {
    validate_optional_overlay_text(value.executable_path.as_deref(), 4096, "MCP 启动器路径")?;
    validate_arguments(&value.arguments)?;
    validate_named_secret_slots(&value.environment, "环境变量")?;
    validate_named_secret_slots(&value.headers, "请求头")?;
    validate_configuration_values(&value.values)?;
    validate_keyed_secret_slots(&value.secrets)?;
    match value.transport {
        McpAssetTransport::Stdio => {
            if value.instance_url.is_some() || !value.headers.is_empty() {
                return Err(AssetError::InvalidMetadata(
                    "stdio MCP Overlay 不能包含实例 URL 或请求头".into(),
                ));
            }
        }
        McpAssetTransport::Sse | McpAssetTransport::StreamableHttp => {
            let instance_url = value
                .instance_url
                .as_deref()
                .ok_or_else(|| AssetError::InvalidMetadata("远程 MCP Overlay 缺少实例 URL".into()))?;
            let parsed =
                url::Url::parse(instance_url).map_err(|_| AssetError::InvalidMetadata("MCP 实例 URL 无效".into()))?;
            if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
                return Err(AssetError::InvalidMetadata(
                    "MCP 实例 URL 必须使用 http 或 https".into(),
                ));
            }
            if value.executable_path.is_some() || !value.arguments.is_empty() || !value.environment.is_empty() {
                return Err(AssetError::InvalidMetadata(
                    "远程 MCP Overlay 不能包含本机启动器、参数或环境变量".into(),
                ));
            }
        }
    }
    Ok(())
}

fn configuration_schema_for(
    kind: AssetKind,
    _entry_file: Option<&str>,
    files: &[AssetDefinitionFile],
) -> Result<Option<AssetConfigurationSchemaDefinition>, AssetError> {
    let schema = match kind {
        AssetKind::EngineAdapter => {
            let bytes = files
                .iter()
                .find(|file| file.path == "engine-adapter.json")
                .ok_or_else(|| AssetError::InvalidMetadata("引擎资产缺少 engine-adapter.json".into()))?;
            crate::parse_engine_adapter_definition(&bytes.content)?.configuration_schema
        }
        AssetKind::Mcp => {
            let bytes = files
                .iter()
                .find(|file| file.path == "mcp.json")
                .ok_or_else(|| AssetError::InvalidMetadata("MCP 资产缺少 mcp.json".into()))?;
            crate::parse_mcp_definition(&bytes.content)?.configuration_schema
        }
        AssetKind::Assistant | AssetKind::Skill => None,
    };
    Ok(schema)
}

fn validate_configuration_schema_values(
    configuration: &AssetPublicConfiguration,
    schema: Option<&AssetConfigurationSchemaDefinition>,
    configured_slots: &BTreeSet<String>,
) -> Result<(), AssetError> {
    let (values, secrets) = match configuration {
        AssetPublicConfiguration::EngineAdapter(value) => (&value.values, &value.secrets),
        AssetPublicConfiguration::Mcp(value) => (&value.values, &value.secrets),
        AssetPublicConfiguration::Assistant(_) | AssetPublicConfiguration::Skill(_) => {
            if !configured_slots.is_empty() {
                return Err(AssetError::InvalidMetadata(
                    "助手和技能配置不能保存未引用的凭据槽".into(),
                ));
            }
            return Ok(());
        }
    };
    let fields = schema
        .map(|schema| {
            schema
                .fields
                .iter()
                .map(|field| (field.key.as_str(), field))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let value_by_key = values
        .iter()
        .map(|value| (value.key.as_str(), value))
        .collect::<BTreeMap<_, _>>();
    let secret_by_key = secrets
        .iter()
        .map(|value| (value.key.as_str(), value))
        .collect::<BTreeMap<_, _>>();
    if value_by_key.len() != values.len() || secret_by_key.len() != secrets.len() {
        return Err(AssetError::InvalidMetadata("配置字段 key 不能重复".into()));
    }
    for value in values {
        let field = fields
            .get(value.key.as_str())
            .ok_or_else(|| AssetError::InvalidMetadata(format!("未知配置字段：{}", value.key)))?;
        if field.secret {
            return Err(AssetError::InvalidMetadata(format!(
                "私密字段 {} 必须通过 secrets 和凭据槽配置",
                value.key
            )));
        }
        let type_matches = matches!(
            (&value.value, field.value_type),
            (AssetPrimitiveValue::String(_), AssetConfigurationValueType::String)
                | (AssetPrimitiveValue::Number(_), AssetConfigurationValueType::Number)
                | (AssetPrimitiveValue::Boolean(_), AssetConfigurationValueType::Boolean)
        );
        if !type_matches {
            return Err(AssetError::InvalidMetadata(format!(
                "配置字段 {} 类型不匹配",
                value.key
            )));
        }
    }
    for secret in secrets {
        let field = fields
            .get(secret.key.as_str())
            .ok_or_else(|| AssetError::InvalidMetadata(format!("未知私密配置字段：{}", secret.key)))?;
        if !field.secret {
            return Err(AssetError::InvalidMetadata(format!(
                "非私密字段 {} 不能通过凭据槽配置",
                secret.key
            )));
        }
    }
    for field in fields.values() {
        if !field.required {
            continue;
        }
        if field.secret {
            let secret = secret_by_key
                .get(field.key.as_str())
                .ok_or_else(|| AssetError::InvalidMetadata(format!("缺少必填私密字段：{}", field.key)))?;
            if !configured_slots.contains(&secret.secret_slot) {
                return Err(AssetError::InvalidMetadata(format!(
                    "必填私密字段 {} 的凭据槽尚未配置",
                    field.key
                )));
            }
        } else if !value_by_key.contains_key(field.key.as_str()) {
            return Err(AssetError::InvalidMetadata(format!("缺少必填配置字段：{}", field.key)));
        }
    }
    let referenced_slots = referenced_secret_slots(configuration);
    for slot in &referenced_slots {
        if !configured_slots.contains(slot) {
            return Err(AssetError::InvalidMetadata(format!("凭据槽 {slot} 尚未配置")));
        }
    }
    if let Some(orphan) = configured_slots.difference(&referenced_slots).next() {
        return Err(AssetError::InvalidMetadata(format!(
            "凭据槽 {orphan} 已不再被配置引用，请在同一事务中清除"
        )));
    }
    Ok(())
}

fn referenced_secret_slots(configuration: &AssetPublicConfiguration) -> BTreeSet<String> {
    let mut slots = BTreeSet::new();
    match configuration {
        AssetPublicConfiguration::Assistant(_) | AssetPublicConfiguration::Skill(_) => {}
        AssetPublicConfiguration::EngineAdapter(value) => {
            slots.extend(value.environment.iter().map(|entry| entry.secret_slot.clone()));
            slots.extend(value.secrets.iter().map(|entry| entry.secret_slot.clone()));
        }
        AssetPublicConfiguration::Mcp(value) => {
            slots.extend(value.environment.iter().map(|entry| entry.secret_slot.clone()));
            slots.extend(value.headers.iter().map(|entry| entry.secret_slot.clone()));
            slots.extend(value.secrets.iter().map(|entry| entry.secret_slot.clone()));
        }
    }
    slots
}

fn validate_arguments(arguments: &[String]) -> Result<(), AssetError> {
    if arguments.len() > 128 {
        return Err(AssetError::InvalidMetadata("运行参数数量超过限制".into()));
    }
    for argument in arguments {
        validate_overlay_text(argument, 4096, "运行参数")?;
    }
    Ok(())
}

fn validate_named_secret_slots(
    values: &[tjuaeui_api_types::AssetNamedSecretSlot],
    label: &str,
) -> Result<(), AssetError> {
    if values.len() > 128 {
        return Err(AssetError::InvalidMetadata(format!("{label}数量超过限制")));
    }
    let mut names = BTreeSet::new();
    for value in values {
        validate_overlay_text(&value.name, 256, label)?;
        validate_secret_slot(&value.secret_slot)?;
        if !names.insert(value.name.to_ascii_lowercase()) {
            return Err(AssetError::InvalidMetadata(format!("{label}名称重复")));
        }
    }
    Ok(())
}

fn validate_keyed_secret_slots(values: &[tjuaeui_api_types::AssetKeyedSecretSlot]) -> Result<(), AssetError> {
    if values.len() > 64 {
        return Err(AssetError::InvalidMetadata("私密配置字段数量超过限制".into()));
    }
    let mut keys = BTreeSet::new();
    for value in values {
        validate_overlay_text(&value.key, 128, "私密配置字段")?;
        validate_secret_slot(&value.secret_slot)?;
        if !keys.insert(value.key.as_str()) {
            return Err(AssetError::InvalidMetadata("私密配置字段 key 重复".into()));
        }
    }
    Ok(())
}

fn validate_configuration_values(values: &[AssetConfigurationValue]) -> Result<(), AssetError> {
    if values.len() > 64 {
        return Err(AssetError::InvalidMetadata("配置字段数量超过限制".into()));
    }
    let mut keys = BTreeSet::new();
    for value in values {
        validate_overlay_text(&value.key, 128, "配置字段")?;
        if let AssetPrimitiveValue::String(text) = &value.value {
            validate_overlay_text(text, 16_384, "字符串配置值")?;
        }
        if !keys.insert(value.key.as_str()) {
            return Err(AssetError::InvalidMetadata("配置字段 key 重复".into()));
        }
    }
    Ok(())
}

fn validate_optional_overlay_text(value: Option<&str>, max_len: usize, label: &str) -> Result<(), AssetError> {
    if let Some(value) = value {
        validate_overlay_text(value, max_len, label)?;
    }
    Ok(())
}

fn validate_overlay_text(value: &str, max_len: usize, label: &str) -> Result<(), AssetError> {
    if value.trim().is_empty() || value.len() > max_len || value.contains('\0') {
        return Err(AssetError::InvalidMetadata(format!("{label}无效")));
    }
    Ok(())
}

fn validate_secret_slot(value: &str) -> Result<(), AssetError> {
    if value.is_empty()
        || value.len() > 128
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err(AssetError::InvalidMetadata("凭据槽名称无效".into()));
    }
    Ok(())
}

fn derive_asset_credential_key(
    master_key: &[u8; 32],
    user_id: &str,
    asset_id: &str,
    slot: &str,
    key_version: i64,
) -> [u8; 32] {
    fn update_length_prefixed(hasher: &mut Sha256, value: &[u8]) {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value);
    }

    let mut hasher = Sha256::new();
    hasher.update(b"tjuaeui-asset-credential-key\0");
    hasher.update(key_version.to_be_bytes());
    hasher.update(master_key);
    update_length_prefixed(&mut hasher, user_id.as_bytes());
    update_length_prefixed(&mut hasher, asset_id.as_bytes());
    update_length_prefixed(&mut hasher, slot.as_bytes());
    hasher.finalize().into()
}

fn kind_to_db(kind: AssetKind) -> &'static str {
    match kind {
        AssetKind::Assistant => "assistant",
        AssetKind::EngineAdapter => "engineAdapter",
        AssetKind::Skill => "skill",
        AssetKind::Mcp => "mcp",
    }
}

fn parse_kind(value: &str) -> Result<AssetKind, AssetError> {
    match value {
        "assistant" => Ok(AssetKind::Assistant),
        "engineAdapter" => Ok(AssetKind::EngineAdapter),
        "skill" => Ok(AssetKind::Skill),
        "mcp" => Ok(AssetKind::Mcp),
        _ => Err(AssetError::InvalidMetadata(format!("未知资产类型：{value}"))),
    }
}

fn origin_to_db(value: AssetOrigin) -> &'static str {
    match value {
        AssetOrigin::Local => "local",
        AssetOrigin::Hub => "hub",
        AssetOrigin::Seed => "seed",
    }
}

fn parse_origin(value: &str) -> Result<AssetOrigin, AssetError> {
    match value {
        "local" => Ok(AssetOrigin::Local),
        "hub" => Ok(AssetOrigin::Hub),
        "seed" => Ok(AssetOrigin::Seed),
        _ => Err(AssetError::InvalidMetadata(format!("未知资产来源：{value}"))),
    }
}

fn trust_to_db(value: AssetTrust) -> &'static str {
    match value {
        AssetTrust::Official => "official",
        AssetTrust::Verified => "verified",
        AssetTrust::Community => "community",
    }
}

fn parse_trust(value: &str) -> Result<AssetTrust, AssetError> {
    match value {
        "official" => Ok(AssetTrust::Official),
        "verified" => Ok(AssetTrust::Verified),
        "community" => Ok(AssetTrust::Community),
        _ => Err(AssetError::InvalidMetadata(format!("未知信任等级：{value}"))),
    }
}

fn scope_to_db(value: AssetScope) -> &'static str {
    match value {
        AssetScope::System => "system",
        AssetScope::User => "user",
    }
}

fn parse_scope(value: &str) -> Result<AssetScope, AssetError> {
    match value {
        "system" => Ok(AssetScope::System),
        "user" => Ok(AssetScope::User),
        _ => Err(AssetError::InvalidMetadata(format!("未知资产范围：{value}"))),
    }
}

fn editability_to_db(value: AssetEditability) -> &'static str {
    match value {
        AssetEditability::ReadOnly => "readOnly",
        AssetEditability::Overlay => "overlay",
        AssetEditability::Full => "full",
    }
}

fn parse_editability(value: &str) -> Result<AssetEditability, AssetError> {
    match value {
        "readOnly" => Ok(AssetEditability::ReadOnly),
        "overlay" => Ok(AssetEditability::Overlay),
        "full" => Ok(AssetEditability::Full),
        _ => Err(AssetError::InvalidMetadata(format!("未知可编辑性：{value}"))),
    }
}

fn parse_tracking_mode(value: &str) -> Result<AssetTrackingMode, AssetError> {
    match value {
        "tracked" => Ok(AssetTrackingMode::Tracked),
        "detached" => Ok(AssetTrackingMode::Detached),
        _ => Err(AssetError::InvalidMetadata(format!("未知跟踪模式：{value}"))),
    }
}

fn public_error_code(error: &AssetError) -> &'static str {
    match error {
        AssetError::NotFound(_) => "ASSET_NOT_FOUND",
        AssetError::UnsafePath(_) => "ASSET_UNSAFE_PATH",
        AssetError::BinaryFile(_) => "ASSET_BINARY_FILE",
        AssetError::FileTooLarge { .. } | AssetError::TotalTooLarge { .. } => "ASSET_TOO_LARGE",
        AssetError::DigestMismatch { .. } | AssetError::CorruptObject(_) => "ASSET_INTEGRITY_FAILED",
        AssetError::ConcurrentModification => "ASSET_CONCURRENT_MODIFICATION",
        AssetError::MergeConflict(_) => "ASSET_MERGE_CONFLICT",
        AssetError::DestructiveConfirmationRequired => "ASSET_DESTRUCTIVE_CONFIRMATION_REQUIRED",
        AssetError::LocalChanges => "ASSET_LOCAL_CHANGES",
        AssetError::MissingBaseSnapshot => "ASSET_BASE_MISSING",
        AssetError::SourceUnavailable(_) => "ASSET_SOURCE_UNAVAILABLE",
        AssetError::OverlayNotConfigured => "ASSET_OVERLAY_NOT_CONFIGURED",
        AssetError::UpstreamMismatch => "ASSET_UPSTREAM_MISMATCH",
        AssetError::InvalidState(_) | AssetError::InvalidMetadata(_) => "ASSET_INVALID_STATE",
        AssetError::RuntimeProjectionUnsupported { code, .. } | AssetError::RuntimeProjectionFailed { code, .. } => {
            code
        }
        AssetError::BundleInvariant(_) => "ASSET_BUNDLE_INVARIANT",
        AssetError::Database(_) | AssetError::Io(_) | AssetError::Json(_) | AssetError::Crypto(_) => "ASSET_INTERNAL",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use tjuaeui_db::{
        CommitAssetRuntimeBindingParams, CreateAssetTryRunReceiptParams, SqliteAssetRepository, init_database_memory,
    };

    async fn setup() -> (AssetCatalogService, tempfile::TempDir) {
        let db = init_database_memory().await.unwrap();
        let temp = tempfile::tempdir().unwrap();
        let repo: Arc<dyn IAssetRepository> = Arc::new(SqliteAssetRepository::new(db.pool().clone()));
        (
            AssetCatalogService::new(repo, temp.path()).with_runtime_projector(Arc::new(
                crate::runtime::test_support::RecordingRuntimeProjector::default(),
            )),
            temp,
        )
    }

    async fn create_test_user(database: &tjuaeui_db::Database, user_id: &str) {
        sqlx::query(
            "INSERT INTO users (id, username, password_hash, created_at, updated_at)
             VALUES (?, ?, '', 1, 1)",
        )
        .bind(user_id)
        .bind(user_id)
        .execute(database.pool())
        .await
        .unwrap();
    }

    fn skill_input(content: &str) -> LocalAssetInput {
        LocalAssetInput {
            id: "skill-demo".into(),
            kind: AssetKind::Skill,
            display_name: "演示技能".into(),
            description: Some("demo".into()),
            origin: AssetOrigin::Hub,
            trust: AssetTrust::Official,
            scope: AssetScope::User,
            editability: AssetEditability::Full,
            entry_file: Some("SKILL.md".into()),
            runtime_id: Some("skill-demo".into()),
            files: vec![AssetDefinitionFile::text("SKILL.md", content)],
            dependency_runtime_ids: BTreeMap::new(),
        }
    }

    fn tracked_input(content: &str, version: &str, revision: char) -> TrackedAssetInput {
        let local = skill_input(content);
        let remote_digest = prepare_definition(local.files.clone()).unwrap().1.digest;
        TrackedAssetInput {
            local,
            package_name: "tjuaeext-skill-demo".into(),
            remote_asset_id: "org.tjuae.skill.demo".into(),
            version: version.into(),
            source_revision: revision.to_string().repeat(40),
            remote_digest,
        }
    }

    fn typed_runtime_tracked_input(kind: AssetKind) -> TrackedAssetInput {
        let (local_id, runtime_id, package_name, remote_asset_id, entry_file, content) = match kind {
            AssetKind::EngineAdapter => (
                "engine-contract",
                "contract-acp",
                "tjuaeext-contract-engine",
                "tjuaeext-contract-engine/engineAdapter/contract-acp",
                "engine-adapter.json",
                include_bytes!("../tests/fixtures/engine-adapter-definition.v1.complete.json").as_slice(),
            ),
            AssetKind::Mcp => (
                "mcp-contract",
                "contract-mcp",
                "tjuaeext-contract-mcp",
                "tjuaeext-contract-mcp/mcp/contract-mcp",
                "mcp.json",
                include_bytes!("../tests/fixtures/mcp-definition.v1.complete.json").as_slice(),
            ),
            AssetKind::Assistant | AssetKind::Skill => unreachable!("测试仅构造强类型运行资产"),
        };
        let files = vec![AssetDefinitionFile {
            path: entry_file.into(),
            content: content.to_vec(),
        }];
        let remote_digest = prepare_definition(files.clone()).unwrap().1.digest;
        TrackedAssetInput {
            local: LocalAssetInput {
                id: local_id.into(),
                kind,
                display_name: runtime_id.into(),
                description: Some("typed runtime contract".into()),
                origin: AssetOrigin::Hub,
                trust: AssetTrust::Official,
                scope: AssetScope::User,
                editability: AssetEditability::Full,
                entry_file: Some(entry_file.into()),
                runtime_id: Some(runtime_id.into()),
                files,
                dependency_runtime_ids: BTreeMap::new(),
            },
            package_name: package_name.into(),
            remote_asset_id: remote_asset_id.into(),
            version: "1.0.0".into(),
            source_revision: "a".repeat(40),
            remote_digest,
        }
    }

    async fn bind_test_runtime(service: &AssetCatalogService, user_id: &str, asset_id: &str) {
        let record = service.repo.get(user_id, asset_id).await.unwrap().unwrap();
        let idempotency_key = format!("bind-{asset_id}");
        let receipt_id = format!("test-binding-receipt-{asset_id}");
        let projection_runtime_id =
            derive_projection_runtime_id(user_id, &record.user_id, asset_id, parse_kind(&record.kind).unwrap())
                .unwrap();
        service
            .repo
            .commit_try_run_receipt(CreateAssetTryRunReceiptParams {
                user_id,
                asset_id,
                receipt_id: &receipt_id,
                idempotency_key: &idempotency_key,
                definition_digest: &record.definition_digest,
                overlay_version: 0,
                portable_runtime_id: record.runtime_id.as_deref().unwrap(),
                projection_runtime_id: &projection_runtime_id,
                created_at: now_ms(),
            })
            .await
            .unwrap();
        service
            .repo
            .commit_runtime_binding(CommitAssetRuntimeBindingParams {
                user_id,
                asset_id,
                kind: &record.kind,
                projection_kind: &record.kind,
                portable_runtime_id: record.runtime_id.as_deref().unwrap(),
                projection_runtime_id: &projection_runtime_id,
                definition_digest: &record.definition_digest,
                overlay_version: 0,
                try_run_receipt_id: &receipt_id,
                health_status: "healthy",
                last_error_code: None,
                projected_at: now_ms(),
                health_checked_at: Some(now_ms()),
            })
            .await
            .unwrap();
    }

    fn reidentify_tracked(
        mut input: TrackedAssetInput,
        local_id: &str,
        runtime_id: &str,
        package_name: &str,
        remote_asset_id: &str,
    ) -> TrackedAssetInput {
        input.local.id = local_id.into();
        input.local.runtime_id = Some(runtime_id.into());
        input.package_name = package_name.into();
        input.remote_asset_id = remote_asset_id.into();
        input
    }

    fn atomic_bundle_inputs() -> Vec<TrackedAssetInput> {
        vec![
            reidentify_tracked(
                tracked_input("# bundle a", "1.0.0", 'a'),
                "skill-bundle-a",
                "bundle-a",
                "tjuaeext-bundle",
                "tjuaeext-bundle/skill/bundle-a",
            ),
            reidentify_tracked(
                tracked_input("# bundle b", "1.0.0", 'a'),
                "skill-bundle-b",
                "bundle-b",
                "tjuaeext-bundle",
                "tjuaeext-bundle/skill/bundle-b",
            ),
        ]
    }

    #[tokio::test]
    async fn runtime_provenance_resolves_tracked_asset_with_exact_upstream() {
        let (service, _temp) = setup().await;
        let input = reidentify_tracked(
            tracked_input("# tracked provenance", "2.3.4", 'f'),
            "skill-local-provenance",
            "runtime-provenance",
            "tjuaeext-provenance",
            "org.tjuae.skill.provenance",
        );
        let expected_digest = input.remote_digest.clone();
        let expected_revision = input.source_revision.clone();
        service
            .install_tracked("system_default_user", "install-provenance", input)
            .await
            .unwrap();

        let provenance = service
            .resolve_runtime_provenance("system_default_user", AssetKind::Skill, "runtime-provenance")
            .await
            .unwrap();

        assert_eq!(provenance.local_asset_id, "skill-local-provenance");
        assert_eq!(provenance.kind, AssetKind::Skill);
        assert_eq!(provenance.local_definition_digest, expected_digest);
        assert_eq!(provenance.runtime_id, "runtime-provenance");
        assert_eq!(provenance.upstream_package.as_deref(), Some("tjuaeext-provenance"));
        assert_eq!(
            provenance.upstream_asset_id.as_deref(),
            Some("org.tjuae.skill.provenance")
        );
        assert_eq!(provenance.upstream_version.as_deref(), Some("2.3.4"));
        assert_eq!(
            provenance.upstream_revision.as_deref(),
            Some(expected_revision.as_str())
        );

        let by_local_id = service
            .resolve_runtime_provenance("system_default_user", AssetKind::Skill, "skill-local-provenance")
            .await
            .unwrap();
        assert_eq!(by_local_id, provenance);
    }

    #[tokio::test]
    async fn runtime_provenance_keeps_local_and_detached_assets_upstream_free() {
        let (service, _temp) = setup().await;
        let mut local = skill_input("# local provenance");
        local.id = "skill-local-only".into();
        local.runtime_id = Some("runtime-local-only".into());
        local.origin = AssetOrigin::Local;
        local.trust = AssetTrust::Community;
        let registered = service.register_local("system_default_user", local).await.unwrap();

        let local_provenance = service
            .resolve_runtime_provenance("system_default_user", AssetKind::Skill, "runtime-local-only")
            .await
            .unwrap();
        assert_eq!(local_provenance.local_asset_id, "skill-local-only");
        assert_eq!(
            local_provenance.local_definition_digest,
            registered.asset.definition_digest
        );
        assert_eq!(
            (
                local_provenance.upstream_package,
                local_provenance.upstream_asset_id,
                local_provenance.upstream_version,
                local_provenance.upstream_revision,
            ),
            (None, None, None, None)
        );

        let tracked = reidentify_tracked(
            tracked_input("# detach provenance", "1.0.0", 'd'),
            "skill-detached-provenance",
            "runtime-detached-provenance",
            "tjuaeext-detached-provenance",
            "org.tjuae.skill.detached-provenance",
        );
        service
            .install_tracked("system_default_user", "install-detached-provenance", tracked)
            .await
            .unwrap();
        service
            .detach("system_default_user", "skill-detached-provenance")
            .await
            .unwrap();

        let detached = service
            .resolve_runtime_provenance("system_default_user", AssetKind::Skill, "runtime-detached-provenance")
            .await
            .unwrap();
        assert_eq!(
            (
                detached.upstream_package,
                detached.upstream_asset_id,
                detached.upstream_version,
                detached.upstream_revision,
            ),
            (None, None, None, None)
        );
    }

    #[tokio::test]
    async fn runtime_provenance_fails_closed_when_reference_is_missing() {
        let (service, _temp) = setup().await;

        let error = service
            .resolve_runtime_provenance("system_default_user", AssetKind::Skill, "missing-runtime")
            .await
            .unwrap_err();

        assert!(matches!(error, AssetError::NotFound(_)));
    }

    #[tokio::test]
    async fn bound_runtime_provenance_requires_exact_committed_binding() {
        let (service, _temp) = setup().await;
        let mut input = skill_input("# bound provenance");
        input.id = "skill-bound-provenance".into();
        input.runtime_id = Some("runtime-bound-provenance".into());
        input.origin = AssetOrigin::Local;
        input.trust = AssetTrust::Community;
        service.register_local("system_default_user", input).await.unwrap();

        let missing = service
            .resolve_bound_runtime_provenance("system_default_user", AssetKind::Skill, "runtime-bound-provenance")
            .await
            .unwrap_err();
        assert!(matches!(missing, AssetError::NotFound(_)));

        bind_test_runtime(&service, "system_default_user", "skill-bound-provenance").await;
        let by_runtime = service
            .resolve_bound_runtime_provenance("system_default_user", AssetKind::Skill, "runtime-bound-provenance")
            .await
            .unwrap();
        let by_local_id = service
            .resolve_bound_runtime_provenance("system_default_user", AssetKind::Skill, "skill-bound-provenance")
            .await
            .unwrap();
        assert_eq!(by_runtime, by_local_id);
    }

    #[tokio::test]
    async fn bound_runtime_assets_share_portable_identity_but_isolate_user_projections() {
        let database = init_database_memory().await.unwrap();
        create_test_user(&database, "alice").await;
        create_test_user(&database, "bob").await;
        let temp = tempfile::tempdir().unwrap();
        let repo: Arc<dyn IAssetRepository> = Arc::new(SqliteAssetRepository::new(database.pool().clone()));
        let service = AssetCatalogService::new(repo, temp.path()).with_runtime_projector(Arc::new(
            crate::runtime::test_support::RecordingRuntimeProjector::default(),
        ));

        let mut system = skill_input("# shared system skill");
        system.id = "shared-system-skill".into();
        system.runtime_id = Some("portable-shared-skill".into());
        system.origin = AssetOrigin::Seed;
        system.scope = AssetScope::System;
        system.editability = AssetEditability::Overlay;
        service.register_local("system_default_user", system).await.unwrap();

        bind_test_runtime(&service, "alice", "shared-system-skill").await;
        bind_test_runtime(&service, "bob", "shared-system-skill").await;

        let alice = service
            .resolve_bound_runtime_asset("alice", AssetKind::Skill, "portable-shared-skill")
            .await
            .unwrap();
        let bob = service
            .resolve_bound_runtime_asset("bob", AssetKind::Skill, "portable-shared-skill")
            .await
            .unwrap();
        assert_eq!(alice.provenance.runtime_id, bob.provenance.runtime_id);
        assert_ne!(alice.projection_runtime_id, bob.projection_runtime_id);
        assert_eq!(
            alice.projection_runtime_id,
            derive_projection_runtime_id("alice", "system_default_user", "shared-system-skill", AssetKind::Skill,)
                .unwrap()
        );
        assert_eq!(
            bob.projection_runtime_id,
            derive_projection_runtime_id("bob", "system_default_user", "shared-system-skill", AssetKind::Skill,)
                .unwrap()
        );
        assert_eq!(
            service
                .list_active_runtime_bindings("alice", AssetKind::Skill)
                .await
                .unwrap()
                .into_iter()
                .map(|asset| asset.projection_runtime_id)
                .collect::<Vec<_>>(),
            vec![alice.projection_runtime_id.clone()]
        );
        assert_eq!(
            service
                .list_active_runtime_bindings("bob", AssetKind::Skill)
                .await
                .unwrap()
                .into_iter()
                .map(|asset| asset.projection_runtime_id)
                .collect::<Vec<_>>(),
            vec![bob.projection_runtime_id.clone()]
        );

        service
            .repo
            .deactivate_runtime("alice", "shared-system-skill", now_ms())
            .await
            .unwrap();
        assert!(matches!(
            service
                .resolve_bound_runtime_asset("alice", AssetKind::Skill, "portable-shared-skill")
                .await,
            Err(AssetError::NotFound(_))
        ));
        assert_eq!(
            service
                .resolve_bound_runtime_asset("bob", AssetKind::Skill, "portable-shared-skill")
                .await
                .unwrap()
                .projection_runtime_id,
            bob.projection_runtime_id
        );
    }

    #[tokio::test]
    async fn runtime_provenance_fails_closed_when_runtime_id_is_ambiguous() {
        let database = init_database_memory().await.unwrap();
        create_test_user(&database, "runtime-provenance-user").await;
        let temp = tempfile::tempdir().unwrap();
        let repo: Arc<dyn IAssetRepository> = Arc::new(SqliteAssetRepository::new(database.pool().clone()));
        let service = AssetCatalogService::new(repo, temp.path()).with_runtime_projector(Arc::new(
            crate::runtime::test_support::RecordingRuntimeProjector::default(),
        ));

        let mut system = skill_input("# system");
        system.id = "skill-ambiguous-system".into();
        system.runtime_id = Some("shared-runtime-provenance".into());
        system.origin = AssetOrigin::Seed;
        system.scope = AssetScope::System;
        service.register_local("system_default_user", system).await.unwrap();

        let mut user = skill_input("# user");
        user.id = "skill-ambiguous-user".into();
        user.runtime_id = Some("shared-runtime-provenance".into());
        user.origin = AssetOrigin::Local;
        user.trust = AssetTrust::Community;
        service.register_local("runtime-provenance-user", user).await.unwrap();

        let error = service
            .resolve_runtime_provenance("runtime-provenance-user", AssetKind::Skill, "shared-runtime-provenance")
            .await
            .unwrap_err();

        assert!(matches!(error, AssetError::BundleInvariant(_)));
    }

    #[tokio::test]
    async fn runtime_provenance_fails_closed_when_workspace_digest_changed_outside_catalog() {
        let (service, _temp) = setup().await;
        let mut input = skill_input("# catalog content");
        input.id = "skill-digest-provenance".into();
        input.runtime_id = Some("runtime-digest-provenance".into());
        input.origin = AssetOrigin::Local;
        input.trust = AssetTrust::Community;
        let registered = service.register_local("system_default_user", input).await.unwrap();
        let workspace_key = service
            .content_store()
            .workspace_key("system_default_user", "skill-digest-provenance");
        let workspace = service.content_store().workspace_path(&workspace_key).unwrap();
        std::fs::write(workspace.join("SKILL.md"), "# modified outside catalog").unwrap();

        let error = service
            .resolve_runtime_provenance("system_default_user", AssetKind::Skill, "runtime-digest-provenance")
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            AssetError::DigestMismatch {
                expected,
                actual
            } if expected == registered.asset.definition_digest && expected != actual
        ));
    }

    #[tokio::test]
    async fn runtime_provenance_local_id_priority_rejects_kind_mismatch() {
        let (service, _temp) = setup().await;
        let mut input = skill_input("# kind mismatch");
        input.id = "shared-reference".into();
        input.runtime_id = Some("skill-runtime".into());
        input.origin = AssetOrigin::Local;
        input.trust = AssetTrust::Community;
        service.register_local("system_default_user", input).await.unwrap();

        let error = service
            .resolve_runtime_provenance("system_default_user", AssetKind::Assistant, "shared-reference")
            .await
            .unwrap_err();

        assert!(matches!(error, AssetError::InvalidMetadata(_)));
    }

    #[test]
    fn three_way_state_matrix_is_content_based() {
        assert_eq!(
            calculate_sync_state("same", Some("same"), Some("same"), true).unwrap(),
            AssetSyncState::Synced
        );
        assert_eq!(
            calculate_sync_state("local", Some("base"), Some("base"), true).unwrap(),
            AssetSyncState::LocalModified
        );
        assert_eq!(
            calculate_sync_state("base", Some("base"), Some("remote"), true).unwrap(),
            AssetSyncState::RemoteUpdated
        );
        assert_eq!(
            calculate_sync_state("local", Some("base"), Some("remote"), true).unwrap(),
            AssetSyncState::Diverged
        );
        assert_eq!(
            calculate_sync_state("remote", Some("base"), Some("remote"), true).unwrap(),
            AssetSyncState::RemoteUpdated
        );
        assert_eq!(
            calculate_sync_state("local", Some("base"), Some("remote"), false).unwrap(),
            AssetSyncState::RemoteUnknown
        );
        assert!(matches!(
            calculate_sync_state("local", None, Some("remote"), true),
            Err(AssetError::MissingBaseSnapshot)
        ));
    }

    #[tokio::test]
    async fn tracked_install_persists_base_and_is_idempotent() {
        let (service, _temp) = setup().await;
        let input = tracked_input("# v1", "1.0.0", 'a');
        let first = service
            .install_tracked("system_default_user", "install-request", input.clone())
            .await
            .unwrap();
        let retried = service
            .install_tracked("system_default_user", "install-request", input)
            .await
            .unwrap();
        assert_eq!(first.operation_id, retried.operation_id);
        assert_eq!(first.state, AssetOperationState::Succeeded);
        let detail = service.get("system_default_user", "skill-demo").await.unwrap();
        assert_eq!(detail.asset.sync_state, Some(AssetSyncState::RemoteUnknown));
        assert!(detail.asset.allowed_actions.contains(&AssetAction::Detach));
    }

    #[tokio::test]
    async fn detach_preserves_definition_and_runtime_but_removes_tracking_and_every_base_for_bundle() {
        let database = init_database_memory().await.unwrap();
        let temp = tempfile::tempdir().unwrap();
        let repository = SqliteAssetRepository::new(database.pool().clone());
        let projector = crate::runtime::test_support::RecordingRuntimeProjector::default();
        let service = AssetCatalogService::new(Arc::new(repository.clone()), temp.path())
            .with_runtime_projector(Arc::new(projector.clone()));
        service
            .install_tracked_bundle("system_default_user", "install-detach-bundle", atomic_bundle_inputs())
            .await
            .unwrap();

        let mut before = BTreeMap::new();
        for id in ["skill-bundle-a", "skill-bundle-b"] {
            before.insert(
                id,
                service
                    .read_file("system_default_user", id, "SKILL.md", AssetContentSource::Local)
                    .await
                    .unwrap()
                    .content,
            );
            let current = repository
                .latest_snapshot("system_default_user", id)
                .await
                .unwrap()
                .unwrap();
            repository
                .create_snapshot(CreateAssetSnapshotParams {
                    user_id: "system_default_user",
                    asset_id: id,
                    base_digest: &format!("sha256-{}", "f".repeat(64)),
                    object_key: &current.object_key,
                    manifest_json: &current.manifest_json,
                    created_at: current.created_at.saturating_sub(1),
                })
                .await
                .unwrap();
        }
        let runtime_before = (
            projector.applied.load(Ordering::SeqCst),
            projector.rolled_back.load(Ordering::SeqCst),
            projector.finalized.load(Ordering::SeqCst),
        );

        let detached = service.detach("system_default_user", "skill-bundle-a").await.unwrap();
        assert_eq!(detached.origin, AssetOrigin::Local);
        assert_eq!(detached.sync_state, None);
        assert!(detached.upstream.is_none());

        for id in ["skill-bundle-a", "skill-bundle-b"] {
            let detail = service.get("system_default_user", id).await.unwrap();
            assert_eq!(detail.asset.origin, AssetOrigin::Local);
            assert_eq!(detail.asset.sync_state, None);
            assert!(detail.asset.upstream.is_none());
            assert_eq!(
                service
                    .read_file("system_default_user", id, "SKILL.md", AssetContentSource::Local,)
                    .await
                    .unwrap()
                    .content,
                before[id]
            );
            let upstream_count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM asset_upstreams WHERE user_id = ? AND asset_id = ?")
                    .bind("system_default_user")
                    .bind(id)
                    .fetch_one(database.pool())
                    .await
                    .unwrap();
            let base_count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM asset_snapshots WHERE user_id = ? AND asset_id = ?")
                    .bind("system_default_user")
                    .bind(id)
                    .fetch_one(database.pool())
                    .await
                    .unwrap();
            assert_eq!((upstream_count, base_count), (0, 0));
        }
        assert_eq!(
            runtime_before,
            (
                projector.applied.load(Ordering::SeqCst),
                projector.rolled_back.load(Ordering::SeqCst),
                projector.finalized.load(Ordering::SeqCst),
            ),
            "解除跟踪是纯元数据事务，不得重建或删除运行投影"
        );
    }

    #[tokio::test]
    async fn detach_bundle_database_failure_rolls_back_all_members_without_touching_runtime() {
        let database = init_database_memory().await.unwrap();
        let temp = tempfile::tempdir().unwrap();
        let repository = SqliteAssetRepository::new(database.pool().clone());
        let projector = crate::runtime::test_support::RecordingRuntimeProjector::default();
        let service = AssetCatalogService::new(Arc::new(repository.clone()), temp.path())
            .with_runtime_projector(Arc::new(projector.clone()));
        service
            .install_tracked_bundle("system_default_user", "install-detach-rollback", atomic_bundle_inputs())
            .await
            .unwrap();
        sqlx::query(
            "CREATE TRIGGER fail_second_bundle_detach
             BEFORE DELETE ON asset_upstreams
             WHEN OLD.user_id = 'system_default_user' AND OLD.asset_id = 'skill-bundle-b'
             BEGIN SELECT RAISE(ABORT, 'injected detach failure'); END",
        )
        .execute(database.pool())
        .await
        .unwrap();
        let runtime_before = (
            projector.applied.load(Ordering::SeqCst),
            projector.rolled_back.load(Ordering::SeqCst),
            projector.finalized.load(Ordering::SeqCst),
        );

        assert!(service.detach("system_default_user", "skill-bundle-a").await.is_err());
        for id in ["skill-bundle-a", "skill-bundle-b"] {
            assert_eq!(
                repository.get("system_default_user", id).await.unwrap().unwrap().origin,
                "hub"
            );
            assert!(
                repository
                    .get_upstream("system_default_user", id)
                    .await
                    .unwrap()
                    .is_some()
            );
            assert!(
                repository
                    .latest_snapshot("system_default_user", id)
                    .await
                    .unwrap()
                    .is_some()
            );
        }
        assert_eq!(
            runtime_before,
            (
                projector.applied.load(Ordering::SeqCst),
                projector.rolled_back.load(Ordering::SeqCst),
                projector.finalized.load(Ordering::SeqCst),
            )
        );
    }

    #[tokio::test]
    async fn dependency_closed_packages_install_atomically_without_runtime_side_effects() {
        let db = init_database_memory().await.unwrap();
        let temp = tempfile::tempdir().unwrap();
        let repo: Arc<dyn IAssetRepository> = Arc::new(SqliteAssetRepository::new(db.pool().clone()));
        let projector = crate::runtime::test_support::RecordingRuntimeProjector::default();
        let service = AssetCatalogService::new(repo, temp.path()).with_runtime_projector(Arc::new(projector.clone()));
        let dependency = reidentify_tracked(
            tracked_input("# dependency", "1.0.0", 'a'),
            "skill-dependency",
            "dependency",
            "tjuaeext-skill-dependency",
            "tjuaeext-skill-dependency/skill/dependency",
        );
        let target = reidentify_tracked(
            tracked_input("# target", "1.0.0", 'a'),
            "skill-target",
            "target",
            "tjuaeext-skill-target",
            "tjuaeext-skill-target/skill/target",
        );

        let result = service
            .install_tracked_closure(
                "system_default_user",
                "dependency-closed-install",
                "skill-target",
                vec![dependency, target],
            )
            .await
            .unwrap();

        assert_eq!(result.state, AssetOperationState::Succeeded);
        assert_eq!(projector.applied.load(Ordering::SeqCst), 0);
        assert_eq!(projector.rolled_back.load(Ordering::SeqCst), 0);
        assert_eq!(projector.finalized.load(Ordering::SeqCst), 0);
        assert!(service.get("system_default_user", "skill-dependency").await.is_ok());
        assert!(service.get("system_default_user", "skill-target").await.is_ok());
    }

    #[tokio::test]
    async fn install_catalog_failure_restores_every_dependency_workspace_without_runtime_calls() {
        let db = init_database_memory().await.unwrap();
        sqlx::query(
            "CREATE TRIGGER fail_asset_catalog_commit
             BEFORE INSERT ON asset_records
             BEGIN
               SELECT RAISE(FAIL, 'injected asset catalog failure');
             END",
        )
        .execute(db.pool())
        .await
        .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let repo: Arc<dyn IAssetRepository> = Arc::new(SqliteAssetRepository::new(db.pool().clone()));
        let projector = crate::runtime::test_support::RecordingRuntimeProjector::default();
        let service = AssetCatalogService::new(repo, temp.path()).with_runtime_projector(Arc::new(projector.clone()));
        let dependency = reidentify_tracked(
            tracked_input("# dependency", "1.0.0", 'a'),
            "skill-dependency",
            "dependency",
            "tjuaeext-skill-dependency",
            "tjuaeext-skill-dependency/skill/dependency",
        );
        let target = reidentify_tracked(
            tracked_input("# target", "1.0.0", 'a'),
            "skill-target",
            "target",
            "tjuaeext-skill-target",
            "tjuaeext-skill-target/skill/target",
        );
        let dependency_workspace = service
            .content_store()
            .workspace_path(
                &service
                    .content_store()
                    .workspace_key("system_default_user", "skill-dependency"),
            )
            .unwrap();
        let target_workspace = service
            .content_store()
            .workspace_path(
                &service
                    .content_store()
                    .workspace_key("system_default_user", "skill-target"),
            )
            .unwrap();

        let result = service
            .install_tracked_closure(
                "system_default_user",
                "dependency-closed-db-failure",
                "skill-target",
                vec![dependency, target],
            )
            .await;

        assert!(matches!(result, Err(AssetError::Database(_))));
        assert_eq!(projector.applied.load(Ordering::SeqCst), 0);
        assert_eq!(projector.rolled_back.load(Ordering::SeqCst), 0);
        assert_eq!(projector.finalized.load(Ordering::SeqCst), 0);
        assert!(!dependency_workspace.exists());
        assert!(!target_workspace.exists());
        assert!(matches!(
            service.get("system_default_user", "skill-dependency").await,
            Err(AssetError::NotFound(_))
        ));
        assert!(matches!(
            service.get("system_default_user", "skill-target").await,
            Err(AssetError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn install_succeeds_even_when_the_runtime_projector_is_unavailable() {
        let db = init_database_memory().await.unwrap();
        let temp = tempfile::tempdir().unwrap();
        let repo: Arc<dyn IAssetRepository> = Arc::new(SqliteAssetRepository::new(db.pool().clone()));
        let projector = crate::runtime::test_support::RecordingRuntimeProjector {
            fail_apply: true,
            ..Default::default()
        };
        let service = AssetCatalogService::new(repo, temp.path()).with_runtime_projector(Arc::new(projector.clone()));
        let workspace = service
            .content_store()
            .workspace_path(
                &service
                    .content_store()
                    .workspace_key("system_default_user", "skill-demo"),
            )
            .unwrap();

        let result = service
            .install_tracked(
                "system_default_user",
                "install-runtime-failure",
                tracked_input("# never visible", "1.0.0", 'a'),
            )
            .await;

        let operation = result.unwrap();
        assert_eq!(operation.state, AssetOperationState::Succeeded);
        assert!(workspace.exists());
        assert!(
            service
                .repo
                .get("system_default_user", "skill-demo")
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(projector.applied.load(Ordering::SeqCst), 0);
        assert_eq!(projector.rolled_back.load(Ordering::SeqCst), 0);
        assert_eq!(projector.finalized.load(Ordering::SeqCst), 0);
        let stored_operation = service
            .repo
            .get_operation_by_idempotency("system_default_user", "install-runtime-failure")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored_operation.state, "succeeded");
        assert_eq!(stored_operation.phase, "complete");
        assert_eq!(stored_operation.recovery_json, "{}");
    }

    #[tokio::test]
    async fn engine_and_mcp_install_only_definition_base_and_upstream() {
        let db = init_database_memory().await.unwrap();
        let temp = tempfile::tempdir().unwrap();
        let repository = SqliteAssetRepository::new(db.pool().clone());
        let projector = crate::runtime::test_support::RecordingRuntimeProjector {
            fail_apply: true,
            ..Default::default()
        };
        let service = AssetCatalogService::new(Arc::new(repository.clone()), temp.path())
            .with_runtime_projector(Arc::new(projector.clone()));

        for (index, kind, asset_id) in [
            (0, AssetKind::EngineAdapter, "engine-contract"),
            (1, AssetKind::Mcp, "mcp-contract"),
        ] {
            let operation = service
                .install_tracked(
                    "system_default_user",
                    &format!("typed-runtime-install-{index}"),
                    typed_runtime_tracked_input(kind),
                )
                .await
                .unwrap();
            assert_eq!(operation.state, AssetOperationState::Succeeded);

            let detail = service.get("system_default_user", asset_id).await.unwrap();
            assert_eq!(detail.asset.runtime_state, AssetRuntimeState::NotConfigured);
            assert!(detail.runtime_binding.is_none());
            assert!(
                repository
                    .latest_snapshot("system_default_user", asset_id)
                    .await
                    .unwrap()
                    .is_some()
            );
            assert!(
                repository
                    .get_upstream("system_default_user", asset_id)
                    .await
                    .unwrap()
                    .is_some()
            );
            assert!(
                repository
                    .get_overlay("system_default_user", asset_id)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(projector.applied.load(Ordering::SeqCst), 0);
        assert_eq!(projector.rolled_back.load(Ordering::SeqCst), 0);
        assert_eq!(projector.finalized.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn runtime_lifecycle_requires_current_try_run_receipt_before_activation() {
        let (service, _temp) = setup().await;
        service
            .install_tracked(
                "system_default_user",
                "install-runtime-skeleton",
                typed_runtime_tracked_input(AssetKind::EngineAdapter),
            )
            .await
            .unwrap();
        let configured = service
            .configure(
                "system_default_user",
                "engine-contract",
                ConfigureAssetRequest {
                    configuration: AssetPublicConfiguration::EngineAdapter(EngineAdapterAssetConfiguration {
                        command: Some("contract-acp".into()),
                        values: vec![AssetConfigurationValue {
                            key: "profile".into(),
                            value: AssetPrimitiveValue::String("default".into()),
                        }],
                        ..Default::default()
                    }),
                    secret_updates: vec![],
                    expected_version: None,
                },
            )
            .await
            .unwrap();
        let detail = service.get("system_default_user", "engine-contract").await.unwrap();
        let base_request = AssetRuntimeCommandRequest {
            idempotency_key: "runtime-validate".into(),
            expected_definition_digest: detail.asset.definition_digest,
            expected_overlay_version: Some(configured.version),
        };
        let before_try = service
            .activate("system_default_user", "engine-contract", base_request.clone())
            .await;
        assert!(matches!(before_try, Err(AssetError::InvalidState(_))));

        let validated = service
            .validate_runtime("system_default_user", "engine-contract", base_request.clone())
            .await
            .unwrap();
        assert_eq!(validated.code.as_deref(), Some("ASSET_RUNTIME_VALIDATED"));

        let mut try_request = base_request.clone();
        try_request.idempotency_key = "runtime-try-run".into();
        let tried = service
            .try_run("system_default_user", "engine-contract", try_request)
            .await
            .unwrap();
        assert_eq!(tried.code.as_deref(), Some("ASSET_RUNTIME_TRY_RUN_SUCCEEDED"));
        assert!(
            service
                .get("system_default_user", "engine-contract")
                .await
                .unwrap()
                .asset
                .allowed_actions
                .contains(&AssetAction::Activate)
        );

        let mut activate_request = base_request;
        activate_request.idempotency_key = "runtime-activate".into();
        let active = service
            .activate("system_default_user", "engine-contract", activate_request)
            .await
            .unwrap();
        assert_eq!(active.runtime_state, AssetRuntimeState::Active);
        assert_eq!(active.code.as_deref(), Some("ASSET_RUNTIME_ACTIVATED"));
        assert!(active.runtime_binding.unwrap().try_run_receipt_id.is_some());
    }

    #[tokio::test]
    async fn projector_internal_compensation_failure_marks_asset_needs_repair() {
        let (mut service, _temp) = setup().await;
        service
            .install_tracked(
                "system_default_user",
                "install-runtime-repair",
                typed_runtime_tracked_input(AssetKind::EngineAdapter),
            )
            .await
            .unwrap();
        let configured = service
            .configure(
                "system_default_user",
                "engine-contract",
                ConfigureAssetRequest {
                    configuration: AssetPublicConfiguration::EngineAdapter(EngineAdapterAssetConfiguration {
                        command: Some("contract-acp".into()),
                        values: vec![AssetConfigurationValue {
                            key: "profile".into(),
                            value: AssetPrimitiveValue::String("default".into()),
                        }],
                        ..Default::default()
                    }),
                    secret_updates: vec![],
                    expected_version: None,
                },
            )
            .await
            .unwrap();
        let detail = service.get("system_default_user", "engine-contract").await.unwrap();
        let mut request = AssetRuntimeCommandRequest {
            idempotency_key: "repair-try-run".into(),
            expected_definition_digest: detail.asset.definition_digest,
            expected_overlay_version: Some(configured.version),
        };
        service
            .try_run("system_default_user", "engine-contract", request.clone())
            .await
            .unwrap();
        service.runtime_projector = Arc::new(crate::runtime::test_support::RecordingRuntimeProjector {
            fail_apply_with_rollback_code: true,
            ..Default::default()
        });
        request.idempotency_key = "repair-activate".into();
        let result = service
            .activate("system_default_user", "engine-contract", request)
            .await;
        assert!(matches!(
            result,
            Err(AssetError::RuntimeProjectionFailed {
                code: "TEST_RUNTIME_APPLY_ROLLBACK_FAILED",
                ..
            })
        ));
        assert_eq!(
            service
                .runtime_status("system_default_user", "engine-contract")
                .await
                .unwrap()
                .runtime_state,
            AssetRuntimeState::NeedsRepair
        );
    }

    #[tokio::test]
    async fn credential_resolver_masks_http_shape_and_decrypts_only_for_the_scoped_asset() {
        let (service, _temp) = setup().await;
        let service = service.with_credential_encryption_key([0x42; 32]);
        let mut input = safe_template_input(
            "engine:secret-demo".into(),
            AssetKind::EngineAdapter,
            "私密引擎".into(),
            Some("验证凭据隔离".into()),
            "engine-secret-demo".into(),
        )
        .unwrap();
        let entry = input
            .files
            .iter_mut()
            .find(|file| file.path == "engine-adapter.json")
            .unwrap();
        let mut definition: serde_json::Value = serde_json::from_slice(&entry.content).unwrap();
        definition["configurationSchema"]["fields"] = serde_json::json!([{
            "key": "apiToken",
            "label": "API 令牌",
            "valueType": "string",
            "required": true,
            "secret": true,
            "binding": {
                "target": "environment",
                "name": "TJUAE_API_TOKEN"
            }
        }]);
        entry.content = serde_json::to_vec_pretty(&definition).unwrap();
        service.register_local("system_default_user", input).await.unwrap();

        let configuration = AssetPublicConfiguration::EngineAdapter(EngineAdapterAssetConfiguration {
            command: Some("tjuae-adapter".into()),
            secrets: vec![tjuaeui_api_types::AssetKeyedSecretSlot {
                key: "apiToken".into(),
                secret_slot: "engine-api-token".into(),
            }],
            ..Default::default()
        });
        let response = service
            .configure(
                "system_default_user",
                "engine:secret-demo",
                ConfigureAssetRequest {
                    configuration: configuration.clone(),
                    secret_updates: vec![AssetSecretUpdate::Set {
                        slot: "engine-api-token".into(),
                        value: "top-secret-value".into(),
                    }],
                    expected_version: None,
                },
            )
            .await
            .unwrap();
        let public_json = serde_json::to_string(&response).unwrap();
        assert!(public_json.contains(ASSET_CREDENTIAL_MASK));
        assert!(!public_json.contains("top-secret-value"));
        assert!(!public_json.contains("ciphertext"));
        assert!(!public_json.contains("valueRef"));

        let resolved =
            RuntimeAssetConfigurationResolver::resolve(&service, "system_default_user", "engine:secret-demo")
                .await
                .unwrap()
                .unwrap();
        assert_eq!(
            resolved.secrets.get("engine-api-token").map(String::as_str),
            Some("top-secret-value")
        );

        let preserved = service
            .configure(
                "system_default_user",
                "engine:secret-demo",
                ConfigureAssetRequest {
                    configuration,
                    secret_updates: vec![],
                    expected_version: Some(response.version),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            preserved.secret_slots[0].masked_value.as_deref(),
            Some(ASSET_CREDENTIAL_MASK)
        );

        let master = [0x42; 32];
        let source_key =
            derive_asset_credential_key(&master, "user-a", "asset-a", "slot-a", ASSET_CREDENTIAL_KEY_VERSION);
        let ciphertext = encrypt_string("replay-me", &source_key).unwrap();
        for (user, asset, slot) in [
            ("user-b", "asset-a", "slot-a"),
            ("user-a", "asset-b", "slot-a"),
            ("user-a", "asset-a", "slot-b"),
        ] {
            let wrong_key = derive_asset_credential_key(&master, user, asset, slot, ASSET_CREDENTIAL_KEY_VERSION);
            assert!(decrypt_string(&ciphertext, &wrong_key).is_err());
        }
    }

    #[tokio::test]
    async fn create_and_duplicate_write_only_independent_local_definitions() {
        let (service, _temp) = setup().await;
        let projector = crate::runtime::test_support::RecordingRuntimeProjector::default();
        let service = service.with_runtime_projector(Arc::new(projector.clone()));
        for (id, kind) in [
            ("assistant:new", AssetKind::Assistant),
            ("engine:new", AssetKind::EngineAdapter),
            ("skill:new", AssetKind::Skill),
            ("mcp:new", AssetKind::Mcp),
        ] {
            let detail = service
                .create(
                    "system_default_user",
                    CreateAssetRequest {
                        id: id.into(),
                        kind,
                        display_name: format!("{kind:?} 新资产"),
                        description: Some("安全模板".into()),
                        runtime_id: None,
                    },
                )
                .await
                .unwrap();
            assert_eq!(detail.asset.origin, AssetOrigin::Local);
            assert_eq!(detail.asset.trust, AssetTrust::Community);
            assert_eq!(detail.asset.scope, AssetScope::User);
            assert_eq!(detail.asset.editability, AssetEditability::Full);
            assert!(detail.asset.upstream.is_none());
            assert!(detail.runtime_binding.is_none());
            assert!(
                service
                    .repo
                    .latest_snapshot("system_default_user", id)
                    .await
                    .unwrap()
                    .is_none()
            );
            assert!(
                service
                    .repo
                    .get_overlay("system_default_user", id)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(projector.applied.load(Ordering::SeqCst), 0);

        service
            .install_tracked(
                "system_default_user",
                "install-duplicate-source",
                tracked_input("# 可复制技能", "1.0.0", 'd'),
            )
            .await
            .unwrap();
        let duplicated = service
            .duplicate(
                "system_default_user",
                "skill-demo",
                DuplicateAssetRequest {
                    id: "skill:copied".into(),
                    display_name: Some("复制技能".into()),
                    description: Some("独立副本".into()),
                    runtime_id: Some("skill-copied".into()),
                },
            )
            .await
            .unwrap();
        assert_eq!(duplicated.asset.origin, AssetOrigin::Local);
        assert!(duplicated.asset.upstream.is_none());
        assert!(duplicated.runtime_binding.is_none());
        assert!(
            service
                .repo
                .latest_snapshot("system_default_user", "skill:copied")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            service
                .repo
                .list_credentials("system_default_user", "skill:copied")
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            service
                .repo
                .get_try_run_receipt("system_default_user", "skill:copied")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(projector.applied.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn sync_and_uninstall_runtime_failures_restore_workspace_and_catalog() {
        let (mut service, _temp) = setup().await;
        service
            .install_tracked(
                "system_default_user",
                "install-before-runtime-failures",
                tracked_input("# original", "1.0.0", 'a'),
            )
            .await
            .unwrap();
        bind_test_runtime(&service, "system_default_user", "skill-demo").await;
        let before = service.get("system_default_user", "skill-demo").await.unwrap();
        let before_base = service
            .read_file(
                "system_default_user",
                "skill-demo",
                "SKILL.md",
                AssetContentSource::Base,
            )
            .await
            .unwrap();
        let failing_projector = crate::runtime::test_support::RecordingRuntimeProjector {
            fail_apply: true,
            ..Default::default()
        };
        service.runtime_projector = Arc::new(failing_projector.clone());

        let sync_result = service
            .sync_fast_forward(
                "system_default_user",
                "sync-runtime-failure",
                tracked_input("# remote update", "2.0.0", 'b'),
            )
            .await;
        assert!(matches!(sync_result, Err(AssetError::RuntimeProjectionFailed { .. })));
        assert_eq!(
            service
                .read_file(
                    "system_default_user",
                    "skill-demo",
                    "SKILL.md",
                    AssetContentSource::Local,
                )
                .await
                .unwrap()
                .content,
            "# original"
        );
        assert_eq!(
            service
                .read_file(
                    "system_default_user",
                    "skill-demo",
                    "SKILL.md",
                    AssetContentSource::Base,
                )
                .await
                .unwrap(),
            before_base
        );
        let after_sync = service.get("system_default_user", "skill-demo").await.unwrap();
        assert_eq!(after_sync.asset.definition_digest, before.asset.definition_digest);
        assert_eq!(after_sync.asset.upstream, before.asset.upstream);
        let sync_operation = service
            .repo
            .get_operation_by_idempotency("system_default_user", "sync-runtime-failure")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(sync_operation.state, "failed");
        assert_eq!(sync_operation.phase, "rolled-back");
        assert_eq!(sync_operation.recovery_json, "{}");

        let uninstall_result = service
            .uninstall("system_default_user", "skill-demo", "uninstall-runtime-failure")
            .await;
        assert!(matches!(
            uninstall_result,
            Err(AssetError::RuntimeProjectionFailed { .. })
        ));
        let after_uninstall = service.get("system_default_user", "skill-demo").await.unwrap();
        assert_eq!(after_uninstall.asset.definition_digest, before.asset.definition_digest);
        assert_eq!(after_uninstall.asset.upstream, before.asset.upstream);
        assert_eq!(
            service
                .read_file(
                    "system_default_user",
                    "skill-demo",
                    "SKILL.md",
                    AssetContentSource::Local,
                )
                .await
                .unwrap()
                .content,
            "# original"
        );
        let uninstall_operation = service
            .repo
            .get_operation_by_idempotency("system_default_user", "uninstall-runtime-failure")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(uninstall_operation.state, "failed");
        assert_eq!(uninstall_operation.phase, "rolled-back");
        assert_eq!(uninstall_operation.recovery_json, "{}");
        assert_eq!(failing_projector.applied.load(Ordering::SeqCst), 0);
        assert_eq!(failing_projector.rolled_back.load(Ordering::SeqCst), 0);
        assert_eq!(failing_projector.finalized.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn sync_catalog_failure_restores_workspace_base_upstream_and_runtime() {
        let db = init_database_memory().await.unwrap();
        let temp = tempfile::tempdir().unwrap();
        let repo: Arc<dyn IAssetRepository> = Arc::new(SqliteAssetRepository::new(db.pool().clone()));
        let projector = crate::runtime::test_support::RecordingRuntimeProjector::default();
        let service = AssetCatalogService::new(repo, temp.path()).with_runtime_projector(Arc::new(projector.clone()));
        let original = tracked_input("# original", "1.0.0", 'a');
        service
            .install_tracked("system_default_user", "install-before-sync-failure", original.clone())
            .await
            .unwrap();
        bind_test_runtime(&service, "system_default_user", "skill-demo").await;
        let before = service.get("system_default_user", "skill-demo").await.unwrap();
        let before_upstream = before.asset.upstream.clone().unwrap();
        let before_base = service
            .read_file(
                "system_default_user",
                "skill-demo",
                "SKILL.md",
                AssetContentSource::Base,
            )
            .await
            .unwrap();
        sqlx::query(
            "CREATE TRIGGER fail_asset_sync_commit
             BEFORE UPDATE ON asset_records
             WHEN OLD.user_id = 'system_default_user' AND OLD.id = 'skill-demo'
             BEGIN
               SELECT RAISE(FAIL, 'injected asset sync failure');
             END",
        )
        .execute(db.pool())
        .await
        .unwrap();

        let result = service
            .sync_fast_forward(
                "system_default_user",
                "sync-db-failure",
                tracked_input("# remote update", "2.0.0", 'b'),
            )
            .await;

        assert!(matches!(result, Err(AssetError::Database(_))));
        assert_eq!(
            service
                .read_file(
                    "system_default_user",
                    "skill-demo",
                    "SKILL.md",
                    AssetContentSource::Local,
                )
                .await
                .unwrap()
                .content,
            "# original"
        );
        assert_eq!(
            service
                .read_file(
                    "system_default_user",
                    "skill-demo",
                    "SKILL.md",
                    AssetContentSource::Base,
                )
                .await
                .unwrap(),
            before_base
        );
        let after = service.get("system_default_user", "skill-demo").await.unwrap();
        assert_eq!(after.asset.definition_digest, before.asset.definition_digest);
        assert_eq!(after.asset.upstream.unwrap(), before_upstream);
        assert_eq!(projector.applied.load(Ordering::SeqCst), 1);
        assert_eq!(projector.rolled_back.load(Ordering::SeqCst), 1);
        assert_eq!(projector.finalized.load(Ordering::SeqCst), 0);
        let operation = service
            .repo
            .get_operation_by_idempotency("system_default_user", "sync-db-failure")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(operation.state, "failed");
        assert_eq!(operation.error_code.as_deref(), Some("ASSET_INTERNAL"));
    }

    #[tokio::test]
    async fn uninstall_catalog_failure_restores_workspace_metadata_and_runtime() {
        let db = init_database_memory().await.unwrap();
        let temp = tempfile::tempdir().unwrap();
        let repo: Arc<dyn IAssetRepository> = Arc::new(SqliteAssetRepository::new(db.pool().clone()));
        let projector = crate::runtime::test_support::RecordingRuntimeProjector::default();
        let service = AssetCatalogService::new(repo, temp.path()).with_runtime_projector(Arc::new(projector.clone()));
        service
            .install_tracked(
                "system_default_user",
                "install-before-uninstall-failure",
                tracked_input("# keep me", "1.0.0", 'a'),
            )
            .await
            .unwrap();
        bind_test_runtime(&service, "system_default_user", "skill-demo").await;
        let before = service.get("system_default_user", "skill-demo").await.unwrap();
        sqlx::query(
            "CREATE TRIGGER fail_asset_uninstall_commit
             BEFORE DELETE ON asset_records
             WHEN OLD.user_id = 'system_default_user' AND OLD.id = 'skill-demo'
             BEGIN
               SELECT RAISE(FAIL, 'injected asset uninstall failure');
             END",
        )
        .execute(db.pool())
        .await
        .unwrap();

        let result = service
            .uninstall("system_default_user", "skill-demo", "uninstall-db-failure")
            .await;

        assert!(matches!(result, Err(AssetError::Database(_))));
        let after = service.get("system_default_user", "skill-demo").await.unwrap();
        assert_eq!(after.asset.definition_digest, before.asset.definition_digest);
        assert_eq!(
            service
                .read_file(
                    "system_default_user",
                    "skill-demo",
                    "SKILL.md",
                    AssetContentSource::Local,
                )
                .await
                .unwrap()
                .content,
            "# keep me"
        );
        assert_eq!(projector.applied.load(Ordering::SeqCst), 1);
        assert_eq!(projector.rolled_back.load(Ordering::SeqCst), 1);
        assert_eq!(projector.finalized.load(Ordering::SeqCst), 0);
        let operation = service
            .repo
            .get_operation_by_idempotency("system_default_user", "uninstall-db-failure")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(operation.state, "failed");
        assert_eq!(operation.error_code.as_deref(), Some("ASSET_INTERNAL"));
    }

    #[tokio::test]
    async fn uninstall_is_idempotent_and_removes_user_workspace() {
        let (service, _temp) = setup().await;
        service
            .install_tracked(
                "system_default_user",
                "install-request",
                tracked_input("# v1", "1.0.0", 'a'),
            )
            .await
            .unwrap();

        let first = service
            .uninstall("system_default_user", "skill-demo", "uninstall-request")
            .await
            .unwrap();
        let retried = service
            .uninstall("system_default_user", "skill-demo", "uninstall-request")
            .await
            .unwrap();

        assert_eq!(first.operation_id, retried.operation_id);
        assert_eq!(first.kind, AssetOperationKind::Uninstall);
        assert_eq!(first.state, AssetOperationState::Succeeded);
        assert!(matches!(
            service.get("system_default_user", "skill-demo").await,
            Err(AssetError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn system_seed_cannot_be_published_or_uninstalled() {
        let (service, _temp) = setup().await;
        let mut seed = skill_input("# seed");
        seed.scope = AssetScope::System;
        seed.origin = AssetOrigin::Seed;
        seed.editability = AssetEditability::ReadOnly;
        let detail = service.register_local("system_default_user", seed).await.unwrap();

        assert!(!detail.asset.allowed_actions.contains(&AssetAction::Publish));
        assert!(!detail.asset.allowed_actions.contains(&AssetAction::Uninstall));
        assert!(matches!(
            service
                .uninstall("system_default_user", "skill-demo", "uninstall-seed")
                .await,
            Err(AssetError::InvalidState(_))
        ));
    }

    #[tokio::test]
    async fn user_metadata_workspaces_upstreams_bases_operations_and_recovery_are_isolated() {
        let db = init_database_memory().await.unwrap();
        create_test_user(&db, "asset-user-a").await;
        create_test_user(&db, "asset-user-b").await;
        let temp = tempfile::tempdir().unwrap();
        let repo: Arc<dyn IAssetRepository> = Arc::new(SqliteAssetRepository::new(db.pool().clone()));
        let service = AssetCatalogService::new(repo, temp.path()).with_runtime_projector(Arc::new(
            crate::runtime::test_support::RecordingRuntimeProjector::default(),
        ));
        let input_a = tracked_input("user a base\n", "1.0.0", 'a');
        let input_b = tracked_input("user b base\n", "2.0.0", 'b');

        let operation_a = service
            .install_tracked("asset-user-a", "same-idempotency-key", input_a)
            .await
            .unwrap();
        let operation_b = service
            .install_tracked("asset-user-b", "same-idempotency-key", input_b)
            .await
            .unwrap();
        assert_ne!(operation_a.operation_id, operation_b.operation_id);

        let record_a = service.repo.get("asset-user-a", "skill-demo").await.unwrap().unwrap();
        let record_b = service.repo.get("asset-user-b", "skill-demo").await.unwrap().unwrap();
        assert_ne!(record_a.workspace_key, record_b.workspace_key);
        assert_ne!(record_a.definition_digest, record_b.definition_digest);
        assert_eq!(
            service
                .read_file("asset-user-a", "skill-demo", "SKILL.md", AssetContentSource::Local,)
                .await
                .unwrap()
                .content,
            "user a base\n"
        );
        assert_eq!(
            service
                .read_file("asset-user-b", "skill-demo", "SKILL.md", AssetContentSource::Local,)
                .await
                .unwrap()
                .content,
            "user b base\n"
        );
        assert_ne!(
            service
                .repo
                .get_upstream("asset-user-a", "skill-demo")
                .await
                .unwrap()
                .unwrap()
                .source_revision,
            service
                .repo
                .get_upstream("asset-user-b", "skill-demo")
                .await
                .unwrap()
                .unwrap()
                .source_revision
        );
        assert_ne!(
            service
                .repo
                .latest_snapshot("asset-user-a", "skill-demo")
                .await
                .unwrap()
                .unwrap()
                .base_digest,
            service
                .repo
                .latest_snapshot("asset-user-b", "skill-demo")
                .await
                .unwrap()
                .unwrap()
                .base_digest
        );
        assert!(
            service
                .repo
                .get_operation("asset-user-b", &operation_a.operation_id)
                .await
                .unwrap()
                .is_none()
        );

        edit_local_for_user(&service, "asset-user-a", "user a private edit\n").await;
        let remote_a = tracked_input("user a remote\n", "3.0.0", 'c');
        let diff_a = service
            .diff_against_remote("asset-user-a", "skill-demo", &remote_a)
            .await
            .unwrap();
        let resolved_a = service
            .resolve_against_remote(
                "asset-user-a",
                "skill-demo",
                remote_a,
                resolution(&diff_a, AssetResolveStrategy::UseRemote, "user-a-use-remote"),
            )
            .await
            .unwrap();
        let recovery_operation_id = resolved_a.recovery_operation_id.unwrap();
        assert!(
            service
                .repo
                .get_operation("asset-user-b", &recovery_operation_id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            service
                .restore_resolution(
                    "asset-user-b",
                    "skill-demo",
                    RestoreAssetRequest {
                        recovery_operation_id,
                        expected_local_digest: record_b.definition_digest,
                        idempotency_key: "cross-user-restore".into(),
                    },
                )
                .await,
            Err(AssetError::NotFound(_))
        ));
        assert_eq!(
            service
                .read_file("asset-user-b", "skill-demo", "SKILL.md", AssetContentSource::Local,)
                .await
                .unwrap()
                .content,
            "user b base\n"
        );
    }

    #[tokio::test]
    async fn local_edit_blocks_remote_fast_forward_and_preserves_content() {
        let (service, _temp) = setup().await;
        service
            .install_tracked(
                "system_default_user",
                "install-request",
                tracked_input("# v1", "1.0.0", 'a'),
            )
            .await
            .unwrap();
        let before = service
            .read_file(
                "system_default_user",
                "skill-demo",
                "SKILL.md",
                AssetContentSource::Local,
            )
            .await
            .unwrap();
        service
            .write_file(
                "system_default_user",
                "skill-demo",
                "SKILL.md",
                "# local",
                &before.digest,
            )
            .await
            .unwrap();
        let result = service
            .sync_fast_forward(
                "system_default_user",
                "sync-request",
                tracked_input("# v2", "2.0.0", 'b'),
            )
            .await;
        assert!(matches!(result, Err(AssetError::LocalChanges)));
        let after = service
            .read_file(
                "system_default_user",
                "skill-demo",
                "SKILL.md",
                AssetContentSource::Local,
            )
            .await
            .unwrap();
        assert_eq!(after.content, "# local");
        assert_eq!(
            service
                .get("system_default_user", "skill-demo")
                .await
                .unwrap()
                .asset
                .sync_state,
            Some(AssetSyncState::RemoteUnknown)
        );
        let base = service
            .read_file(
                "system_default_user",
                "skill-demo",
                "SKILL.md",
                AssetContentSource::Base,
            )
            .await
            .unwrap();
        assert_eq!(base.content, "# v1");
        let base_detail = service
            .get_from_source("system_default_user", "skill-demo", AssetContentSource::Base)
            .await
            .unwrap();
        assert_eq!(base_detail.content_source, AssetContentSource::Base);
        assert_ne!(base_detail.source_digest, base_detail.asset.definition_digest);
    }

    #[tokio::test]
    async fn remote_update_fast_forwards_when_local_still_matches_base() {
        let (service, _temp) = setup().await;
        service
            .install_tracked(
                "system_default_user",
                "install-request",
                tracked_input("# v1", "1.0.0", 'a'),
            )
            .await
            .unwrap();
        let operation = service
            .sync_fast_forward(
                "system_default_user",
                "sync-request",
                tracked_input("# v2", "2.0.0", 'b'),
            )
            .await
            .unwrap();
        assert_eq!(operation.state, AssetOperationState::Succeeded);
        let after = service
            .read_file(
                "system_default_user",
                "skill-demo",
                "SKILL.md",
                AssetContentSource::Local,
            )
            .await
            .unwrap();
        assert_eq!(after.content, "# v2");
        assert_eq!(
            service
                .get("system_default_user", "skill-demo")
                .await
                .unwrap()
                .asset
                .sync_state,
            Some(AssetSyncState::RemoteUnknown)
        );
    }

    #[tokio::test]
    async fn file_write_uses_digest_for_optimistic_concurrency() {
        let (service, _temp) = setup().await;
        service
            .register_local("system_default_user", skill_input("# original"))
            .await
            .unwrap();
        let file = service
            .read_file(
                "system_default_user",
                "skill-demo",
                "SKILL.md",
                AssetContentSource::Local,
            )
            .await
            .unwrap();
        service
            .write_file(
                "system_default_user",
                "skill-demo",
                "SKILL.md",
                "# changed",
                &file.digest,
            )
            .await
            .unwrap();
        let stale = service
            .write_file(
                "system_default_user",
                "skill-demo",
                "SKILL.md",
                "# overwrite",
                &file.digest,
            )
            .await;
        assert!(matches!(stale, Err(AssetError::ConcurrentModification)));
    }

    async fn edit_local_for_user(service: &AssetCatalogService, user_id: &str, content: &str) {
        let file = service
            .read_file(user_id, "skill-demo", "SKILL.md", AssetContentSource::Local)
            .await
            .unwrap();
        service
            .write_file(user_id, "skill-demo", "SKILL.md", content, &file.digest)
            .await
            .unwrap();
    }

    async fn edit_local(service: &AssetCatalogService, content: &str) {
        edit_local_for_user(service, "system_default_user", content).await;
    }

    fn resolution(diff: &AssetDiffResponse, strategy: AssetResolveStrategy, key: &str) -> ResolveAssetRequest {
        ResolveAssetRequest {
            strategy,
            expected_local_digest: diff.local_digest.clone(),
            expected_base_digest: diff.base_digest.clone(),
            expected_remote_digest: diff.remote_digest.clone(),
            idempotency_key: key.into(),
            confirm_destructive: strategy == AssetResolveStrategy::UseRemote,
        }
    }

    #[tokio::test]
    async fn complete_diff_distinguishes_local_only_and_remote_only_changes() {
        let (service, _temp) = setup().await;
        let base = tracked_input("one\ntwo\n", "1.0.0", 'a');
        service
            .install_tracked("system_default_user", "install-diff", base.clone())
            .await
            .unwrap();

        edit_local(&service, "LOCAL\ntwo\n").await;
        let local_only = service
            .diff_against_remote("system_default_user", "skill-demo", &base)
            .await
            .unwrap();
        assert_eq!(local_only.sync_state, AssetSyncState::LocalModified);
        assert_eq!(
            local_only.files[0].status,
            tjuaeui_api_types::AssetDiffFileStatus::LocalModified
        );

        let (service, _temp) = setup().await;
        service
            .install_tracked("system_default_user", "install-remote-diff", base)
            .await
            .unwrap();
        let remote = tracked_input("one\nREMOTE\n", "2.0.0", 'b');
        let remote_only = service
            .diff_against_remote("system_default_user", "skill-demo", &remote)
            .await
            .unwrap();
        assert_eq!(remote_only.sync_state, AssetSyncState::RemoteUpdated);
        assert_eq!(
            remote_only.files[0].status,
            tjuaeui_api_types::AssetDiffFileStatus::RemoteModified
        );
        assert_eq!(remote_only.files.len(), 1);
        assert!(remote_only.files[0].base.is_some());
        assert!(remote_only.files[0].local.is_some());
        assert!(remote_only.files[0].remote.is_some());
    }

    #[tokio::test]
    async fn diff_materializes_a_verified_uncached_remote_object_before_comparing() {
        let (service, _temp) = setup().await;
        service
            .install_tracked(
                "system_default_user",
                "install-uncached",
                tracked_input("base\n", "1.0.0", 'a'),
            )
            .await
            .unwrap();
        let remote = tracked_input("remote\n", "2.0.0", 'b');
        let key = remote.remote_digest.strip_prefix("sha256-").unwrap();
        let object = service.content_store().object_path(key).unwrap();
        assert!(!object.exists());

        let diff = service
            .diff_against_remote("system_default_user", "skill-demo", &remote)
            .await
            .unwrap();
        assert_eq!(diff.sync_state, AssetSyncState::RemoteUpdated);
        assert!(object.join("SKILL.md").is_file());
        assert_eq!(
            diff.files[0].status,
            tjuaeui_api_types::AssetDiffFileStatus::RemoteModified
        );
    }

    #[tokio::test]
    async fn non_overlapping_changes_auto_merge_and_advance_base_to_remote() {
        let (service, _temp) = setup().await;
        service
            .install_tracked(
                "system_default_user",
                "install-merge",
                tracked_input("one\ntwo\nthree\n", "1.0.0", 'a'),
            )
            .await
            .unwrap();
        edit_local(&service, "LOCAL\ntwo\nthree\n").await;
        let remote = tracked_input("one\ntwo\nREMOTE\n", "2.0.0", 'b');
        let diff = service
            .diff_against_remote("system_default_user", "skill-demo", &remote)
            .await
            .unwrap();
        assert_eq!(diff.files[0].status, tjuaeui_api_types::AssetDiffFileStatus::Diverged);

        let resolved = service
            .resolve_against_remote(
                "system_default_user",
                "skill-demo",
                remote,
                resolution(&diff, AssetResolveStrategy::AutoMerge, "resolve-merge"),
            )
            .await
            .unwrap();
        assert_eq!(resolved.asset.sync_state, Some(AssetSyncState::LocalModified));
        assert_eq!(
            service
                .read_file(
                    "system_default_user",
                    "skill-demo",
                    "SKILL.md",
                    AssetContentSource::Local,
                )
                .await
                .unwrap()
                .content,
            "LOCAL\ntwo\nREMOTE\n"
        );
        assert_eq!(
            service
                .read_file(
                    "system_default_user",
                    "skill-demo",
                    "SKILL.md",
                    AssetContentSource::Base,
                )
                .await
                .unwrap()
                .content,
            "one\ntwo\nREMOTE\n"
        );
    }

    #[tokio::test]
    async fn overlapping_changes_fail_closed_and_digest_races_are_rejected() {
        let (service, _temp) = setup().await;
        service
            .install_tracked(
                "system_default_user",
                "install-conflict",
                tracked_input("one\ntwo\n", "1.0.0", 'a'),
            )
            .await
            .unwrap();
        edit_local(&service, "one\nLOCAL\n").await;
        let remote = tracked_input("one\nREMOTE\n", "2.0.0", 'b');
        let diff = service
            .diff_against_remote("system_default_user", "skill-demo", &remote)
            .await
            .unwrap();
        assert_eq!(diff.sync_state, AssetSyncState::Conflict);
        let conflict = service
            .resolve_against_remote(
                "system_default_user",
                "skill-demo",
                remote.clone(),
                resolution(&diff, AssetResolveStrategy::AutoMerge, "resolve-conflict"),
            )
            .await;
        assert!(matches!(conflict, Err(AssetError::MergeConflict(_))));
        assert_eq!(
            service
                .read_file(
                    "system_default_user",
                    "skill-demo",
                    "SKILL.md",
                    AssetContentSource::Local,
                )
                .await
                .unwrap()
                .content,
            "one\nLOCAL\n"
        );

        let mut stale = resolution(&diff, AssetResolveStrategy::KeepLocal, "resolve-stale");
        stale.expected_local_digest = format!("sha256-{}", "0".repeat(64));
        assert!(matches!(
            service
                .resolve_against_remote("system_default_user", "skill-demo", remote, stale)
                .await,
            Err(AssetError::ConcurrentModification)
        ));
    }

    #[tokio::test]
    async fn use_remote_keeps_recoverable_snapshot_and_restore_recovers_local() {
        let (service, _temp) = setup().await;
        service
            .install_tracked(
                "system_default_user",
                "install-recovery",
                tracked_input("base\n", "1.0.0", 'a'),
            )
            .await
            .unwrap();
        edit_local(&service, "local unpublished\n").await;
        let remote = tracked_input("remote current\n", "2.0.0", 'b');
        let diff = service
            .diff_against_remote("system_default_user", "skill-demo", &remote)
            .await
            .unwrap();
        let resolved = service
            .resolve_against_remote(
                "system_default_user",
                "skill-demo",
                remote,
                resolution(&diff, AssetResolveStrategy::UseRemote, "resolve-use-remote"),
            )
            .await
            .unwrap();
        let recovery_operation_id = resolved.recovery_operation_id.unwrap();
        assert_eq!(resolved.asset.sync_state, Some(AssetSyncState::Synced));
        let resolved_definition_digest = resolved.asset.definition_digest;
        let current = service
            .read_file(
                "system_default_user",
                "skill-demo",
                "SKILL.md",
                AssetContentSource::Local,
            )
            .await
            .unwrap();
        assert_eq!(current.content, "remote current\n");

        let restored = service
            .restore_resolution(
                "system_default_user",
                "skill-demo",
                RestoreAssetRequest {
                    recovery_operation_id,
                    expected_local_digest: resolved_definition_digest,
                    idempotency_key: "restore-use-remote".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(restored.asset.sync_state, Some(AssetSyncState::RemoteUnknown));
        assert_eq!(
            service
                .read_file(
                    "system_default_user",
                    "skill-demo",
                    "SKILL.md",
                    AssetContentSource::Local,
                )
                .await
                .unwrap()
                .content,
            "local unpublished\n"
        );
    }

    #[tokio::test]
    async fn use_remote_runtime_failure_rolls_back_workspace() {
        let (mut service, _temp) = setup().await;
        service
            .install_tracked(
                "system_default_user",
                "install-rollback",
                tracked_input("base\n", "1.0.0", 'a'),
            )
            .await
            .unwrap();
        bind_test_runtime(&service, "system_default_user", "skill-demo").await;
        edit_local(&service, "local\n").await;
        let remote = tracked_input("remote\n", "2.0.0", 'b');
        let diff = service
            .diff_against_remote("system_default_user", "skill-demo", &remote)
            .await
            .unwrap();
        let before = service.get("system_default_user", "skill-demo").await.unwrap();
        let before_base = service
            .read_file(
                "system_default_user",
                "skill-demo",
                "SKILL.md",
                AssetContentSource::Base,
            )
            .await
            .unwrap();
        service.runtime_projector = Arc::new(crate::runtime::test_support::RecordingRuntimeProjector {
            fail_apply: true,
            ..Default::default()
        });
        let result = service
            .resolve_against_remote(
                "system_default_user",
                "skill-demo",
                remote,
                resolution(&diff, AssetResolveStrategy::UseRemote, "resolve-rollback"),
            )
            .await;
        assert!(matches!(result, Err(AssetError::RuntimeProjectionFailed { .. })));
        assert_eq!(
            service
                .read_file(
                    "system_default_user",
                    "skill-demo",
                    "SKILL.md",
                    AssetContentSource::Local,
                )
                .await
                .unwrap()
                .content,
            "local\n"
        );
        assert_eq!(
            service
                .read_file(
                    "system_default_user",
                    "skill-demo",
                    "SKILL.md",
                    AssetContentSource::Base,
                )
                .await
                .unwrap(),
            before_base
        );
        let after = service.get("system_default_user", "skill-demo").await.unwrap();
        assert_eq!(after.asset.definition_digest, before.asset.definition_digest);
        assert_eq!(after.asset.upstream, before.asset.upstream);
        let operation = service
            .repo
            .get_operation_by_idempotency("system_default_user", "resolve-rollback")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(operation.state, "failed");
        assert_eq!(operation.phase, "rolled-back");
        assert_eq!(operation.recovery_json, "{}");
    }

    #[tokio::test]
    async fn use_remote_catalog_failure_restores_workspace_metadata_base_and_runtime() {
        let db = init_database_memory().await.unwrap();
        let temp = tempfile::tempdir().unwrap();
        let repo: Arc<dyn IAssetRepository> = Arc::new(SqliteAssetRepository::new(db.pool().clone()));
        let projector = crate::runtime::test_support::RecordingRuntimeProjector::default();
        let service = AssetCatalogService::new(repo, temp.path()).with_runtime_projector(Arc::new(projector.clone()));
        service
            .install_tracked(
                "system_default_user",
                "install-before-resolve-failure",
                tracked_input("base\n", "1.0.0", 'a'),
            )
            .await
            .unwrap();
        bind_test_runtime(&service, "system_default_user", "skill-demo").await;
        edit_local(&service, "local unpublished\n").await;
        let before = service.get("system_default_user", "skill-demo").await.unwrap();
        let before_base = service
            .read_file(
                "system_default_user",
                "skill-demo",
                "SKILL.md",
                AssetContentSource::Base,
            )
            .await
            .unwrap();
        let remote = tracked_input("remote current\n", "2.0.0", 'b');
        let diff = service
            .diff_against_remote("system_default_user", "skill-demo", &remote)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TRIGGER fail_asset_resolve_commit
             BEFORE UPDATE ON asset_records
             WHEN OLD.user_id = 'system_default_user' AND OLD.id = 'skill-demo'
             BEGIN
               SELECT RAISE(FAIL, 'injected asset resolve failure');
             END",
        )
        .execute(db.pool())
        .await
        .unwrap();

        let result = service
            .resolve_against_remote(
                "system_default_user",
                "skill-demo",
                remote,
                resolution(&diff, AssetResolveStrategy::UseRemote, "resolve-db-failure"),
            )
            .await;

        assert!(matches!(result, Err(AssetError::Database(_))));
        assert_eq!(
            service
                .read_file(
                    "system_default_user",
                    "skill-demo",
                    "SKILL.md",
                    AssetContentSource::Local,
                )
                .await
                .unwrap()
                .content,
            "local unpublished\n"
        );
        assert_eq!(
            service
                .read_file(
                    "system_default_user",
                    "skill-demo",
                    "SKILL.md",
                    AssetContentSource::Base,
                )
                .await
                .unwrap(),
            before_base
        );
        let after = service.get("system_default_user", "skill-demo").await.unwrap();
        assert_eq!(after.asset.definition_digest, before.asset.definition_digest);
        assert_eq!(after.asset.upstream, before.asset.upstream);
        assert_eq!(projector.applied.load(Ordering::SeqCst), 2);
        assert_eq!(projector.rolled_back.load(Ordering::SeqCst), 1);
        assert_eq!(projector.finalized.load(Ordering::SeqCst), 1);
        let operation = service
            .repo
            .get_operation_by_idempotency("system_default_user", "resolve-db-failure")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(operation.state, "failed");
        assert_eq!(operation.error_code.as_deref(), Some("ASSET_INTERNAL"));
    }

    #[test]
    fn local_asset_does_not_advertise_restore_without_a_recovery_operation() {
        let actions = allowed_actions(
            None,
            AssetEditability::Full,
            AssetScope::User,
            AssetKind::Skill,
            AssetRuntimeState::Inactive,
            false,
        );
        assert!(!actions.contains(&AssetAction::Restore));
    }

    #[test]
    fn runtime_actions_follow_the_independent_runtime_state() {
        for kind in [
            AssetKind::Assistant,
            AssetKind::EngineAdapter,
            AssetKind::Skill,
            AssetKind::Mcp,
        ] {
            let actions = allowed_actions(
                Some(AssetSyncState::Synced),
                AssetEditability::Full,
                AssetScope::User,
                kind,
                AssetRuntimeState::Inactive,
                false,
            );
            assert!(actions.contains(&AssetAction::Configure));
            assert!(actions.contains(&AssetAction::Validate));
            assert!(actions.contains(&AssetAction::TryRun));
            assert!(!actions.contains(&AssetAction::Activate));
            assert!(!actions.contains(&AssetAction::Deactivate));
        }
        for kind in [AssetKind::EngineAdapter, AssetKind::Mcp] {
            let actions = allowed_actions(
                Some(AssetSyncState::Synced),
                AssetEditability::Full,
                AssetScope::User,
                kind,
                AssetRuntimeState::NotConfigured,
                false,
            );
            assert!(!actions.contains(&AssetAction::Validate));
            assert!(!actions.contains(&AssetAction::TryRun));
        }
        let active = allowed_actions(
            None,
            AssetEditability::Full,
            AssetScope::User,
            AssetKind::Assistant,
            AssetRuntimeState::Active,
            false,
        );
        assert!(active.contains(&AssetAction::Deactivate));
    }
}
