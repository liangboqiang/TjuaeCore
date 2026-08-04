use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::json;
use tjuaeui_api_types::{
    ConversationTraceRuntimeAssetRef, ConversationTraceRuntimeAssetSnapshot, ConversationTraceSpan,
    ConversationTraceSpanKind, ConversationTraceSpanStatus, ConversationTraceStatus, ConversationTraceSummary,
    ConversationTraceUpdateKind, ConversationTraceUpdatedPayload, WebSocketMessage,
};
use tjuaeui_common::now_ms;
use tjuaeui_db::{
    CompleteConversationTraceParams, ConversationTraceObservation, ConversationTraceRow,
    ConversationTraceRuntimeAssetSnapshotRow, ConversationTraceSpanRow, ConversationTraceSpanWriteResult, DbError,
    IConversationTraceRepository,
};
use tjuaeui_realtime::EventBroadcaster;
use tokio::sync::{mpsc, oneshot};
use tracing::warn;

const TRACE_COMMAND_CAPACITY: usize = 512;
const TRACE_LIFECYCLE_RESERVE: usize = 64;
const TRACE_DATA_CAPACITY: usize = TRACE_COMMAND_CAPACITY - TRACE_LIFECYCLE_RESERVE;
const TRACE_IO_TIMEOUT: Duration = Duration::from_secs(2);
const TRACE_RECEIPT_ACK_TIMEOUT: Duration = Duration::from_secs(5);
const TRACE_RETENTION_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Debug, Clone, Default)]
pub struct ConversationTraceStartContext {
    pub backend: Option<String>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub input_size: i64,
}

pub(crate) struct ConversationTraceCompletion {
    pub status: &'static str,
    pub ended_at: i64,
    pub output_size: i64,
    pub error_code: Option<String>,
    pub retryable: Option<bool>,
    pub incomplete: bool,
    pub dropped_span_count: i64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum RecordedSpanKind {
    Thinking,
    Tool,
    Permission,
    Runtime,
}

impl RecordedSpanKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Thinking => "thinking",
            Self::Tool => "tool",
            Self::Permission => "permission",
            Self::Runtime => "runtime",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum RecordedSpanStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
}

impl RecordedSpanStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }
}

pub(crate) struct RecordedSpan<'a> {
    pub kind: RecordedSpanKind,
    pub source_id: Option<&'a str>,
    pub source_message_id: Option<&'a str>,
    pub name: &'a str,
    pub status: RecordedSpanStatus,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub safe_attributes: serde_json::Value,
}

pub(crate) struct RuntimeSpan<'a> {
    pub phase: &'a str,
    pub status: RecordedSpanStatus,
    pub started_at: i64,
    pub ended_at: i64,
    pub asset_kind: Option<&'a str>,
    pub local_asset_id: Option<&'a str>,
    pub error_code: Option<&'a str>,
}

#[derive(Debug)]
enum TraceCommand {
    Start {
        owner_user_id: String,
        trace: ConversationTraceRow,
    },
    Observe {
        conversation_id: String,
        trace_id: String,
        observed_at: i64,
        output_started: bool,
    },
    RuntimeAssetsLoaded {
        snapshot: ConversationTraceRuntimeAssetSnapshotRow,
        persisted: oneshot::Sender<bool>,
    },
    Span(ConversationTraceSpanRow),
    Flush {
        conversation_id: String,
        trace_id: String,
        observed_at: i64,
        output_size: i64,
        dropped_span_count: i64,
    },
    Complete {
        conversation_id: String,
        trace_id: String,
        status: &'static str,
        ended_at: i64,
        output_size: i64,
        error_code: Option<String>,
        retryable: Option<bool>,
        incomplete: bool,
        dropped_span_count: i64,
    },
}

impl TraceCommand {
    fn identifiers(&self) -> (&str, &str) {
        match self {
            Self::Start { trace, .. } => (&trace.conversation_id, &trace.trace_id),
            Self::Observe {
                conversation_id,
                trace_id,
                ..
            }
            | Self::Flush {
                conversation_id,
                trace_id,
                ..
            }
            | Self::Complete {
                conversation_id,
                trace_id,
                ..
            } => (conversation_id, trace_id),
            Self::RuntimeAssetsLoaded { snapshot, .. } => (&snapshot.conversation_id, &snapshot.trace_id),
            Self::Span(span) => (&span.conversation_id, &span.trace_id),
        }
    }
}

#[derive(Debug)]
struct QueuedTraceCommand {
    command: TraceCommand,
    data_slot: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TraceKey {
    conversation_id: String,
    trace_id: String,
}

impl TraceKey {
    fn new(conversation_id: String, trace_id: String) -> Self {
        Self {
            conversation_id,
            trace_id,
        }
    }
}

#[derive(Debug, Clone)]
struct ActiveTrace {
    owner_user_id: String,
    runtime_asset_snapshot: Option<ConversationTraceRuntimeAssetSnapshotRow>,
}

/// A single bounded writer serializes all Trace persistence for a conversation
/// service. Ordinary telemetry remains non-blocking. The runtime asset receipt
/// boundary awaits one bounded acknowledgement because execution must not begin
/// unless its audited, actual-load receipt was durably accepted.
pub(crate) struct ConversationTraceWriter {
    tx: mpsc::Sender<QueuedTraceCommand>,
    queued_data: Arc<AtomicUsize>,
}

impl ConversationTraceWriter {
    pub(crate) fn spawn(
        repository: Arc<dyn IConversationTraceRepository>,
        broadcaster: Arc<dyn EventBroadcaster>,
    ) -> Arc<Self> {
        Self::spawn_with_retention_interval(repository, broadcaster, TRACE_RETENTION_INTERVAL)
    }

    fn spawn_with_retention_interval(
        repository: Arc<dyn IConversationTraceRepository>,
        broadcaster: Arc<dyn EventBroadcaster>,
        retention_interval: Duration,
    ) -> Arc<Self> {
        let (tx, rx) = mpsc::channel(TRACE_COMMAND_CAPACITY);
        let queued_data = Arc::new(AtomicUsize::new(0));
        tokio::spawn(run_trace_writer(
            repository,
            broadcaster,
            rx,
            Arc::clone(&queued_data),
            retention_interval,
        ));
        Arc::new(Self { tx, queued_data })
    }

    pub(crate) fn start(
        &self,
        owner_user_id: String,
        conversation_id: String,
        trace_id: String,
        started_at: i64,
        context: ConversationTraceStartContext,
    ) {
        let trace = ConversationTraceRow {
            trace_id,
            conversation_id,
            status: "running".to_owned(),
            backend: context.backend,
            model: context.model,
            mode: context.mode,
            started_at,
            first_event_at: None,
            first_output_at: None,
            ended_at: None,
            duration_ms: None,
            input_size: context.input_size.max(0),
            output_size: 0,
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            cost_usd: None,
            error_code: None,
            retryable: None,
            incomplete: false,
            truncated: false,
            span_count: 0,
            dropped_span_count: 0,
            updated_at: started_at,
        };
        self.submit_lifecycle(TraceCommand::Start { owner_user_id, trace });
    }

