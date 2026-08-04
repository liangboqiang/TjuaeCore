use std::sync::Arc;
use std::time::Instant;

use tjuaeui_api_types::{
    A2aAgentResponse, A2aAuthKind, A2aBinding, A2aCompatibilityMode, A2aConfiguredCredentialSummary,
    A2aCredentialInput, A2aCredentialLocation, AgentSnapshotCheckKind, CompleteA2aOAuthRequest, CreateA2aAgentRequest,
    DiscoverA2aAgentRequest, DiscoverA2aAgentResponse, StartA2aOAuthRequest, StartA2aOAuthResponse,
    UpdateA2aAgentRequest,
};
use tjuaeui_common::{decrypt_string, encrypt_string, generate_short_id, now_ms};
use tjuaeui_db::{
    A2aAgentProfileRow, A2aCredentialRow, IA2aRepository, UpdateAgentAvailabilitySnapshotParams,
    UpsertA2aAgentProfileParams, UpsertA2aCredentialParams, UpsertAgentMetadataParams,
};
use tjuaeui_realtime::EventBroadcaster;
use url::Url;

use crate::error::AgentError;
use crate::manager::a2a::{A2aClient, A2aClientConfig, GrpcA2aClient, IA2aClient};
use crate::protocol::a2a::card::{CardParseOptions, MAX_AGENT_CARD_BYTES, parse_agent_card};
use crate::registry::AgentRegistry;

use super::discovery::{A2aCardDiscovery, CardCacheValidators, DiscoveredA2aCard};
use super::mapper::{binding_db_name, card_summary};
use super::oauth::{A2aOAuthCoordinator, resolve_oauth_secret};
use super::security::mtls_material;
use super::signature::evaluate_agent_card_signatures;

const A2A_SORT_ORDER_DEFAULT: i64 = 1600;

#[derive(Clone)]
pub struct A2aAgentService {
    pub(super) repo: Arc<dyn IA2aRepository>,
    pub(super) registry: Arc<AgentRegistry>,
    discovery: A2aCardDiscovery,
    oauth: A2aOAuthCoordinator,
    pub(super) encryption_key: [u8; 32],
    pub(super) broadcaster: Option<Arc<dyn EventBroadcaster>>,
}

impl A2aAgentService {
    pub fn new(repo: Arc<dyn IA2aRepository>, registry: Arc<AgentRegistry>, encryption_key: [u8; 32]) -> Self {
        Self {
            repo,
            registry,
            discovery: A2aCardDiscovery,
            oauth: A2aOAuthCoordinator::default(),
            encryption_key,
            broadcaster: None,
        }
    }

    pub fn with_broadcaster(mut self, broadcaster: Arc<dyn EventBroadcaster>) -> Self {
        self.broadcaster = Some(broadcaster);
        self
    }

    pub async fn discover(&self, request: DiscoverA2aAgentRequest) -> Result<DiscoverA2aAgentResponse, AgentError> {
        Ok(self.discovery.discover(&request, None).await?.response)
    }

    pub async fn list(&self) -> Result<Vec<A2aAgentResponse>, AgentError> {
        let rows = self.repo.list_profiles().await.map_err(db_error)?;
        let mut responses = Vec::with_capacity(rows.len());
        for row in rows {
            responses.push(self.row_to_response(row).await?);
        }
        Ok(responses)
    }

    pub async fn get(&self, agent_id: &str) -> Result<A2aAgentResponse, AgentError> {
        let row = self
            .repo
            .find_profile(agent_id)
            .await
            .map_err(db_error)?
            .ok_or_else(|| AgentError::not_found(format!("找不到 A2A Agent“{agent_id}”")))?;
        self.row_to_response(row).await
    }

    pub async fn start_oauth(
        &self,
        agent_id: &str,
        request: StartA2aOAuthRequest,
    ) -> Result<StartA2aOAuthResponse, AgentError> {
        let profile = self
            .repo
            .find_profile(agent_id)
            .await
            .map_err(db_error)?
            .ok_or_else(|| AgentError::not_found(format!("找不到 A2A Agent“{agent_id}”")))?;
        let card: a2a::AgentCard = profile
            .normalized_card_json
            .as_deref()
            .ok_or_else(|| AgentError::conflict("A2A Agent 尚无 Card 缓存"))
            .and_then(|value| serde_json::from_str(value).map_err(|_| AgentError::internal("A2A Card 缓存损坏")))?;
        self.oauth
            .start(
                agent_id,
                &card,
                request,
                super::security::A2aNetworkPolicy {
                    allow_insecure: profile.allow_insecure,
                    allow_private_network: profile.allow_private_network,
                },
            )
            .await
    }

