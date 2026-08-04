use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use reqwest::{Certificate, ClientBuilder, Identity};
use tjuaeui_api_types::{A2aAuthKind, A2aCredentialInput};
use url::Url;

use crate::error::AgentError;

const DEFAULT_AGENT_CARD_PATH: &str = "/.well-known/agent-card.json";
const MAX_MTLS_PEM_BYTES: usize = 512 * 1024;
const CLIENT_CERTIFICATE_PEM_KEY: &str = "client_certificate_pem";
const CA_CERTIFICATE_PEM_KEY: &str = "ca_certificate_pem";

#[derive(Debug, Clone)]
pub(crate) struct A2aMtlsMaterial {
    pub client_certificate_pem: String,
    pub private_key_pem: String,
    pub ca_certificate_pem: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct A2aNetworkPolicy {
    pub allow_insecure: bool,
    pub allow_private_network: bool,
}

pub(crate) fn normalize_card_url(input: &str, policy: A2aNetworkPolicy) -> Result<Url, AgentError> {
    let mut url = Url::parse(input.trim()).map_err(|_| AgentError::bad_request("A2A 地址不是有效 URL"))?;
    validate_url_shape(&url, policy)?;

    let is_explicit_card = url.path().ends_with("/agent-card.json") || url.path().ends_with(".json");
    if !is_explicit_card {
        url.set_path(DEFAULT_AGENT_CARD_PATH);
        url.set_query(None);
    }
    url.set_fragment(None);
    Ok(url)
}

pub(crate) fn base_url(url: &Url) -> Result<Url, AgentError> {
    Url::parse(&url.origin().ascii_serialization()).map_err(|_| AgentError::bad_request("无法从 A2A 地址确定来源"))
}

pub(crate) async fn validate_network_target(url: &Url, policy: A2aNetworkPolicy) -> Result<(), AgentError> {
    resolve_network_target(url, policy).await.map(|_| ())
}

/// Resolve once, validate every returned address, and return the exact socket
/// addresses callers should pin into their HTTP client. This closes the DNS
/// validation/use gap for direct requests.
pub(crate) async fn resolve_network_target(url: &Url, policy: A2aNetworkPolicy) -> Result<Vec<SocketAddr>, AgentError> {
    validate_url_shape(url, policy)?;
    let host = url
        .host_str()
        .ok_or_else(|| AgentError::bad_request("A2A 地址缺少主机名"))?;
    if !policy.allow_private_network && (host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost")) {
        return Err(private_network_error());
    }

    let port = url
        .port_or_known_default()
        .ok_or_else(|| AgentError::bad_request("A2A 地址缺少有效端口"))?;
    if let Ok(ip) = host.parse::<IpAddr>() {
        if !policy.allow_private_network {
            validate_ip(ip)?;
        }
        return Ok(vec![SocketAddr::new(ip, port)]);
    }

    let resolved = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| AgentError::bad_gateway("无法解析 A2A 主机名"))?;
    let mut addresses = Vec::new();
    for socket in resolved {
        if !policy.allow_private_network {
            validate_ip(socket.ip())?;
        }
        if !addresses.contains(&socket) {
            addresses.push(socket);
        }
    }
    if addresses.is_empty() {
        return Err(AgentError::bad_gateway("A2A 主机名没有可用地址"));
    }
    Ok(addresses)
}

pub(crate) fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme().eq_ignore_ascii_case(right.scheme())
        && left.host_str().map(str::to_ascii_lowercase) == right.host_str().map(str::to_ascii_lowercase)
        && left.port_or_known_default() == right.port_or_known_default()
}

