use crate::{
    DbError,
    models::{
        ConversationTraceRow, ConversationTraceRuntimeAssetSnapshotRow,
        ConversationTraceRuntimeAssetSnapshotSummaryRow, ConversationTraceSpanRow,
    },
};

pub const CONVERSATION_TRACE_RETENTION_DAYS: i64 = 30;
pub const CONVERSATION_TRACE_MAX_PER_CONVERSATION: u32 = 100;
pub const CONVERSATION_TRACE_MAX_SPANS: i64 = 500;
pub const CONVERSATION_TRACE_MAX_SAFE_ATTRIBUTES_BYTES: usize = 4 * 1024;
pub const CONVERSATION_TRACE_MAX_RUNTIME_ASSETS: usize = 256;

#[derive(Debug, Clone, Copy)]
pub struct ConversationTraceObservation {
    pub observed_at: i64,
    pub output_started: bool,
    pub output_size_delta: i64,
}

#[derive(Debug, Clone)]
pub struct CompleteConversationTraceParams<'a> {
    pub status: &'a str,
    pub ended_at: i64,
    pub error_code: Option<&'a str>,
    pub retryable: Option<bool>,
    pub incomplete: bool,
    pub dropped_span_count: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConversationTraceSpanWriteResult {
    Stored(Box<ConversationTraceSpanRow>),
    DroppedLimit,
    IgnoredTerminalTrace,
}

/// Persistence boundary for privacy-preserving conversation execution traces.
#[async_trait::async_trait]
pub trait IConversationTraceRepository: Send + Sync {
    async fn start_trace(&self, trace: &ConversationTraceRow) -> Result<ConversationTraceRow, DbError>;

    async fn observe_trace(
        &self,
        conversation_id: &str,
        trace_id: &str,
        observation: ConversationTraceObservation,
    ) -> Result<Option<ConversationTraceRow>, DbError>;

    async fn complete_trace(
        &self,
        conversation_id: &str,
        trace_id: &str,
        params: CompleteConversationTraceParams<'_>,
    ) -> Result<Option<ConversationTraceRow>, DbError>;

    async fn upsert_span(&self, span: &ConversationTraceSpanRow) -> Result<ConversationTraceSpanWriteResult, DbError>;

    async fn record_dropped_spans(
        &self,
        conversation_id: &str,
        trace_id: &str,
        count: i64,
        observed_at: i64,
    ) -> Result<(), DbError>;

    async fn get_trace(&self, conversation_id: &str, trace_id: &str) -> Result<Option<ConversationTraceRow>, DbError>;

    async fn list_traces(&self, conversation_id: &str, limit: u32) -> Result<Vec<ConversationTraceRow>, DbError>;

    async fn list_spans(&self, conversation_id: &str, trace_id: &str)
    -> Result<Vec<ConversationTraceSpanRow>, DbError>;

    /// Persist an immutable, runtime-confirmed asset receipt.
    ///
    /// The repository verifies the `(user, conversation, trace)` ownership
    /// tuple and rejects a conflicting second receipt. The accepted model has
    /// no field capable of carrying local roots, contents or environment data.
    async fn save_runtime_asset_snapshot(
        &self,
        snapshot: &ConversationTraceRuntimeAssetSnapshotRow,
    ) -> Result<ConversationTraceRuntimeAssetSnapshotRow, DbError>;

    async fn get_runtime_asset_snapshot(
        &self,
        user_id: &str,
        conversation_id: &str,
        trace_id: &str,
    ) -> Result<Option<ConversationTraceRuntimeAssetSnapshotRow>, DbError>;

    async fn list_runtime_asset_snapshot_summaries(
        &self,
        user_id: &str,
        conversation_id: &str,
    ) -> Result<Vec<ConversationTraceRuntimeAssetSnapshotSummaryRow>, DbError>;

    /// Mark traces left running by a previous process as interrupted.
    async fn interrupt_running_traces(&self, interrupted_at: i64) -> Result<u64, DbError>;

    /// Apply both the age and per-conversation count retention bounds.
    async fn prune_traces(&self, conversation_id: &str, now: i64) -> Result<u64, DbError>;

    /// Remove traces older than the global age bound, including inactive conversations.
    async fn prune_expired_traces(&self, now: i64) -> Result<u64, DbError>;
}