    pub async fn complete_oauth(
        &self,
        agent_id: &str,
        request: CompleteA2aOAuthRequest,
    ) -> Result<A2aAgentResponse, AgentError> {
        let profile = self
            .repo
            .find_profile(agent_id)
            .await
            .map_err(db_error)?
            .ok_or_else(|| AgentError::not_found(format!("找不到 A2A Agent“{agent_id}”")))?;
        let credential = self.oauth.complete(agent_id, request).await?;
        let endpoint = profile
            .selected_interface_url
            .as_deref()
            .ok_or_else(|| AgentError::conflict("A2A Agent 尚无可用接口缓存"))?;
        let origin = origin_of(endpoint)?;
        let encrypted_secret = credential
            .secret
            .as_deref()
            .map(|secret| {
                encrypt_string(secret, &self.encryption_key).map_err(|error| AgentError::internal(error.to_string()))
            })
            .transpose()?;
        let metadata_json = credential
            .metadata
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| AgentError::bad_request(format!("OAuth metadata 无效：{error}")))?;
        let mut ids = credential_ids(&profile)?;
        let existing_rows = self.repo.find_credentials(&ids).await.map_err(db_error)?;
        let existing_id = credential.scheme_name.as_deref().and_then(|scheme_name| {
            existing_rows
                .iter()
                .find(|row| row.scheme_name.as_deref() == Some(scheme_name))
                .map(|row| row.id.as_str())
        });
        let saved = self
            .repo
            .upsert_credential(UpsertA2aCredentialParams {
                id: existing_id,
                scheme_name: credential.scheme_name.as_deref(),
                auth_kind: auth_kind_db_name(credential.kind),
                header_name: None,
                encrypted_secret: encrypted_secret.as_deref(),
                metadata_json: metadata_json.as_deref(),
                origin: &origin,
            })
            .await
            .map_err(db_error)?;
        if !ids.contains(&saved.id) {
            ids.push(saved.id);
        }
        self.replace_profile_credentials(&profile, &ids).await?;
        self.refresh_with_kind_internal(agent_id, AgentSnapshotCheckKind::Manual, false)
            .await
    }

    pub async fn create(&self, request: CreateA2aAgentRequest) -> Result<A2aAgentResponse, AgentError> {
        let credentials = effective_credentials(request.credential.as_ref(), &request.credentials);
        let discovery_request = DiscoverA2aAgentRequest {
            url: request.url,
            allow_insecure: request.allow_insecure,
            allow_private_network: request.allow_private_network,
            compatibility_mode: request.compatibility_mode,
            credential: None,
            credentials: credentials.clone(),
        };
        let mut discovered = self.discovery.discover(&discovery_request, None).await?;
        let trusted_origin = validate_selected_origin(&discovered, request.trusted_origin.as_deref())?;
        let credentials = normalize_and_validate_credentials(credentials, &discovered.normalized_card_json)?;
        self.attach_extended_card(
            &mut discovered,
            credentials.clone(),
            discovery_request.allow_insecure,
            discovery_request.allow_private_network,
        )
        .await?;
        let agent_id = generate_short_id();
        let saved_credentials = self.persist_credentials(&credentials, &discovered).await?;
        let credential_ids = saved_credentials.iter().map(|row| row.id.clone()).collect::<Vec<_>>();
        let display_name = request
            .display_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(&discovered.response.card.name);

        if let Err(error) = self.upsert_metadata(&agent_id, display_name, true, &discovered).await {
            self.cleanup_credentials(&saved_credentials).await;
            return Err(error);
        }
        if let Err(error) = self
            .persist_profile(
                &agent_id,
                Some(display_name),
                discovery_request.allow_insecure,
                discovery_request.allow_private_network,
                &credential_ids,
                trusted_origin.as_deref(),
                &discovered,
                evaluate_agent_card_signatures(&discovered.raw_card_json)?,
            )
            .await
        {
            let _ = self.registry.repo_handle().delete(&agent_id).await;
            self.cleanup_credentials(&saved_credentials).await;
            return Err(error);
        }
        self.record_online(&agent_id, AgentSnapshotCheckKind::Manual, 0).await?;
        self.registry
            .reload_one(&agent_id)
            .await
            .map_err(|error| AgentError::internal(format!("重新加载 A2A Agent 失败：{error}")))?;
        self.get(&agent_id).await
    }

    pub async fn update(&self, agent_id: &str, request: UpdateA2aAgentRequest) -> Result<A2aAgentResponse, AgentError> {
        let existing = self
            .repo
            .find_profile(agent_id)
            .await
            .map_err(db_error)?
            .ok_or_else(|| AgentError::not_found(format!("找不到 A2A Agent“{agent_id}”")))?;
        let metadata = self
            .registry
            .repo_handle()
            .get(agent_id)
            .await
            .map_err(db_error)?
            .ok_or_else(|| AgentError::not_found(format!("找不到 Agent 元数据“{agent_id}”")))?;

        let compatibility_mode = request
            .compatibility_mode
            .unwrap_or_else(|| compatibility_from_db(&existing.compatibility_mode));
        let allow_insecure = request.allow_insecure.unwrap_or(existing.allow_insecure);
        let allow_private_network = request.allow_private_network.unwrap_or(existing.allow_private_network);
        let url = request.url.clone().unwrap_or_else(|| existing.card_url.clone());
        let existing_credentials = if request.clear_credentials {
            Vec::new()
        } else {
            self.load_credentials_for_url(&existing, &url).await?
        };
        let requested_credentials = match request.credentials.as_ref() {
            Some(values) => values.clone(),
            None => match request.credential.as_ref() {
                Some(Some(value)) => vec![value.clone()],
                Some(None) => Vec::new(),
                None if request.clear_credentials => Vec::new(),
                None => existing_credentials.clone(),
            },
        };
        let effective_credentials = if request.credential.is_some() || request.credentials.is_some() {
            merge_stored_credentials(requested_credentials, &existing_credentials)
        } else {
            requested_credentials
        };
        let discovery_request = DiscoverA2aAgentRequest {
            url,
            allow_insecure,
            allow_private_network,
            compatibility_mode,
            credential: None,
            credentials: effective_credentials.clone(),
        };
        let mut discovered = self.discovery.discover(&discovery_request, None).await?;
        let effective_credentials =
            normalize_and_validate_credentials(effective_credentials, &discovered.normalized_card_json)?;
        let requested_trusted_origin = request
            .trusted_origin
            .as_ref()
            .and_then(|value| value.as_deref())
            .or(existing.trusted_origin.as_deref());
        let trusted_origin = validate_selected_origin(&discovered, requested_trusted_origin)?;
        self.attach_extended_card(
            &mut discovered,
            effective_credentials.clone(),
            allow_insecure,
            allow_private_network,
        )
        .await?;
        let display_name = match request.display_name {
            Some(value) => value,
            None => existing.display_name.clone(),
        };
        let effective_name = display_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(&discovered.response.card.name);

        let replace_credentials =
            request.credential.is_some() || request.credentials.is_some() || request.clear_credentials;
        let old_credential_ids = credential_ids(&existing)?;
        let new_credentials = if replace_credentials {
            self.persist_credentials(&effective_credentials, &discovered).await?
        } else {
            self.repo
                .find_credentials(&old_credential_ids)
                .await
                .map_err(db_error)?
        };
        let new_credential_ids = new_credentials.iter().map(|row| row.id.clone()).collect::<Vec<_>>();

        self.upsert_metadata(agent_id, effective_name, metadata.enabled, &discovered)
            .await?;
        let signature_status = evaluate_agent_card_signatures(&discovered.raw_card_json)?;
        self.persist_profile(
            agent_id,
            display_name.as_deref(),
            allow_insecure,
            allow_private_network,
            &new_credential_ids,
            trusted_origin.as_deref(),
            &discovered,
            signature_status,
        )
        .await?;
        if replace_credentials {
            for old_id in old_credential_ids {
                if !new_credential_ids.contains(&old_id) {
                    let _ = self.repo.delete_credential(&old_id).await;
                }
            }
        }
        self.record_online(agent_id, AgentSnapshotCheckKind::Manual, 0).await?;
        self.registry
            .reload_one(agent_id)
            .await
            .map_err(|error| AgentError::internal(format!("重新加载 A2A Agent 失败：{error}")))?;
        self.get(agent_id).await
    }

    pub async fn refresh(&self, agent_id: &str) -> Result<A2aAgentResponse, AgentError> {
        self.refresh_with_kind(agent_id, AgentSnapshotCheckKind::Manual).await
    }

    pub async fn refresh_with_kind(
        &self,
        agent_id: &str,
        kind: AgentSnapshotCheckKind,
    ) -> Result<A2aAgentResponse, AgentError> {
        self.refresh_with_kind_internal(agent_id, kind, true).await
    }

    async fn refresh_with_kind_internal(
        &self,
        agent_id: &str,
        kind: AgentSnapshotCheckKind,
        use_validators: bool,
    ) -> Result<A2aAgentResponse, AgentError> {
        let started = Instant::now();
        let existing = self
            .repo
            .find_profile(agent_id)
            .await
            .map_err(db_error)?
            .ok_or_else(|| AgentError::not_found(format!("找不到 A2A Agent“{agent_id}”")))?;
        let card_credentials = self.load_credentials_for_url(&existing, &existing.card_url).await?;
        let request = DiscoverA2aAgentRequest {
            url: existing.card_url.clone(),
            allow_insecure: existing.allow_insecure,
            allow_private_network: existing.allow_private_network,
            compatibility_mode: compatibility_from_db(&existing.compatibility_mode),
            credential: None,
            credentials: card_credentials,
        };
        let validators = CardCacheValidators {
            etag: existing.etag.clone(),
            last_modified: existing.last_modified.clone(),
        };
        let mut discovered = match self
            .discovery
            .discover(&request, use_validators.then_some(&validators))
            .await
        {
            Ok(discovered) => discovered,
            Err(AgentError::Conflict(_)) => {
                self.record_online(agent_id, kind, started.elapsed().as_millis() as i64)
                    .await?;
                self.registry
                    .reload_one(agent_id)
                    .await
                    .map_err(|error| AgentError::internal(format!("重新加载 A2A Agent 失败：{error}")))?;
                return self.get(agent_id).await;
            }
            Err(error) => {
                self.record_offline(agent_id, kind, started.elapsed().as_millis() as i64, &error)
                    .await;
                let _ = self.registry.reload_one(agent_id).await;
                return Err(error);
            }
        };
        let trusted_origin = validate_selected_origin(&discovered, existing.trusted_origin.as_deref())?;
        let interface_credentials = self
            .load_credentials_for_url(&existing, &discovered.response.card.selected_interface_url)
            .await?;
        self.attach_extended_card(
            &mut discovered,
            interface_credentials,
            existing.allow_insecure,
            existing.allow_private_network,
        )
        .await?;
        let display_name = existing
            .display_name
            .as_deref()
            .unwrap_or(&discovered.response.card.name);
        let metadata = self
            .registry
            .repo_handle()
            .get(agent_id)
            .await
            .map_err(db_error)?
            .ok_or_else(|| AgentError::not_found(format!("找不到 Agent 元数据“{agent_id}”")))?;
        self.upsert_metadata(agent_id, display_name, metadata.enabled, &discovered)
            .await?;
        let signature_status = evaluate_agent_card_signatures(&discovered.raw_card_json)?;
        let existing_credential_ids = credential_ids(&existing)?;
        self.persist_profile(
            agent_id,
            existing.display_name.as_deref(),
            existing.allow_insecure,
            existing.allow_private_network,
            &existing_credential_ids,
            trusted_origin.as_deref(),
            &discovered,
            signature_status,
        )
        .await?;
        self.record_online(agent_id, kind, started.elapsed().as_millis() as i64)
            .await?;
        self.registry
            .reload_one(agent_id)
            .await
            .map_err(|error| AgentError::internal(format!("重新加载 A2A Agent 失败：{error}")))?;
        self.get(agent_id).await
    }

    pub async fn delete(&self, agent_id: &str) -> Result<(), AgentError> {
        let existing = self
            .repo
            .find_profile(agent_id)
            .await
            .map_err(db_error)?
            .ok_or_else(|| AgentError::not_found(format!("找不到 A2A Agent“{agent_id}”")))?;
        for subscription in self
            .repo
            .list_push_subscriptions(agent_id)
            .await
            .map_err(db_error)?
            .into_iter()
            .filter(|subscription| subscription.revoked_at.is_none())
        {
            if let Err(error) = self.revoke_push(agent_id, &subscription.id).await {
                tracing::warn!(
                    agent_id,
                    subscription_id = %subscription.id,
                    error = %error,
                    "failed to revoke remote A2A push config during agent deletion"
                );
            }
        }
        self.repo.delete_profile(agent_id).await.map_err(db_error)?;
        self.registry.repo_handle().delete(agent_id).await.map_err(db_error)?;
        for credential_ref in credential_ids(&existing)? {
            let _ = self.repo.delete_credential(&credential_ref).await;
        }
        let _ = self.registry.reload_one(agent_id).await;
        Ok(())
    }

    async fn upsert_metadata(
        &self,
        agent_id: &str,
        name: &str,
        enabled: bool,
        discovered: &DiscoveredA2aCard,
    ) -> Result<(), AgentError> {
        let source_info = serde_json::json!({
            "version": discovered.response.card.agent_version,
        })
        .to_string();
        let capabilities = discovered.response.card.capabilities.to_string();
        let auth_methods = discovered.response.card.security_schemes.to_string();
        self.registry
            .repo_handle()
            .upsert(&UpsertAgentMetadataParams {
                id: agent_id,
                icon: None,
                name,
                name_i18n: None,
                description: Some(&discovered.response.card.description),
                description_i18n: None,
                backend: Some("a2a"),
                agent_type: "a2a",
                agent_source: "custom",
                agent_source_info: Some(&source_info),
                enabled,
                command: None,
                args: None,
                env: None,
                native_skills_dirs: None,
                behavior_policy: None,
                yolo_id: None,
                agent_capabilities: Some(&capabilities),
                auth_methods: Some(&auth_methods),
                config_options: None,
                available_modes: None,
                available_models: None,
                available_commands: None,
                sort_order: A2A_SORT_ORDER_DEFAULT,
            })
            .await
            .map_err(db_error)?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn persist_profile(
        &self,
        agent_id: &str,
        display_name: Option<&str>,
        allow_insecure: bool,
        allow_private_network: bool,
        credential_ids: &[String],
        trusted_origin: Option<&str>,
        discovered: &DiscoveredA2aCard,
        signature_status: &str,
    ) -> Result<(), AgentError> {
        let response = &discovered.response;
        let derived_trust_status = if response.requires_origin_confirmation {
            if trusted_origin.is_some() {
                "trusted"
            } else {
                "untrusted"
            }
        } else {
            "origin_verified"
        };
        let credential_refs_json =
            serde_json::to_string(credential_ids).map_err(|_| AgentError::internal("无法编码 A2A 凭据引用"))?;
        self.repo
            .upsert_profile(UpsertA2aAgentProfileParams {
                agent_id,
                card_url: &response.card_url,
                base_url: &response.base_url,
                display_name,
                allow_insecure,
                allow_private_network,
                compatibility_mode: compatibility_db_name(response.compatibility_mode),
                raw_card_json: Some(&discovered.raw_card_json),
                normalized_card_json: Some(&discovered.normalized_card_json),
                extended_card_json: discovered.extended_card_json.as_deref(),
                protocol_version: Some(&response.card.protocol_version),
                selected_binding: Some(binding_db_name(response.card.selected_binding)),
                selected_interface_url: Some(&response.card.selected_interface_url),
                credential_ref: credential_ids.first().map(String::as_str),
                credential_refs_json: &credential_refs_json,
                selected_tenant: response.card.selected_tenant.as_deref(),
                etag: discovered.etag.as_deref(),
                last_modified: discovered.last_modified.as_deref(),
                cache_expires_at: Some(discovered.cache_expires_at),
                fetched_at: Some(discovered.fetched_at),
                card_hash: Some(&discovered.card_hash),
                signature_status,
                trust_status: derived_trust_status,
                trusted_origin,
            })
            .await
            .map_err(db_error)?;
        Ok(())
    }

    async fn persist_credentials(
        &self,
        credentials: &[A2aCredentialInput],
        discovered: &DiscoveredA2aCard,
    ) -> Result<Vec<A2aCredentialRow>, AgentError> {
        let origin = origin_of(&discovered.response.card.selected_interface_url)?;
        let mut rows = Vec::with_capacity(credentials.len());
        for credential in credentials.iter().filter(|value| value.kind != A2aAuthKind::None) {
            if credential.kind == A2aAuthKind::Mtls {
                mtls_material(Some(credential))?;
            }
            let encrypted_secret = credential
                .secret
                .as_deref()
                .map(|secret| {
                    encrypt_string(secret, &self.encryption_key)
                        .map_err(|error| AgentError::internal(error.to_string()))
                })
                .transpose()?;
            let mut metadata = credential.metadata.clone().unwrap_or_else(|| serde_json::json!({}));
            if let Some(location) = credential.location {
                metadata["location"] =
                    serde_json::to_value(location).map_err(|_| AgentError::bad_request("凭据位置无效"))?;
            }
            let metadata_json = serde_json::to_string(&metadata)
                .map_err(|error| AgentError::bad_request(format!("凭据 metadata 无效：{error}")))?;
            let row = self
                .repo
                .upsert_credential(UpsertA2aCredentialParams {
                    id: None,
                    scheme_name: credential.scheme_name.as_deref(),
                    auth_kind: auth_kind_db_name(credential.kind),
                    header_name: credential.header_name.as_deref(),
                    encrypted_secret: encrypted_secret.as_deref(),
                    metadata_json: Some(&metadata_json),
                    origin: &origin,
                })
                .await
                .map_err(db_error)?;
            rows.push(row);
        }
        Ok(rows)
    }

    async fn replace_profile_credentials(
        &self,
        profile: &A2aAgentProfileRow,
        credential_ids: &[String],
    ) -> Result<(), AgentError> {
        let credential_refs_json =
            serde_json::to_string(credential_ids).map_err(|_| AgentError::internal("无法编码 A2A 凭据引用"))?;
        self.repo
            .upsert_profile(UpsertA2aAgentProfileParams {
                agent_id: &profile.agent_id,
                card_url: &profile.card_url,
                base_url: &profile.base_url,
                display_name: profile.display_name.as_deref(),
                allow_insecure: profile.allow_insecure,
                allow_private_network: profile.allow_private_network,
                compatibility_mode: &profile.compatibility_mode,
                raw_card_json: profile.raw_card_json.as_deref(),
                normalized_card_json: profile.normalized_card_json.as_deref(),
                extended_card_json: profile.extended_card_json.as_deref(),
                protocol_version: profile.protocol_version.as_deref(),
                selected_binding: profile.selected_binding.as_deref(),
                selected_interface_url: profile.selected_interface_url.as_deref(),
                credential_ref: credential_ids.first().map(String::as_str),
                credential_refs_json: &credential_refs_json,
                selected_tenant: profile.selected_tenant.as_deref(),
                etag: profile.etag.as_deref(),
                last_modified: profile.last_modified.as_deref(),
                cache_expires_at: profile.cache_expires_at,
                fetched_at: profile.fetched_at,
                card_hash: profile.card_hash.as_deref(),
                signature_status: &profile.signature_status,
                trust_status: &profile.trust_status,
                trusted_origin: profile.trusted_origin.as_deref(),
            })
            .await
            .map_err(db_error)?;
        Ok(())
    }

    pub(super) async fn load_credentials_for_url(
        &self,
        profile: &A2aAgentProfileRow,
        target_url: &str,
    ) -> Result<Vec<A2aCredentialInput>, AgentError> {
        let ids = credential_ids(profile)?;
        let rows = self.repo.find_credentials(&ids).await.map_err(db_error)?;
        let target_origin = origin_of(target_url)?;
        let mut credentials = Vec::with_capacity(rows.len());
        for row in rows {
            if target_origin != row.origin {
                continue;
            }
            let auth_kind = auth_kind_from_db(&row.auth_kind);
            let decrypted_secret = row
                .encrypted_secret
                .as_deref()
                .map(|value| {
                    decrypt_string(value, &self.encryption_key).map_err(|error| AgentError::internal(error.to_string()))
                })
                .transpose()?;
            let secret = if matches!(auth_kind, A2aAuthKind::OAuth2 | A2aAuthKind::Oidc) {
                match decrypted_secret {
                    Some(value) => {
                        let resolved = resolve_oauth_secret(
                            &value,
                            super::security::A2aNetworkPolicy {
                                allow_insecure: profile.allow_insecure,
                                allow_private_network: profile.allow_private_network,
                            },
                        )
                        .await?;
                        if let Some(refreshed_bundle) = resolved.refreshed_bundle {
                            let encrypted = encrypt_string(&refreshed_bundle, &self.encryption_key)
                                .map_err(|error| AgentError::internal(error.to_string()))?;
                            self.repo
                                .upsert_credential(UpsertA2aCredentialParams {
                                    id: Some(&row.id),
                                    scheme_name: row.scheme_name.as_deref(),
                                    auth_kind: &row.auth_kind,
                                    header_name: row.header_name.as_deref(),
                                    encrypted_secret: Some(&encrypted),
                                    metadata_json: row.metadata_json.as_deref(),
                                    origin: &row.origin,
                                })
                                .await
                                .map_err(db_error)?;
                        }
                        Some(resolved.access_token)
                    }
                    None => None,
                }
            } else {
                decrypted_secret
            };
            let metadata = row
                .metadata_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .map_err(|_| AgentError::internal("A2A 凭据 metadata 损坏"))?;
            let location = metadata
                .as_ref()
                .and_then(|value: &serde_json::Value| value.get("location"))
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|_| AgentError::internal("A2A 凭据位置损坏"))?;
            credentials.push(A2aCredentialInput {
                kind: auth_kind,
                scheme_name: row.scheme_name,
                header_name: row.header_name,
                location,
                secret,
                metadata,
            });
        }
        Ok(credentials)
    }

    async fn attach_extended_card(
        &self,
        discovered: &mut DiscoveredA2aCard,
        credentials: Vec<A2aCredentialInput>,
        allow_insecure: bool,
        allow_private_network: bool,
    ) -> Result<(), AgentError> {
        let public_card: a2a::AgentCard = serde_json::from_str(&discovered.normalized_card_json)
            .map_err(|_| AgentError::internal("A2A Card 缓存损坏"))?;
        if public_card.capabilities.extended_agent_card != Some(true) || credentials.is_empty() {
            return Ok(());
        }
        let endpoint = Url::parse(&discovered.response.card.selected_interface_url)
            .map_err(|_| AgentError::internal("A2A 接口 URL 损坏"))?;
        let config = A2aClientConfig {
            endpoint,
            binding: discovered.response.card.selected_binding,
            credentials,
            tenant: discovered.response.card.selected_tenant.clone(),
            compatibility_mode: discovered.response.compatibility_mode,
            extensions: discovered.response.card.required_extensions.clone(),
            allow_insecure,
            allow_private_network,
        };
        let client: Arc<dyn IA2aClient> = match config.binding {
            A2aBinding::Grpc => Arc::new(GrpcA2aClient::connect(config).await?),
            A2aBinding::JsonRpc | A2aBinding::HttpJson => Arc::new(A2aClient::new(config)?),
        };
        let card = client.get_extended_agent_card().await?;
        let encoded = serde_json::to_vec(&card)
            .map_err(|error| AgentError::internal(format!("编码扩展 Agent Card 失败：{error}")))?;
        if encoded.len() > MAX_AGENT_CARD_BYTES {
            return Err(AgentError::bad_gateway("扩展 Agent Card 超过 1 MiB 限制"));
        }
        let parsed = parse_agent_card(&encoded, &CardParseOptions::default())
            .map_err(|error| AgentError::bad_gateway(format!("扩展 Agent Card 无效：{error}")))?;
        discovered.extended_card_json = Some(
            serde_json::to_string(&parsed.card)
                .map_err(|error| AgentError::internal(format!("缓存扩展 Agent Card 失败：{error}")))?,
        );
        Ok(())
    }

    async fn row_to_response(&self, row: A2aAgentProfileRow) -> Result<A2aAgentResponse, AgentError> {
        let card = row
            .normalized_card_json
            .as_deref()
            .map(serde_json::from_str::<a2a::AgentCard>)
            .transpose()
            .map_err(|_| AgentError::internal("A2A Card 缓存损坏"))?
            .map(|card| card_summary(&card))
            .transpose()?;
        let extended_card = row
            .extended_card_json
            .as_deref()
            .map(serde_json::from_str::<a2a::AgentCard>)
            .transpose()
            .map_err(|_| AgentError::internal("A2A 扩展 Card 缓存损坏"))?
            .map(|card| card_summary(&card))
            .transpose()?;
        let ids = credential_ids(&row)?;
        let credentials = self.repo.find_credentials(&ids).await.map_err(db_error)?;
        let credential_kinds = credentials
            .iter()
            .map(|row| auth_kind_from_db(&row.auth_kind))
            .collect::<Vec<_>>();
        let configured_security_schemes = credentials.iter().filter_map(|row| row.scheme_name.clone()).collect();
        let configured_credentials = credentials
            .iter()
            .map(|credential| {
                let location = credential
                    .metadata_json
                    .as_deref()
                    .and_then(|metadata| serde_json::from_str::<serde_json::Value>(metadata).ok())
                    .and_then(|metadata| metadata.get("location").cloned())
                    .and_then(|location| serde_json::from_value::<A2aCredentialLocation>(location).ok());
                A2aConfiguredCredentialSummary {
                    kind: auth_kind_from_db(&credential.auth_kind),
                    scheme_name: credential.scheme_name.clone(),
                    header_name: credential.header_name.clone(),
                    location,
                }
            })
            .collect();
        Ok(A2aAgentResponse {
            agent_id: row.agent_id,
            card_url: row.card_url,
            base_url: row.base_url,
            display_name: row.display_name,
            allow_insecure: row.allow_insecure,
            allow_private_network: row.allow_private_network,
            compatibility_mode: compatibility_from_db(&row.compatibility_mode),
            card,
            has_extended_card: extended_card.is_some(),
            extended_card,
            has_credentials: !credentials.is_empty(),
            credential_kind: credential_kinds.first().copied(),
            credential_kinds,
            configured_security_schemes,
            configured_credentials,
            etag: row.etag,
            last_modified: row.last_modified,
            cache_expires_at: row.cache_expires_at,
            fetched_at: row.fetched_at,
            signature_status: row.signature_status,
            trust_status: row.trust_status,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }

    async fn record_online(
        &self,
        agent_id: &str,
        kind: AgentSnapshotCheckKind,
        latency_ms: i64,
    ) -> Result<(), AgentError> {
        let now = now_ms();
        self.registry
            .repo_handle()
            .update_availability_snapshot(
                agent_id,
                &UpdateAgentAvailabilitySnapshotParams {
                    last_check_status: Some("online"),
                    last_check_kind: Some(snapshot_kind_name(kind)),
                    last_check_error_code: None,
                    last_check_error_message: None,
                    last_check_guidance: None,
                    last_check_latency_ms: Some(latency_ms),
                    last_check_at: Some(now),
                    last_success_at: Some(now),
                    last_failure_at: None,
                },
            )
            .await
            .map_err(db_error)?;
        Ok(())
    }

    async fn record_offline(&self, agent_id: &str, kind: AgentSnapshotCheckKind, latency_ms: i64, error: &AgentError) {
        let now = now_ms();
        let _ = self
            .registry
            .repo_handle()
            .update_availability_snapshot(
                agent_id,
                &UpdateAgentAvailabilitySnapshotParams {
                    last_check_status: Some("offline"),
                    last_check_kind: Some(snapshot_kind_name(kind)),
                    last_check_error_code: Some(error_code(error)),
                    last_check_error_message: Some(&error.public_message()),
                    last_check_guidance: Some("检查 A2A 地址、网络策略、认证信息和 Agent Card 兼容性"),
                    last_check_latency_ms: Some(latency_ms),
                    last_check_at: Some(now),
                    last_success_at: None,
                    last_failure_at: Some(now),
                },
            )
            .await;
    }

    async fn cleanup_credentials(&self, credentials: &[A2aCredentialRow]) {
        for credential in credentials {
            let _ = self.repo.delete_credential(&credential.id).await;
        }
    }
}

fn validate_selected_origin(
    discovered: &DiscoveredA2aCard,
    trusted_origin: Option<&str>,
) -> Result<Option<String>, AgentError> {
    if !discovered.response.requires_origin_confirmation {
        return Ok(None);
    }
    let selected_origin = origin_of(&discovered.response.card.selected_interface_url)?;
    if trusted_origin != Some(selected_origin.as_str()) {
        return Err(AgentError::conflict(format!(
            "a2a_origin_confirmation_required:{selected_origin}"
        )));
    }
    Ok(Some(selected_origin))
}

fn origin_of(value: &str) -> Result<String, AgentError> {
    let url = Url::parse(value).map_err(|_| AgentError::bad_request("A2A 来源 URL 无效"))?;
    Ok(url.origin().ascii_serialization())
}

fn compatibility_db_name(mode: A2aCompatibilityMode) -> &'static str {
    match mode {
        A2aCompatibilityMode::V1 => "v1",
        A2aCompatibilityMode::V03 => "v0_3",
    }
}

