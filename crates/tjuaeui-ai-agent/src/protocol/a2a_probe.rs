use std::time::Duration;

use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde_json::Value;
use tjuaeui_api_types::{TryConnectA2aAgentRequest, TryConnectA2aAgentResponse};

/// 在不落库的情况下发现远程 A2A Agent Card。
///
/// 探测过程不会安装任何软件；既可传入 Agent Card 的完整地址，也可传入
/// Agent 服务根地址，后者会继续尝试标准的 well-known 地址。
pub async fn discover(req: &TryConnectA2aAgentRequest) -> Result<TryConnectA2aAgentResponse, String> {
    let endpoint = req.endpoint.trim();
    let parsed = reqwest::Url::parse(endpoint).map_err(|error| format!("A2A 地址无效：{error}"))?;
    if parsed.scheme() != "https" && !(req.allow_insecure && parsed.scheme() == "http") {
        return Err("A2A 默认要求 HTTPS；仅在确认目标可信时才允许 HTTP".to_owned());
    }

    let mut headers = HeaderMap::new();
    if let Some(token) = req.auth_token.as_deref().filter(|value| !value.trim().is_empty()) {
        let value = match req.auth_type.as_deref().unwrap_or("bearer") {
            "api_key" => token.to_owned(),
            _ => format!("Bearer {token}"),
        };
        let header_name = if req.auth_type.as_deref() == Some("api_key") {
            reqwest::header::HeaderName::from_static("x-api-key")
        } else {
            AUTHORIZATION
        };
        headers.insert(
            header_name,
            HeaderValue::from_str(&value).map_err(|_| "A2A 凭据包含无效字符".to_owned())?,
        );
    }

    let client = reqwest::Client::builder()
        .default_headers(headers)
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|error| format!("创建 A2A 客户端失败：{error}"))?;

    let mut last_error = String::new();
    for candidate in candidate_urls(&parsed) {
        match client.get(candidate.clone()).send().await {
            Ok(response) if response.status().is_success() => match response.json::<Value>().await {
                Ok(card) => match parse_agent_card(&candidate, &card) {
                    Ok(result) => return Ok(result),
                    Err(error) => last_error = error,
                },
                Err(error) => last_error = format!("{candidate} 不是有效的 Agent Card JSON：{error}"),
            },
            Ok(response) => last_error = format!("{} 返回 HTTP {}", candidate, response.status()),
            Err(error) => last_error = format!("{}：{}", candidate, error),
        }
    }
    Err(format!("未发现可用的 A2A Agent Card：{last_error}"))
}

fn parse_agent_card(endpoint: &reqwest::Url, card: &Value) -> Result<TryConnectA2aAgentResponse, String> {
    let name = card
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{endpoint} 返回的 A2A Agent Card 缺少 name"))?;
    let version = card.get("version").and_then(Value::as_str).map(str::to_owned);
    Ok(TryConnectA2aAgentResponse {
        name: name.to_owned(),
        version,
        endpoint: endpoint.to_string(),
    })
}

fn candidate_urls(endpoint: &reqwest::Url) -> Vec<reqwest::Url> {
    if endpoint.path().ends_with(".json") {
        return vec![endpoint.clone()];
    }
    let mut urls = vec![endpoint.clone()];
    for path in ["/.well-known/agent-card.json", "/.well-known/agent.json"] {
        if let Ok(url) = endpoint.join(path)
            && !urls.contains(&url)
        {
            urls.push(url);
        }
    }
    urls
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn base_url_generates_standard_discovery_candidates() {
        let endpoint = reqwest::Url::parse("https://agent.example.com/service").unwrap();
        let candidates = candidate_urls(&endpoint)
            .into_iter()
            .map(|url| url.to_string())
            .collect::<Vec<_>>();

        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates[0], "https://agent.example.com/service");
        assert_eq!(candidates[1], "https://agent.example.com/.well-known/agent-card.json");
        assert_eq!(candidates[2], "https://agent.example.com/.well-known/agent.json");
    }

    #[test]
    fn direct_json_url_has_no_extra_candidates() {
        let endpoint = reqwest::Url::parse("https://agent.example.com/custom/card.json").unwrap();
        let candidates = candidate_urls(&endpoint);

        assert_eq!(candidates, vec![endpoint]);
    }

    #[test]
    fn valid_agent_card_returns_metadata_and_resolved_url() {
        let endpoint = reqwest::Url::parse("https://agent.example.com/.well-known/agent-card.json").unwrap();
        let result = parse_agent_card(&endpoint, &json!({ "name": "Planner", "version": "1.2.0" })).unwrap();

        assert_eq!(result.name, "Planner");
        assert_eq!(result.version.as_deref(), Some("1.2.0"));
        assert_eq!(result.endpoint, endpoint.as_str());
    }

    #[test]
    fn agent_card_without_name_is_rejected() {
        let endpoint = reqwest::Url::parse("https://agent.example.com/card.json").unwrap();
        let error = parse_agent_card(&endpoint, &json!({ "version": "1.2.0" })).unwrap_err();

        assert!(error.contains("缺少 name"));
    }

    #[tokio::test]
    async fn insecure_http_is_rejected_by_default() {
        let error = discover(&TryConnectA2aAgentRequest {
            endpoint: "http://agent.example.com".to_owned(),
            auth_type: None,
            auth_token: None,
            allow_insecure: false,
        })
        .await
        .unwrap_err();

        assert!(error.contains("默认要求 HTTPS"));
    }
}
