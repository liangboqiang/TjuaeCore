use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use tjuaeui_common::{RuntimeAssetDigestInput, compute_runtime_asset_snapshot_id};

use crate::{
    DbError,
    models::{
        ConversationTraceRow, ConversationTraceRuntimeAssetRefRow, ConversationTraceRuntimeAssetSnapshotRow,
        ConversationTraceRuntimeAssetSnapshotSummaryRow, ConversationTraceSpanRow,
    },
    repository::conversation_trace::{
        CONVERSATION_TRACE_MAX_PER_CONVERSATION, CONVERSATION_TRACE_MAX_RUNTIME_ASSETS,
        CONVERSATION_TRACE_MAX_SAFE_ATTRIBUTES_BYTES, CONVERSATION_TRACE_MAX_SPANS, CONVERSATION_TRACE_RETENTION_DAYS,
        CompleteConversationTraceParams, ConversationTraceObservation, ConversationTraceSpanWriteResult,
        IConversationTraceRepository,
    },
};

const MILLIS_PER_DAY: i64 = 24 * 60 * 60 * 1_000;
const TRACE_COLUMNS: &str = "trace_id, conversation_id, status, backend, model, mode, started_at, \
    first_event_at, first_output_at, ended_at, duration_ms, input_size, output_size, input_tokens, \
    output_tokens, total_tokens, cost_usd, error_code, retryable, incomplete, truncated, span_count, \
    dropped_span_count, updated_at";
const SPAN_COLUMNS: &str = "span_id, trace_id, conversation_id, kind, source_id, source_message_id, name, \
    status, started_at, ended_at, duration_ms, safe_attributes, updated_at";
const RUNTIME_ASSET_REF_COLUMNS: &str = "local_asset_id, kind, local_definition_digest, \
    runtime_content_digest, upstream_package, upstream_asset_id, upstream_version, upstream_revision";

#[derive(Clone, Debug)]
pub struct SqliteConversationTraceRepository {
    pool: SqlitePool,
}

impl SqliteConversationTraceRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    async fn fetch_trace(
        executor: &mut Transaction<'_, Sqlite>,
        conversation_id: &str,
        trace_id: &str,
    ) -> Result<Option<ConversationTraceRow>, DbError> {
        let sql = format!("SELECT {TRACE_COLUMNS} FROM conversation_traces WHERE conversation_id = ? AND trace_id = ?");
        Ok(sqlx::query_as::<_, ConversationTraceRow>(&sql)
            .bind(conversation_id)
            .bind(trace_id)
            .fetch_optional(&mut **executor)
            .await?)
    }

    async fn prune_in_transaction(
        transaction: &mut Transaction<'_, Sqlite>,
        conversation_id: &str,
        now: i64,
    ) -> Result<u64, DbError> {
        let cutoff = now.saturating_sub(CONVERSATION_TRACE_RETENTION_DAYS.saturating_mul(MILLIS_PER_DAY));
        let result = sqlx::query(
            "DELETE FROM conversation_traces
             WHERE conversation_id = ?
               AND (
                    started_at < ?
                    OR trace_id NOT IN (
                        SELECT trace_id
                        FROM conversation_traces
                        WHERE conversation_id = ?
                        ORDER BY started_at DESC, trace_id DESC
                        LIMIT ?
                    )
               )",
        )
        .bind(conversation_id)
        .bind(cutoff)
        .bind(conversation_id)
        .bind(i64::from(CONVERSATION_TRACE_MAX_PER_CONVERSATION))
        .execute(&mut **transaction)
        .await?;
        Ok(result.rows_affected())
    }
}

