use tjuaeui_db::{
    CreateA2aDelegationParams, CreateA2aDelegationPermissionParams, IA2aRepository, RecordA2aAuditParams,
    RecordA2aPushDeliveryParams, RecordA2aPushDeliveryResult, SqliteA2aRepository, UpdateA2aDelegationParams,
    UpsertA2aAgentProfileParams, UpsertA2aCredentialParams, UpsertA2aPushSubscriptionParams, UpsertA2aTaskParams,
    init_database_memory,
};

async fn setup() -> (SqliteA2aRepository, tjuaeui_db::Database) {
    let database = init_database_memory().await.expect("database");
    let repo = SqliteA2aRepository::new(database.pool().clone());
    seed_agent(database.pool(), "a2a-test").await;
    (repo, database)
}

async fn seed_agent(pool: &tjuaeui_db::SqlitePool, agent_id: &str) {
    let now = tjuaeui_common::now_ms();
    sqlx::query(
        "INSERT INTO agent_metadata (
            id, name, agent_type, agent_source, enabled, sort_order, created_at, updated_at
         ) VALUES (?, 'A2A Test', 'a2a', 'custom', 1, 5000, ?, ?)",
    )
    .bind(agent_id)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .expect("seed agent");
}

async fn seed_conversation(pool: &tjuaeui_db::SqlitePool, conversation_id: &str) {
    let now = tjuaeui_common::now_ms();
    sqlx::query(
        "INSERT INTO conversations (
            id, user_id, name, type, status, created_at, updated_at
         ) VALUES (?, 'system_default_user', 'A2A', 'a2a', 'running', ?, ?)",
    )
    .bind(conversation_id)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .expect("seed conversation");
}

async fn seed_task(
    repo: &SqliteA2aRepository,
    database: &tjuaeui_db::Database,
    task_id: &str,
    conversation_id: &str,
    agent_id: &str,
) -> tjuaeui_db::A2aTaskRow {
    seed_conversation(database.pool(), conversation_id).await;
    let remote_task_id = format!("remote-{task_id}");
    let context_id = format!("context-{task_id}");
    repo.upsert_task(UpsertA2aTaskParams {
        id: Some(task_id),
        conversation_id,
        agent_id,
        remote_task_id: Some(&remote_task_id),
        context_id: Some(&context_id),
        state: "working",
        interface_snapshot_json: r#"{"binding":"json_rpc"}"#,
        last_event_id: None,
        artifact_snapshot_json: None,
        push_config_json: None,
    })
    .await
    .expect("seed task")
}

