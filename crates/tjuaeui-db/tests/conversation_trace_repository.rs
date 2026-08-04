use tjuaeui_common::{RuntimeAssetDigestInput, compute_runtime_asset_snapshot_id};
use tjuaeui_db::{
    CompleteConversationTraceParams, ConversationTraceObservation, ConversationTraceRow,
    ConversationTraceRuntimeAssetRefRow, ConversationTraceRuntimeAssetSnapshotRow, ConversationTraceSpanRow,
    ConversationTraceSpanWriteResult, IConversationRepository, IConversationTraceRepository, IUserRepository,
    SqliteConversationRepository, SqliteConversationTraceRepository, SqliteUserRepository, init_database_memory,
    models::ConversationRow,
};

const USER_ID: &str = "system_default_user";

async fn setup() -> (
    SqliteConversationRepository,
    SqliteConversationTraceRepository,
    tjuaeui_db::Database,
) {
    let db = init_database_memory().await.unwrap();
    (
        SqliteConversationRepository::new(db.pool().clone()),
        SqliteConversationTraceRepository::new(db.pool().clone()),
        db,
    )
}

fn conversation(id: &str, user_id: &str) -> ConversationRow {
    let now = tjuaeui_common::now_ms();
    ConversationRow {
        id: id.to_owned(),
        user_id: user_id.to_owned(),
        name: "Trace test".to_owned(),
        r#type: "tjuae_cli".to_owned(),
        extra: "{}".to_owned(),
        model: None,
        status: Some("running".to_owned()),
        source: Some("tjuaeui".to_owned()),
        channel_chat_id: None,
        pinned: false,
        pinned_at: None,
        created_at: now,
        updated_at: now,
        project_id: None,
        folder_id: None,
    }
}

fn trace(conversation_id: &str, trace_id: &str, started_at: i64) -> ConversationTraceRow {
    ConversationTraceRow {
        trace_id: trace_id.to_owned(),
        conversation_id: conversation_id.to_owned(),
        status: "running".to_owned(),
        backend: Some("codex".to_owned()),
        model: Some("gpt-5.6".to_owned()),
        mode: Some("default".to_owned()),
        started_at,
        first_event_at: None,
        first_output_at: None,
        ended_at: None,
        duration_ms: None,
        input_size: 12,
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
    }
}

fn span(conversation_id: &str, trace_id: &str, index: usize) -> ConversationTraceSpanRow {
    ConversationTraceSpanRow {
        span_id: format!("{trace_id}:tool:{index}"),
        trace_id: trace_id.to_owned(),
        conversation_id: conversation_id.to_owned(),
        kind: "tool".to_owned(),
        source_id: Some(format!("call-{index}")),
        source_message_id: None,
        name: "read".to_owned(),
        status: "running".to_owned(),
        started_at: 2_000 + index as i64,
        ended_at: None,
        duration_ms: None,
        safe_attributes: r#"{"tool_kind":"read"}"#.to_owned(),
        updated_at: 2_000 + index as i64,
    }
}

