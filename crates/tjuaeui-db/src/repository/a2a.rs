use crate::error::DbError;
use crate::models::{
    A2aAgentProfileRow, A2aAuditEventRow, A2aCredentialRow, A2aDelegationPermissionRow, A2aDelegationRow,
    A2aPushSubscriptionRow, A2aTaskRow,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordA2aPushDeliveryResult {
    Accepted,
    Duplicate,
    RateLimited,
}

#[async_trait::async_trait]
pub trait IA2aRepository: Send + Sync {
    async fn list_profiles(&self) -> Result<Vec<A2aAgentProfileRow>, DbError>;
    async fn find_profile(&self, agent_id: &str) -> Result<Option<A2aAgentProfileRow>, DbError>;
    async fn upsert_profile(&self, params: UpsertA2aAgentProfileParams<'_>) -> Result<A2aAgentProfileRow, DbError>;
    async fn delete_profile(&self, agent_id: &str) -> Result<(), DbError>;

    async fn find_credential(&self, id: &str) -> Result<Option<A2aCredentialRow>, DbError>;
    async fn find_credentials(&self, ids: &[String]) -> Result<Vec<A2aCredentialRow>, DbError>;
    async fn upsert_credential(&self, params: UpsertA2aCredentialParams<'_>) -> Result<A2aCredentialRow, DbError>;
    async fn delete_credential(&self, id: &str) -> Result<(), DbError>;

    async fn find_task_by_conversation(&self, conversation_id: &str) -> Result<Option<A2aTaskRow>, DbError>;
    async fn find_task(&self, id: &str) -> Result<Option<A2aTaskRow>, DbError>;
    async fn find_task_by_remote(&self, agent_id: &str, remote_task_id: &str) -> Result<Option<A2aTaskRow>, DbError>;
    async fn list_tasks_by_agent(&self, agent_id: &str) -> Result<Vec<A2aTaskRow>, DbError>;
    async fn upsert_task(&self, params: UpsertA2aTaskParams<'_>) -> Result<A2aTaskRow, DbError>;

    async fn find_push_subscription(&self, id: &str) -> Result<Option<A2aPushSubscriptionRow>, DbError>;
    async fn list_push_subscriptions(&self, agent_id: &str) -> Result<Vec<A2aPushSubscriptionRow>, DbError>;
    async fn upsert_push_subscription(
        &self,
        params: UpsertA2aPushSubscriptionParams<'_>,
    ) -> Result<A2aPushSubscriptionRow, DbError>;
    async fn revoke_push_subscription(&self, id: &str, revoked_at: i64) -> Result<(), DbError>;
    async fn record_push_delivery(
        &self,
        params: RecordA2aPushDeliveryParams<'_>,
        max_per_minute: i64,
    ) -> Result<RecordA2aPushDeliveryResult, DbError>;
    async fn delete_push_delivery(&self, subscription_id: &str, event_key: &str) -> Result<(), DbError>;

    async fn create_delegation_permission(
        &self,
        params: CreateA2aDelegationPermissionParams<'_>,
    ) -> Result<A2aDelegationPermissionRow, DbError>;
    async fn find_delegation_permission(&self, id: &str) -> Result<Option<A2aDelegationPermissionRow>, DbError>;
    async fn approve_delegation_permission(
        &self,
        id: &str,
        capability_token_hash: &str,
        approved_at: i64,
    ) -> Result<A2aDelegationPermissionRow, DbError>;
    async fn revoke_delegation_permission(&self, id: &str, revoked_at: i64) -> Result<(), DbError>;
    async fn create_delegation(&self, params: CreateA2aDelegationParams<'_>) -> Result<A2aDelegationRow, DbError>;
    async fn find_delegation(&self, id: &str) -> Result<Option<A2aDelegationRow>, DbError>;
    async fn find_delegation_by_idempotency(
        &self,
        parent_task_id: &str,
        target_agent_id: &str,
        idempotency_key: &str,
    ) -> Result<Option<A2aDelegationRow>, DbError>;
    async fn list_delegations_by_parent(&self, parent_task_id: &str) -> Result<Vec<A2aDelegationRow>, DbError>;
    async fn update_delegation(&self, params: UpdateA2aDelegationParams<'_>) -> Result<A2aDelegationRow, DbError>;
    async fn record_a2a_audit(&self, params: RecordA2aAuditParams<'_>) -> Result<A2aAuditEventRow, DbError>;
    async fn list_a2a_audit_for_task(&self, task_id: &str) -> Result<Vec<A2aAuditEventRow>, DbError>;
}

#[derive(Debug)]
pub struct UpsertA2aAgentProfileParams<'a> {
    pub agent_id: &'a str,
    pub card_url: &'a str,
    pub base_url: &'a str,
    pub display_name: Option<&'a str>,
    pub allow_insecure: bool,
    pub allow_private_network: bool,
    pub compatibility_mode: &'a str,
    pub raw_card_json: Option<&'a str>,
    pub normalized_card_json: Option<&'a str>,
    pub extended_card_json: Option<&'a str>,
    pub protocol_version: Option<&'a str>,
    pub selected_binding: Option<&'a str>,
    pub selected_interface_url: Option<&'a str>,
    pub credential_ref: Option<&'a str>,
    pub credential_refs_json: &'a str,
    pub selected_tenant: Option<&'a str>,
    pub etag: Option<&'a str>,
    pub last_modified: Option<&'a str>,
    pub cache_expires_at: Option<i64>,
    pub fetched_at: Option<i64>,
    pub card_hash: Option<&'a str>,
    pub signature_status: &'a str,
    pub trust_status: &'a str,
    pub trusted_origin: Option<&'a str>,
}

#[derive(Debug)]
pub struct UpsertA2aCredentialParams<'a> {
    pub id: Option<&'a str>,
    pub scheme_name: Option<&'a str>,
    pub auth_kind: &'a str,
    pub header_name: Option<&'a str>,
    pub encrypted_secret: Option<&'a str>,
    pub metadata_json: Option<&'a str>,
    pub origin: &'a str,
}

#[derive(Debug)]
pub struct UpsertA2aTaskParams<'a> {
    pub id: Option<&'a str>,
    pub conversation_id: &'a str,
    pub agent_id: &'a str,
    pub remote_task_id: Option<&'a str>,
    pub context_id: Option<&'a str>,
    pub state: &'a str,
    pub interface_snapshot_json: &'a str,
    pub last_event_id: Option<&'a str>,
    pub artifact_snapshot_json: Option<&'a str>,
    pub push_config_json: Option<&'a str>,
}

#[derive(Debug)]
pub struct UpsertA2aPushSubscriptionParams<'a> {
    pub id: &'a str,
    pub agent_id: &'a str,
    pub task_id: &'a str,
    pub config_id: &'a str,
    pub callback_url: &'a str,
    pub path_secret_hash: &'a str,
    pub notification_token_hash: &'a str,
    pub expires_at: i64,
}

#[derive(Debug)]
pub struct RecordA2aPushDeliveryParams<'a> {
    pub subscription_id: &'a str,
    pub event_key: &'a str,
    pub event_kind: &'a str,
    pub task_id: &'a str,
    pub payload_hash: &'a str,
    pub received_at: i64,
}

#[derive(Debug)]
pub struct CreateA2aDelegationPermissionParams<'a> {
    pub id: &'a str,
    pub parent_task_id: &'a str,
    pub target_agent_ids_json: &'a str,
    pub scopes_json: &'a str,
    pub requested_expires_at: i64,
}

#[derive(Debug)]
pub struct CreateA2aDelegationParams<'a> {
    pub id: &'a str,
    pub parent_task_id: &'a str,
    pub target_agent_id: &'a str,
    pub permission_id: &'a str,
    pub idempotency_key: &'a str,
}

#[derive(Debug)]
pub struct UpdateA2aDelegationParams<'a> {
    pub id: &'a str,
    pub child_task_id: Option<&'a str>,
    pub state: &'a str,
    pub context_id: Option<&'a str>,
    pub last_error_code: Option<&'a str>,
}

#[derive(Debug)]
pub struct RecordA2aAuditParams<'a> {
    pub event_type: &'a str,
    pub actor_agent_id: Option<&'a str>,
    pub target_agent_id: Option<&'a str>,
    pub task_id: Option<&'a str>,
    pub delegation_id: Option<&'a str>,
    pub metadata_json: &'a str,
}