    pub(crate) fn complete(
        &self,
        conversation_id: String,
        trace_id: String,
        completion: ConversationTraceCompletion,
    ) -> bool {
        self.submit_lifecycle(TraceCommand::Complete {
            conversation_id,
            trace_id,
            status: completion.status,
            ended_at: completion.ended_at,
            output_size: completion.output_size,
            error_code: completion.error_code,
            retryable: completion.retryable,
            incomplete: completion.incomplete,
            dropped_span_count: completion.dropped_span_count,
        })
    }

    pub(crate) async fn runtime_assets_loaded(&self, snapshot: ConversationTraceRuntimeAssetSnapshotRow) -> bool {
        let (persisted, receiver) = oneshot::channel();
        if !self.submit_lifecycle(TraceCommand::RuntimeAssetsLoaded { snapshot, persisted }) {
            return false;
        }
        matches!(
            tokio::time::timeout(TRACE_RECEIPT_ACK_TIMEOUT, receiver).await,
            Ok(Ok(true))
        )
    }

    /// Record one runtime lifecycle boundary using an explicit safe-field
    /// allow-list. Paths, commands, environment values and protocol payloads
    /// cannot enter this span shape.
    pub(crate) fn record_runtime_span(&self, conversation_id: &str, trace_id: &str, span: RuntimeSpan<'_>) -> bool {
        let Some(phase) = safe_identifier(span.phase) else {
            return false;
        };
        let asset_kind = span.asset_kind.and_then(safe_identifier);
        let local_asset_id = span.local_asset_id.and_then(safe_identifier);
        let error_code = span.error_code.and_then(safe_identifier);
        let source_id = local_asset_id
            .as_ref()
            .map(|asset_id| format!("{phase}:{asset_id}"))
            .or_else(|| Some(phase.clone()));
        let safe_attributes = json!({
            "phase": phase,
            "assetKind": asset_kind,
            "localAssetId": local_asset_id,
            "errorCode": error_code,
        });
        let ended_at = span.ended_at.max(span.started_at);
        self.submit_data(TraceCommand::Span(ConversationTraceSpanRow {
            span_id: format!("{trace_id}:runtime:{}", source_id.as_deref().unwrap_or("runtime")),
            trace_id: trace_id.to_owned(),
            conversation_id: conversation_id.to_owned(),
            kind: RecordedSpanKind::Runtime.as_str().to_owned(),
            source_id,
            source_message_id: None,
            name: safe_span_name(span.phase, "runtime"),
            status: span.status.as_str().to_owned(),
            started_at: span.started_at,
            ended_at: Some(ended_at),
            duration_ms: Some(ended_at.saturating_sub(span.started_at)),
            safe_attributes: safe_attributes.to_string(),
            updated_at: ended_at,
        }))
    }

    fn submit_data(&self, command: TraceCommand) -> bool {
        if self
            .queued_data
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |queued| {
                (queued < TRACE_DATA_CAPACITY).then_some(queued + 1)
            })
            .is_err()
        {
            let (conversation_id, trace_id) = command.identifiers();
            warn!(
                conversation_id,
                trace_id,
                data_capacity = TRACE_DATA_CAPACITY,
                "对话 Trace 数据队列达到高水位，已保留生命周期命令容量"
            );
            return false;
        }

        if self.submit(command, true) {
            true
        } else {
            self.queued_data.fetch_sub(1, Ordering::AcqRel);
            false
        }
    }

    fn submit_lifecycle(&self, command: TraceCommand) -> bool {
        self.submit(command, false)
    }

    fn submit(&self, command: TraceCommand, data_slot: bool) -> bool {
        let (conversation_id, trace_id) = command.identifiers();
        let trace_id = trace_id.to_owned();
        let conversation_id = conversation_id.to_owned();
        let command_class = if data_slot { "data" } else { "lifecycle" };
        match self.tx.try_send(QueuedTraceCommand { command, data_slot }) {
            Ok(()) => true,
            Err(error) => {
                let reason = match error {
                    mpsc::error::TrySendError::Full(_) => "queue_full",
                    mpsc::error::TrySendError::Closed(_) => "writer_closed",
                };
                warn!(
                    trace_id,
                    conversation_id, reason, command_class, "对话 Trace 写入队列拒绝了更新"
                );
                false
            }
        }
    }
}

/// Non-blocking, bounded producer used by the stream relay.
///
/// It accepts lifecycle metadata only. There is deliberately no method that
/// accepts prompt text, assistant text, thinking text, tool input or output.
pub(crate) struct ConversationTraceSink {
    conversation_id: String,
    trace_id: String,
    writer: Arc<ConversationTraceWriter>,
    saw_event: AtomicBool,
    saw_output: AtomicBool,
    dropped_span_count: AtomicI64,
}

impl ConversationTraceSink {
    pub(crate) fn spawn(
        owner_user_id: String,
        conversation_id: String,
        trace_id: String,
        writer: Arc<ConversationTraceWriter>,
        started_at: i64,
        context: ConversationTraceStartContext,
    ) -> Self {
        writer.start(
            owner_user_id,
            conversation_id.clone(),
            trace_id.clone(),
            started_at,
            context,
        );
        Self {
            conversation_id,
            trace_id,
            writer,
            saw_event: AtomicBool::new(false),
            saw_output: AtomicBool::new(false),
            dropped_span_count: AtomicI64::new(0),
        }
    }

    pub(crate) fn observe_event(&self) {
        if self
            .saw_event
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            self.try_send(
                TraceCommand::Observe {
                    conversation_id: self.conversation_id.clone(),
                    trace_id: self.trace_id.clone(),
                    observed_at: now_ms(),
                    output_started: false,
                },
                false,
            );
        }
    }

    pub(crate) fn observe_output(&self) {
        self.observe_event();
        if self
            .saw_output
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            self.try_send(
                TraceCommand::Observe {
                    conversation_id: self.conversation_id.clone(),
                    trace_id: self.trace_id.clone(),
                    observed_at: now_ms(),
                    output_started: true,
                },
                false,
            );
        }
    }

    pub(crate) fn record_span(&self, span: RecordedSpan<'_>) {
        self.observe_event();
        let safe_source_id = span.source_id.and_then(safe_identifier);
        let span_key = safe_source_id
            .clone()
            .unwrap_or_else(|| format!("anonymous-{}", now_ms()));
        let span_id = format!("{}:{}:{span_key}", self.trace_id, span.kind.as_str());
        let attributes = if span.safe_attributes.is_object() {
            span.safe_attributes.to_string()
        } else {
            "{}".to_owned()
        };
        self.try_send(
            TraceCommand::Span(ConversationTraceSpanRow {
                span_id,
                trace_id: self.trace_id.clone(),
                conversation_id: self.conversation_id.clone(),
                kind: span.kind.as_str().to_owned(),
                source_id: safe_source_id,
                source_message_id: span.source_message_id.and_then(safe_identifier),
                name: safe_span_name(span.name, span.kind.as_str()),
                status: span.status.as_str().to_owned(),
                started_at: span.started_at,
                ended_at: span.ended_at,
                duration_ms: span
                    .ended_at
                    .map(|ended_at| ended_at.saturating_sub(span.started_at).max(0)),
                safe_attributes: attributes,
                updated_at: span.ended_at.unwrap_or_else(now_ms),
            }),
            true,
        );
    }

    pub(crate) fn complete(
        self,
        status: &'static str,
        output_size: usize,
        error_code: Option<String>,
        retryable: Option<bool>,
        incomplete: bool,
    ) -> bool {
        let command = TraceCommand::Complete {
            conversation_id: self.conversation_id.clone(),
            trace_id: self.trace_id.clone(),
            status,
            ended_at: now_ms(),
            output_size: i64::try_from(output_size).unwrap_or(i64::MAX),
            error_code,
            retryable,
            incomplete,
            dropped_span_count: self.dropped_span_count.load(Ordering::Relaxed),
        };
        self.writer.submit_lifecycle(command)
    }

    pub(crate) fn flush(self, output_size: usize) -> bool {
        let command = TraceCommand::Flush {
            conversation_id: self.conversation_id.clone(),
            trace_id: self.trace_id.clone(),
            observed_at: now_ms(),
            output_size: i64::try_from(output_size).unwrap_or(i64::MAX),
            dropped_span_count: self.dropped_span_count.load(Ordering::Relaxed),
        };
        self.writer.submit_lifecycle(command)
    }

    fn try_send(&self, command: TraceCommand, counts_as_dropped_span: bool) {
        if !self.writer.submit_data(command) && counts_as_dropped_span {
            self.dropped_span_count.fetch_add(1, Ordering::Relaxed);
        }
    }
}