#[async_trait::async_trait]
impl IConversationTraceRepository for SqliteConversationTraceRepository {
    async fn start_trace(&self, trace: &ConversationTraceRow) -> Result<ConversationTraceRow, DbError> {
        let mut transaction = self.pool.begin().await?;
        let backend = sanitize_label(trace.backend.as_deref());
        let model = sanitize_label(trace.model.as_deref());
        let mode = sanitize_label(trace.mode.as_deref());
        sqlx::query(
            "INSERT INTO conversation_traces (
                trace_id, conversation_id, status, backend, model, mode, started_at,
                first_event_at, first_output_at, ended_at, duration_ms, input_size, output_size,
                input_tokens, output_tokens, total_tokens, cost_usd, error_code, retryable,
                incomplete, truncated, span_count, dropped_span_count, updated_at
             ) VALUES (?, ?, 'running', ?, ?, ?, ?, NULL, NULL, NULL, NULL, ?, 0, NULL, NULL, NULL,
                       NULL, NULL, NULL, 0, 0, 0, 0, ?)
             ON CONFLICT(conversation_id, trace_id) DO UPDATE SET
                backend = COALESCE(conversation_traces.backend, excluded.backend),
                model = COALESCE(conversation_traces.model, excluded.model),
                mode = COALESCE(conversation_traces.mode, excluded.mode),
                input_size = MAX(conversation_traces.input_size, excluded.input_size),
                updated_at = MAX(conversation_traces.updated_at, excluded.updated_at)
             WHERE conversation_traces.status = 'running'",
        )
        .bind(&trace.trace_id)
        .bind(&trace.conversation_id)
        .bind(backend)
        .bind(model)
        .bind(mode)
        .bind(trace.started_at)
        .bind(trace.input_size.max(0))
        .bind(trace.updated_at)
        .execute(&mut *transaction)
        .await?;

