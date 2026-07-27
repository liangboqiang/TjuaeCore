use tjuaeui_ai_agent::{AcpError, AgentError};
use tjuaeui_db::DbError;

/// Application-level error contract for the conversation domain.
///
/// This type may preserve structured lower-layer errors for domain decisions,
/// but HTTP and WebSocket boundaries must map it through an explicit public
/// output mapper. Do not render `ConversationError::Acp` directly to clients.
#[derive(Debug, thiserror::Error)]
pub enum ConversationError {
    #[error("找不到对话：{id}")]
    NotFound { id: String },

    #[error("找不到消息：{id}")]
    MessageNotFound { id: String },

    #[error("找不到产物：{id}")]
    ArtifactNotFound { id: String },

    #[error("找不到对话对应的活动智能体：{conversation_id}")]
    ActiveAgentNotFound { conversation_id: String },

    #[error("对话已归档：{reason}")]
    Archived { id: String, reason: String },

    #[error("请求无效：{reason}")]
    BadRequest { reason: String },

    #[error("对话正忙：{reason}")]
    Busy { reason: String },

    #[error("无权访问：{reason}")]
    Forbidden { reason: String },

    #[error("未找到：{reason}")]
    NotFoundReason { reason: String },

    #[error("未认证：{reason}")]
    Unauthorized { reason: String },

    #[error("请求频率受限")]
    RateLimited,

    #[error("上游网关错误：{reason}")]
    BadGateway { reason: String },

    #[error("请求超时：{reason}")]
    Timeout { reason: String },

    #[error("ACP 配置选项确认超时")]
    ConfigConfirmationTimeout {
        conversation_id: String,
        option_id: String,
        requested: String,
        last_observed: Option<String>,
    },

    #[error("ACP 配置更新正在进行")]
    ConfigUpdateInProgress {
        conversation_id: String,
        option_id: String,
        requested: String,
    },

    #[error("该对话需要团队运行时：{conversation_id}")]
    TeamRuntimeRequired { conversation_id: String, team_id: String },

    #[error("请求内容无法处理：{reason}")]
    Unprocessable { reason: String },

    #[error("内部错误：{reason}")]
    Internal { reason: String },

    #[error("工作区路径不可用：{path}")]
    WorkspacePathUnavailable { path: String },

    #[error("执行期间工作区路径不可用：{path}")]
    WorkspacePathRuntimeUnavailable { path: String },

    #[error("无法连接 OpenClaw Gateway：{detail}")]
    OpenClawGatewayUnreachable { detail: String },

    #[error("ACP 错误")]
    Acp(#[from] AcpError),
}

impl ConversationError {
    pub(crate) fn internal(reason: impl Into<String>) -> Self {
        Self::Internal { reason: reason.into() }
    }

    pub(crate) fn bad_request(reason: impl Into<String>) -> Self {
        Self::BadRequest { reason: reason.into() }
    }

    pub(crate) fn not_found_reason(reason: impl Into<String>) -> Self {
        Self::NotFoundReason { reason: reason.into() }
    }

    pub(crate) fn to_agent_error(&self) -> AgentError {
        match self {
            Self::NotFound { id } => AgentError::not_found(format!("找不到对话：{id}")),
            Self::MessageNotFound { id } => AgentError::not_found(format!("找不到消息：{id}")),
            Self::ArtifactNotFound { id } => AgentError::not_found(format!("找不到产物：{id}")),
            Self::ActiveAgentNotFound { .. } => AgentError::not_found("找不到此对话对应的活动 Agent"),
            Self::Archived { reason, .. } => AgentError::conversation_archived(reason.clone()),
            Self::BadRequest { reason } => AgentError::bad_request(reason.clone()),
            Self::Busy { reason } => AgentError::conflict(reason.clone()),
            Self::Forbidden { reason } => AgentError::forbidden(reason.clone()),
            Self::NotFoundReason { reason } => AgentError::not_found(reason.clone()),
            Self::Unauthorized { reason } => AgentError::unauthorized(reason.clone()),
            Self::RateLimited => AgentError::RateLimited,
            Self::BadGateway { reason } => AgentError::bad_gateway(reason.clone()),
            Self::Timeout { reason } => AgentError::timeout(reason.clone()),
            Self::ConfigConfirmationTimeout { .. } => AgentError::timeout("ACP 配置选项确认超时"),
            Self::ConfigUpdateInProgress { .. } => AgentError::conflict("ACP 配置更新正在进行"),
            Self::TeamRuntimeRequired { .. } => AgentError::conflict("此对话属于团队，请使用团队运行时会话"),
            Self::Unprocessable { reason } => AgentError::bad_request(reason.clone()),
            Self::Internal { reason } => AgentError::internal(reason.clone()),
            Self::WorkspacePathUnavailable { path } => AgentError::bad_request(format!("工作区路径不可用：{path}")),
            Self::WorkspacePathRuntimeUnavailable { path } => {
                AgentError::workspace_path_runtime_unavailable(path.clone())
            }
            Self::OpenClawGatewayUnreachable { detail } => AgentError::bad_gateway(detail.clone()),
            Self::Acp(err) => AgentError::bad_gateway(err.to_string()),
        }
    }