pub(crate) fn mtls_material(credential: Option<&A2aCredentialInput>) -> Result<Option<A2aMtlsMaterial>, AgentError> {
    let Some(credential) = credential.filter(|value| value.kind == A2aAuthKind::Mtls) else {
        return Ok(None);
    };
    let private_key_pem = required_pem(
        credential.secret.as_deref(),
        "mTLS 凭据缺少客户端私钥",
        &["-----BEGIN PRIVATE KEY-----", "-----BEGIN RSA PRIVATE KEY-----"],
    )?;
    let client_certificate_pem = required_pem(
        credential
            .metadata
            .as_ref()
            .and_then(|value| value.get(CLIENT_CERTIFICATE_PEM_KEY))
            .and_then(serde_json::Value::as_str),
        "mTLS 凭据缺少客户端证书",
        &["-----BEGIN CERTIFICATE-----"],
    )?;
    let ca_certificate_pem = credential
        .metadata
        .as_ref()
        .and_then(|value| value.get(CA_CERTIFICATE_PEM_KEY))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            validate_pem(value, "mTLS CA 证书格式无效", &["-----BEGIN CERTIFICATE-----"])?;
            Ok::<_, AgentError>(value.to_owned())
        })
        .transpose()?;

    Ok(Some(A2aMtlsMaterial {
        client_certificate_pem,
        private_key_pem,
        ca_certificate_pem,
    }))
}

pub(crate) fn apply_mtls_to_reqwest_builder(
    mut builder: ClientBuilder,
    credentials: &[A2aCredentialInput],
) -> Result<ClientBuilder, AgentError> {
    let mtls_credentials = credentials
        .iter()
        .filter(|credential| credential.kind == A2aAuthKind::Mtls)
        .collect::<Vec<_>>();
    if mtls_credentials.len() > 1 {
        return Err(AgentError::bad_request("单个 A2A 连接只能配置一份 mTLS 身份"));
    }
    let Some(material) = mtls_material(mtls_credentials.first().copied())? else {
        return Ok(builder);
    };
    let identity_pem = format!(
        "{}\n{}",
        material.client_certificate_pem.trim(),
        material.private_key_pem.trim()
    );
    let identity = Identity::from_pem(identity_pem.as_bytes())
        .map_err(|_| AgentError::bad_request("mTLS 客户端证书或私钥无法解析，且必须互相匹配"))?;
    builder = builder.identity(identity);
    if let Some(ca_certificate_pem) = material.ca_certificate_pem {
        let certificate = Certificate::from_pem(ca_certificate_pem.as_bytes())
            .map_err(|_| AgentError::bad_request("mTLS CA 证书无法解析"))?;
        builder = builder.add_root_certificate(certificate);
    }
    Ok(builder)
}

fn required_pem(value: Option<&str>, missing_message: &str, markers: &[&str]) -> Result<String, AgentError> {
    let value = value.map(str::trim).filter(|value| !value.is_empty());
    let Some(value) = value else {
        return Err(AgentError::bad_request(missing_message));
    };
    validate_pem(value, missing_message, markers)?;
    Ok(value.to_owned())
}

fn validate_pem(value: &str, invalid_message: &str, markers: &[&str]) -> Result<(), AgentError> {
    if value.len() > MAX_MTLS_PEM_BYTES {
        return Err(AgentError::bad_request("单个 mTLS PEM 内容不得超过 512 KiB"));
    }
    if !markers.iter().any(|marker| value.contains(marker)) {
        return Err(AgentError::bad_request(invalid_message));
    }
    Ok(())
}

fn validate_url_shape(url: &Url, policy: A2aNetworkPolicy) -> Result<(), AgentError> {
    match url.scheme() {
        "https" => {}
        "http" if policy.allow_insecure => {}
        "http" => {
            return Err(AgentError::bad_request(
                "A2A 默认要求 HTTPS；如确需 HTTP，请显式允许不安全连接",
            ));
        }
        _ => return Err(AgentError::bad_request("A2A 地址仅支持 HTTPS 或 HTTP")),
    }
    if url.host_str().is_none() {
        return Err(AgentError::bad_request("A2A 地址缺少主机名"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(AgentError::bad_request("A2A 地址不得内嵌用户名或密码"));
    }
    if url.fragment().is_some() {
        return Err(AgentError::bad_request("A2A 地址不得包含片段标识"));
    }
    Ok(())
}

fn validate_ip(ip: IpAddr) -> Result<(), AgentError> {
    let blocked = match ip {
        IpAddr::V4(ip) => is_blocked_v4(ip),
        IpAddr::V6(ip) => is_blocked_v6(ip),
    };
    if blocked { Err(private_network_error()) } else { Ok(()) }
}

fn is_blocked_v4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || octets[0] == 0
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
        || (octets[0] == 198 && matches!(octets[1], 18 | 19))
        || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
        || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
        || octets[0] >= 240
}

fn is_blocked_v6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_unique_local()
        || ip.is_unicast_link_local()
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || ip.to_ipv4_mapped().is_some_and(is_blocked_v4)
}

