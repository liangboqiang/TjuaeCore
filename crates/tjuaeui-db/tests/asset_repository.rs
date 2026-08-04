use tjuaeui_db::{
    CommitAssetRuntimeBindingParams, ConfigureAssetOverlayParams, CreateAssetSnapshotParams,
    CreateAssetTryRunReceiptParams, EncryptedAssetSecretUpdate, IAssetRepository, SqliteAssetRepository,
    StartAssetOperationParams, UpdateAssetOperationParams, UpsertAssetRecordParams, UpsertAssetUpstreamParams,
    init_database_memory,
};

const SYSTEM_USER: &str = "system_default_user";

async fn setup() -> (SqliteAssetRepository, tjuaeui_db::Database) {
    let db = init_database_memory().await.unwrap();
    let repo = SqliteAssetRepository::new(db.pool().clone());
    (repo, db)
}

async fn seed_user(db: &tjuaeui_db::Database, id: &str) {
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, created_at, updated_at)
         VALUES (?, ?, '', 1, 1)",
    )
    .bind(id)
    .bind(id)
    .execute(db.pool())
    .await
    .unwrap();
}

async fn create_asset(repo: &SqliteAssetRepository, user_id: &str, id: &str, digest: &str) {
    repo.upsert_record(UpsertAssetRecordParams {
        user_id,
        id,
        kind: "skill",
        display_name: "演示技能",
        description: Some("description"),
        origin: "hub",
        trust: "official",
        scope: "user",
        editability: "full",
        workspace_key: "users/demo/assets/skill-demo",
        definition_digest: digest,
        entry_file: Some("SKILL.md"),
        runtime_id: Some(id),
        now: 10,
    })
    .await
    .unwrap();
}

