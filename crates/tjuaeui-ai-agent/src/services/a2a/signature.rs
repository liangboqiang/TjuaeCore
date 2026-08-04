use serde_json::Value;

use crate::error::AgentError;

/// Report signature presence without maintaining a product-owned trust store.
///
/// A2A Agent Card signatures describe detached JWS values, but the protocol
/// does not define a user-editable root store or a universal key-discovery
/// mechanism. TLS and the confirmed interface origin remain the trust boundary;
/// cards that contain signatures are surfaced as signed but unverified.
pub(crate) fn evaluate_agent_card_signatures(raw_card_json: &str) -> Result<&'static str, AgentError> {
    let card: Value =
        serde_json::from_str(raw_card_json).map_err(|_| AgentError::bad_gateway("Agent Card JSON 无效"))?;
    let signatures = card
        .get("signatures")
        .cloned()
        .map(serde_json::from_value::<Vec<a2a::AgentCardSignature>>)
        .transpose()
        .map_err(|_| AgentError::bad_gateway("Agent Card 签名结构无效"))?
        .unwrap_or_default();
    Ok(if signatures.is_empty() {
        "unsigned"
    } else {
        "signed_unverified"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_signature_presence_without_local_trust_roots() {
        assert_eq!(
            evaluate_agent_card_signatures(r#"{"name":"Agent"}"#).unwrap(),
            "unsigned"
        );
        assert_eq!(
            evaluate_agent_card_signatures(r#"{"name":"Agent","signatures":[{"protected":"e30","signature":"AA"}]}"#)
                .unwrap(),
            "signed_unverified"
        );
    }
}