        Self::prune_in_transaction(&mut transaction, &trace.conversation_id, trace.started_at).await?;
        let stored = Self::fetch_trace(&mut transaction, &trace.conversation_id, &trace.trace_id)
            .await?
            .ok_or_else(|| DbError::NotFound(format!("conversation trace {}", trace.trace_id)))?;
        transaction.commit().await?;
        Ok(stored)
    }

    async fn observe_trace(
        &self,
        conversation_id: &str,
        trace_id: &str,
        observation: ConversationTraceObservation,
    ) -> Result<Option<ConversationTraceRow>, DbError> {
        let output_size_delta = observation.output_size_delta.max(0);
        let result = sqlx::query(
            "UPDATE conversation_traces
             SET first_event_at = COALESCE(first_event_at, ?),
                 first_output_at = CASE
                     WHEN ? THEN COALESCE(first_output_at, ?)
                     ELSE first_output_at
                 END,
                 output_size = output_size + ?,
                 updated_at = ?
             WHERE conversation_id = ? AND trace_id = ? AND status = 'running'",
        )
        .bind(observation.observed_at)
        .bind(observation.output_started)
        .bind(observation.observed_at)
        .bind(output_size_delta)
        .bind(observation.observed_at)
        .bind(conversation_id)
        .bind(trace_id)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }

        let sql = format!("SELECT {TRACE_COLUMNS} FROM conversation_traces WHERE conversation_id = ? AND trace_id = ?");
        Ok(sqlx::query_as::<_, ConversationTraceRow>(&sql)
            .bind(conversation_id)
            .bind(trace_id)
            .fetch_optional(&self.pool)
            .await?)
    }

    async fn complete_trace(
        &self,
        conversation_id: &str,
        trace_id: &str,
        params: CompleteConversationTraceParams<'_>,
    ) -> Result<Option<ConversationTraceRow>, DbError> {
        let status = sanitize_trace_status(params.status);
        let span_status = match status {
            "cancelled" => "cancelled",
            "interrupted" => "interrupted",
            "failed" => "failed",
            _ => "succeeded",
        };
        let error_code = sanitize_error_code(params.error_code);
        let mut transaction = self.pool.begin().await?;

        let result = sqlx::query(
            "UPDATE conversation_traces
             SET status = ?, ended_at = ?, duration_ms = MAX(0, ? - started_at),
                 error_code = ?, retryable = ?, incomplete = ?,
                 dropped_span_count = dropped_span_count + ?,
                 truncated = CASE WHEN ? > 0 THEN 1 ELSE truncated END,
                 updated_at = ?
             WHERE conversation_id = ? AND trace_id = ? AND status = 'running'",
        )
        .bind(status)
        .bind(params.ended_at)
        .bind(params.ended_at)
        .bind(error_code)
        .bind(params.retryable)
        .bind(params.incomplete)
        .bind(params.dropped_span_count.max(0))
        .bind(params.dropped_span_count.max(0))
        .bind(params.ended_at)
        .bind(conversation_id)
        .bind(trace_id)
        .execute(&mut *transaction)
        .await?;
        if result.rows_affected() == 0 {
            transaction.commit().await?;
            return Ok(None);
        }

        sqlx::query(
            "UPDATE conversation_trace_spans
             SET status = ?, ended_at = ?, duration_ms = MAX(0, ? - started_at), updated_at = ?
             WHERE conversation_id = ? AND trace_id = ? AND status = 'running'",
        )
        .bind(span_status)
        .bind(params.ended_at)
        .bind(params.ended_at)
        .bind(params.ended_at)
        .bind(conversation_id)
        .bind(trace_id)
        .execute(&mut *transaction)
        .await?;

        let stored = Self::fetch_trace(&mut transaction, conversation_id, trace_id).await?;
        transaction.commit().await?;
        Ok(stored)
    }

    async fn upsert_span(&self, span: &ConversationTraceSpanRow) -> Result<ConversationTraceSpanWriteResult, DbError> {
        let mut transaction = self.pool.begin().await?;
        let trace_meta = sqlx::query(
            "SELECT status FROM conversation_traces
             WHERE conversation_id = ? AND trace_id = ?",
        )
        .bind(&span.conversation_id)
        .bind(&span.trace_id)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(trace_meta) = trace_meta else {
            return Err(DbError::NotFound(format!("conversation trace {}", span.trace_id)));
        };
        let trace_status: String = trace_meta.try_get("status")?;
        if trace_status != "running" {
            transaction.commit().await?;
            return Ok(ConversationTraceSpanWriteResult::IgnoredTerminalTrace);
        }
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(
                    SELECT 1 FROM conversation_trace_spans
                    WHERE conversation_id = ? AND trace_id = ? AND span_id = ?
                 )",
        )
        .bind(&span.conversation_id)
        .bind(&span.trace_id)
        .bind(&span.span_id)
        .fetch_one(&mut *transaction)
        .await?;
        if !exists {
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM conversation_trace_spans
                 WHERE conversation_id = ? AND trace_id = ?",
            )
            .bind(&span.conversation_id)
            .bind(&span.trace_id)
            .fetch_one(&mut *transaction)
            .await?;
            if count >= CONVERSATION_TRACE_MAX_SPANS {
                sqlx::query(
                    "UPDATE conversation_traces
                     SET truncated = 1, dropped_span_count = dropped_span_count + 1, updated_at = ?
                     WHERE conversation_id = ? AND trace_id = ? AND status = 'running'",
                )
                .bind(span.updated_at)
                .bind(&span.conversation_id)
                .bind(&span.trace_id)
                .execute(&mut *transaction)
                .await?;
                transaction.commit().await?;
                return Ok(ConversationTraceSpanWriteResult::DroppedLimit);
            }
        }

        let (safe_attributes, attributes_truncated) = sanitize_safe_attributes(&span.safe_attributes);
        let kind = sanitize_span_kind(&span.kind);
        let status = sanitize_span_status(&span.status);
        let ended_at = span.ended_at;
        let name = sanitize_span_name(&span.name, kind);
        sqlx::query(
            "INSERT INTO conversation_trace_spans (
                span_id, trace_id, conversation_id, kind, source_id, source_message_id,
                name, status, started_at, ended_at, duration_ms, safe_attributes, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(conversation_id, trace_id, span_id) DO UPDATE SET
                status = excluded.status,
                ended_at = excluded.ended_at,
                duration_ms = CASE
                    WHEN excluded.ended_at IS NULL THEN conversation_trace_spans.duration_ms
                    ELSE MAX(0, excluded.ended_at - conversation_trace_spans.started_at)
                END,
                safe_attributes = excluded.safe_attributes,
                updated_at = excluded.updated_at",
        )
        .bind(&span.span_id)
        .bind(&span.trace_id)
        .bind(&span.conversation_id)
        .bind(kind)
        .bind(sanitize_identifier(span.source_id.as_deref()))
        .bind(sanitize_identifier(span.source_message_id.as_deref()))
        .bind(name)
        .bind(status)
        .bind(span.started_at)
        .bind(ended_at)
        .bind(ended_at.map(|ended_at| ended_at.saturating_sub(span.started_at).max(0)))
        .bind(safe_attributes)
        .bind(span.updated_at)
        .execute(&mut *transaction)
        .await?;

        sqlx::query(
            "UPDATE conversation_traces
             SET span_count = (
                    SELECT COUNT(*) FROM conversation_trace_spans
                    WHERE conversation_id = ? AND trace_id = ?
                 ),
                 truncated = CASE WHEN ? THEN 1 ELSE truncated END,
                 updated_at = MAX(updated_at, ?)
             WHERE conversation_id = ? AND trace_id = ? AND status = 'running'",
        )
        .bind(&span.conversation_id)
        .bind(&span.trace_id)
        .bind(attributes_truncated)
        .bind(span.updated_at)
        .bind(&span.conversation_id)
        .bind(&span.trace_id)
        .execute(&mut *transaction)
        .await?;

        let sql = format!(
            "SELECT {SPAN_COLUMNS} FROM conversation_trace_spans
             WHERE conversation_id = ? AND trace_id = ? AND span_id = ?"
        );
        let stored = sqlx::query_as::<_, ConversationTraceSpanRow>(&sql)
            .bind(&span.conversation_id)
            .bind(&span.trace_id)
            .bind(&span.span_id)
            .fetch_one(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(ConversationTraceSpanWriteResult::Stored(Box::new(stored)))
    }

    async fn record_dropped_spans(
        &self,
        conversation_id: &str,
        trace_id: &str,
        count: i64,
        observed_at: i64,
    ) -> Result<(), DbError> {
        let count = count.max(0);
        if count == 0 {
            return Ok(());
        }
        sqlx::query(
            "UPDATE conversation_traces
             SET dropped_span_count = dropped_span_count + ?, truncated = 1, updated_at = ?
             WHERE conversation_id = ? AND trace_id = ? AND status = 'running'",
        )
        .bind(count)
        .bind(observed_at)
        .bind(conversation_id)
        .bind(trace_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_trace(&self, conversation_id: &str, trace_id: &str) -> Result<Option<ConversationTraceRow>, DbError> {
        let sql = format!("SELECT {TRACE_COLUMNS} FROM conversation_traces WHERE conversation_id = ? AND trace_id = ?");
        Ok(sqlx::query_as::<_, ConversationTraceRow>(&sql)
            .bind(conversation_id)
            .bind(trace_id)
            .fetch_optional(&self.pool)
            .await?)
    }

    async fn list_traces(&self, conversation_id: &str, limit: u32) -> Result<Vec<ConversationTraceRow>, DbError> {
        let limit = limit.clamp(1, CONVERSATION_TRACE_MAX_PER_CONVERSATION);
        let sql = format!(
            "SELECT {TRACE_COLUMNS} FROM conversation_traces
             WHERE conversation_id = ? ORDER BY started_at DESC, trace_id DESC LIMIT ?"
        );
        Ok(sqlx::query_as::<_, ConversationTraceRow>(&sql)
            .bind(conversation_id)
            .bind(i64::from(limit))
            .fetch_all(&self.pool)
            .await?)
    }

    async fn list_spans(
        &self,
        conversation_id: &str,
        trace_id: &str,
    ) -> Result<Vec<ConversationTraceSpanRow>, DbError> {
        let sql = format!(
            "SELECT {SPAN_COLUMNS} FROM conversation_trace_spans
             WHERE conversation_id = ? AND trace_id = ? ORDER BY started_at, span_id LIMIT ?"
        );
        Ok(sqlx::query_as::<_, ConversationTraceSpanRow>(&sql)
            .bind(conversation_id)
            .bind(trace_id)
            .bind(CONVERSATION_TRACE_MAX_SPANS)
            .fetch_all(&self.pool)
            .await?)
    }

    async fn save_runtime_asset_snapshot(
        &self,
        snapshot: &ConversationTraceRuntimeAssetSnapshotRow,
    ) -> Result<ConversationTraceRuntimeAssetSnapshotRow, DbError> {
        let snapshot = sanitize_runtime_asset_snapshot(snapshot)?;
        let mut transaction = self.pool.begin().await?;
        let owned_trace_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1
                FROM conversation_traces AS trace
                JOIN conversations AS conversation
                  ON conversation.id = trace.conversation_id
                WHERE conversation.user_id = ?
                  AND trace.conversation_id = ?
                  AND trace.trace_id = ?
             )",
        )
        .bind(&snapshot.user_id)
        .bind(&snapshot.conversation_id)
        .bind(&snapshot.trace_id)
        .fetch_one(&mut *transaction)
        .await?;
        if !owned_trace_exists {
            return Err(DbError::NotFound(format!(
                "owned conversation trace {}/{}",
                snapshot.conversation_id, snapshot.trace_id
            )));
        }

        if let Some(existing) = fetch_runtime_asset_snapshot(
            &mut transaction,
            &snapshot.user_id,
            &snapshot.conversation_id,
            &snapshot.trace_id,
        )
        .await?
        {
            if same_runtime_asset_snapshot_payload(&existing, &snapshot) {
                transaction.commit().await?;
                return Ok(existing);
            }
            return Err(DbError::Conflict(format!(
                "conversation trace runtime asset snapshot {}/{}",
                snapshot.conversation_id, snapshot.trace_id
            )));
        }

        sqlx::query(
            "INSERT INTO conversation_trace_runtime_asset_snapshots (
                user_id, conversation_id, trace_id, runtime_snapshot_id, created_at
             ) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&snapshot.user_id)
        .bind(&snapshot.conversation_id)
        .bind(&snapshot.trace_id)
        .bind(&snapshot.runtime_snapshot_id)
        .bind(snapshot.created_at)
        .execute(&mut *transaction)
        .await?;

        for (position, asset) in snapshot.assets.iter().enumerate() {
            sqlx::query(
                "INSERT INTO conversation_trace_runtime_asset_refs (
                    user_id, conversation_id, trace_id, position, local_asset_id, kind,
                    local_definition_digest, runtime_content_digest, upstream_package,
                    upstream_asset_id, upstream_version, upstream_revision
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&snapshot.user_id)
            .bind(&snapshot.conversation_id)
            .bind(&snapshot.trace_id)
            .bind(i64::try_from(position).unwrap_or(i64::MAX))
            .bind(&asset.local_asset_id)
            .bind(&asset.kind)
            .bind(&asset.local_definition_digest)
            .bind(&asset.runtime_content_digest)
            .bind(&asset.upstream_package)
            .bind(&asset.upstream_asset_id)
            .bind(&asset.upstream_version)
            .bind(&asset.upstream_revision)
            .execute(&mut *transaction)
            .await?;
        }

        let stored = fetch_runtime_asset_snapshot(
            &mut transaction,
            &snapshot.user_id,
            &snapshot.conversation_id,
            &snapshot.trace_id,
        )
        .await?
        .ok_or_else(|| DbError::NotFound(format!("runtime asset snapshot {}", snapshot.runtime_snapshot_id)))?;
        transaction.commit().await?;
        Ok(stored)
    }

    async fn get_runtime_asset_snapshot(
        &self,
        user_id: &str,
        conversation_id: &str,
        trace_id: &str,
    ) -> Result<Option<ConversationTraceRuntimeAssetSnapshotRow>, DbError> {
        let mut transaction = self.pool.begin().await?;
        let snapshot = fetch_runtime_asset_snapshot(&mut transaction, user_id, conversation_id, trace_id).await?;
        transaction.commit().await?;
        Ok(snapshot)
    }

    async fn list_runtime_asset_snapshot_summaries(
        &self,
        user_id: &str,
        conversation_id: &str,
    ) -> Result<Vec<ConversationTraceRuntimeAssetSnapshotSummaryRow>, DbError> {
        Ok(sqlx::query_as::<_, ConversationTraceRuntimeAssetSnapshotSummaryRow>(
            "SELECT snapshot.trace_id, snapshot.runtime_snapshot_id
             FROM conversation_trace_runtime_asset_snapshots AS snapshot
             JOIN conversations AS conversation
               ON conversation.id = snapshot.conversation_id
              AND conversation.user_id = snapshot.user_id
             WHERE snapshot.user_id = ? AND snapshot.conversation_id = ?
             ORDER BY snapshot.created_at DESC, snapshot.trace_id DESC",
        )
        .bind(user_id)
        .bind(conversation_id)
        .fetch_all(&self.pool)
        .await?)
    }

    async fn interrupt_running_traces(&self, interrupted_at: i64) -> Result<u64, DbError> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            "UPDATE conversation_trace_spans
             SET status = 'interrupted', ended_at = ?, duration_ms = MAX(0, ? - started_at), updated_at = ?
             WHERE status = 'running'",
        )
        .bind(interrupted_at)
        .bind(interrupted_at)
        .bind(interrupted_at)
        .execute(&mut *transaction)
        .await?;
        let result = sqlx::query(
            "UPDATE conversation_traces
             SET status = 'interrupted', ended_at = ?, duration_ms = MAX(0, ? - started_at),
                 incomplete = 1, updated_at = ?
             WHERE status = 'running'",
        )
        .bind(interrupted_at)
        .bind(interrupted_at)
        .bind(interrupted_at)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(result.rows_affected())
    }

    async fn prune_traces(&self, conversation_id: &str, now: i64) -> Result<u64, DbError> {
        let mut transaction = self.pool.begin().await?;
        let deleted = Self::prune_in_transaction(&mut transaction, conversation_id, now).await?;
        transaction.commit().await?;
        Ok(deleted)
    }

    async fn prune_expired_traces(&self, now: i64) -> Result<u64, DbError> {
        let cutoff = now.saturating_sub(CONVERSATION_TRACE_RETENTION_DAYS.saturating_mul(MILLIS_PER_DAY));
        let result = sqlx::query("DELETE FROM conversation_traces WHERE started_at < ?")
            .bind(cutoff)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}