async fn run_trace_writer(
    repository: Arc<dyn IConversationTraceRepository>,
    broadcaster: Arc<dyn EventBroadcaster>,
    mut rx: mpsc::Receiver<QueuedTraceCommand>,
    queued_data: Arc<AtomicUsize>,
    retention_interval: Duration,
) {
    let mut active = HashMap::<TraceKey, ActiveTrace>::new();
    let mut retention = tokio::time::interval_at(tokio::time::Instant::now() + retention_interval, retention_interval);

    loop {
        let queued = tokio::select! {
            queued = rx.recv() => {
                let Some(queued) = queued else {
                    break;
                };
                queued
            }
            _ = retention.tick() => {
                let _ = trace_io(
                    repository.prune_expired_traces(now_ms()),
                    "retention_prune",
                    None,
                    None,
                ).await;
                continue;
            }
        };
        if queued.data_slot {
            queued_data.fetch_sub(1, Ordering::AcqRel);
        }
        let command = queued.command;

        match command {
            TraceCommand::Start { owner_user_id, trace } => {
                let key = TraceKey::new(trace.conversation_id.clone(), trace.trace_id.clone());
                let existing = active.get(&key).cloned();
                let trace_id = trace.trace_id.clone();
                let conversation_id = trace.conversation_id.clone();
                let Some(stored) = trace_io(
                    repository.start_trace(&trace),
                    "start",
                    Some(&conversation_id),
                    Some(&trace_id),
                )
                .await
                else {
                    continue;
                };
                if stored.status != "running" {
                    continue;
                }
                let existing_runtime_asset_snapshot =
                    existing.as_ref().and_then(|meta| meta.runtime_asset_snapshot.clone());
                let target_user_id = existing
                    .as_ref()
                    .map(|meta| meta.owner_user_id.as_str())
                    .unwrap_or(owner_user_id.as_str());
                if existing.is_none() {
                    active.insert(
                        key,
                        ActiveTrace {
                            owner_user_id: owner_user_id.clone(),
                            runtime_asset_snapshot: None,
                        },
                    );
                }
                broadcast_trace_update(
                    broadcaster.as_ref(),
                    target_user_id,
                    if existing.is_some() {
                        ConversationTraceUpdateKind::TraceUpdated
                    } else {
                        ConversationTraceUpdateKind::TraceStarted
                    },
                    stored,
                    None,
                    existing_runtime_asset_snapshot,
                );
            }
            TraceCommand::Observe {
                conversation_id,
                trace_id,
                observed_at,
                output_started,
            } => {
                let key = TraceKey::new(conversation_id.clone(), trace_id.clone());
                let Some(meta) = active.get(&key).cloned() else {
                    continue;
                };
                if let Some(Some(trace)) = trace_io(
                    repository.observe_trace(
                        &conversation_id,
                        &trace_id,
                        ConversationTraceObservation {
                            observed_at,
                            output_started,
                            output_size_delta: 0,
                        },
                    ),
                    "observe",
                    Some(&conversation_id),
                    Some(&trace_id),
                )
                .await
                {
                    broadcast_trace_update(
                        broadcaster.as_ref(),
                        &meta.owner_user_id,
                        ConversationTraceUpdateKind::TraceUpdated,
                        trace,
                        None,
                        meta.runtime_asset_snapshot,
                    );
                }
            }
            TraceCommand::RuntimeAssetsLoaded { snapshot, persisted } => {
                let trace_id = snapshot.trace_id.clone();
                let conversation_id = snapshot.conversation_id.clone();
                let key = TraceKey::new(conversation_id.clone(), trace_id.clone());
                let Some(meta) = active.get(&key).cloned() else {
                    let _ = persisted.send(false);
                    continue;
                };
                let Some(stored_snapshot) = trace_io(
                    repository.save_runtime_asset_snapshot(&snapshot),
                    "runtime_assets_loaded",
                    Some(&conversation_id),
                    Some(&trace_id),
                )
                .await
                else {
                    let _ = persisted.send(false);
                    continue;
                };
                if let Some(active_trace) = active.get_mut(&key) {
                    active_trace.runtime_asset_snapshot = Some(stored_snapshot.clone());
                }
                let _ = persisted.send(true);
                if let Some(Some(trace)) = trace_io(
                    repository.get_trace(&conversation_id, &trace_id),
                    "runtime_assets_snapshot",
                    Some(&conversation_id),
                    Some(&trace_id),
                )
                .await
                {
                    broadcast_trace_update(
                        broadcaster.as_ref(),
                        &meta.owner_user_id,
                        ConversationTraceUpdateKind::RuntimeAssetsLoaded,
                        trace,
                        None,
                        Some(stored_snapshot),
                    );
                }
            }
            TraceCommand::Span(span) => {
                let trace_id = span.trace_id.clone();
                let conversation_id = span.conversation_id.clone();
                let key = TraceKey::new(conversation_id.clone(), trace_id.clone());
                let Some(meta) = active.get(&key).cloned() else {
                    continue;
                };
                match trace_io(
                    repository.upsert_span(&span),
                    "span",
                    Some(&conversation_id),
                    Some(&trace_id),
                )
                .await
                {
                    Some(ConversationTraceSpanWriteResult::Stored(span)) => {
                        if let Some(Some(trace)) = trace_io(
                            repository.get_trace(&conversation_id, &trace_id),
                            "span_snapshot",
                            Some(&conversation_id),
                            Some(&trace_id),
                        )
                        .await
                        {
                            broadcast_trace_update(
                                broadcaster.as_ref(),
                                &meta.owner_user_id,
                                ConversationTraceUpdateKind::SpanUpdated,
                                trace,
                                Some(*span),
                                meta.runtime_asset_snapshot,
                            );
                        }
                    }
                    Some(
                        ConversationTraceSpanWriteResult::DroppedLimit
                        | ConversationTraceSpanWriteResult::IgnoredTerminalTrace,
                    )
                    | None => {}
                }
            }
            TraceCommand::Flush {
                conversation_id,
                trace_id,
                observed_at,
                output_size,
                dropped_span_count,
            } => {
                let key = TraceKey::new(conversation_id.clone(), trace_id.clone());
                let Some(_meta) = active.get(&key).cloned() else {
                    continue;
                };
                if output_size > 0 {
                    let _ = trace_io(
                        repository.observe_trace(
                            &conversation_id,
                            &trace_id,
                            ConversationTraceObservation {
                                observed_at,
                                output_started: true,
                                output_size_delta: output_size,
                            },
                        ),
                        "flush_output",
                        Some(&conversation_id),
                        Some(&trace_id),
                    )
                    .await;
                }
                let _ = trace_io(
                    repository.record_dropped_spans(&conversation_id, &trace_id, dropped_span_count, observed_at),
                    "flush_dropped_spans",
                    Some(&conversation_id),
                    Some(&trace_id),
                )
                .await;
            }
            TraceCommand::Complete {
                conversation_id,
                trace_id,
                status,
                ended_at,
                output_size,
                error_code,
                retryable,
                incomplete,
                dropped_span_count,
            } => {
                let key = TraceKey::new(conversation_id.clone(), trace_id.clone());
                let Some(meta) = active.remove(&key) else {
                    continue;
                };
                if output_size > 0 {
                    let _ = trace_io(
                        repository.observe_trace(
                            &conversation_id,
                            &trace_id,
                            ConversationTraceObservation {
                                observed_at: ended_at,
                                output_started: true,
                                output_size_delta: output_size,
                            },
                        ),
                        "complete_output",
                        Some(&conversation_id),
                        Some(&trace_id),
                    )
                    .await;
                }
                if let Some(Some(trace)) = trace_io(
                    repository.complete_trace(
                        &conversation_id,
                        &trace_id,
                        CompleteConversationTraceParams {
                            status,
                            ended_at,
                            error_code: error_code.as_deref(),
                            retryable,
                            incomplete,
                            dropped_span_count,
                        },
                    ),
                    "complete",
                    Some(&conversation_id),
                    Some(&trace_id),
                )
                .await
                {
                    broadcast_trace_update(
                        broadcaster.as_ref(),
                        &meta.owner_user_id,
                        ConversationTraceUpdateKind::TraceCompleted,
                        trace,
                        None,
                        meta.runtime_asset_snapshot,
                    );
                }
            }
        }
    }
}

