use std::collections::HashSet;
use std::time::Duration;

use base64::Engine;
use reqwest::header::{
    ACCEPT, AUTHORIZATION, CACHE_CONTROL, CONTENT_LENGTH, COOKIE, ETAG, HeaderMap, HeaderName, HeaderValue,
    IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED, LOCATION,
};
use reqwest::{Client, StatusCode};
use sha2::{Digest, Sha256};
use tjuaeui_api_types::{
    A2aAuthKind, A2aCompatibilityMode, A2aCredentialInput, A2aCredentialLocation, DiscoverA2aAgentRequest,
    DiscoverA2aAgentResponse,
};
use url::Url;

use crate::error::AgentError;
use crate::protocol::a2a::card::{A2aCardSource, CardParseOptions, MAX_AGENT_CARD_BYTES, parse_agent_card};

use super::mapper::card_summary;
use super::security::{
    A2aNetworkPolicy, apply_mtls_to_reqwest_builder, base_url, normalize_card_url, resolve_network_target, same_origin,
    validate_network_target,
};

const MAX_REDIRECTS: usize = 3;
const DEFAULT_CACHE_TTL_MS: i64 = 5 * 60 * 1000;

#[derive(Debug, Clone, Default)]
pub(crate) struct A2aCardDiscovery;