async fn fetch_runtime_asset_snapshot(
    transaction: &mut Transaction<'_, Sqlite>,
    user_id: &str,
    conversation_id: &str,
    trace_id: &str,
) -> Result<Option<ConversationTraceRuntimeAssetSnapshotRow>, DbError> {
    let header = sqlx::query(
        "SELECT snapshot.runtime_snapshot_id, snapshot.created_at
         FROM conversation_trace_runtime_asset_snapshots AS snapshot
         JOIN conversations AS conversation
           ON conversation.id = snapshot.conversation_id
          AND conversation.user_id = snapshot.user_id
         WHERE snapshot.user_id = ?
           AND snapshot.conversation_id = ?
           AND snapshot.trace_id = ?",
    )
    .bind(user_id)
    .bind(conversation_id)
    .bind(trace_id)
    .fetch_optional(&mut **transaction)
    .await?;
    let Some(header) = header else {
        return Ok(None);
    };
    let sql = format!(
        "SELECT {RUNTIME_ASSET_REF_COLUMNS}
         FROM conversation_trace_runtime_asset_refs
         WHERE user_id = ? AND conversation_id = ? AND trace_id = ?
         ORDER BY position"
    );
    let assets = sqlx::query_as::<_, ConversationTraceRuntimeAssetRefRow>(&sql)
        .bind(user_id)
        .bind(conversation_id)
        .bind(trace_id)
        .fetch_all(&mut **transaction)
        .await?;
    Ok(Some(ConversationTraceRuntimeAssetSnapshotRow {
        user_id: user_id.to_owned(),
        conversation_id: conversation_id.to_owned(),
        trace_id: trace_id.to_owned(),
        runtime_snapshot_id: header.try_get("runtime_snapshot_id")?,
        assets,
        created_at: header.try_get("created_at")?,
    }))
}