fn compatibility_from_db(value: &str) -> A2aCompatibilityMode {
    if value == "v0_3" {
        A2aCompatibilityMode::V03
    } else {
        A2aCompatibilityMode::V1
    }
}

fn auth_kind_db_name(kind: A2aAuthKind) -> &'static str {
    match kind {
        A2aAuthKind::None => "none",
        A2aAuthKind::Bearer => "bearer",
        A2aAuthKind::ApiKey => "api_key",
        A2aAuthKind::Basic => "basic",
        A2aAuthKind::CustomHeader => "custom_header",
        A2aAuthKind::OAuth2 => "oauth2",
        A2aAuthKind::Oidc => "oidc",
        A2aAuthKind::Mtls => "mtls",
    }
}

fn auth_kind_from_db(value: &str) -> A2aAuthKind {
    match value {
        "bearer" => A2aAuthKind::Bearer,
        "api_key" => A2aAuthKind::ApiKey,
        "basic" => A2aAuthKind::Basic,
        "custom_header" => A2aAuthKind::CustomHeader,
        "oauth2" => A2aAuthKind::OAuth2,
        "oidc" => A2aAuthKind::Oidc,
        "mtls" => A2aAuthKind::Mtls,
        _ => A2aAuthKind::None,
    }
}

fn effective_credentials(
    legacy: Option<&A2aCredentialInput>,
    credentials: &[A2aCredentialInput],
) -> Vec<A2aCredentialInput> {
    if credentials.is_empty() {
        legacy.iter().cloned().cloned().collect()
    } else {
        credentials.to_vec()
    }
}