fn runtime_snapshot(
    user_id: &str,
    conversation_id: &str,
    trace_id: &str,
    _snapshot_hex: char,
    definition_hex: char,
) -> ConversationTraceRuntimeAssetSnapshotRow {
    let assets = vec![ConversationTraceRuntimeAssetRefRow {
        local_asset_id: "frontend-design".into(),
        kind: "skill".into(),
        local_definition_digest: format!("sha256-{}", definition_hex.to_string().repeat(64)),
        runtime_content_digest: format!("sha256-{}", "c".repeat(64)),
        upstream_package: Some("tjuae-official/assets".into()),
        upstream_asset_id: Some("frontend-design".into()),
        upstream_version: Some("1.0.0".into()),
        upstream_revision: Some("abc123".into()),
    }];
    let runtime_snapshot_id = compute_runtime_asset_snapshot_id(
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
    ConversationTraceRuntimeAssetSnapshotRow {
        user_id: user_id.to_owned(),
        conversation_id: conversation_id.to_owned(),
        trace_id: trace_id.to_owned(),
        runtime_snapshot_id,
        assets,
        created_at: 2_000,
    }
}

#[tokio::test]
async fn trace_crud_records_only_sanitized_operational_metadata() {
    let (conversations, traces, _db) = setup().await;
    conversations
        .create(&conversation("conv-trace", USER_ID))
        .await
        .unwrap();

    let mut root = trace("conv-trace", "turn-1", 1_000);
    root.model = Some(r"C:\Users\someone\.secrets".to_owned());
    let started = traces.start_trace(&root).await.unwrap();
    assert_eq!(started.trace_id, "turn-1");
    assert_eq!(started.model, None, "absolute paths must not enter trace metadata");

    let mut tool = span("conv-trace", "turn-1", 1);
    tool.safe_attributes =
        r#"{"tool_kind":"read","command":"type C:\\secret.txt","authorization":"Bearer token"}"#.to_owned();
    let stored = traces.upsert_span(&tool).await.unwrap();
    let ConversationTraceSpanWriteResult::Stored(stored) = stored else {
        panic!("span should be stored");
    };
    assert_eq!(stored.safe_attributes, r#"{"tool_kind":"read"}"#);
    assert!(!stored.safe_attributes.contains("secret"));
    assert!(!stored.safe_attributes.contains("Bearer"));

    let observed = traces
        .observe_trace(
            "conv-trace",
            "turn-1",
            ConversationTraceObservation {
                observed_at: 2_500,
                output_started: true,
                output_size_delta: 9,
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(observed.first_event_at, Some(2_500));
    assert_eq!(observed.first_output_at, Some(2_500));
    assert_eq!(observed.output_size, 9);

    let completed = traces
        .complete_trace(
            "conv-trace",
            "turn-1",
            CompleteConversationTraceParams {
                status: "succeeded",
                ended_at: 3_000,
                error_code: None,
                retryable: None,
                incomplete: false,
                dropped_span_count: 0,
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completed.status, "succeeded");
    assert_eq!(completed.duration_ms, Some(2_000));

    let spans = traces.list_spans("conv-trace", "turn-1").await.unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].status, "succeeded");
    assert_eq!(spans[0].duration_ms, Some(999));
}

#[tokio::test]
async fn reads_are_scoped_by_conversation_and_cascade_on_delete() {
    let (conversations, traces, db) = setup().await;
    conversations
        .create(&conversation("conv-owner", USER_ID))
        .await
        .unwrap();
    conversations
        .create(&conversation("conv-other", USER_ID))
        .await
        .unwrap();
    traces
        .start_trace(&trace("conv-owner", "turn-private", 1_000))
        .await
        .unwrap();
    traces
        .upsert_span(&span("conv-owner", "turn-private", 0))
        .await
        .unwrap();

    assert!(
        traces.get_trace("conv-other", "turn-private").await.unwrap().is_none(),
        "a trace id cannot be read through another conversation"
    );
    assert!(
        traces
            .list_spans("conv-other", "turn-private")
            .await
            .unwrap()
            .is_empty()
    );

    conversations.delete("conv-owner").await.unwrap();
    let trace_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM conversation_traces")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let span_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM conversation_trace_spans")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(trace_count, 0);
    assert_eq!(span_count, 0);
}

#[tokio::test]
async fn identical_trace_and_span_ids_are_isolated_across_conversations_and_users() {
    let (conversations, traces, db) = setup().await;
    let users = SqliteUserRepository::new(db.pool().clone());
    let user_a = users.create_user("trace-user-a", "hash-a").await.unwrap();
    let user_b = users.create_user("trace-user-b", "hash-b").await.unwrap();
    conversations
        .create(&conversation("conv-same-a", &user_a.id))
        .await
        .unwrap();
    conversations
        .create(&conversation("conv-same-b", &user_b.id))
        .await
        .unwrap();

    let trace_id = "turn-deterministic-collision";
    traces
        .start_trace(&trace("conv-same-a", trace_id, 1_000))
        .await
        .unwrap();
    traces
        .start_trace(&trace("conv-same-b", trace_id, 2_000))
        .await
        .unwrap();

    let (observed_a, observed_b) = tokio::join!(
        traces.observe_trace(
            "conv-same-a",
            trace_id,
            ConversationTraceObservation {
                observed_at: 1_100,
                output_started: true,
                output_size_delta: 3,
            }
        ),
        traces.observe_trace(
            "conv-same-b",
            trace_id,
            ConversationTraceObservation {
                observed_at: 2_100,
                output_started: true,
                output_size_delta: 7,
            }
        )
    );
    assert_eq!(observed_a.unwrap().unwrap().output_size, 3);
    assert_eq!(observed_b.unwrap().unwrap().output_size, 7);

    let span_a = span("conv-same-a", trace_id, 0);
    let span_b = span("conv-same-b", trace_id, 0);
    let (stored_a, stored_b) = tokio::join!(traces.upsert_span(&span_a), traces.upsert_span(&span_b));
    assert!(matches!(stored_a.unwrap(), ConversationTraceSpanWriteResult::Stored(_)));
    assert!(matches!(stored_b.unwrap(), ConversationTraceSpanWriteResult::Stored(_)));

    let (completed_a, observed_b_after_a_completed) = tokio::join!(
        traces.complete_trace(
            "conv-same-a",
            trace_id,
            CompleteConversationTraceParams {
                status: "succeeded",
                ended_at: 1_500,
                error_code: None,
                retryable: None,
                incomplete: false,
                dropped_span_count: 0,
            }
        ),
        traces.observe_trace(
            "conv-same-b",
            trace_id,
            ConversationTraceObservation {
                observed_at: 2_200,
                output_started: true,
                output_size_delta: 5,
            }
        )
    );
    assert_eq!(completed_a.unwrap().unwrap().status, "succeeded");
    assert_eq!(observed_b_after_a_completed.unwrap().unwrap().output_size, 12);

    traces
        .complete_trace(
            "conv-same-b",
            trace_id,
            CompleteConversationTraceParams {
                status: "failed",
                ended_at: 2_500,
                error_code: Some("B_FAILED"),
                retryable: Some(false),
                incomplete: true,
                dropped_span_count: 1,
            },
        )
        .await
        .unwrap();

    let stored_a = traces.get_trace("conv-same-a", trace_id).await.unwrap().unwrap();
    let stored_b = traces.get_trace("conv-same-b", trace_id).await.unwrap().unwrap();
    assert_eq!((stored_a.status.as_str(), stored_a.output_size), ("succeeded", 3));
    assert_eq!((stored_b.status.as_str(), stored_b.output_size), ("failed", 12));
    assert_eq!(traces.list_spans("conv-same-a", trace_id).await.unwrap().len(), 1);
    assert_eq!(traces.list_spans("conv-same-b", trace_id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn runtime_asset_receipts_are_owner_scoped_immutable_and_replay_stable() {
    let (conversations, traces, db) = setup().await;
    let users = SqliteUserRepository::new(db.pool().clone());
    let user_a = users.create_user("runtime-assets-a", "hash-a").await.unwrap();
    let user_b = users.create_user("runtime-assets-b", "hash-b").await.unwrap();
    conversations
        .create(&conversation("conv-assets-a", &user_a.id))
        .await
        .unwrap();
    conversations
        .create(&conversation("conv-assets-b", &user_b.id))
        .await
        .unwrap();
    let trace_id = "turn-shared";
    traces
        .start_trace(&trace("conv-assets-a", trace_id, 1_000))
        .await
        .unwrap();
    traces
        .start_trace(&trace("conv-assets-b", trace_id, 1_000))
        .await
        .unwrap();

    let snapshot_a = runtime_snapshot(&user_a.id, "conv-assets-a", trace_id, 'a', 'b');
    let snapshot_b = runtime_snapshot(&user_b.id, "conv-assets-b", trace_id, 'c', 'd');
    let mut mismatched_id = snapshot_a.clone();
    mismatched_id.runtime_snapshot_id = format!("sha256-{}", "f".repeat(64));
    assert!(matches!(
        traces.save_runtime_asset_snapshot(&mismatched_id).await,
        Err(tjuaeui_db::DbError::Init(_))
    ));
    let mut noncanonical_upstream = snapshot_a.clone();
    noncanonical_upstream.assets[0].upstream_version = Some(" 1.0.0 ".into());
    assert!(matches!(
        traces.save_runtime_asset_snapshot(&noncanonical_upstream).await,
        Err(tjuaeui_db::DbError::Init(_))
    ));
    let stored_a = traces.save_runtime_asset_snapshot(&snapshot_a).await.unwrap();
    let stored_b = traces.save_runtime_asset_snapshot(&snapshot_b).await.unwrap();
    assert_eq!(stored_a.runtime_snapshot_id, snapshot_a.runtime_snapshot_id);
    assert_eq!(stored_b.runtime_snapshot_id, snapshot_b.runtime_snapshot_id);

    assert!(
        traces
            .get_runtime_asset_snapshot(&user_b.id, "conv-assets-a", trace_id)
            .await
            .unwrap()
            .is_none(),
        "another user cannot read a receipt through a known conversation and trace id"
    );
    let wrong_owner_write = runtime_snapshot(&user_b.id, "conv-assets-a", trace_id, 'e', 'f');
    assert!(matches!(
        traces.save_runtime_asset_snapshot(&wrong_owner_write).await,
        Err(tjuaeui_db::DbError::NotFound(_))
    ));

    let mut idempotent_retry = snapshot_a.clone();
    idempotent_retry.created_at = 9_999;
    let retried = traces.save_runtime_asset_snapshot(&idempotent_retry).await.unwrap();
    assert_eq!(retried.created_at, stored_a.created_at);

    let conflicting = runtime_snapshot(&user_a.id, "conv-assets-a", trace_id, 'e', 'f');
    assert!(matches!(
        traces.save_runtime_asset_snapshot(&conflicting).await,
        Err(tjuaeui_db::DbError::Conflict(_))
    ));
    let replayed = traces
        .get_runtime_asset_snapshot(&user_a.id, "conv-assets-a", trace_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(replayed, stored_a, "replay must use the immutable stored receipt");

    let summaries = traces
        .list_runtime_asset_snapshot_summaries(&user_a.id, "conv-assets-a")
        .await
        .unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].trace_id, trace_id);
    assert_eq!(summaries[0].runtime_snapshot_id, stored_a.runtime_snapshot_id);
}

#[tokio::test]
async fn runtime_asset_schema_contains_only_receipt_identity_and_digest_columns() {
    let (_conversations, _traces, db) = setup().await;
    let columns =
        sqlx::query_scalar::<_, String>("SELECT name FROM pragma_table_info('conversation_trace_runtime_asset_refs')")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(
        columns,
        [
            "user_id",
            "conversation_id",
            "trace_id",
            "position",
            "local_asset_id",
            "kind",
            "local_definition_digest",
            "runtime_content_digest",
            "upstream_package",
            "upstream_asset_id",
            "upstream_version",
            "upstream_revision",
        ]
    );
}

#[tokio::test]
async fn restart_marks_running_trace_and_spans_interrupted() {
    let (conversations, traces, _db) = setup().await;
    conversations
        .create(&conversation("conv-restart", USER_ID))
        .await
        .unwrap();
    traces
        .start_trace(&trace("conv-restart", "turn-running", 1_000))
        .await
        .unwrap();
    traces
        .upsert_span(&span("conv-restart", "turn-running", 0))
        .await
        .unwrap();

    assert_eq!(traces.interrupt_running_traces(4_000).await.unwrap(), 1);
    let root = traces.get_trace("conv-restart", "turn-running").await.unwrap().unwrap();
    assert_eq!(root.status, "interrupted");
    assert!(root.incomplete);
    assert_eq!(root.duration_ms, Some(3_000));
    let spans = traces.list_spans("conv-restart", "turn-running").await.unwrap();
    assert_eq!(spans[0].status, "interrupted");
}

#[tokio::test]
async fn terminal_trace_rejects_observe_span_drop_and_second_completion() {
    let (conversations, traces, _db) = setup().await;
    conversations
        .create(&conversation("conv-terminal", USER_ID))
        .await
        .unwrap();
    traces
        .start_trace(&trace("conv-terminal", "turn-terminal", 1_000))
        .await
        .unwrap();
    traces
        .upsert_span(&span("conv-terminal", "turn-terminal", 0))
        .await
        .unwrap();
    let completed = traces
        .complete_trace(
            "conv-terminal",
            "turn-terminal",
            CompleteConversationTraceParams {
                status: "interrupted",
                ended_at: 3_000,
                error_code: None,
                retryable: None,
                incomplete: true,
                dropped_span_count: 2,
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completed.status, "interrupted");
    assert!(completed.incomplete);
    assert_eq!(completed.dropped_span_count, 2);

    assert!(
        traces
            .observe_trace(
                "conv-terminal",
                "turn-terminal",
                ConversationTraceObservation {
                    observed_at: 4_000,
                    output_started: true,
                    output_size_delta: 99,
                },
            )
            .await
            .unwrap()
            .is_none()
    );
    let mut late_span = span("conv-terminal", "turn-terminal", 1);
    late_span.updated_at = 4_000;
    assert_eq!(
        traces.upsert_span(&late_span).await.unwrap(),
        ConversationTraceSpanWriteResult::IgnoredTerminalTrace
    );
    traces
        .record_dropped_spans("conv-terminal", "turn-terminal", 10, 4_000)
        .await
        .unwrap();
    assert!(
        traces
            .complete_trace(
                "conv-terminal",
                "turn-terminal",
                CompleteConversationTraceParams {
                    status: "failed",
                    ended_at: 4_000,
                    error_code: Some("LATE_FAILURE"),
                    retryable: Some(true),
                    incomplete: false,
                    dropped_span_count: 10,
                },
            )
            .await
            .unwrap()
            .is_none()
    );

    let stored = traces
        .get_trace("conv-terminal", "turn-terminal")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.status, "interrupted");
    assert_eq!(stored.ended_at, Some(3_000));
    assert_eq!(stored.output_size, 0);
    assert_eq!(stored.dropped_span_count, 2);
    assert!(stored.incomplete);
    assert_eq!(
        traces.list_spans("conv-terminal", "turn-terminal").await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn retention_keeps_at_most_one_hundred_recent_traces() {
    let (conversations, traces, _db) = setup().await;
    conversations
        .create(&conversation("conv-retention", USER_ID))
        .await
        .unwrap();
    let base = tjuaeui_common::now_ms();
    for index in 0..105 {
        traces
            .start_trace(&trace(
                "conv-retention",
                &format!("turn-{index:03}"),
                base + i64::from(index),
            ))
            .await
            .unwrap();
    }

    let rows = traces.list_traces("conv-retention", 100).await.unwrap();
    assert_eq!(rows.len(), 100);
    assert_eq!(rows[0].trace_id, "turn-104");
    assert_eq!(rows.last().unwrap().trace_id, "turn-005");
}

#[tokio::test]
async fn startup_retention_removes_expired_traces_from_inactive_conversations() {
    let (conversations, traces, _db) = setup().await;
    conversations
        .create(&conversation("conv-expired", USER_ID))
        .await
        .unwrap();
    let now = tjuaeui_common::now_ms();
    let thirty_one_days_ms = 31_i64 * 24 * 60 * 60 * 1_000;
    traces
        .start_trace(&trace("conv-expired", "turn-expired", now - thirty_one_days_ms))
        .await
        .unwrap();

    assert_eq!(traces.prune_expired_traces(now).await.unwrap(), 1);
    assert!(
        traces
            .get_trace("conv-expired", "turn-expired")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn span_limit_is_hard_and_reports_drops_without_growing_storage() {
    let (conversations, traces, _db) = setup().await;
    conversations
        .create(&conversation("conv-limit", USER_ID))
        .await
        .unwrap();
    traces
        .start_trace(&trace("conv-limit", "turn-limit", 1_000))
        .await
        .unwrap();

    for index in 0..500 {
        let result = traces
            .upsert_span(&span("conv-limit", "turn-limit", index))
            .await
            .unwrap();
        assert!(matches!(result, ConversationTraceSpanWriteResult::Stored(_)));
    }
    assert_eq!(
        traces
            .upsert_span(&span("conv-limit", "turn-limit", 500))
            .await
            .unwrap(),
        ConversationTraceSpanWriteResult::DroppedLimit
    );

    let root = traces.get_trace("conv-limit", "turn-limit").await.unwrap().unwrap();
    assert_eq!(root.span_count, 500);
    assert_eq!(root.dropped_span_count, 1);
    assert!(root.truncated);
    assert_eq!(traces.list_spans("conv-limit", "turn-limit").await.unwrap().len(), 500);
}