fn sanitize_runtime_asset_snapshot(
    snapshot: &ConversationTraceRuntimeAssetSnapshotRow,
) -> Result<ConversationTraceRuntimeAssetSnapshotRow, DbError> {
    if !is_sha256_digest(&snapshot.runtime_snapshot_id) {
        return Err(DbError::Init("runtime snapshot id 必须是小写 sha256 摘要".into()));
    }
    if snapshot.assets.is_empty() || snapshot.assets.len() > CONVERSATION_TRACE_MAX_RUNTIME_ASSETS {
        return Err(DbError::Init(format!(
            "runtime asset 数量必须在 1 到 {CONVERSATION_TRACE_MAX_RUNTIME_ASSETS} 之间"
        )));
    }

    let mut seen = BTreeSet::new();
    let mut assets = Vec::with_capacity(snapshot.assets.len());
    for asset in &snapshot.assets {
        let local_asset_id = safe_runtime_asset_id(&asset.local_asset_id)
            .ok_or_else(|| DbError::Init("runtime local asset id 不安全".into()))?;
        let kind =
            safe_runtime_asset_kind(&asset.kind).ok_or_else(|| DbError::Init("runtime asset kind 不受支持".into()))?;
        if !is_sha256_digest(&asset.local_definition_digest) {
            return Err(DbError::Init(
                "runtime asset local definition digest 必须是小写 sha256 摘要".into(),
            ));
        }
        if !is_sha256_digest(&asset.runtime_content_digest) {
            return Err(DbError::Init(
                "runtime asset content digest 必须是小写 sha256 摘要".into(),
            ));
        }
        if !seen.insert((kind.to_owned(), local_asset_id.clone())) {
            return Err(DbError::Conflict(format!(
                "duplicate runtime asset {kind}:{local_asset_id}"
            )));
        }
        let upstream_package =
            sanitize_optional_runtime_label(asset.upstream_package.as_deref(), 256, RuntimeLabelKind::Package)?;
        let upstream_asset_id =
            sanitize_optional_runtime_label(asset.upstream_asset_id.as_deref(), 256, RuntimeLabelKind::AssetId)?;
        let upstream_version =
            sanitize_optional_runtime_label(asset.upstream_version.as_deref(), 128, RuntimeLabelKind::Version)?;
        let upstream_revision =
            sanitize_optional_runtime_label(asset.upstream_revision.as_deref(), 256, RuntimeLabelKind::Version)?;
        let upstream_presence = [
            upstream_package.is_some(),
            upstream_asset_id.is_some(),
            upstream_version.is_some(),
            upstream_revision.is_some(),
        ];
        if upstream_presence.iter().any(|present| *present) && !upstream_presence.iter().all(|present| *present) {
            return Err(DbError::Init(
                "runtime asset upstream provenance 必须完整或全部为空".into(),
            ));
        }
        assets.push(ConversationTraceRuntimeAssetRefRow {
            local_asset_id,
            kind: kind.to_owned(),
            local_definition_digest: asset.local_definition_digest.clone(),
            runtime_content_digest: asset.runtime_content_digest.clone(),
            upstream_package,
            upstream_asset_id,
            upstream_version,
            upstream_revision,
        });
    }
    assets.sort_by(|left, right| (&left.kind, &left.local_asset_id).cmp(&(&right.kind, &right.local_asset_id)));
    let canonical_snapshot_id = compute_runtime_asset_snapshot_id(
        assets
            .iter()
            .map(|asset| RuntimeAssetDigestInput {
                local_asset_id: &asset.local_asset_id,
                kind: &asset.kind,
                local_definition_digest: &asset.local_definition_digest,
                runtime_content_digest: &asset.runtime_content_digest,
                upstream_package: asset.upstream_package.as_deref(),
                upstream_asset_id: asset.upstream_asset_id.as_deref(),
                upstream_version: asset.upstream_version.as_deref(),
                upstream_revision: asset.upstream_revision.as_deref(),
            })
            .collect(),
    );
    if snapshot.runtime_snapshot_id != canonical_snapshot_id {
        return Err(DbError::Init("runtime snapshot id 与规范化资产集合不匹配".into()));
    }

    Ok(ConversationTraceRuntimeAssetSnapshotRow {
        user_id: snapshot.user_id.clone(),
        conversation_id: snapshot.conversation_id.clone(),
        trace_id: snapshot.trace_id.clone(),
        runtime_snapshot_id: snapshot.runtime_snapshot_id.clone(),
        assets,
        created_at: snapshot.created_at,
    })
}

