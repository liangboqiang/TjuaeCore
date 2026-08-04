use sqlx::{SqliteConnection, SqlitePool};

use crate::{
    DbError,
    models::{
        AssetCredentialRow, AssetOperationRow, AssetOverlayRow, AssetRecordRow, AssetRuntimeBindingRow,
        AssetRuntimeStateRow, AssetSnapshotRow, AssetTryRunReceiptRow, AssetUpstreamRow,
    },
    repository::asset::{
        CommitAssetRuntimeBindingParams, CommitResolvedAssetParams, CommitTrackedAssetParams,
        ConfigureAssetOverlayParams, CreateAssetSnapshotParams, CreateAssetTryRunReceiptParams,
        EncryptedAssetSecretUpdate, IAssetRepository, SetAssetRuntimeStateParams, StartAssetOperationParams,
        UpdateAssetOperationParams, UpsertAssetRecordParams, UpsertAssetUpstreamParams,
    },
};

const ASSET_COLUMNS: &str = "user_id, id, kind, display_name, description, origin, trust, scope, \
    editability, workspace_key, definition_digest, entry_file, runtime_id, created_at, updated_at";
const UPSTREAM_COLUMNS: &str = "user_id, asset_id, package_name, remote_asset_id, version, source_revision, \
    remote_digest, tracking_mode, checked_at";
const SNAPSHOT_COLUMNS: &str = "user_id, asset_id, base_digest, object_key, manifest_json, created_at";
const OPERATION_COLUMNS: &str = "user_id, operation_id, idempotency_key, asset_id, kind, state, phase, error_code, \
    recovery_json, started_at, finished_at, updated_at";
const RUNTIME_STATE_COLUMNS: &str = "user_id, asset_owner_id, asset_id, state, last_error_code, updated_at";
const OVERLAY_COLUMNS: &str = "user_id, asset_owner_id, asset_id, kind, overlay_json, version, updated_at";
const CREDENTIAL_COLUMNS: &str =
    "user_id, asset_owner_id, asset_id, slot, ciphertext, key_version, created_at, updated_at";
const TRY_RUN_RECEIPT_COLUMNS: &str = "user_id, asset_owner_id, asset_id, receipt_id, idempotency_key, \
    definition_digest, overlay_version, portable_runtime_id, projection_runtime_id, created_at";
const RUNTIME_BINDING_COLUMNS: &str = "user_id, asset_owner_id, asset_id, kind, projection_kind, \
    portable_runtime_id, projection_runtime_id, definition_digest, overlay_version, health_status, \
    try_run_receipt_id, last_error_code, projected_at, health_checked_at";

#[derive(Clone, Debug)]
pub struct SqliteAssetRepository {
    pool: SqlitePool,
}

impl SqliteAssetRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

fn validate_detach_asset_ids(asset_ids: &[String]) -> Result<(), DbError> {
    if asset_ids.is_empty() {
        return Err(DbError::Conflict("detach asset bundle is empty".into()));
    }
    let unique = asset_ids.iter().collect::<std::collections::BTreeSet<_>>();
    if unique.len() != asset_ids.len() {
        return Err(DbError::Conflict("detach asset bundle contains duplicate ids".into()));
    }
    Ok(())
}

fn initial_runtime_state(kind: &str) -> &'static str {
    match kind {
        "engineAdapter" | "mcp" => "notConfigured",
        _ => "inactive",
    }
}

async fn ensure_initial_runtime_state(
    connection: &mut SqliteConnection,
    user_id: &str,
    asset_id: &str,
    kind: &str,
    now: i64,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO asset_runtime_states (
            user_id, asset_owner_id, asset_id, state, last_error_code, updated_at
         ) VALUES (?, ?, ?, ?, NULL, ?)
         ON CONFLICT(user_id, asset_id) DO UPDATE SET
            asset_owner_id = excluded.asset_owner_id,
            state = CASE
                WHEN asset_runtime_states.asset_owner_id = excluded.asset_owner_id
                    THEN asset_runtime_states.state
                ELSE excluded.state
            END,
            last_error_code = CASE
                WHEN asset_runtime_states.asset_owner_id = excluded.asset_owner_id
                    THEN asset_runtime_states.last_error_code
                ELSE NULL
            END,
            updated_at = CASE
                WHEN asset_runtime_states.asset_owner_id = excluded.asset_owner_id
                    THEN asset_runtime_states.updated_at
                ELSE excluded.updated_at
            END",
    )
    .bind(user_id)
    .bind(user_id)
    .bind(asset_id)
    .bind(initial_runtime_state(kind))
    .bind(now)
    .execute(connection)
    .await?;
    Ok(())
}

async fn resolve_visible_asset(
    connection: &mut SqliteConnection,
    user_id: &str,
    asset_id: &str,
) -> Result<(String, String, String, Option<String>), DbError> {
    sqlx::query_as::<_, (String, String, String, Option<String>)>(
        "SELECT user_id, kind, definition_digest, runtime_id
         FROM asset_records
         WHERE id = ? AND (
            user_id = ?
            OR (user_id = 'system_default_user' AND scope = 'system')
         )
         ORDER BY CASE WHEN user_id = ? THEN 0 ELSE 1 END
         LIMIT 1",
    )
    .bind(asset_id)
    .bind(user_id)
    .bind(user_id)
    .fetch_optional(connection)
    .await?
    .ok_or_else(|| DbError::NotFound(format!("asset {asset_id}")))
}

/// Definition 与当前 active binding 在同一数据库事务内前移。
///
/// 运行投影由上层的补偿事务先行替换；这里仅在 catalog 提交成功时更新
/// binding 摘要并使旧试跑回执失效。这样 inactive 资产没有投影副作用，
/// active 资产也不会出现 catalog 新、binding 旧的可观察中间状态。
async fn advance_binding_after_definition_write(
    connection: &mut SqliteConnection,
    user_id: &str,
    asset_id: &str,
    definition_digest: &str,
    now: i64,
) -> Result<(), DbError> {
    sqlx::query(
        "DELETE FROM asset_try_run_receipts
         WHERE user_id = ? AND asset_id = ? AND definition_digest <> ?",
    )
    .bind(user_id)
    .bind(asset_id)
    .bind(definition_digest)
    .execute(&mut *connection)
    .await?;
    sqlx::query(
        "UPDATE asset_runtime_bindings
         SET definition_digest = ?, try_run_receipt_id = NULL,
             projected_at = ?, last_error_code = NULL
         WHERE user_id = ? AND asset_id = ? AND definition_digest <> ?",
    )
    .bind(definition_digest)
    .bind(now)
    .bind(user_id)
    .bind(asset_id)
    .bind(definition_digest)
    .execute(&mut *connection)
    .await?;
    Ok(())
}