#[derive(Debug, Clone, Default)]
pub(crate) struct CardCacheValidators {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct DiscoveredA2aCard {
    pub response: DiscoverA2aAgentResponse,
    pub raw_card_json: String,
    pub normalized_card_json: String,
    pub extended_card_json: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub cache_expires_at: i64,
    pub fetched_at: i64,
    pub card_hash: String,
}

impl A2aCardDiscovery {
    pub(crate) async fn discover(
        &self,
        request: &DiscoverA2aAgentRequest,
        validators: Option<&CardCacheValidators>,
    ) -> Result<DiscoveredA2aCard, AgentError> {
        let policy = A2aNetworkPolicy {
            allow_insecure: request.allow_insecure,
            allow_private_network: request.allow_private_network,
        };
        let initial_url = normalize_card_url(&request.url, policy)?;
        let fetched_at = tjuaeui_common::now_ms();
        let credentials = effective_request_credentials(request);
        let fetched = fetch_card(initial_url, policy, &credentials, validators).await?;
        let options = CardParseOptions {
            allow_v03: request.compatibility_mode == A2aCompatibilityMode::V03,
            supported_extensions: HashSet::new(),
        };
        let parsed = parse_agent_card(&fetched.body, &options).map_err(protocol_error_to_agent)?;
        let summary = card_summary(&parsed.card)?;
        let selected_url = Url::parse(&summary.selected_interface_url)
            .map_err(|_| AgentError::bad_request("Agent Card 的接口地址不是有效 URL"))?;
        validate_network_target(&selected_url, policy).await?;
        let base = base_url(&fetched.final_url)?;
        let requires_origin_confirmation = !same_origin(&fetched.final_url, &selected_url);
        let mut warnings = fetched.warnings;
        if requires_origin_confirmation {
            warnings.push("Agent 接口与 Agent Card 不同源，保存前需要确认信任该来源".to_owned());
        }
        let normalized_card_json = serde_json::to_string(&parsed.card)
            .map_err(|error| AgentError::internal(format!("编码 Agent Card 失败：{error}")))?;
        let raw_card_json = String::from_utf8(fetched.body.clone())
            .map_err(|_| AgentError::bad_gateway("Agent Card 不是 UTF-8 JSON"))?;
        let card_hash = hex::encode(Sha256::digest(normalized_card_json.as_bytes()));

        Ok(DiscoveredA2aCard {
            response: DiscoverA2aAgentResponse {
                card_url: fetched.final_url.to_string(),
                base_url: base.to_string(),
                compatibility_mode: match parsed.source {
                    A2aCardSource::V1 => A2aCompatibilityMode::V1,
                    A2aCardSource::V03Compatibility => A2aCompatibilityMode::V03,
                },
                requires_authentication: parsed.card.security_requirements.is_some(),
                requires_origin_confirmation,
                card: summary,
                warnings,
            },
            raw_card_json,
            normalized_card_json,
            extended_card_json: None,
            etag: fetched.etag,
            last_modified: fetched.last_modified,
            cache_expires_at: fetched_at + fetched.max_age_ms.unwrap_or(DEFAULT_CACHE_TTL_MS),
            fetched_at,
            card_hash,
        })
    }
}

struct FetchedCard {
    final_url: Url,
    body: Vec<u8>,
    etag: Option<String>,
    last_modified: Option<String>,
    max_age_ms: Option<i64>,
    warnings: Vec<String>,
}

fn build_client(
    url: &Url,
    addresses: &[std::net::SocketAddr],
    credentials: &[A2aCredentialInput],
) -> Result<Client, AgentError> {
    let host = url
        .host_str()
        .ok_or_else(|| AgentError::bad_request("A2A 地址缺少主机名"))?;
    if credentials.iter().any(|value| value.kind == A2aAuthKind::Mtls) && url.scheme() != "https" {
        return Err(AgentError::bad_request("mTLS 只能用于 HTTPS A2A 地址"));
    }
    let builder = apply_mtls_to_reqwest_builder(
        Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .resolve_to_addrs(host, addresses)
            .user_agent(concat!("tjuaecore-a2a/", env!("CARGO_PKG_VERSION"))),
        credentials,
    )?;
    tjuaeui_runtime::apply_network_proxy_to_http_client(builder)
        .build()
        .map_err(|_| AgentError::internal("无法创建 A2A 网络客户端"))
}

async fn fetch_card(
    initial_url: Url,
    policy: A2aNetworkPolicy,
    credentials: &[A2aCredentialInput],
    validators: Option<&CardCacheValidators>,
) -> Result<FetchedCard, AgentError> {
    let credential_origin = initial_url.clone();
    let mut current = initial_url;
    let mut warnings = Vec::new();

    for redirect_count in 0..=MAX_REDIRECTS {
        let addresses = resolve_network_target(&current, policy).await?;
        let same_credential_origin = same_origin(&credential_origin, &current);
        let client = build_client(
            &current,
            &addresses,
            if same_credential_origin { credentials } else { &[] },
        )?;
        let request_url = if same_credential_origin {
            authenticated_url(&current, credentials)?
        } else {
            current.clone()
        };
        let mut builder = client
            .get(request_url)
            .header(ACCEPT, "application/json, application/*+json");
        if same_credential_origin {
            builder = apply_credentials(builder, credentials)?;
            if let Some(validators) = validators {
                if let Some(etag) = validators.etag.as_deref() {
                    builder = builder.header(IF_NONE_MATCH, etag);
                }
                if let Some(last_modified) = validators.last_modified.as_deref() {
                    builder = builder.header(IF_MODIFIED_SINCE, last_modified);
                }
            }
        }
        let mut response = builder
            .send()
            .await
            .map_err(|_| AgentError::bad_gateway("无法连接 A2A Agent Card 地址"))?;

        // HTTP 304 belongs to the 3xx class but is a cache-validation result,
        // not a redirect and therefore legitimately has no Location header.
        if response.status() == StatusCode::NOT_MODIFIED {
            return Err(AgentError::conflict("A2A Agent Card 未变化"));
        }

        if response.status().is_redirection() {
            if redirect_count == MAX_REDIRECTS {
                return Err(AgentError::bad_gateway("A2A Agent Card 重定向次数过多"));
            }
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| AgentError::bad_gateway("A2A 重定向缺少有效 Location"))?;
            let next = current
                .join(location)
                .map_err(|_| AgentError::bad_gateway("A2A 重定向地址无效"))?;
            if !same_origin(&current, &next) {
                warnings.push("Agent Card 发生跨来源重定向，凭据未转发".to_owned());
            }
            current = next;
            continue;
        }

        match response.status() {
            StatusCode::UNAUTHORIZED => return Err(AgentError::unauthorized("A2A Agent Card 需要认证")),
            StatusCode::FORBIDDEN => return Err(AgentError::forbidden("A2A Agent Card 拒绝访问")),
            StatusCode::TOO_MANY_REQUESTS => return Err(AgentError::RateLimited),
            status if !status.is_success() => {
                return Err(AgentError::bad_gateway(format!(
                    "A2A Agent Card 返回 HTTP {}",
                    status.as_u16()
                )));
            }
            _ => {}
        }

        reject_oversized_content_length(response.headers())?;
        let etag = header_string(response.headers(), ETAG);
        let last_modified = header_string(response.headers(), LAST_MODIFIED);
        let max_age_ms = parse_max_age_ms(response.headers());
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| AgentError::bad_gateway("读取 A2A Agent Card 失败"))?
        {
            if body.len().saturating_add(chunk.len()) > MAX_AGENT_CARD_BYTES {
                return Err(AgentError::bad_gateway("A2A Agent Card 超过 1 MiB 限制"));
            }
            body.extend_from_slice(&chunk);
        }
        return Ok(FetchedCard {
            final_url: current,
            body,
            etag,
            last_modified,
            max_age_ms,
            warnings,
        });
    }
    Err(AgentError::bad_gateway("A2A Agent Card 重定向失败"))
}

