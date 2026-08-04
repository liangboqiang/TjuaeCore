use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use dashmap::DashMap;
use jsonwebtoken::jwk::{JwkSet, KeyOperations, PublicKeyUse};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use reqwest::header::{ACCEPT, CONTENT_LENGTH, CONTENT_TYPE};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tjuaeui_api_types::{
    A2aAuthKind, A2aCredentialInput, A2aOAuthFlowKind, CompleteA2aOAuthRequest, StartA2aOAuthRequest,
    StartA2aOAuthResponse,
};
use url::Url;

use crate::error::AgentError;

use super::security::{A2aNetworkPolicy, resolve_network_target};

const MAX_OAUTH_RESPONSE_BYTES: usize = 256 * 1024;
const MAX_PENDING_SECONDS: u64 = 30 * 60;
const TOKEN_EXPIRY_SKEW_MS: i64 = 30_000;

#[derive(Debug, Clone)]
struct PendingOAuth {
    agent_id: String,
    auth_kind: A2aAuthKind,
    scheme_name: String,
    flow: A2aOAuthFlowKind,
    token_url: Url,
    client_id: String,
    client_secret: Option<String>,
    redirect_uri: Option<String>,
    code_verifier: Option<String>,
    device_code: Option<String>,
    scopes: Vec<String>,
    expires_at: i64,
    next_poll_at: i64,
    interval_seconds: u64,
    oidc_issuer: Option<String>,
    oidc_jwks_url: Option<Url>,
    oidc_nonce: Option<String>,
    policy: A2aNetworkPolicy,
}

#[derive(Debug, Clone)]
struct OAuthEndpoints {
    auth_kind: A2aAuthKind,
    scheme_name: String,
    authorization_url: Option<Url>,
    token_url: Url,
    device_authorization_url: Option<Url>,
    scopes: Vec<String>,
    oidc_issuer: Option<String>,
    oidc_jwks_url: Option<Url>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct OAuthTokenBundle {
    pub version: u8,
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
    pub token_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(default)]
    pub scopes: Vec<String>,
    pub token_url: String,
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    /// Client Credentials has no refresh token; an expired token is renewed
    /// by repeating the original grant with the stored client identity.
    #[serde(default)]
    pub client_credentials: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oidc_issuer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oidc_jwks_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default = "default_token_type")]
    token_type: String,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeviceAuthorizationResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    expires_in: u64,
    #[serde(default = "default_device_interval")]
    interval: u64,
}

#[derive(Debug, Deserialize)]
struct OidcConfiguration {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    #[serde(default)]
    device_authorization_endpoint: Option<String>,
    #[serde(default)]
    scopes_supported: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct OAuthErrorResponse {
    error: String,
}

#[derive(Clone, Default)]
pub(crate) struct A2aOAuthCoordinator {
    pending: Arc<DashMap<String, PendingOAuth>>,
}

impl A2aOAuthCoordinator {
    pub(crate) async fn start(
        &self,
        agent_id: &str,
        card: &a2a::AgentCard,
        request: StartA2aOAuthRequest,
        policy: A2aNetworkPolicy,
    ) -> Result<StartA2aOAuthResponse, AgentError> {
        validate_start_request(&request)?;
        self.remove_expired();
        let endpoints = select_endpoints(card, request.scheme_name.as_deref(), request.flow, policy).await?;
        let flow = request.flow.unwrap_or_else(|| {
            if endpoints.authorization_url.is_some() {
                A2aOAuthFlowKind::AuthorizationCode
            } else if endpoints.device_authorization_url.is_some() {
                A2aOAuthFlowKind::DeviceCode
            } else {
                A2aOAuthFlowKind::ClientCredentials
            }
        });
        let mut scopes = if request.scopes.is_empty() {
            endpoints.scopes.clone()
        } else {
            request.scopes.clone()
        };
        if endpoints.auth_kind == A2aAuthKind::Oidc && !scopes.iter().any(|scope| scope == "openid") {
            scopes.insert(0, "openid".to_owned());
        }
        let state = random_urlsafe(32)?;
        let now = tjuaeui_common::now_ms();

        match flow {
            A2aOAuthFlowKind::AuthorizationCode => {
                let mut authorization_url = endpoints
                    .authorization_url
                    .clone()
                    .ok_or_else(|| AgentError::bad_request("所选 A2A 安全方案不支持 Authorization Code"))?;
                let redirect_uri = request
                    .redirect_uri
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| AgentError::bad_request("Authorization Code 流程需要 redirect_uri"))?;
                validate_redirect_uri(redirect_uri)?;
                let verifier = random_urlsafe(48)?;
                let oidc_nonce = (endpoints.auth_kind == A2aAuthKind::Oidc)
                    .then(|| random_urlsafe(32))
                    .transpose()?;
                let challenge =
                    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
                {
                    let mut query = authorization_url.query_pairs_mut();
                    query.append_pair("response_type", "code");
                    query.append_pair("client_id", request.client_id.trim());
                    query.append_pair("redirect_uri", redirect_uri);
                    query.append_pair("state", &state);
                    query.append_pair("code_challenge", &challenge);
                    query.append_pair("code_challenge_method", "S256");
                    if !scopes.is_empty() {
                        query.append_pair("scope", &scopes.join(" "));
                    }
                    if let Some(nonce) = oidc_nonce.as_deref() {
                        query.append_pair("nonce", nonce);
                    }
                }
                let expires_at = now + (MAX_PENDING_SECONDS as i64 * 1000);
                self.pending.insert(
                    state.clone(),
                    PendingOAuth {
                        agent_id: agent_id.to_owned(),
                        auth_kind: endpoints.auth_kind,
                        scheme_name: endpoints.scheme_name,
                        flow,
                        token_url: endpoints.token_url,
                        client_id: request.client_id.trim().to_owned(),
                        client_secret: normalized_secret(request.client_secret),
                        redirect_uri: Some(redirect_uri.to_owned()),
                        code_verifier: Some(verifier),
                        device_code: None,
                        scopes,
                        expires_at,
                        next_poll_at: now,
                        interval_seconds: 0,
                        oidc_issuer: endpoints.oidc_issuer,
                        oidc_jwks_url: endpoints.oidc_jwks_url,
                        oidc_nonce,
                        policy,
                    },
                );
                Ok(StartA2aOAuthResponse {
                    state,
                    flow,
                    authorization_url: Some(authorization_url.to_string()),
                    verification_uri: None,
                    verification_uri_complete: None,
                    user_code: None,
                    expires_at,
                    interval_seconds: None,
                })
            }
            A2aOAuthFlowKind::DeviceCode => {
                let device_url = endpoints
                    .device_authorization_url
                    .clone()
                    .ok_or_else(|| AgentError::bad_request("所选 A2A 安全方案不支持 Device Code"))?;
                let mut form = vec![("client_id", request.client_id.trim().to_owned())];
                if !scopes.is_empty() {
                    form.push(("scope", scopes.join(" ")));
                }
                let response: DeviceAuthorizationResponse = post_form_json(&device_url, policy, &form).await?;
                if response.device_code.is_empty() || response.user_code.is_empty() {
                    return Err(AgentError::bad_gateway("OAuth Device Code 响应缺少必要字段"));
                }
                let expires_in = response.expires_in.clamp(30, MAX_PENDING_SECONDS);
                let interval_seconds = response.interval.clamp(1, 60);
                let expires_at = now + (expires_in as i64 * 1000);
                self.pending.insert(
                    state.clone(),
                    PendingOAuth {
                        agent_id: agent_id.to_owned(),
                        auth_kind: endpoints.auth_kind,
                        scheme_name: endpoints.scheme_name,
                        flow,
                        token_url: endpoints.token_url,
                        client_id: request.client_id.trim().to_owned(),
                        client_secret: normalized_secret(request.client_secret),
                        redirect_uri: None,
                        code_verifier: None,
                        device_code: Some(response.device_code),
                        scopes,
                        expires_at,
                        next_poll_at: now + (interval_seconds as i64 * 1000),
                        interval_seconds,
                        oidc_issuer: endpoints.oidc_issuer,
                        oidc_jwks_url: endpoints.oidc_jwks_url,
                        oidc_nonce: None,
                        policy,
                    },
                );
                Ok(StartA2aOAuthResponse {
                    state,
                    flow,
                    authorization_url: None,
                    verification_uri: Some(response.verification_uri),
                    verification_uri_complete: response.verification_uri_complete,
                    user_code: Some(response.user_code),
                    expires_at,
                    interval_seconds: Some(interval_seconds),
                })
            }
            A2aOAuthFlowKind::ClientCredentials => {
                let expires_at = now + (MAX_PENDING_SECONDS as i64 * 1000);
                self.pending.insert(
                    state.clone(),
                    PendingOAuth {
                        agent_id: agent_id.to_owned(),
                        auth_kind: endpoints.auth_kind,
                        scheme_name: endpoints.scheme_name,
                        flow,
                        token_url: endpoints.token_url,
                        client_id: request.client_id.trim().to_owned(),
                        client_secret: normalized_secret(request.client_secret),
                        redirect_uri: None,
                        code_verifier: None,
                        device_code: None,
                        scopes,
                        expires_at,
                        next_poll_at: now,
                        interval_seconds: 0,
                        oidc_issuer: endpoints.oidc_issuer,
                        oidc_jwks_url: endpoints.oidc_jwks_url,
                        oidc_nonce: None,
                        policy,
                    },
                );
                Ok(StartA2aOAuthResponse {
                    state,
                    flow,
                    authorization_url: None,
                    verification_uri: None,
                    verification_uri_complete: None,
                    user_code: None,
                    expires_at,
                    interval_seconds: None,
                })
            }
        }
    }