fn same_runtime_asset_snapshot_payload(
    left: &ConversationTraceRuntimeAssetSnapshotRow,
    right: &ConversationTraceRuntimeAssetSnapshotRow,
) -> bool {
    left.user_id == right.user_id
        && left.conversation_id == right.conversation_id
        && left.trace_id == right.trace_id
        && left.runtime_snapshot_id == right.runtime_snapshot_id
        && left.assets == right.assets
}

fn is_sha256_digest(value: &str) -> bool {
    value.strip_prefix("sha256-").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn safe_runtime_asset_id(value: &str) -> Option<String> {
    if value != value.trim()
        || value.is_empty()
        || value.len() > 256
        || value.starts_with("sk-")
        || value.to_ascii_lowercase().starts_with("bearer")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':' | b'@' | b'+'))
    {
        None
    } else {
        Some(value.to_owned())
    }
}

fn safe_runtime_asset_kind(value: &str) -> Option<&'static str> {
    match value {
        "assistant" => Some("assistant"),
        "engineAdapter" => Some("engineAdapter"),
        "skill" => Some("skill"),
        "mcp" => Some("mcp"),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy)]
enum RuntimeLabelKind {
    Package,
    AssetId,
    Version,
}

fn sanitize_optional_runtime_label(
    value: Option<&str>,
    max_len: usize,
    kind: RuntimeLabelKind,
) -> Result<Option<String>, DbError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let lower = value.to_ascii_lowercase();
    let allowed =
        |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'@' | b'+' | b'/' | b':');
    let valid_shape = value == value.trim()
        && !value.is_empty()
        && value.len() <= max_len
        && !value.starts_with(['/', '\\'])
        && !value.contains('\\')
        && !value.contains("..")
        && !value.contains("://")
        && !lower.starts_with("bearer")
        && !lower.starts_with("sk-")
        && !lower.starts_with("ghp_")
        && !lower.contains("token=")
        && value.bytes().all(allowed)
        && !(value.len() >= 3 && value.as_bytes()[1] == b':' && matches!(value.as_bytes()[2], b'/' | b'\\'));
    let kind_valid = match kind {
        RuntimeLabelKind::Package => value.contains('/') || !value.contains(':'),
        RuntimeLabelKind::AssetId | RuntimeLabelKind::Version => true,
    };
    if !valid_shape || !kind_valid {
        return Err(DbError::Init("runtime asset upstream 标签不安全".into()));
    }
    Ok(Some(value.to_owned()))
}

