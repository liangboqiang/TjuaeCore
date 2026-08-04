use serde::{Deserialize, Serialize};
use tjuaeui_common::TimestampMs;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum A2aCompatibilityMode {
    #[default]
    V1,
    V03,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum A2aBinding {
    JsonRpc,
    HttpJson,
    Grpc,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum A2aAuthKind {
    #[default]
    None,
    Bearer,
    ApiKey,
    Basic,
    CustomHeader,
    OAuth2,
    Oidc,
    Mtls,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum A2aCredentialLocation {
    #[default]
    Header,
    Query,
    Cookie,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2aCredentialInput {
    pub kind: A2aAuthKind,
    /// Name of the Agent Card security scheme this credential satisfies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheme_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<A2aCredentialLocation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverA2aAgentRequest {
    pub url: String,
    #[serde(default)]
    pub allow_insecure: bool,
    #[serde(default)]
    pub allow_private_network: bool,
    #[serde(default)]
    pub compatibility_mode: A2aCompatibilityMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<A2aCredentialInput>,
    /// A2A security requirements are OR-of-AND expressions, so a request may
    /// need more than one credential. `credential` remains as a wire-compatible
    /// alias for older clients.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credentials: Vec<A2aCredentialInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateA2aAgentRequest {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default)]
    pub allow_insecure: bool,
    #[serde(default)]
    pub allow_private_network: bool,
    #[serde(default)]
    pub compatibility_mode: A2aCompatibilityMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<A2aCredentialInput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credentials: Vec<A2aCredentialInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trusted_origin: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateA2aAgentRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_insecure: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_private_network: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatibility_mode: Option<A2aCompatibilityMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<Option<A2aCredentialInput>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials: Option<Vec<A2aCredentialInput>>,
    #[serde(default)]
    pub clear_credentials: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trusted_origin: Option<Option<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2aAgentSkillSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_modes: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_modes: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2aAgentCardSummary {
    pub name: String,
    pub description: String,
    pub agent_version: String,
    pub protocol_version: String,
    pub selected_binding: A2aBinding,
    pub selected_interface_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_tenant: Option<String>,
    #[serde(default)]
    pub supported_interfaces: Vec<A2aAgentInterfaceSummary>,
    #[serde(default)]
    pub supported_bindings: Vec<A2aBinding>,
    #[serde(default)]
    pub default_input_modes: Vec<String>,
    #[serde(default)]
    pub default_output_modes: Vec<String>,
    #[serde(default)]
    pub skills: Vec<A2aAgentSkillSummary>,
    #[serde(default)]
    pub capabilities: serde_json::Value,
    #[serde(default)]
    pub security_schemes: serde_json::Value,
    #[serde(default)]
    pub security_requirements: serde_json::Value,
    #[serde(default)]
    pub required_extensions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2aAgentInterfaceSummary {
    pub url: String,
    pub binding: A2aBinding,
    pub protocol_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverA2aAgentResponse {
    pub card_url: String,
    pub base_url: String,
    pub compatibility_mode: A2aCompatibilityMode,
    pub card: A2aAgentCardSummary,
    #[serde(default)]
    pub requires_authentication: bool,
    #[serde(default)]
    pub requires_origin_confirmation: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// Non-secret credential metadata returned to configuration UIs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2aConfiguredCredentialSummary {
    pub kind: A2aAuthKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheme_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<A2aCredentialLocation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2aAgentResponse {
    pub agent_id: String,
    pub card_url: String,
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub allow_insecure: bool,
    pub allow_private_network: bool,
    pub compatibility_mode: A2aCompatibilityMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<A2aAgentCardSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extended_card: Option<A2aAgentCardSummary>,
    #[serde(default)]
    pub has_extended_card: bool,
    #[serde(default)]
    pub has_credentials: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_kind: Option<A2aAuthKind>,
    #[serde(default)]
    pub credential_kinds: Vec<A2aAuthKind>,
    #[serde(default)]
    pub configured_security_schemes: Vec<String>,
    #[serde(default)]
    pub configured_credentials: Vec<A2aConfiguredCredentialSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_expires_at: Option<TimestampMs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<TimestampMs>,
    pub signature_status: String,
    pub trust_status: String,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum A2aOAuthFlowKind {
    AuthorizationCode,
    DeviceCode,
    ClientCredentials,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartA2aOAuthRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheme_name: Option<String>,
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<A2aOAuthFlowKind>,
    #[serde(default)]
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartA2aOAuthResponse {
    pub state: String,
    pub flow: A2aOAuthFlowKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_uri_complete: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_code: Option<String>,
    pub expires_at: TimestampMs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteA2aOAuthRequest {
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterA2aPushRequest {
    pub task_id: String,
    pub callback_base_url: String,
    #[serde(default = "default_push_expiry_seconds")]
    pub expires_in_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2aPushSubscriptionResponse {
    pub id: String,
    pub agent_id: String,
    pub task_id: String,
    pub config_id: String,
    pub callback_url: String,
    pub expires_at: TimestampMs,
    pub revoked: bool,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
}

const fn default_push_expiry_seconds() -> u64 {
    24 * 60 * 60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestA2aDelegationPermission {
    pub parent_task_id: String,
    pub target_agent_ids: Vec<String>,
    pub scopes: Vec<String>,
    #[serde(default = "default_delegation_expiry_seconds")]
    pub expires_in_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2aDelegationPermissionResponse {
    pub id: String,
    pub parent_task_id: String,
    pub target_agent_ids: Vec<String>,
    pub scopes: Vec<String>,
    pub status: String,
    pub expires_at: TimestampMs,
    pub approved_at: Option<TimestampMs>,
    pub revoked_at: Option<TimestampMs>,
    /// Returned exactly once when a pending permission is approved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability_token: Option<String>,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegateA2aTaskRequest {
    pub parent_task_id: String,
    pub target_agent_id: String,
    pub permission_id: String,
    pub capability_token: String,
    pub message: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2aDelegationResponse {
    pub id: String,
    pub parent_task_id: String,
    pub child_task_id: Option<String>,
    pub target_agent_id: String,
    pub permission_id: String,
    pub state: String,
    pub context_id: Option<String>,
    pub last_error_code: Option<String>,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2aDelegationTaskNode {
    pub id: String,
    pub agent_id: String,
    pub remote_task_id: Option<String>,
    pub context_id: Option<String>,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2aAuditEventResponse {
    pub id: String,
    pub event_type: String,
    pub actor_agent_id: Option<String>,
    pub target_agent_id: Option<String>,
    pub task_id: Option<String>,
    pub delegation_id: Option<String>,
    pub metadata: serde_json::Value,
    pub created_at: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2aDelegationGraphResponse {
    pub root_task_id: String,
    pub tasks: Vec<A2aDelegationTaskNode>,
    pub delegations: Vec<A2aDelegationResponse>,
    pub audit: Vec<A2aAuditEventResponse>,
}

const fn default_delegation_expiry_seconds() -> u64 {
    60 * 60
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_secret_is_input_only_by_response_shape() {
        let response = A2aAgentResponse {
            agent_id: "agent".to_owned(),
            card_url: "https://agent.example/.well-known/agent-card.json".to_owned(),
            base_url: "https://agent.example".to_owned(),
            display_name: None,
            allow_insecure: false,
            allow_private_network: false,
            compatibility_mode: A2aCompatibilityMode::V1,
            card: None,
            extended_card: None,
            has_extended_card: false,
            has_credentials: true,
            credential_kind: Some(A2aAuthKind::Bearer),
            credential_kinds: vec![A2aAuthKind::Bearer],
            configured_security_schemes: Vec::new(),
            configured_credentials: vec![A2aConfiguredCredentialSummary {
                kind: A2aAuthKind::Bearer,
                scheme_name: None,
                header_name: None,
                location: None,
            }],
            etag: None,
            last_modified: None,
            cache_expires_at: None,
            fetched_at: None,
            signature_status: "unchecked".to_owned(),
            trust_status: "untrusted".to_owned(),
            created_at: 1,
            updated_at: 1,
        };

        let json = serde_json::to_value(response).expect("serialize response");

        assert!(json.get("secret").is_none());
        assert!(json["configured_credentials"][0].get("secret").is_none());
    }
}