    pub(crate) async fn complete(
        &self,
        agent_id: &str,
        request: CompleteA2aOAuthRequest,
    ) -> Result<A2aCredentialInput, AgentError> {
        self.remove_expired();
        let pending = self
            .pending
            .get(&request.state)
            .map(|entry| entry.clone())
            .ok_or_else(|| AgentError::not_found("OAuth 授权状态不存在或已过期"))?;
        if pending.agent_id != agent_id {
            return Err(AgentError::forbidden("OAuth 授权状态不属于该 A2A Agent"));
        }
        let now = tjuaeui_common::now_ms();
        if now >= pending.expires_at {
            self.pending.remove(&request.state);
            return Err(AgentError::unauthorized("OAuth 授权已过期，请重新开始"));
        }

        let mut form = vec![
            ("client_id", pending.client_id.clone()),
            (
                "grant_type",
                match pending.flow {
                    A2aOAuthFlowKind::AuthorizationCode => "authorization_code",
                    A2aOAuthFlowKind::DeviceCode => "urn:ietf:params:oauth:grant-type:device_code",
                    A2aOAuthFlowKind::ClientCredentials => "client_credentials",
                }
                .to_owned(),
            ),
        ];
        if let Some(client_secret) = pending.client_secret.as_ref() {
            form.push(("client_secret", client_secret.clone()));
        }
        match pending.flow {
            A2aOAuthFlowKind::AuthorizationCode => {
                let code = request
                    .code
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| AgentError::bad_request("OAuth 完成请求缺少 authorization code"))?;
                form.push(("code", code.to_owned()));
                form.push(("redirect_uri", pending.redirect_uri.clone().unwrap_or_default()));
                form.push(("code_verifier", pending.code_verifier.clone().unwrap_or_default()));
            }
            A2aOAuthFlowKind::DeviceCode => {
                if now < pending.next_poll_at {
                    return Err(AgentError::conflict("OAuth Device Code 尚未到下一次轮询时间"));
                }
                form.push(("device_code", pending.device_code.clone().unwrap_or_default()));
            }
            A2aOAuthFlowKind::ClientCredentials => {
                if !pending.scopes.is_empty() {
                    form.push(("scope", pending.scopes.join(" ")));
                }
            }
        }

        let token = match post_token(&pending.token_url, pending.policy, &form).await {
            Ok(token) => token,
            Err(TokenExchangeError::Pending) => {
                if let Some(mut entry) = self.pending.get_mut(&request.state) {
                    entry.next_poll_at = now + (entry.interval_seconds as i64 * 1000);
                }
                return Err(AgentError::conflict("OAuth Device Code 正在等待用户授权"));
            }
            Err(TokenExchangeError::SlowDown) => {
                if let Some(mut entry) = self.pending.get_mut(&request.state) {
                    entry.interval_seconds = (entry.interval_seconds + 5).min(60);
                    entry.next_poll_at = now + (entry.interval_seconds as i64 * 1000);
                }
                return Err(AgentError::conflict("OAuth 服务要求降低轮询频率"));
            }
            Err(TokenExchangeError::Agent(error)) => return Err(error),
        };
        self.pending.remove(&request.state);
        token_to_credential(token, pending).await
    }

    fn remove_expired(&self) {
        let now = tjuaeui_common::now_ms();
        self.pending.retain(|_, flow| flow.expires_at > now);
    }
}