fn sanitize_label(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    let lower = value.to_ascii_lowercase();
    if value.is_empty()
        || value.len() > 128
        || value.chars().any(char::is_control)
        || value.contains("://")
        || value.starts_with('/')
        || value.starts_with('\\')
        || value.contains('\\')
        || value.contains("..")
        || lower.starts_with("sk-")
        || lower.starts_with("bearer ")
        || lower.contains("api_key")
        || lower.contains("token=")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':' | b'/'))
        || (value.len() >= 3 && value.as_bytes()[1] == b':' && matches!(value.as_bytes()[2], b'/' | b'\\'))
    {
        return None;
    }
    Some(value.to_owned())
}

fn sanitize_identifier(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty()
        || value.len() > 256
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        None
    } else {
        Some(value.to_owned())
    }
}

fn sanitize_error_code(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty()
        || value.len() > 96
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        None
    } else {
        Some(value.to_owned())
    }
}

fn sanitize_trace_status(value: &str) -> &'static str {
    match value {
        "succeeded" => "succeeded",
        "failed" => "failed",
        "cancelled" => "cancelled",
        "interrupted" => "interrupted",
        _ => "failed",
    }
}

fn sanitize_span_kind(value: &str) -> &'static str {
    match value {
        "thinking" => "thinking",
        "permission" => "permission",
        _ => "tool",
    }
}

