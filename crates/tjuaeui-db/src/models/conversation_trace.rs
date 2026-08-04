use serde::{Deserialize, Serialize};
use tjuaeui_common::TimestampMs;

/// Privacy-preserving operational summary for one conversation turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, sqlx::FromRow)]
pub struct ConversationTraceRow {
    pub trace_id: String,
    pub conversation_id: String,
    pub status: String,
    pub backend: Option<String>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub started_at: TimestampMs,
    pub first_event_at: Option<TimestampMs>,
    pub first_output_at: Option<TimestampMs>,
    pub ended_at: Option<TimestampMs>,
    pub duration_ms: Option<i64>,
    pub input_size: i64,
    pub output_size: i64,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    pub error_code: Option<String>,
    pub retryable: Option<bool>,
    pub incomplete: bool,
    pub truncated: bool,
    pub span_count: i64,
    pub dropped_span_count: i64,
    pub updated_at: TimestampMs,
}

/// Sanitized thinking/tool/permission lifecycle metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, sqlx::FromRow)]
pub struct ConversationTraceSpanRow {
    pub span_id: String,
    pub trace_id: String,
    pub conversation_id: String,
    pub kind: String,
    pub source_id: Option<String>,
    pub source_message_id: Option<String>,
    pub name: String,
    pub status: String,
    pub started_at: TimestampMs,
    pub ended_at: Option<TimestampMs>,
    pub duration_ms: Option<i64>,
    pub safe_attributes: String,
    pub updated_at: TimestampMs,
}

/// Safe identity of one asset definition accepted by a conversation runtime.
///
/// This persistence model intentionally has no root, path, content or
/// environment fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, sqlx::FromRow)]
pub struct ConversationTraceRuntimeAssetRefRow {
    pub local_asset_id: String,
    pub kind: String,
    pub local_definition_digest: String,
    /// Internal evidence used to validate the runtime-produced receipt.
    /// Trace API responses deliberately omit this implementation digest.
    pub runtime_content_digest: String,
    pub upstream_package: Option<String>,
    pub upstream_asset_id: Option<String>,
    pub upstream_version: Option<String>,
    pub upstream_revision: Option<String>,
}

/// Immutable runtime asset receipt associated with exactly one owned trace.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConversationTraceRuntimeAssetSnapshotRow {
    pub user_id: String,
    pub conversation_id: String,
    pub trace_id: String,
    pub runtime_snapshot_id: String,
    pub assets: Vec<ConversationTraceRuntimeAssetRefRow>,
    pub created_at: TimestampMs,
}

/// Lightweight association used by the Trace list endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, sqlx::FromRow)]
pub struct ConversationTraceRuntimeAssetSnapshotSummaryRow {
    pub trace_id: String,
    pub runtime_snapshot_id: String,
}