pub(crate) fn decode_oauth_bundle(value: &str) -> Option<OAuthTokenBundle> {
    serde_json::from_str::<OAuthTokenBundle>(value)
        .ok()
        .filter(|bundle| bundle.version == 1 && !bundle.access_token.is_empty())
}

pub(crate) fn runtime_oauth_secret(value: &str) -> Result<String, AgentError> {
    let Some(bundle) = decode_oauth_bundle(value) else {
        if value.trim_start().starts_with('{') {
            return Err(AgentError::internal("A2A OAuth 凭据格式损坏"));
        }
        return Ok(value.to_owned());
    };
    if bundle
        .expires_at
        .is_some_and(|expires_at| expires_at <= tjuaeui_common::now_ms() + TOKEN_EXPIRY_SKEW_MS)
    {
        return Err(AgentError::unauthorized("A2A OAuth 访问令牌已过期，请重新授权"));
    }
    Ok(bundle.access_token)
}

pub(crate) struct ResolvedOAuthSecret {
    pub access_token: String,
    pub refreshed_bundle: Option<String>,
}

pub(crate) async fn resolve_oauth_secret(
    value: &str,
    policy: A2aNetworkPolicy,
) -> Result<ResolvedOAuthSecret, AgentError> {
    let Some(mut bundle) = decode_oauth_bundle(value) else {
        return Ok(ResolvedOAuthSecret {
            access_token: runtime_oauth_secret(value)?,
            refreshed_bundle: None,
        });
    };
    if bundle
        .expires_at
        .is_none_or(|expires_at| expires_at > tjuaeui_common::now_ms() + TOKEN_EXPIRY_SKEW_MS)
    {
        return Ok(ResolvedOAuthSecret {
            access_token: bundle.access_token,
            refreshed_bundle: None,
        });
    }
    let token_url = validated_endpoint(&bundle.token_url, policy).await?;
    let mut form = if let Some(refresh_token) = bundle.refresh_token.clone() {
        vec![
            ("grant_type", "refresh_token".to_owned()),
            ("refresh_token", refresh_token),
            ("client_id", bundle.client_id.clone()),
        ]
    } else if bundle.client_credentials {
        vec![
            ("grant_type", "client_credentials".to_owned()),
            ("client_id", bundle.client_id.clone()),
        ]
    } else {
        return Err(AgentError::unauthorized(
            "A2A OAuth 访问令牌已过期且没有 refresh_token，请重新授权",
        ));
    };
    if let Some(client_secret) = bundle.client_secret.as_ref() {
        form.push(("client_secret", client_secret.clone()));
    }
    if !bundle.scopes.is_empty() {
        form.push(("scope", bundle.scopes.join(" ")));
    }
    let refreshed = post_token(&token_url, policy, &form)
        .await
        .map_err(|error| match error {
            TokenExchangeError::Pending | TokenExchangeError::SlowDown => {
                AgentError::bad_gateway("OAuth Refresh Token 响应状态无效")
            }
            TokenExchangeError::Agent(error) => error,
        })?;
    if let Some(id_token) = refreshed.id_token.as_deref()
        && let (Some(issuer), Some(jwks_url)) = (bundle.oidc_issuer.as_deref(), bundle.oidc_jwks_url.as_deref())
    {
        let jwks_url = validated_endpoint(jwks_url, policy).await?;
        validate_oidc_id_token(id_token, &bundle.client_id, issuer, &jwks_url, None, policy).await?;
    }
    bundle.access_token = refreshed.access_token;
    if refreshed.refresh_token.is_some() {
        bundle.refresh_token = refreshed.refresh_token;
    }
    if refreshed.id_token.is_some() {
        bundle.id_token = refreshed.id_token;
    }
    bundle.token_type = refreshed.token_type;
    bundle.expires_at = refreshed
        .expires_in
        .map(|seconds| tjuaeui_common::now_ms() + (seconds.min(31_536_000) as i64 * 1000));
    if let Some(scope) = refreshed.scope {
        bundle.scopes = scope.split_whitespace().map(str::to_owned).collect();
    }
    let refreshed_bundle = serde_json::to_string(&bundle)
        .map_err(|error| AgentError::internal(format!("编码刷新后的 OAuth token bundle 失败：{error}")))?;
    Ok(ResolvedOAuthSecret {
        access_token: bundle.access_token,
        refreshed_bundle: Some(refreshed_bundle),
    })
}

async fn token_to_credential(
    token: OAuthTokenResponse,
    pending: PendingOAuth,
) -> Result<A2aCredentialInput, AgentError> {
    if token.access_token.trim().is_empty() {
        return Err(AgentError::bad_gateway("OAuth Token 响应缺少 access_token"));
    }
    if pending.auth_kind == A2aAuthKind::Oidc {
        let id_token = token
            .id_token
            .as_deref()
            .ok_or_else(|| AgentError::bad_gateway("OIDC Token 响应缺少 id_token"))?;
        let issuer = pending
            .oidc_issuer
            .as_deref()
            .ok_or_else(|| AgentError::internal("OIDC 授权状态缺少 issuer"))?;
        let jwks_url = pending
            .oidc_jwks_url
            .as_ref()
            .ok_or_else(|| AgentError::internal("OIDC 授权状态缺少 jwks_uri"))?;
        validate_oidc_id_token(
            id_token,
            &pending.client_id,
            issuer,
            jwks_url,
            pending.oidc_nonce.as_deref(),
            pending.policy,
        )
        .await?;
    }
    let scopes = token
        .scope
        .as_deref()
        .map(|value| value.split_whitespace().map(str::to_owned).collect())
        .unwrap_or(pending.scopes);
    let expires_at = token
        .expires_in
        .map(|seconds| tjuaeui_common::now_ms() + (seconds.min(31_536_000) as i64 * 1000));
    let bundle = OAuthTokenBundle {
        version: 1,
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        id_token: token.id_token,
        token_type: token.token_type.clone(),
        expires_at,
        scopes: scopes.clone(),
        token_url: pending.token_url.to_string(),
        client_id: pending.client_id,
        client_secret: pending.client_secret,
        client_credentials: pending.flow == A2aOAuthFlowKind::ClientCredentials,
        oidc_issuer: pending.oidc_issuer,
        oidc_jwks_url: pending.oidc_jwks_url.map(|url| url.to_string()),
    };
    let secret = serde_json::to_string(&bundle)
        .map_err(|error| AgentError::internal(format!("编码 OAuth token bundle 失败：{error}")))?;
    Ok(A2aCredentialInput {
        kind: pending.auth_kind,
        scheme_name: Some(pending.scheme_name.clone()),
        header_name: None,
        location: None,
        secret: Some(secret),
        metadata: Some(serde_json::json!({
            "oauth_bundle_version": 1,
            "scheme_name": pending.scheme_name,
            "flow": pending.flow,
            "token_type": token.token_type,
            "expires_at": expires_at,
            "scopes": scopes,
        })),
    })
}