async fn track_asset(repo: &SqliteAssetRepository, user_id: &str, id: &str, package_name: &str, digest: &str) {
    repo.upsert_upstream(UpsertAssetUpstreamParams {
        user_id,
        asset_id: id,
        package_name,
        remote_asset_id: &format!("{package_name}/skill/{id}"),
        version: "1.0.0",
        source_revision: &"b".repeat(40),
        remote_digest: digest,
        tracking_mode: "tracked",
        checked_at: Some(11),
    })
    .await
    .unwrap();
    repo.create_snapshot(CreateAssetSnapshotParams {
        user_id,
        asset_id: id,
        base_digest: digest,
        object_key: &format!("objects/{}", digest.trim_start_matches("sha256-")),
        manifest_json: "[]",
        created_at: 12,
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn asset_records_upstream_and_full_snapshot_are_persisted() {
    let (repo, _db) = setup().await;
    let digest = format!("sha256-{}", "a".repeat(64));
    create_asset(&repo, SYSTEM_USER, "skill-demo", &digest).await;

    repo.upsert_upstream(UpsertAssetUpstreamParams {
        user_id: SYSTEM_USER,
        asset_id: "skill-demo",
        package_name: "tjuaeext-skill-demo",
        remote_asset_id: "org.tjuae.skill.demo",
        version: "1.0.0",
        source_revision: &"b".repeat(40),
        remote_digest: &digest,
        tracking_mode: "tracked",
        checked_at: Some(11),
    })
    .await
    .unwrap();
    let snapshot = repo
        .create_snapshot(CreateAssetSnapshotParams {
            user_id: SYSTEM_USER,
            asset_id: "skill-demo",
            base_digest: &digest,
            object_key: &format!("objects/{}", digest.trim_start_matches("sha256-")),
            manifest_json: r#"[{"path":"SKILL.md","digest":"sha256-a"}]"#,
            created_at: 12,
        })
        .await
        .unwrap();

    assert_eq!(repo.list(SYSTEM_USER, Some("skill")).await.unwrap().len(), 1);
    assert_eq!(
        repo.get_upstream(SYSTEM_USER, "skill-demo")
            .await
            .unwrap()
            .unwrap()
            .source_revision
            .len(),
        40
    );
    assert_eq!(
        repo.latest_snapshot(SYSTEM_USER, "skill-demo").await.unwrap().unwrap(),
        snapshot
    );
}

#[tokio::test]
async fn asset_records_reject_unowned_workspace_scope() {
    let (repo, _db) = setup().await;
    let result = repo
        .upsert_record(UpsertAssetRecordParams {
            user_id: SYSTEM_USER,
            id: "workspace-without-identity",
            kind: "skill",
            display_name: "无归属工作区资产",
            description: None,
            origin: "local",
            trust: "community",
            scope: "workspace",
            editability: "full",
            workspace_key: "users/demo/assets/workspace-without-identity",
            definition_digest: "sha256-invalid-scope",
            entry_file: Some("SKILL.md"),
            runtime_id: None,
            now: 10,
        })
        .await;

    assert!(result.is_err(), "V1 数据库不得持久化没有稳定工作区身份的资产");
}

#[tokio::test]
async fn asset_repository_isolates_users() {
    let (repo, db) = setup().await;
    seed_user(&db, "other-user").await;
    create_asset(&repo, SYSTEM_USER, "same-id", "sha256-system").await;
    create_asset(&repo, "other-user", "same-id", "sha256-other").await;

    assert_eq!(
        repo.get(SYSTEM_USER, "same-id")
            .await
            .unwrap()
            .unwrap()
            .definition_digest,
        "sha256-system"
    );
    assert_eq!(
        repo.get("other-user", "same-id")
            .await
            .unwrap()
            .unwrap()
            .definition_digest,
        "sha256-other"
    );
}

#[tokio::test]
async fn system_seed_is_visible_to_other_users_but_user_assets_remain_private() {
    let (repo, db) = setup().await;
    seed_user(&db, "other-user").await;
    repo.upsert_record(UpsertAssetRecordParams {
        user_id: SYSTEM_USER,
        id: "system-skill",
        kind: "skill",
        display_name: "系统技能",
        description: None,
        origin: "seed",
        trust: "official",
        scope: "system",
        editability: "readOnly",
        workspace_key: "system/skill",
        definition_digest: "sha256-system",
        entry_file: Some("SKILL.md"),
        runtime_id: None,
        now: 1,
    })
    .await
    .unwrap();
    create_asset(&repo, SYSTEM_USER, "private-skill", "sha256-private").await;

    let visible = repo.list("other-user", Some("skill")).await.unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].id, "system-skill");
    assert!(repo.get("other-user", "system-skill").await.unwrap().is_some());
    assert!(repo.get("other-user", "private-skill").await.unwrap().is_none());
}

#[tokio::test]
async fn user_asset_shadows_same_id_system_seed_without_duplicate_rows() {
    let (repo, db) = setup().await;
    seed_user(&db, "other-user").await;
    repo.upsert_record(UpsertAssetRecordParams {
        user_id: SYSTEM_USER,
        id: "shadowed-skill",
        kind: "skill",
        display_name: "系统技能",
        description: None,
        origin: "seed",
        trust: "official",
        scope: "system",
        editability: "readOnly",
        workspace_key: "system/shadowed-skill",
        definition_digest: "sha256-system",
        entry_file: Some("SKILL.md"),
        runtime_id: None,
        now: 1,
    })
    .await
    .unwrap();
    create_asset(&repo, "other-user", "shadowed-skill", "sha256-user").await;

    let visible = repo.list("other-user", Some("skill")).await.unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].user_id, "other-user");
    assert_eq!(visible[0].definition_digest, "sha256-user");
}