async fn trace_io<T>(
    future: impl Future<Output = Result<T, DbError>>,
    operation: &'static str,
    conversation_id: Option<&str>,
    trace_id: Option<&str>,
) -> Option<T> {
    match tokio::time::timeout(TRACE_IO_TIMEOUT, future).await {
        Ok(Ok(value)) => Some(value),
        Ok(Err(error)) => {
            warn!(
                operation,
                conversation_id,
                trace_id,
                error = %error,
                "对话 Trace 后台写入失败"
            );
            None
        }
        Err(_) => {
            warn!(
                operation,
                conversation_id,
                trace_id,
                timeout_ms = TRACE_IO_TIMEOUT.as_millis(),
                "对话 Trace 后台写入超时"
            );
            None
        }
    }
}

pub(crate) fn broadcast_trace_update(
    broadcaster: &dyn EventBroadcaster,
    owner_user_id: &str,
    update_kind: ConversationTraceUpdateKind,
    trace: ConversationTraceRow,
    span: Option<ConversationTraceSpanRow>,
    runtime_asset_snapshot: Option<ConversationTraceRuntimeAssetSnapshotRow>,
) {
    let runtime_snapshot_id = runtime_asset_snapshot
        .as_ref()
        .map(|snapshot| snapshot.runtime_snapshot_id.as_str());
    let trace = trace_row_to_api_with_runtime_snapshot(trace, runtime_snapshot_id);
    let payload = ConversationTraceUpdatedPayload {
        conversation_id: trace.conversation_id.clone(),
        trace_id: trace.trace_id.clone(),
        turn_id: trace.trace_id.clone(),
        update_kind,
        trace,
        span: span.map(trace_span_row_to_api),
        runtime_asset_snapshot: runtime_asset_snapshot.map(trace_runtime_asset_snapshot_row_to_api),
    };
    broadcaster.broadcast_to_user(
        owner_user_id,
        WebSocketMessage::new(
            "conversation.traceUpdated",
            serde_json::to_value(payload).unwrap_or_else(|_| json!({})),
        ),
    );
}

pub(crate) fn trace_row_to_api_with_runtime_snapshot(
    row: ConversationTraceRow,
    runtime_snapshot_id: Option<&str>,
) -> ConversationTraceSummary {
    ConversationTraceSummary {
        trace_id: row.trace_id,
        conversation_id: row.conversation_id,
        status: match row.status.as_str() {
            "running" => ConversationTraceStatus::Running,
            "succeeded" => ConversationTraceStatus::Succeeded,
            "cancelled" => ConversationTraceStatus::Cancelled,
            "interrupted" => ConversationTraceStatus::Interrupted,
            _ => ConversationTraceStatus::Failed,
        },
        backend: row.backend,
        model: row.model,
        mode: row.mode,
        started_at: row.started_at,
        first_event_at: row.first_event_at,
        first_output_at: row.first_output_at,
        ended_at: row.ended_at,
        duration_ms: row.duration_ms,
        input_size: row.input_size,
        output_size: row.output_size,
        input_tokens: row.input_tokens,
        output_tokens: row.output_tokens,
        total_tokens: row.total_tokens,
        cost_usd: row.cost_usd,
        error_code: row.error_code,
        retryable: row.retryable,
        incomplete: row.incomplete,
        truncated: row.truncated,
        span_count: row.span_count,
        dropped_span_count: row.dropped_span_count,
        runtime_snapshot_id: runtime_snapshot_id.map(str::to_owned),
        updated_at: row.updated_at,
    }
}

pub(crate) fn trace_runtime_asset_snapshot_row_to_api(
    row: ConversationTraceRuntimeAssetSnapshotRow,
) -> ConversationTraceRuntimeAssetSnapshot {
    ConversationTraceRuntimeAssetSnapshot {
        runtime_snapshot_id: row.runtime_snapshot_id,
        assets: row
            .assets
            .into_iter()
            .map(|asset| ConversationTraceRuntimeAssetRef {
                local_asset_id: asset.local_asset_id,
                kind: asset.kind,
                local_definition_digest: asset.local_definition_digest,
                upstream_package: asset.upstream_package,
                upstream_asset_id: asset.upstream_asset_id,
                upstream_version: asset.upstream_version,
                upstream_revision: asset.upstream_revision,
            })
            .collect(),
    }
}