fn merge_stored_credentials(
    mut requested: Vec<A2aCredentialInput>,
    stored: &[A2aCredentialInput],
) -> Vec<A2aCredentialInput> {
    for credential in &mut requested {
        let matching = stored.iter().find(|saved| {
            match (
                credential.scheme_name.as_deref().filter(|name| !name.is_empty()),
                saved.scheme_name.as_deref().filter(|name| !name.is_empty()),
            ) {
                (Some(left), Some(right)) => left == right,
                (None, None) => {
                    credential.kind == saved.kind
                        && credential.header_name == saved.header_name
                        && credential.location == saved.location
                }
                _ => false,
            }
        });
        let Some(saved) = matching else {
            continue;
        };
        if credential.secret.as_deref().is_none_or(str::is_empty) {
            credential.secret = saved.secret.clone();
        }
        match (&mut credential.metadata, &saved.metadata) {
            (None, Some(metadata)) => credential.metadata = Some(metadata.clone()),
            (Some(metadata), Some(saved_metadata)) => {
                if let (Some(target), Some(source)) = (metadata.as_object_mut(), saved_metadata.as_object()) {
                    for (key, value) in source {
                        target.entry(key.clone()).or_insert_with(|| value.clone());
                    }
                }
            }
            _ => {}
        }
    }
    requested
}

