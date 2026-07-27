use tjuaeui_api_types::{AgentErrorCode, AgentErrorOwnership};

use super::*;

#[test]
fn tjuae_cli_structured_malformed_tool_call_error_is_provider_error() {
    let error = TjuaeCliAgentError::ToolCallMalformed { count: 3, limit: 3 };
    let send_error = tjuae_cli_engine_error_to_send_error(&error);

    assert_eq!(send_error.code(), Some(AgentErrorCode::UserLlmProviderInvalidRequest));
    assert_eq!(send_error.ownership(), Some(AgentErrorOwnership::UserLlmProvider));
    assert_eq!(send_error.stream_error().retryable, Some(false));
}

#[test]
fn tjuae_cli_provider_rate_limited_appends_response_body_to_detail() {
    let error = TjuaeCliAgentError::Provider(ProviderError::RateLimited {
        retry_after_ms: 5000,
        body: Some(r#"{"error":{"code":"insufficient_quota","message":"You exceeded your current quota"}}"#.to_owned()),
    });
    let send_error = tjuae_cli_engine_error_to_send_error(&error);

    assert_eq!(send_error.code(), Some(AgentErrorCode::UserLlmProviderRateLimited));
    let detail = send_error
        .stream_error()
        .detail
        .as_deref()
        .expect("rate-limited errors must carry a detail");
    assert!(
        detail.contains("提供商响应："),
        "detail should include the provider body marker; got: {detail}"
    );
    assert!(
        detail.contains("insufficient_quota"),
        "detail should surface the raw provider signal; got: {detail}"
    );
}

#[test]
fn tjuae_cli_provider_rate_limited_without_body_falls_back_to_bare_detail() {
    let error = TjuaeCliAgentError::Provider(ProviderError::RateLimited {
        retry_after_ms: 5000,
        body: None,
    });
    let send_error = tjuae_cli_engine_error_to_send_error(&error);

    let detail = send_error
        .stream_error()
        .detail
        .as_deref()
        .expect("rate-limited errors must carry a detail");
    assert!(
        !detail.contains("提供商响应："),
        "detail must not add the body marker when body is absent; got: {detail}"
    );
    assert!(
        detail.contains("请求受到限流"),
        "detail should still include the base message; got: {detail}"
    );
}

#[test]
fn tjuae_cli_provider_rate_limited_ignores_whitespace_only_body() {
    let error = TjuaeCliAgentError::Provider(ProviderError::RateLimited {
        retry_after_ms: 5000,
        body: Some("   \n\t  ".to_owned()),
    });
    let send_error = tjuae_cli_engine_error_to_send_error(&error);

    let detail = send_error
        .stream_error()
        .detail
        .as_deref()
        .expect("rate-limited errors must carry a detail");
    assert!(
        !detail.contains("提供商响应："),
        "whitespace-only body should be treated as absent; got: {detail}"
    );
}

#[test]
fn tjuae_cli_provider_connection_error_is_user_llm_provider_error() {
    let error = TjuaeCliAgentError::Provider(ProviderError::Connection(
        "Signable request error: failed to create canonical request".to_owned(),
    ));
    let send_error = tjuae_cli_engine_error_to_send_error(&error);

    assert_eq!(send_error.code(), Some(AgentErrorCode::UserLlmProviderNetworkError));
    assert_eq!(send_error.ownership(), Some(AgentErrorOwnership::UserLlmProvider));
    assert_eq!(send_error.stream_error().retryable, Some(true));
}

#[test]
fn provider_error_summary_classifies_network_without_body() {
    let error = TjuaeCliAgentError::Provider(ProviderError::Connection("connect failed".into()));
    let summary = tjuae_cli_runtime_error_summary(&error);

    assert_eq!(summary.kind, "provider");
    assert_eq!(summary.provider_error_class, Some("network"));
    assert_eq!(summary.http_status, None);
}

#[test]
fn tool_call_failure_summary_classifies_loop() {
    let error = TjuaeCliAgentError::ToolCallFailures { count: 3, limit: 3 };
    let summary = tjuae_cli_runtime_error_summary(&error);

    assert_eq!(summary.kind, "tool_call_failures");
    assert_eq!(summary.failure_count, Some(3));
    assert_eq!(summary.failure_limit, Some(3));
}

#[test]
fn tjuae_cli_api_connection_error_is_user_llm_provider_network_error() {
    let error = TjuaeCliAgentError::Provider(ProviderError::Connection("error decoding response body".to_owned()));
    let send_error = tjuae_cli_engine_error_to_send_error(&error);

    assert_eq!(send_error.code(), Some(AgentErrorCode::UserLlmProviderNetworkError));
    assert_eq!(send_error.ownership(), Some(AgentErrorOwnership::UserLlmProvider));
    assert_eq!(send_error.stream_error().retryable, Some(true));
}

#[test]
fn tjuae_cli_provider_status_error_uses_status_instead_of_message_text() {
    let error = TjuaeCliAgentError::Provider(ProviderError::Api {
        status: 401,
        message: "credentials failed".to_owned(),
    });
    let send_error = tjuae_cli_engine_error_to_send_error(&error);

    assert_eq!(send_error.code(), Some(AgentErrorCode::UserLlmProviderAuthFailed));
    assert_eq!(send_error.ownership(), Some(AgentErrorOwnership::UserLlmProvider));
    assert_eq!(send_error.stream_error().retryable, Some(false));
}

#[test]
fn tjuae_cli_context_too_long_is_provider_context_error() {
    let error = TjuaeCliAgentError::ContextTooLong {
        input_tokens: 120_000,
        limit: 100_000,
    };
    let send_error = tjuae_cli_engine_error_to_send_error(&error);

    assert_eq!(send_error.code(), Some(AgentErrorCode::UserLlmProviderContextTooLarge));
    assert_eq!(send_error.ownership(), Some(AgentErrorOwnership::UserLlmProvider));
    assert_eq!(send_error.stream_error().retryable, Some(false));
}

#[test]
fn tjuae_cli_repeated_malformed_tool_call_is_user_llm_provider_error() {
    let error = TjuaeCliAgentError::ToolCallMalformed { count: 3, limit: 3 };
    let send_error = tjuae_cli_engine_error_to_send_error(&error);

    assert_eq!(send_error.code(), Some(AgentErrorCode::UserLlmProviderInvalidRequest));
    assert_eq!(send_error.ownership(), Some(AgentErrorOwnership::UserLlmProvider));
    assert_eq!(send_error.stream_error().retryable, Some(false));
}

#[test]
fn tjuae_cli_tool_call_failures_are_unknown_upstream_error() {
    let error = TjuaeCliAgentError::ToolCallFailures { count: 3, limit: 3 };
    let send_error = tjuae_cli_engine_error_to_send_error(&error);

    assert_eq!(send_error.code(), Some(AgentErrorCode::UnknownUpstreamError));
    assert_eq!(send_error.ownership(), Some(AgentErrorOwnership::UnknownUpstream));
    assert_eq!(send_error.stream_error().retryable, Some(true));
}