fn profile_params<'a>(agent_id: &'a str, credential_ref: Option<&'a str>) -> UpsertA2aAgentProfileParams<'a> {
    UpsertA2aAgentProfileParams {
        agent_id,
        card_url: "https://agent.example/.well-known/agent-card.json",
        base_url: "https://agent.example",
        display_name: Some("Remote Planner"),
        allow_insecure: false,
        allow_private_network: false,
        compatibility_mode: "v1",
        raw_card_json: Some(r#"{"name":"Remote Planner"}"#),
        normalized_card_json: Some(r#"{"name":"Remote Planner","supportedInterfaces":[]}"#),
        extended_card_json: None,
        protocol_version: Some("1.0"),
        selected_binding: Some("json_rpc"),
        selected_interface_url: Some("https://agent.example/jsonrpc"),
        credential_ref,
        credential_refs_json: "[]",
        selected_tenant: None,
        etag: Some("\"card-v1\""),
        last_modified: None,
        cache_expires_at: Some(2_000),
        fetched_at: Some(1_000),
        card_hash: Some("sha256:test"),
        signature_status: "unchecked",
        trust_status: "untrusted",
        trusted_origin: None,
    }
}

#[tokio::test]
async fn profile_roundtrip_preserves_card_cache_metadata() {
    let (repo, _database) = setup().await;

    let profile = repo
        .upsert_profile(profile_params("a2a-test", None))
        .await
        .expect("upsert profile");

    assert_eq!(profile.protocol_version.as_deref(), Some("1.0"));
    assert_eq!(profile.etag.as_deref(), Some("\"card-v1\""));
    assert_eq!(profile.cache_expires_at, Some(2_000));
}

#[tokio::test]
async fn credential_is_referenced_without_copying_secret_into_profile() {
    let (repo, _database) = setup().await;
    let credential = repo
        .upsert_credential(UpsertA2aCredentialParams {
            id: None,
            scheme_name: Some("bearerAuth"),
            auth_kind: "bearer",
            header_name: None,
            encrypted_secret: Some("encrypted-value"),
            metadata_json: None,
            origin: "https://agent.example",
        })
        .await
        .expect("upsert credential");

    let profile = repo
        .upsert_profile(profile_params("a2a-test", Some(&credential.id)))
        .await
        .expect("upsert profile");

    assert_eq!(profile.credential_ref.as_deref(), Some(credential.id.as_str()));
    assert!(
        !profile
            .raw_card_json
            .as_deref()
            .unwrap_or_default()
            .contains("encrypted-value")
    );
}

#[tokio::test]
async fn task_roundtrip_supports_restart_recovery_lookup() {
    let (repo, database) = setup().await;
    repo.upsert_profile(profile_params("a2a-test", None))
        .await
        .expect("upsert profile");
    seed_conversation(database.pool(), "conv-a2a").await;

    repo.upsert_task(UpsertA2aTaskParams {
        id: None,
        conversation_id: "conv-a2a",
        agent_id: "a2a-test",
        remote_task_id: Some("remote-task"),
        context_id: Some("remote-context"),
        state: "working",
        interface_snapshot_json: r#"{"binding":"json_rpc"}"#,
        last_event_id: Some("event-7"),
        artifact_snapshot_json: Some("[]"),
        push_config_json: None,
    })
    .await
    .expect("upsert task");

    let recovered = repo
        .find_task_by_conversation("conv-a2a")
        .await
        .expect("find task")
        .expect("task");

    assert_eq!(recovered.remote_task_id.as_deref(), Some("remote-task"));
    assert_eq!(recovered.last_event_id.as_deref(), Some("event-7"));
}

#[tokio::test]
async fn push_delivery_is_idempotent_rate_limited_and_stores_only_secret_hashes() {
    let (repo, database) = setup().await;
    repo.upsert_profile(profile_params("a2a-test", None))
        .await
        .expect("upsert profile");
    seed_task(&repo, &database, "task-push", "conv-push", "a2a-test").await;
    let expires_at = tjuaeui_common::now_ms() + 60_000;

    let subscription = repo
        .upsert_push_subscription(UpsertA2aPushSubscriptionParams {
            id: "subscription-1",
            agent_id: "a2a-test",
            task_id: "task-push",
            config_id: "remote-config",
            callback_url: "https://localhost/api/a2a/push/subscription-1/[redacted]",
            path_secret_hash: "sha256:path-secret",
            notification_token_hash: "sha256:notification-token",
            expires_at,
        })
        .await
        .expect("push subscription");

    assert!(!subscription.callback_url.contains("actual-path-secret"));
    assert_eq!(subscription.path_secret_hash, "sha256:path-secret");
    assert_eq!(subscription.notification_token_hash, "sha256:notification-token");

    let delivery = |event_key, payload_hash| RecordA2aPushDeliveryParams {
        subscription_id: "subscription-1",
        event_key,
        event_kind: "task",
        task_id: "task-push",
        payload_hash,
        received_at: 10_000,
    };
    assert_eq!(
        repo.record_push_delivery(delivery("event-1", "sha256:payload-1"), 1)
            .await
            .expect("first delivery"),
        RecordA2aPushDeliveryResult::Accepted
    );
    assert_eq!(
        repo.record_push_delivery(delivery("event-1", "sha256:payload-1"), 1)
            .await
            .expect("duplicate delivery"),
        RecordA2aPushDeliveryResult::Duplicate
    );

    assert_eq!(
        repo.record_push_delivery(delivery("event-2", "sha256:payload-2"), 1)
            .await
            .expect("rate limited delivery"),
        RecordA2aPushDeliveryResult::RateLimited
    );

    repo.delete_push_delivery("subscription-1", "event-1")
        .await
        .expect("delete receipt");
    assert_eq!(
        repo.record_push_delivery(delivery("event-2", "sha256:payload-2"), 1)
            .await
            .expect("retry after rollback"),
        RecordA2aPushDeliveryResult::Accepted
    );

    repo.revoke_push_subscription("subscription-1", 20_000)
        .await
        .expect("revoke subscription");
    let revoked = repo
        .find_push_subscription("subscription-1")
        .await
        .expect("find subscription")
        .expect("subscription");
    assert_eq!(revoked.revoked_at, Some(20_000));
}

#[tokio::test]
async fn delegation_permission_token_lifecycle_graph_and_audit_are_persisted_safely() {
    let (repo, database) = setup().await;
    seed_agent(database.pool(), "a2a-target").await;
    repo.upsert_profile(profile_params("a2a-test", None))
        .await
        .expect("parent profile");
    repo.upsert_profile(profile_params("a2a-target", None))
        .await
        .expect("target profile");
    seed_task(&repo, &database, "task-parent", "conv-parent", "a2a-test").await;
    seed_task(&repo, &database, "task-child", "conv-child", "a2a-target").await;

    let now = tjuaeui_common::now_ms();
    let permission = repo
        .create_delegation_permission(CreateA2aDelegationPermissionParams {
            id: "permission-1",
            parent_task_id: "task-parent",
            target_agent_ids_json: r#"["a2a-target"]"#,
            scopes_json: r#"["delegate:message","delegate:cancel"]"#,
            requested_expires_at: now + 60_000,
        })
        .await
        .expect("create permission");
    assert_eq!(permission.status, "pending");
    assert!(permission.capability_token_hash.is_none());

    let approved = repo
        .approve_delegation_permission("permission-1", "sha256:capability-token", now)
        .await
        .expect("approve permission");
    assert_eq!(approved.status, "approved");
    assert_eq!(
        approved.capability_token_hash.as_deref(),
        Some("sha256:capability-token")
    );
    assert_ne!(
        approved.capability_token_hash.as_deref(),
        Some("actual-capability-token")
    );

    let edge = repo
        .create_delegation(CreateA2aDelegationParams {
            id: "delegation-1",
            parent_task_id: "task-parent",
            target_agent_id: "a2a-target",
            permission_id: "permission-1",
            idempotency_key: "request-1",
        })
        .await
        .expect("create delegation");
    assert_eq!(edge.state, "dispatching");

    let duplicate = repo
        .create_delegation(CreateA2aDelegationParams {
            id: "delegation-duplicate",
            parent_task_id: "task-parent",
            target_agent_id: "a2a-target",
            permission_id: "permission-1",
            idempotency_key: "request-1",
        })
        .await;
    assert!(duplicate.is_err(), "idempotency key must be unique per edge");

    let updated = repo
        .update_delegation(UpdateA2aDelegationParams {
            id: "delegation-1",
            child_task_id: Some("task-child"),
            state: "active",
            context_id: Some("context-task-child"),
            last_error_code: None,
        })
        .await
        .expect("update delegation");
    assert_eq!(updated.child_task_id.as_deref(), Some("task-child"));
    assert_eq!(
        repo.find_delegation_by_idempotency("task-parent", "a2a-target", "request-1")
            .await
            .expect("lookup idempotency")
            .expect("delegation")
            .id,
        "delegation-1"
    );

    repo.record_a2a_audit(RecordA2aAuditParams {
        event_type: "delegation.dispatched",
        actor_agent_id: Some("a2a-test"),
        target_agent_id: Some("a2a-target"),
        task_id: Some("task-parent"),
        delegation_id: Some("delegation-1"),
        metadata_json: r#"{"messageHash":"sha256:message","messageBytes":42}"#,
    })
    .await
    .expect("record audit");
    let audit = repo.list_a2a_audit_for_task("task-parent").await.expect("list audit");
    assert_eq!(audit.len(), 1);
    assert!(audit[0].metadata_json.contains("messageHash"));
    assert!(!audit[0].metadata_json.contains("actual-capability-token"));
    assert!(!audit[0].metadata_json.contains("secret message"));

    repo.revoke_delegation_permission("permission-1", now + 1)
        .await
        .expect("revoke permission");
    let revoked = repo
        .find_delegation_permission("permission-1")
        .await
        .expect("find permission")
        .expect("permission");
    assert_eq!(revoked.status, "revoked");
    assert!(revoked.capability_token_hash.is_none());
}

#[tokio::test]
async fn deleting_missing_profile_returns_not_found() {
    let (repo, _database) = setup().await;

    let error = repo.delete_profile("missing").await.expect_err("must fail");

    assert!(matches!(error, tjuaeui_db::DbError::NotFound(_)));
}
