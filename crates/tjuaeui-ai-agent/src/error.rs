use crate::protocol::error::AcpError;
use crate::runtime_assets::RuntimeAssetFailureReason;

/// Crate-owned error model for ai-agent business, runtime, and protocol
/// orchestration code.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AgentError {
    #[error("请求无效：{0}")]
    BadRequest(String),
    #[error("未认证：{0}")]
    Unauthorized(String),
    #[error("无权访问：{0}")]
    Forbidden(String),
    #[error("未找到：{0}")]
    NotFound(String),
    #[error("发生冲突：{0}")]
    Conflict(String),
    #[error("上游网关错误：{0}")]
    BadGateway(String),
    #[error("超时：{0}")]
    Timeout(String),
    #[error("请求频率受限")]
    RateLimited,
    #[error("对话已归档：{0}")]
    ConversationArchived(String),
    #[error("执行期间工作区路径不可用：{0}")]
    WorkspacePathRuntimeUnavailable(String),
    #[error("运行资产加载契约失败（reasonCode={reason}）：{diagnostic}")]
    RuntimeAssetContract {
        reason: RuntimeAssetFailureReason,
        diagnostic: String,
    },
    #[error("内部错误：{0}")]
    Internal(String),
    #[error(transparent)]
    Acp(#[from] AcpError),
}

impl AgentError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::BadRequest(message.into())
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::Unauthorized(message.into())
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::Forbidden(message.into())
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::NotFound(message.into())
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::Conflict(message.into())
    }

    pub fn bad_gateway(message: impl Into<String>) -> Self {
        Self::BadGateway(message.into())
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::Timeout(message.into())
    }

    pub fn conversation_archived(message: impl Into<String>) -> Self {
        Self::ConversationArchived(message.into())
    }

    pub fn workspace_path_runtime_unavailable(path: impl Into<String>) -> Self {
        Self::WorkspacePathRuntimeUnavailable(path.into())
    }

    pub fn runtime_asset_contract(reason: RuntimeAssetFailureReason, diagnostic: impl Into<String>) -> Self {
        Self::RuntimeAssetContract {
            reason,
            diagnostic: diagnostic.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }

    pub(crate) fn public_message(&self) -> String {
        match self {
            Self::BadRequest(message)
            | Self::Unauthorized(message)
            | Self::Forbidden(message)
            | Self::NotFound(message)
            | Self::Conflict(message)
            | Self::BadGateway(message)
            | Self::Timeout(message)
            | Self::ConversationArchived(message)
            | Self::WorkspacePathRuntimeUnavailable(message)
            | Self::Internal(message) => message.clone(),
            Self::RuntimeAssetContract { reason, diagnostic } => {
                format!("reasonCode={}; {diagnostic}", reason.as_code())
            }
            Self::RateLimited => "请求频率受限".to_owned(),
            Self::Acp(err) => err.to_string(),
        }
    }
}
