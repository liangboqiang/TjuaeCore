use std::collections::HashSet;

use a2a::{AgentCard, TRANSPORT_PROTOCOL_GRPC, TRANSPORT_PROTOCOL_HTTP_JSON, TRANSPORT_PROTOCOL_JSONRPC};
use serde_json::Value;

use super::error::A2aProtocolError;
use super::v03::normalize_v03_card;

pub(crate) const MAX_AGENT_CARD_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum A2aCardSource {
    V1,
    V03Compatibility,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CardParseOptions {
    pub allow_v03: bool,
    pub supported_extensions: HashSet<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ParsedAgentCard {
    pub card: AgentCard,
    pub source: A2aCardSource,
}

pub(crate) fn parse_agent_card(bytes: &[u8], options: &CardParseOptions) -> Result<ParsedAgentCard, A2aProtocolError> {
    if bytes.len() > MAX_AGENT_CARD_BYTES {
        return Err(A2aProtocolError::InvalidCard(format!(
            "内容超过 {} 字节限制",
            MAX_AGENT_CARD_BYTES
        )));
    }

    let raw: Value = serde_json::from_slice(bytes).map_err(|error| A2aProtocolError::InvalidJson(error.to_string()))?;
    let object = raw
        .as_object()
        .ok_or_else(|| A2aProtocolError::InvalidCard("顶层必须是对象".to_owned()))?;

    let (normalized, source) = if object.contains_key("supportedInterfaces") {
        (raw, A2aCardSource::V1)
    } else if object.contains_key("url") || object.contains_key("preferredTransport") {
        if !options.allow_v03 {
            return Err(A2aProtocolError::V03CompatibilityRequired);
        }
        (normalize_v03_card(raw)?, A2aCardSource::V03Compatibility)
    } else {
        return Err(A2aProtocolError::InvalidCard(
            "缺少 supportedInterfaces；也不是可识别的 v0.3 Card".to_owned(),
        ));
    };

    let card: AgentCard =
        serde_json::from_value(normalized).map_err(|error| A2aProtocolError::InvalidCard(error.to_string()))?;
    validate_card(&card, options)?;

    Ok(ParsedAgentCard { card, source })
}

fn validate_card(card: &AgentCard, options: &CardParseOptions) -> Result<(), A2aProtocolError> {
    if card.name.trim().is_empty() {
        return Err(A2aProtocolError::InvalidCard("name 不能为空".to_owned()));
    }
    if card.supported_interfaces.is_empty() {
        return Err(A2aProtocolError::InvalidCard("supportedInterfaces 不能为空".to_owned()));
    }

    for interface in &card.supported_interfaces {
        if interface.url.trim().is_empty() {
            return Err(A2aProtocolError::InvalidCard(
                "supportedInterfaces.url 不能为空".to_owned(),
            ));
        }
    }
    if !card.supported_interfaces.iter().any(|interface| {
        interface.protocol_version == a2a::VERSION && is_supported_binding(&interface.protocol_binding)
    }) {
        let versions = card
            .supported_interfaces
            .iter()
            .map(|interface| interface.protocol_version.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        if !card
            .supported_interfaces
            .iter()
            .any(|interface| is_supported_binding(&interface.protocol_binding))
        {
            return Err(A2aProtocolError::UnsupportedBinding(
                card.supported_interfaces
                    .iter()
                    .map(|interface| interface.protocol_binding.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        }
        return Err(A2aProtocolError::UnsupportedVersion(versions));
    }

    if let Some(extensions) = card.capabilities.extensions.as_ref() {
        for extension in extensions {
            if extension.required == Some(true) && !options.supported_extensions.contains(&extension.uri) {
                return Err(A2aProtocolError::UnsupportedRequiredExtension(extension.uri.clone()));
            }
        }
    }

    Ok(())
}

fn is_supported_binding(binding: &str) -> bool {
    [
        TRANSPORT_PROTOCOL_JSONRPC,
        TRANSPORT_PROTOCOL_HTTP_JSON,
        TRANSPORT_PROTOCOL_GRPC,
    ]
    .iter()
    .any(|supported| binding.eq_ignore_ascii_case(supported))
}

#[cfg(test)]
#[path = "card_test.rs"]
mod card_test;
