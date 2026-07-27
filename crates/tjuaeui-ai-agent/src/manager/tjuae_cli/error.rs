use tjuae_agent::error::AgentError as TjuaeCliAgentError;
use tjuae_providers::ProviderError;
use tjuaeui_api_types::{
    AgentErrorCode, AgentErrorOwnership, AgentErrorResolution, AgentErrorResolutionKind, AgentErrorResolutionTarget,
};

use crate::protocol::send_error::AgentSendError;

pub(super) fn tjuae_cli_engine_error_to_send_error(error: &TjuaeCliAgentError) -> AgentSendError {
    let detail = format!("TjuaeCLI Agent 错误：{error}");
    match error {
        TjuaeCliAgentError::Provider(provider_error) => tjuae_cli_provider_error_to_send_error(provider_error, detail),
        TjuaeCliAgentError::ToolCallMalformed { .. } => provider_send_error(
            "模型提供商反复返回格式错误的工具调用",
            AgentErrorCode::UserLlmProviderInvalidRequest,
            detail,
            false,
            AgentErrorResolutionKind::ChangeModel,
            Some(AgentErrorResolutionTarget::ProviderSettings),
        ),
        TjuaeCliAgentError::ToolCallFailures { .. } => tool_call_failure_send_error(detail),
        TjuaeCliAgentError::ContextTooLong { .. } => provider_send_error(
            "请求内容超出当前模型的上下文窗口",
            AgentErrorCode::UserLlmProviderContextTooLarge,
            detail,
            false,
            AgentErrorResolutionKind::ReduceContext,
            None,
        ),
        TjuaeCliAgentError::ApiError(_) => unknown_upstream_send_error(detail),
        TjuaeCliAgentError::UserAborted => unknown_upstream_send_error(detail),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TjuaeCliRuntimeErrorSummary {
    pub(super) kind: &'static str,
    pub(super) provider_error_class: Option<&'static str>,
    pub(super) http_status: Option<u16>,
    pub(super) failure_count: Option<usize>,
    pub(super) failure_limit: Option<usize>,
}

impl TjuaeCliRuntimeErrorSummary {
    fn new(kind: &'static str, provider_error_class: Option<&'static str>) -> Self {
        Self {
            kind,
            provider_error_class,
            http_status: None,
            failure_count: None,
            failure_limit: None,
        }
    }
}

pub(super) fn tjuae_cli_runtime_error_summary(error: &TjuaeCliAgentError) -> TjuaeCliRuntimeErrorSummary {
    match error {
        TjuaeCliAgentError::Provider(ProviderError::Api { status, .. }) => TjuaeCliRuntimeErrorSummary {
            http_status: Some(*status),
            ..TjuaeCliRuntimeErrorSummary::new("provider", Some("http_status"))
        },
        TjuaeCliAgentError::Provider(ProviderError::Connection(_) | ProviderError::Http(_)) => {
            TjuaeCliRuntimeErrorSummary::new("provider", Some("network"))
        }
        TjuaeCliAgentError::Provider(ProviderError::RateLimited { .. }) => TjuaeCliRuntimeErrorSummary {
            http_status: Some(429),
            ..TjuaeCliRuntimeErrorSummary::new("provider", Some("rate_limited"))
        },
        TjuaeCliAgentError::Provider(ProviderError::PromptTooLong(_)) => {
            TjuaeCliRuntimeErrorSummary::new("provider", Some("context_too_large"))
        }
        TjuaeCliAgentError::Provider(ProviderError::Parse(_)) => {
            TjuaeCliRuntimeErrorSummary::new("provider", Some("parse"))
        }
        TjuaeCliAgentError::ToolCallFailures { count, limit } => TjuaeCliRuntimeErrorSummary {
            kind: "tool_call_failures",
            provider_error_class: None,
            http_status: None,
            failure_count: Some(*count),
            failure_limit: Some(*limit),
        },
        TjuaeCliAgentError::ToolCallMalformed { count, limit } => TjuaeCliRuntimeErrorSummary {
            kind: "tool_call_malformed",
            provider_error_class: None,
            http_status: None,
            failure_count: Some(*count),
            failure_limit: Some(*limit),
        },
        TjuaeCliAgentError::ContextTooLong { .. } => {
            TjuaeCliRuntimeErrorSummary::new("context_too_large", Some("context_too_large"))
        }
        TjuaeCliAgentError::ApiError(_) => TjuaeCliRuntimeErrorSummary::new("api_error", None),
        TjuaeCliAgentError::UserAborted => TjuaeCliRuntimeErrorSummary::new("user_aborted", None),
    }
}

fn tjuae_cli_provider_error_to_send_error(error: &ProviderError, detail: String) -> AgentSendError {
    match error {
        ProviderError::Api { status, .. } => tjuae_cli_provider_status_to_send_error(*status, detail),
        ProviderError::RateLimited { body, .. } => provider_send_error(
            "模型提供商限制了请求频率",
            AgentErrorCode::UserLlmProviderRateLimited,
            append_provider_body(detail, body.as_deref()),
            true,
            AgentErrorResolutionKind::Retry,
            None,
        ),
        ProviderError::PromptTooLong(_) => provider_send_error(
            "请求内容超出当前模型的上下文窗口",
            AgentErrorCode::UserLlmProviderContextTooLarge,
            detail,
            false,
            AgentErrorResolutionKind::ReduceContext,
            None,
        ),
        ProviderError::Connection(_) | ProviderError::Http(_) => provider_send_error(
            "无法连接模型提供商",
            AgentErrorCode::UserLlmProviderNetworkError,
            detail,
            true,
            AgentErrorResolutionKind::CheckProviderBaseUrl,
            Some(AgentErrorResolutionTarget::ProviderSettings),
        ),
        ProviderError::Parse(_) => provider_send_error(
            "模型提供商返回服务器错误",
            AgentErrorCode::UserLlmProviderGatewayError,
            detail,
            true,
            AgentErrorResolutionKind::Retry,
            None,
        ),
    }
}

fn tjuae_cli_provider_status_to_send_error(status: u16, detail: String) -> AgentSendError {
    match status {
        400 => provider_send_error(
            "模型提供商拒绝了请求",
            AgentErrorCode::UserLlmProviderInvalidRequest,
            detail,
            false,
            AgentErrorResolutionKind::SendFeedback,
            Some(AgentErrorResolutionTarget::Feedback),
        ),
        401 => provider_send_error(
            "模型提供商拒绝了请求",
            AgentErrorCode::UserLlmProviderAuthFailed,
            detail,
            false,
            AgentErrorResolutionKind::CheckProviderCredentials,
            Some(AgentErrorResolutionTarget::ProviderSettings),
        ),
        402 => provider_send_error(
            "模型提供商账户需要处理计费问题",
            AgentErrorCode::UserLlmProviderBillingRequired,
            detail,
            false,
            AgentErrorResolutionKind::CheckProviderBilling,
            Some(AgentErrorResolutionTarget::ProviderSettings),
        ),
        403 => provider_send_error(
            "模型提供商拒绝访问该请求",
            AgentErrorCode::UserLlmProviderPermissionDenied,
            detail,
            false,
            AgentErrorResolutionKind::CheckProviderCredentials,
            Some(AgentErrorResolutionTarget::ProviderSettings),
        ),
        404 => provider_send_error(
            "未找到模型提供商接口",
            AgentErrorCode::UserLlmProviderEndpointNotFound,
            detail,
            false,
            AgentErrorResolutionKind::CheckProviderBaseUrl,
            Some(AgentErrorResolutionTarget::ProviderSettings),
        ),
        408 | 504 => provider_send_error(
            "模型提供商未及时响应",
            AgentErrorCode::UserLlmProviderTimeout,
            detail,
            true,
            AgentErrorResolutionKind::Retry,
            None,
        ),
        429 => provider_send_error(
            "模型提供商限制了请求频率",
            AgentErrorCode::UserLlmProviderRateLimited,
            detail,
            true,
            AgentErrorResolutionKind::Retry,
            None,
        ),
        500..=599 => provider_send_error(
            "模型提供商返回服务器错误",
            AgentErrorCode::UserLlmProviderGatewayError,
            detail,
            true,
            AgentErrorResolutionKind::Retry,
            None,
        ),
        _ => provider_send_error(
            "模型提供商返回错误",
            AgentErrorCode::UserLlmProviderGatewayError,
            detail,
            true,
            AgentErrorResolutionKind::Retry,
            None,
        ),
    }
}

fn provider_send_error(
    message: &'static str,
    code: AgentErrorCode,
    detail: String,
    retryable: bool,
    resolution_kind: AgentErrorResolutionKind,
    resolution_target: Option<AgentErrorResolutionTarget>,
) -> AgentSendError {
    AgentSendError::new(
        message,
        code,
        AgentErrorOwnership::UserLlmProvider,
        Some(detail),
        retryable,
        false,
        Some(AgentErrorResolution::new(resolution_kind, resolution_target)),
    )
}

fn unknown_upstream_send_error(detail: String) -> AgentSendError {
    AgentSendError::new(
        "上游 Agent 处理请求时失败",
        AgentErrorCode::UnknownUpstreamError,
        AgentErrorOwnership::UnknownUpstream,
        Some(detail),
        true,
        true,
        Some(AgentErrorResolution::new(
            AgentErrorResolutionKind::SendFeedback,
            Some(AgentErrorResolutionTarget::Feedback),
        )),
    )
}

/// Append the raw upstream response body (if any) to the detail string so
/// the UI's technical-details drawer surfaces provider-specific hints such
/// as `insufficient_quota`, `payment_required`, or per-endpoint rate-limit
/// notes. The body is passed through the existing `sanitize_error_detail`
/// pipeline downstream (redaction + truncation), so no extra scrubbing is
/// needed here.
fn append_provider_body(detail: String, body: Option<&str>) -> String {
    match body.map(str::trim).filter(|b| !b.is_empty()) {
        Some(body) => format!("{detail}\n提供商响应：{body}"),
        None => detail,
    }
}

fn tool_call_failure_send_error(detail: String) -> AgentSendError {
    AgentSendError::new(
        "上游 Agent 反复执行工具调用失败",
        AgentErrorCode::UnknownUpstreamError,
        AgentErrorOwnership::UnknownUpstream,
        Some(detail),
        true,
        true,
        Some(AgentErrorResolution::new(AgentErrorResolutionKind::Retry, None)),
    )
}

#[cfg(test)]
#[path = "error_test.rs"]
mod error_test;