#[tokio::test]
async fn operation_idempotency_returns_the_original_operation() {
    let (repo, _db) = setup().await;
    create_asset(&repo, SYSTEM_USER, "skill-demo", "sha256-local").await;
    let first = repo
        .start_operation(StartAssetOperationParams {
            user_id: SYSTEM_USER,
            operation_id: "op-first",
            idempotency_key: "install:request-1",
            asset_id: "skill-demo",
            kind: "install",
            phase: "staging",
            recovery_json: "{}",
            started_at: 20,
        })
        .await
        .unwrap();
    let retried = repo
        .start_operation(StartAssetOperationParams {
            user_id: SYSTEM_USER,
            operation_id: "op-second",
            idempotency_key: "install:request-1",
            asset_id: "skill-demo",
            kind: "install",
            phase: "different",
            recovery_json: "{}",
            started_at: 21,
        })
        .await
        .unwrap();

    assert_eq!(first.operation_id, "op-first");
    assert_eq!(retried.operation_id, first.operation_id);
    let completed = repo
        .update_operation(
            SYSTEM_USER,
            &first.operation_id,
            UpdateAssetOperationParams {
                state: "succeeded",
                phase: "complete",
                error_code: None,
                recovery_json: "{}",
                finished_at: Some(25),
                updated_at: 25,
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completed.state, "succeeded");
    assert!(repo.list_recoverable_operations().await.unwrap().is_empty());
}

#[tokio::test]
async fn uninstall_commit_removes_asset_and_completes_operation_atomically() {
    let (repo, _db) = setup().await;
    create_asset(&repo, SYSTEM_USER, "remove-me", "sha256-local").await;
    repo.start_operation(StartAssetOperationParams {
        user_id: SYSTEM_USER,
        operation_id: "op-uninstall",
        idempotency_key: "uninstall:remove-me",
        asset_id: "remove-me",
        kind: "uninstall",
        phase: "staging",
        recovery_json: r#"{"workspaceKey":"safe-key"}"#,
        started_at: 20,
    })
    .await
    .unwrap();

    let operation = repo
        .commit_uninstall(SYSTEM_USER, "remove-me", "op-uninstall", 21)
        .await
        .unwrap();

    assert_eq!(operation.state, "succeeded");
    assert_eq!(operation.phase, "complete");
    assert_eq!(operation.recovery_json, "{}");
    assert!(repo.get(SYSTEM_USER, "remove-me").await.unwrap().is_none());
}

#[tokio::test]
async fn uninstall_commit_rolls_back_delete_when_operation_does_not_match() {
    let (repo, _db) = setup().await;
    create_asset(&repo, SYSTEM_USER, "keep-me", "sha256-local").await;

    let result = repo
        .commit_uninstall(SYSTEM_USER, "keep-me", "missing-operation", 21)
        .await;

    assert!(result.is_err());
    assert!(repo.get(SYSTEM_USER, "keep-me").await.unwrap().is_some());
}

#[tokio::test]
async fn tracked_asset_commit_rejects_mismatched_snapshot_without_partial_write() {
    let (repo, _db) = setup().await;
    let result = repo
        .commit_tracked_asset(
            UpsertAssetRecordParams {
                user_id: SYSTEM_USER,
                id: "atomic-demo",
                kind: "skill",
                display_name: "Atomic",
                description: None,
                origin: "hub",
                trust: "verified",
                scope: "user",
                editability: "full",
                workspace_key: "assets/atomic-demo",
                definition_digest: "sha256-local",
                entry_file: Some("SKILL.md"),
                runtime_id: None,
                now: 1,
            },
            UpsertAssetUpstreamParams {
                user_id: SYSTEM_USER,
                asset_id: "atomic-demo",
                package_name: "tjuaeext-atomic-demo",
                remote_asset_id: "org.tjuae.skill.atomic-demo",
                version: "1.0.0",
                source_revision: &"a".repeat(40),
                remote_digest: "sha256-local",
                tracking_mode: "tracked",
                checked_at: Some(1),
            },
            CreateAssetSnapshotParams {
                user_id: SYSTEM_USER,
                asset_id: "atomic-demo",
                base_digest: "sha256-different",
                object_key: "objects/different",
                manifest_json: "[]",
                created_at: 1,
            },
        )
        .await;

    assert!(result.is_err());
    assert!(repo.get(SYSTEM_USER, "atomic-demo").await.unwrap().is_none());
}

#[tokio::test]
async fn detach_resolution_converts_the_whole_bundle_to_local_and_removes_all_bases() {
    let (repo, db) = setup().await;
    let digest_a = format!("sha256-{}", "a".repeat(64));
    let digest_b = format!("sha256-{}", "b".repeat(64));
    for (id, digest) in [("bundle-a", &digest_a), ("bundle-b", &digest_b)] {
        create_asset(&repo, SYSTEM_USER, id, digest).await;
        track_asset(&repo, SYSTEM_USER, id, "tjuaeext-bundle", digest).await;
        repo.create_snapshot(CreateAssetSnapshotParams {
            user_id: SYSTEM_USER,
            asset_id: id,
            base_digest: &format!("sha256-{}", "c".repeat(64)),
            object_key: "objects/older",
            manifest_json: "[]",
            created_at: 1,
        })
        .await
        .unwrap();
    }
    repo.start_operation(StartAssetOperationParams {
        user_id: SYSTEM_USER,
        operation_id: "op-detach",
        idempotency_key: "resolve:detach",
        asset_id: "bundle-a",
        kind: "resolve",
        phase: "detach",
        recovery_json: "{}",
        started_at: 20,
    })
    .await
    .unwrap();

    let operation = repo
        .commit_detach_resolution(
            SYSTEM_USER,
            &["bundle-a".into(), "bundle-b".into()],
            "bundle-a",
            "op-detach",
            21,
        )
        .await
        .unwrap();

    assert_eq!(operation.state, "succeeded");
    for id in ["bundle-a", "bundle-b"] {
        assert_eq!(repo.get(SYSTEM_USER, id).await.unwrap().unwrap().origin, "local");
        assert!(repo.get_upstream(SYSTEM_USER, id).await.unwrap().is_none());
        assert!(repo.latest_snapshot(SYSTEM_USER, id).await.unwrap().is_none());
        let snapshot_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM asset_snapshots WHERE user_id = ? AND asset_id = ?")
                .bind(SYSTEM_USER)
                .bind(id)
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(snapshot_count, 0);
    }
}

#[tokio::test]
async fn detach_bundle_rolls_back_every_member_when_the_second_delete_fails() {
    let (repo, db) = setup().await;
    let digest_a = format!("sha256-{}", "a".repeat(64));
    let digest_b = format!("sha256-{}", "b".repeat(64));
    for (id, digest) in [("bundle-a", &digest_a), ("bundle-b", &digest_b)] {
        create_asset(&repo, SYSTEM_USER, id, digest).await;
        track_asset(&repo, SYSTEM_USER, id, "tjuaeext-bundle", digest).await;
    }
    sqlx::query(
        "CREATE TRIGGER fail_second_detach
         BEFORE DELETE ON asset_upstreams
         WHEN OLD.user_id = 'system_default_user' AND OLD.asset_id = 'bundle-b'
         BEGIN SELECT RAISE(ABORT, 'injected detach failure'); END",
    )
    .execute(db.pool())
    .await
    .unwrap();

    let result = repo
        .detach_assets(SYSTEM_USER, &["bundle-a".into(), "bundle-b".into()], 21)
        .await;
    assert!(result.is_err());

    for id in ["bundle-a", "bundle-b"] {
        assert_eq!(repo.get(SYSTEM_USER, id).await.unwrap().unwrap().origin, "hub");
        assert!(repo.get_upstream(SYSTEM_USER, id).await.unwrap().is_some());
        assert!(repo.latest_snapshot(SYSTEM_USER, id).await.unwrap().is_some());
    }
}

#[tokio::test]
async fn publishing_uses_only_the_github_operation_ledger() {
    let (_repo, db) = setup().await;
    let removed_table = format!("asset_{}", "publications");
    let dead_table_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?")
            .bind(removed_table)
            .fetch_one(db.pool())
            .await
            .unwrap();
    let authoritative_table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'github_publish_operations'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();

    assert_eq!(dead_table_count, 0);
    assert_eq!(authoritative_table_count, 1);
}

#[tokio::test]
async fn typed_overlay_runtime_state_and_binding_have_an_independent_lifecycle() {
    let (repo, _db) = setup().await;
    let digest = format!("sha256-{}", "a".repeat(64));
    repo.upsert_record(UpsertAssetRecordParams {
        user_id: SYSTEM_USER,
        id: "engine-demo",
        kind: "engineAdapter",
        display_name: "演示引擎",
        description: None,
        origin: "hub",
        trust: "verified",
        scope: "user",
        editability: "full",
        workspace_key: "users/system/assets/engine-demo",
        definition_digest: &digest,
        entry_file: Some("engine-adapter.json"),
        runtime_id: Some("engine-demo"),
        now: 10,
    })
    .await
    .unwrap();
    assert_eq!(
        repo.get_runtime_state(SYSTEM_USER, "engine-demo")
            .await
            .unwrap()
            .unwrap()
            .state,
        "notConfigured"
    );

    let first = repo
        .configure_overlay(ConfigureAssetOverlayParams {
            user_id: SYSTEM_USER,
            asset_id: "engine-demo",
            kind: "engineAdapter",
            overlay_json: r#"{"kind":"engineAdapter","configuration":{"command":"demo"}}"#,
            expected_version: None,
            secret_updates: &[],
            now: 11,
        })
        .await
        .unwrap();
    assert_eq!(first.version, 1);
    assert_eq!(
        repo.get_runtime_state(SYSTEM_USER, "engine-demo")
            .await
            .unwrap()
            .unwrap()
            .state,
        "inactive"
    );

    repo.commit_try_run_receipt(CreateAssetTryRunReceiptParams {
        user_id: SYSTEM_USER,
        asset_id: "engine-demo",
        receipt_id: "receipt-engine-demo",
        idempotency_key: "try-engine-demo",
        definition_digest: &digest,
        overlay_version: 1,
        portable_runtime_id: "engine-demo",
        projection_runtime_id: "tjuae-proj-v1-0000000000000000000000000000000000000000000000000000000000000000",
        created_at: 12,
    })
    .await
    .unwrap();
    repo.commit_runtime_binding(CommitAssetRuntimeBindingParams {
        user_id: SYSTEM_USER,
        asset_id: "engine-demo",
        kind: "engineAdapter",
        projection_kind: "engineAdapter",
        portable_runtime_id: "engine-demo",
        projection_runtime_id: "tjuae-proj-v1-0000000000000000000000000000000000000000000000000000000000000000",
        definition_digest: &digest,
        overlay_version: 1,
        try_run_receipt_id: "receipt-engine-demo",
        health_status: "healthy",
        last_error_code: None,
        projected_at: 12,
        health_checked_at: Some(12),
    })
    .await
    .unwrap();
    assert_eq!(
        repo.get_runtime_state(SYSTEM_USER, "engine-demo")
            .await
            .unwrap()
            .unwrap()
            .state,
        "active"
    );

    let second = repo
        .configure_overlay(ConfigureAssetOverlayParams {
            user_id: SYSTEM_USER,
            asset_id: "engine-demo",
            kind: "engineAdapter",
            overlay_json: r#"{"kind":"engineAdapter","configuration":{"command":"demo-v2"}}"#,
            expected_version: Some(1),
            secret_updates: &[],
            now: 13,
        })
        .await
        .unwrap();
    assert_eq!(second.version, 2);
    assert_eq!(
        repo.get_runtime_state(SYSTEM_USER, "engine-demo")
            .await
            .unwrap()
            .unwrap()
            .state,
        "needsRepair"
    );
    assert_eq!(
        repo.get_runtime_binding(SYSTEM_USER, "engine-demo")
            .await
            .unwrap()
            .unwrap()
            .overlay_version,
        1
    );

    let inactive = repo.deactivate_runtime(SYSTEM_USER, "engine-demo", 14).await.unwrap();
    assert_eq!(inactive.state, "inactive");
    assert!(
        repo.get_runtime_binding(SYSTEM_USER, "engine-demo")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn overlays_and_runtime_metadata_are_isolated_per_user_for_system_assets() {
    let (repo, db) = setup().await;
    seed_user(&db, "alice").await;
    seed_user(&db, "bob").await;
    repo.upsert_record(UpsertAssetRecordParams {
        user_id: SYSTEM_USER,
        id: "system-engine",
        kind: "engineAdapter",
        display_name: "系统引擎",
        description: None,
        origin: "seed",
        trust: "official",
        scope: "system",
        editability: "overlay",
        workspace_key: "system/engine",
        definition_digest: "sha256-system-engine",
        entry_file: Some("engine-adapter.json"),
        runtime_id: Some("system-engine"),
        now: 1,
    })
    .await
    .unwrap();

    for (user_id, command) in [("alice", "alice-engine"), ("bob", "bob-engine")] {
        repo.configure_overlay(ConfigureAssetOverlayParams {
            user_id,
            asset_id: "system-engine",
            kind: "engineAdapter",
            overlay_json: &format!(r#"{{"kind":"engineAdapter","configuration":{{"command":"{command}"}}}}"#),
            expected_version: None,
            secret_updates: &[],
            now: 2,
        })
        .await
        .unwrap();
    }

    let alice = repo.get_overlay("alice", "system-engine").await.unwrap().unwrap();
    let bob = repo.get_overlay("bob", "system-engine").await.unwrap().unwrap();
    assert_eq!(alice.asset_owner_id, SYSTEM_USER);
    assert_eq!(bob.asset_owner_id, SYSTEM_USER);
    assert_ne!(alice.overlay_json, bob.overlay_json);
    assert_eq!(
        repo.get_runtime_state("alice", "system-engine")
            .await
            .unwrap()
            .unwrap()
            .state,
        "inactive"
    );

    create_asset(&repo, "alice", "alice-private", "sha256-alice").await;
    assert!(repo.get_overlay("bob", "alice-private").await.unwrap().is_none());
    assert!(
        repo.configure_overlay(ConfigureAssetOverlayParams {
            user_id: "bob",
            asset_id: "alice-private",
            kind: "skill",
            overlay_json: r#"{"kind":"skill","configuration":{}}"#,
            expected_version: None,
            secret_updates: &[],
            now: 3,
        })
        .await
        .is_err()
    );
}

#[tokio::test]
async fn portable_runtime_ids_may_be_shared_but_projection_bindings_are_user_isolated() {
    let (repo, db) = setup().await;
    seed_user(&db, "alice").await;
    seed_user(&db, "bob").await;
    repo.upsert_record(UpsertAssetRecordParams {
        user_id: SYSTEM_USER,
        id: "shared-skill",
        kind: "skill",
        display_name: "共享技能",
        description: None,
        origin: "seed",
        trust: "official",
        scope: "system",
        editability: "overlay",
        workspace_key: "system/shared-skill",
        definition_digest: "sha256-shared-skill",
        entry_file: Some("SKILL.md"),
        runtime_id: Some("portable-shared-skill"),
        now: 1,
    })
    .await
    .unwrap();

    let alice_projection = "tjuae-proj-v1-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let bob_projection = "tjuae-proj-v1-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    for (user_id, projection_runtime_id, receipt_id) in [
        ("alice", alice_projection, "receipt-alice"),
        ("bob", bob_projection, "receipt-bob"),
    ] {
        let idempotency_key = format!("try-{user_id}");
        repo.commit_try_run_receipt(CreateAssetTryRunReceiptParams {
            user_id,
            asset_id: "shared-skill",
            receipt_id,
            idempotency_key: &idempotency_key,
            definition_digest: "sha256-shared-skill",
            overlay_version: 0,
            portable_runtime_id: "portable-shared-skill",
            projection_runtime_id,
            created_at: 2,
        })
        .await
        .unwrap();
        repo.commit_runtime_binding(CommitAssetRuntimeBindingParams {
            user_id,
            asset_id: "shared-skill",
            kind: "skill",
            projection_kind: "skill",
            portable_runtime_id: "portable-shared-skill",
            projection_runtime_id,
            definition_digest: "sha256-shared-skill",
            overlay_version: 0,
            try_run_receipt_id: receipt_id,
            health_status: "healthy",
            last_error_code: None,
            projected_at: 3,
            health_checked_at: Some(3),
        })
        .await
        .unwrap();
    }

    let alice = repo.list_runtime_bindings("alice", Some("skill")).await.unwrap();
    let bob = repo.list_runtime_bindings("bob", Some("skill")).await.unwrap();
    assert_eq!(alice.len(), 1);
    assert_eq!(bob.len(), 1);
    assert_eq!(alice[0].portable_runtime_id, bob[0].portable_runtime_id);
    assert_eq!(alice[0].projection_runtime_id, alice_projection);
    assert_eq!(bob[0].projection_runtime_id, bob_projection);
    assert_ne!(alice[0].projection_runtime_id, bob[0].projection_runtime_id);

    repo.deactivate_runtime("alice", "shared-skill", 4).await.unwrap();
    assert!(
        repo.get_runtime_binding("alice", "shared-skill")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        repo.get_runtime_binding("bob", "shared-skill")
            .await
            .unwrap()
            .unwrap()
            .projection_runtime_id,
        bob_projection
    );
    assert_eq!(
        repo.get_runtime_state("bob", "shared-skill")
            .await
            .unwrap()
            .unwrap()
            .state,
        "active"
    );
}

#[tokio::test]
async fn receipt_rejects_a_portable_runtime_id_that_is_not_in_the_definition() {
    let (repo, _db) = setup().await;
    create_asset(&repo, SYSTEM_USER, "portable-check", "sha256-portable-check").await;

    let result = repo
        .commit_try_run_receipt(CreateAssetTryRunReceiptParams {
            user_id: SYSTEM_USER,
            asset_id: "portable-check",
            receipt_id: "receipt-portable-check",
            idempotency_key: "try-portable-check",
            definition_digest: "sha256-portable-check",
            overlay_version: 0,
            portable_runtime_id: "forged-portable-id",
            projection_runtime_id: "tjuae-proj-v1-cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            created_at: 2,
        })
        .await;

    assert!(matches!(result, Err(tjuaeui_db::DbError::Conflict(_))));
    assert!(
        repo.get_try_run_receipt(SYSTEM_USER, "portable-check")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn overlay_and_binding_conflicts_roll_back_without_partial_runtime_state() {
    let (repo, _db) = setup().await;
    create_asset(&repo, SYSTEM_USER, "rollback-skill", "sha256-current").await;
    repo.configure_overlay(ConfigureAssetOverlayParams {
        user_id: SYSTEM_USER,
        asset_id: "rollback-skill",
        kind: "skill",
        overlay_json: r#"{"kind":"skill","configuration":{}}"#,
        expected_version: None,
        secret_updates: &[],
        now: 2,
    })
    .await
    .unwrap();

    let stale = repo
        .configure_overlay(ConfigureAssetOverlayParams {
            user_id: SYSTEM_USER,
            asset_id: "rollback-skill",
            kind: "skill",
            overlay_json: r#"{"kind":"skill","configuration":{},"changed":true}"#,
            expected_version: Some(0),
            secret_updates: &[],
            now: 3,
        })
        .await;
    assert!(stale.is_err());
    let unchanged = repo.get_overlay(SYSTEM_USER, "rollback-skill").await.unwrap().unwrap();
    assert_eq!(unchanged.version, 1);
    assert!(!unchanged.overlay_json.contains("changed"));

    let stale_binding = repo
        .commit_runtime_binding(CommitAssetRuntimeBindingParams {
            user_id: SYSTEM_USER,
            asset_id: "rollback-skill",
            kind: "skill",
            projection_kind: "skill",
            portable_runtime_id: "rollback-skill",
            projection_runtime_id: "tjuae-proj-v1-1111111111111111111111111111111111111111111111111111111111111111",
            definition_digest: "sha256-stale",
            overlay_version: 1,
            try_run_receipt_id: "missing-receipt",
            health_status: "healthy",
            last_error_code: None,
            projected_at: 4,
            health_checked_at: Some(4),
        })
        .await;
    assert!(stale_binding.is_err());
    assert!(
        repo.get_runtime_binding(SYSTEM_USER, "rollback-skill")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        repo.get_runtime_state(SYSTEM_USER, "rollback-skill")
            .await
            .unwrap()
            .unwrap()
            .state,
        "inactive"
    );
}

#[tokio::test]
async fn credential_set_preserve_clear_conflict_and_uninstall_are_atomic() {
    let (repo, db) = setup().await;
    seed_user(&db, "other-user").await;
    create_asset(&repo, SYSTEM_USER, "credential-skill", "sha256-credential").await;

    let first_updates = [EncryptedAssetSecretUpdate::Set {
        slot: "api-token",
        ciphertext: "ciphertext-v1",
        key_version: 1,
    }];
    repo.configure_overlay(ConfigureAssetOverlayParams {
        user_id: SYSTEM_USER,
        asset_id: "credential-skill",
        kind: "skill",
        overlay_json: r#"{"kind":"skill","configuration":{}}"#,
        expected_version: None,
        secret_updates: &first_updates,
        now: 20,
    })
    .await
    .unwrap();
    let stored = repo.list_credentials(SYSTEM_USER, "credential-skill").await.unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].slot, "api-token");
    assert_eq!(stored[0].ciphertext, "ciphertext-v1");
    assert!(
        repo.list_credentials("other-user", "credential-skill")
            .await
            .unwrap()
            .is_empty()
    );

    repo.configure_overlay(ConfigureAssetOverlayParams {
        user_id: SYSTEM_USER,
        asset_id: "credential-skill",
        kind: "skill",
        overlay_json: r#"{"kind":"skill","configuration":{}}"#,
        expected_version: Some(1),
        secret_updates: &[],
        now: 21,
    })
    .await
    .unwrap();
    assert_eq!(
        repo.list_credentials(SYSTEM_USER, "credential-skill").await.unwrap()[0].ciphertext,
        "ciphertext-v1"
    );

    let conflicting = [EncryptedAssetSecretUpdate::Set {
        slot: "api-token",
        ciphertext: "must-not-commit",
        key_version: 1,
    }];
    assert!(
        repo.configure_overlay(ConfigureAssetOverlayParams {
            user_id: SYSTEM_USER,
            asset_id: "credential-skill",
            kind: "skill",
            overlay_json: r#"{"kind":"skill","configuration":{}}"#,
            expected_version: Some(1),
            secret_updates: &conflicting,
            now: 22,
        })
        .await
        .is_err()
    );
    assert_eq!(
        repo.list_credentials(SYSTEM_USER, "credential-skill").await.unwrap()[0].ciphertext,
        "ciphertext-v1"
    );

    let clear = [EncryptedAssetSecretUpdate::Clear { slot: "api-token" }];
    repo.configure_overlay(ConfigureAssetOverlayParams {
        user_id: SYSTEM_USER,
        asset_id: "credential-skill",
        kind: "skill",
        overlay_json: r#"{"kind":"skill","configuration":{}}"#,
        expected_version: Some(2),
        secret_updates: &clear,
        now: 23,
    })
    .await
    .unwrap();
    assert!(
        repo.list_credentials(SYSTEM_USER, "credential-skill")
            .await
            .unwrap()
            .is_empty()
    );

    let second_set = [EncryptedAssetSecretUpdate::Set {
        slot: "api-token",
        ciphertext: "ciphertext-v2",
        key_version: 1,
    }];
    repo.configure_overlay(ConfigureAssetOverlayParams {
        user_id: SYSTEM_USER,
        asset_id: "credential-skill",
        kind: "skill",
        overlay_json: r#"{"kind":"skill","configuration":{}}"#,
        expected_version: Some(3),
        secret_updates: &second_set,
        now: 24,
    })
    .await
    .unwrap();
    assert!(repo.delete(SYSTEM_USER, "credential-skill").await.unwrap());
    assert!(
        repo.list_credentials(SYSTEM_USER, "credential-skill")
            .await
            .unwrap()
            .is_empty()
    );
}
