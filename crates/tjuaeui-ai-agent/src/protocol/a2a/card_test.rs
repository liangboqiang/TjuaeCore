use std::collections::HashSet;

use super::*;

fn v1_card() -> Vec<u8> {
    br#"{
        "name": "Test Agent",
        "description": "Agent for protocol tests",
        "version": "2026.7",
        "supportedInterfaces": [{
            "url": "https://agent.example/jsonrpc",
            "protocolBinding": "JSONRPC",
            "protocolVersion": "1.0"
        }],
        "capabilities": { "streaming": true },
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"],
        "skills": []
    }"#
    .to_vec()
}

#[test]
fn parses_v1_card() {
    let parsed = parse_agent_card(&v1_card(), &CardParseOptions::default()).expect("valid card");

    assert_eq!(parsed.source, A2aCardSource::V1);
    assert_eq!(parsed.card.name, "Test Agent");
    assert_eq!(parsed.card.supported_interfaces[0].protocol_version, "1.0");
}

#[test]
fn rejects_v03_without_explicit_compatibility() {
    let raw = br#"{
        "name": "Legacy Agent",
        "description": "Legacy",
        "version": "1",
        "url": "https://legacy.example/a2a",
        "preferredTransport": "JSONRPC",
        "capabilities": {},
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"],
        "skills": []
    }"#;

    let error = parse_agent_card(raw, &CardParseOptions::default()).expect_err("must reject");

    assert!(matches!(error, A2aProtocolError::V03CompatibilityRequired));
}

#[test]
fn normalizes_v03_when_compatibility_is_explicit() {
    let raw = br#"{
        "name": "Legacy Agent",
        "description": "Legacy",
        "version": "1",
        "url": "https://legacy.example/a2a",
        "preferredTransport": "JSONRPC",
        "additionalInterfaces": [{
            "url": "https://legacy.example/rest",
            "transport": "HTTP+JSON"
        }],
        "capabilities": { "supportsAuthenticatedExtendedCard": true },
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"],
        "skills": []
    }"#;
    let options = CardParseOptions {
        allow_v03: true,
        supported_extensions: HashSet::new(),
    };

    let parsed = parse_agent_card(raw, &options).expect("compatibility parse");

    assert_eq!(parsed.source, A2aCardSource::V03Compatibility);
    assert_eq!(parsed.card.supported_interfaces.len(), 2);
    assert_eq!(parsed.card.capabilities.extended_agent_card, Some(true));
}

#[test]
fn rejects_required_extension_without_client_support() {
    let raw = br#"{
        "name": "Extended Agent",
        "description": "Requires an extension",
        "version": "1",
        "supportedInterfaces": [{
            "url": "https://agent.example/a2a",
            "protocolBinding": "JSONRPC",
            "protocolVersion": "1.0"
        }],
        "capabilities": {
            "extensions": [{
                "uri": "urn:example:required",
                "required": true
            }]
        },
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"],
        "skills": []
    }"#;

    let error = parse_agent_card(raw, &CardParseOptions::default()).expect_err("must reject");

    assert!(matches!(
        error,
        A2aProtocolError::UnsupportedRequiredExtension(uri)
        if uri == "urn:example:required"
    ));
}

#[test]
fn accepts_required_extension_when_registered() {
    let mut supported_extensions = HashSet::new();
    supported_extensions.insert("urn:example:required".to_owned());
    let options = CardParseOptions {
        allow_v03: false,
        supported_extensions,
    };
    let raw = br#"{
        "name": "Extended Agent",
        "description": "Requires an extension",
        "version": "1",
        "supportedInterfaces": [{
            "url": "https://agent.example/a2a",
            "protocolBinding": "JSONRPC",
            "protocolVersion": "1.0"
        }],
        "capabilities": {
            "extensions": [{
                "uri": "urn:example:required",
                "required": true
            }]
        },
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"],
        "skills": []
    }"#;

    let parsed = parse_agent_card(raw, &options).expect("extension is supported");

    assert_eq!(parsed.card.name, "Extended Agent");
}

#[test]
fn rejects_oversized_card_before_json_parsing() {
    let raw = vec![b' '; MAX_AGENT_CARD_BYTES + 1];

    let error = parse_agent_card(&raw, &CardParseOptions::default()).expect_err("must reject");

    assert!(matches!(error, A2aProtocolError::InvalidCard(_)));
}