fn credential_ids(profile: &A2aAgentProfileRow) -> Result<Vec<String>, AgentError> {
    let mut ids = serde_json::from_str::<Vec<String>>(&profile.credential_refs_json)
        .map_err(|_| AgentError::internal("A2A 凭据引用缓存损坏"))?;
    if ids.is_empty()
        && let Some(id) = profile.credential_ref.as_ref().filter(|id| !id.is_empty())
    {
        ids.push(id.clone());
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}

fn normalize_and_validate_credentials(
    credentials: Vec<A2aCredentialInput>,
    normalized_card_json: &str,
) -> Result<Vec<A2aCredentialInput>, AgentError> {
    if credentials.len() > 8 {
        return Err(AgentError::bad_request("单个 A2A Agent 最多配置 8 份凭据"));
    }
    let card: a2a::AgentCard =
        serde_json::from_str(normalized_card_json).map_err(|_| AgentError::internal("A2A Card 缓存损坏"))?;
    let schemes = card.security_schemes.as_ref();
    let mut normalized = credentials
        .into_iter()
        .filter(|credential| credential.kind != A2aAuthKind::None)
        .collect::<Vec<_>>();

    for credential in &mut normalized {
        if credential.secret.as_deref().map(str::trim).is_none_or(str::is_empty) {
            return Err(AgentError::bad_request("A2A 凭据值不能为空"));
        }
        if credential
            .secret
            .as_ref()
            .is_some_and(|secret| secret.len() > 1024 * 1024)
        {
            return Err(AgentError::bad_request("单份 A2A 凭据不得超过 1 MiB"));
        }
        if credential.scheme_name.as_deref().is_none_or(str::is_empty) {
            let candidates = schemes
                .into_iter()
                .flat_map(|values| values.iter())
                .filter(|(_, scheme)| credential_matches_scheme(credential.kind, scheme))
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>();
            if candidates.len() == 1 {
                credential.scheme_name = candidates.into_iter().next();
            }
        }
        if let (Some(name), Some(schemes)) = (credential.scheme_name.as_deref(), schemes) {
            let scheme = schemes
                .get(name)
                .ok_or_else(|| AgentError::bad_request(format!("Agent Card 未声明安全方案“{name}”")))?;
            if !credential_matches_scheme(credential.kind, scheme) {
                return Err(AgentError::bad_request(format!(
                    "凭据类型与 Agent Card 安全方案“{name}”不匹配"
                )));
            }
            if let a2a::SecurityScheme::ApiKey(api_key) = scheme {
                credential.header_name = Some(api_key.name.clone());
                credential.location = Some(match api_key.location.to_ascii_lowercase().as_str() {
                    "query" => tjuaeui_api_types::A2aCredentialLocation::Query,
                    "cookie" => tjuaeui_api_types::A2aCredentialLocation::Cookie,
                    _ => tjuaeui_api_types::A2aCredentialLocation::Header,
                });
            }
        }
    }

    let mut configured = normalized
        .iter()
        .filter_map(|credential| credential.scheme_name.as_deref())
        .collect::<Vec<_>>();
    configured.sort_unstable();
    if configured.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(AgentError::bad_request("同一 A2A 安全方案不能配置多份凭据"));
    }

    if let Some(requirements) = card.security_requirements.as_ref()
        && !requirements.is_empty()
        && !requirements
            .iter()
            .any(|alternative| alternative.keys().all(|name| configured.contains(&name.as_str())))
    {
        let deferred_oauth = schemes.is_some_and(|schemes| {
            requirements.iter().any(|alternative| {
                !alternative.is_empty()
                    && alternative.keys().all(|name| {
                        schemes.get(name).is_some_and(|scheme| {
                            matches!(
                                scheme,
                                a2a::SecurityScheme::OAuth2(_) | a2a::SecurityScheme::OpenIdConnect(_)
                            )
                        })
                    })
            })
        });
        if !deferred_oauth {
            return Err(AgentError::unauthorized(
                "当前凭据不能满足 Agent Card 声明的任一安全要求组合",
            ));
        }
    }
    Ok(normalized)
}