async fn detach_assets_in_connection(
    connection: &mut SqliteConnection,
    user_id: &str,
    asset_ids: &[String],
    updated_at: i64,
) -> Result<Vec<AssetRecordRow>, DbError> {
    validate_detach_asset_ids(asset_ids)?;
    for asset_id in asset_ids {
        let updated = sqlx::query(
            "UPDATE asset_records SET origin = 'local', updated_at = ?
             WHERE user_id = ? AND id = ?",
        )
        .bind(updated_at)
        .bind(user_id)
        .bind(asset_id)
        .execute(&mut *connection)
        .await?
        .rows_affected();
        if updated == 0 {
            return Err(DbError::NotFound(format!("asset {asset_id}")));
        }

        let removed_upstream = sqlx::query(
            "DELETE FROM asset_upstreams
             WHERE user_id = ? AND asset_id = ? AND tracking_mode = 'tracked'",
        )
        .bind(user_id)
        .bind(asset_id)
        .execute(&mut *connection)
        .await?
        .rows_affected();
        if removed_upstream == 0 {
            return Err(DbError::Conflict(format!("asset {asset_id} is not tracked")));
        }

        let removed_bases = sqlx::query("DELETE FROM asset_snapshots WHERE user_id = ? AND asset_id = ?")
            .bind(user_id)
            .bind(asset_id)
            .execute(&mut *connection)
            .await?
            .rows_affected();
        if removed_bases == 0 {
            return Err(DbError::Conflict(format!("asset {asset_id} has no base snapshot")));
        }
    }

    let sql = format!("SELECT {ASSET_COLUMNS} FROM asset_records WHERE user_id = ? AND id = ?");
    let mut records = Vec::with_capacity(asset_ids.len());
    for asset_id in asset_ids {
        records.push(
            sqlx::query_as::<_, AssetRecordRow>(&sql)
                .bind(user_id)
                .bind(asset_id)
                .fetch_one(&mut *connection)
                .await?,
        );
    }
    Ok(records)
}

#[async_trait::async_trait]
impl IAssetRepository for SqliteAssetRepository {
    async fn list(&self, user_id: &str, kind: Option<&str>) -> Result<Vec<AssetRecordRow>, DbError> {
        let sql = if kind.is_some() {
            format!(
                "SELECT {ASSET_COLUMNS} FROM asset_records
                 WHERE (
                    user_id = ?
                    OR (
                        user_id = 'system_default_user'
                        AND scope = 'system'
                        AND NOT EXISTS (
                            SELECT 1 FROM asset_records own
                            WHERE own.user_id = ? AND own.id = asset_records.id
                        )
                    )
                 )
                   AND kind = ?
                 ORDER BY CASE WHEN user_id = ? THEN 0 ELSE 1 END, updated_at DESC, id"
            )
        } else {
            format!(
                "SELECT {ASSET_COLUMNS} FROM asset_records
                 WHERE user_id = ?
                    OR (
                        user_id = 'system_default_user'
                        AND scope = 'system'
                        AND NOT EXISTS (
                            SELECT 1 FROM asset_records own
                            WHERE own.user_id = ? AND own.id = asset_records.id
                        )
                    )
                 ORDER BY CASE WHEN user_id = ? THEN 0 ELSE 1 END, updated_at DESC, id"
            )
        };
        let query = sqlx::query_as::<_, AssetRecordRow>(&sql).bind(user_id);
        let rows = if let Some(kind) = kind {
            query
                .bind(user_id)
                .bind(kind)
                .bind(user_id)
                .fetch_all(&self.pool)
                .await?
        } else {
            query.bind(user_id).bind(user_id).fetch_all(&self.pool).await?
        };
        Ok(rows)
    }

    async fn get(&self, user_id: &str, asset_id: &str) -> Result<Option<AssetRecordRow>, DbError> {
        let sql = format!(
            "SELECT {ASSET_COLUMNS} FROM asset_records
             WHERE id = ? AND (user_id = ? OR (user_id = 'system_default_user' AND scope = 'system'))
             ORDER BY CASE WHEN user_id = ? THEN 0 ELSE 1 END LIMIT 1"
        );
        Ok(sqlx::query_as::<_, AssetRecordRow>(&sql)
            .bind(asset_id)
            .bind(user_id)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?)
    }

    async fn upsert_record(&self, params: UpsertAssetRecordParams<'_>) -> Result<AssetRecordRow, DbError> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO asset_records (
                user_id, id, kind, display_name, description, origin, trust, scope, editability,
                workspace_key, definition_digest, entry_file, runtime_id, created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(user_id, id) DO UPDATE SET
                kind = excluded.kind,
                display_name = excluded.display_name,
                description = excluded.description,
                origin = excluded.origin,
                trust = excluded.trust,
                scope = excluded.scope,
                editability = excluded.editability,
                workspace_key = excluded.workspace_key,
                definition_digest = excluded.definition_digest,
                entry_file = excluded.entry_file,
                runtime_id = excluded.runtime_id,
                updated_at = excluded.updated_at",
        )
        .bind(params.user_id)
        .bind(params.id)
        .bind(params.kind)
        .bind(params.display_name)
        .bind(params.description)
        .bind(params.origin)
        .bind(params.trust)
        .bind(params.scope)
        .bind(params.editability)
        .bind(params.workspace_key)
        .bind(params.definition_digest)
        .bind(params.entry_file)
        .bind(params.runtime_id)
        .bind(params.now)
        .bind(params.now)
        .execute(&mut *transaction)
        .await?;
        ensure_initial_runtime_state(&mut transaction, params.user_id, params.id, params.kind, params.now).await?;
        advance_binding_after_definition_write(
            &mut transaction,
            params.user_id,
            params.id,
            params.definition_digest,
            params.now,
        )
        .await?;
        let sql = format!("SELECT {ASSET_COLUMNS} FROM asset_records WHERE user_id = ? AND id = ?");
        let record = sqlx::query_as::<_, AssetRecordRow>(&sql)
            .bind(params.user_id)
            .bind(params.id)
            .fetch_one(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(record)
    }

    async fn commit_tracked_asset(
        &self,
        record: UpsertAssetRecordParams<'_>,
        upstream: UpsertAssetUpstreamParams<'_>,
        snapshot: CreateAssetSnapshotParams<'_>,
    ) -> Result<AssetRecordRow, DbError> {
        let asset_id = record.id.to_owned();
        let committed = self
            .commit_tracked_assets(&[CommitTrackedAssetParams {
                record,
                upstream,
                snapshot,
            }])
            .await?;
        committed
            .into_iter()
            .find(|row| row.id == asset_id)
            .ok_or_else(|| DbError::NotFound(format!("asset {asset_id}")))
    }