    pub(crate) fn error_code(&self) -> &'static str {
        match self {
            Self::NotFound { .. }
            | Self::MessageNotFound { .. }
            | Self::ArtifactNotFound { .. }
            | Self::ActiveAgentNotFound { .. }
            | Self::NotFoundReason { .. } => "NOT_FOUND",
            Self::BadRequest { .. } => "BAD_REQUEST",
            Self::Unauthorized { .. } => "UNAUTHORIZED",
            Self::Forbidden { .. } => "FORBIDDEN",
            Self::Busy { .. } => "CONFLICT",
            Self::RateLimited => "RATE_LIMITED",
            Self::Internal { .. } | Self::Acp(_) => "INTERNAL_ERROR",
            Self::BadGateway { .. } => "BAD_GATEWAY",
            Self::Timeout { .. } => "TIMEOUT",
            Self::ConfigConfirmationTimeout { .. } => "confirmation_timeout",
            Self::ConfigUpdateInProgress { .. } => "config_update_in_progress",
            Self::TeamRuntimeRequired { .. } => "TEAM_RUNTIME_REQUIRED",
            Self::Unprocessable { .. } => "UNPROCESSABLE_ENTITY",
            Self::Archived { .. } => "CONVERSATION_ARCHIVED",
            Self::WorkspacePathUnavailable { .. } => "WORKSPACE_PATH_UNAVAILABLE",
            Self::WorkspacePathRuntimeUnavailable { .. } => "WORKSPACE_PATH_RUNTIME_UNAVAILABLE",
            Self::OpenClawGatewayUnreachable { .. } => "USER_AGENT_OPENCLAW_GATEWAY_UNREACHABLE",
        }
    }
}

impl From<AgentError> for ConversationError {
    fn from(error: AgentError) -> Self {
        match error {
            AgentError::NotFound(reason) => Self::NotFoundReason { reason },
            AgentError::BadRequest(reason) => Self::BadRequest { reason },
            AgentError::Unauthorized(reason) => Self::Unauthorized { reason },
            AgentError::Forbidden(reason) => Self::Forbidden { reason },
            AgentError::Conflict(reason) => Self::Busy { reason },
            AgentError::RateLimited => Self::RateLimited,
            AgentError::Internal(reason) => Self::Internal { reason },
            AgentError::BadGateway(reason) => Self::BadGateway { reason },
            AgentError::Timeout(reason) => Self::Timeout { reason },
            AgentError::ConversationArchived(reason) => Self::Archived {
                id: String::new(),
                reason,
            },
            AgentError::WorkspacePathRuntimeUnavailable(path) => Self::WorkspacePathRuntimeUnavailable { path },
            AgentError::Acp(err) => Self::Acp(err),
            _ => Self::Internal {
                reason: error.to_string(),
            },
        }
    }
}

impl From<DbError> for ConversationError {
    fn from(error: DbError) -> Self {
        match error {
            DbError::NotFound(reason) => Self::NotFoundReason { reason },
            DbError::Conflict(reason) => Self::Busy { reason },
            DbError::Query(e) => Self::Internal {
                reason: format!("数据库错误：{e}"),
            },
            DbError::Migration(e) => Self::Internal {
                reason: format!("迁移错误：{e}"),
            },
            DbError::Init(reason) => Self::Internal {
                reason: format!("数据库初始化错误：{reason}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_error<E: std::error::Error + Send + Sync + 'static>() {}

    fn assert_from_acp<T: From<AcpError>>() {}

    fn assert_from_agent<T: From<AgentError>>() {}

    fn assert_from_db<T: From<DbError>>() {}

    #[test]
    fn conversation_error_is_error_contract() {
        assert_error::<ConversationError>();
    }

    #[test]
    fn conversation_error_has_acp_from_impl() {
        assert_from_acp::<ConversationError>();
    }

    #[test]
    fn conversation_error_has_agent_from_impl() {
        assert_from_agent::<ConversationError>();
    }

    #[test]
    fn conversation_error_has_db_from_impl() {
        assert_from_db::<ConversationError>();
    }
}