async fn select_endpoints(
    card: &a2a::AgentCard,
    scheme_name: Option<&str>,
    requested_flow: Option<A2aOAuthFlowKind>,
    policy: A2aNetworkPolicy,
) -> Result<OAuthEndpoints, AgentError> {
    let schemes = card
        .security_schemes
        .as_ref()
        .ok_or_else(|| AgentError::bad_request("Agent Card 未声明 OAuth/OIDC 安全方案"))?;
    let selected = if let Some(name) = scheme_name {
        schemes
            .get(name)
            .map(|scheme| (name.to_owned(), scheme))
            .ok_or_else(|| AgentError::bad_request("Agent Card 中不存在所选安全方案"))?
    } else {
        schemes
            .iter()
            .find(|(_, scheme)| {
                matches!(
                    scheme,
                    a2a::SecurityScheme::OAuth2(_) | a2a::SecurityScheme::OpenIdConnect(_)
                )
            })
            .map(|(name, scheme)| (name.clone(), scheme))
            .ok_or_else(|| AgentError::bad_request("Agent Card 未声明 OAuth/OIDC 安全方案"))?
    };
    match selected.1 {
        a2a::SecurityScheme::OAuth2(scheme) => match &scheme.flows {
            a2a::OAuthFlows::AuthorizationCode(flow) => {
                if requested_flow == Some(A2aOAuthFlowKind::DeviceCode) {
                    return Err(AgentError::bad_request("所选 OAuth 安全方案不支持 Device Code"));
                }
                let authorization_url = validated_endpoint(&flow.authorization_url, policy).await?;
                let token_url = validated_endpoint(&flow.token_url, policy).await?;
                Ok(OAuthEndpoints {
                    auth_kind: A2aAuthKind::OAuth2,
                    scheme_name: selected.0,
                    authorization_url: Some(authorization_url),
                    token_url,
                    device_authorization_url: None,
                    scopes: flow.scopes.keys().cloned().collect(),
                    oidc_issuer: None,
                    oidc_jwks_url: None,
                })
            }
            a2a::OAuthFlows::DeviceCode(flow) => {
                if requested_flow == Some(A2aOAuthFlowKind::AuthorizationCode) {
                    return Err(AgentError::bad_request("所选 OAuth 安全方案不支持 Authorization Code"));
                }
                let device_authorization_url = validated_endpoint(&flow.device_authorization_url, policy).await?;
                let token_url = validated_endpoint(&flow.token_url, policy).await?;
                Ok(OAuthEndpoints {
                    auth_kind: A2aAuthKind::OAuth2,
                    scheme_name: selected.0,
                    authorization_url: None,
                    token_url,
                    device_authorization_url: Some(device_authorization_url),
                    scopes: flow.scopes.keys().cloned().collect(),
                    oidc_issuer: None,
                    oidc_jwks_url: None,
                })
            }
            a2a::OAuthFlows::ClientCredentials(flow) => {
                if requested_flow.is_some_and(|flow| flow != A2aOAuthFlowKind::ClientCredentials) {
                    return Err(AgentError::bad_request("所选 OAuth 安全方案仅支持 Client Credentials"));
                }
                let token_url = validated_endpoint(&flow.token_url, policy).await?;
                Ok(OAuthEndpoints {
                    auth_kind: A2aAuthKind::OAuth2,
                    scheme_name: selected.0,
                    authorization_url: None,
                    token_url,
                    device_authorization_url: None,
                    scopes: flow.scopes.keys().cloned().collect(),
                    oidc_issuer: None,
                    oidc_jwks_url: None,
                })
            }
            _ => Err(AgentError::bad_request(
                "TjuaeUI 支持 OAuth Authorization Code + PKCE、Device Code 和 Client Credentials",
            )),
        },
        a2a::SecurityScheme::OpenIdConnect(scheme) => {
            if requested_flow == Some(A2aOAuthFlowKind::ClientCredentials) {
                return Err(AgentError::bad_request(
                    "OIDC 安全方案不支持 Client Credentials；请使用 OAuth2 clientCredentials flow",
                ));
            }
            let configuration_url = validated_endpoint(&scheme.open_id_connect_url, policy).await?;
            let configuration: OidcConfiguration = get_json(&configuration_url, policy).await?;
            let issuer_url = validated_endpoint(&configuration.issuer, policy).await?;
            let issuer = issuer_url.to_string();
            if issuer != configuration.issuer {
                return Err(AgentError::bad_gateway(
                    "OIDC discovery issuer 必须是规范化后的绝对 URL",
                ));
            }
            let authorization_url = validated_endpoint(&configuration.authorization_endpoint, policy).await?;
            let token_url = validated_endpoint(&configuration.token_endpoint, policy).await?;
            let jwks_url = validated_endpoint(&configuration.jwks_uri, policy).await?;
            let device_authorization_url = match configuration.device_authorization_endpoint {
                Some(value) => Some(validated_endpoint(&value, policy).await?),
                None => None,
            };
            if requested_flow == Some(A2aOAuthFlowKind::DeviceCode) && device_authorization_url.is_none() {
                return Err(AgentError::bad_request(
                    "OIDC Provider 未声明 Device Authorization Endpoint",
                ));
            }
            Ok(OAuthEndpoints {
                auth_kind: A2aAuthKind::Oidc,
                scheme_name: selected.0,
                authorization_url: Some(authorization_url),
                token_url,
                device_authorization_url,
                scopes: configuration.scopes_supported,
                oidc_issuer: Some(issuer),
                oidc_jwks_url: Some(jwks_url),
            })
        }
        _ => Err(AgentError::bad_request("所选 Agent Card 安全方案不是 OAuth/OIDC")),
    }
}