    async fn commit_tracked_assets(
        &self,
        assets: &[CommitTrackedAssetParams<'_>],
    ) -> Result<Vec<AssetRecordRow>, DbError> {
        if assets.is_empty() {
            return Err(DbError::Conflict("tracked asset bundle is empty".into()));
        }
        let mut identities = std::collections::BTreeSet::new();
        for asset in assets {
            let record = &asset.record;
            let upstream = &asset.upstream;
            let snapshot = &asset.snapshot;
            if record.user_id != upstream.user_id
                || record.user_id != snapshot.user_id
                || record.id != upstream.asset_id
                || record.id != snapshot.asset_id
                || record.definition_digest != snapshot.base_digest
                || !identities.insert((record.user_id, record.id))
            {
                return Err(DbError::Conflict(
                    "tracked asset bundle transaction fields do not match".into(),
                ));
            }
        }
        let mut transaction = self.pool.begin().await?;
        let mut stored = Vec::with_capacity(assets.len());
        for asset in assets {
            let record = &asset.record;
            let upstream = &asset.upstream;
            let snapshot = &asset.snapshot;
            sqlx::query(
                "INSERT INTO asset_records (
                    user_id, id, kind, display_name, description, origin, trust, scope, editability,
                    workspace_key, definition_digest, entry_file, runtime_id, created_at, updated_at
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(user_id, id) DO UPDATE SET
                    kind = excluded.kind, display_name = excluded.display_name,
                    description = excluded.description, origin = excluded.origin, trust = excluded.trust,
                    scope = excluded.scope, editability = excluded.editability,
                    workspace_key = excluded.workspace_key, definition_digest = excluded.definition_digest,
                    entry_file = excluded.entry_file, runtime_id = excluded.runtime_id,
                    updated_at = excluded.updated_at",
            )
            .bind(record.user_id)
            .bind(record.id)
            .bind(record.kind)
            .bind(record.display_name)
            .bind(record.description)
            .bind(record.origin)
            .bind(record.trust)
            .bind(record.scope)
            .bind(record.editability)
            .bind(record.workspace_key)
            .bind(record.definition_digest)
            .bind(record.entry_file)
            .bind(record.runtime_id)
            .bind(record.now)
            .bind(record.now)
            .execute(&mut *transaction)
            .await?;
            ensure_initial_runtime_state(&mut transaction, record.user_id, record.id, record.kind, record.now).await?;
            advance_binding_after_definition_write(
                &mut transaction,
                record.user_id,
                record.id,
                record.definition_digest,
                record.now,
            )
            .await?;
            sqlx::query(
                "INSERT INTO asset_upstreams (
                    user_id, asset_id, package_name, remote_asset_id, version, source_revision,
                    remote_digest, tracking_mode, checked_at
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(user_id, asset_id) DO UPDATE SET
                    package_name = excluded.package_name, remote_asset_id = excluded.remote_asset_id,
                    version = excluded.version, source_revision = excluded.source_revision,
                    remote_digest = excluded.remote_digest, tracking_mode = excluded.tracking_mode,
                    checked_at = excluded.checked_at",
            )
            .bind(upstream.user_id)
            .bind(upstream.asset_id)
            .bind(upstream.package_name)
            .bind(upstream.remote_asset_id)
            .bind(upstream.version)
            .bind(upstream.source_revision)
            .bind(upstream.remote_digest)
            .bind(upstream.tracking_mode)
            .bind(upstream.checked_at)
            .execute(&mut *transaction)
            .await?;
            sqlx::query(
                "INSERT INTO asset_snapshots (
                    user_id, asset_id, base_digest, object_key, manifest_json, created_at
                 ) VALUES (?, ?, ?, ?, ?, ?)
                 ON CONFLICT(user_id, asset_id, base_digest) DO UPDATE SET
                    object_key = excluded.object_key, manifest_json = excluded.manifest_json",
            )
            .bind(snapshot.user_id)
            .bind(snapshot.asset_id)
            .bind(snapshot.base_digest)
            .bind(snapshot.object_key)
            .bind(snapshot.manifest_json)
            .bind(snapshot.created_at)
            .execute(&mut *transaction)
            .await?;
            let sql = format!("SELECT {ASSET_COLUMNS} FROM asset_records WHERE user_id = ? AND id = ?");
            stored.push(
                sqlx::query_as::<_, AssetRecordRow>(&sql)
                    .bind(record.user_id)
                    .bind(record.id)
                    .fetch_one(&mut *transaction)
                    .await?,
            );
        }
        transaction.commit().await?;
        Ok(stored)
    }

