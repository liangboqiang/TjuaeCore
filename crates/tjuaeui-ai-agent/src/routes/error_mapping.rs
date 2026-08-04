#![allow(clippy::disallowed_types)]

use tjuaeui_common::ApiError;

use crate::error::AgentError;
use crate::protocol::error::AcpError;

pub(crate) fn agent_error_to_api_error(err: AgentError) -> ApiError {
    match err {
        AgentError::BadRequest(message) => ApiError::BadRequest(message),
        AgentError::Unauthorized(message) => ApiError::Unauthorized(message),
        AgentError::Forbidden(message) => ApiError::Forbidden(message),
        AgentError::NotFound(message) => ApiError::NotFound(message),
        AgentError::Conflict(message) => ApiError::Conflict(message),
        AgentError::BadGateway(message) => ApiError::BadGateway(message),
        AgentError::Timeout(message) => ApiError::Timeout(message),
        AgentError::RateLimited => ApiError::RateLimited,
        AgentError::ConversationArchived(message) => ApiError::ConversationArchived(message),
        AgentError::WorkspacePathRuntimeUnavailable(path) => ApiError::WorkspacePathRuntimeUnavailable(path),
        AgentError::Internal(message) => ApiError::Internal(message),
        AgentError::Acp(err) => acp_error_to_api_error(err),
    }
}

fn acp_error_to_api_error(err: AcpError) -> ApiError {
    match &err {
        AcpError::SpawnFailed { .. } | AcpError::StartupCrash { .. } | AcpError::Disconnected { .. } => {
            ApiError::BadGateway(acp_error_public_message(&err))
        }
        AcpError::AuthRequired => ApiError::Unauthorized("Agent 需要身份验证".into()),
        AcpError::ProtocolParseError { .. } => ApiError::BadGateway(acp_error_public_message(&err)),
        AcpError::InvalidRequest { .. } => ApiError::BadRequest(acp_error_public_message(&err)),
        AcpError::SessionNotFound { .. } => ApiError::NotFound(acp_error_public_message(&err)),
        AcpError::ResourceNotFound { .. } => ApiError::NotFound(acp_error_public_message(&err)),
        AcpError::MethodNotFound { .. } => ApiError::BadRequest(acp_error_public_message(&err)),
        AcpError::InvalidParams { .. } => ApiError::BadRequest(acp_error_public_message(&err)),
        AcpError::AgentInternal { .. } => ApiError::BadGateway(acp_error_public_message(&err)),
        AcpError::OtherProtocolError { .. } => ApiError::BadGateway(acp_error_public_message(&err)),
        AcpError::NotConnected => ApiError::BadGateway(acp_error_public_message(&err)),
        AcpError::InitTimeout { .. } => ApiError::BadGateway(acp_error_public_message(&err)),
        // Short config/mode/model RPC timeout: connection is alive, the agent
        // just did not answer in time. Surface as a retryable Timeout so the
        // user can immediately retry the config change (see ELECTRON-3MS).
        AcpError::RequestTimeout { .. } => ApiError::Timeout(acp_error_public_message(&err)),
    }
}

