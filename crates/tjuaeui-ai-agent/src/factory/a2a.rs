use std::sync::Arc;

use tjuaeui_api_types::{A2aAuthKind, A2aBinding, A2aCompatibilityMode, A2aCredentialInput, A2aCredentialLocation};
use tjuaeui_common::{decrypt_string, encrypt_string, now_ms};
use url::Url;

use super::AgentFactoryDeps;
use super::context::FactoryContext;
use crate::agent_task::AgentInstance;
use crate::error::AgentError;
use crate::manager::a2a::{A2aAgentManager, A2aClient, A2aClientConfig, GrpcA2aClient, IA2aClient};
use crate::runtime_assets::{
    RuntimeAssetLoadRequest, RuntimeBoundaryPhase, RuntimeBoundaryReporter, handshake_runtime_asset_receipt,
};
use crate::services::a2a::oauth::resolve_oauth_secret;
use crate::services::a2a::security::A2aNetworkPolicy;
use crate::session_context::A2aSessionBuildContext;
use tjuaeui_db::UpsertA2aCredentialParams;

pub(super) async fn build(
    deps: Arc<AgentFactoryDeps>,
    context: A2aSessionBuildContext,
    factory: FactoryContext,
    runtime_asset_request: Option<RuntimeAssetLoadRequest>,
    runtime_boundary_reporter: Option<RuntimeBoundaryReporter>,
) -> Result<AgentInstance, AgentError> {
    let profile = deps
        .a2a_repo
        .find_profile(&context.agent_id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| AgentError::not_found(format!("找不到 A2A Agent“{}”", context.agent_id)))?;
    if !matches!(profile.trust_status.as_str(), "origin_verified" | "trusted") {
        return Err(AgentError::forbidden("A2A Agent 的接口来源尚未被信任或已被拒绝"));
    }
    let endpoint = profile
        .selected_interface_url
        .as_deref()
        .ok_or_else(|| AgentError::conflict("A2A Agent 尚无可用接口缓存"))
        .and_then(|value| Url::parse(value).map_err(|_| AgentError::internal("A2A 接口缓存 URL 损坏")))?;
    let binding = match profile.selected_binding.as_deref() {
        Some("json_rpc") => A2aBinding::JsonRpc,
        Some("http_json") => A2aBinding::HttpJson,
        Some("grpc") => A2aBinding::Grpc,
        _ => return Err(AgentError::conflict("A2A Agent 尚无可用协议绑定")),
    };
    let card: a2a::AgentCard = profile
        .normalized_card_json
        .as_deref()
        .ok_or_else(|| AgentError::conflict("A2A Agent 尚无 Card 缓存"))
        .and_then(|value| serde_json::from_str(value).map_err(|_| AgentError::internal("A2A Card 缓存损坏")))?;
    let mut credential_ids = serde_json::from_str::<Vec<String>>(&profile.credential_refs_json)
        .map_err(|_| AgentError::internal("A2A 凭据引用缓存损坏"))?;
    if credential_ids.is_empty()
        && let Some(id) = profile.credential_ref.as_ref()
    {
        credential_ids.push(id.clone());
    }
    let rows = deps
        .a2a_repo
        .find_credentials(&credential_ids)
        .await
        .map_err(db_error)?;
    if rows.len() != credential_ids.len() {
        return Err(AgentError::conflict("A2A 凭据引用不存在"));
    }
    let mut credentials = Vec::with_capacity(rows.len());
    for row in rows {
        let endpoint_origin = endpoint.origin().ascii_serialization();
        if row.origin != endpoint_origin {
            return Err(AgentError::forbidden("A2A 凭据来源与当前接口不一致"));
        }
        let auth_kind = auth_kind_from_db(&row.auth_kind);
        let decrypted_secret = row
            .encrypted_secret
            .as_deref()
            .map(|value| {
                decrypt_string(value, &deps.encryption_key)
                    .map_err(|error| AgentError::internal(format!("无法解密 A2A 凭据：{error}")))
            })
            .transpose()?;
        let secret = if matches!(auth_kind, A2aAuthKind::OAuth2 | A2aAuthKind::Oidc) {
            match decrypted_secret {
                Some(value) => {
                    let resolved = resolve_oauth_secret(
                        &value,
                        A2aNetworkPolicy {
                            allow_insecure: profile.allow_insecure,
                            allow_private_network: profile.allow_private_network,
                        },
                    )
                    .await?;
                    if let Some(refreshed_bundle) = resolved.refreshed_bundle {
                        let encrypted = encrypt_string(&refreshed_bundle, &deps.encryption_key)
                            .map_err(|error| AgentError::internal(format!("无法加密 A2A OAuth 凭据：{error}")))?;
                        deps.a2a_repo
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
            .map(serde_json::from_value::<A2aCredentialLocation>)
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
    let interface_snapshot_json = serde_json::json!({
        "agent_id": context.agent_id,
        "endpoint": endpoint,
        "binding": binding,
        "protocol_version": profile.protocol_version,
        "tenant": profile.selected_tenant,
        "card_hash": profile.card_hash,
    })
    .to_string();
    let client_config = A2aClientConfig {
        endpoint,
        binding,
        credentials,
        tenant: profile.selected_tenant,
        compatibility_mode: if profile.compatibility_mode == "v0_3" {
            A2aCompatibilityMode::V03
        } else {
            A2aCompatibilityMode::V1
        },
        extensions: card
            .capabilities
            .extensions
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|extension| extension.required == Some(true))
            .map(|extension| extension.uri.clone())
            .collect(),
        allow_insecure: profile.allow_insecure,
        allow_private_network: profile.allow_private_network,
    };
    let client: Arc<dyn IA2aClient> = match binding {
        A2aBinding::Grpc => {
            let started_at = now_ms();
            match GrpcA2aClient::connect(client_config).await {
                Ok(client) => {
                    if let Some(reporter) = runtime_boundary_reporter.as_ref() {
                        reporter.succeeded(RuntimeBoundaryPhase::Connect, started_at, now_ms(), None);
                    }
                    Arc::new(client)
                }
                Err(error) => {
                    if let Some(reporter) = runtime_boundary_reporter.as_ref() {
                        reporter.failed(
                            RuntimeBoundaryPhase::Connect,
                            started_at,
                            now_ms(),
                            None,
                            "TJUAE_RUNTIME_REMOTE_CONNECT_FAILED",
                        );
                    }
                    return Err(error);
                }
            }
        }
        A2aBinding::JsonRpc | A2aBinding::HttpJson => Arc::new(A2aClient::new(client_config)?),
    };
    let runtime_asset_receipt = if let Some(request) = runtime_asset_request.as_ref() {
        // 对 HTTP/JSON A2A，构造 client 本身不会访问远端；扩展 Card 请求是
        // 实际的认证协议往返。只有握手成功后才能确认引擎适配器已运行。
        let started_at = now_ms();
        if let Err(error) = client.get_extended_agent_card().await {
            if let Some(reporter) = runtime_boundary_reporter.as_ref() {
                let ended_at = now_ms();
                if request.runtime_assets.is_empty() {
                    reporter.failed(
                        RuntimeBoundaryPhase::Handshake,
                        started_at,
                        ended_at,
                        None,
                        "TJUAE_RUNTIME_HANDSHAKE_FAILED",
                    );
                } else {
                    for asset in &request.runtime_assets {
                        reporter.failed(
                            RuntimeBoundaryPhase::Handshake,
                            started_at,
                            ended_at,
                            Some(asset),
                            "TJUAE_RUNTIME_HANDSHAKE_FAILED",
                        );
                    }
                }
            }
            return Err(error);
        }
        if let Some(reporter) = runtime_boundary_reporter.as_ref() {
            let ended_at = now_ms();
            if request.runtime_assets.is_empty() {
                reporter.succeeded(RuntimeBoundaryPhase::Handshake, started_at, ended_at, None);
            } else {
                for asset in &request.runtime_assets {
                    reporter.succeeded(RuntimeBoundaryPhase::Handshake, started_at, ended_at, Some(asset));
                }
            }
        }
        Some(
            handshake_runtime_asset_receipt(request)
                .map_err(|error| AgentError::conflict(format!("A2A 运行资产回执无法确认：{error}")))?,
        )
    } else {
        None
    };
    let inject_started_at = now_ms();
    let manager_result = A2aAgentManager::new_with_runtime_asset_receipt(
        factory.conversation_id,
        factory.workspace,
        context.agent_id,
        client,
        deps.a2a_repo.clone(),
        interface_snapshot_json,
        card.default_output_modes,
        card.capabilities.streaming == Some(true),
        context.preset_context,
        runtime_asset_receipt,
    )
    .await;
    if let Some(reporter) = runtime_boundary_reporter.as_ref()
        && let Some(request) = runtime_asset_request.as_ref()
    {
        let ended_at = now_ms();
        for asset in &request.core_assets {
            match &manager_result {
                Ok(_) => reporter.succeeded(RuntimeBoundaryPhase::Inject, inject_started_at, ended_at, Some(asset)),
                Err(_) => reporter.failed(
                    RuntimeBoundaryPhase::Inject,
                    inject_started_at,
                    ended_at,
                    Some(asset),
                    "TJUAE_RUNTIME_ASSISTANT_INJECT_FAILED",
                ),
            }
        }
    }
    let manager = manager_result?;
    Ok(AgentInstance::A2a(Arc::new(manager)))
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

fn db_error(error: impl std::fmt::Display) -> AgentError {
    AgentError::internal(format!("读取 A2A 配置失败：{error}"))
}