async fn validate_oidc_id_token(
    id_token: &str,
    client_id: &str,
    issuer: &str,
    jwks_url: &Url,
    expected_nonce: Option<&str>,
    policy: A2aNetworkPolicy,
) -> Result<(), AgentError> {
    if id_token.len() > 64 * 1024 {
        return Err(AgentError::bad_gateway("OIDC id_token 超过 64 KiB 限制"));
    }
    let header = decode_header(id_token).map_err(|_| AgentError::unauthorized("OIDC id_token Header 无效"))?;
    if !matches!(
        header.alg,
        Algorithm::RS256
            | Algorithm::RS384
            | Algorithm::RS512
            | Algorithm::PS256
            | Algorithm::PS384
            | Algorithm::PS512
            | Algorithm::ES256
            | Algorithm::ES384
            | Algorithm::EdDSA
    ) {
        return Err(AgentError::unauthorized("OIDC id_token 不允许使用对称签名算法"));
    }
    let kid = header
        .kid
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AgentError::unauthorized("OIDC id_token Header 缺少 kid"))?;
    let jwks: JwkSet = get_json(jwks_url, policy).await?;
    if jwks.keys.len() > 128 {
        return Err(AgentError::bad_gateway("OIDC JWKS 密钥数量超过限制"));
    }
    let jwk = jwks
        .find(kid)
        .ok_or_else(|| AgentError::unauthorized("OIDC id_token 的 kid 不在 Provider JWKS 中"))?;
    if matches!(jwk.common.public_key_use, Some(PublicKeyUse::Encryption)) {
        return Err(AgentError::unauthorized("OIDC JWK 不允许用于签名验证"));
    }
    if jwk
        .common
        .key_operations
        .as_ref()
        .is_some_and(|operations| !operations.contains(&KeyOperations::Verify))
    {
        return Err(AgentError::unauthorized("OIDC JWK 未授权 verify 操作"));
    }
    if jwk
        .common
        .key_algorithm
        .is_some_and(|algorithm| algorithm.to_string() != format!("{:?}", header.alg))
    {
        return Err(AgentError::unauthorized("OIDC id_token 算法与 JWK 声明不匹配"));
    }
    let decoding_key = DecodingKey::from_jwk(jwk).map_err(|_| AgentError::unauthorized("OIDC JWK 无法用于签名验证"))?;
    let mut validation = Validation::new(header.alg);
    validation.leeway = 60;
    validation.validate_nbf = true;
    validation.set_required_spec_claims(&["exp", "iss", "sub", "aud", "iat"]);
    validation.set_audience(&[client_id]);
    validation.set_issuer(&[issuer]);
    let token = decode::<serde_json::Value>(id_token, &decoding_key, &validation)
        .map_err(|_| AgentError::unauthorized("OIDC id_token 签名或标准 Claims 验证失败"))?;

    if let Some(expected_nonce) = expected_nonce {
        let nonce = token
            .claims
            .get("nonce")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| AgentError::unauthorized("OIDC id_token 缺少 nonce"))?;
        if !constant_time_eq(nonce, expected_nonce) {
            return Err(AgentError::unauthorized("OIDC id_token nonce 不匹配"));
        }
    }
    let audiences = token.claims.get("aud");
    let multiple_audiences = audiences
        .and_then(serde_json::Value::as_array)
        .is_some_and(|values| values.len() > 1);
    if multiple_audiences && token.claims.get("azp").and_then(serde_json::Value::as_str) != Some(client_id) {
        return Err(AgentError::unauthorized(
            "OIDC id_token 包含多个 audience 时 azp 必须匹配 client_id",
        ));
    }
    if let Some(azp) = token.claims.get("azp").and_then(serde_json::Value::as_str)
        && azp != client_id
    {
        return Err(AgentError::unauthorized("OIDC id_token azp 不匹配 client_id"));
    }
    let issued_at = token
        .claims
        .get("iat")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| AgentError::unauthorized("OIDC id_token iat 无效"))?;
    let now = (tjuaeui_common::now_ms().max(0) as u64) / 1000;
    if issued_at > now.saturating_add(60) {
        return Err(AgentError::unauthorized("OIDC id_token iat 位于未来"));
    }
    Ok(())
}

async fn validated_endpoint(value: &str, policy: A2aNetworkPolicy) -> Result<Url, AgentError> {
    let url = Url::parse(value.trim()).map_err(|_| AgentError::bad_request("OAuth/OIDC Endpoint URL 无效"))?;
    resolve_network_target(&url, policy).await?;
    Ok(url)
}

fn validate_redirect_uri(value: &str) -> Result<(), AgentError> {
    if value.len() > 2048 {
        return Err(AgentError::bad_request("OAuth redirect_uri 过长"));
    }
    let url = Url::parse(value).map_err(|_| AgentError::bad_request("OAuth redirect_uri 无效"))?;
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(AgentError::bad_request("OAuth redirect_uri 不得包含凭据或片段"));
    }
    match url.scheme() {
        "https" => Ok(()),
        "http"
            if url.host_str().is_some_and(|host| {
                host.eq_ignore_ascii_case("localhost")
                    || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
            }) =>
        {
            Ok(())
        }
        scheme if !scheme.is_empty() && scheme != "http" => Ok(()),
        _ => Err(AgentError::bad_request(
            "OAuth redirect_uri 必须使用 HTTPS、loopback HTTP 或已注册的自定义 scheme",
        )),
    }
}

fn validate_start_request(request: &StartA2aOAuthRequest) -> Result<(), AgentError> {
    let client_id = request.client_id.trim();
    if client_id.is_empty() || client_id.len() > 512 {
        return Err(AgentError::bad_request("OAuth client_id 不能为空且不得超过 512 字节"));
    }
    if request
        .client_secret
        .as_deref()
        .is_some_and(|value| value.len() > 16 * 1024)
    {
        return Err(AgentError::bad_request("OAuth client_secret 不得超过 16 KiB"));
    }
    if request.scopes.len() > 64
        || request
            .scopes
            .iter()
            .any(|scope| scope.is_empty() || scope.len() > 256 || scope.chars().any(char::is_whitespace))
    {
        return Err(AgentError::bad_request("OAuth scopes 数量或格式无效"));
    }
    Ok(())
}

async fn pinned_client(url: &Url, policy: A2aNetworkPolicy) -> Result<Client, AgentError> {
    let addresses = resolve_network_target(url, policy).await?;
    let host = url
        .host_str()
        .ok_or_else(|| AgentError::bad_request("OAuth/OIDC Endpoint 缺少主机名"))?;
    tjuaeui_runtime::apply_network_proxy_to_http_client(
        Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .resolve_to_addrs(host, &addresses)
            .user_agent(concat!("tjuaecore-a2a-oauth/", env!("CARGO_PKG_VERSION"))),
    )
    .build()
    .map_err(|_| AgentError::internal("无法创建 A2A OAuth 网络客户端"))
}