fn private_network_error() -> AgentError {
    AgentError::forbidden("A2A 目标解析到本机、私网或保留地址；如确需访问，请显式信任私有网络")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secure_public() -> A2aNetworkPolicy {
        A2aNetworkPolicy {
            allow_insecure: false,
            allow_private_network: false,
        }
    }

    #[test]
    fn base_url_becomes_well_known_card_url() {
        let url = normalize_card_url("https://agent.example/path?q=1", secure_public()).unwrap();
        assert_eq!(url.as_str(), "https://agent.example/.well-known/agent-card.json");
    }

    #[test]
    fn explicit_card_url_is_preserved() {
        let url = normalize_card_url(
            "https://agent.example/custom/agent-card.json?tenant=one",
            secure_public(),
        )
        .unwrap();
        assert_eq!(url.as_str(), "https://agent.example/custom/agent-card.json?tenant=one");
    }

    #[test]
    fn rejects_http_without_explicit_permission() {
        assert!(normalize_card_url("http://agent.example", secure_public()).is_err());
    }

    #[test]
    fn blocks_private_and_reserved_addresses() {
        for input in ["127.0.0.1", "10.0.0.1", "169.254.1.1", "100.64.0.1", "2001:db8::1"] {
            let ip: IpAddr = input.parse().unwrap();
            assert!(validate_ip(ip).is_err(), "{input} should be blocked");
        }
    }

    #[test]
    fn recognizes_same_origin_with_default_ports() {
        let left = Url::parse("https://agent.example/card").unwrap();
        let right = Url::parse("https://AGENT.example:443/rpc").unwrap();
        assert!(same_origin(&left, &right));
    }

    #[test]
    fn rejects_incomplete_mtls_material() {
        let credential = A2aCredentialInput {
            kind: A2aAuthKind::Mtls,
            scheme_name: Some("mtlsAuth".to_owned()),
            header_name: None,
            location: None,
            secret: Some("-----BEGIN PRIVATE KEY-----\ninvalid\n-----END PRIVATE KEY-----".to_owned()),
            metadata: None,
        };
        assert!(mtls_material(Some(&credential)).is_err());
    }

    #[test]
    fn valid_mtls_identity_and_custom_ca_build_a_reqwest_client() {
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let certificate_pem = cert.pem();
        let credential = A2aCredentialInput {
            kind: A2aAuthKind::Mtls,
            scheme_name: Some("mtlsAuth".to_owned()),
            header_name: None,
            location: None,
            secret: Some(signing_key.serialize_pem()),
            metadata: Some(serde_json::json!({
                CLIENT_CERTIFICATE_PEM_KEY: certificate_pem,
                CA_CERTIFICATE_PEM_KEY: certificate_pem,
            })),
        };

        let material = mtls_material(Some(&credential)).unwrap().unwrap();
        assert!(material.client_certificate_pem.contains("BEGIN CERTIFICATE"));
        assert!(material.private_key_pem.contains("BEGIN PRIVATE KEY"));
        assert!(material.ca_certificate_pem.is_some());
        apply_mtls_to_reqwest_builder(ClientBuilder::new(), &[credential])
            .unwrap()
            .build()
            .unwrap();
    }
}
