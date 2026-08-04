use sqlx::SqlitePool;

use crate::error::DbError;
use crate::models::{
    A2aAgentProfileRow, A2aAuditEventRow, A2aCredentialRow, A2aDelegationPermissionRow, A2aDelegationRow,
    A2aPushSubscriptionRow, A2aTaskRow,
};
use crate::repository::a2a::{
    CreateA2aDelegationParams, CreateA2aDelegationPermissionParams, IA2aRepository, RecordA2aAuditParams,
    RecordA2aPushDeliveryParams, RecordA2aPushDeliveryResult, UpdateA2aDelegationParams, UpsertA2aAgentProfileParams,
    UpsertA2aCredentialParams, UpsertA2aPushSubscriptionParams, UpsertA2aTaskParams,
};

#[derive(Clone, Debug)]
pub struct SqliteA2aRepository {
    pool: SqlitePool,
}

impl SqliteA2aRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl IA2aRepository for SqliteA2aRepository {
    async fn list_profiles(&self) -> Result<Vec<A2aAgentProfileRow>, DbError> {
        Ok(
            sqlx::query_as::<_, A2aAgentProfileRow>("SELECT * FROM a2a_agent_profiles ORDER BY created_at ASC")
                .fetch_all(&self.pool)
                .await?,
        )
    }

    async fn find_profile(&self, agent_id: &str) -> Result<Option<A2aAgentProfileRow>, DbError> {
        Ok(
            sqlx::query_as::<_, A2aAgentProfileRow>("SELECT * FROM a2a_agent_profiles WHERE agent_id = ?")
                .bind(agent_id)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    async fn upsert_profile(&self, params: UpsertA2aAgentProfileParams<'_>) -> Result<A2aAgentProfileRow, DbError> {
        let now = tjuaeui_common::now_ms();
        sqlx::query(
            "INSERT INTO a2a_agent_profiles (
                agent_id, card_url, base_url, display_name, allow_insecure,
                allow_private_network,
                compatibility_mode, raw_card_json, normalized_card_json,
                extended_card_json, protocol_version, selected_binding,
                selected_interface_url, credential_ref, credential_refs_json,
                selected_tenant, etag, last_modified,
                cache_expires_at, fetched_at, card_hash, signature_status,
                trust_status, trusted_origin, created_at, updated_at
             ) VALUES (
                ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?
             )
             ON CONFLICT(agent_id) DO UPDATE SET
                card_url = excluded.card_url,
                base_url = excluded.base_url,
                display_name = excluded.display_name,
                allow_insecure = excluded.allow_insecure,
                allow_private_network = excluded.allow_private_network,
                compatibility_mode = excluded.compatibility_mode,
                raw_card_json = excluded.raw_card_json,
                normalized_card_json = excluded.normalized_card_json,
                extended_card_json = excluded.extended_card_json,
                protocol_version = excluded.protocol_version,
                selected_binding = excluded.selected_binding,
                selected_interface_url = excluded.selected_interface_url,
                credential_ref = excluded.credential_ref,
                credential_refs_json = excluded.credential_refs_json,
                selected_tenant = excluded.selected_tenant,
                etag = excluded.etag,
                last_modified = excluded.last_modified,
                cache_expires_at = excluded.cache_expires_at,
                fetched_at = excluded.fetched_at,
                card_hash = excluded.card_hash,
                signature_status = excluded.signature_status,
                trust_status = excluded.trust_status,
                trusted_origin = excluded.trusted_origin,
                updated_at = excluded.updated_at",
        )
        .bind(params.agent_id)
        .bind(params.card_url)
        .bind(params.base_url)
        .bind(params.display_name)
        .bind(params.allow_insecure)
        .bind(params.allow_private_network)
        .bind(params.compatibility_mode)
        .bind(params.raw_card_json)
        .bind(params.normalized_card_json)
        .bind(params.extended_card_json)
        .bind(params.protocol_version)
        .bind(params.selected_binding)
        .bind(params.selected_interface_url)
        .bind(params.credential_ref)
        .bind(params.credential_refs_json)
        .bind(params.selected_tenant)
        .bind(params.etag)
        .bind(params.last_modified)
        .bind(params.cache_expires_at)
        .bind(params.fetched_at)
        .bind(params.card_hash)
        .bind(params.signature_status)
        .bind(params.trust_status)
        .bind(params.trusted_origin)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;

        self.find_profile(params.agent_id)
            .await?
            .ok_or_else(|| DbError::Init("A2A profile disappeared after upsert".to_owned()))
    }

    async fn delete_profile(&self, agent_id: &str) -> Result<(), DbError> {
        let result = sqlx::query("DELETE FROM a2a_agent_profiles WHERE agent_id = ?")
            .bind(agent_id)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(DbError::NotFound(format!("A2A agent profile '{agent_id}' not found")));
        }
        Ok(())
    }