pub(crate) fn trace_span_row_to_api(row: ConversationTraceSpanRow) -> ConversationTraceSpan {
    let safe_attributes = serde_json::from_str(&row.safe_attributes)
        .ok()
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| json!({}));
    ConversationTraceSpan {
        span_id: row.span_id,
        trace_id: row.trace_id,
        kind: match row.kind.as_str() {
            "thinking" => ConversationTraceSpanKind::Thinking,
            "permission" => ConversationTraceSpanKind::Permission,
            "runtime" => ConversationTraceSpanKind::Runtime,
            _ => ConversationTraceSpanKind::Tool,
        },
        source_id: row.source_id,
        source_message_id: row.source_message_id,
        name: row.name,
        status: match row.status.as_str() {
            "running" => ConversationTraceSpanStatus::Running,
            "succeeded" => ConversationTraceSpanStatus::Succeeded,
            "cancelled" => ConversationTraceSpanStatus::Cancelled,
            "interrupted" => ConversationTraceSpanStatus::Interrupted,
            _ => ConversationTraceSpanStatus::Failed,
        },
        started_at: row.started_at,
        ended_at: row.ended_at,
        duration_ms: row.duration_ms,
        safe_attributes,
        updated_at: row.updated_at,
    }
}

fn safe_identifier(value: &str) -> Option<String> {
    let value = value.trim();
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

fn safe_span_name(value: &str, fallback: &str) -> String {
    safe_identifier(value).unwrap_or_else(|| fallback.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tjuaeui_db::{
        CONVERSATION_TRACE_MAX_SPANS, ConversationTraceRuntimeAssetRefRow, IConversationRepository, IUserRepository,
        SqliteConversationRepository, SqliteConversationTraceRepository, SqliteUserRepository, init_database_memory,
        models::ConversationRow,
    };

    fn trace_conversation(id: &str, user_id: &str) -> ConversationRow {
        ConversationRow {
            id: id.to_owned(),
            user_id: user_id.to_owned(),
            name: "Trace".to_owned(),
            r#type: "tjuae_cli".to_owned(),
            extra: "{}".to_owned(),
            model: None,
            status: Some("running".to_owned()),
            source: Some("tjuaeui".to_owned()),
            channel_chat_id: None,
            pinned: false,
            pinned_at: None,
            created_at: 1,
            updated_at: 1,
            project_id: None,
            folder_id: None,
        }
    }

    #[derive(Clone)]
    struct SlowSpanTraceRepository {
        inner: SqliteConversationTraceRepository,
        span_delay: Duration,
    }

    #[async_trait::async_trait]
    impl IConversationTraceRepository for SlowSpanTraceRepository {
        async fn start_trace(&self, trace: &ConversationTraceRow) -> Result<ConversationTraceRow, DbError> {
            self.inner.start_trace(trace).await
        }

        async fn observe_trace(
            &self,
            conversation_id: &str,
            trace_id: &str,
            observation: ConversationTraceObservation,
        ) -> Result<Option<ConversationTraceRow>, DbError> {
            self.inner.observe_trace(conversation_id, trace_id, observation).await
        }

        async fn complete_trace(
            &self,
            conversation_id: &str,
            trace_id: &str,
            params: CompleteConversationTraceParams<'_>,
        ) -> Result<Option<ConversationTraceRow>, DbError> {
            self.inner.complete_trace(conversation_id, trace_id, params).await
        }

        async fn upsert_span(
            &self,
            span: &ConversationTraceSpanRow,
        ) -> Result<ConversationTraceSpanWriteResult, DbError> {
            tokio::time::sleep(self.span_delay).await;
            self.inner.upsert_span(span).await
        }

        async fn record_dropped_spans(
            &self,
            conversation_id: &str,
            trace_id: &str,
            count: i64,
            observed_at: i64,
        ) -> Result<(), DbError> {
            self.inner
                .record_dropped_spans(conversation_id, trace_id, count, observed_at)
                .await
        }

        async fn get_trace(
            &self,
            conversation_id: &str,
            trace_id: &str,
        ) -> Result<Option<ConversationTraceRow>, DbError> {
            self.inner.get_trace(conversation_id, trace_id).await
        }

        async fn list_traces(&self, conversation_id: &str, limit: u32) -> Result<Vec<ConversationTraceRow>, DbError> {
            self.inner.list_traces(conversation_id, limit).await
        }

        async fn list_spans(
            &self,
            conversation_id: &str,
            trace_id: &str,
        ) -> Result<Vec<ConversationTraceSpanRow>, DbError> {
            self.inner.list_spans(conversation_id, trace_id).await
        }

        async fn save_runtime_asset_snapshot(
            &self,
            snapshot: &ConversationTraceRuntimeAssetSnapshotRow,
        ) -> Result<ConversationTraceRuntimeAssetSnapshotRow, DbError> {
            self.inner.save_runtime_asset_snapshot(snapshot).await
        }

        async fn get_runtime_asset_snapshot(
            &self,
            user_id: &str,
            conversation_id: &str,
            trace_id: &str,
        ) -> Result<Option<ConversationTraceRuntimeAssetSnapshotRow>, DbError> {
            self.inner
                .get_runtime_asset_snapshot(user_id, conversation_id, trace_id)
                .await
        }

        async fn list_runtime_asset_snapshot_summaries(
            &self,
            user_id: &str,
            conversation_id: &str,
        ) -> Result<Vec<tjuaeui_db::ConversationTraceRuntimeAssetSnapshotSummaryRow>, DbError> {
            self.inner
                .list_runtime_asset_snapshot_summaries(user_id, conversation_id)
                .await
        }

        async fn interrupt_running_traces(&self, interrupted_at: i64) -> Result<u64, DbError> {
            self.inner.interrupt_running_traces(interrupted_at).await
        }

        async fn prune_traces(&self, conversation_id: &str, now: i64) -> Result<u64, DbError> {
            self.inner.prune_traces(conversation_id, now).await
        }

        async fn prune_expired_traces(&self, now: i64) -> Result<u64, DbError> {
            self.inner.prune_expired_traces(now).await
        }
    }

    #[test]
    fn sink_surface_has_no_content_bearing_argument() {
        assert_eq!(safe_span_name("read_file", "tool"), "read_file");
        assert_eq!(safe_span_name("type C:\\secret.txt", "tool"), "tool");
        assert_eq!(safe_identifier("/home/user/private.txt"), None);
    }

    #[test]
    fn runtime_span_maps_to_typed_api_without_content_fields() {
        let span = trace_span_row_to_api(ConversationTraceSpanRow {
            span_id: "turn-1:runtime:connect:mcp.docs".into(),
            trace_id: "turn-1".into(),
            conversation_id: "conv-1".into(),
            kind: "runtime".into(),
            source_id: Some("connect:mcp.docs".into()),
            source_message_id: None,
            name: "connect".into(),
            status: "succeeded".into(),
            started_at: 10,
            ended_at: Some(12),
            duration_ms: Some(2),
            safe_attributes: json!({
                "phase": "connect",
                "assetKind": "mcp",
                "localAssetId": "mcp.docs",
                "errorCode": null,
            })
            .to_string(),
            updated_at: 12,
        });

        assert_eq!(span.kind, ConversationTraceSpanKind::Runtime);
        assert_eq!(span.safe_attributes["phase"], "connect");
        assert!(span.safe_attributes.get("path").is_none());
        assert!(span.safe_attributes.get("command").is_none());
        assert!(span.safe_attributes.get("env").is_none());
    }

    #[test]
    fn websocket_contract_is_snake_case_with_stable_null_span() {
        let trace = trace_row_to_api_with_runtime_snapshot(
            ConversationTraceRow {
                trace_id: "turn-1".to_owned(),
                conversation_id: "conv-1".to_owned(),
                status: "running".to_owned(),
                backend: None,
                model: None,
                mode: None,
                started_at: 1,
                first_event_at: None,
                first_output_at: None,
                ended_at: None,
                duration_ms: None,
                input_size: 0,
                output_size: 0,
                input_tokens: None,
                output_tokens: None,
                total_tokens: None,
                cost_usd: None,
                error_code: None,
                retryable: None,
                incomplete: false,
                truncated: false,
                span_count: 0,
                dropped_span_count: 0,
                updated_at: 1,
            },
            None,
        );
        let payload = ConversationTraceUpdatedPayload {
            conversation_id: "conv-1".to_owned(),
            trace_id: "turn-1".to_owned(),
            turn_id: "turn-1".to_owned(),
            update_kind: ConversationTraceUpdateKind::TraceStarted,
            trace,
            span: None,
            runtime_asset_snapshot: None,
        };
        let value = serde_json::to_value(payload).unwrap();
        assert_eq!(value["update_kind"], "trace_started");
        assert!(value["span"].is_null());
        assert!(value.get("updateKind").is_none());
    }

    #[tokio::test]
    async fn writer_emits_typed_sanitized_websocket_updates() {
        let database = init_database_memory().await.unwrap();
        let conversation_repo = SqliteConversationRepository::new(database.pool().clone());
        conversation_repo
            .create(&tjuaeui_db::models::ConversationRow {
                id: "conv-live-trace".to_owned(),
                user_id: "system_default_user".to_owned(),
                name: "Trace".to_owned(),
                r#type: "tjuae_cli".to_owned(),
                extra: "{}".to_owned(),
                model: None,
                status: Some("running".to_owned()),
                source: Some("tjuaeui".to_owned()),
                channel_chat_id: None,
                pinned: false,
                pinned_at: None,
                created_at: 1,
                updated_at: 1,
                project_id: None,
                folder_id: None,
            })
            .await
            .unwrap();
        let trace_repo = Arc::new(tjuaeui_db::SqliteConversationTraceRepository::new(
            database.pool().clone(),
        ));
        let bus = Arc::new(tjuaeui_realtime::BroadcastEventBus::new(16));
        let mut events = bus.subscribe_user();
        let writer = ConversationTraceWriter::spawn(trace_repo, bus);
        let sink = ConversationTraceSink::spawn(
            "system_default_user".to_owned(),
            "conv-live-trace".to_owned(),
            "turn-live-trace".to_owned(),
            writer.clone(),
            10,
            ConversationTraceStartContext::default(),
        );
        let runtime_assets = vec![ConversationTraceRuntimeAssetRefRow {
            local_asset_id: "assistant-review".to_owned(),
            kind: "assistant".to_owned(),
            local_definition_digest: format!("sha256-{}", "b".repeat(64)),
            runtime_content_digest: format!("sha256-{}", "c".repeat(64)),
            upstream_package: Some("official-assistants".to_owned()),
            upstream_asset_id: Some("review".to_owned()),
            upstream_version: Some("1.2.3".to_owned()),
            upstream_revision: Some("abc123".to_owned()),
        }];
        let runtime_snapshot_id = tjuaeui_common::compute_runtime_asset_snapshot_id(
            runtime_assets
                .iter()
                .map(|asset| tjuaeui_common::RuntimeAssetDigestInput {
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
        assert!(
            writer
                .runtime_assets_loaded(ConversationTraceRuntimeAssetSnapshotRow {
                    user_id: "system_default_user".to_owned(),
                    conversation_id: "conv-live-trace".to_owned(),
                    trace_id: "turn-live-trace".to_owned(),
                    runtime_snapshot_id: runtime_snapshot_id.clone(),
                    assets: runtime_assets,
                    created_at: 10,
                })
                .await
        );
        sink.record_span(RecordedSpan {
            kind: RecordedSpanKind::Tool,
            source_id: Some("call-1"),
            source_message_id: Some("call-1"),
            name: "read_file",
            status: RecordedSpanStatus::Succeeded,
            started_at: 11,
            ended_at: Some(12),
            safe_attributes: json!({"tool_kind":"read","command":"secret"}),
        });
        sink.complete("succeeded", 0, None, None, false);

        let mut received = Vec::new();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let targeted = events.recv().await.unwrap();
                assert_eq!(targeted.user_id, "system_default_user");
                let completed = targeted.event.data["update_kind"] == "trace_completed";
                received.push(targeted.event);
                if completed {
                    break;
                }
            }
        })
        .await
        .unwrap();

        assert!(received.iter().all(|event| event.name == "conversation.traceUpdated"));
        let span_update = received
            .iter()
            .find(|event| event.data["update_kind"] == "span_updated")
            .unwrap();
        assert_eq!(span_update.data["trace_id"], "turn-live-trace");
        assert_eq!(span_update.data["span"]["safe_attributes"]["tool_kind"], "read");
        assert!(span_update.data["span"]["safe_attributes"].get("command").is_none());
        assert!(span_update.data.get("updateKind").is_none());
        let asset_update = received
            .iter()
            .find(|event| event.data["update_kind"] == "runtime_assets_loaded")
            .unwrap();
        assert_eq!(asset_update.data["trace"]["runtime_snapshot_id"], runtime_snapshot_id);
        let snapshot = asset_update.data["runtime_asset_snapshot"].as_object().unwrap();
        assert_eq!(snapshot.len(), 2);
        assert!(
            ["assets", "runtimeSnapshotId"]
                .into_iter()
                .all(|field| snapshot.contains_key(field))
        );
        let asset = snapshot["assets"][0].as_object().unwrap();
        assert_eq!(asset.len(), 7);
        assert!(
            [
                "kind",
                "localAssetId",
                "localDefinitionDigest",
                "upstreamAssetId",
                "upstreamPackage",
                "upstreamRevision",
                "upstreamVersion",
            ]
            .into_iter()
            .all(|field| asset.contains_key(field))
        );
    }

    #[tokio::test]
    async fn flush_precedes_completion_and_late_updates_cannot_change_snapshot() {
        let database = init_database_memory().await.unwrap();
        let conversation_repo = SqliteConversationRepository::new(database.pool().clone());
        conversation_repo
            .create(&tjuaeui_db::models::ConversationRow {
                id: "conv-ordered-trace".to_owned(),
                user_id: "system_default_user".to_owned(),
                name: "Trace order".to_owned(),
                r#type: "tjuae_cli".to_owned(),
                extra: "{}".to_owned(),
                model: None,
                status: Some("running".to_owned()),
                source: Some("tjuaeui".to_owned()),
                channel_chat_id: None,
                pinned: false,
                pinned_at: None,
                created_at: 1,
                updated_at: 1,
                project_id: None,
                folder_id: None,
            })
            .await
            .unwrap();
        let trace_repo = Arc::new(tjuaeui_db::SqliteConversationTraceRepository::new(
            database.pool().clone(),
        ));
        let bus = Arc::new(tjuaeui_realtime::BroadcastEventBus::new(32));
        let mut events = bus.subscribe_user();
        let writer = ConversationTraceWriter::spawn(trace_repo.clone(), bus);
        let sink = ConversationTraceSink::spawn(
            "system_default_user".to_owned(),
            "conv-ordered-trace".to_owned(),
            "turn-ordered-trace".to_owned(),
            writer.clone(),
            10,
            ConversationTraceStartContext::default(),
        );
        sink.record_span(RecordedSpan {
            kind: RecordedSpanKind::Tool,
            source_id: Some("call-before"),
            source_message_id: None,
            name: "read",
            status: RecordedSpanStatus::Succeeded,
            started_at: 11,
            ended_at: Some(12),
            safe_attributes: json!({"tool_kind":"read"}),
        });
        sink.flush(7);
        writer.complete(
            "conv-ordered-trace".to_owned(),
            "turn-ordered-trace".to_owned(),
            ConversationTraceCompletion {
                status: "succeeded",
                ended_at: 20,
                output_size: 0,
                error_code: None,
                retryable: None,
                incomplete: false,
                dropped_span_count: 0,
            },
        );

        let late = ConversationTraceSink::spawn(
            "system_default_user".to_owned(),
            "conv-ordered-trace".to_owned(),
            "turn-ordered-trace".to_owned(),
            writer,
            21,
            ConversationTraceStartContext::default(),
        );
        late.record_span(RecordedSpan {
            kind: RecordedSpanKind::Tool,
            source_id: Some("call-after"),
            source_message_id: None,
            name: "edit",
            status: RecordedSpanStatus::Failed,
            started_at: 21,
            ended_at: Some(22),
            safe_attributes: json!({"tool_kind":"edit"}),
        });
        late.flush(100);

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let targeted = events.recv().await.unwrap();
                assert_eq!(targeted.user_id, "system_default_user");
                if targeted.event.data["update_kind"] == "trace_completed" {
                    break;
                }
            }
        })
        .await
        .unwrap();
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }

        let stored = trace_repo
            .get_trace("conv-ordered-trace", "turn-ordered-trace")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.status, "succeeded");
        assert_eq!(stored.output_size, 7);
        assert_eq!(stored.span_count, 1);
        assert_eq!(
            trace_repo
                .list_spans("conv-ordered-trace", "turn-ordered-trace")
                .await
                .unwrap()[0]
                .source_id
                .as_deref(),
            Some("call-before")
        );
    }

    #[tokio::test]
    async fn writer_periodically_prunes_expired_traces_after_startup() {
        let database = init_database_memory().await.unwrap();
        let conversation_repo = SqliteConversationRepository::new(database.pool().clone());
        conversation_repo
            .create(&tjuaeui_db::models::ConversationRow {
                id: "conv-periodic-retention".to_owned(),
                user_id: "system_default_user".to_owned(),
                name: "Trace retention".to_owned(),
                r#type: "tjuae_cli".to_owned(),
                extra: "{}".to_owned(),
                model: None,
                status: Some("running".to_owned()),
                source: Some("tjuaeui".to_owned()),
                channel_chat_id: None,
                pinned: false,
                pinned_at: None,
                created_at: 1,
                updated_at: 1,
                project_id: None,
                folder_id: None,
            })
            .await
            .unwrap();
        let trace_repo = Arc::new(tjuaeui_db::SqliteConversationTraceRepository::new(
            database.pool().clone(),
        ));
        let now = now_ms();
        trace_repo
            .start_trace(&ConversationTraceRow {
                trace_id: "turn-expired-periodic".to_owned(),
                conversation_id: "conv-periodic-retention".to_owned(),
                status: "running".to_owned(),
                backend: None,
                model: None,
                mode: None,
                started_at: now - 31 * 24 * 60 * 60 * 1_000,
                first_event_at: None,
                first_output_at: None,
                ended_at: None,
                duration_ms: None,
                input_size: 0,
                output_size: 0,
                input_tokens: None,
                output_tokens: None,
                total_tokens: None,
                cost_usd: None,
                error_code: None,
                retryable: None,
                incomplete: false,
                truncated: false,
                span_count: 0,
                dropped_span_count: 0,
                updated_at: now,
            })
            .await
            .unwrap();
        let bus = Arc::new(tjuaeui_realtime::BroadcastEventBus::new(8));
        let _writer =
            ConversationTraceWriter::spawn_with_retention_interval(trace_repo.clone(), bus, Duration::from_millis(10));

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if trace_repo
                    .get_trace("conv-periodic-retention", "turn-expired-periodic")
                    .await
                    .unwrap()
                    .is_none()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn repeated_running_start_enriches_the_initial_default_context() {
        let database = init_database_memory().await.unwrap();
        let conversations = SqliteConversationRepository::new(database.pool().clone());
        conversations
            .create(&trace_conversation("conv-enriched-trace", "system_default_user"))
            .await
            .unwrap();
        let traces = Arc::new(SqliteConversationTraceRepository::new(database.pool().clone()));
        let bus = Arc::new(tjuaeui_realtime::BroadcastEventBus::new(16));
        let mut events = bus.subscribe_user();
        let writer = ConversationTraceWriter::spawn(traces.clone(), bus);

        writer.start(
            "system_default_user".to_owned(),
            "conv-enriched-trace".to_owned(),
            "turn-enriched-trace".to_owned(),
            10,
            ConversationTraceStartContext::default(),
        );
        writer.start(
            "system_default_user".to_owned(),
            "conv-enriched-trace".to_owned(),
            "turn-enriched-trace".to_owned(),
            11,
            ConversationTraceStartContext {
                backend: Some("codex".to_owned()),
                model: Some("gpt-5.6".to_owned()),
                mode: Some("agent-full-access".to_owned()),
                input_size: 42,
            },
        );
        writer.start(
            "system_default_user".to_owned(),
            "conv-enriched-trace".to_owned(),
            "turn-enriched-trace".to_owned(),
            12,
            ConversationTraceStartContext {
                backend: Some("other".to_owned()),
                model: Some("other-model".to_owned()),
                mode: Some("other-mode".to_owned()),
                input_size: 7,
            },
        );

        tokio::time::timeout(Duration::from_secs(2), async {
            let mut update_count = 0;
            loop {
                let event = events.recv().await.unwrap();
                if event.event.data["conversation_id"] == "conv-enriched-trace"
                    && event.event.data["update_kind"] == "trace_updated"
                {
                    assert_eq!(event.user_id, "system_default_user");
                    update_count += 1;
                    if update_count == 2 {
                        break;
                    }
                }
            }
        })
        .await
        .unwrap();

        let stored = traces
            .get_trace("conv-enriched-trace", "turn-enriched-trace")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.backend.as_deref(), Some("codex"));
        assert_eq!(stored.model.as_deref(), Some("gpt-5.6"));
        assert_eq!(stored.mode.as_deref(), Some("agent-full-access"));
        assert_eq!(stored.input_size, 42);
    }

    #[tokio::test]
    async fn same_trace_id_stays_isolated_in_two_active_conversations_and_users() {
        let database = init_database_memory().await.unwrap();
        let users = SqliteUserRepository::new(database.pool().clone());
        let user_a = users.create_user("writer-trace-a", "hash-a").await.unwrap();
        let user_b = users.create_user("writer-trace-b", "hash-b").await.unwrap();
        let conversations = SqliteConversationRepository::new(database.pool().clone());
        conversations
            .create(&trace_conversation("conv-writer-a", &user_a.id))
            .await
            .unwrap();
        conversations
            .create(&trace_conversation("conv-writer-b", &user_b.id))
            .await
            .unwrap();
        let traces = Arc::new(SqliteConversationTraceRepository::new(database.pool().clone()));
        let bus = Arc::new(tjuaeui_realtime::BroadcastEventBus::new(128));
        let mut events = bus.subscribe_user();
        let writer = ConversationTraceWriter::spawn(traces.clone(), bus);
        let trace_id = "turn-writer-deterministic-collision";

        let sink_a = ConversationTraceSink::spawn(
            user_a.id.clone(),
            "conv-writer-a".to_owned(),
            trace_id.to_owned(),
            writer.clone(),
            10,
            ConversationTraceStartContext::default(),
        );
        let sink_b = ConversationTraceSink::spawn(
            user_b.id.clone(),
            "conv-writer-b".to_owned(),
            trace_id.to_owned(),
            writer.clone(),
            20,
            ConversationTraceStartContext::default(),
        );
        sink_a.observe_output();
        sink_b.observe_output();
        sink_a.record_span(RecordedSpan {
            kind: RecordedSpanKind::Tool,
            source_id: Some("same-call"),
            source_message_id: None,
            name: "read",
            status: RecordedSpanStatus::Succeeded,
            started_at: 11,
            ended_at: Some(12),
            safe_attributes: json!({"tool_kind":"read"}),
        });
        assert!(sink_a.flush(3));
        assert!(writer.complete(
            "conv-writer-a".to_owned(),
            trace_id.to_owned(),
            ConversationTraceCompletion {
                status: "succeeded",
                ended_at: 30,
                output_size: 0,
                error_code: None,
                retryable: None,
                incomplete: false,
                dropped_span_count: 0,
            },
        ));

        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let event = events.recv().await.unwrap();
                if event.event.data["conversation_id"] == "conv-writer-a"
                    && event.event.data["update_kind"] == "trace_completed"
                {
                    assert_eq!(event.user_id, user_a.id);
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(
            traces
                .get_trace("conv-writer-b", trace_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "running",
            "completing the colliding trace in conversation A must not terminate B"
        );

        sink_b.record_span(RecordedSpan {
            kind: RecordedSpanKind::Tool,
            source_id: Some("same-call"),
            source_message_id: None,
            name: "edit",
            status: RecordedSpanStatus::Failed,
            started_at: 21,
            ended_at: Some(22),
            safe_attributes: json!({"tool_kind":"edit"}),
        });
        assert!(sink_b.flush(7));
        assert!(writer.complete(
            "conv-writer-b".to_owned(),
            trace_id.to_owned(),
            ConversationTraceCompletion {
                status: "failed",
                ended_at: 40,
                output_size: 0,
                error_code: Some("B_FAILED".to_owned()),
                retryable: Some(false),
                incomplete: true,
                dropped_span_count: 0,
            },
        ));
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let event = events.recv().await.unwrap();
                if event.event.data["conversation_id"] == "conv-writer-b"
                    && event.event.data["update_kind"] == "trace_completed"
                {
                    assert_eq!(event.user_id, user_b.id);
                    break;
                }
            }
        })
        .await
        .unwrap();

        let stored_a = traces.get_trace("conv-writer-a", trace_id).await.unwrap().unwrap();
        let stored_b = traces.get_trace("conv-writer-b", trace_id).await.unwrap().unwrap();
        assert_eq!((stored_a.status.as_str(), stored_a.output_size), ("succeeded", 3));
        assert_eq!((stored_b.status.as_str(), stored_b.output_size), ("failed", 7));
        assert_eq!(traces.list_spans("conv-writer-a", trace_id).await.unwrap().len(), 1);
        assert_eq!(traces.list_spans("conv-writer-b", trace_id).await.unwrap().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn slow_span_burst_cannot_displace_completion_from_the_bounded_queue() {
        let database = init_database_memory().await.unwrap();
        let conversations = SqliteConversationRepository::new(database.pool().clone());
        conversations
            .create(&trace_conversation("conv-slow-burst", "system_default_user"))
            .await
            .unwrap();
        let inner = SqliteConversationTraceRepository::new(database.pool().clone());
        let slow = Arc::new(SlowSpanTraceRepository {
            inner: inner.clone(),
            span_delay: Duration::from_millis(2),
        });
        let bus = Arc::new(tjuaeui_realtime::BroadcastEventBus::new(8));
        let writer = ConversationTraceWriter::spawn(slow, bus);
        let sink = ConversationTraceSink::spawn(
            "system_default_user".to_owned(),
            "conv-slow-burst".to_owned(),
            "turn-slow-burst".to_owned(),
            writer.clone(),
            10,
            ConversationTraceStartContext::default(),
        );

        for index in 0..500 {
            let source_id = format!("burst-{index}");
            sink.record_span(RecordedSpan {
                kind: RecordedSpanKind::Tool,
                source_id: Some(&source_id),
                source_message_id: None,
                name: "execute",
                status: RecordedSpanStatus::Running,
                started_at: 11 + i64::from(index),
                ended_at: None,
                safe_attributes: json!({"tool_kind":"execute"}),
            });
            sink.record_span(RecordedSpan {
                kind: RecordedSpanKind::Tool,
                source_id: Some(&source_id),
                source_message_id: None,
                name: "execute",
                status: RecordedSpanStatus::Succeeded,
                started_at: 11 + i64::from(index),
                ended_at: Some(12 + i64::from(index)),
                safe_attributes: json!({"tool_kind":"execute"}),
            });
        }
        assert!(writer.queued_data.load(Ordering::Acquire) <= TRACE_DATA_CAPACITY);
        assert!(
            sink.complete("succeeded", 9, None, None, false),
            "the reserved lifecycle capacity must accept Complete after a data burst"
        );

        let stored = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Some(trace) = inner.get_trace("conv-slow-burst", "turn-slow-burst").await.unwrap()
                    && trace.status != "running"
                {
                    break trace;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("completion must reach the slow repository");
        assert_eq!(stored.status, "succeeded");
        assert_eq!(stored.output_size, 9);
        assert!(stored.truncated);
        assert!(stored.dropped_span_count > 0);
        assert!(stored.span_count <= CONVERSATION_TRACE_MAX_SPANS);
        assert_eq!(writer.queued_data.load(Ordering::Acquire), 0);
    }

    #[test]
    fn saturated_lifecycle_queue_reports_complete_rejection() {
        let (tx, _rx) = mpsc::channel(TRACE_COMMAND_CAPACITY);
        let writer = ConversationTraceWriter {
            tx,
            queued_data: Arc::new(AtomicUsize::new(0)),
        };
        for index in 0..TRACE_COMMAND_CAPACITY {
            assert!(writer.submit_lifecycle(TraceCommand::Flush {
                conversation_id: "conv-full-lifecycle".to_owned(),
                trace_id: format!("turn-{index}"),
                observed_at: 1,
                output_size: 0,
                dropped_span_count: 0,
            }));
        }
        assert!(!writer.complete(
            "conv-full-lifecycle".to_owned(),
            "turn-final".to_owned(),
            ConversationTraceCompletion {
                status: "failed",
                ended_at: 2,
                output_size: 0,
                error_code: Some("QUEUE_FULL".to_owned()),
                retryable: None,
                incomplete: true,
                dropped_span_count: 0,
            }
        ));
    }
}
