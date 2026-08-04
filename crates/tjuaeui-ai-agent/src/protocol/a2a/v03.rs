use serde_json::{Map, Value, json};

use super::error::A2aProtocolError;

pub(super) fn normalize_v03_card(raw: Value) -> Result<Value, A2aProtocolError> {
    let mut card = raw
        .as_object()
        .cloned()
        .ok_or_else(|| A2aProtocolError::InvalidCard("v0.3 Card 顶层必须是对象".to_owned()))?;

    let primary_url = take_required_string(&mut card, "url")?;
    let preferred_transport = take_optional_string(&mut card, "preferredTransport")
        .unwrap_or_else(|| a2a::TRANSPORT_PROTOCOL_JSONRPC.to_owned());
    let mut interfaces = vec![interface_value(primary_url, preferred_transport)];

    if let Some(additional) = card.remove("additionalInterfaces") {
        let values = additional
            .as_array()
            .ok_or_else(|| A2aProtocolError::InvalidCard("additionalInterfaces 必须是数组".to_owned()))?;
        for value in values {
            let object = value
                .as_object()
                .ok_or_else(|| A2aProtocolError::InvalidCard("additionalInterfaces 项必须是对象".to_owned()))?;
            let url = object
                .get("url")
                .and_then(Value::as_str)
                .ok_or_else(|| A2aProtocolError::InvalidCard("additionalInterfaces.url 必须是字符串".to_owned()))?
                .to_owned();
            let binding = object
                .get("transport")
                .or_else(|| object.get("protocolBinding"))
                .and_then(Value::as_str)
                .ok_or_else(|| A2aProtocolError::InvalidCard("additionalInterfaces.transport 必须是字符串".to_owned()))?
                .to_owned();
            interfaces.push(interface_value(url, binding));
        }
    }
    card.insert("supportedInterfaces".to_owned(), Value::Array(interfaces));

    card.remove("protocolVersion");
    if let Some(security) = card.remove("security") {
        card.insert("securityRequirements".to_owned(), security);
    }
    normalize_v03_capabilities(&mut card);
    normalize_v03_security_schemes(&mut card)?;

    Ok(Value::Object(card))
}

fn interface_value(url: String, binding: String) -> Value {
    json!({
        "url": url,
        "protocolBinding": binding,
        "protocolVersion": a2a::VERSION,
    })
}

fn normalize_v03_capabilities(card: &mut Map<String, Value>) {
    let Some(capabilities) = card.get_mut("capabilities").and_then(Value::as_object_mut) else {
        return;
    };
    if let Some(value) = capabilities.remove("supportsAuthenticatedExtendedCard") {
        capabilities.insert("extendedAgentCard".to_owned(), value);
    }
    capabilities.remove("stateTransitionHistory");
}

fn normalize_v03_security_schemes(card: &mut Map<String, Value>) -> Result<(), A2aProtocolError> {
    let Some(schemes) = card.get_mut("securitySchemes").and_then(Value::as_object_mut) else {
        return Ok(());
    };

    for (name, scheme) in schemes.iter_mut() {
        let object = scheme
            .as_object()
            .ok_or_else(|| A2aProtocolError::InvalidCard(format!("securitySchemes.{name} 必须是对象")))?;
        if object.keys().any(|key| key.ends_with("SecurityScheme")) {
            continue;
        }

        let kind = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| A2aProtocolError::InvalidCard(format!("securitySchemes.{name}.type 缺失")))?;
        let wrapped = match kind {
            "apiKey" => {
                let mut inner = object.clone();
                inner.remove("type");
                if let Some(location) = inner.remove("in") {
                    inner.insert("location".to_owned(), location);
                }
                json!({ "apiKeySecurityScheme": inner })
            }
            "http" => {
                let mut inner = object.clone();
                inner.remove("type");
                json!({ "httpAuthSecurityScheme": inner })
            }
            "oauth2" => {
                let mut inner = object.clone();
                inner.remove("type");
                json!({ "oauth2SecurityScheme": inner })
            }
            "openIdConnect" => {
                let mut inner = object.clone();
                inner.remove("type");
                json!({ "openIdConnectSecurityScheme": inner })
            }
            "mutualTLS" => {
                let mut inner = object.clone();
                inner.remove("type");
                json!({ "mtlsSecurityScheme": inner })
            }
            other => {
                return Err(A2aProtocolError::InvalidCard(format!(
                    "securitySchemes.{name}.type 不受支持：{other}"
                )));
            }
        };
        *scheme = wrapped;
    }
    Ok(())
}

fn take_required_string(object: &mut Map<String, Value>, key: &str) -> Result<String, A2aProtocolError> {
    take_optional_string(object, key).ok_or_else(|| A2aProtocolError::InvalidCard(format!("{key} 必须是非空字符串")))
}

fn take_optional_string(object: &mut Map<String, Value>, key: &str) -> Option<String> {
    object
        .remove(key)
        .and_then(|value| value.as_str().map(str::trim).map(str::to_owned))
        .filter(|value| !value.is_empty())
}
