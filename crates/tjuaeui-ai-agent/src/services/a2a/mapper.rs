use a2a::{AgentCard, AgentInterface};
use tjuaeui_api_types::{A2aAgentCardSummary, A2aAgentInterfaceSummary, A2aAgentSkillSummary, A2aBinding};

use crate::error::AgentError;

pub(crate) fn select_interface(card: &AgentCard) -> Result<(&AgentInterface, A2aBinding), AgentError> {
    for preferred in [A2aBinding::JsonRpc, A2aBinding::HttpJson, A2aBinding::Grpc] {
        if let Some(interface) = card.supported_interfaces.iter().find(|interface| {
            interface.protocol_version == a2a::VERSION
                && binding_from_protocol(&interface.protocol_binding) == Some(preferred)
        }) {
            return Ok((interface, preferred));
        }
    }
    Err(AgentError::bad_request("Agent Card 没有可用的 A2A v1 接口"))
}

pub(crate) fn card_summary(card: &AgentCard) -> Result<A2aAgentCardSummary, AgentError> {
    let (selected, selected_binding) = select_interface(card)?;
    let supported_bindings = card
        .supported_interfaces
        .iter()
        .filter(|interface| interface.protocol_version == a2a::VERSION)
        .filter_map(|interface| binding_from_protocol(&interface.protocol_binding))
        .fold(Vec::new(), |mut values, binding| {
            if !values.contains(&binding) {
                values.push(binding);
            }
            values
        });
    let supported_interfaces = card
        .supported_interfaces
        .iter()
        .filter_map(|interface| {
            binding_from_protocol(&interface.protocol_binding).map(|binding| A2aAgentInterfaceSummary {
                url: interface.url.clone(),
                binding,
                protocol_version: interface.protocol_version.clone(),
                tenant: interface.tenant.clone(),
            })
        })
        .collect();
    let required_extensions = card
        .capabilities
        .extensions
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|extension| extension.required == Some(true))
        .map(|extension| extension.uri.clone())
        .collect();
    let capabilities = serde_json::to_value(&card.capabilities)
        .map_err(|error| AgentError::internal(format!("编码 A2A 能力失败：{error}")))?;
    let mut security_schemes = serde_json::to_value(&card.security_schemes)
        .map_err(|error| AgentError::internal(format!("编码 A2A 安全方案失败：{error}")))?;
    if security_schemes.is_null() {
        security_schemes = serde_json::json!({});
    }
    let mut security_requirements = serde_json::to_value(&card.security_requirements)
        .map_err(|error| AgentError::internal(format!("编码 A2A 安全要求失败：{error}")))?;
    if security_requirements.is_null() {
        security_requirements = serde_json::json!([]);
    }

    Ok(A2aAgentCardSummary {
        name: card.name.clone(),
        description: card.description.clone(),
        agent_version: card.version.clone(),
        protocol_version: selected.protocol_version.clone(),
        selected_binding,
        selected_interface_url: selected.url.clone(),
        selected_tenant: selected.tenant.clone(),
        supported_interfaces,
        supported_bindings,
        default_input_modes: card.default_input_modes.clone(),
        default_output_modes: card.default_output_modes.clone(),
        skills: card
            .skills
            .iter()
            .map(|skill| A2aAgentSkillSummary {
                id: skill.id.clone(),
                name: skill.name.clone(),
                description: skill.description.clone(),
                tags: skill.tags.clone(),
                input_modes: skill.input_modes.clone(),
                output_modes: skill.output_modes.clone(),
            })
            .collect(),
        capabilities,
        security_schemes,
        security_requirements,
        required_extensions,
    })
}

pub(crate) fn binding_from_protocol(protocol: &str) -> Option<A2aBinding> {
    if protocol.eq_ignore_ascii_case(a2a::TRANSPORT_PROTOCOL_JSONRPC) {
        Some(A2aBinding::JsonRpc)
    } else if protocol.eq_ignore_ascii_case(a2a::TRANSPORT_PROTOCOL_HTTP_JSON) {
        Some(A2aBinding::HttpJson)
    } else if protocol.eq_ignore_ascii_case(a2a::TRANSPORT_PROTOCOL_GRPC) {
        Some(A2aBinding::Grpc)
    } else {
        None
    }
}

pub(crate) fn binding_db_name(binding: A2aBinding) -> &'static str {
    match binding {
        A2aBinding::JsonRpc => "json_rpc",
        A2aBinding::HttpJson => "http_json",
        A2aBinding::Grpc => "grpc",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_summary_normalizes_absent_security_to_no_auth_contract() {
        let card: AgentCard = serde_json::from_value(serde_json::json!({
            "name": "No-auth Agent",
            "description": "A2A discovery fixture",
            "version": "1.0.0",
            "supportedInterfaces": [{
                "url": "https://agent.example/a2a",
                "protocolBinding": "JSONRPC",
                "protocolVersion": "1.0"
            }],
            "capabilities": {},
            "defaultInputModes": ["text/plain"],
            "defaultOutputModes": ["text/plain"],
            "skills": []
        }))
        .expect("valid A2A Agent Card");

        let summary = card_summary(&card).expect("card summary");

        assert_eq!(summary.security_schemes, serde_json::json!({}));
        assert_eq!(summary.security_requirements, serde_json::json!([]));
    }
}