    async fn commit_resolved_asset(&self, params: CommitResolvedAssetParams<'_>) -> Result<AssetOperationRow, DbError> {
        let record = &params.record;
        let upstream = &params.upstream;
        let snapshot = &params.snapshot;
        if record.user_id != upstream.user_id
            || record.user_id != snapshot.user_id
            || record.id != upstream.asset_id
            || record.id != snapshot.asset_id
            || upstream.remote_digest != snapshot.base_digest
        {
            return Err(DbError::Conflict(
                "resolved asset transaction fields do not match".into(),
            ));
        }
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO asset_records (
                user_id, id, kind, display_name, description, origin, trust, scope, editability,
                workspace_key, definition_digest, entry_file, runtime_id, created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(user_id, id) DO UPDATE SET
                kind = excluded.kind, display_name = excluded.display_name,
                description = excluded.description, origin = excluded.origin, trust = excluded.trust,
                scope = excluded.scope, editability = excluded.editability,
                workspace_key = excluded.workspace_key, definition_digest = excluded.definition_digest,
                entry_file = excluded.entry_file, runtime_id = excluded.runtime_id,
                updated_at = excluded.updated_at",
        )
        .bind(record.user_id)
        .bind(record.id)
        .bind(record.kind)
        .bind(record.display_name)
        .bind(record.description)
        .bind(record.origin)
        .bind(record.trust)
        .bind(record.scope)
        .bind(record.editability)
        .bind(record.workspace_key)
        .bind(record.definition_digest)
        .bind(record.entry_file)
        .bind(record.runtime_id)
        .bind(record.now)
        .bind(record.now)
        .execute(&mut *transaction)
        .await?;
        ensure_initial_runtime_state(&mut transaction, record.user_id, record.id, record.kind, record.now).await?;
        advance_binding_after_definition_write(
            &mut transaction,
            record.user_id,
            record.id,
            record.definition_digest,
            record.now,
        )
        .await?;
        sqlx::query(
            "INSERT INTO asset_upstreams (
                user_id, asset_id, package_name, remote_asset_id, version, source_revision,
                remote_digest, tracking_mode, checked_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(user_id, asset_id) DO UPDATE SET
                package_name = excluded.package_name, remote_asset_id = excluded.remote_asset_id,
                version = excluded.version, source_revision = excluded.source_revision,
                remote_digest = excluded.remote_digest, tracking_mode = excluded.tracking_mode,
                checked_at = excluded.checked_at",
        )
        .bind(upstream.user_id)
        .bind(upstream.asset_id)
        .bind(upstream.package_name)
        .bind(upstream.remote_asset_id)
        .bind(upstream.version)
        .bind(upstream.source_revision)
        .bind(upstream.remote_digest)
        .bind(upstream.tracking_mode)
        .bind(upstream.checked_at)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "INSERT INTO asset_snapshots (
                user_id, asset_id, base_digest, object_key, manifest_json, created_at
             ) VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(user_id, asset_id, base_digest) DO UPDATE SET
                object_key = excluded.object_key, manifest_json = excluded.manifest_json",
        )
        .bind(snapshot.user_id)
        .bind(snapshot.asset_id)
        .bind(snapshot.base_digest)
        .bind(snapshot.object_key)
        .bind(snapshot.manifest_json)
        .bind(snapshot.created_at)
        .execute(&mut *transaction)
        .await?;
        let updated = sqlx::query(
            "UPDATE asset_operations
             SET state = 'succeeded', phase = 'complete', error_code = NULL,
                 recovery_json = ?, finished_at = ?, updated_at = ?
             WHERE user_id = ? AND operation_id = ? AND asset_id = ? AND state = 'running'",
        )
        .bind(params.recovery_json)
        .bind(params.finished_at)
        .bind(params.finished_at)
        .bind(record.user_id)
        .bind(params.operation_id)
        .bind(record.id)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        if updated == 0 {
            return Err(DbError::Conflict("resolve operation is not running".into()));
        }
        let sql = format!("SELECT {OPERATION_COLUMNS} FROM asset_operations WHERE user_id = ? AND operation_id = ?");
        let operation = sqlx::query_as::<_, AssetOperationRow>(&sql)
            .bind(record.user_id)
            .bind(params.operation_id)
            .fetch_one(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(operation)
    }

    async fn commit_detach_resolution(
        &self,
        user_id: &str,
        asset_ids: &[String],
        operation_asset_id: &str,
        operation_id: &str,
        finished_at: i64,
    ) -> Result<AssetOperationRow, DbError> {
        let mut transaction = self.pool.begin().await?;
        detach_assets_in_connection(&mut transaction, user_id, asset_ids, finished_at).await?;
        let updated = sqlx::query(
            "UPDATE asset_operations
             SET state = 'succeeded', phase = 'complete', error_code = NULL,
                 recovery_json = '{}', finished_at = ?, updated_at = ?
             WHERE user_id = ? AND operation_id = ? AND asset_id = ? AND state = 'running'",
        )
        .bind(finished_at)
        .bind(finished_at)
        .bind(user_id)
        .bind(operation_id)
        .bind(operation_asset_id)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        if updated == 0 {
            return Err(DbError::Conflict("resolve operation is not running".into()));
        }
        let sql = format!("SELECT {OPERATION_COLUMNS} FROM asset_operations WHERE user_id = ? AND operation_id = ?");
        let operation = sqlx::query_as::<_, AssetOperationRow>(&sql)
            .bind(user_id)
            .bind(operation_id)
            .fetch_one(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(operation)
    }

    async fn detach_assets(
        &self,
        user_id: &str,
        asset_ids: &[String],
        updated_at: i64,
    ) -> Result<Vec<AssetRecordRow>, DbError> {
        let mut transaction = self.pool.begin().await?;
        let records = detach_assets_in_connection(&mut transaction, user_id, asset_ids, updated_at).await?;
        transaction.commit().await?;
        Ok(records)
    }

    async fn commit_restored_asset(
        &self,
        record: UpsertAssetRecordParams<'_>,
        operation_id: &str,
        finished_at: i64,
    ) -> Result<AssetOperationRow, DbError> {
        let mut transaction = self.pool.begin().await?;
        let updated_record = sqlx::query(
            "UPDATE asset_records
             SET definition_digest = ?, updated_at = ?
             WHERE user_id = ? AND id = ? AND workspace_key = ?",
        )
        .bind(record.definition_digest)
        .bind(record.now)
        .bind(record.user_id)
        .bind(record.id)
        .bind(record.workspace_key)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        if updated_record == 0 {
            return Err(DbError::NotFound(format!("asset {}", record.id)));
        }
        advance_binding_after_definition_write(
            &mut transaction,
            record.user_id,
            record.id,
            record.definition_digest,
            record.now,
        )
        .await?;
        let updated_operation = sqlx::query(
            "UPDATE asset_operations
             SET state = 'succeeded', phase = 'complete', error_code = NULL,
                 recovery_json = '{}', finished_at = ?, updated_at = ?
             WHERE user_id = ? AND operation_id = ? AND asset_id = ? AND state = 'running'",
        )
        .bind(finished_at)
        .bind(finished_at)
        .bind(record.user_id)
        .bind(operation_id)
        .bind(record.id)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        if updated_operation == 0 {
            return Err(DbError::Conflict("restore operation is not running".into()));
        }
        let sql = format!("SELECT {OPERATION_COLUMNS} FROM asset_operations WHERE user_id = ? AND operation_id = ?");
        let operation = sqlx::query_as::<_, AssetOperationRow>(&sql)
            .bind(record.user_id)
            .bind(operation_id)
            .fetch_one(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(operation)
    }

    async fn commit_uninstall(
        &self,
        user_id: &str,
        asset_id: &str,
        operation_id: &str,
        finished_at: i64,
    ) -> Result<AssetOperationRow, DbError> {
        self.commit_uninstall_assets(user_id, &[asset_id.to_owned()], operation_id, asset_id, finished_at)
            .await
    }

    async fn commit_uninstall_assets(
        &self,
        user_id: &str,
        asset_ids: &[String],
        operation_id: &str,
        operation_asset_id: &str,
        finished_at: i64,
    ) -> Result<AssetOperationRow, DbError> {
        if asset_ids.is_empty() {
            return Err(DbError::Conflict("uninstall asset bundle is empty".into()));
        }
        let unique = asset_ids.iter().collect::<std::collections::BTreeSet<_>>();
        if unique.len() != asset_ids.len() {
            return Err(DbError::Conflict(
                "uninstall asset bundle contains duplicate ids".into(),
            ));
        }
        let mut transaction = self.pool.begin().await?;
        for asset_id in asset_ids {
            let deleted = sqlx::query("DELETE FROM asset_records WHERE user_id = ? AND id = ?")
                .bind(user_id)
                .bind(asset_id)
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            if deleted == 0 {
                return Err(DbError::NotFound(format!("asset {asset_id}")));
            }
        }
        let updated = sqlx::query(
            "UPDATE asset_operations
             SET state = 'succeeded', phase = 'complete', error_code = NULL,
                 recovery_json = '{}', finished_at = ?, updated_at = ?
             WHERE user_id = ? AND operation_id = ? AND asset_id = ? AND state = 'running'",
        )
        .bind(finished_at)
        .bind(finished_at)
        .bind(user_id)
        .bind(operation_id)
        .bind(operation_asset_id)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        if updated == 0 {
            return Err(DbError::Conflict("uninstall operation is not running".into()));
        }
        let sql = format!("SELECT {OPERATION_COLUMNS} FROM asset_operations WHERE user_id = ? AND operation_id = ?");
        let operation = sqlx::query_as::<_, AssetOperationRow>(&sql)
            .bind(user_id)
            .bind(operation_id)
            .fetch_one(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(operation)
    }

    async fn delete(&self, user_id: &str, asset_id: &str) -> Result<bool, DbError> {
        Ok(sqlx::query("DELETE FROM asset_records WHERE user_id = ? AND id = ?")
            .bind(user_id)
            .bind(asset_id)
            .execute(&self.pool)
            .await?
            .rows_affected()
            > 0)
    }

    async fn get_upstream(&self, user_id: &str, asset_id: &str) -> Result<Option<AssetUpstreamRow>, DbError> {
        let sql = format!("SELECT {UPSTREAM_COLUMNS} FROM asset_upstreams WHERE user_id = ? AND asset_id = ?");
        Ok(sqlx::query_as::<_, AssetUpstreamRow>(&sql)
            .bind(user_id)
            .bind(asset_id)
            .fetch_optional(&self.pool)
            .await?)
    }

    async fn upsert_upstream(&self, params: UpsertAssetUpstreamParams<'_>) -> Result<AssetUpstreamRow, DbError> {
        sqlx::query(
            "INSERT INTO asset_upstreams (
                user_id, asset_id, package_name, remote_asset_id, version, source_revision,
                remote_digest, tracking_mode, checked_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(user_id, asset_id) DO UPDATE SET
                package_name = excluded.package_name,
                remote_asset_id = excluded.remote_asset_id,
                version = excluded.version,
                source_revision = excluded.source_revision,
                remote_digest = excluded.remote_digest,
                tracking_mode = excluded.tracking_mode,
                checked_at = excluded.checked_at",
        )
        .bind(params.user_id)
        .bind(params.asset_id)
        .bind(params.package_name)
        .bind(params.remote_asset_id)
        .bind(params.version)
        .bind(params.source_revision)
        .bind(params.remote_digest)
        .bind(params.tracking_mode)
        .bind(params.checked_at)
        .execute(&self.pool)
        .await?;
        self.get_upstream(params.user_id, params.asset_id)
            .await?
            .ok_or_else(|| DbError::NotFound(format!("asset upstream {}", params.asset_id)))
    }

    async fn create_snapshot(&self, params: CreateAssetSnapshotParams<'_>) -> Result<AssetSnapshotRow, DbError> {
        sqlx::query(
            "INSERT INTO asset_snapshots (
                user_id, asset_id, base_digest, object_key, manifest_json, created_at
             ) VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(user_id, asset_id, base_digest) DO UPDATE SET
                object_key = excluded.object_key,
                manifest_json = excluded.manifest_json",
        )
        .bind(params.user_id)
        .bind(params.asset_id)
        .bind(params.base_digest)
        .bind(params.object_key)
        .bind(params.manifest_json)
        .bind(params.created_at)
        .execute(&self.pool)
        .await?;
        let sql = format!(
            "SELECT {SNAPSHOT_COLUMNS} FROM asset_snapshots
             WHERE user_id = ? AND asset_id = ? AND base_digest = ?"
        );
        sqlx::query_as::<_, AssetSnapshotRow>(&sql)
            .bind(params.user_id)
            .bind(params.asset_id)
            .bind(params.base_digest)
            .fetch_one(&self.pool)
            .await
            .map_err(DbError::from)
    }

    async fn latest_snapshot(&self, user_id: &str, asset_id: &str) -> Result<Option<AssetSnapshotRow>, DbError> {
        let sql = format!(
            "SELECT {SNAPSHOT_COLUMNS} FROM asset_snapshots
             WHERE user_id = ? AND asset_id = ? ORDER BY created_at DESC, base_digest DESC LIMIT 1"
        );
        Ok(sqlx::query_as::<_, AssetSnapshotRow>(&sql)
            .bind(user_id)
            .bind(asset_id)
            .fetch_optional(&self.pool)
            .await?)
    }

    async fn start_operation(&self, params: StartAssetOperationParams<'_>) -> Result<AssetOperationRow, DbError> {
        sqlx::query(
            "INSERT INTO asset_operations (
                user_id, operation_id, idempotency_key, asset_id, kind, state, phase,
                error_code, recovery_json, started_at, finished_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, 'running', ?, NULL, ?, ?, NULL, ?)
             ON CONFLICT(user_id, idempotency_key) DO NOTHING",
        )
        .bind(params.user_id)
        .bind(params.operation_id)
        .bind(params.idempotency_key)
        .bind(params.asset_id)
        .bind(params.kind)
        .bind(params.phase)
        .bind(params.recovery_json)
        .bind(params.started_at)
        .bind(params.started_at)
        .execute(&self.pool)
        .await?;
        self.get_operation_by_idempotency(params.user_id, params.idempotency_key)
            .await?
            .ok_or_else(|| DbError::NotFound(format!("asset operation {}", params.operation_id)))
    }

    async fn get_operation_by_idempotency(
        &self,
        user_id: &str,
        idempotency_key: &str,
    ) -> Result<Option<AssetOperationRow>, DbError> {
        let sql = format!("SELECT {OPERATION_COLUMNS} FROM asset_operations WHERE user_id = ? AND idempotency_key = ?");
        Ok(sqlx::query_as::<_, AssetOperationRow>(&sql)
            .bind(user_id)
            .bind(idempotency_key)
            .fetch_optional(&self.pool)
            .await?)
    }

    async fn get_operation(&self, user_id: &str, operation_id: &str) -> Result<Option<AssetOperationRow>, DbError> {
        let sql = format!("SELECT {OPERATION_COLUMNS} FROM asset_operations WHERE user_id = ? AND operation_id = ?");
        Ok(sqlx::query_as::<_, AssetOperationRow>(&sql)
            .bind(user_id)
            .bind(operation_id)
            .fetch_optional(&self.pool)
            .await?)
    }

    async fn update_operation(
        &self,
        user_id: &str,
        operation_id: &str,
        params: UpdateAssetOperationParams<'_>,
    ) -> Result<Option<AssetOperationRow>, DbError> {
        sqlx::query(
            "UPDATE asset_operations
             SET state = ?, phase = ?, error_code = ?, recovery_json = ?, finished_at = ?, updated_at = ?
             WHERE user_id = ? AND operation_id = ?",
        )
        .bind(params.state)
        .bind(params.phase)
        .bind(params.error_code)
        .bind(params.recovery_json)
        .bind(params.finished_at)
        .bind(params.updated_at)
        .bind(user_id)
        .bind(operation_id)
        .execute(&self.pool)
        .await?;
        let sql = format!("SELECT {OPERATION_COLUMNS} FROM asset_operations WHERE user_id = ? AND operation_id = ?");
        Ok(sqlx::query_as::<_, AssetOperationRow>(&sql)
            .bind(user_id)
            .bind(operation_id)
            .fetch_optional(&self.pool)
            .await?)
    }

    async fn list_recoverable_operations(&self) -> Result<Vec<AssetOperationRow>, DbError> {
        let sql = format!(
            "SELECT {OPERATION_COLUMNS} FROM asset_operations
             WHERE state IN ('queued', 'running') ORDER BY started_at, operation_id"
        );
        Ok(sqlx::query_as::<_, AssetOperationRow>(&sql)
            .fetch_all(&self.pool)
            .await?)
    }

    async fn get_runtime_state(&self, user_id: &str, asset_id: &str) -> Result<Option<AssetRuntimeStateRow>, DbError> {
        let sql = format!(
            "SELECT {RUNTIME_STATE_COLUMNS}
             FROM asset_runtime_states state
             WHERE state.user_id = ? AND state.asset_id = ?
               AND state.asset_owner_id = (
                    SELECT record.user_id
                    FROM asset_records record
                    WHERE record.id = ? AND (
                        record.user_id = ?
                        OR (record.user_id = 'system_default_user' AND record.scope = 'system')
                    )
                    ORDER BY CASE WHEN record.user_id = ? THEN 0 ELSE 1 END
                    LIMIT 1
               )"
        );
        Ok(sqlx::query_as::<_, AssetRuntimeStateRow>(&sql)
            .bind(user_id)
            .bind(asset_id)
            .bind(asset_id)
            .bind(user_id)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?)
    }

    async fn get_overlay(&self, user_id: &str, asset_id: &str) -> Result<Option<AssetOverlayRow>, DbError> {
        let sql = format!(
            "SELECT {OVERLAY_COLUMNS}
             FROM asset_overlays overlay
             WHERE overlay.user_id = ? AND overlay.asset_id = ?
               AND overlay.asset_owner_id = (
                    SELECT record.user_id
                    FROM asset_records record
                    WHERE record.id = ? AND (
                        record.user_id = ?
                        OR (record.user_id = 'system_default_user' AND record.scope = 'system')
                    )
                    ORDER BY CASE WHEN record.user_id = ? THEN 0 ELSE 1 END
                    LIMIT 1
               )"
        );
        Ok(sqlx::query_as::<_, AssetOverlayRow>(&sql)
            .bind(user_id)
            .bind(asset_id)
            .bind(asset_id)
            .bind(user_id)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?)
    }

    async fn configure_overlay(&self, params: ConfigureAssetOverlayParams<'_>) -> Result<AssetOverlayRow, DbError> {
        let mut transaction = self.pool.begin().await?;
        let (asset_owner_id, asset_kind, _, _) =
            resolve_visible_asset(&mut transaction, params.user_id, params.asset_id).await?;
        if asset_kind != params.kind {
            return Err(DbError::Conflict("overlay kind does not match asset kind".into()));
        }

        let current = sqlx::query_as::<_, AssetOverlayRow>(&format!(
            "SELECT {OVERLAY_COLUMNS}
             FROM asset_overlays
             WHERE user_id = ? AND asset_id = ? AND asset_owner_id = ?"
        ))
        .bind(params.user_id)
        .bind(params.asset_id)
        .bind(&asset_owner_id)
        .fetch_optional(&mut *transaction)
        .await?;
        let next_version = match (current.as_ref(), params.expected_version) {
            (None, None) => 1,
            (Some(current), Some(expected)) if current.version == expected => current.version + 1,
            _ => return Err(DbError::Conflict("asset overlay version does not match".into())),
        };

        sqlx::query(
            "INSERT INTO asset_overlays (
                user_id, asset_owner_id, asset_id, kind, overlay_json, version, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(user_id, asset_id) DO UPDATE SET
                asset_owner_id = excluded.asset_owner_id,
                kind = excluded.kind,
                overlay_json = excluded.overlay_json,
                version = excluded.version,
                updated_at = excluded.updated_at",
        )
        .bind(params.user_id)
        .bind(&asset_owner_id)
        .bind(params.asset_id)
        .bind(params.kind)
        .bind(params.overlay_json)
        .bind(next_version)
        .bind(params.now)
        .execute(&mut *transaction)
        .await?;

        let mut updated_slots = std::collections::BTreeSet::new();
        for update in params.secret_updates {
            let slot = match update {
                EncryptedAssetSecretUpdate::Set { slot, .. } | EncryptedAssetSecretUpdate::Clear { slot } => *slot,
            };
            if !updated_slots.insert(slot) {
                return Err(DbError::Conflict(
                    "asset credential slot is updated more than once".into(),
                ));
            }
            match update {
                EncryptedAssetSecretUpdate::Set {
                    slot,
                    ciphertext,
                    key_version,
                } => {
                    sqlx::query(
                        "INSERT INTO asset_credentials (
                            user_id, asset_owner_id, asset_id, slot, ciphertext,
                            key_version, created_at, updated_at
                         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                         ON CONFLICT(user_id, asset_id, slot) DO UPDATE SET
                            asset_owner_id = excluded.asset_owner_id,
                            ciphertext = excluded.ciphertext,
                            key_version = excluded.key_version,
                            updated_at = excluded.updated_at",
                    )
                    .bind(params.user_id)
                    .bind(&asset_owner_id)
                    .bind(params.asset_id)
                    .bind(slot)
                    .bind(ciphertext)
                    .bind(key_version)
                    .bind(params.now)
                    .bind(params.now)
                    .execute(&mut *transaction)
                    .await?;
                }
                EncryptedAssetSecretUpdate::Clear { slot } => {
                    sqlx::query(
                        "DELETE FROM asset_credentials
                         WHERE user_id = ? AND asset_id = ? AND asset_owner_id = ? AND slot = ?",
                    )
                    .bind(params.user_id)
                    .bind(params.asset_id)
                    .bind(&asset_owner_id)
                    .bind(slot)
                    .execute(&mut *transaction)
                    .await?;
                }
            }
        }

        // 任意配置版本变化都会使旧试跑回执失效。
        sqlx::query(
            "DELETE FROM asset_try_run_receipts
             WHERE user_id = ? AND asset_id = ? AND asset_owner_id = ?",
        )
        .bind(params.user_id)
        .bind(params.asset_id)
        .bind(&asset_owner_id)
        .execute(&mut *transaction)
        .await?;

        let has_binding: bool = sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM asset_runtime_bindings
                WHERE user_id = ? AND asset_id = ? AND asset_owner_id = ?
             )",
        )
        .bind(params.user_id)
        .bind(params.asset_id)
        .bind(&asset_owner_id)
        .fetch_one(&mut *transaction)
        .await?;
        let state = if has_binding { "needsRepair" } else { "inactive" };
        sqlx::query(
            "INSERT INTO asset_runtime_states (
                user_id, asset_owner_id, asset_id, state, last_error_code, updated_at
             ) VALUES (?, ?, ?, ?, NULL, ?)
             ON CONFLICT(user_id, asset_id) DO UPDATE SET
                asset_owner_id = excluded.asset_owner_id,
                state = excluded.state,
                last_error_code = NULL,
                updated_at = excluded.updated_at",
        )
        .bind(params.user_id)
        .bind(&asset_owner_id)
        .bind(params.asset_id)
        .bind(state)
        .bind(params.now)
        .execute(&mut *transaction)
        .await?;

        let stored = sqlx::query_as::<_, AssetOverlayRow>(&format!(
            "SELECT {OVERLAY_COLUMNS} FROM asset_overlays
             WHERE user_id = ? AND asset_id = ? AND asset_owner_id = ?"
        ))
        .bind(params.user_id)
        .bind(params.asset_id)
        .bind(&asset_owner_id)
        .fetch_one(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(stored)
    }

    async fn list_credentials(&self, user_id: &str, asset_id: &str) -> Result<Vec<AssetCredentialRow>, DbError> {
        let sql = format!(
            "SELECT {CREDENTIAL_COLUMNS}
             FROM asset_credentials credential
             WHERE credential.user_id = ? AND credential.asset_id = ?
               AND credential.asset_owner_id = (
                    SELECT record.user_id
                    FROM asset_records record
                    WHERE record.id = ? AND (
                        record.user_id = ?
                        OR (record.user_id = 'system_default_user' AND record.scope = 'system')
                    )
                    ORDER BY CASE WHEN record.user_id = ? THEN 0 ELSE 1 END
                    LIMIT 1
               )
             ORDER BY credential.slot"
        );
        Ok(sqlx::query_as::<_, AssetCredentialRow>(&sql)
            .bind(user_id)
            .bind(asset_id)
            .bind(asset_id)
            .bind(user_id)
            .bind(user_id)
            .fetch_all(&self.pool)
            .await?)
    }

    async fn get_try_run_receipt(
        &self,
        user_id: &str,
        asset_id: &str,
    ) -> Result<Option<AssetTryRunReceiptRow>, DbError> {
        let sql = format!(
            "SELECT {TRY_RUN_RECEIPT_COLUMNS}
             FROM asset_try_run_receipts receipt
             WHERE receipt.user_id = ? AND receipt.asset_id = ?
               AND receipt.asset_owner_id = (
                    SELECT record.user_id
                    FROM asset_records record
                    WHERE record.id = ? AND (
                        record.user_id = ?
                        OR (record.user_id = 'system_default_user' AND record.scope = 'system')
                    )
                    ORDER BY CASE WHEN record.user_id = ? THEN 0 ELSE 1 END
                    LIMIT 1
               )"
        );
        Ok(sqlx::query_as::<_, AssetTryRunReceiptRow>(&sql)
            .bind(user_id)
            .bind(asset_id)
            .bind(asset_id)
            .bind(user_id)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?)
    }

    async fn get_try_run_receipt_by_idempotency(
        &self,
        user_id: &str,
        idempotency_key: &str,
    ) -> Result<Option<AssetTryRunReceiptRow>, DbError> {
        let sql = format!(
            "SELECT {TRY_RUN_RECEIPT_COLUMNS}
             FROM asset_try_run_receipts receipt
             WHERE receipt.user_id = ? AND receipt.idempotency_key = ?
               AND EXISTS (
                    SELECT 1 FROM asset_records record
                    WHERE record.user_id = receipt.asset_owner_id
                      AND record.id = receipt.asset_id
                      AND (
                        record.user_id = ?
                        OR (record.user_id = 'system_default_user' AND record.scope = 'system')
                      )
               )"
        );
        Ok(sqlx::query_as::<_, AssetTryRunReceiptRow>(&sql)
            .bind(user_id)
            .bind(idempotency_key)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?)
    }

    async fn commit_try_run_receipt(
        &self,
        params: CreateAssetTryRunReceiptParams<'_>,
    ) -> Result<AssetTryRunReceiptRow, DbError> {
        let mut transaction = self.pool.begin().await?;
        let (asset_owner_id, _, definition_digest, portable_runtime_id) =
            resolve_visible_asset(&mut transaction, params.user_id, params.asset_id).await?;
        if definition_digest != params.definition_digest {
            return Err(DbError::Conflict("try-run receipt Definition is stale".into()));
        }
        if portable_runtime_id.as_deref() != Some(params.portable_runtime_id) {
            return Err(DbError::Conflict(
                "try-run receipt portable runtime ID does not match the current asset Definition".into(),
            ));
        }
        let overlay_version = sqlx::query_scalar::<_, i64>(
            "SELECT version FROM asset_overlays
             WHERE user_id = ? AND asset_id = ? AND asset_owner_id = ?",
        )
        .bind(params.user_id)
        .bind(params.asset_id)
        .bind(&asset_owner_id)
        .fetch_optional(&mut *transaction)
        .await?
        .unwrap_or(0);
        if overlay_version != params.overlay_version {
            return Err(DbError::Conflict("try-run receipt Overlay is stale".into()));
        }

        sqlx::query(
            "INSERT INTO asset_try_run_receipts (
                user_id, asset_owner_id, asset_id, receipt_id, idempotency_key,
                definition_digest, overlay_version, portable_runtime_id, projection_runtime_id, created_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(user_id, asset_id) DO UPDATE SET
                asset_owner_id = excluded.asset_owner_id,
                receipt_id = excluded.receipt_id,
                idempotency_key = excluded.idempotency_key,
                definition_digest = excluded.definition_digest,
                overlay_version = excluded.overlay_version,
                portable_runtime_id = excluded.portable_runtime_id,
                projection_runtime_id = excluded.projection_runtime_id,
                created_at = excluded.created_at",
        )
        .bind(params.user_id)
        .bind(&asset_owner_id)
        .bind(params.asset_id)
        .bind(params.receipt_id)
        .bind(params.idempotency_key)
        .bind(params.definition_digest)
        .bind(params.overlay_version)
        .bind(params.portable_runtime_id)
        .bind(params.projection_runtime_id)
        .bind(params.created_at)
        .execute(&mut *transaction)
        .await?;
        let stored = sqlx::query_as::<_, AssetTryRunReceiptRow>(&format!(
            "SELECT {TRY_RUN_RECEIPT_COLUMNS}
             FROM asset_try_run_receipts
             WHERE user_id = ? AND asset_id = ? AND asset_owner_id = ?"
        ))
        .bind(params.user_id)
        .bind(params.asset_id)
        .bind(&asset_owner_id)
        .fetch_one(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(stored)
    }

    async fn set_runtime_state(&self, params: SetAssetRuntimeStateParams<'_>) -> Result<AssetRuntimeStateRow, DbError> {
        let mut transaction = self.pool.begin().await?;
        let (asset_owner_id, _, _, _) =
            resolve_visible_asset(&mut transaction, params.user_id, params.asset_id).await?;
        sqlx::query(
            "INSERT INTO asset_runtime_states (
                user_id, asset_owner_id, asset_id, state, last_error_code, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(user_id, asset_id) DO UPDATE SET
                asset_owner_id = excluded.asset_owner_id,
                state = excluded.state,
                last_error_code = excluded.last_error_code,
                updated_at = excluded.updated_at",
        )
        .bind(params.user_id)
        .bind(&asset_owner_id)
        .bind(params.asset_id)
        .bind(params.state)
        .bind(params.last_error_code)
        .bind(params.now)
        .execute(&mut *transaction)
        .await?;
        let stored = sqlx::query_as::<_, AssetRuntimeStateRow>(&format!(
            "SELECT {RUNTIME_STATE_COLUMNS}
             FROM asset_runtime_states
             WHERE user_id = ? AND asset_id = ? AND asset_owner_id = ?"
        ))
        .bind(params.user_id)
        .bind(params.asset_id)
        .bind(&asset_owner_id)
        .fetch_one(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(stored)
    }

    async fn get_runtime_binding(
        &self,
        user_id: &str,
        asset_id: &str,
    ) -> Result<Option<AssetRuntimeBindingRow>, DbError> {
        let sql = format!(
            "SELECT {RUNTIME_BINDING_COLUMNS}
             FROM asset_runtime_bindings binding
             WHERE binding.user_id = ? AND binding.asset_id = ?
               AND binding.asset_owner_id = (
                    SELECT record.user_id
                    FROM asset_records record
                    WHERE record.id = ? AND (
                        record.user_id = ?
                        OR (record.user_id = 'system_default_user' AND record.scope = 'system')
                    )
                    ORDER BY CASE WHEN record.user_id = ? THEN 0 ELSE 1 END
                    LIMIT 1
               )"
        );
        Ok(sqlx::query_as::<_, AssetRuntimeBindingRow>(&sql)
            .bind(user_id)
            .bind(asset_id)
            .bind(asset_id)
            .bind(user_id)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?)
    }

    async fn list_runtime_bindings(
        &self,
        user_id: &str,
        kind: Option<&str>,
    ) -> Result<Vec<AssetRuntimeBindingRow>, DbError> {
        let mut builder = sqlx::QueryBuilder::new(format!(
            "SELECT {RUNTIME_BINDING_COLUMNS}
             FROM asset_runtime_bindings binding
             WHERE binding.user_id = "
        ));
        builder.push_bind(user_id);
        builder.push(
            " AND EXISTS (
                SELECT 1 FROM asset_records record
                WHERE record.user_id = binding.asset_owner_id
                  AND record.id = binding.asset_id
                  AND (
                    record.user_id = ",
        );
        builder.push_bind(user_id);
        builder.push(
            " OR (record.user_id = 'system_default_user' AND record.scope = 'system')
                  )
             )",
        );
        if let Some(kind) = kind {
            builder.push(" AND binding.kind = ");
            builder.push_bind(kind);
        }
        builder.push(" ORDER BY binding.kind, binding.asset_id");
        Ok(builder
            .build_query_as::<AssetRuntimeBindingRow>()
            .fetch_all(&self.pool)
            .await?)
    }

    async fn commit_runtime_binding(
        &self,
        params: CommitAssetRuntimeBindingParams<'_>,
    ) -> Result<AssetRuntimeBindingRow, DbError> {
        let mut transaction = self.pool.begin().await?;
        let (asset_owner_id, asset_kind, definition_digest, portable_runtime_id) =
            resolve_visible_asset(&mut transaction, params.user_id, params.asset_id).await?;
        if asset_kind != params.kind
            || definition_digest != params.definition_digest
            || portable_runtime_id.as_deref() != Some(params.portable_runtime_id)
        {
            return Err(DbError::Conflict(
                "runtime binding does not match the current asset Definition".into(),
            ));
        }
        let overlay_version = sqlx::query_scalar::<_, i64>(
            "SELECT version FROM asset_overlays
             WHERE user_id = ? AND asset_id = ? AND asset_owner_id = ?",
        )
        .bind(params.user_id)
        .bind(params.asset_id)
        .bind(&asset_owner_id)
        .fetch_optional(&mut *transaction)
        .await?;
        match overlay_version {
            Some(version) if version == params.overlay_version => {}
            None if params.overlay_version == 0 => {}
            _ => return Err(DbError::Conflict("runtime binding overlay version is stale".into())),
        }
        let receipt_matches: bool = sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM asset_try_run_receipts
                WHERE user_id = ? AND asset_id = ? AND asset_owner_id = ?
                  AND receipt_id = ? AND definition_digest = ?
                  AND overlay_version = ? AND portable_runtime_id = ?
                  AND projection_runtime_id = ?
             )",
        )
        .bind(params.user_id)
        .bind(params.asset_id)
        .bind(&asset_owner_id)
        .bind(params.try_run_receipt_id)
        .bind(params.definition_digest)
        .bind(params.overlay_version)
        .bind(params.portable_runtime_id)
        .bind(params.projection_runtime_id)
        .fetch_one(&mut *transaction)
        .await?;
        if !receipt_matches {
            return Err(DbError::Conflict(
                "runtime binding has no current successful try-run receipt".into(),
            ));
        }

        sqlx::query(
            "INSERT INTO asset_runtime_bindings (
                user_id, asset_owner_id, asset_id, kind, projection_kind,
                portable_runtime_id, projection_runtime_id,
                definition_digest, overlay_version, health_status, try_run_receipt_id,
                last_error_code, projected_at, health_checked_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(user_id, asset_id) DO UPDATE SET
                asset_owner_id = excluded.asset_owner_id,
                kind = excluded.kind,
                projection_kind = excluded.projection_kind,
                portable_runtime_id = excluded.portable_runtime_id,
                projection_runtime_id = excluded.projection_runtime_id,
                definition_digest = excluded.definition_digest,
                overlay_version = excluded.overlay_version,
                health_status = excluded.health_status,
                try_run_receipt_id = excluded.try_run_receipt_id,
                last_error_code = excluded.last_error_code,
                projected_at = excluded.projected_at,
                health_checked_at = excluded.health_checked_at",
        )
        .bind(params.user_id)
        .bind(&asset_owner_id)
        .bind(params.asset_id)
        .bind(params.kind)
        .bind(params.projection_kind)
        .bind(params.portable_runtime_id)
        .bind(params.projection_runtime_id)
        .bind(params.definition_digest)
        .bind(params.overlay_version)
        .bind(params.health_status)
        .bind(params.try_run_receipt_id)
        .bind(params.last_error_code)
        .bind(params.projected_at)
        .bind(params.health_checked_at)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "INSERT INTO asset_runtime_states (
                user_id, asset_owner_id, asset_id, state, last_error_code, updated_at
             ) VALUES (?, ?, ?, 'active', NULL, ?)
             ON CONFLICT(user_id, asset_id) DO UPDATE SET
                asset_owner_id = excluded.asset_owner_id,
                state = 'active',
                last_error_code = NULL,
                updated_at = excluded.updated_at",
        )
        .bind(params.user_id)
        .bind(&asset_owner_id)
        .bind(params.asset_id)
        .bind(params.projected_at)
        .execute(&mut *transaction)
        .await?;
        let stored = sqlx::query_as::<_, AssetRuntimeBindingRow>(&format!(
            "SELECT {RUNTIME_BINDING_COLUMNS}
             FROM asset_runtime_bindings
             WHERE user_id = ? AND asset_id = ? AND asset_owner_id = ?"
        ))
        .bind(params.user_id)
        .bind(params.asset_id)
        .bind(&asset_owner_id)
        .fetch_one(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(stored)
    }

    async fn deactivate_runtime(
        &self,
        user_id: &str,
        asset_id: &str,
        now: i64,
    ) -> Result<AssetRuntimeStateRow, DbError> {
        let mut transaction = self.pool.begin().await?;
        let (asset_owner_id, asset_kind, _, _) = resolve_visible_asset(&mut transaction, user_id, asset_id).await?;
        sqlx::query(
            "DELETE FROM asset_runtime_bindings
             WHERE user_id = ? AND asset_id = ? AND asset_owner_id = ?",
        )
        .bind(user_id)
        .bind(asset_id)
        .bind(&asset_owner_id)
        .execute(&mut *transaction)
        .await?;
        let has_overlay: bool = sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM asset_overlays
                WHERE user_id = ? AND asset_id = ? AND asset_owner_id = ?
             )",
        )
        .bind(user_id)
        .bind(asset_id)
        .bind(&asset_owner_id)
        .fetch_one(&mut *transaction)
        .await?;
        let state = if has_overlay {
            "inactive"
        } else {
            initial_runtime_state(&asset_kind)
        };
        sqlx::query(
            "INSERT INTO asset_runtime_states (
                user_id, asset_owner_id, asset_id, state, last_error_code, updated_at
             ) VALUES (?, ?, ?, ?, NULL, ?)
             ON CONFLICT(user_id, asset_id) DO UPDATE SET
                asset_owner_id = excluded.asset_owner_id,
                state = excluded.state,
                last_error_code = NULL,
                updated_at = excluded.updated_at",
        )
        .bind(user_id)
        .bind(&asset_owner_id)
        .bind(asset_id)
        .bind(state)
        .bind(now)
        .execute(&mut *transaction)
        .await?;
        let stored = sqlx::query_as::<_, AssetRuntimeStateRow>(&format!(
            "SELECT {RUNTIME_STATE_COLUMNS}
             FROM asset_runtime_states
             WHERE user_id = ? AND asset_id = ? AND asset_owner_id = ?"
        ))
        .bind(user_id)
        .bind(asset_id)
        .bind(&asset_owner_id)
        .fetch_one(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(stored)
    }
}
