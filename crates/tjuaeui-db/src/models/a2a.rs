use serde::{Deserialize, Serialize};
use tjuaeui_common::TimestampMs;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct A2aAgentProfileRow {
    pub agent_id: String,
    pub card_url: String,
    pub base_url: String,
    pub display_name: Option<String>,
    pub allow_insecure: bool,
    pub allow_private_network: bool,
    pub compatibility_mode: String,
    pub raw_card_json: Option<String>,
    pub normalized_card_json: Option<String>,
    pub extended_card_json: Option<String>,
    pub protocol_version: Option<String>,
    pub selected_binding: Option<String>,
    pub selected_interface_url: Option<String>,
    pub credential_ref: Option<String>,
    pub credential_refs_json: String,
    pub selected_tenant: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub cache_expires_at: Option<TimestampMs>,
    pub fetched_at: Option<TimestampMs>,
    pub card_hash: Option<String>,
    pub signature_status: String,
    pub trust_status: String,
    pub trusted_origin: Option<String>,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct A2aCredentialRow {
    pub id: String,
    pub scheme_name: Option<String>,
    pub auth_kind: String,
    pub header_name: Option<String>,
    pub encrypted_secret: Option<String>,
    pub metadata_json: Option<String>,
    pub origin: String,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct A2aTaskRow {
    pub id: String,
    pub conversation_id: String,
    pub agent_id: String,
    pub remote_task_id: Option<String>,
    pub context_id: Option<String>,
    pub state: String,
    pub interface_snapshot_json: String,
    pub last_event_id: Option<String>,
    pub artifact_snapshot_json: Option<String>,
    pub push_config_json: Option<String>,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct A2aPushSubscriptionRow {
    pub id: String,
    pub agent_id: String,
    pub task_id: String,
    pub config_id: String,
    pub callback_url: String,
    pub path_secret_hash: String,
    pub notification_token_hash: String,
    pub expires_at: TimestampMs,
    pub revoked_at: Option<TimestampMs>,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct A2aDelegationPermissionRow {
    pub id: String,
    pub parent_task_id: String,
    pub target_agent_ids_json: String,
    pub scopes_json: String,
    pub status: String,
    pub capability_token_hash: Option<String>,
    pub requested_expires_at: TimestampMs,
    pub approved_at: Option<TimestampMs>,
    pub revoked_at: Option<TimestampMs>,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct A2aDelegationRow {
    pub id: String,
    pub parent_task_id: String,
    pub child_task_id: Option<String>,
    pub target_agent_id: String,
    pub permission_id: String,
    pub idempotency_key: String,
    pub state: String,
    pub context_id: Option<String>,
    pub last_error_code: Option<String>,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct A2aAuditEventRow {
    pub id: String,
    pub event_type: String,
    pub actor_agent_id: Option<String>,
    pub target_agent_id: Option<String>,
    pub task_id: Option<String>,
    pub delegation_id: Option<String>,
    pub metadata_json: String,
    pub created_at: TimestampMs,
}