    async fn find_credential(&self, id: &str) -> Result<Option<A2aCredentialRow>, DbError> {
        Ok(
            sqlx::query_as::<_, A2aCredentialRow>("SELECT * FROM a2a_credentials WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    async fn find_credentials(&self, ids: &[String]) -> Result<Vec<A2aCredentialRow>, DbError> {
        let mut rows = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(row) = self.find_credential(id).await? {
                rows.push(row);
            }
        }
        Ok(rows)
    }

    async fn upsert_credential(&self, params: UpsertA2aCredentialParams<'_>) -> Result<A2aCredentialRow, DbError> {
        let id = params
            .id
            .map(str::to_owned)
            .unwrap_or_else(|| tjuaeui_common::generate_prefixed_id("a2acred"));
        let now = tjuaeui_common::now_ms();
        sqlx::query(
            "INSERT INTO a2a_credentials (
                id, scheme_name, auth_kind, header_name, encrypted_secret, metadata_json,
                origin, created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                scheme_name = excluded.scheme_name,
                auth_kind = excluded.auth_kind,
                header_name = excluded.header_name,
                encrypted_secret = excluded.encrypted_secret,
                metadata_json = excluded.metadata_json,
                origin = excluded.origin,
                updated_at = excluded.updated_at",
        )
        .bind(&id)
        .bind(params.scheme_name)
        .bind(params.auth_kind)
        .bind(params.header_name)
        .bind(params.encrypted_secret)
        .bind(params.metadata_json)
        .bind(params.origin)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;

        self.find_credential(&id)
            .await?
            .ok_or_else(|| DbError::Init("A2A credential disappeared after upsert".to_owned()))
    }

    async fn delete_credential(&self, id: &str) -> Result<(), DbError> {
        let result = sqlx::query("DELETE FROM a2a_credentials WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(DbError::NotFound(format!("A2A credential '{id}' not found")));
        }
        Ok(())
    }

    async fn find_task_by_conversation(&self, conversation_id: &str) -> Result<Option<A2aTaskRow>, DbError> {
        Ok(sqlx::query_as::<_, A2aTaskRow>(
            "SELECT * FROM a2a_tasks WHERE conversation_id = ? ORDER BY updated_at DESC LIMIT 1",
        )
        .bind(conversation_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    async fn find_task(&self, id: &str) -> Result<Option<A2aTaskRow>, DbError> {
        Ok(sqlx::query_as::<_, A2aTaskRow>("SELECT * FROM a2a_tasks WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?)
    }

    async fn find_task_by_remote(&self, agent_id: &str, remote_task_id: &str) -> Result<Option<A2aTaskRow>, DbError> {
        Ok(sqlx::query_as::<_, A2aTaskRow>(
            "SELECT * FROM a2a_tasks
             WHERE agent_id = ? AND remote_task_id = ?
             ORDER BY updated_at DESC LIMIT 1",
        )
        .bind(agent_id)
        .bind(remote_task_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    async fn list_tasks_by_agent(&self, agent_id: &str) -> Result<Vec<A2aTaskRow>, DbError> {
        Ok(
            sqlx::query_as::<_, A2aTaskRow>("SELECT * FROM a2a_tasks WHERE agent_id = ? ORDER BY updated_at DESC")
                .bind(agent_id)
                .fetch_all(&self.pool)
                .await?,
        )
    }

    async fn upsert_task(&self, params: UpsertA2aTaskParams<'_>) -> Result<A2aTaskRow, DbError> {
        let id = params
            .id
            .map(str::to_owned)
            .unwrap_or_else(|| tjuaeui_common::generate_prefixed_id("a2atask"));
        let now = tjuaeui_common::now_ms();
        sqlx::query(
            "INSERT INTO a2a_tasks (
                id, conversation_id, agent_id, remote_task_id, context_id,
                state, interface_snapshot_json, last_event_id,
                artifact_snapshot_json, push_config_json, created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                remote_task_id = excluded.remote_task_id,
                context_id = excluded.context_id,
                state = excluded.state,
                interface_snapshot_json = excluded.interface_snapshot_json,
                last_event_id = excluded.last_event_id,
                artifact_snapshot_json = excluded.artifact_snapshot_json,
                push_config_json = excluded.push_config_json,
                updated_at = excluded.updated_at",
        )
        .bind(&id)
        .bind(params.conversation_id)
        .bind(params.agent_id)
        .bind(params.remote_task_id)
        .bind(params.context_id)
        .bind(params.state)
        .bind(params.interface_snapshot_json)
        .bind(params.last_event_id)
        .bind(params.artifact_snapshot_json)
        .bind(params.push_config_json)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;

        sqlx::query_as::<_, A2aTaskRow>("SELECT * FROM a2a_tasks WHERE id = ?")
            .bind(&id)
            .fetch_one(&self.pool)
            .await
            .map_err(Into::into)
    }

    async fn find_push_subscription(&self, id: &str) -> Result<Option<A2aPushSubscriptionRow>, DbError> {
        Ok(
            sqlx::query_as::<_, A2aPushSubscriptionRow>("SELECT * FROM a2a_push_subscriptions WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    async fn list_push_subscriptions(&self, agent_id: &str) -> Result<Vec<A2aPushSubscriptionRow>, DbError> {
        Ok(sqlx::query_as::<_, A2aPushSubscriptionRow>(
            "SELECT * FROM a2a_push_subscriptions
             WHERE agent_id = ?
             ORDER BY updated_at DESC",
        )
        .bind(agent_id)
        .fetch_all(&self.pool)
        .await?)
    }

    async fn upsert_push_subscription(
        &self,
        params: UpsertA2aPushSubscriptionParams<'_>,
    ) -> Result<A2aPushSubscriptionRow, DbError> {
        let now = tjuaeui_common::now_ms();
        sqlx::query(
            "INSERT INTO a2a_push_subscriptions (
                id, agent_id, task_id, config_id, callback_url,
                path_secret_hash, notification_token_hash, expires_at,
                revoked_at, created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                config_id = excluded.config_id,
                callback_url = excluded.callback_url,
                path_secret_hash = excluded.path_secret_hash,
                notification_token_hash = excluded.notification_token_hash,
                expires_at = excluded.expires_at,
                revoked_at = NULL,
                updated_at = excluded.updated_at",
        )
        .bind(params.id)
        .bind(params.agent_id)
        .bind(params.task_id)
        .bind(params.config_id)
        .bind(params.callback_url)
        .bind(params.path_secret_hash)
        .bind(params.notification_token_hash)
        .bind(params.expires_at)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        self.find_push_subscription(params.id)
            .await?
            .ok_or_else(|| DbError::Init("A2A push subscription disappeared after upsert".to_owned()))
    }

    async fn revoke_push_subscription(&self, id: &str, revoked_at: i64) -> Result<(), DbError> {
        let result = sqlx::query(
            "UPDATE a2a_push_subscriptions
             SET revoked_at = ?, updated_at = ?
             WHERE id = ? AND revoked_at IS NULL",
        )
        .bind(revoked_at)
        .bind(revoked_at)
        .bind(id)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(DbError::NotFound(format!(
                "A2A push subscription '{id}' not found or already revoked"
            )));
        }
        Ok(())
    }

    async fn record_push_delivery(
        &self,
        params: RecordA2aPushDeliveryParams<'_>,
        max_per_minute: i64,
    ) -> Result<RecordA2aPushDeliveryResult, DbError> {
        let mut tx = self.pool.begin().await?;
        let duplicate: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM a2a_push_deliveries
             WHERE subscription_id = ? AND event_key = ? LIMIT 1",
        )
        .bind(params.subscription_id)
        .bind(params.event_key)
        .fetch_optional(&mut *tx)
        .await?;
        if duplicate.is_some() {
            tx.rollback().await?;
            return Ok(RecordA2aPushDeliveryResult::Duplicate);
        }
        let cutoff = params.received_at.saturating_sub(60_000);
        let recent: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM a2a_push_deliveries
             WHERE subscription_id = ? AND received_at >= ?",
        )
        .bind(params.subscription_id)
        .bind(cutoff)
        .fetch_one(&mut *tx)
        .await?;
        if recent >= max_per_minute {
            tx.rollback().await?;
            return Ok(RecordA2aPushDeliveryResult::RateLimited);
        }
        let id = tjuaeui_common::generate_prefixed_id("a2apush");
        sqlx::query(
            "INSERT INTO a2a_push_deliveries (
                id, subscription_id, event_key, event_kind, task_id,
                payload_hash, received_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(params.subscription_id)
        .bind(params.event_key)
        .bind(params.event_kind)
        .bind(params.task_id)
        .bind(params.payload_hash)
        .bind(params.received_at)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(RecordA2aPushDeliveryResult::Accepted)
    }

    async fn delete_push_delivery(&self, subscription_id: &str, event_key: &str) -> Result<(), DbError> {
        sqlx::query(
            "DELETE FROM a2a_push_deliveries
             WHERE subscription_id = ? AND event_key = ?",
        )
        .bind(subscription_id)
        .bind(event_key)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn create_delegation_permission(
        &self,
        params: CreateA2aDelegationPermissionParams<'_>,
    ) -> Result<A2aDelegationPermissionRow, DbError> {
        let now = tjuaeui_common::now_ms();
        sqlx::query(
            "INSERT INTO a2a_delegation_permissions (
                id, parent_task_id, target_agent_ids_json, scopes_json,
                status, capability_token_hash, requested_expires_at,
                approved_at, revoked_at, created_at, updated_at
             ) VALUES (?, ?, ?, ?, 'pending', NULL, ?, NULL, NULL, ?, ?)",
        )
        .bind(params.id)
        .bind(params.parent_task_id)
        .bind(params.target_agent_ids_json)
        .bind(params.scopes_json)
        .bind(params.requested_expires_at)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        self.find_delegation_permission(params.id)
            .await?
            .ok_or_else(|| DbError::Init("A2A delegation permission disappeared after insert".to_owned()))
    }

    async fn find_delegation_permission(&self, id: &str) -> Result<Option<A2aDelegationPermissionRow>, DbError> {
        Ok(
            sqlx::query_as::<_, A2aDelegationPermissionRow>("SELECT * FROM a2a_delegation_permissions WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    async fn approve_delegation_permission(
        &self,
        id: &str,
        capability_token_hash: &str,
        approved_at: i64,
    ) -> Result<A2aDelegationPermissionRow, DbError> {
        let result = sqlx::query(
            "UPDATE a2a_delegation_permissions
             SET status = 'approved', capability_token_hash = ?,
                 approved_at = ?, updated_at = ?
             WHERE id = ? AND status = 'pending' AND requested_expires_at > ?",
        )
        .bind(capability_token_hash)
        .bind(approved_at)
        .bind(approved_at)
        .bind(id)
        .bind(approved_at)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() != 1 {
            return Err(DbError::NotFound(format!(
                "A2A delegation permission '{id}' is missing, expired, or already decided"
            )));
        }
        self.find_delegation_permission(id)
            .await?
            .ok_or_else(|| DbError::Init("A2A delegation permission disappeared after approval".to_owned()))
    }

    async fn revoke_delegation_permission(&self, id: &str, revoked_at: i64) -> Result<(), DbError> {
        let result = sqlx::query(
            "UPDATE a2a_delegation_permissions
             SET status = 'revoked', capability_token_hash = NULL,
                 revoked_at = ?, updated_at = ?
             WHERE id = ? AND status IN ('pending', 'approved')",
        )
        .bind(revoked_at)
        .bind(revoked_at)
        .bind(id)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() != 1 {
            return Err(DbError::NotFound(format!(
                "A2A delegation permission '{id}' is missing or inactive"
            )));
        }
        Ok(())
    }

    async fn create_delegation(&self, params: CreateA2aDelegationParams<'_>) -> Result<A2aDelegationRow, DbError> {
        let now = tjuaeui_common::now_ms();
        sqlx::query(
            "INSERT INTO a2a_delegations (
                id, parent_task_id, child_task_id, target_agent_id,
                permission_id, idempotency_key, state, context_id,
                last_error_code, created_at, updated_at
             ) VALUES (?, ?, NULL, ?, ?, ?, 'dispatching', NULL, NULL, ?, ?)",
        )
        .bind(params.id)
        .bind(params.parent_task_id)
        .bind(params.target_agent_id)
        .bind(params.permission_id)
        .bind(params.idempotency_key)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        self.find_delegation(params.id)
            .await?
            .ok_or_else(|| DbError::Init("A2A delegation disappeared after insert".to_owned()))
    }

    async fn find_delegation(&self, id: &str) -> Result<Option<A2aDelegationRow>, DbError> {
        Ok(
            sqlx::query_as::<_, A2aDelegationRow>("SELECT * FROM a2a_delegations WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    async fn find_delegation_by_idempotency(
        &self,
        parent_task_id: &str,
        target_agent_id: &str,
        idempotency_key: &str,
    ) -> Result<Option<A2aDelegationRow>, DbError> {
        Ok(sqlx::query_as::<_, A2aDelegationRow>(
            "SELECT * FROM a2a_delegations
             WHERE parent_task_id = ? AND target_agent_id = ? AND idempotency_key = ?",
        )
        .bind(parent_task_id)
        .bind(target_agent_id)
        .bind(idempotency_key)
        .fetch_optional(&self.pool)
        .await?)
    }

    async fn list_delegations_by_parent(&self, parent_task_id: &str) -> Result<Vec<A2aDelegationRow>, DbError> {
        Ok(sqlx::query_as::<_, A2aDelegationRow>(
            "SELECT * FROM a2a_delegations
             WHERE parent_task_id = ? ORDER BY created_at ASC",
        )
        .bind(parent_task_id)
        .fetch_all(&self.pool)
        .await?)
    }

    async fn update_delegation(&self, params: UpdateA2aDelegationParams<'_>) -> Result<A2aDelegationRow, DbError> {
        let now = tjuaeui_common::now_ms();
        let result = sqlx::query(
            "UPDATE a2a_delegations
             SET child_task_id = COALESCE(?, child_task_id),
                 state = ?, context_id = COALESCE(?, context_id),
                 last_error_code = ?, updated_at = ?
             WHERE id = ?",
        )
        .bind(params.child_task_id)
        .bind(params.state)
        .bind(params.context_id)
        .bind(params.last_error_code)
        .bind(now)
        .bind(params.id)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() != 1 {
            return Err(DbError::NotFound(format!("A2A delegation '{}' not found", params.id)));
        }
        self.find_delegation(params.id)
            .await?
            .ok_or_else(|| DbError::Init("A2A delegation disappeared after update".to_owned()))
    }

    async fn record_a2a_audit(&self, params: RecordA2aAuditParams<'_>) -> Result<A2aAuditEventRow, DbError> {
        let id = tjuaeui_common::generate_prefixed_id("a2aaudit");
        let now = tjuaeui_common::now_ms();
        sqlx::query(
            "INSERT INTO a2a_audit_events (
                id, event_type, actor_agent_id, target_agent_id,
                task_id, delegation_id, metadata_json, created_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(params.event_type)
        .bind(params.actor_agent_id)
        .bind(params.target_agent_id)
        .bind(params.task_id)
        .bind(params.delegation_id)
        .bind(params.metadata_json)
        .bind(now)
        .execute(&self.pool)
        .await?;
        sqlx::query_as::<_, A2aAuditEventRow>("SELECT * FROM a2a_audit_events WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .map_err(Into::into)
    }

    async fn list_a2a_audit_for_task(&self, task_id: &str) -> Result<Vec<A2aAuditEventRow>, DbError> {
        Ok(sqlx::query_as::<_, A2aAuditEventRow>(
            "SELECT * FROM a2a_audit_events
             WHERE task_id = ? ORDER BY created_at ASC",
        )
        .bind(task_id)
        .fetch_all(&self.pool)
        .await?)
    }
}