async fn get_json<T: for<'de> Deserialize<'de>>(url: &Url, policy: A2aNetworkPolicy) -> Result<T, AgentError> {
    let client = pinned_client(url, policy).await?;
    let response = client
        .get(url.clone())
        .header(ACCEPT, "application/json")
        .send()
        .await
        .map_err(|_| AgentError::bad_gateway("无法读取 OIDC 配置"))?;
    let bytes = checked_response_bytes(response, "OIDC 配置").await?;
    serde_json::from_slice(&bytes).map_err(|_| AgentError::bad_gateway("OIDC 配置格式无效"))
}

async fn post_form_json<T: for<'de> Deserialize<'de>>(
    url: &Url,
    policy: A2aNetworkPolicy,
    form: &[(&str, String)],
) -> Result<T, AgentError> {
    let response = post_form(url, policy, form).await?;
    let bytes = checked_response_bytes(response, "OAuth 响应").await?;
    serde_json::from_slice(&bytes).map_err(|_| AgentError::bad_gateway("OAuth 响应格式无效"))
}

enum TokenExchangeError {
    Pending,
    SlowDown,
    Agent(AgentError),
}

async fn post_token(
    url: &Url,
    policy: A2aNetworkPolicy,
    form: &[(&str, String)],
) -> Result<OAuthTokenResponse, TokenExchangeError> {
    let response = post_form(url, policy, form).await.map_err(TokenExchangeError::Agent)?;
    let status = response.status();
    let bytes = read_limited(response, "OAuth Token 响应")
        .await
        .map_err(TokenExchangeError::Agent)?;
    if !status.is_success() {
        let error = serde_json::from_slice::<OAuthErrorResponse>(&bytes)
            .ok()
            .map(|value| value.error)
            .unwrap_or_else(|| format!("http_{}", status.as_u16()));
        return match error.as_str() {
            "authorization_pending" => Err(TokenExchangeError::Pending),
            "slow_down" => Err(TokenExchangeError::SlowDown),
            "access_denied" => Err(TokenExchangeError::Agent(AgentError::forbidden(
                "OAuth 授权被用户或服务拒绝",
            ))),
            "expired_token" | "invalid_grant" => Err(TokenExchangeError::Agent(AgentError::unauthorized(
                "OAuth 授权码或设备码已失效",
            ))),
            "invalid_client" => Err(TokenExchangeError::Agent(AgentError::unauthorized(
                "OAuth client_id 或 client_secret 无效",
            ))),
            _ => Err(TokenExchangeError::Agent(AgentError::bad_gateway(format!(
                "OAuth Token Endpoint 返回错误：{error}"
            )))),
        };
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| TokenExchangeError::Agent(AgentError::bad_gateway("OAuth Token 响应格式无效")))
}

async fn post_form(
    url: &Url,
    policy: A2aNetworkPolicy,
    form: &[(&str, String)],
) -> Result<reqwest::Response, AgentError> {
    let client = pinned_client(url, policy).await?;
    let body = {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        for (key, value) in form {
            serializer.append_pair(key, value);
        }
        serializer.finish()
    };
    client
        .post(url.clone())
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(ACCEPT, "application/json")
        .body(body)
        .send()
        .await
        .map_err(|_| AgentError::bad_gateway("OAuth Endpoint 请求失败"))
}

async fn checked_response_bytes(response: reqwest::Response, label: &str) -> Result<Vec<u8>, AgentError> {
    let status = response.status();
    let bytes = read_limited(response, label).await?;
    match status {
        StatusCode::UNAUTHORIZED => Err(AgentError::unauthorized(format!("{label}需要认证"))),
        StatusCode::FORBIDDEN => Err(AgentError::forbidden(format!("{label}拒绝访问"))),
        StatusCode::TOO_MANY_REQUESTS => Err(AgentError::RateLimited),
        status if status.is_redirection() => Err(AgentError::bad_gateway(format!("{label}发生未经允许的重定向"))),
        status if !status.is_success() => Err(AgentError::bad_gateway(format!("{label}返回 HTTP {}", status.as_u16()))),
        _ => Ok(bytes),
    }
}

async fn read_limited(mut response: reqwest::Response, label: &str) -> Result<Vec<u8>, AgentError> {
    if response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > MAX_OAUTH_RESPONSE_BYTES)
    {
        return Err(AgentError::bad_gateway(format!("{label}超过 256 KiB 限制")));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| AgentError::bad_gateway(format!("读取{label}失败")))?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_OAUTH_RESPONSE_BYTES {
            return Err(AgentError::bad_gateway(format!("{label}超过 256 KiB 限制")));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn normalized_secret(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    left.len() == right.len()
        && left
            .as_bytes()
            .iter()
            .zip(right.as_bytes())
            .fold(0_u8, |diff, (left, right)| diff | (left ^ right))
            == 0
}

fn random_urlsafe(bytes: usize) -> Result<String, AgentError> {
    let mut random = vec![0_u8; bytes];
    getrandom::getrandom(&mut random).map_err(|_| AgentError::internal("无法生成 OAuth 安全随机数"))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random))
}

fn default_token_type() -> String {
    "Bearer".to_owned()
}