fn credential_matches_scheme(kind: A2aAuthKind, scheme: &a2a::SecurityScheme) -> bool {
    match scheme {
        a2a::SecurityScheme::ApiKey(_) => kind == A2aAuthKind::ApiKey,
        a2a::SecurityScheme::HttpAuth(value) => {
            (value.scheme.eq_ignore_ascii_case("bearer") && kind == A2aAuthKind::Bearer)
                || (value.scheme.eq_ignore_ascii_case("basic") && kind == A2aAuthKind::Basic)
                || kind == A2aAuthKind::CustomHeader
        }
        a2a::SecurityScheme::OAuth2(_) => kind == A2aAuthKind::OAuth2,
        a2a::SecurityScheme::OpenIdConnect(_) => kind == A2aAuthKind::Oidc,
        a2a::SecurityScheme::MutualTls(_) => kind == A2aAuthKind::Mtls,
    }
}

fn error_code(error: &AgentError) -> &'static str {
    match error {
        AgentError::Unauthorized(_) => "a2a_auth_required",
        AgentError::Forbidden(_) => "a2a_policy_denied",
        AgentError::Timeout(_) => "a2a_timeout",
        AgentError::RateLimited => "a2a_rate_limited",
        AgentError::BadRequest(_) => "a2a_card_invalid",
        _ => "a2a_connection_failed",
    }
}

fn snapshot_kind_name(kind: AgentSnapshotCheckKind) -> &'static str {
    match kind {
        AgentSnapshotCheckKind::Startup => "startup",
        AgentSnapshotCheckKind::Scheduled => "scheduled",
        AgentSnapshotCheckKind::Manual => "manual",
        AgentSnapshotCheckKind::Session => "session",
    }
}

fn db_error(error: impl std::fmt::Display) -> AgentError {
    AgentError::internal(format!("A2A 数据库操作失败：{error}"))
}