fn apply_credentials(
    mut builder: reqwest::RequestBuilder,
    credentials: &[A2aCredentialInput],
) -> Result<reqwest::RequestBuilder, AgentError> {
    let mut authorization_set = false;
    let mut cookies = Vec::new();
    for credential in credentials {
        let secret = credential.secret.as_deref().unwrap_or_default();
        builder = match credential.kind {
            A2aAuthKind::None => builder,
            A2aAuthKind::Bearer | A2aAuthKind::OAuth2 | A2aAuthKind::Oidc => {
                if authorization_set {
                    return Err(AgentError::bad_request("A2A 凭据包含多个互斥的 Authorization 方案"));
                }
                authorization_set = true;
                builder.header(AUTHORIZATION, format!("Bearer {secret}"))
            }
            A2aAuthKind::Basic => {
                if authorization_set {
                    return Err(AgentError::bad_request("A2A 凭据包含多个互斥的 Authorization 方案"));
                }
                authorization_set = true;
                let encoded = base64::engine::general_purpose::STANDARD.encode(secret);
                builder.header(AUTHORIZATION, format!("Basic {encoded}"))
            }
            A2aAuthKind::ApiKey if credential_location(credential) == A2aCredentialLocation::Query => builder,
            A2aAuthKind::ApiKey if credential_location(credential) == A2aCredentialLocation::Cookie => {
                let name = credential_name(credential, "Cookie API Key 需要名称")?;
                validate_cookie_component(name)?;
                validate_cookie_component(secret)?;
                cookies.push(format!("{name}={secret}"));
                builder
            }
            A2aAuthKind::ApiKey | A2aAuthKind::CustomHeader => {
                let header_name = credential
                    .header_name
                    .as_deref()
                    .ok_or_else(|| AgentError::bad_request("API Key 或自定义 Header 需要 header_name"))?;
                let name = HeaderName::from_bytes(header_name.as_bytes())
                    .map_err(|_| AgentError::bad_request("凭据 Header 名称无效"))?;
                let value = HeaderValue::from_str(secret).map_err(|_| AgentError::bad_request("凭据值包含非法字符"))?;
                builder.header(name, value)
            }
            A2aAuthKind::Mtls => builder,
        };
    }
    if !cookies.is_empty() {
        builder = builder.header(COOKIE, cookies.join("; "));
    }
    Ok(builder)
}

fn authenticated_url(url: &Url, credentials: &[A2aCredentialInput]) -> Result<Url, AgentError> {
    let mut authenticated = url.clone();
    for credential in credentials.iter().filter(|credential| {
        credential.kind == A2aAuthKind::ApiKey && credential_location(credential) == A2aCredentialLocation::Query
    }) {
        let name = credential_name(credential, "Query API Key 需要参数名称")?;
        if name.len() > 128 || name.bytes().any(|byte| byte.is_ascii_control()) {
            return Err(AgentError::bad_request("Query API Key 参数名称无效"));
        }
        if authenticated.query_pairs().any(|(key, _)| key == name) {
            return Err(AgentError::bad_request("Agent Card URL 已包含同名 Query API Key 参数"));
        }
        authenticated
            .query_pairs_mut()
            .append_pair(name, credential.secret.as_deref().unwrap_or_default());
    }
    Ok(authenticated)
}

fn effective_request_credentials(request: &DiscoverA2aAgentRequest) -> Vec<A2aCredentialInput> {
    if !request.credentials.is_empty() {
        request.credentials.clone()
    } else {
        request.credential.iter().cloned().collect()
    }
}