fn sanitize_span_status(value: &str) -> &'static str {
    match value {
        "succeeded" => "succeeded",
        "failed" => "failed",
        "cancelled" => "cancelled",
        "interrupted" => "interrupted",
        _ => "running",
    }
}

fn sanitize_span_name(value: &str, kind: &str) -> String {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        kind.to_owned()
    } else {
        value.to_owned()
    }
}

fn sanitize_safe_attributes(raw: &str) -> (String, bool) {
    let Ok(Value::Object(attributes)) = serde_json::from_str::<Value>(raw) else {
        return ("{}".to_owned(), true);
    };
    let mut safe = BTreeMap::<String, Value>::new();
    let mut changed = false;
    for (key, value) in attributes {
        let accepted = match (key.as_str(), value) {
            ("synthetic", Value::Bool(value)) => Some(Value::Bool(value)),
            ("exit_code", Value::Number(value)) if value.as_i64().is_some() => Some(Value::Number(value)),
            ("tool_kind", Value::String(value))
                if matches!(
                    value.as_str(),
                    "read"
                        | "edit"
                        | "delete"
                        | "move"
                        | "search"
                        | "execute"
                        | "think"
                        | "fetch"
                        | "switch_mode"
                        | "other"
                ) =>
            {
                Some(Value::String(value))
            }
            ("permission_kind", Value::String(value))
                if matches!(value.as_str(), "tool" | "filesystem" | "network" | "other") =>
            {
                Some(Value::String(value))
            }
            _ => None,
        };
        if let Some(value) = accepted {
            safe.insert(key, value);
        } else {
            changed = true;
        }
    }
    let serialized = serde_json::to_string(&safe).unwrap_or_else(|_| "{}".to_owned());
    if serialized.len() > CONVERSATION_TRACE_MAX_SAFE_ATTRIBUTES_BYTES {
        ("{}".to_owned(), true)
    } else {
        (serialized, changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_attributes_drop_unknown_or_textual_values() {
        let (sanitized, truncated) =
            sanitize_safe_attributes(r#"{"tool_kind":"read","command":"secret","synthetic":true}"#);
        assert!(truncated);
        assert_eq!(sanitized, r#"{"synthetic":true,"tool_kind":"read"}"#);
    }

    #[test]
    fn labels_reject_urls_and_absolute_paths() {
        assert_eq!(sanitize_label(Some("codex")), Some("codex".to_owned()));
        assert_eq!(sanitize_label(Some("https://example.invalid")), None);
        assert_eq!(sanitize_label(Some(r"C:\secret\file")), None);
    }
}
