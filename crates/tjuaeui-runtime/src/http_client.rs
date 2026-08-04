use std::time::Duration;

const DEFAULT_RUNTIME_USER_AGENT: &str = concat!("tjuaecore/", env!("CARGO_PKG_VERSION"));

pub fn build_http_client(connect_timeout: Duration, timeout: Duration) -> Result<reqwest::Client, String> {
    crate::network_proxy::apply_network_proxy_to_http_client(reqwest::Client::builder())
        .connect_timeout(connect_timeout)
        .timeout(timeout)
        .user_agent(DEFAULT_RUNTIME_USER_AGENT)
        .build()
        .map_err(|error| format!("build http client: {error}"))
}

/// 构建适用于长时间流式响应的代理感知 HTTP 客户端。
///
/// 只限制建立连接的时间，不设置整个请求的截止时间，避免长对话流在正常输出期间
/// 被固定总时长中断。
pub fn build_streaming_http_client(connect_timeout: Duration) -> Result<reqwest::Client, String> {
    crate::network_proxy::apply_network_proxy_to_http_client(reqwest::Client::builder())
        .connect_timeout(connect_timeout)
        .user_agent(DEFAULT_RUNTIME_USER_AGENT)
        .build()
        .map_err(|error| format!("build streaming http client: {error}"))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::build_http_client;

    #[test]
    fn build_http_client_succeeds_with_runtime_defaults() {
        let _client = build_http_client(Duration::from_secs(1), Duration::from_secs(1)).expect("client");
    }

    #[test]
    fn build_streaming_http_client_succeeds_without_request_timeout() {
        let _client = super::build_streaming_http_client(Duration::from_secs(1)).expect("client");
    }
}