fn credential_location(credential: &A2aCredentialInput) -> A2aCredentialLocation {
    credential.location.unwrap_or_else(|| {
        credential
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("location"))
            .and_then(serde_json::Value::as_str)
            .and_then(|value| match value.to_ascii_lowercase().as_str() {
                "query" => Some(A2aCredentialLocation::Query),
                "cookie" => Some(A2aCredentialLocation::Cookie),
                "header" => Some(A2aCredentialLocation::Header),
                _ => None,
            })
            .unwrap_or_default()
    })
}

fn credential_name<'a>(credential: &'a A2aCredentialInput, message: &str) -> Result<&'a str, AgentError> {
    credential
        .header_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| AgentError::bad_request(message))
}

fn validate_cookie_component(value: &str) -> Result<(), AgentError> {
    if value.is_empty()
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || matches!(byte, b';' | b',' | b'=' | b' '))
    {
        return Err(AgentError::bad_request("Cookie API Key 包含非法字符"));
    }
    Ok(())
}

fn reject_oversized_content_length(headers: &HeaderMap) -> Result<(), AgentError> {
    if headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > MAX_AGENT_CARD_BYTES)
    {
        return Err(AgentError::bad_gateway("A2A Agent Card 超过 1 MiB 限制"));
    }
    Ok(())
}

fn header_string(headers: &HeaderMap, name: HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn parse_max_age_ms(headers: &HeaderMap) -> Option<i64> {
    let value = headers.get(CACHE_CONTROL)?.to_str().ok()?;
    value.split(',').find_map(|directive| {
        let (name, seconds) = directive.trim().split_once('=')?;
        if !name.trim().eq_ignore_ascii_case("max-age") {
            return None;
        }
        seconds
            .trim()
            .trim_matches('"')
            .parse::<i64>()
            .ok()
            .map(|value| value.max(0).saturating_mul(1000))
    })
}

fn protocol_error_to_agent(error: crate::protocol::a2a::error::A2aProtocolError) -> AgentError {
    AgentError::bad_request(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn parses_cache_control_max_age() {
        let mut headers = HeaderMap::new();
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("public, max-age=120"));
        assert_eq!(parse_max_age_ms(&headers), Some(120_000));
    }

    #[test]
    fn refuses_invalid_custom_header_name() {
        let client = Client::new();
        let credential = A2aCredentialInput {
            kind: A2aAuthKind::CustomHeader,
            scheme_name: Some("customAuth".to_owned()),
            header_name: Some("bad header".to_owned()),
            location: None,
            secret: Some("secret".to_owned()),
            metadata: None,
        };
        let builder = client.get("https://agent.example");
        assert!(apply_credentials(builder, &[credential]).is_err());
    }

    #[test]
    fn query_api_key_does_not_mutate_discovery_url() {
        let original = Url::parse("https://agent.example/.well-known/agent-card.json").unwrap();
        let credential = A2aCredentialInput {
            kind: A2aAuthKind::ApiKey,
            scheme_name: Some("queryKey".to_owned()),
            header_name: Some("key".to_owned()),
            location: Some(A2aCredentialLocation::Query),
            secret: Some("private".to_owned()),
            metadata: Some(serde_json::json!({ "location": "query" })),
        };
        let request = authenticated_url(&original, &[credential]).unwrap();
        assert!(!original.as_str().contains("private"));
        assert_eq!(
            request
                .query_pairs()
                .find(|(name, _)| name == "key")
                .map(|(_, value)| value.into_owned())
                .as_deref(),
            Some("private")
        );
    }

    #[tokio::test]
    async fn not_modified_is_treated_as_cache_hit_instead_of_redirect() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            let _ = socket.read(&mut request).await.unwrap();
            socket
                .write_all(b"HTTP/1.1 304 Not Modified\r\nETag: \"same\"\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        });

        let result = fetch_card(
            Url::parse(&format!("http://{address}/.well-known/agent-card.json")).unwrap(),
            A2aNetworkPolicy {
                allow_insecure: true,
                allow_private_network: true,
            },
            &[],
            Some(&CardCacheValidators {
                etag: Some("\"same\"".to_owned()),
                last_modified: None,
            }),
        )
        .await;
        let error = match result {
            Ok(_) => panic!("304 must not be decoded as an Agent Card"),
            Err(error) => error,
        };

        assert!(matches!(error, AgentError::Conflict(_)));
    }
}