fn acp_error_public_message(err: &AcpError) -> String {
    match err {
        AcpError::SpawnFailed { .. } | AcpError::StartupCrash { .. } | AcpError::Disconnected { .. } => {
            "Agent 进程不可用。".to_owned()
        }
        AcpError::AuthRequired => "Agent 需要身份验证。".to_owned(),
        AcpError::ProtocolParseError { .. } => "Agent 返回了格式错误的协议数据。".to_owned(),
        AcpError::InvalidRequest { .. } => "Agent 拒绝了无效协议请求。".to_owned(),
        AcpError::SessionNotFound { .. } => "未找到 Agent 会话。".to_owned(),
        AcpError::ResourceNotFound { .. } => "未找到 Agent 资源。".to_owned(),
        AcpError::MethodNotFound { .. } => "Agent 不支持此方法。".to_owned(),
        AcpError::InvalidParams { .. } => "ACP 请求参数无效。".to_owned(),
        AcpError::AgentInternal { code, .. } => format!("Agent 内部错误（代码 {code}）"),
        AcpError::OtherProtocolError { code, .. } => format!("Agent 协议错误（代码 {code}）"),
        AcpError::NotConnected => "ACP 协议未连接。".to_owned(),
        AcpError::InitTimeout { .. } => "Agent 初始化超时。".to_owned(),
        AcpError::RequestTimeout { .. } => "Agent 未及时响应请求。".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    #[test]
    fn acp_error_to_api_error_status_codes() {
        let cases = vec![
            (AcpError::SpawnFailed { message: "x".into() }, StatusCode::BAD_GATEWAY),
            (AcpError::AuthRequired, StatusCode::UNAUTHORIZED),
            (
                AcpError::ProtocolParseError {
                    message: "Parse error".into(),
                },
                StatusCode::BAD_GATEWAY,
            ),
            (
                AcpError::InvalidRequest {
                    message: "Invalid request".into(),
                },
                StatusCode::BAD_REQUEST,
            ),
            (
                AcpError::SessionNotFound { session_id: "s".into() },
                StatusCode::NOT_FOUND,
            ),
            (
                AcpError::ResourceNotFound {
                    resource: Some("file:///missing.txt".into()),
                    message: "Resource not found".into(),
                },
                StatusCode::NOT_FOUND,
            ),
            (AcpError::MethodNotFound { method: "m".into() }, StatusCode::BAD_REQUEST),
            (AcpError::InvalidParams { message: "p".into() }, StatusCode::BAD_REQUEST),
            (
                AcpError::AgentInternal {
                    message: "e".into(),
                    code: -1,
                    data: None,
                },
                StatusCode::BAD_GATEWAY,
            ),
            (
                AcpError::OtherProtocolError {
                    code: -32099,
                    message: "custom error".into(),
                    data: None,
                },
                StatusCode::BAD_GATEWAY,
            ),
            (AcpError::NotConnected, StatusCode::BAD_GATEWAY),
            (AcpError::InitTimeout { timeout_secs: 30 }, StatusCode::BAD_GATEWAY),
        ];

        for (acp_err, expected_status) in cases {
            let api_err = acp_error_to_api_error(acp_err);
            assert_eq!(api_err.status_code(), expected_status, "Mismatch for {api_err:?}");
        }
    }

    #[test]
    fn acp_error_to_api_error_omits_stderr_and_structured_data() {
        let startup = acp_error_to_api_error(AcpError::StartupCrash {
            exit_code: Some(1),
            signal: None,
            stderr: "Authorization: Bearer sk-secret".into(),
        });
        assert!(!startup.to_string().contains("sk-secret"));
        assert!(!startup.to_string().contains("Authorization"));

        let internal = acp_error_to_api_error(AcpError::AgentInternal {
            message: "Internal error".into(),
            code: -32603,
            data: Some(serde_json::json!({
                "error": "Failed to connect MCP servers",
                "api_key": "sk-secret"
            })),
        });
        let rendered = internal.to_string();
        assert!(rendered.contains("Agent 内部错误（代码 -32603）"));
        assert!(!rendered.contains("Failed to connect MCP servers"));
        assert!(!rendered.contains("sk-secret"));
        assert!(!rendered.contains("api_key"));
    }

    #[test]
    fn acp_error_to_api_error_uses_fixed_public_messages() {
        let cases = vec![
            acp_error_to_api_error(AcpError::SpawnFailed {
                message: "spawn failed at /tmp/agent with token sk-secret".into(),
            }),
            acp_error_to_api_error(AcpError::SessionNotFound {
                session_id: "/tmp/session-123".into(),
            }),
            acp_error_to_api_error(AcpError::MethodNotFound {
                method: "debug.dumpSecrets".into(),
            }),
            acp_error_to_api_error(AcpError::InvalidParams {
                message: "invalid path /tmp/private and token sk-secret".into(),
            }),
            acp_error_to_api_error(AcpError::InitTimeout { timeout_secs: 42 }),
        ];

        for api_err in cases {
            let rendered = api_err.public_message();
            assert!(!rendered.contains("/tmp"), "leaked path in {rendered}");
            assert!(!rendered.contains("sk-secret"), "leaked token in {rendered}");
            assert!(!rendered.contains("debug.dumpSecrets"), "leaked method in {rendered}");
            assert!(!rendered.contains("42"), "leaked internal timeout in {rendered}");
        }
    }
}