const fn default_device_interval() -> u64 {
    5
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use jsonwebtoken::{EncodingKey, Header, encode};
    use rcgen::{KeyPair, PKCS_ED25519};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::oneshot;

    use super::*;

    async fn spawn_json_endpoint(initial_body: &str) -> (Url, Arc<Mutex<String>>, oneshot::Receiver<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let body = Arc::new(Mutex::new(initial_body.to_owned()));
        let server_body = body.clone();
        let (request_tx, request_rx) = oneshot::channel();
        tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut request = vec![0_u8; 64 * 1024];
            let bytes_read = stream.read(&mut request).await.unwrap_or_default();
            request.truncate(bytes_read);
            let _ = request_tx.send(String::from_utf8_lossy(&request).into_owned());
            let body = server_body.lock().unwrap().clone();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.shutdown().await;
        });
        (Url::parse(&format!("http://{address}/")).unwrap(), body, request_rx)
    }

    fn test_policy() -> A2aNetworkPolicy {
        A2aNetworkPolicy {
            allow_insecure: true,
            allow_private_network: true,
        }
    }

    fn test_card(scheme_name: &str, scheme: a2a::SecurityScheme) -> a2a::AgentCard {
        a2a::AgentCard {
            name: "OAuth Test Agent".to_owned(),
            description: "test".to_owned(),
            version: "1.0.0".to_owned(),
            supported_interfaces: vec![a2a::AgentInterface::new("http://127.0.0.1:9/a2a", "JSONRPC")],
            capabilities: a2a::AgentCapabilities {
                streaming: Some(true),
                push_notifications: Some(false),
                extensions: None,
                extended_agent_card: None,
            },
            default_input_modes: vec!["text/plain".to_owned()],
            default_output_modes: vec!["text/plain".to_owned()],
            skills: Vec::new(),
            provider: None,
            documentation_url: None,
            icon_url: None,
            security_schemes: Some(HashMap::from([(scheme_name.to_owned(), scheme)])),
            security_requirements: None,
            signatures: None,
        }
    }

    fn start_request(flow: A2aOAuthFlowKind) -> StartA2aOAuthRequest {
        StartA2aOAuthRequest {
            scheme_name: None,
            client_id: "desktop-client".to_owned(),
            client_secret: None,
            redirect_uri: (flow == A2aOAuthFlowKind::AuthorizationCode)
                .then(|| "http://127.0.0.1:8756/callback".to_owned()),
            flow: Some(flow),
            scopes: vec!["tasks.read".to_owned()],
        }
    }

    #[test]
    fn redirect_uri_policy_accepts_only_safe_http_loopback() {
        assert!(validate_redirect_uri("http://127.0.0.1:8756/callback").is_ok());
        assert!(validate_redirect_uri("http://localhost:8756/callback").is_ok());
        assert!(validate_redirect_uri("http://example.com/callback").is_err());
        assert!(validate_redirect_uri("https://example.com/callback").is_ok());
        assert!(validate_redirect_uri("tjuae://oauth/callback").is_ok());
    }

    #[test]
    fn oauth_bundle_does_not_decode_arbitrary_secret() {
        assert!(decode_oauth_bundle("plain-access-token").is_none());
    }

    #[tokio::test]
    async fn authorization_code_start_builds_pkce_state_and_redirect_parameters() {
        let card = test_card(
            "oauth",
            a2a::SecurityScheme::OAuth2(a2a::OAuth2SecurityScheme {
                flows: a2a::OAuthFlows::AuthorizationCode(a2a::AuthorizationCodeOAuthFlow {
                    authorization_url: "http://127.0.0.1:9/authorize".to_owned(),
                    token_url: "http://127.0.0.1:9/token".to_owned(),
                    scopes: HashMap::from([("tasks.read".to_owned(), "Read tasks".to_owned())]),
                    refresh_url: None,
                    pkce_required: Some(true),
                }),
                description: None,
                oauth2_metadata_url: None,
            }),
        );
        let response = A2aOAuthCoordinator::default()
            .start(
                "agent",
                &card,
                start_request(A2aOAuthFlowKind::AuthorizationCode),
                test_policy(),
            )
            .await
            .unwrap();
        let authorization_url = Url::parse(response.authorization_url.as_deref().unwrap()).unwrap();
        let query: HashMap<_, _> = authorization_url.query_pairs().into_owned().collect();

        assert_eq!(query.get("response_type").map(String::as_str), Some("code"));
        assert_eq!(query.get("code_challenge_method").map(String::as_str), Some("S256"));
        assert!(query.get("code_challenge").is_some_and(|value| value.len() >= 43));
        assert_eq!(query.get("state"), Some(&response.state));
        assert_eq!(
            query.get("redirect_uri").map(String::as_str),
            Some("http://127.0.0.1:8756/callback")
        );
    }

    #[tokio::test]
    async fn device_code_flow_exchanges_token_and_persists_refreshable_bundle() {
        let device_response = serde_json::json!({
            "device_code": "device-secret",
            "user_code": "ABCD-EFGH",
            "verification_uri": "https://issuer.example/activate",
            "expires_in": 600,
            "interval": 1
        })
        .to_string();
        let (device_url, _device_body, device_request) = spawn_json_endpoint(&device_response).await;
        let token_response = serde_json::json!({
            "access_token": "oauth-access",
            "refresh_token": "oauth-refresh",
            "token_type": "Bearer",
            "expires_in": 3600,
            "scope": "tasks.read"
        })
        .to_string();
        let (token_url, _token_body, token_request) = spawn_json_endpoint(&token_response).await;
        let card = test_card(
            "oauth-device",
            a2a::SecurityScheme::OAuth2(a2a::OAuth2SecurityScheme {
                flows: a2a::OAuthFlows::DeviceCode(a2a::DeviceCodeOAuthFlow {
                    device_authorization_url: device_url.to_string(),
                    token_url: token_url.to_string(),
                    scopes: HashMap::from([("tasks.read".to_owned(), "Read tasks".to_owned())]),
                    refresh_url: None,
                }),
                description: None,
                oauth2_metadata_url: None,
            }),
        );
        let coordinator = A2aOAuthCoordinator::default();
        let started = coordinator
            .start(
                "agent",
                &card,
                start_request(A2aOAuthFlowKind::DeviceCode),
                test_policy(),
            )
            .await
            .unwrap();
        coordinator.pending.get_mut(&started.state).unwrap().next_poll_at = 0;
        let credential = coordinator
            .complete(
                "agent",
                CompleteA2aOAuthRequest {
                    state: started.state,
                    code: None,
                },
            )
            .await
            .unwrap();
        let bundle = decode_oauth_bundle(credential.secret.as_deref().unwrap()).unwrap();

        assert_eq!(credential.kind, A2aAuthKind::OAuth2);
        assert_eq!(bundle.access_token, "oauth-access");
        assert_eq!(bundle.refresh_token.as_deref(), Some("oauth-refresh"));
        assert!(device_request.await.unwrap().contains("client_id=desktop-client"));
        let token_request = token_request.await.unwrap();
        assert!(token_request.contains("device_code=device-secret"));
        assert!(token_request.contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"));
    }

    #[tokio::test]
    async fn client_credentials_flow_exchanges_and_persists_renewable_client_identity() {
        let token_response = serde_json::json!({
            "access_token": "service-access",
            "token_type": "Bearer",
            "expires_in": 3600,
            "scope": "tasks.read"
        })
        .to_string();
        let (token_url, _token_body, token_request) = spawn_json_endpoint(&token_response).await;
        let card = test_card(
            "oauth-service",
            a2a::SecurityScheme::OAuth2(a2a::OAuth2SecurityScheme {
                flows: a2a::OAuthFlows::ClientCredentials(a2a::ClientCredentialsOAuthFlow {
                    token_url: token_url.to_string(),
                    scopes: HashMap::from([("tasks.read".to_owned(), "Read tasks".to_owned())]),
                    refresh_url: None,
                }),
                description: None,
                oauth2_metadata_url: None,
            }),
        );
        let coordinator = A2aOAuthCoordinator::default();
        let mut request = start_request(A2aOAuthFlowKind::ClientCredentials);
        request.client_secret = Some("service-secret".to_owned());
        let started = coordinator.start("agent", &card, request, test_policy()).await.unwrap();
        let credential = coordinator
            .complete(
                "agent",
                CompleteA2aOAuthRequest {
                    state: started.state,
                    code: None,
                },
            )
            .await
            .unwrap();
        let bundle = decode_oauth_bundle(credential.secret.as_deref().unwrap()).unwrap();

        assert_eq!(bundle.access_token, "service-access");
        assert!(bundle.client_credentials);
        assert_eq!(bundle.client_secret.as_deref(), Some("service-secret"));
        let token_request = token_request.await.unwrap();
        assert!(token_request.contains("grant_type=client_credentials"));
        assert!(token_request.contains("client_id=desktop-client"));
        assert!(token_request.contains("client_secret=service-secret"));
        assert!(token_request.contains("scope=tasks.read"));
    }

    #[tokio::test]
    async fn oidc_authorization_code_validates_signed_id_token_and_nonce() {
        let signing_key = KeyPair::generate_for(&PKCS_ED25519).unwrap();
        let public_key = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signing_key.public_key_raw());
        let jwks = serde_json::json!({
            "keys": [{
                "kty": "OKP",
                "crv": "Ed25519",
                "x": public_key,
                "kid": "oidc-test-key",
                "use": "sig",
                "key_ops": ["verify"],
                "alg": "EdDSA"
            }]
        })
        .to_string();
        let (jwks_url, _jwks_body, _jwks_request) = spawn_json_endpoint(&jwks).await;
        let (token_url, token_body, token_request) = spawn_json_endpoint("{}").await;
        let issuer = "http://127.0.0.1:9/".to_owned();
        let discovery = serde_json::json!({
            "issuer": issuer,
            "authorization_endpoint": "http://127.0.0.1:9/authorize",
            "token_endpoint": token_url,
            "jwks_uri": jwks_url,
            "scopes_supported": ["openid", "tasks.read"]
        })
        .to_string();
        let (discovery_url, _discovery_body, _discovery_request) = spawn_json_endpoint(&discovery).await;
        let card = test_card(
            "oidc",
            a2a::SecurityScheme::OpenIdConnect(a2a::OpenIdConnectSecurityScheme {
                open_id_connect_url: discovery_url.to_string(),
                description: None,
            }),
        );
        let coordinator = A2aOAuthCoordinator::default();
        let started = coordinator
            .start(
                "agent",
                &card,
                start_request(A2aOAuthFlowKind::AuthorizationCode),
                test_policy(),
            )
            .await
            .unwrap();
        let authorization_url = Url::parse(started.authorization_url.as_deref().unwrap()).unwrap();
        let nonce = authorization_url
            .query_pairs()
            .find(|(name, _)| name == "nonce")
            .map(|(_, value)| value.into_owned())
            .unwrap();
        let now = (tjuaeui_common::now_ms() as u64) / 1000;
        let claims = serde_json::json!({
            "iss": issuer,
            "sub": "user-123",
            "aud": "desktop-client",
            "exp": now + 600,
            "iat": now,
            "nonce": nonce
        });
        let mut header = Header::new(Algorithm::EdDSA);
        header.kid = Some("oidc-test-key".to_owned());
        let id_token = encode(
            &header,
            &claims,
            &EncodingKey::from_ed_pem(signing_key.serialize_pem().as_bytes()).unwrap(),
        )
        .unwrap();
        *token_body.lock().unwrap() = serde_json::json!({
            "access_token": "oidc-access",
            "id_token": id_token,
            "token_type": "Bearer",
            "expires_in": 3600,
            "scope": "openid tasks.read"
        })
        .to_string();

        let credential = coordinator
            .complete(
                "agent",
                CompleteA2aOAuthRequest {
                    state: started.state,
                    code: Some("authorization-code".to_owned()),
                },
            )
            .await
            .unwrap();
        assert_eq!(credential.kind, A2aAuthKind::Oidc);
        assert_eq!(
            decode_oauth_bundle(credential.secret.as_deref().unwrap())
                .unwrap()
                .access_token,
            "oidc-access"
        );
        let token_request = token_request.await.unwrap();
        assert!(token_request.contains("code=authorization-code"));
        assert!(token_request.contains("code_verifier="));
    }

    #[tokio::test]
    async fn oidc_rejects_id_token_signed_by_a_key_outside_provider_jwks() {
        let trusted_key = KeyPair::generate_for(&PKCS_ED25519).unwrap();
        let attacker_key = KeyPair::generate_for(&PKCS_ED25519).unwrap();
        let jwks = serde_json::json!({
            "keys": [{
                "kty": "OKP",
                "crv": "Ed25519",
                "x": base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(trusted_key.public_key_raw()),
                "kid": "shared-kid",
                "use": "sig",
                "key_ops": ["verify"],
                "alg": "EdDSA"
            }]
        })
        .to_string();
        let (jwks_url, _body, _request) = spawn_json_endpoint(&jwks).await;
        let issuer = "http://127.0.0.1:9/";
        let now = (tjuaeui_common::now_ms() as u64) / 1000;
        let mut header = Header::new(Algorithm::EdDSA);
        header.kid = Some("shared-kid".to_owned());
        let token = encode(
            &header,
            &serde_json::json!({
                "iss": issuer,
                "sub": "user-123",
                "aud": "desktop-client",
                "exp": now + 600,
                "iat": now,
                "nonce": "expected-nonce"
            }),
            &EncodingKey::from_ed_pem(attacker_key.serialize_pem().as_bytes()).unwrap(),
        )
        .unwrap();

        assert!(
            validate_oidc_id_token(
                &token,
                "desktop-client",
                issuer,
                &jwks_url,
                Some("expected-nonce"),
                test_policy(),
            )
            .await
            .is_err()
        );
    }
}
